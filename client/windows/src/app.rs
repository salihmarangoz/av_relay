// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    audio::{AudioFeed, AudioOutput},
    crypto::StaticKey,
    discovery,
    media::{MediaEvent, Receiver, Sinks, delay::PlayoutDelay},
    proto::{
        self, ClientMsg, ServerMsg,
        pairing::{self, ClientInfo},
        session,
    },
    store::{PairedSender, Settings, Store},
    vcam::{Camera, VideoFeed},
};
use anyhow::{Context, Result, bail};
use std::{
    net::IpAddr,
    sync::mpsc as std_mpsc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::mpsc,
    time::{interval, timeout},
};

#[derive(Debug)]
pub enum Command {
    Pair {
        host: String,
        port: u16,
        code: String,
    },
    Discover,
    Enable(bool),
    Video(bool),
    Audio(bool),
    Resolution(proto::VideoFormat),
    AudioDevice(Option<String>),
    StartMinimized(bool),
    Forget,
    Quit,
}
#[derive(Debug)]
pub enum Event {
    Status(String),
    Problem(String),
    Paired(String),
    Found(Vec<discovery::FoundSender>),
    Demand {
        video: bool,
        audio: bool,
    },
    Enabled(bool),
    Connection {
        connected: bool,
        sender: Option<String>,
    },
    Capture {
        video: proto::DeviceState,
        audio: proto::DeviceState,
    },
    Settings(Settings),
}
pub type Commands = mpsc::UnboundedSender<Command>;
pub type Events = std_mpsc::Receiver<Event>;

pub fn start(store: Store) -> Result<(Commands, Events, std::thread::JoinHandle<()>)> {
    let identity = store.identity()?;
    let settings = store.settings()?;
    let sender = store.paired()?;
    let (tx, rx) = mpsc::unbounded_channel();
    let (events, event_rx) = std_mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("av-relay-control".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|rt| {
                    rt.block_on(
                        Core {
                            store,
                            identity,
                            enabled: settings.enabled,
                            settings,
                            sender,
                            video: None,
                            audio: None,
                            commands: rx,
                            events,
                            quitting: false,
                        }
                        .run(),
                    )
                });
            if let Err(e) = result {
                log::error!("receiver: {e:#}");
            }
        })?;
    Ok((tx, event_rx, thread))
}
struct Core {
    store: Store,
    identity: StaticKey,
    settings: Settings,
    sender: Option<PairedSender>,
    enabled: bool,
    video: Option<Camera>,
    audio: Option<AudioOutput>,
    commands: mpsc::UnboundedReceiver<Command>,
    events: std_mpsc::Sender<Event>,
    quitting: bool,
}
impl Core {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
    fn status(&self, text: impl Into<String>) {
        self.emit(Event::Status(text.into()));
    }
    fn problem(&self, error: impl std::fmt::Display) {
        log::warn!("{error}");
        self.emit(Event::Problem(error.to_string()));
    }
    fn demand(&self) -> ClientMsg {
        ClientMsg::Media {
            video: self
                .video
                .as_ref()
                .is_some_and(|c| c.in_use.load(std::sync::atomic::Ordering::Acquire)),
            audio: self.audio.is_some(),
        }
    }
    fn clear(&self) {
        if let Some(v) = &self.video {
            v.feed.clear();
        }
        if let Some(a) = &self.audio {
            a.feed.clear();
        }
    }
    async fn run(mut self) -> Result<()> {
        let mut backoff = 1u64;
        while !self.quitting {
            if !self.enabled || self.sender.is_none() {
                self.emit(Event::Connection {
                    connected: false,
                    sender: self.sender.as_ref().map(|s| s.name.clone()),
                });
                self.status(if self.enabled {
                    "Pair with a sender to connect."
                } else {
                    "AV Relay is disabled."
                });
                let Some(command) = self.commands.recv().await else {
                    break;
                };
                self.command(command).await;
                continue;
            }
            let sender = self.sender.clone().unwrap();
            self.emit(Event::Connection {
                connected: false,
                sender: Some(sender.name.clone()),
            });
            self.status(format!("Connecting to {}…", sender.name));
            // Keep the UI responsive even while a sleeping sender times out.
            let result = tokio::select! {
                result=connect_sender(&sender) => result,
                command=self.commands.recv() => { if let Some(c)=command {self.command(c).await;} else {break;} continue; }
            };
            let result = match result {
                Ok((stream, address, port)) => {
                    match self.session(stream, &sender).await {
                        Ok(()) => {
                            backoff = 1;
                            Ok(())
                        }
                        Err(e) => {
                            // Only authenticated sessions may update the saved address.
                            log::debug!("session at {address}:{port}: {e:#}");
                            Err(e)
                        }
                    }
                }
                Err(e) => Err(e),
            };
            self.clear();
            self.emit(Event::Connection {
                connected: false,
                sender: self.sender.as_ref().map(|s| s.name.clone()),
            });
            if self.quitting {
                break;
            }
            if let Err(e) = result {
                if matches!(
                    e.downcast_ref::<session::SessionError>(),
                    Some(session::SessionError::NotPaired | session::SessionError::AuthFailed)
                ) {
                    self.enabled = false;
                    self.video = None;
                    self.audio = None;
                    self.emit(Event::Enabled(false));
                    self.emit(Event::Demand {
                        video: false,
                        audio: false,
                    });
                    self.problem(format!("{e:#}"));
                }
                if e.downcast_ref::<SenderBye>()
                    .is_some_and(|b| b.0 == "sleep")
                {
                    backoff = 30;
                }
                self.status(format!("Waiting for {}: {e:#}", sender.name));
            }
            if !self.enabled || self.sender.is_none() {
                continue;
            }
            tokio::select! {
                _=tokio::time::sleep(Duration::from_secs(backoff)) => {},
                command=self.commands.recv() => {if let Some(c)=command {self.command(c).await;} else {break;}}
            }
            backoff = (backoff * 2).min(30);
        }
        self.clear();
        Ok(())
    }
    async fn command(&mut self, command: Command) {
        let result:Result<()>=async {
            match command {
                Command::Quit => {self.quitting=true;},
                Command::Enable(enabled) => {self.settings.enabled=enabled;self.store.save_settings(&self.settings)?;self.enabled=enabled; if !enabled {self.video=None; self.audio=None;}},
                Command::Forget => {self.store.forget()?; self.sender=None; self.video=None; self.audio=None;},
                Command::Discover => {
                    let events=self.events.clone();
                    tokio::task::spawn_blocking(move || {let _=events.send(Event::Found(discovery::browse(Duration::from_secs(3))));});
                },
                Command::Pair {host,port,code} => {
                    self.status("Pairing…");
                    let stream=timeout(Duration::from_secs(5),TcpStream::connect((host.as_str(),port))).await??;
                    stream.set_nodelay(true)?;
                    let info=ClientInfo {name:computer_name(),app:app_name()};
                    let paired=pairing::start(stream).await?.finish(&code,&self.identity,&info).await?;
                    let sender=PairedSender {public_key:paired.public_key.iter().map(|x|format!("{x:02x}")).collect(),device_id:paired.device_id,name:paired.name.clone(),address:host,port};
                    self.store.save_paired(&sender)?; self.sender=Some(sender); self.enabled=true;self.settings.enabled=true;self.store.save_settings(&self.settings)?;
                    self.emit(Event::Paired(paired.name));
                },
                Command::Video(enabled) => {
                    if enabled && self.video.is_none() {
                        if !crate::vcam::installed() {bail!("OBS Virtual Camera is not registered. Install OBS Studio, or run its virtualcam-install.bat as administrator.");}
                        self.video=Some(Camera::start(self.settings.video)?);
                    } else if !enabled {self.video=None;}
                },
                Command::Audio(enabled) => {
                    if enabled && self.audio.is_none() {
                        let events=self.events.clone();
                        self.audio=Some(AudioOutput::start(self.settings.audio_device.as_deref(),move |message| {let _=events.send(Event::Problem(message));})?);
                    } else if !enabled {self.audio=None;}
                },
                Command::Resolution(format) => {
                    if !crate::store::VIDEO_PRESETS.iter().any(|(_,preset)|*preset==format) {bail!("Unsupported camera resolution");}
                    self.video=None;
                    self.settings.video=format;
                    self.store.save_settings(&self.settings)?;
                },
                Command::AudioDevice(name) => {self.audio=None; self.settings.audio_device=name; self.store.save_settings(&self.settings)?;},
                Command::StartMinimized(on) => {self.settings.start_minimized=on;self.store.save_settings(&self.settings)?;},
            }
            Ok(())
        }.await;
        if let Err(e) = result {
            self.problem(format!("{e:#}"));
        }
        self.emit(Event::Enabled(self.enabled));
        self.emit(Event::Settings(self.settings.clone()));
        self.emit(Event::Demand {
            video: self.video.is_some(),
            audio: self.audio.is_some(),
        });
    }
    async fn session(&mut self, stream: TcpStream, sender: &PairedSender) -> Result<()> {
        let local: IpAddr = stream.local_addr()?.ip();
        let remote = stream.peer_addr()?;
        let (mut reader, mut writer) =
            session::connect(stream, &self.identity, &sender.key()?).await?;
        writer
            .send(&ClientMsg::Hello {
                name: computer_name(),
                app: app_name(),
                video: self.settings.video,
            })
            .await?;
        match timeout(Duration::from_secs(10), reader.recv()).await?? {
            ServerMsg::Welcome { device_id, .. }
                if device_id.as_deref() == Some(sender.device_id.as_str()) => {}
            other => bail!("Unexpected welcome: {other:?}"),
        }
        let mut saved = sender.clone();
        saved.address = remote.ip().to_string();
        saved.port = remote.port();
        self.store.save_paired(&saved)?;
        self.sender = Some(saved);
        self.status(format!(
            "Connected to {}. Turn Camera or Microphone on when needed.",
            sender.name
        ));
        self.emit(Event::Connection {
            connected: true,
            sender: Some(sender.name.clone()),
        });
        // A partial frame must survive timer and UI events. Reading in a
        // dedicated task avoids cancelling read_exact halfway through a frame.
        let (control_tx, mut control_rx) = mpsc::channel(16);
        let _reader = AbortReader(tokio::spawn(async move {
            loop {
                let message = reader.recv().await;
                let failed = message.is_err();
                if control_tx.send(message).await.is_err() || failed {
                    break;
                }
            }
        }));
        let (media_tx, mut media_rx) = mpsc::unbounded_channel();
        // Feeds are mutable so toggles can change outputs without renegotiating.
        let video_feed = std::sync::Arc::new(std::sync::Mutex::new(
            self.video.as_ref().map(|c| c.feed.clone()),
        ));
        let audio_feed = std::sync::Arc::new(std::sync::Mutex::new(
            self.audio.as_ref().map(|a| a.feed.clone()),
        ));
        let v = video_feed.clone();
        let a = audio_feed.clone();
        let media = Receiver::new(
            Some(local),
            self.settings.video,
            Duration::from_millis(300),
            Sinks {
                video: Box::new(move |frame| {
                    if let Some(feed) = v.lock().unwrap().as_ref() {
                        VideoFeed::push(feed, frame);
                    }
                }),
                audio: Box::new(move |samples| {
                    if let Some(feed) = a.lock().unwrap().as_ref() {
                        AudioFeed::push(feed, samples);
                    }
                }),
            },
            move |event| {
                let _ = media_tx.send(event);
            },
        )?;
        writer.send(&self.demand()).await?;
        let mut ping = interval(Duration::from_secs(5));
        let mut sync = interval(Duration::from_millis(250));
        let mut last = Instant::now();
        let mut ping_id = 0;
        let mut delay = PlayoutDelay::new(Duration::from_millis(300));
        let mut failed: Option<Instant> = None;
        let mut last_demand = self.demand();
        loop {
            tokio::select! {
                message=control_rx.recv() => {
                    let message=message.context("Control connection closed")??; last=Instant::now();
                    match message {
                        ServerMsg::Offer{sdp} => media.handle_offer(&sdp)?,
                        ServerMsg::Ice(candidate) => media.add_candidate(&candidate),
                        ServerMsg::MediaState{video,audio} => {
                            log::info!("Sender capture: camera {:?}, microphone {:?}",video.state,audio.state);
                            if let Some(error)=&video.error {self.problem(format!("Sender camera: {error}"));}
                            if let Some(error)=&audio.error {self.problem(format!("Sender microphone: {error}"));}
                            self.emit(Event::Capture{video,audio});
                        },
                        ServerMsg::Bye{reason} => {
                            if reason=="unpaired" {self.store.forget()?;self.sender=None;self.video=None;self.audio=None;self.emit(Event::Demand{video:false,audio:false});}
                            return Err(SenderBye(reason).into());
                        },
                        ServerMsg::Error{code,message} => bail!("{}",message.unwrap_or(code)),
                        _ => {},
                    }
                },
                event=media_rx.recv() => match event {
                    Some(MediaEvent::Answer(sdp)) => writer.send(&ClientMsg::Answer{sdp}).await?,
                    Some(MediaEvent::Candidate(c)) => writer.send(&ClientMsg::Ice(c)).await?,
                    Some(MediaEvent::Error(error)) => bail!("Media: {error}"),
                    Some(MediaEvent::State(state)) => {
                        if state=="failed" && failed.is_none() {failed=Some(Instant::now()); writer.send(&ClientMsg::IceRestart).await?;}
                        if state=="connected" {failed=None;}
                    },
                    None=>bail!("Media engine closed"),
                },
                command=self.commands.recv() => {
                    let command=command.unwrap_or(Command::Quit);
                    let restart=matches!(&command,Command::Pair{..}|Command::Resolution(_)|Command::Enable(false)|Command::Forget|Command::Quit);
                    if matches!(&command,Command::Forget) {let _=timeout(Duration::from_secs(1),writer.send(&ClientMsg::Unpair)).await;}
                    else if restart {let _=timeout(Duration::from_secs(1),writer.send(&ClientMsg::Bye{reason:"quit".into()})).await;}
                    if restart {self.clear();} self.command(command).await;
                    *video_feed.lock().unwrap()=self.video.as_ref().map(|v|v.feed.clone());
                    *audio_feed.lock().unwrap()=self.audio.as_ref().map(|a|a.feed.clone());
                    if restart {return Ok(());}
                    last_demand=self.demand();
                    writer.send(&last_demand).await?;
                },
                _=ping.tick() => {
                    if last.elapsed()>Duration::from_secs(15) {bail!("Sender did not answer for 15 seconds");}
                    if failed.is_some_and(|t|t.elapsed()>Duration::from_secs(10)) {bail!("WebRTC did not recover");}
                    ping_id+=1; writer.send(&ClientMsg::Ping{id:ping_id}).await?;
                },
                _=sync.tick() => {
                    if self.audio.as_ref().is_some_and(|a|a.failed.load(std::sync::atomic::Ordering::Acquire)) {
                        self.audio=None;
                        *audio_feed.lock().unwrap()=None;
                        self.emit(Event::Demand{video:self.video.is_some(),audio:false});
                        writer.send(&self.demand()).await?;
                    }
                    let demand=self.demand();
                    if demand!=last_demand {
                        log::info!("Capture demand: {demand:?}");
                        writer.send(&demand).await?;
                        last_demand=demand;
                    }
                    let (audio,video)=media.spreads();
                    let spreads:Vec<f64>=audio.into_iter().chain(video).collect();
                    if let Some(d)=delay.update(&spreads,Duration::from_millis(250)) {media.set_delay(d,Duration::from_millis(40));}
                },
            }
        }
    }
}
async fn connect_sender(sender: &PairedSender) -> Result<(TcpStream, String, u16)> {
    if let Ok(Ok(stream)) = timeout(
        Duration::from_secs(3),
        TcpStream::connect((sender.address.as_str(), sender.port)),
    )
    .await
    {
        stream.set_nodelay(true)?;
        return Ok((stream, sender.address.clone(), sender.port));
    }
    let id = sender.device_id.clone();
    let found =
        tokio::task::spawn_blocking(move || discovery::find_sender(&id, Duration::from_secs(3)))
            .await?;
    let (ip, port) = found.context("Sender not found on the local network")?;
    let stream = timeout(Duration::from_secs(3), TcpStream::connect((ip, port))).await??;
    stream.set_nodelay(true)?;
    Ok((stream, ip.to_string(), port))
}
fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into())
}
fn app_name() -> String {
    format!("av-relay {} windows", env!("CARGO_PKG_VERSION"))
}

struct AbortReader(tokio::task::JoinHandle<()>);
impl Drop for AbortReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Sender disconnected: {0}")]
struct SenderBye(String);
