//! Developer commands for testing parts of the client on their own:
//! the virtual camera, pairing, and a session against a real Mac.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use tokio::net::TcpStream;

use crate::audio::{self, PipeWire};
use crate::proto::pairing::{self, ClientInfo};
use crate::proto::{ClientMsg, DEFAULT_PORT, ServerMsg, VideoFormat, session};
use crate::store::{PairedMac, Store, hex_encode};
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
/// into the virtual microphone, by default 200 ppm fast like a drifting Mac
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

/// `pair HOST[:PORT] [CODE]`: pairs with a Mac and saves it.
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
                print!("Code shown on the Mac: ");
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line
            }
        };
        let server = attempt.finish(code.trim(), &me, &client_info()).await?;
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        store.save_paired_mac(&PairedMac {
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

/// `session-test [SECONDS]`: opens a session with the paired Mac, asks for
/// the camera and then the microphone, and prints what the Mac says.
pub fn session_test(args: &[String]) -> Result<()> {
    let seconds: u64 = args.first().map(|s| s.parse()).transpose()?.unwrap_or(12);
    let store = Store::open_default()?;
    let me = store.identity()?;
    let mac = store.paired_mac()?.context("no paired Mac; run `av-relay pair HOST` first")?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let stream = TcpStream::connect((mac.address.as_str(), mac.port)).await?;
        stream.set_nodelay(true)?;
        let (mut reader, mut writer) = session::connect(stream, &me, &mac.key()?).await?;
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
