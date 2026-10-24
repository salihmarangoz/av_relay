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
//!
//! More audio than that queued (a stall's backlog, or a drop of the playout
//! delay, which hands its step over at once) is caught up by playing a
//! little faster at the same pitch (stretch.rs): up to 10% in speech, twice
//! as fast where it's quiet. Nothing is thrown away unless far too much is
//! queued.

use ringbuf::HeapCons;
use ringbuf::traits::{Consumer, Observer};
use std::sync::atomic::Ordering;

use super::{MicCounters, RATE, stretch};

/// Audio arrives in 20 ms chunks (Opus frames, paced by the WebRTC jitter
/// buffer), while the consumer takes one graph quantum per cycle (often
/// 21 ms, less when an app asks for low latency). Measured just before a
/// cycle takes its quantum, the queue should hold that quantum, plus one
/// chunk because chunks arrive whole, plus a margin for the network.
pub const CHUNK: usize = RATE as usize / 50;
/// The margin on a calm network...
const MIN_MARGIN: f64 = RATE as f64 / 100.0;
/// ...and the most it grows to after stalls. Small: the playout delay
/// (media::delay) holds audio back against the network; this only covers
/// the last step, from the synchronized sink to the app.
const MAX_MARGIN: f64 = RATE as f64 * 0.06;
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
/// More than this beyond the target, the excess is caught up by playing
/// faster, until it's down to a chunk or less; the rest is the resampler's.
pub const CATCH_UP_ABOVE: usize = CHUNK * 2;
/// In loud audio, catching up plays at most this much faster...
const CATCH_UP: f64 = 0.1;
/// ...in quiet audio, at most twice as fast.
const CATCH_UP_QUIET: usize = 1;
/// Beyond the target by more than this, drop the excess at once,
/// crossfaded: catching up would leave the sound behind the picture for
/// seconds. Only a long stall's backlog does this.
const MAX_EXCESS: usize = CHUNK * 10;
/// Length of the fade before a gap and after it.
const FADE: usize = RATE as usize / 400;
/// Audio is quiet when its level is within this of the noise floor (10 dB;
/// the sender sends its mic unprocessed, room noise and all)...
const QUIET_OVER_FLOOR: f32 = 3.16;
/// ...or below this (-60 dBFS), and never when above this (-30 dBFS).
const QUIET_BELOW: f32 = 0.001;
const QUIET_CEILING: f32 = 0.0316;
/// The level is measured in blocks this long (5 ms), so a word's onset
/// shows.
const LEVEL_BLOCK: usize = RATE as usize / 200;
/// The noise floor follows quieter cycles at once, louder ones by 3 dB a
/// second.
const FLOOR_RISE_PER_SEC: f32 = 1.41;

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
    /// The level (RMS) of the room's noise in the audio played.
    floor: f32,
    /// Whether the excess is being caught up (see CATCH_UP_ABOVE)...
    catching_up: bool,
    /// ...and frames loud audio may lose now, earned at CATCH_UP.
    budget: f64,
    /// Audio looked at ahead of playing it.
    ahead: Vec<f32>,
}

impl Default for Playout {
    fn default() -> Self {
        Self {
            margin: MIN_MARGIN,
            primed: false,
            gap: 0,
            dry: 0,
            last_stall: 0,
            left: 0,
            stalled: false,
            calm: 0,
            fade_in: 0,
            floor: QUIET_CEILING,
            catching_up: false,
            budget: 0.0,
            // Room for the largest quantum (8192) and a cut, so the audio
            // thread doesn't allocate.
            ahead: Vec::with_capacity(16_384),
        }
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
        if error.is_some() {
            self.follow_floor(out);
        }
        error
    }

    /// Moves the noise floor with the level of audio just played.
    fn follow_floor(&mut self, out: &[f32]) {
        let rise = FLOOR_RISE_PER_SEC.powf(out.len() as f32 / RATE as f32);
        self.floor = (self.floor * rise).min(level(out)).max(QUIET_BELOW / QUIET_OVER_FLOOR);
    }

    /// Whether `audio` is quiet throughout, against the noise floor.
    fn quiet(&self, audio: &[f32]) -> bool {
        let limit = (self.floor * QUIET_OVER_FLOOR).max(QUIET_BELOW).min(QUIET_CEILING);
        audio.chunks(LEVEL_BLOCK).all(|block| level(block) <= limit)
    }

    /// Fills `out` from `queue`, shortened by up to `room` frames (see
    /// stretch.rs). Returns how many frames it took.
    fn catch_up(&mut self, queue: &mut HeapCons<f32>, out: &mut [f32], room: usize, counters: &MicCounters) -> usize {
        let wanted = out.len();
        let quiet_cut = room.min(wanted * CATCH_UP_QUIET).min(stretch::MAX_CUT);
        let loud_cut = room.min(self.budget as usize).min(stretch::MAX_CUT);
        let need = stretch::input_needed(wanted, quiet_cut.max(loud_cut));
        self.ahead.resize(need, 0.0);
        if queue.peek_slice(&mut self.ahead) < need {
            queue.pop_slice(out);
            return wanted;
        }
        let quiet = self.quiet(&self.ahead[..quiet_cut + stretch::CROSSFADE]);
        let used = stretch::shorten(&self.ahead, out, if quiet { quiet_cut } else { loud_cut }, quiet);
        queue.skip(used);
        if !quiet {
            self.budget -= (used - wanted) as f64;
        }
        counters.stretched_frames.fetch_add((used - wanted) as u64, Ordering::Relaxed);
        used
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
            // What came in during the gap plays, caught up; only far too
            // much is dropped (in silence anyway).
            if avail > target + MAX_EXCESS {
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

        self.budget = (self.budget + wanted as f64 * CATCH_UP).min(stretch::MAX_CUT as f64);
        let excess = avail.saturating_sub(target);
        if excess > CATCH_UP_ABOVE {
            self.catching_up = true;
        } else if excess <= CHUNK {
            self.catching_up = false;
        }
        counters.catching_up.store(self.catching_up, Ordering::Relaxed);
        let used = if self.catching_up {
            self.catch_up(queue, out, excess - CHUNK / 2, counters)
        } else {
            queue.pop_slice(out);
            wanted
        };
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
        (Some(target as f64 - avail as f64), avail - used)
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

/// RMS of `audio`.
fn level(audio: &[f32]) -> f32 {
    (audio.iter().map(|s| s * s).sum::<f32>() / audio.len().max(1) as f32).sqrt()
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
    use std::time::Duration;
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
        // A 40 ms hiccup every 10 s between the synchronized sink and the
        // app (network stalls are the playout delay's job).
        let stalls: Vec<(f64, f64)> = (1..10).map(|i| (i as f64 * 10.0, 0.04)).collect();
        let (c, margin, _) = simulate(100.0, &stalls);
        assert!(c.underruns.load(Ordering::Relaxed) <= 1, "only the first one may be heard");
        // ...and the cushion stays while they keep coming.
        assert!(margin > MIN_MARGIN, "margin {margin}");
        // Delayed audio is played, not dropped (apart from priming at start).
        assert!(c.skipped_frames.load(Ordering::Relaxed) < CHUNK as u64 * 2);
    }

    /// Frequency from upward zero crossings.
    fn frequency(x: &[f32]) -> f32 {
        let ups = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * RATE as f32 / x.len() as f32
    }

    #[test]
    fn a_backlog_is_caught_up_at_the_same_pitch() {
        // A 150 ms stall, its backlog coming in at once (spread out, the
        // simulation would deliver it out of order).
        let (c, _, played) = simulate(10.0, &[(3.0, 0.15)]);
        // Played, faster, rather than dropped...
        assert_eq!(c.skipped_frames.load(Ordering::Relaxed), 0);
        assert!(c.stretched_frames.load(Ordering::Relaxed) > 0);
        // ...the 440 Hz tone still 440 Hz and unbroken...
        let after = &played[RATE as usize * 3 + RATE as usize / 5..RATE as usize * 6];
        let hz = frequency(after);
        assert!((hz - 440.0).abs() < 3.0, "{hz} Hz");
        let worst = after.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.05, "a jump of {worst}");
        // ...and caught up by the end, but for what the resampler takes
        // (no rate matching here, and chunks arriving whole).
        let (fill, target) = (c.fill.load(Ordering::Relaxed), c.target.load(Ordering::Relaxed));
        assert!(fill <= target + CATCH_UP_ABOVE as u64, "{fill} queued for a target of {target}");
    }

    #[test]
    fn a_backlog_too_big_to_catch_up_is_dropped() {
        // A 400 ms stall whose backlog takes 200 ms to come in.
        let ((c, _, _), most_above) = simulate_spread(10.0, &[(3.0, 0.4)], 0.2);
        assert!(c.skipped_frames.load(Ordering::Relaxed) > 0);
        // At most MAX_EXCESS, plus what comes in (twice as fast) by the next
        // cycle.
        assert!(most_above <= MAX_EXCESS + CHUNK * 3, "{most_above} frames above the target");
    }

    /// Cycles of 1024 frames (fed as fast as played) until the excess over
    /// the target, `extra` frames of `signal` at the start, is caught up.
    fn cycles_to_catch_up(extra: usize, signal: impl Fn(usize) -> f32) -> (usize, MicCounters) {
        let (mut prod, mut cons) = HeapRb::<f32>::new(RATE as usize).split();
        let counters = MicCounters::default();
        let mut playout = Playout::default();
        let quantum = 1024usize;
        let mut sample = 0usize;
        let mut push = |prod: &mut ringbuf::HeapProd<f32>, n: usize| {
            for _ in 0..n {
                prod.try_push(signal(sample)).unwrap();
                sample += 1;
            }
        };
        push(&mut prod, playout.target(quantum) + extra);
        let mut out = vec![0.0; quantum];
        for cycle in 1..1000 {
            playout.fill(&mut cons, &mut out, &counters);
            push(&mut prod, quantum);
            if cons.occupied_len() <= playout.target(quantum) + CHUNK {
                return (cycle, counters);
            }
        }
        panic!("never caught up");
    }

    #[test]
    fn speech_catches_up_at_most_10_percent_faster_and_pauses_at_once() {
        let tone = |i: usize| (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.3;
        // 100 ms over, 80 ms of it to catch up: at 10%, 38 cycles (0.8 s)
        // of loud audio at least...
        let (cycles, c) = cycles_to_catch_up(RATE as usize / 10, tone);
        assert!((38..80).contains(&cycles), "{cycles} cycles");
        assert_eq!(c.skipped_frames.load(Ordering::Relaxed), 0);
        // ...and a few cycles of room noise at -50 dBFS.
        let (cycles, _) = cycles_to_catch_up(RATE as usize / 10, |i| tone(i) * 0.015);
        assert!(cycles <= 6, "{cycles} cycles");
    }

    /// The playout delay coming down from `from` to its minimum, each step
    /// handing the mic that much `signal` at once, the next step waiting
    /// until the mic has caught up (as app::serve does). Returns how long
    /// it took and the most the sound fell behind the picture, in seconds.
    fn delay_coming_down(from: f64, signal: impl Fn(usize) -> f32) -> (f64, f64) {
        use crate::media::delay::{Fall, MIN, PlayoutDelay};
        let (mut prod, mut cons) = HeapRb::<f32>::new(RATE as usize).split();
        let counters = MicCounters::default();
        let mut playout = Playout::default();
        let mut delay = PlayoutDelay::new(Duration::from_secs_f64(from));
        let (quantum, tick) = (1024usize, Duration::from_millis(250));
        let (mut sample, mut t, mut next_chunk, mut next_cycle, mut next_tick) = (0usize, 0.0f64, 0.0f64, 0.0f64, 1.0f64);
        let mut push = |prod: &mut ringbuf::HeapProd<f32>, n: usize| {
            for _ in 0..n {
                prod.try_push(signal(sample)).unwrap();
                sample += 1;
            }
        };
        let mut out = vec![0.0; quantum];
        let mut behind = 0.0f64;
        while delay.current() > MIN {
            t = next_chunk.min(next_cycle).min(next_tick);
            if t == next_chunk {
                push(&mut prod, CHUNK);
                next_chunk += CHUNK as f64 / RATE as f64;
            } else if t == next_cycle {
                playout.fill(&mut cons, &mut out, &counters);
                if playout.primed {
                    let over = cons.occupied_len().saturating_sub(playout.target(quantum));
                    behind = behind.max(over as f64 / RATE as f64);
                }
                next_cycle += quantum as f64 / RATE as f64;
            } else {
                let fall = if counters.caught_up() { Fall::Step } else { Fall::Hold };
                let before = delay.current();
                delay.update(&[0.0], tick, fall);
                push(&mut prod, ((before - delay.current()).as_secs_f64() * RATE as f64) as usize);
                next_tick += tick.as_secs_f64();
            }
            assert!(t < 120.0, "still at {:?}", delay.current());
        }
        (t - 1.0, behind)
    }

    #[test]
    fn the_delay_comes_down_in_seconds_with_the_sound_in_step() {
        let tone = |i: usize| (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.3;
        // From 1.14 s, in speech: 100 ms a second or so.
        let (took, behind) = delay_coming_down(1.14, tone);
        assert!((6.0..15.0).contains(&took), "{took} s");
        assert!(behind < 0.08, "sound {behind} s behind");
        // In a pause, faster.
        let (took, behind) = delay_coming_down(1.14, |i| tone(i) * 0.015);
        assert!(took < 6.0, "{took} s");
        assert!(behind < 0.08, "sound {behind} s behind");
    }

    #[test]
    fn quiet_is_told_from_speech_by_the_room_noise() {
        let block = |amplitude: f32, from: usize, n: usize| -> Vec<f32> {
            (from..from + n).map(|i| (i as f32 * 200.0 * std::f32::consts::TAU / RATE as f32).sin() * amplitude).collect()
        };
        // 10 s of speech at -20 dBFS (words of 300 ms, 100 ms apart) over
        // room noise at -50 dBFS teach the floor.
        let (noise, word) = (0.0045, 0.14);
        let mut playout = Playout::default();
        for i in (0..RATE as usize * 10).step_by(1024) {
            let t = i as f64 / RATE as f64;
            playout.follow_floor(&block(if t % 0.4 < 0.3 { word } else { noise }, i, 1024));
        }
        assert!(playout.quiet(&block(noise, 0, 1200)));
        assert!(!playout.quiet(&block(word, 0, 1200)));
        // A word starting at the very end still counts.
        let mut onset = block(noise, 0, 1200);
        onset[960..].copy_from_slice(&block(word, 960, 240));
        assert!(!playout.quiet(&onset));
        // A loud room is never quiet, however steady.
        let mut loud = Playout::default();
        for i in (0..RATE as usize * 10).step_by(1024) {
            loud.follow_floor(&block(0.1, i, 1024));
        }
        assert!(!loud.quiet(&block(0.1, 0, 1200)));
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
        prod.push_iter(std::iter::repeat_n(0.1, CHUNK * 20));
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
