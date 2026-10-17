//! The client's core: keeps a session with the paired Mac and, while it
//! lasts, the virtual camera and microphone, the media receiver and the
//! default input. The tray (or the headless `run` command) drives it with
//! [`Command`]s and shows its [`Status`].
//!
//! While the Mac is connected:
//! - "MacBook Camera" exists and "MacBook Microphone" is the default input;
//! - an app opening either device makes the client send `media`, so the
//!   Mac's camera or microphone runs only while it's used (protocol.md 9).
//!
//! When the connection drops, all of that goes away again and the core
//! reconnects: the last address first, then wherever Bonjour finds the Mac.

pub mod discovery;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::audio::{DefaultInput, PipeWire};
use crate::media::{MediaEvent, Receiver, Sinks};
use crate::proto::pairing::{self, ClientInfo, PairingAttempt};
use crate::proto::session::{self, SessionError};
use crate::proto::{CaptureState, ClientMsg, DeviceState, ServerMsg, VideoFormat};
use crate::store::{PairedMac, Store, hex_encode};
use crate::vcam::{self, CameraFormat, VirtualCamera};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(5);
const SILENCE_LIMIT: Duration = Duration::from_secs(15);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const BONJOUR_WAIT: Duration = Duration::from_secs(3);

/// What the UI shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Starting,
    NotPaired,
    /// Paired, but the Mac isn't reachable right now.
    Searching { mac: String, last_error: Option<String> },
    Connected { mac: String, camera: Device, mic: Device },
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
    /// A new pairing was saved, or a setting changed: (re)connect.
    Paired,
    /// Remove the pairing (on the Mac too, if it's connected).
    Forget,
    Quit,
}

/// Problems with the local setup that stop the client from working.
#[derive(Debug, Clone)]
pub struct SetupProblems(pub Vec<String>);

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

/// Runs until [`Command::Quit`]: connects to the paired Mac, keeps the
/// session and its devices, and reconnects after drops.
pub async fn run(core: Arc<Core>, status: watch::Sender<Status>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let mac = match core.store.paired_mac() {
            Ok(Some(mac)) => mac,
            Ok(None) => {
                status.send_replace(Status::NotPaired);
                match commands.recv().await {
                    Some(Command::Quit) | None => return,
                    Some(_) => continue,
                }
            }
            Err(e) => {
                log::error!("reading the paired Mac: {e:#}");
                status.send_replace(Status::NotPaired);
                match commands.recv().await {
                    Some(Command::Quit) | None => return,
                    Some(Command::Forget) => {
                        let _ = core.store.forget_paired_mac();
                        continue;
                    }
                    Some(_) => continue,
                }
            }
        };
        if !matches!(*status.borrow(), Status::Searching { .. }) {
            status.send_replace(Status::Searching { mac: mac.name.clone(), last_error: None });
        }

        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let attempt = connect_and_serve(&core, &mac, &status, ctl_rx);
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

        match pending {
            Some(Command::Quit) => return,
            Some(Command::Forget) => {
                if let Err(e) = core.store.forget_paired_mac() {
                    log::error!("forgetting the Mac: {e:#}");
                }
                backoff = Duration::from_secs(1);
                continue;
            }
            Some(Command::Paired) => {
                backoff = Duration::from_secs(1);
                continue;
            }
            None => {}
        }

        let last_error = match outcome {
            Ok(Ended::Served) => {
                backoff = Duration::from_secs(1);
                None
            }
            Ok(Ended::Stopped) => None,
            Ok(Ended::Unpaired) => {
                log::info!("the Mac removed this computer");
                let _ = core.store.forget_paired_mac();
                continue;
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
        status.send_replace(Status::Searching { mac: mac.name.clone(), last_error });
        // Wait, but react to commands meanwhile.
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            cmd = commands.recv() => match cmd {
                Some(Command::Quit) | None => return,
                Some(Command::Forget) => { let _ = core.store.forget_paired_mac(); }
                Some(Command::Paired) => {}
            },
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

enum Ctl {
    Unpair,
    Quit,
}

enum Ended {
    /// A session ran and ended (the Mac quit, the network dropped...).
    Served,
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
    serve(core, mac, status, stream, peer, local.ip(), ctl).await
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
    CameraUsed(bool),
    MicUsed(bool),
    Media(MediaEvent),
}

/// Removes the virtual microphone however the session ends: like the
/// camera, it exists only while the Mac is connected.
struct MicNodeGuard<'a>(&'a Core);

impl Drop for MicNodeGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.0.pw.stop_mic() {
            log::warn!("removing the virtual microphone: {e:#}");
        }
    }
}

/// Undoes the default-input change however the session ends.
struct DefaultInputGuard<'a>(&'a Core);

impl Drop for DefaultInputGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.0.default_input.give_back(&self.0.pw) {
            log::warn!("restoring the default input: {e:#}");
        }
    }
}

/// One session, from the handshake until it ends.
async fn serve(
    core: &Arc<Core>,
    mac: &PairedMac,
    status: &watch::Sender<Status>,
    stream: TcpStream,
    peer: SocketAddr,
    local_ip: IpAddr,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
) -> Result<Ended> {
    let me = core.store.identity()?;
    // Read per session, so a changed setting applies at the next connection.
    let video: VideoFormat = core.store.settings().unwrap_or_default().video;
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
        _ => unreachable!("nothing else runs yet"),
    };
    log::info!("connected to {mac_name} at {peer}");
    remember_address(core, mac, peer);

    // The devices exist from now on, until this function returns.
    let camera = match core.camera_nr {
        Some(nr) => {
            let format = CameraFormat { width: video.width, height: video.height, fps: video.fps };
            let tx = tx.clone();
            match VirtualCamera::open(nr, format, move |used| {
                let _ = tx.send(Event::CameraUsed(used));
            }) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    log::error!("opening the virtual camera: {e:#}");
                    None
                }
            }
        }
        None => None,
    };
    let mic = Arc::new(core.pw.start_mic({
        let tx = tx.clone();
        move |used| {
            let _ = tx.send(Event::MicUsed(used));
        }
    })?);
    // Declared first, so it drops last: the default input goes back before
    // the microphone node it points at is removed.
    let _mic_node = MicNodeGuard(core);
    core.default_input.take_over(&core.pw)?;
    let _default_input = DefaultInputGuard(core);
    let receiver = Receiver::new(
        Some(local_ip),
        video,
        Sinks {
            video: Box::new({
                let camera = camera.clone();
                move |frame| {
                    if let Some(c) = &camera {
                        c.push_frame(frame);
                    }
                }
            }),
            audio: Box::new({
                let mic = mic.clone();
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

    let mut cam_state = Device::default();
    let mut mic_state = Device::default();
    let publish = |cam: &Device, mic: &Device| {
        status.send_replace(Status::Connected { mac: mac_name.clone(), camera: cam.clone(), mic: mic.clone() });
    };
    publish(&cam_state, &mic_state);
    writer.send(&ClientMsg::Media { video: false, audio: false }).await?;

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_id = 0u64;
    let mut last_heard = Instant::now();
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
                                    if let Some(c) = &camera { c.clear(); }
                                }
                                publish(&cam_state, &mic_state);
                            }
                            ServerMsg::Bye { reason } => {
                                log::info!("{mac_name} said bye: {reason}");
                                return Ok(if reason == "unpaired" { Ended::Unpaired } else { Ended::Served });
                            }
                            ServerMsg::Error { code, message } => {
                                return Err(anyhow!("the Mac reported an error: {code} {}", message.unwrap_or_default()));
                            }
                            ServerMsg::Pong { .. } | ServerMsg::Welcome { .. } | ServerMsg::Unknown => {}
                        }
                    }
                    Event::CameraUsed(used) => {
                        cam_state.used_here = used;
                        writer.send(&ClientMsg::Media { video: cam_state.used_here, audio: mic_state.used_here }).await?;
                        publish(&cam_state, &mic_state);
                    }
                    Event::MicUsed(used) => {
                        mic_state.used_here = used;
                        writer.send(&ClientMsg::Media { video: cam_state.used_here, audio: mic_state.used_here }).await?;
                        publish(&cam_state, &mic_state);
                    }
                    Event::Media(MediaEvent::Answer(sdp)) => writer.send(&ClientMsg::Answer { sdp }).await?,
                    Event::Media(MediaEvent::Candidate(c)) => writer.send(&ClientMsg::Ice(c)).await?,
                    Event::Media(MediaEvent::State(state)) => log::info!("media {state}"),
                    Event::Media(MediaEvent::Error(e)) => return Err(anyhow!("media: {e}")),
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT {
                    return Err(anyhow!("the Mac stopped answering"));
                }
                ping_id += 1;
                writer.send(&ClientMsg::Ping { id: ping_id }).await?;
            }
            ctl_msg = ctl.recv() => match ctl_msg {
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
