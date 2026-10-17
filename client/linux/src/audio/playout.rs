//! What the virtual mic plays each cycle: how much audio to keep queued,
//! and what to do when the network stalls.
//!
//! Over Wi-Fi, audio often arrives in bursts: nothing for 100-300 ms, then
//! all of it at once. Playing what was delayed (instead of dropping it to
//! keep the latency low) and keeping a cushion that grows after such a gap
//! turns repeated stutters into, at most, one gap per bad stretch. The
//! cushion shrinks again, slowly, once the network has been calm for a while,
//! and the resampler (steered by the caller's DLL) absorbs the change
//! inaudibly. Gaps fade out and in, so they never click.

use ringbuf::HeapCons;
use ringbuf::traits::{Consumer, Observer};
use std::sync::atomic::Ordering;

use super::{MicCounters, RATE};

/// Audio arrives in 20 ms chunks (Opus frames, paced by the WebRTC jitter
/// buffer), while the consumer takes one graph quantum per cycle (often
/// 21 ms, less when an app asks for low latency). Measured just before a
/// cycle takes its quantum, the queue should hold that quantum, plus one
/// chunk because chunks arrive whole, plus a margin for the network.
pub const CHUNK: usize = RATE as usize / 50;
/// The margin on a calm network...
const MIN_MARGIN: f64 = RATE as f64 / 100.0;
/// ...and the most it grows to after stalls.
const MAX_MARGIN: f64 = RATE as f64 / 5.0;
/// Added on top of a stall's length when the margin grows (a stall is
/// measured in whole cycles).
const SAFETY: f64 = CHUNK as f64;
/// A grown margin stays this long after the last underrun or near miss
/// (Wi-Fi stalls tend to recur, e.g. every ~25 s with a laptop's background
/// scans)...
const HOLD: usize = RATE as usize * 30;
/// ...then shrinks by this fraction of the time played (1 ms per second, a
/// 0.1% speed-up for the resampler).
const DECAY: f64 = 0.001;
/// A gap longer than this isn't a network stall (the sender's mic stopped, say):
/// starting again doesn't grow the margin.
const MAX_STALL: usize = RATE as usize;
/// Beyond the target by more than this, drop the excess at once,
/// crossfaded. It happens right after a stall: the delayed audio can keep
/// coming in after playback resumed. Catching up through the resampler would
/// take a minute, with audio late (and out of step with video) all along.
const MAX_EXCESS: usize = CHUNK * 3;
/// Length of the fade before a gap and after it.
const FADE: usize = RATE as usize / 400;

#[derive(Debug)]
pub struct Playout {
    /// Frames of cushion for the network, on top of quantum + chunk.
    margin: f64,
    /// Whether the queue holds enough audio to play from; until then, silence.
    primed: bool,
    /// Frames of silence played since the last underrun, while unprimed.
    gap: usize,
    /// Frames played (or silence) since audio last arrived...
    dry: usize,
    /// ...and that count when audio last arrived after a while: the length
    /// of the latest stall.
    last_stall: usize,
    /// What the queue held after the previous cycle, to tell arrivals.
    left: usize,
    /// Whether the current silence follows an underrun (rather than a start).
    stalled: bool,
    /// Frames played since the last underrun or near miss.
    calm: usize,
    /// Frames of fade-in still to apply.
    fade_in: usize,
}

impl Default for Playout {
    fn default() -> Self {
        Self { margin: MIN_MARGIN, primed: false, gap: 0, dry: 0, last_stall: 0, left: 0, stalled: false, calm: 0, fade_in: 0 }
    }
}

impl Playout {
    /// How full the queue should be just before a cycle takes `wanted` frames.
    pub fn target(&self, wanted: usize) -> usize {
        wanted + CHUNK + self.margin as usize
    }

    /// Fills `out` (one cycle) from `queue`. Returns the fill error for the
    /// rate-matching DLL (target minus queued, in frames) when the cycle
    /// played normally, `None` during silence.
    pub fn fill(&mut self, queue: &mut HeapCons<f32>, out: &mut [f32], counters: &MicCounters) -> Option<f64> {
        let wanted = out.len();
        out.fill(0.0);
        let avail = queue.occupied_len();
        counters.fill.store(avail as u64, Ordering::Relaxed);
        // Audio arrived since the last cycle: a dry spell, if any, is over,
        // and its length is the latest stall's.
        if avail > self.left {
            self.last_stall = self.dry;
            self.dry = 0;
        }
        self.dry += wanted;
        let (error, left) = self.play(queue, out, counters, avail);
        self.left = left;
        error
    }

    /// One cycle, from `avail` frames queued. Returns the DLL's error, and
    /// what's left queued.
    fn play(&mut self, queue: &mut HeapCons<f32>, out: &mut [f32], counters: &MicCounters, mut avail: usize) -> (Option<f64>, usize) {
        let wanted = out.len();
        if !self.primed {
            // After a stall, keep enough cushion to ride out one as long as
            // the latest, and build it up now, while it's silent anyway.
            if self.stalled && self.gap <= MAX_STALL {
                let grown = self.margin.max(self.last_stall as f64 + SAFETY).min(MAX_MARGIN);
                if grown > self.margin {
                    self.margin = grown;
                    log::debug!("virtual mic: margin now {} ms", self.margin as usize * 1000 / RATE as usize);
                }
            }
            let target = self.target(wanted);
            counters.target.store(target as u64, Ordering::Relaxed);
            if avail < target {
                self.gap += wanted;
                counters.silent_frames.fetch_add(wanted as u64, Ordering::Relaxed);
                return (None, avail);
            }
            if avail > target {
                counters.skipped_frames.fetch_add((avail - target) as u64, Ordering::Relaxed);
                queue.skip(avail - target);
                avail = target;
            }
            self.primed = true;
            self.stalled = false;
            self.gap = 0;
            self.fade_in = FADE;
        }
        let target = self.target(wanted);
        counters.target.store(target as u64, Ordering::Relaxed);

        // Crossfaded from where playback was into where it continues.
        let mut crossfade = None;
        if avail > target + MAX_EXCESS {
            log::debug!("virtual mic: {avail} frames queued, dropping to {target}");
            let mut old = [0.0f32; FADE];
            queue.pop_slice(&mut old);
            counters.skipped_frames.fetch_add((avail - target) as u64, Ordering::Relaxed);
            queue.skip(avail - target - FADE);
            // FADE old frames are in hand, and the queue now holds `target`.
            avail = target;
            crossfade = Some(old);
        }

        if avail < wanted {
            // Underrun: play what's left, fading out, then silence until
            // refilled.
            log::debug!("virtual mic underrun: {avail} < {wanted}");
            queue.pop_slice(&mut out[..avail]);
            self.apply_fade_in(&mut out[..avail]);
            fade_out(&mut out[..avail]);
            counters.underruns.fetch_add(1, Ordering::Relaxed);
            counters.silent_frames.fetch_add((wanted - avail) as u64, Ordering::Relaxed);
            self.primed = false;
            self.stalled = true;
            self.gap = wanted - avail;
            self.calm = 0;
            return (None, 0);
        }

        queue.pop_slice(out);
        self.apply_fade_in(out);
        if let Some(old) = crossfade {
            for (i, (s, o)) in out.iter_mut().zip(old).enumerate() {
                let w = i as f32 / FADE as f32;
                *s = o * (1.0 - w) + *s * w;
            }
        }
        // A stall that used up half the cushion was a near miss: keep it.
        if ((avail - wanted) as f64) < self.margin / 2.0 {
            self.calm = 0;
        }
        self.calm += wanted;
        if self.calm > HOLD {
            self.margin = (self.margin - wanted as f64 * DECAY).max(MIN_MARGIN);
        }
        (Some(target as f64 - avail as f64), avail - wanted)
    }

    fn apply_fade_in(&mut self, out: &mut [f32]) {
        for s in out.iter_mut() {
            if self.fade_in == 0 {
                break;
            }
            *s *= (FADE - self.fade_in) as f32 / FADE as f32;
            self.fade_in -= 1;
        }
    }
}

/// Ramps the end of `out` down to silence.
fn fade_out(out: &mut [f32]) {
    let n = out.len().min(FADE);
    let start = out.len() - n;
    for (i, s) in out[start..].iter_mut().enumerate() {
        *s *= (n - 1 - i) as f32 / n as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;
    use ringbuf::traits::{Observer, Producer, Split};

    /// Runs `secs` of audio: 20 ms chunks from the network, except that
    /// the chunks due during a stall arrive after it, spread over `spread`
    /// seconds; a consumer takes 1024 frames per 1024/48 ms. Returns the
    /// counters, the margin at the end, and everything played.
    fn simulate(secs: f64, stalls: &[(f64, f64)]) -> (MicCounters, f64, Vec<f32>) {
        simulate_spread(secs, stalls, 0.0).0
    }

    /// Also returns the most the queue held above its target, in frames,
    /// measured after each cycle once playing.
    fn simulate_spread(secs: f64, stalls: &[(f64, f64)], spread: f64) -> ((MicCounters, f64, Vec<f32>), usize) {
        let (mut prod, mut cons) = HeapRb::<f32>::new(RATE as usize).split();
        let counters = MicCounters::default();
        let mut playout = Playout::default();
        let mut played = Vec::new();
        let (quantum, chunk) = (1024usize, CHUNK);
        let mut sample = 0u64;
        let mut next_chunk = 0.0f64;
        let mut next_cycle = 0.0f64;
        let arrival = |t: f64| {
            stalls.iter().find(|(s, len)| t >= *s && t < s + len).map_or(t, |(s, len)| s + len + spread * (t - s) / len)
        };
        let mut most_above = 0usize;
        let mut pending: Vec<(f64, Vec<f32>)> = Vec::new();
        while next_cycle < secs {
            if next_chunk <= next_cycle {
                // A 440 Hz tone, so gaps and fades show up in the samples.
                let data: Vec<f32> = (0..chunk)
                    .map(|i| ((sample + i as u64) as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.5)
                    .collect();
                sample += chunk as u64;
                pending.push((arrival(next_chunk), data));
                next_chunk += chunk as f64 / RATE as f64;
            } else {
                for (_, data) in pending.extract_if(.., |(t, _)| *t <= next_cycle) {
                    prod.push_slice(&data);
                }
                let mut out = vec![0.0; quantum];
                playout.fill(&mut cons, &mut out, &counters);
                played.extend_from_slice(&out);
                if playout.primed {
                    most_above = most_above.max(cons.occupied_len().saturating_sub(playout.target(quantum)));
                }
                next_cycle += quantum as f64 / RATE as f64;
            }
        }
        ((counters, playout.margin, played), most_above)
    }

    #[test]
    fn a_steady_stream_never_underruns() {
        let (c, margin, _) = simulate(30.0, &[]);
        assert_eq!(c.underruns.load(Ordering::Relaxed), 0);
        assert_eq!(margin, MIN_MARGIN);
    }

    #[test]
    fn the_margin_grows_after_a_stall_so_the_next_ones_pass() {
        // A 150 ms stall every 25 s, like the Wi-Fi in the field.
        let stalls: Vec<(f64, f64)> = (1..8).map(|i| (i as f64 * 25.0, 0.15)).collect();
        let (c, margin, _) = simulate(200.0, &stalls);
        assert_eq!(c.underruns.load(Ordering::Relaxed), 1, "only the first stall should be heard");
        // ...and the cushion stays while stalls keep coming.
        assert!(margin > 0.1 * RATE as f64, "margin {margin}");
        // Delayed audio is played, not dropped (apart from priming at start).
        assert!(c.skipped_frames.load(Ordering::Relaxed) < CHUNK as u64 * 2);
    }

    #[test]
    fn audio_that_trickles_in_after_a_stall_is_dropped() {
        // A 400 ms stall whose backlog takes 200 ms to come in.
        let ((c, _, _), most_above) = simulate_spread(10.0, &[(3.0, 0.4)], 0.2);
        assert!(c.skipped_frames.load(Ordering::Relaxed) > 0);
        // At most 60 ms (MAX_EXCESS) plus a chunk arriving whole.
        assert!(most_above <= CHUNK * 4, "{most_above} frames above the target");
    }

    #[test]
    fn after_a_crossfaded_drop_the_books_match_the_queue() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(RATE as usize).split();
        let counters = MicCounters::default();
        let mut playout = Playout::default();
        let quantum = 1024usize;
        let mut out = vec![0.0; quantum];
        prod.push_iter(std::iter::repeat_n(0.1, playout.target(quantum)));
        playout.fill(&mut cons, &mut out, &counters);
        prod.push_iter(std::iter::repeat_n(0.1, CHUNK * 10));
        let error = playout.fill(&mut cons, &mut out, &counters).unwrap();
        assert_eq!(playout.left, cons.occupied_len());
        assert_eq!(error, 0.0);
    }

    #[test]
    fn the_margin_shrinks_back_when_calm() {
        let (_, margin, _) = simulate(240.0, &[(2.0, 0.15)]);
        assert_eq!(margin, MIN_MARGIN);
    }

    #[test]
    fn gaps_fade_instead_of_clicking() {
        let (c, _, played) = simulate(6.0, &[(2.0, 0.3)]);
        assert_eq!(c.underruns.load(Ordering::Relaxed), 1);
        // The tone moves at most ~0.03 per sample; a hard cut to or from
        // silence would jump by up to 0.5.
        let start = played.iter().position(|s| *s != 0.0).unwrap();
        let worst = played[start..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.05, "a jump of {worst}");
    }
}
