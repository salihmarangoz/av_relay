// SPDX-License-Identifier: GPL-3.0-or-later
//! OBS's Windows virtual camera reads NV12 from a named, triple-buffered
//! mapping. Layout: obs-studio/plugins/win-dshow/shared-memory-queue.c.
//! OBS must be installed, but its own virtual-camera output must be stopped.
use crate::proto::VideoFormat;
use anyhow::{Context, Result, bail};
use std::{
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    System::Memory::*,
};

#[repr(C)]
struct Header {
    write_idx: AtomicU32,
    read_idx: AtomicU32,
    state: AtomicU32,
    offsets: [u32; 3],
    kind: u32,
    width: u32,
    height: u32,
    interval: u64,
    reserved: [u32; 8],
}
const _: () = assert!(std::mem::size_of::<Header>() == 80);
fn align32(n: usize) -> usize {
    (n + 31) & !31
}

struct Queue {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    size: usize,
    pixels: usize,
    query: QueryObject,
}
type QueryObject =
    unsafe extern "system" fn(HANDLE, i32, *mut std::ffi::c_void, u32, *mut u32) -> i32;
fn object_query() -> Result<QueryObject> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    unsafe {
        let module = GetModuleHandleW(crate::wide("ntdll.dll").as_ptr());
        let query = GetProcAddress(module, c"NtQueryObject".as_ptr().cast())
            .context("Camera use detection is unavailable")?;
        Ok(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            QueryObject,
        >(query))
    }
}
// The mapping is owned and used only by the camera thread.
impl Queue {
    fn new(format: VideoFormat) -> Result<Self> {
        let query = object_query()?;
        let pixels = format.width as usize * format.height as usize;
        let bytes = pixels * 3 / 2;
        let stride = align32(bytes + 32);
        let start = align32(std::mem::size_of::<Header>());
        let size = start + stride * 3;
        let name = crate::wide("OBSVirtualCamVideo");
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                PAGE_READWRITE,
                0,
                size.try_into()?,
                name.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            bail!(
                "OBS Virtual Camera is already producing video. Stop Virtual Camera in OBS and close other AV Relay instances."
            );
        }
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if view.Value.is_null() {
            unsafe {
                CloseHandle(handle);
            }
            return Err(std::io::Error::last_os_error().into());
        }
        let queue = Self {
            handle,
            view,
            size,
            pixels,
            query,
        };
        unsafe {
            ptr::write_bytes(queue.view.Value.cast::<u8>(), 0, size);
            ptr::write(
                queue.view.Value.cast::<Header>(),
                Header {
                    write_idx: AtomicU32::new(0),
                    read_idx: AtomicU32::new(0),
                    state: AtomicU32::new(1),
                    offsets: std::array::from_fn(|i| (start + i * stride) as u32),
                    kind: 0,
                    width: format.width,
                    height: format.height,
                    interval: 10_000_000 / format.fps as u64,
                    reserved: [0; 8],
                },
            );
        }
        Ok(queue)
    }
    /// OBS readers open this mapping only while their capture graph runs.
    /// Its public object information counts our one handle plus consumers.
    fn in_use(&self) -> bool {
        let mut info = [0u32; 14];
        let status = unsafe {
            (self.query)(
                self.handle,
                0,
                info.as_mut_ptr().cast(),
                std::mem::size_of_val(&info) as u32,
                ptr::null_mut(),
            )
        };
        status >= 0 && info[2] > 1
    }
    fn write(&mut self, frame: &[u8], timestamp: u64) {
        if frame.len() != self.pixels * 3 / 2 {
            return;
        }
        unsafe {
            let header = &*self.view.Value.cast::<Header>();
            let index = header.write_idx.load(Ordering::Relaxed).wrapping_add(1);
            let offset = header.offsets[index as usize % 3] as usize;
            debug_assert!(offset + 32 + frame.len() <= self.size);
            let slot = self.view.Value.cast::<u8>().add(offset);
            ptr::write_unaligned(slot.cast::<u64>(), timestamp);
            ptr::copy_nonoverlapping(frame.as_ptr(), slot.add(32), frame.len());
            header.write_idx.store(index, Ordering::Release);
            header.read_idx.store(index, Ordering::Release);
            header.state.store(2, Ordering::Release);
        }
    }
}
impl Drop for Queue {
    fn drop(&mut self) {
        unsafe {
            (*self.view.Value.cast::<Header>())
                .state
                .store(3, Ordering::Release);
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

struct Frame {
    pixels: Vec<u8>,
    received: Option<Instant>,
}
#[derive(Clone)]
pub struct VideoFeed {
    frame: Arc<Mutex<Frame>>,
    width: usize,
    height: usize,
}
impl VideoFeed {
    pub fn push(&self, yuy2: &[u8]) {
        let mut frame = self.frame.lock().unwrap();
        if yuy2_to_nv12(yuy2, &mut frame.pixels, self.width, self.height) {
            frame.received = Some(Instant::now());
        }
    }
    pub fn clear(&self) {
        self.frame.lock().unwrap().received = None;
    }
}
pub struct Camera {
    pub feed: VideoFeed,
    pub in_use: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Camera {
    pub fn start(format: VideoFormat) -> Result<Self> {
        if format.width % 2 != 0 || format.height % 2 != 0 || format.fps == 0 {
            bail!("Camera dimensions must be even and fps nonzero");
        }
        let pixels = format.width as usize * format.height as usize;
        let feed = VideoFeed {
            frame: Arc::new(Mutex::new(Frame {
                pixels: black(pixels),
                received: None,
            })),
            width: format.width as usize,
            height: format.height as usize,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let in_use = Arc::new(AtomicBool::new(false));
        let used = in_use.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let frame = feed.frame.clone();
        let stopped = stop.clone();
        let thread = thread::Builder::new()
            .name("virtual-camera".into())
            .spawn(move || {
                let mut queue = match Queue::new(format) {
                    Ok(queue) => {
                        let _ = ready_tx.send(Ok(()));
                        queue
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let blank = black(pixels);
                let start = Instant::now();
                let period = Duration::from_secs_f64(1.0 / format.fps as f64);
                let mut next = start;
                while !stopped.load(Ordering::Acquire) {
                    used.store(queue.in_use(), Ordering::Release);
                    {
                        let frame = frame.lock().unwrap();
                        let image = if frame
                            .received
                            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
                        {
                            &frame.pixels
                        } else {
                            &blank
                        };
                        queue.write(image, start.elapsed().as_nanos() as u64);
                    }
                    next += period;
                    thread::sleep(next.saturating_duration_since(Instant::now()));
                    if Instant::now().saturating_duration_since(next) > period {
                        next = Instant::now();
                    }
                }
            })?;
        ready_rx.recv().context("Virtual camera thread stopped")??;
        Ok(Self {
            feed,
            in_use,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Camera {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn black(pixels: usize) -> Vec<u8> {
    let mut out = vec![128; pixels * 3 / 2];
    out[..pixels].fill(16);
    out
}

pub fn installed() -> bool {
    use windows_sys::Win32::System::Registry::*;
    let mut key = ptr::null_mut();
    let path = crate::wide("CLSID\\{A3FCE0F5-3493-419F-958A-ABA1250EC20B}\\InprocServer32");
    let ok = unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, path.as_ptr(), 0, KEY_READ, &mut key) } == 0;
    if ok {
        unsafe {
            RegCloseKey(key);
        }
    }
    ok
}

/// YUY2's horizontal chroma is averaged vertically to NV12's 2x2 chroma.
fn yuy2_to_nv12(src: &[u8], dst: &mut [u8], width: usize, height: usize) -> bool {
    let pixels = width * height;
    if width % 2 != 0 || height % 2 != 0 || src.len() != pixels * 2 || dst.len() != pixels * 3 / 2 {
        return false;
    }
    for i in 0..pixels {
        dst[i] = src[i * 2];
    }
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let a = (y * width + x) * 2;
            let b = a + width * 2;
            let uv = pixels + y / 2 * width + x;
            dst[uv] = ((src[a + 1] as u16 + src[b + 1] as u16 + 1) / 2) as u8;
            dst[uv + 1] = ((src[a + 3] as u16 + src[b + 3] as u16 + 1) / 2) as u8;
        }
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nv12_has_correct_luma_chroma_and_bounds() {
        let mut dst = [0; 6];
        assert!(yuy2_to_nv12(
            &[10, 20, 30, 40, 50, 60, 70, 80],
            &mut dst,
            2,
            2
        ));
        assert_eq!(dst, [10, 30, 50, 70, 40, 60]);
        assert!(!yuy2_to_nv12(&[0; 7], &mut dst, 2, 2));
    }
    #[test]
    fn obs_mapping_is_exclusive_and_header_matches() {
        let format = VideoFormat {
            width: 16,
            height: 16,
            fps: 30,
        };
        let Ok(mut queue) = Queue::new(format) else {
            return;
        }; // OBS can be running on a developer's desktop.
        assert!(Queue::new(format).is_err());
        assert!(!queue.in_use());
        let consumer = unsafe {
            OpenFileMappingW(FILE_MAP_READ, 0, crate::wide("OBSVirtualCamVideo").as_ptr())
        };
        assert!(!consumer.is_null());
        assert!(queue.in_use());
        unsafe {
            CloseHandle(consumer);
        }
        assert!(!queue.in_use());
        queue.write(&black(256), 123);
        unsafe {
            let header = &*queue.view.Value.cast::<Header>();
            assert_eq!(header.state.load(Ordering::Acquire), 2);
            assert_eq!(header.interval, 333333);
            let slot = queue
                .view
                .Value
                .cast::<u8>()
                .add(header.offsets[1] as usize);
            assert_eq!(ptr::read_unaligned(slot.cast::<u64>()), 123);
            assert_eq!(*slot.add(32), 16);
        }
    }
}
