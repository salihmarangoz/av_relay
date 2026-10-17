// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests the actual Windows output devices without a Mac or GStreamer.
//! cargo run --no-default-features --features windows-audio --example device-test
use av_relay_windows::{audio::AudioOutput, proto::VideoFormat, vcam::Camera};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(10);
    let format = VideoFormat {
        width: 1280,
        height: 720,
        fps: 30,
    };
    let camera = Camera::start(format)?;
    let audio = AudioOutput::start(None, |e| eprintln!("{e}"))?;
    println!(
        "OBS Virtual Camera: 1280x720 test pattern; {}: 440 Hz test tone for {seconds} seconds",
        audio.name
    );
    let stop = Instant::now() + Duration::from_secs(seconds);
    let video_feed = camera.feed.clone();
    let video_thread = std::thread::spawn(move || {
        let mut frame = vec![0u8; 1280 * 720 * 2];
        let mut video_index = 0usize;
        let mut video_at = Instant::now();
        while Instant::now() < stop {
            for y in 0..720 {
                for x in (0..1280).step_by(2) {
                    let offset = (y * 1280 + x) * 2;
                    let square = (x as isize - (video_index * 8 % 1280) as isize).abs() < 50
                        && (280..440).contains(&y);
                    let luma = if square {
                        235
                    } else {
                        [60, 125, 190][x * 3 / 1280]
                    };
                    frame[offset] = luma;
                    frame[offset + 1] = if square { 128 } else { 90 };
                    frame[offset + 2] = luma;
                    frame[offset + 3] = if square { 128 } else { 180 };
                }
            }
            video_feed.push(&frame);
            video_index += 1;
            video_at += Duration::from_nanos(1_000_000_000 / 30);
            std::thread::sleep(video_at.saturating_duration_since(Instant::now()));
        }
    });
    let mut audio_at = Instant::now();
    let mut audio_index = 0u64;
    while Instant::now() < stop {
        let samples: Vec<f32> = (0..960)
            .map(|i| {
                (((audio_index + i) as f64 * 440.0 * std::f64::consts::TAU / 48000.0).sin() * 0.1)
                    as f32
            })
            .collect();
        audio.feed.push(&samples);
        audio_index += 960;
        audio_at += Duration::from_millis(20);
        std::thread::sleep(audio_at.saturating_duration_since(Instant::now()));
    }
    let _ = video_thread.join();
    Ok(())
}
