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
/// The shared queue's states (shared-memory-queue.h).
const STARTING: u32 = 1;
const READY: u32 = 2;
const STOPPING: u32 = 3;
const CAMERA_MAPPING: &str = "OBSVirtualCamVideo";
/// How long a restart waits for readers to let go of the previous mapping.
const RELEASE_WAIT: Duration = Duration::from_millis(1500);
// The mapping is owned and used only by the camera thread.
impl Queue {
    fn new(format: VideoFormat) -> Result<Self> {
        Self::open(CAMERA_MAPPING, format, RELEASE_WAIT)
    }
    /// A reader keeps a stopped producer's mapping alive until it notices
    /// the stop, which a reader whose graph isn't running never does: the
    /// camera then couldn't be switched back on. A stopped mapping of the
    /// same format is taken over; a different one is waited for.
    fn open(name: &str, format: VideoFormat, wait: Duration) -> Result<Self> {
        let deadline = Instant::now() + wait;
        loop {
            match Self::create(name, format)? {
                Ok(queue) => return Ok(queue),
                Err(state) if state != STOPPING || Instant::now() >= deadline => {
                    if state == STOPPING {
                        bail!(
                            "OBS Virtual Camera is still open in another app at a different resolution. Close the camera there, then try again."
                        );
                    }
                    bail!(
                        "OBS Virtual Camera is already producing video. Stop Virtual Camera in OBS and close other AV Relay instances."
                    );
                }
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
    /// The queue, or the state of a mapping that already exists and can't
    /// be taken over.
    fn create(name: &str, format: VideoFormat) -> Result<std::result::Result<Self, u32>> {
        let query = object_query()?;
        let pixels = format.width as usize * format.height as usize;
        let bytes = pixels * 3 / 2;
        let stride = align32(bytes + 32);
        let start = align32(std::mem::size_of::<Header>());
        let size = start + stride * 3;
        let name = crate::wide(name);
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
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if view.Value.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            // An existing mapping smaller than this format's.
            return if existed { Ok(Err(STOPPING)) } else { Err(error.into()) };
        }
        let offsets = std::array::from_fn(|i| (start + i * stride) as u32);
        let interval = 10_000_000 / format.fps as u64;
        if existed {
            let header = unsafe { &*view.Value.cast::<Header>() };
            let state = header.state.load(Ordering::Acquire);
            // Readers keep the format they read when the queue became ready.
            let same = header.width == format.width
                && header.height == format.height
                && header.interval == interval
                && header.offsets == offsets;
            if state != STOPPING || !same {
                unsafe {
                    UnmapViewOfFile(view);
                    CloseHandle(handle);
                }
                return Ok(Err(state));
            }
            log::info!("taking over the stopped virtual camera mapping");
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
                    state: AtomicU32::new(STARTING),
                    offsets,
                    kind: 0,
                    width: format.width,
                    height: format.height,
                    interval,
                    reserved: [0; 8],
                },
            );
        }
        Ok(Ok(queue))
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
            header.state.store(READY, Ordering::Release);
        }
    }
}
impl Drop for Queue {
    fn drop(&mut self) {
        unsafe {
            (*self.view.Value.cast::<Header>())
                .state
                .store(STOPPING, Ordering::Release);
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

/// Since the last [`VideoFeed::take_stats`].
#[derive(Debug, Default, Clone, Copy)]
pub struct CamStats {
    /// Frames from the sender.
    pub pushed: u64,
    /// Of those, frames that came more than 1.5 frame intervals after the
    /// one before.
    pub late: u64,
    /// The longest time between two frames from the sender.
    pub max_gap: Duration,
    /// Frames written again because the next one hadn't come.
    pub repeated: u64,
    /// Black frames written (no frame from the sender).
    pub placeholder: u64,
}
struct Frame {
    pixels: Vec<u8>,
    received: Option<Instant>,
    stats: CamStats,
}
#[derive(Clone)]
pub struct VideoFeed {
    frame: Arc<Mutex<Frame>>,
    width: usize,
    height: usize,
    period: Duration,
}
impl VideoFeed {
    pub fn push(&self, yuy2: &[u8]) {
        let mut frame = self.frame.lock().unwrap();
        if yuy2_to_nv12(yuy2, &mut frame.pixels, self.width, self.height) {
            let now = Instant::now();
            if let Some(gap) = frame.received.map(|t| now - t) {
                frame.stats.max_gap = frame.stats.max_gap.max(gap);
                if gap > self.period * 3 / 2 {
                    frame.stats.late += 1;
                }
            }
            frame.stats.pushed += 1;
            frame.received = Some(now);
        }
    }
    pub fn clear(&self) {
        self.frame.lock().unwrap().received = None;
    }
    pub fn take_stats(&self) -> CamStats {
        std::mem::take(&mut self.frame.lock().unwrap().stats)
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
                stats: CamStats::default(),
            })),
            width: format.width as usize,
            height: format.height as usize,
            period: Duration::from_secs_f64(1.0 / format.fps as f64),
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
                let mut written = None;
                while !stopped.load(Ordering::Acquire) {
                    used.store(queue.in_use(), Ordering::Release);
                    {
                        let mut frame = frame.lock().unwrap();
                        let fresh = frame
                            .received
                            .filter(|t| t.elapsed() < Duration::from_secs(1));
                        if fresh.is_none() {
                            frame.stats.placeholder += 1;
                        } else if fresh == written {
                            frame.stats.repeated += 1;
                        }
                        written = fresh;
                        let image = if fresh.is_some() { &frame.pixels } else { &blank };
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
    /// A mapping name of the test's own: the real camera may be in use.
    fn test_name(test: &str) -> String {
        format!("AVRelayTest-{test}-{}", std::process::id())
    }
    const FORMAT: VideoFormat = VideoFormat {
        width: 16,
        height: 16,
        fps: 30,
    };
    #[test]
    fn mapping_is_exclusive_and_header_matches() {
        let name = test_name("exclusive");
        let mut queue = Queue::open(&name, FORMAT, Duration::ZERO).unwrap();
        let error = Queue::open(&name, FORMAT, Duration::ZERO).err().unwrap();
        assert!(error.to_string().contains("already producing"), "{error}");
        assert!(!queue.in_use());
        let consumer = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, crate::wide(&name).as_ptr()) };
        assert!(!consumer.is_null());
        assert!(queue.in_use());
        unsafe {
            CloseHandle(consumer);
        }
        assert!(!queue.in_use());
        queue.write(&black(256), 123);
        unsafe {
            let header = &*queue.view.Value.cast::<Header>();
            assert_eq!(header.state.load(Ordering::Acquire), READY);
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
    /// A reader that never noticed the stop keeps the old mapping: the
    /// camera must still come back on.
    #[test]
    fn restarts_while_a_reader_holds_the_stopped_mapping() {
        let name = test_name("restart");
        let wide = crate::wide(&name);
        let queue = Queue::open(&name, FORMAT, Duration::ZERO).unwrap();
        let reader = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide.as_ptr()) };
        assert!(!reader.is_null());
        drop(queue);
        let mut queue = Queue::open(&name, FORMAT, Duration::ZERO).unwrap();
        assert!(queue.in_use(), "the reader's handle is on the same mapping");
        queue.write(&black(256), 1);
        let view = unsafe { MapViewOfFile(reader, FILE_MAP_READ, 0, 0, 0) };
        assert!(!view.Value.is_null());
        unsafe {
            let header = &*view.Value.cast::<Header>();
            assert_eq!(header.state.load(Ordering::Acquire), READY);
            assert_eq!((header.width, header.height), (16, 16));
            UnmapViewOfFile(view);
        }
        // A different format can't be taken over while the reader holds on.
        drop(queue);
        let bigger = VideoFormat { width: 32, ..FORMAT };
        let error = Queue::open(&name, bigger, Duration::from_millis(100)).err().unwrap();
        assert!(error.to_string().contains("still open"), "{error}");
        // Once it lets go, the new format starts at once.
        unsafe {
            CloseHandle(reader);
        }
        Queue::open(&name, bigger, Duration::ZERO).unwrap();
    }
}
