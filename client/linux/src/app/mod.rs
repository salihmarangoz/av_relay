//! The client's core: keeps a session with the paired Mac, the virtual
//! camera and microphone, the media receiver and the default input. The
//! tray (or the headless `run` command) drives it with [`Command`]s and
//! shows its [`Status`].
//!
//! While the Mac is connected:
//! - "MacBook Camera" exists and "MacBook Microphone" is the default input;
//! - an app opening either device makes the client send `media`, so the
//!   Mac's camera or microphone runs only while it's used (protocol.md 9).
//!
//! When the connection drops, the devices stay for a grace period (5 min),
//! the camera showing a "Waiting for <Mac>…" picture and the microphone
//! silence, so a short drop goes unnoticed by apps; with "Keep virtual
//! devices on" they stay for good. Meanwhile the core reconnects: the last
//! address first, then wherever Bonjour finds the Mac.

pub mod discovery;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::audio::{self, DefaultInput, MicFeed, MicStats, PipeWire};
use crate::media::{InboundStats, MediaEvent, Receiver, Sinks};
use crate::proto::pairing::{self, ClientInfo, PairingAttempt};
use crate::proto::session::{self, SessionError};
use crate::proto::{CaptureState, ClientMsg, DeviceState, ServerMsg, VideoFormat};
use crate::store::{PairedMac, Store, hex_encode};
use crate::vcam::{self, CamStats, CameraFormat, VirtualCamera};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(5);
/// How often a session logs its media statistics while an app uses a device.
const STATS_EVERY: Duration = Duration::from_secs(10);
/// How often the video delay follows the mic's queue (see `video_delay`).
const SYNC_EVERY: Duration = Duration::from_secs(1);
const SILENCE_LIMIT: Duration = Duration::from_secs(15);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// While the devices are held for the Mac, someone is likely waiting on it
/// (a call showing the waiting picture): retry more often. An attempt costs
/// a TCP connect on the LAN, and a Mac that refuses answers at once.
const BACKOFF_MAX_HELD: Duration = Duration::from_secs(10);
const BONJOUR_WAIT: Duration = Duration::from_secs(3);
/// webrtcbin doesn't recover well through ICE restarts: when media stays
/// failed this long, start a fresh session instead (the Mac would give up
/// after 30 s anyway, protocol.md 10).
const MEDIA_FAILED_LIMIT: Duration = Duration::from_secs(10);
/// How long the devices outlive a connection that dropped.
fn grace_period() -> Duration {
    // Overridable for tests only.
    std::env::var("AV_RELAY_GRACE_SECS").ok().and_then(|s| s.parse().ok()).map(Duration::from_secs).unwrap_or(Duration::from_secs(300))
}

/// What the UI shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Starting,
    NotPaired { devices: Hold },
    /// Paired, but the Mac isn't reachable right now.
    Searching { mac: String, last_error: Option<String>, devices: Hold },
    Connected { mac: String, camera: Device, mic: Device },
}

/// Whether the virtual devices exist while the Mac is away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    None,
    /// Until this time (seconds since the Unix epoch).
    Until(u64),
    /// "Keep virtual devices on".
    Always,
}

/// One of the Mac's devices as the UI shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Device {
    /// An app on this computer is using the virtual device.
    pub used_here: bool,
    /// The Mac's capture state and device name.
    pub state: Option<CaptureState>,
    pub name: Option<String>,
    pub error: Option<String>,
}

/// What the UI asks of the core.
#[derive(Debug)]
pub enum Command {
    /// A new pairing was saved: connect to it.
    Paired,
    /// The camera quality changed (saved in the settings): it applies when no
    /// app uses the camera, with a fresh session.
    VideoChanged,
    /// "Keep virtual devices on" changed (saved in the settings).
    KeepDevicesChanged,
    /// Remove the pairing (on the Mac too, if it's connected).
    Forget,
    Quit,
}

pub struct Core {
    store: Store,
    pw: PipeWire,
    default_input: DefaultInput,
    camera_nr: Option<u32>,
    problems: Vec<String>,
}

impl Core {
    /// Sets up what doesn't depend on the Mac. Problems with the camera
    /// (v4l2loopback) are reported, not fatal: the microphone still works.
    pub fn new(store: Store) -> Result<Arc<Self>> {
        let pw = PipeWire::start().context("connecting to PipeWire")?;
        let default_input = DefaultInput::new()?;
        // A crash may have left the virtual mic as the default input.
        if let Err(e) = default_input.give_back(&pw) {
            log::warn!("restoring the default input: {e:#}");
        }
        let mut problems = Vec::new();
        let camera_nr = match vcam::check_loopback().and_then(|_| vcam::ensure_device(vcam::CARD_LABEL)) {
            Ok(nr) => Some(nr),
            Err(e) => {
                log::error!("virtual camera unavailable: {e:#}");
                problems.push(format!("Camera: {e:#}"));
                None
            }
        };
        Ok(Arc::new(Self { store, pw, default_input, camera_nr, problems }))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn client_info() -> ClientInfo {
        let name = std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "Linux".into());
        ClientInfo { name, app: format!("av-relay {} linux", env!("CARGO_PKG_VERSION")) }
    }
}

/// Starts pairing with the Mac at `host:port`; the UI then asks for the
/// code and calls [`finish_pairing`].
pub async fn start_pairing(host: &str, port: u16) -> Result<PairingAttempt<TcpStream>> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| anyhow!("No answer from {host}:{port}. Check the address and that AV Relay runs on the Mac."))?
        .map_err(|e| anyhow!("Couldn't connect to {host}:{port}: {e}"))?;
    stream.set_nodelay(true)?;
    Ok(pairing::start(stream).await?)
}

/// Completes pairing with the code and saves the Mac.
pub async fn finish_pairing(core: &Core, attempt: PairingAttempt<TcpStream>, host: &str, port: u16, code: &str) -> Result<String> {
    let me = core.store.identity()?;
    let server = attempt.finish(code, &me, &Core::client_info()).await?;
    core.store.save_paired_mac(&PairedMac {
        public_key: hex_encode(&server.public_key),
        device_id: server.device_id.clone(),
        name: server.name.clone(),
        address: host.to_string(),
        port,
        paired_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    })?;
    log::info!("paired with {} ({})", server.name, server.device_id);
    Ok(server.name)
}

/// The virtual camera and microphone, with the default input pointing at the
/// microphone. They outlive sessions (grace period, "keep on"); dropping
/// them gives the default input back first, then removes the devices.
struct Devices {
    core: Arc<Core>,
    camera: Option<Arc<VirtualCamera>>,
    format: VideoFormat,
    mic: Arc<MicFeed>,
    camera_used: Arc<AtomicBool>,
    mic_used: Arc<AtomicBool>,
    /// Pings whoever watches usage: the session, or the idle loop.
    notify: mpsc::UnboundedSender<()>,
}

impl Devices {
    fn create(core: &Arc<Core>, format: VideoFormat, notify: &mpsc::UnboundedSender<()>) -> Result<Self> {
        let camera_used = Arc::new(AtomicBool::new(false));
        let mic_used = Arc::new(AtomicBool::new(false));
        let mic = Arc::new(core.pw.start_mic({
            let (flag, notify) = (mic_used.clone(), notify.clone());
            move |used| {
                flag.store(used, Ordering::Relaxed);
                let _ = notify.send(());
            }
        })?);
        if let Err(e) = core.default_input.take_over(&core.pw) {
            let _ = core.pw.stop_mic();
            return Err(e);
        }
        let camera = open_camera(core, format, &camera_used, notify);
        log::info!("virtual devices on");
        Ok(Self { core: core.clone(), camera, format, mic, camera_used, mic_used, notify: notify.clone() })
    }

    fn camera_used(&self) -> bool {
        self.camera_used.load(Ordering::Relaxed)
    }

    fn mic_used(&self) -> bool {
        self.mic_used.load(Ordering::Relaxed)
    }

    /// The Mac is here: black until its first frame.
    fn show_live(&self) {
        if let Some(c) = &self.camera {
            c.set_placeholder(None);
        }
    }

    /// The Mac is away: the waiting picture.
    fn show_waiting(&self, mac: Option<&str>) {
        let Some(c) = &self.camera else { return };
        let format = CameraFormat { width: self.format.width, height: self.format.height, fps: self.format.fps };
        match vcam::waiting_frame(format, mac) {
            Ok(frame) => c.set_placeholder(Some(frame)),
            Err(e) => log::warn!("drawing the waiting picture: {e:#}"),
        }
        c.clear();
    }

    /// Reopens the camera in `want` if it differs and no app uses it now
    /// (its format is fixed while one does). Only between sessions: the
    /// media receiver holds the camera while one runs. Returns whether the
    /// camera now has `want`.
    fn apply_format(&mut self, want: VideoFormat) -> bool {
        if want == self.format {
            return true;
        }
        if self.camera_used() {
            return false;
        }
        self.camera = None;
        self.format = want;
        self.camera = open_camera(&self.core, want, &self.camera_used, &self.notify);
        log::info!("virtual camera now {}x{} at {} fps", want.width, want.height, want.fps);
        true
    }
}

fn open_camera(
    core: &Core,
    format: VideoFormat,
    used: &Arc<AtomicBool>,
    notify: &mpsc::UnboundedSender<()>,
) -> Option<Arc<VirtualCamera>> {
    let nr = core.camera_nr?;
    let camera_format = CameraFormat { width: format.width, height: format.height, fps: format.fps };
    let (flag, notify) = (used.clone(), notify.clone());
    match VirtualCamera::open(nr, camera_format, move |in_use| {
        flag.store(in_use, Ordering::Relaxed);
        let _ = notify.send(());
    }) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            log::error!("opening the virtual camera: {e:#}");
            None
        }
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        // The default input goes back before the node it points at goes.
        if let Err(e) = self.core.default_input.give_back(&self.core.pw) {
            log::warn!("restoring the default input: {e:#}");
        }
        if let Err(e) = self.core.pw.stop_mic() {
            log::warn!("removing the virtual microphone: {e:#}");
        }
        log::info!("virtual devices off");
        // The camera closes when its last reference (here) goes.
    }
}

/// What the core keeps between sessions.
struct Kept {
    devices: Option<Devices>,
    /// When the devices go away, if nothing changes.
    until: Option<Instant>,
    notify_tx: mpsc::UnboundedSender<()>,
    notify_rx: mpsc::UnboundedReceiver<()>,
}

impl Kept {
    fn hold(&self, keep: bool) -> Hold {
        match (&self.devices, keep, self.until) {
            (None, _, _) => Hold::None,
            (Some(_), true, _) => Hold::Always,
            (Some(_), false, Some(t)) => {
                let left = t.saturating_duration_since(Instant::now());
                Hold::Until((SystemTime::now() + left).duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
            }
            (Some(_), false, None) => Hold::Always,
        }
    }

    /// Between sessions: "keep on" makes the devices exist; otherwise they
    /// stay until the grace period ends. A pending quality change applies
    /// once no app uses the camera.
    fn settle(&mut self, core: &Arc<Core>, mac: Option<&str>) {
        let settings = core.store.settings().unwrap_or_default();
        if settings.keep_devices {
            self.until = None;
            if self.devices.is_none() {
                match Devices::create(core, settings.video, &self.notify_tx) {
                    Ok(d) => self.devices = Some(d),
                    Err(e) => log::error!("creating the virtual devices: {e:#}"),
                }
            }
        } else if self.devices.is_some() && self.until.is_none() {
            self.until = Some(Instant::now() + grace_period());
        }
        if let Some(d) = &mut self.devices {
            d.apply_format(settings.video);
            d.show_waiting(mac);
        }
    }

    fn expire(&mut self) {
        if self.until.is_some_and(|t| Instant::now() >= t) {
            log::info!("the Mac stayed away; removing the virtual devices");
            self.devices = None;
            self.until = None;
        }
    }
}

/// Runs until [`Command::Quit`]: connects to the paired Mac, keeps the
/// session and the devices, and reconnects after drops.
pub async fn run(core: Arc<Core>, status: watch::Sender<Status>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let (notify_tx, notify_rx) = mpsc::unbounded_channel();
    let mut kept = Kept { devices: None, until: None, notify_tx, notify_rx };
    let mut backoff = Duration::from_secs(1);
    loop {
        let keep = core.store.settings().unwrap_or_default().keep_devices;
        let mac = match core.store.paired_mac() {
            Ok(Some(mac)) => mac,
            other => {
                if let Err(e) = other {
                    log::error!("reading the paired Mac: {e:#}");
                }
                kept.settle(&core, None);
                status.send_replace(Status::NotPaired { devices: kept.hold(keep) });
                match idle(&core, &mut kept, &mut commands, None, None, &status).await {
                    Some(Command::Quit) | None => return,
                    _ => continue,
                }
            }
        };
        kept.settle(&core, Some(&mac.name));
        let hold = kept.hold(keep);
        if !matches!(*status.borrow(), Status::Searching { .. }) {
            status.send_replace(Status::Searching { mac: mac.name.clone(), last_error: None, devices: hold });
        }

        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        // Scoped: the session borrows `kept` until its future is dropped.
        let (outcome, pending) = {
        let attempt = connect_and_serve(&core, &mac, &status, ctl_rx, &mut kept);
        tokio::pin!(attempt);
        let mut pending: Option<Command> = None;
        let outcome = loop {
            tokio::select! {
                outcome = &mut attempt => break outcome,
                cmd = commands.recv() => match cmd {
                    // Let a live session say goodbye (or unpair) itself.
                    Some(Command::Quit) | None => { let _ = ctl_tx.send(Ctl::Quit); pending = Some(Command::Quit); }
                    Some(Command::Forget) => { let _ = ctl_tx.send(Ctl::Unpair); pending = Some(Command::Forget); }
                    Some(Command::Paired) => { let _ = ctl_tx.send(Ctl::Quit); pending = Some(Command::Paired); }
                    // Applied by the session (when the camera is idle) or after it.
                    Some(Command::VideoChanged) => { let _ = ctl_tx.send(Ctl::VideoChanged); continue; }
                    Some(Command::KeepDevicesChanged) => continue,
                },
            }
            // A session gets a moment to finish politely; an attempt that
            // isn't connected yet just stops.
            if pending.is_some() {
                match tokio::time::timeout(Duration::from_secs(3), &mut attempt).await {
                    Ok(outcome) => break outcome,
                    Err(_) => break Ok(Ended::Stopped),
                }
            }
        };
        (outcome, pending)
        };

        match pending {
            Some(Command::Quit) => return,
            Some(Command::Forget) => {
                if let Err(e) = core.store.forget_paired_mac() {
                    log::error!("forgetting the Mac: {e:#}");
                }
                // Forgetting the Mac ends its devices too (unless kept on).
                if !core.store.settings().unwrap_or_default().keep_devices {
                    kept.devices = None;
                    kept.until = None;
                }
                backoff = Duration::from_secs(1);
                continue;
            }
            Some(_) => {
                backoff = Duration::from_secs(1);
                continue;
            }
            None => {}
        }

        let last_error = match outcome {
            Ok(Ended::Served) | Ok(Ended::Restart) => {
                backoff = Duration::from_secs(1);
                None
            }
            Ok(Ended::Stopped) => None,
            Ok(Ended::Unpaired) => {
                log::info!("the Mac removed this computer");
                let _ = core.store.forget_paired_mac();
                if !core.store.settings().unwrap_or_default().keep_devices {
                    kept.devices = None;
                    kept.until = None;
                }
                continue;
            }
            Ok(Ended::MacAsleep) => {
                // protocol.md 7: back off; it refuses sessions until it wakes
                // (capped lower below while the devices are held).
                backoff = BACKOFF_MAX;
                None
            }
            Err(e) => {
                log::info!("connection to {}: {e:#}", mac.name);
                if let Some(SessionError::NotPaired | SessionError::AuthFailed) = e.downcast_ref::<SessionError>() {
                    // The Mac no longer knows us, or was reset: pairing again is
                    // the only way; keep trying slowly in case it comes back.
                    backoff = BACKOFF_MAX;
                }
                Some(format!("{e:#}"))
            }
        };
        kept.settle(&core, Some(&mac.name));
        let keep = core.store.settings().unwrap_or_default().keep_devices;
        status.send_replace(Status::Searching { mac: mac.name.clone(), last_error, devices: kept.hold(keep) });
        let cap = if kept.devices.is_some() { BACKOFF_MAX_HELD } else { BACKOFF_MAX };
        backoff = backoff.min(cap);
        // Wait before the next attempt, minding the devices and commands.
        match idle(&core, &mut kept, &mut commands, Some(Instant::now() + backoff), Some(&mac), &status).await {
            Some(Command::Quit) | None => return,
            Some(Command::Forget) => {
                let _ = core.store.forget_paired_mac();
                if !core.store.settings().unwrap_or_default().keep_devices {
                    kept.devices = None;
                    kept.until = None;
                }
            }
            _ => {}
        }
        let cap = if kept.devices.is_some() { BACKOFF_MAX_HELD } else { BACKOFF_MAX };
        backoff = (backoff * 2).min(cap);
    }
}

/// Waits (until `wake`, or for a command when there's no `wake`) while
/// keeping the devices: removes them when their grace period ends, applies
/// "keep on" and quality changes, and follows camera usage. Returns the
/// command that ended the wait, or `Some(Paired)` when `wake` came.
async fn idle(
    core: &Arc<Core>,
    kept: &mut Kept,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    wake: Option<Instant>,
    mac: Option<&PairedMac>,
    status: &watch::Sender<Status>,
) -> Option<Command> {
    let mac_name = mac.map(|m| m.name.clone());
    let publish = |kept: &Kept| {
        let hold = kept.hold(core.store.settings().unwrap_or_default().keep_devices);
        status.send_modify(|s| match s {
            Status::Searching { devices, .. } | Status::NotPaired { devices } => *devices = hold,
            _ => {}
        });
    };
    loop {
        let until = kept.until;
        let deadline = match (wake, until) {
            (Some(w), Some(u)) => Some(w.min(u)),
            (Some(w), None) => Some(w),
            (None, u) => u,
        };
        tokio::select! {
            _ = async { match deadline { Some(d) => tokio::time::sleep_until(d.into()).await, None => std::future::pending().await } } => {
                kept.expire();
                publish(kept);
                if wake.is_some_and(|w| Instant::now() >= w) {
                    return Some(Command::Paired);
                }
            }
            Some(()) = kept.notify_rx.recv() => {
                // A quality change waits for the camera to be free.
                kept.settle(core, mac_name.as_deref());
            }
            cmd = commands.recv() => match cmd {
                Some(Command::VideoChanged) | Some(Command::KeepDevicesChanged) => {
                    kept.settle(core, mac_name.as_deref());
                    if !core.store.settings().unwrap_or_default().keep_devices && kept.devices.is_some() && kept.until.is_none() {
                        kept.until = Some(Instant::now() + grace_period());
                    }
                    publish(kept);
                }
                other => return other,
            },
        }
    }
}

enum Ctl {
    Unpair,
    Quit,
    VideoChanged,
}

enum Ended {
    /// A session ran and ended (the Mac quit, the network dropped...).
    Served,
    /// Ended to apply a setting; reconnect at once.
    Restart,
    /// The Mac is going to sleep: wait longer before reconnecting.
    MacAsleep,
    /// Stopped on request.
    Stopped,
    /// The Mac (or this side) removed the pairing.
    Unpaired,
}

/// Connects (last address, then Bonjour) and serves one session.
async fn connect_and_serve(
    core: &Arc<Core>,
    mac: &PairedMac,
    status: &watch::Sender<Status>,
    ctl: mpsc::UnboundedReceiver<Ctl>,
    kept: &mut Kept,
) -> Result<Ended> {
    let stream = match connect(&mac.address, mac.port).await {
        Ok(s) => s,
        Err(first) => {
            let id = mac.device_id.clone();
            let found = tokio::task::spawn_blocking(move || discovery::find_mac(&id, BONJOUR_WAIT)).await?;
            match found {
                Some((ip, port)) if (ip.to_string(), port) != (mac.address.clone(), mac.port) => {
                    log::info!("Bonjour found {} at {ip}:{port}", mac.name);
                    connect(&ip.to_string(), port).await?
                }
                _ => return Err(first),
            }
        }
    };
    let peer = stream.peer_addr()?;
    let local = stream.local_addr()?;
    serve(core, mac, status, stream, peer, local.ip(), ctl, kept).await
}

async fn connect(host: &str, port: u16) -> Result<TcpStream> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| anyhow!("the Mac didn't answer at {host}:{port}"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

enum Event {
    Server(Result<ServerMsg, SessionError>),
    Media(MediaEvent),
}

/// One session, from the handshake until it ends.
#[allow(clippy::too_many_arguments)]
async fn serve(
    core: &Arc<Core>,
    mac: &PairedMac,
    status: &watch::Sender<Status>,
    stream: TcpStream,
    peer: SocketAddr,
    local_ip: IpAddr,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    kept: &mut Kept,
) -> Result<Ended> {
    let me = core.store.identity()?;
    // The camera's format if the devices exist (it's fixed while apps use
    // it), else the setting.
    let video: VideoFormat = kept.devices.as_ref().map(|d| d.format).unwrap_or_else(|| core.store.settings().unwrap_or_default().video);
    let (mut reader, mut writer) = session::connect(stream, &me, &mac.key()?).await?;
    let info = Core::client_info();
    writer.send(&ClientMsg::Hello { name: info.name, app: info.app, video }).await?;

    // Reading isn't cancel-safe (a frame read half-way would be lost), so a
    // task of its own reads and forwards; everything else is one select.
    let (tx, mut events) = mpsc::unbounded_channel::<Event>();
    let reader_task = tokio::spawn({
        let tx = tx.clone();
        async move {
            loop {
                let msg = reader.recv().await;
                let failed = msg.is_err();
                if tx.send(Event::Server(msg)).is_err() || failed {
                    break;
                }
            }
        }
    });
    struct AbortOnDrop(tokio::task::JoinHandle<()>);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _reader_task = AbortOnDrop(reader_task);

    let welcome = tokio::time::timeout(WELCOME_TIMEOUT, events.recv())
        .await
        .map_err(|_| anyhow!("the Mac didn't welcome us"))?
        .context("the connection closed")?;
    let mac_name = match welcome {
        Event::Server(Ok(ServerMsg::Welcome { name, .. })) => name,
        Event::Server(Err(e)) => return Err(e.into()),
        Event::Server(Ok(other)) => return Err(anyhow!("expected welcome, got {other:?}")),
        Event::Media(_) => unreachable!("no media yet"),
    };
    log::info!("connected to {mac_name} at {peer}");
    remember_address(core, mac, peer);

    // The devices exist from now on (they may already, from before).
    if kept.devices.is_none() {
        kept.devices = Some(Devices::create(core, video, &kept.notify_tx)?);
    }
    kept.until = None;
    let devices = kept.devices.as_ref().expect("just made sure");
    devices.show_live();
    let receiver = Receiver::new(
        Some(local_ip),
        video,
        Sinks {
            video: Box::new({
                let camera = devices.camera.clone();
                move |frame| {
                    if let Some(c) = &camera {
                        c.push_frame(frame);
                    }
                }
            }),
            audio: Box::new({
                let mic = devices.mic.clone();
                move |samples| mic.push(samples)
            }),
        },
        {
            let tx = tx.clone();
            move |e| {
                let _ = tx.send(Event::Media(e));
            }
        },
    )?;

    let mut cam_state = Device { used_here: devices.camera_used(), ..Default::default() };
    let mut mic_state = Device { used_here: devices.mic_used(), ..Default::default() };
    let publish = |cam: &Device, mic: &Device| {
        status.send_replace(Status::Connected { mac: mac_name.clone(), camera: cam.clone(), mic: mic.clone() });
    };
    publish(&cam_state, &mic_state);
    // An app may still hold a device from before a drop: ask at once.
    writer.send(&ClientMsg::Media { video: cam_state.used_here, audio: mic_state.used_here }).await?;
    let mut video_changed = false;

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_id = 0u64;
    let mut last_heard = Instant::now();
    let mut media_failed_since: Option<Instant> = None;
    let mut stats_tick = tokio::time::interval(STATS_EVERY);
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut prev_inbound = Vec::<InboundStats>::new();
    let mut sync_tick = tokio::time::interval(SYNC_EVERY);
    sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { return Ok(Ended::Served) };
                match event {
                    Event::Server(msg) => {
                        last_heard = Instant::now();
                        match msg? {
                            ServerMsg::Offer { sdp } => receiver.handle_offer(&sdp)?,
                            ServerMsg::Ice(c) => receiver.add_candidate(&c),
                            ServerMsg::MediaState { video, audio } => {
                                update_device(&mut cam_state, &video);
                                update_device(&mut mic_state, &audio);
                                // Without the Mac's camera, show the placeholder,
                                // not the last frame frozen.
                                if video.state != CaptureState::On {
                                    if let Some(c) = &devices.camera { c.clear(); }
                                }
                                publish(&cam_state, &mic_state);
                            }
                            ServerMsg::Bye { reason } => {
                                log::info!("{mac_name} said bye: {reason}");
                                return Ok(match reason.as_str() {
                                    "unpaired" => Ended::Unpaired,
                                    "sleep" => Ended::MacAsleep,
                                    _ => Ended::Served,
                                });
                            }
                            ServerMsg::Error { code, message } => {
                                return Err(anyhow!("the Mac reported an error: {code} {}", message.unwrap_or_default()));
                            }
                            ServerMsg::Pong { .. } | ServerMsg::Welcome { .. } | ServerMsg::Unknown => {}
                        }
                    }
                    Event::Media(MediaEvent::Answer(sdp)) => writer.send(&ClientMsg::Answer { sdp }).await?,
                    Event::Media(MediaEvent::Candidate(c)) => writer.send(&ClientMsg::Ice(c)).await?,
                    Event::Media(MediaEvent::State(state)) => {
                        log::info!("media {state}");
                        match state.as_str() {
                            "failed" => { media_failed_since.get_or_insert_with(Instant::now); }
                            "connected" => media_failed_since = None,
                            _ => {}
                        }
                    }
                    Event::Media(MediaEvent::Error(e)) => return Err(anyhow!("media: {e}")),
                }
            }
            Some(()) = kept.notify_rx.recv() => {
                let (cam, mic) = (devices.camera_used(), devices.mic_used());
                if (cam, mic) != (cam_state.used_here, mic_state.used_here) {
                    cam_state.used_here = cam;
                    mic_state.used_here = mic;
                    writer.send(&ClientMsg::Media { video: cam, audio: mic }).await?;
                    publish(&cam_state, &mic_state);
                }
                // A new quality applies with a fresh session once the camera is free.
                if video_changed && !cam {
                    let _ = writer.send(&ClientMsg::Bye { reason: "quit".into() }).await;
                    return Ok(Ended::Restart);
                }
            }
            _ = sync_tick.tick() => {
                receiver.set_video_delay(video_delay(devices.mic_used(), devices.mic.counters().target_frames()));
            }
            _ = stats_tick.tick() => {
                // Taking them resets the counts, also while nothing is in use.
                let cam = devices.camera.as_ref().map(|c| c.take_stats());
                let mic = devices.mic.counters().take();
                let inbound = receiver.stats();
                if devices.camera_used() || devices.mic_used() {
                    log::info!("{}", stats_line(&inbound, &prev_inbound, cam, mic));
                }
                prev_inbound = inbound;
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT {
                    return Err(anyhow!("the Mac stopped answering"));
                }
                if media_failed_since.is_some_and(|t| t.elapsed() > MEDIA_FAILED_LIMIT) {
                    return Err(anyhow!("the media connection failed and didn't recover"));
                }
                ping_id += 1;
                writer.send(&ClientMsg::Ping { id: ping_id }).await?;
            }
            ctl_msg = ctl.recv() => match ctl_msg {
                Some(Ctl::VideoChanged) => {
                    video_changed = core.store.settings().unwrap_or_default().video != video;
                    if video_changed && !devices.camera_used() {
                        let _ = writer.send(&ClientMsg::Bye { reason: "quit".into() }).await;
                        return Ok(Ended::Restart);
                    }
                }
                Some(Ctl::Unpair) => {
                    writer.send(&ClientMsg::Unpair).await?;
                    // The Mac answers bye(unpaired); don't wait long for it.
                    let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
                    return Ok(Ended::Unpaired);
                }
                Some(Ctl::Quit) | None => {
                    let _ = writer.send(&ClientMsg::Bye { reason: "quit".into() }).await;
                    return Ok(Ended::Stopped);
                }
            },
        }
    }
}

/// How much longer than audio to hold video back: about as long as audio
/// waits in the mic's queue (which grows on a stalling network), so the two
/// stay in step. The queue's target counts one chunk for how audio arrives,
/// and video decoding takes a little longer than audio's, hence the minus.
/// Without the mic in use, video only waits its own minimum.
fn video_delay(mic_used: bool, mic_target_frames: u64) -> Duration {
    const VIDEO_HEAD_START: Duration = Duration::from_millis(30);
    if !mic_used {
        return Duration::ZERO;
    }
    Duration::from_micros(mic_target_frames * 1_000_000 / audio::RATE as u64).saturating_sub(VIDEO_HEAD_START)
}

/// One log line about the last `STATS_EVERY`: what arrived over the
/// network (counts since `prev`), and how the camera and mic were fed.
fn stats_line(now: &[InboundStats], prev: &[InboundStats], cam: Option<CamStats>, mic: MicStats) -> String {
    let ms = |frames: u64| frames * 1000 / audio::RATE as u64;
    let mut line = String::from("stats:");
    for s in now {
        let p = prev.iter().find(|p| p.ssrc == s.ssrc).cloned().unwrap_or_default();
        line += &format!(
            " {} rx {} lost {} jitter {:.1} ms nack {} pli {} jb-lost {} jb-late {} rtx {}/{} |",
            if s.video { "video" } else { "audio" },
            s.packets_received.saturating_sub(p.packets_received),
            s.packets_lost - p.packets_lost,
            s.jitter_ms,
            s.nacks.saturating_sub(p.nacks),
            s.plis.saturating_sub(p.plis),
            s.jb_lost.saturating_sub(p.jb_lost),
            s.jb_late.saturating_sub(p.jb_late),
            s.rtx_ok.saturating_sub(p.rtx_ok),
            s.rtx_requests.saturating_sub(p.rtx_requests),
        );
    }
    if let Some(c) = cam {
        line += &format!(
            " camera {} frames, {} late, max gap {} ms, {} repeated, {} placeholder |",
            c.pushed,
            c.late,
            c.max_gap.as_millis(),
            c.repeated,
            c.placeholder
        );
    }
    line += &format!(
        " mic {} underruns, {} ms silence, {} ms skipped, {} ms overflow, queue {}/{} ms",
        mic.underruns,
        ms(mic.silent_frames),
        ms(mic.skipped_frames),
        ms(mic.overflow_samples),
        ms(mic.fill),
        ms(mic.target)
    );
    line
}

fn update_device(device: &mut Device, state: &DeviceState) {
    device.state = Some(state.state);
    device.name = state.device.clone();
    device.error = state.error.clone();
}

/// Keeps the address that worked, for the next reconnect.
fn remember_address(core: &Core, mac: &PairedMac, peer: SocketAddr) {
    let address = peer.ip().to_string();
    if address != mac.address || peer.port() != mac.port {
        let mut updated = mac.clone();
        updated.address = address;
        updated.port = peer.port();
        if let Err(e) = core.store.save_paired_mac(&updated) {
            log::warn!("saving the Mac's new address: {e:#}");
        }
    }
}
