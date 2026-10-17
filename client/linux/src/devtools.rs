//! Developer commands for testing parts of the client on their own:
//! the virtual camera, pairing, and a session against a real sender.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use tokio::net::TcpStream;

use crate::audio::{self, PipeWire};
use crate::media::{MediaEvent, Receiver, Sinks};
use crate::proto::pairing::{self, ClientInfo};
use crate::proto::{ClientMsg, DEFAULT_PORT, ServerMsg, VideoFormat, session};
use crate::store::{PairedSender, Store, hex_encode};
use crate::vcam;

/// Shows a moving test pattern on the virtual camera and logs when apps
/// start and stop using it.
pub fn vcam_test() -> Result<()> {
    let version = vcam::check_loopback()?;
    log::info!("v4l2loopback {}.{}.{}", version.0, version.1, version.2);
    let nr = vcam::ensure_device(vcam::CARD_LABEL)?;
    let format = vcam::CameraFormat { width: 1280, height: 720, fps: 30 };
    let cam = vcam::VirtualCamera::open(nr, format, |in_use| log::info!("usage callback: in_use={in_use}"))?;
    println!("Open {} in an app (e.g. `ffplay {}`); Ctrl+C stops.", cam.path().display(), cam.path().display());

    let start = Instant::now();
    let mut frame = vec![0u8; format.frame_size()];
    loop {
        let t = start.elapsed().as_secs_f32();
        fill_test_pattern(&mut frame, format, t);
        cam.push_frame(&frame);
        std::thread::sleep(Duration::from_secs(1) / format.fps);
    }
}

/// Vertical colour bars scrolling sideways, so a frozen picture is obvious.
fn fill_test_pattern(frame: &mut [u8], format: vcam::CameraFormat, t: f32) {
    const BARS: [(u8, u8, u8); 4] = [(180, 128, 128), (145, 54, 34), (41, 240, 110), (82, 90, 240)];
    let shift = (t * 200.0) as usize;
    let w = format.width as usize;
    for (y, row) in frame.chunks_exact_mut(w * 2).enumerate() {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let bar = ((x * 2 + shift + y / 8) * BARS.len() / w) % BARS.len();
            let (luma, cb, cr) = BARS[bar];
            px.copy_from_slice(&[luma, cb, luma, cr]);
        }
    }
}

/// `vmic-test [SECONDS] [--default] [--drift PPM]`: plays a 440 Hz tone
/// into the virtual microphone, by default 200 ppm fast like a drifting sender
/// clock, and logs when apps start and stop recording. With --default it is
/// the default input meanwhile, and the previous default comes back at the
/// end.
pub fn vmic_test(args: &[String]) -> Result<()> {
    let seconds: u64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(30);
    let make_default = args.iter().any(|a| a == "--default");
    let ppm: f64 = args
        .iter()
        .position(|a| a == "--drift")
        .and_then(|i| args.get(i + 1))
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(200.0);
    let pw = PipeWire::start()?;
    let default_input = audio::DefaultInput::new()?;
    // As the app does at startup: repair a default left behind by a crash.
    default_input.give_back(&pw)?;
    log::info!("configured default input: {}", pw.default_source()?.as_deref().unwrap_or("(automatic)"));
    let feed = pw.start_mic(|in_use| log::info!("usage callback: in_use={in_use}"))?;
    if make_default {
        default_input.take_over(&pw)?;
    }
    println!("Record from '{}' (e.g. `pw-record --target {} out.wav`); stops in {seconds} s.", audio::MIC_DESCRIPTION, audio::MIC_NODE_NAME);

    // 20 ms chunks, like Opus frames, at 48 kHz + the drift.
    let rate = audio::RATE as f64 * (1.0 + ppm / 1e6);
    let chunk = (audio::RATE / 50) as usize;
    let start = Instant::now();
    let mut phase = 0.0f64;
    let mut sent = 0usize;
    let mut buf = vec![0f32; chunk];
    while start.elapsed() < Duration::from_secs(seconds) {
        let due = (start.elapsed().as_secs_f64() * rate) as usize;
        while sent + chunk <= due {
            for s in buf.iter_mut() {
                *s = (phase * std::f64::consts::TAU).sin() as f32 * 0.3;
                phase = (phase + 440.0 / audio::RATE as f64).fract();
            }
            feed.push(&buf);
            sent += chunk;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if make_default {
        default_input.give_back(&pw)?;
    }
    pw.stop_mic()?;
    drop(pw);
    Ok(())
}

/// `media-test [SECONDS]`: a local GStreamer WebRTC sender (moving ball,
/// 720p H.264, and a 440 Hz Opus tone) talks to the real receiver, which
/// feeds the virtual camera (1080p) and microphone: everything but the sender.
pub fn media_test(args: &[String]) -> Result<()> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_sdp as gst_sdp;
    use gstreamer_webrtc as gst_webrtc;
    use std::sync::{Arc, mpsc};

    let seconds: u64 = args.first().map(|s| s.parse()).transpose()?.unwrap_or(20);
    gst::init()?;
    vcam::check_loopback()?;
    let nr = vcam::ensure_device(vcam::CARD_LABEL)?;
    let format = vcam::CameraFormat { width: 1920, height: 1080, fps: 30 };
    // The camera may be busy (the app is running): carry on without it.
    let cam = match vcam::VirtualCamera::open(nr, format, |u| log::info!("camera in use: {u}")) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            log::warn!("no virtual camera for this test: {e:#}");
            None
        }
    };
    let pw = PipeWire::start()?;
    let feed = Arc::new(pw.start_mic(|u| log::info!("mic in use: {u}"))?);

    enum Signal {
        FromReceiver(MediaEvent),
        Offer(String),
        SenderCandidate(u32, String),
    }
    let (tx, rx) = mpsc::channel::<Signal>();
    let frames = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let samples = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let receiver = Receiver::new(
        Some("127.0.0.1".parse()?),
        crate::proto::VideoFormat { width: format.width, height: format.height, fps: format.fps },
        Sinks {
            video: Box::new({
                let cam = cam.clone();
                let frames = frames.clone();
                move |f| {
                    frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let Some(cam) = &cam {
                        cam.push_frame(f)
                    }
                }
            }),
            audio: Box::new({
                let feed = feed.clone();
                let samples = samples.clone();
                move |a| {
                    samples.fetch_add(a.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    feed.push(a)
                }
            }),
        },
        {
            let tx = tx.clone();
            move |e| {
                let _ = tx.send(Signal::FromReceiver(e));
            }
        },
    )?;

    let sender = gst::parse::launch(
        "webrtcbin name=send bundle-policy=max-bundle \
         videotestsrc is-live=true pattern=ball ! video/x-raw,width=1280,height=720,framerate=30/1 ! videoconvert ! \
         x264enc tune=zerolatency speed-preset=ultrafast key-int-max=60 bitrate=4000 ! video/x-h264,profile=constrained-baseline ! \
         rtph264pay config-interval=-1 aggregate-mode=zero-latency ! \
         application/x-rtp,media=video,encoding-name=H264,payload=102,clock-rate=90000 ! send. \
         audiotestsrc is-live=true wave=sine freq=440 volume=0.3 ! audioconvert ! audioresample ! opusenc ! rtpopuspay ! \
         application/x-rtp,media=audio,encoding-name=OPUS,payload=111,clock-rate=48000 ! send.",
    )?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
    let send = sender.by_name("send").context("no webrtcbin")?;
    // Like libwebrtc: transport-wide sequence numbers on every packet, so the
    // receiver must answer with transport-cc feedback.
    for (i, pay) in sender.iterate_elements().into_iter().flatten().filter(|e| e.name().starts_with("rtp")).enumerate() {
        use gstreamer_rtp::prelude::*;
        let ext = gst::ElementFactory::make("rtphdrexttwcc").build()?.downcast::<gstreamer_rtp::RTPHeaderExtension>().unwrap();
        ext.set_id(5);
        pay.set_property("auto-header-extension", true);
        pay.emit_by_name::<()>("add-extension", &[&ext]);
        log::debug!("transport-cc on {} ({i})", pay.name());
    }
    send.property::<gst::glib::Object>("ice-agent").emit_by_name::<bool>("add-local-ip-address", &[&"127.0.0.1"]);
    send.connect("on-negotiation-needed", false, {
        let tx = tx.clone();
        move |values| {
            let send = values[0].get::<gst::Element>().ok()?;
            let send2 = send.clone();
            let tx = tx.clone();
            let promise = gst::Promise::with_change_func(move |reply| {
                let Ok(Some(reply)) = reply else { return };
                let Ok(offer) = reply.value("offer").and_then(|v| v.get::<gst_webrtc::WebRTCSessionDescription>().map_err(|_| unreachable!())) else { return };
                send2.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
                let _ = tx.send(Signal::Offer(offer.sdp().as_text().unwrap()));
            });
            send.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
            None
        }
    });
    send.connect("on-ice-candidate", false, {
        let tx = tx.clone();
        move |values| {
            let _ = tx.send(Signal::SenderCandidate(values[1].get().ok()?, values[2].get().ok()?));
            None
        }
    });
    sender.set_state(gst::State::Playing)?;
    // The sender's m-sections are sendonly.
    for i in 0..2 {
        if let Some(t) = send.emit_by_name::<Option<gst_webrtc::WebRTCRTPTransceiver>>("get-transceiver", &[&(i as i32)]) {
            t.set_property("direction", gst_webrtc::WebRTCRTPTransceiverDirection::Sendonly);
        }
    }

    let start = Instant::now();
    let mut last_report = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Signal::Offer(sdp)) => {
                log::info!("offer: {} bytes", sdp.len());
                receiver.handle_offer(&sdp)?;
            }
            Ok(Signal::FromReceiver(MediaEvent::Answer(sdp))) => {
                log::info!("answer: {} bytes; video fmtp: {:?}", sdp.len(), sdp.lines().find(|l| l.starts_with("a=fmtp") && l.contains("packetization")));
                let msg = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).map_err(|_| anyhow::anyhow!("bad answer"))?;
                let answer = gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, msg);
                send.emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
            }
            Ok(Signal::FromReceiver(MediaEvent::Candidate(c))) => {
                log::debug!("receiver candidate: {}", c.candidate);
                send.emit_by_name::<()>("add-ice-candidate", &[&c.sdp_mline_index, &c.candidate]);
            }
            Ok(Signal::FromReceiver(e)) => log::info!("media event: {e:?}"),
            Ok(Signal::SenderCandidate(mline, c)) => {
                log::debug!("sender candidate: {c}");
                receiver.add_candidate(&crate::proto::IceCandidate { candidate: c, sdp_mid: None, sdp_mline_index: mline });
            }
            Err(_) => {}
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            for s in receiver.stats() {
                println!("  {s:?}");
            }
            println!(
                "after {:>2}s: {} video frames, {:.1} s of audio decoded",
                start.elapsed().as_secs(),
                frames.load(std::sync::atomic::Ordering::Relaxed),
                samples.load(std::sync::atomic::Ordering::Relaxed) as f64 / audio::RATE as f64
            );
        }
    }
    sender.set_state(gst::State::Null)?;
    drop(receiver);
    Ok(())
}

/// `answer-test OFFER.sdp`: prints the answer the receiver gives to an
/// offer (e.g. one the sender logged), without any network.
pub fn answer_test(args: &[String]) -> Result<()> {
    let Some(path) = args.first() else { bail!("usage: av-relay answer-test OFFER.sdp") };
    let offer = std::fs::read_to_string(path)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let receiver = Receiver::new(
        Some("127.0.0.1".parse()?),
        crate::proto::VideoFormat { width: 1920, height: 1080, fps: 30 },
        Sinks { video: Box::new(|_| {}), audio: Box::new(|_| {}) },
        move |e| {
            let _ = tx.send(e);
        },
    )?;
    receiver.handle_offer(&offer)?;
    loop {
        match rx.recv_timeout(Duration::from_secs(5)).context("no answer within 5 s")? {
            MediaEvent::Answer(sdp) => {
                print!("{sdp}");
                return Ok(());
            }
            MediaEvent::Error(e) => bail!(e),
            _ => {}
        }
    }
}

fn client_info() -> ClientInfo {
    let name = std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "Linux".into());
    ClientInfo { name, app: format!("av-relay {} linux", env!("CARGO_PKG_VERSION")) }
}

fn split_host_port(target: &str) -> Result<(String, u16)> {
    match target.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') || host.starts_with('[') => {
            Ok((host.trim_matches(['[', ']']).to_string(), port.parse().context("bad port")?))
        }
        _ => Ok((target.to_string(), DEFAULT_PORT)),
    }
}

/// `pair HOST[:PORT] [CODE]`: pairs with a sender and saves it.
pub fn pair(args: &[String]) -> Result<()> {
    let Some(target) = args.first() else { bail!("usage: av-relay pair HOST[:PORT] [CODE]") };
    let (host, port) = split_host_port(target)?;
    let store = Store::open_default()?;
    let me = store.identity()?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((host.as_str(), port)))
            .await
            .context("connecting timed out")??;
        stream.set_nodelay(true)?;
        let attempt = pairing::start(stream).await?;
        let code = match args.get(1) {
            Some(c) => c.clone(),
            None => {
                print!("Code shown on the sender: ");
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line
            }
        };
        let server = attempt.finish(code.trim(), &me, &client_info()).await?;
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        store.save_paired_sender(&PairedSender {
            public_key: hex_encode(&server.public_key),
            device_id: server.device_id.clone(),
            name: server.name.clone(),
            address: host.clone(),
            port,
            paired_at,
        })?;
        println!("Paired with {} (device ID {}, {})", server.name, server.device_id, server.app);
        Ok(())
    })
}

/// `session-test [SECONDS]`: opens a session with the paired sender, asks for
/// the camera and then the microphone, and prints what the sender says.
pub fn session_test(args: &[String]) -> Result<()> {
    let seconds: u64 = args.first().map(|s| s.parse()).transpose()?.unwrap_or(12);
    let store = Store::open_default()?;
    let me = store.identity()?;
    let sender = store.paired_sender()?.context("no paired sender; run `av-relay pair HOST` first")?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let stream = TcpStream::connect((sender.address.as_str(), sender.port)).await?;
        stream.set_nodelay(true)?;
        let (mut reader, mut writer) = session::connect(stream, &me, &sender.key()?).await?;
        let info = client_info();
        writer
            .send(&ClientMsg::Hello { name: info.name, app: info.app, video: VideoFormat { width: 1280, height: 720, fps: 30 } })
            .await?;
        let start = Instant::now();
        let mut step = 0;
        let mut ping_id = 0;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        while start.elapsed() < Duration::from_secs(seconds) {
            tokio::select! {
                msg = reader.recv() => match msg? {
                    ServerMsg::Offer { sdp } => println!("< offer ({} bytes of SDP, {} m-lines)", sdp.len(), sdp.matches("\nm=").count()),
                    other => println!("< {other:?}"),
                },
                _ = tick.tick() => {
                    let t = start.elapsed().as_secs();
                    let media = match (step, t) {
                        (0, 2..) => Some((true, false)),
                        (1, 5..) => Some((true, true)),
                        (2, 8..) => Some((false, false)),
                        _ => None,
                    };
                    if let Some((video, audio)) = media {
                        println!("> media video={video} audio={audio}");
                        writer.send(&ClientMsg::Media { video, audio }).await?;
                        step += 1;
                    }
                    if t % 5 == 0 {
                        ping_id += 1;
                        writer.send(&ClientMsg::Ping { id: ping_id }).await?;
                    }
                }
            }
        }
        writer.send(&ClientMsg::Bye { reason: "quit".into() }).await?;
        println!("> bye");
        Ok(())
    })
}
