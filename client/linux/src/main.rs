mod vcam;

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("vcam-test") => vcam_test(),
        _ => bail!("usage: av-relay vcam-test"),
    }
}

/// Shows a moving test pattern on the virtual camera and logs when apps
/// start and stop using it.
fn vcam_test() -> Result<()> {
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
