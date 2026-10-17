// SPDX-License-Identifier: GPL-3.0-or-later
//! Play only to a selected cable endpoint; never send the sender's microphone
//! to the default speakers. CPAL uses Windows WASAPI shared mode.
use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
pub const RATE: u32 = 48_000;
const MAX_QUEUE: usize = RATE as usize / 4;

struct Buffer {
    samples: VecDeque<f32>,
    last: Instant,
    position: f64,
    primed: bool,
}
#[derive(Clone)]
pub struct AudioFeed(Arc<Mutex<Buffer>>);
impl AudioFeed {
    pub fn push(&self, samples: &[f32]) {
        let mut b = self.0.lock().unwrap();
        if b.last.elapsed() > Duration::from_millis(500) {
            b.samples.clear();
            b.primed = false;
        }
        b.last = Instant::now();
        let room = MAX_QUEUE.saturating_sub(samples.len());
        while b.samples.len() > room {
            b.samples.pop_front();
        }
        b.samples
            .extend(samples.iter().rev().take(MAX_QUEUE).rev().copied());
    }
    pub fn clear(&self) {
        let mut b = self.0.lock().unwrap();
        b.samples.clear();
        b.primed = false;
        b.position = 0.0;
    }
}
pub struct AudioOutput {
    pub feed: AudioFeed,
    pub name: String,
    pub failed: Arc<AtomicBool>,
    _stream: cpal::Stream,
}
pub fn devices() -> Result<Vec<String>> {
    Ok(cpal::default_host()
        .output_devices()?
        .filter_map(|d| d.name().ok())
        .collect())
}
impl AudioOutput {
    pub fn start(selected: Option<&str>, error: impl Fn(String) + Send + 'static) -> Result<Self> {
        let device = cpal::default_host().output_devices()?.find(|d| d.name().is_ok_and(|name| match selected {
            Some(selected) => name == selected,
            None => name.to_ascii_lowercase().contains("cable input"),
        })).context("VB-CABLE playback endpoint not found. Install VB-CABLE, restart Windows, then select CABLE Input in AV Relay.")?;
        let name = device.name()?;
        // Use the device's native mix format, resampling our mono 48 kHz input.
        let config = device.default_output_config()?;
        let feed = AudioFeed(Arc::new(Mutex::new(Buffer {
            samples: VecDeque::with_capacity(MAX_QUEUE),
            last: Instant::now(),
            position: 0.0,
            primed: false,
        })));
        let channels = config.channels() as usize;
        let rate = config.sample_rate().0;
        let queue = feed.0.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let failed_callback = failed.clone();
        let error = move |err| {
            failed_callback.store(true, Ordering::Release);
            error(format!("Audio output failed: {err}"));
        };
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |out: &mut [f32], _| fill(out, channels, rate, &queue),
                error,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config.into(),
                move |out: &mut [i16], _| fill(out, channels, rate, &queue),
                error,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config.into(),
                move |out: &mut [u16], _| fill(out, channels, rate, &queue),
                error,
                None,
            )?,
            other => bail!("Unsupported playback sample format {other:?}"),
        };
        stream.play()?;
        Ok(Self {
            feed,
            name,
            failed,
            _stream: stream,
        })
    }
}
fn fill<T: cpal::Sample + cpal::FromSample<f32>>(
    out: &mut [T],
    channels: usize,
    rate: u32,
    queue: &Mutex<Buffer>,
) {
    out.fill(T::from_sample(0.0));
    let Ok(mut b) = queue.try_lock() else { return };
    if b.last.elapsed() > Duration::from_millis(500) {
        b.samples.clear();
        b.primed = false;
        return;
    }
    let target = RATE as usize * 40 / 1000;
    if !b.primed {
        if b.samples.len() < target {
            return;
        }
        b.primed = true;
        b.position = 0.0;
    }
    // Slowly match the sender and receiver clocks, keeping the queue near 40 ms.
    let correction =
        ((b.samples.len() as f64 - target as f64) / target as f64 * 0.002).clamp(-0.005, 0.005);
    let step = RATE as f64 / rate as f64 * (1.0 + correction);
    for frame in out.chunks_mut(channels) {
        if b.samples.len() < 2 {
            b.primed = false;
            break;
        }
        let sample = b.samples[0] + (b.samples[1] - b.samples[0]) * b.position as f32;
        frame.fill(T::from_sample(sample.clamp(-1.0, 1.0)));
        b.position += step;
        while b.position >= 1.0 && !b.samples.is_empty() {
            b.samples.pop_front();
            b.position -= 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cable_buffer_is_bounded_and_silent_after_clear() {
        let feed = AudioFeed(Arc::new(Mutex::new(Buffer {
            samples: VecDeque::new(),
            last: Instant::now(),
            position: 0.0,
            primed: false,
        })));
        feed.push(&vec![0.25; RATE as usize]);
        assert_eq!(feed.0.lock().unwrap().samples.len(), MAX_QUEUE);
        let mut out = [0.0; 960];
        fill(&mut out, 2, RATE, &feed.0);
        assert!(out.iter().all(|s| *s == 0.25));
        feed.clear();
        fill(&mut out, 2, RATE, &feed.0);
        assert!(out.iter().all(|s| *s == 0.0));
    }
}
