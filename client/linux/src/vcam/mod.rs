//! The virtual camera: a v4l2loopback device that apps see as a webcam.
//!
//! The device itself is created once (at startup) through v4l2loopback's
//! control device, with exclusive caps: it is listed as a capture device only
//! while a writer has it open. So [`VirtualCamera`] lives exactly as long as
//! the Mac is available: opening it makes the camera appear in apps, dropping
//! it makes it disappear again.
//!
//! v4l2loopback (0.13+) tells the writer when a reader starts or stops
//! streaming. That is how we know an app is using the camera and the Mac
//! should start capturing.

mod sys;
mod waiting;

pub use waiting::waiting_frame;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

const CONTROL_DEVICE: &str = "/dev/v4l2loopback";
const SYSFS_VIDEO: &str = "/sys/class/video4linux";

/// The label apps show for the camera.
pub const CARD_LABEL: &str = "MacBook Camera";
/// /dev/video48 when it's free: a stable path, because some apps remember the
/// camera by its path.
pub const PREFERRED_DEVICE_NR: i32 = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl CameraFormat {
    /// Size of one YUYV (4:2:2, 2 bytes per pixel) frame.
    pub fn frame_size(&self) -> usize {
        self.width as usize * self.height as usize * 2
    }

    fn frame_interval(&self) -> Duration {
        Duration::from_secs(1) / self.fps.max(1)
    }
}

/// Checks that v4l2loopback is loaded, new enough and usable by this user,
/// and returns its version.
pub fn check_loopback() -> Result<(u32, u32, u32)> {
    let ctl = match File::open(CONTROL_DEVICE) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => bail!(
            "the v4l2loopback kernel module isn't loaded. On Ubuntu 24.04 with the HWE kernel, install it with \
             `sudo apt install linux-modules-v4l2loopback-generic-hwe-24.04` and restart the computer."
        ),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => bail!(
            "no access to {CONTROL_DEVICE}. Reinstalling AV Relay restores the rule that grants it; \
             `sudo udevadm trigger --action=change --sysname-match=v4l2loopback` applies it now."
        ),
        Err(e) => return Err(e).context(CONTROL_DEVICE),
    };
    let mut code = 0u32;
    unsafe { sys::loopback_ctl_version(ctl.as_raw_fd(), &mut code) }
        .context("v4l2loopback is too old: it has no version ioctl")?;
    let version = (code >> 16, (code >> 8) & 0xff, code & 0xff);
    if version < sys::MIN_LOOPBACK_VERSION {
        let (a, b, c) = sys::MIN_LOOPBACK_VERSION;
        bail!(
            "v4l2loopback {}.{}.{} can't tell when an app uses the camera; {a}.{b}.{c} or newer is needed. \
             Ubuntu 24.04's v4l2loopback-dkms is too old: use the HWE kernel's module instead \
             (`sudo apt install linux-modules-v4l2loopback-generic-hwe-24.04`, then restart).",
            version.0,
            version.1,
            version.2
        );
    }
    Ok(version)
}

/// Returns the number N of the loopback device /dev/videoN labelled `label`,
/// creating it if it doesn't exist.
pub fn ensure_device(label: &str) -> Result<u32> {
    if let Some(nr) = find_device(label)? {
        return Ok(nr);
    }
    let ctl = OpenOptions::new()
        .read(true)
        .write(true)
        .open(CONTROL_DEVICE)
        .with_context(|| format!("opening {CONTROL_DEVICE}"))?;
    let mut config = sys::v4l2_loopback_config {
        output_nr: PREFERRED_DEVICE_NR,
        unused: PREFERRED_DEVICE_NR,
        card_label: [0; 32],
        min_width: 0,
        max_width: 0,
        min_height: 0,
        max_height: 0,
        max_buffers: 0,
        max_openers: 0,
        debug: 0,
        // Exclusive caps: a capture device only while a writer is attached.
        announce_all_caps: 0,
    };
    let bytes = label.as_bytes();
    let n = bytes.len().min(config.card_label.len() - 1);
    config.card_label[..n].copy_from_slice(&bytes[..n]);

    let nr = match unsafe { sys::loopback_ctl_add(ctl.as_raw_fd(), &config) } {
        Err(Errno::EEXIST) => {
            config.output_nr = -1;
            config.unused = -1;
            unsafe { sys::loopback_ctl_add(ctl.as_raw_fd(), &config) }
        }
        other => other,
    }
    .context("creating the loopback camera device")?;
    log::info!("created /dev/video{nr} ({label})");
    Ok(nr as u32)
}

fn find_device(label: &str) -> Result<Option<u32>> {
    let entries = match fs::read_dir(SYSFS_VIDEO) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context(SYSFS_VIDEO),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(nr) = name.to_str().and_then(|n| n.strip_prefix("video")).and_then(|n| n.parse().ok()) else {
            continue;
        };
        // Loopback devices are virtual; a real camera has a bus behind it.
        let real = fs::canonicalize(entry.path()).unwrap_or_default();
        if !real.to_string_lossy().contains("/devices/virtual/") {
            continue;
        }
        if fs::read_to_string(entry.path().join("name")).is_ok_and(|n| n.trim_end() == label) {
            return Ok(Some(nr));
        }
    }
    Ok(None)
}

struct State {
    in_use: bool,
    /// The newest frame from the Mac; empty until one arrives.
    frame: Vec<u8>,
    /// Shown while there is no frame: black, or the waiting picture.
    placeholder: Vec<u8>,
    /// When the newest frame arrived.
    last_push: Instant,
    last_write: Instant,
}

struct Shared {
    file: File,
    format: CameraFormat,
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn write(&self, frame: &[u8], state: &mut State) {
        if let Err(e) = (&self.file).write_all(frame) {
            log::warn!("writing a frame to the virtual camera: {e}");
        }
        state.last_write = Instant::now();
    }
}

/// The writer side of the loopback device. While it exists, apps list the
/// camera; `on_usage(true)` is called when an app starts streaming from it
/// and `on_usage(false)` when it stops.
pub struct VirtualCamera {
    shared: Arc<Shared>,
    path: PathBuf,
    threads: Vec<JoinHandle<()>>,
}

impl VirtualCamera {
    pub fn open(nr: u32, format: CameraFormat, on_usage: impl Fn(bool) + Send + 'static) -> Result<Self> {
        let path = PathBuf::from(format!("/dev/video{nr}"));
        let file = open_retrying(&path)?;

        let mut cap: sys::v4l2_capability = unsafe { std::mem::zeroed() };
        unsafe { sys::vidioc_querycap(file.as_raw_fd(), &mut cap) }.context("VIDIOC_QUERYCAP")?;
        if !cap.driver.starts_with(b"v4l2 loopback\0") {
            bail!("{} isn't a v4l2loopback device", path.display());
        }

        let mut fmt = sys::v4l2_format {
            type_: sys::V4L2_BUF_TYPE_VIDEO_OUTPUT,
            fmt: sys::v4l2_format_union {
                pix: sys::v4l2_pix_format {
                    width: format.width,
                    height: format.height,
                    pixelformat: sys::V4L2_PIX_FMT_YUYV,
                    field: sys::V4L2_FIELD_NONE,
                    bytesperline: format.width * 2,
                    sizeimage: format.frame_size() as u32,
                    colorspace: sys::V4L2_COLORSPACE_REC709,
                    ..Default::default()
                },
            },
        };
        unsafe { sys::vidioc_s_fmt(file.as_raw_fd(), &mut fmt) }
            .with_context(|| format!("setting {}x{} YUYV on {}", format.width, format.height, path.display()))?;
        let got = unsafe { fmt.fmt.pix };
        if (got.width, got.height, got.pixelformat) != (format.width, format.height, sys::V4L2_PIX_FMT_YUYV) {
            bail!(
                "{} is locked to another format ({}x{}); close the apps using it and try again",
                path.display(),
                got.width,
                got.height
            );
        }

        let mut parm = sys::v4l2_streamparm {
            type_: sys::V4L2_BUF_TYPE_VIDEO_OUTPUT,
            parm: sys::v4l2_streamparm_union {
                output: sys::v4l2_outputparm {
                    timeperframe: sys::v4l2_fract { numerator: 1, denominator: format.fps },
                    ..Default::default()
                },
            },
        };
        unsafe { sys::vidioc_s_parm(file.as_raw_fd(), &mut parm) }.context("setting the frame rate")?;

        let sub = sys::v4l2_event_subscription { type_: sys::V4L2_EVENT_PRI_CLIENT_USAGE, ..Default::default() };
        unsafe { sys::vidioc_subscribe_event(file.as_raw_fd(), &sub) }
            .context("subscribing to v4l2loopback's client-usage events")?;

        let placeholder = black(format);
        let shared = Arc::new(Shared {
            file,
            format,
            state: Mutex::new(State {
                in_use: false,
                frame: Vec::new(),
                placeholder,
                last_push: Instant::now(),
                last_write: Instant::now(),
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        // The first write turns the device into a capture device for apps.
        {
            let mut state = shared.state.lock().unwrap();
            let placeholder = std::mem::take(&mut state.placeholder);
            shared.write(&placeholder, &mut state);
            state.placeholder = placeholder;
        }

        let usage = {
            let shared = shared.clone();
            thread::Builder::new().name("vcam-usage".into()).spawn(move || watch_usage(&shared, on_usage))?
        };
        let keeper = {
            let shared = shared.clone();
            thread::Builder::new().name("vcam-keeper".into()).spawn(move || keep_frames(&shared))?
        };
        log::info!("virtual camera on {} at {}x{} {} fps", path.display(), format.width, format.height, format.fps);
        Ok(Self { shared, path, threads: vec![usage, keeper] })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn format(&self) -> CameraFormat {
        self.shared.format
    }

    /// Shows a YUYV frame (exactly `format().frame_size()` bytes) to the app
    /// using the camera.
    pub fn push_frame(&self, frame: &[u8]) {
        if frame.len() != self.shared.format.frame_size() {
            log::warn!("dropping a {}-byte frame; expected {}", frame.len(), self.shared.format.frame_size());
            return;
        }
        let mut state = self.shared.state.lock().unwrap();
        state.frame.clear();
        state.frame.extend_from_slice(frame);
        state.last_push = Instant::now();
        if state.in_use {
            self.shared.write(frame, &mut state);
        }
    }

    /// Goes back to the placeholder, e.g. when the Mac stops its camera.
    pub fn clear(&self) {
        self.shared.state.lock().unwrap().frame.clear();
    }

    /// What to show while there's no frame from the Mac: `None` is black
    /// (the Mac's camera is starting), `Some` e.g. the waiting picture.
    pub fn set_placeholder(&self, frame: Option<Vec<u8>>) {
        let frame = frame.filter(|f| f.len() == self.shared.format.frame_size()).unwrap_or_else(|| black(self.shared.format));
        self.shared.state.lock().unwrap().placeholder = frame;
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        log::info!("virtual camera on {} closed", self.path.display());
    }
}

/// Black in YUYV: Y = 16, Cb = Cr = 128 (limited range).
fn black(format: CameraFormat) -> Vec<u8> {
    [16u8, 128].repeat(format.frame_size() / 2)
}

/// udev grants the desktop user access to a new device node a moment after
/// it appears, so retry briefly.
fn open_retrying(path: &Path) -> Result<File> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match OpenOptions::new().read(true).write(true).custom_flags(libc::O_CLOEXEC).open(path) {
            Ok(f) => return Ok(f),
            Err(e)
                if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied)
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
        }
    }
}

fn watch_usage(shared: &Shared, on_usage: impl Fn(bool)) {
    let fd = shared.file.as_fd();
    while !shared.stop.load(Ordering::Relaxed) {
        let mut fds = [PollFd::new(fd, PollFlags::POLLPRI)];
        match poll(&mut fds, PollTimeout::from(200u16)) {
            Ok(0) | Err(Errno::EINTR) => continue,
            Ok(_) => {}
            Err(e) => {
                log::error!("polling the virtual camera for events: {e}");
                return;
            }
        }
        // Drain the queue; the last event holds the current state.
        let mut in_use = None;
        loop {
            let mut ev: sys::v4l2_event = unsafe { std::mem::zeroed() };
            match unsafe { sys::vidioc_dqevent(fd.as_raw_fd(), &mut ev) } {
                Ok(_) => {
                    if ev.type_ == sys::V4L2_EVENT_PRI_CLIENT_USAGE {
                        in_use = Some(ev.u[0] as u32 != 0);
                    }
                    if ev.pending == 0 {
                        break;
                    }
                }
                Err(Errno::ENOENT) => break,
                Err(e) => {
                    log::warn!("VIDIOC_DQEVENT: {e}");
                    break;
                }
            }
        }
        let Some(in_use) = in_use else { continue };
        let changed = {
            let mut state = shared.state.lock().unwrap();
            let changed = state.in_use != in_use;
            state.in_use = in_use;
            changed
        };
        if changed {
            log::info!("virtual camera {}", if in_use { "in use" } else { "idle" });
            shared.wake.notify_all();
            on_usage(in_use);
        }
    }
}

/// While an app streams, makes sure it keeps getting frames: the
/// placeholder at the frame rate until the Mac's first frame arrives, and the
/// newest frame again if the Mac's stream stalls. Apps treat a camera that
/// sends nothing as broken. A frame that is merely a little late (network
/// jitter) is not a stall: repeating the old one just before it would stutter.
fn keep_frames(shared: &Shared) {
    let interval = shared.format.frame_interval();
    let stall = (interval * 5 / 2).max(Duration::from_millis(100));
    let mut state = shared.state.lock().unwrap();
    while !shared.stop.load(Ordering::Relaxed) {
        if !state.in_use {
            state = shared.wake.wait_timeout(state, Duration::from_millis(500)).unwrap().0;
            continue;
        }
        let due = if state.frame.is_empty() {
            state.last_write + interval
        } else {
            (state.last_push + stall).max(state.last_write + interval)
        };
        let now = Instant::now();
        if now >= due {
            let frame = std::mem::take(&mut state.frame);
            if frame.is_empty() {
                let placeholder = std::mem::take(&mut state.placeholder);
                shared.write(&placeholder, &mut state);
                state.placeholder = placeholder;
            } else {
                shared.write(&frame, &mut state);
            }
            state.frame = frame;
            continue;
        }
        state = shared.wake.wait_timeout(state, due - now).unwrap().0;
    }
}
