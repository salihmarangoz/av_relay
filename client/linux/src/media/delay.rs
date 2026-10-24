//! How long to hold media back before playing it: the playout delay, set
//! from what the network actually does.
//!
//! For every packet, arrival time minus capture time (its RTP timestamp) is
//! the network delay plus a constant offset between the two clocks: how
//! much later than the luckiest packet it came. A retransmission or a
//! keyframe paced out over many packets makes packets a little late all the
//! time; a Wi-Fi stall makes a burst of them very late, now and then.
//!
//! The delay always covers the first kind. It covers a stall only when
//! stalls recur: one that came within [`WINDOW`] of another, for [`WINDOW`]
//! after it, and only as late as the smaller of the two, so a single huge
//! one doesn't set the delay. A one-off stall is left to the jitter buffer:
//! its packets come too late and are played as lost, one gap, instead of
//! everything being held back for minutes after it. WebRTC's NetEq does the
//! same (a high quantile of recent delays, not the largest). Audio and
//! video share one delay, so they stay in step.
//!
//! The delay rises at once when recurring stalls need more. It falls in
//! steps the mic catches up with by playing a little faster at the same
//! pitch (audio::stretch), the picture skipping a frame now and then; where
//! nothing catches up (Windows, for now), 2 ms a second through the
//! resampler.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

/// A stall counts for this long, and only if another came within this long
/// of it (before or after); stalls further apart are each left out.
const WINDOW: f64 = 15.0;
/// Bucket length: each bucket keeps its earliest and latest packet (NetEq
/// keeps the latest of every 500 ms).
const BUCKET: f64 = 0.5;
/// Late buckets closer than this are one stall: its backlog coming in.
const MERGE: f64 = 1.0;
/// The delay never goes below this: normal jitter, a retransmission, a
/// keyframe's packets, and the occasional Wi-Fi hiccup of 0.1-0.2 s even
/// after a calm minute (the host: no stutter first, delay is fine)...
pub const MIN: Duration = Duration::from_millis(200);
/// ...or above this (the host's limit).
pub const MAX: Duration = Duration::from_millis(2000);
/// Added to the spread.
const MARGIN: f64 = 0.04;
/// Lateness the minimum delay covers anyway: not a stall.
const STALL: f64 = MIN.as_secs_f64() - MARGIN;
/// A step down: the sinks hand over this much at once, and the sound is
/// this much behind the picture until the mic has caught up (in 0.5 s of
/// speech, at once in a pause).
const FALL_STEP: f64 = 0.05;
/// Without a mic that catches up: 2 ms per second (a 0.2% speed-up for the
/// resampler, inaudible)...
const SLOW_FALL_PER_SEC: f64 = 0.002;
/// ...applied 1 ms at a time, so the rate matching sees a smooth speed-up.
const SLOW_FALL_STEP: f64 = 0.001;
/// Rises go up in steps of this.
const STEP: f64 = 0.01;
/// A stream's first seconds don't count: what the sender queued while its
/// camera or microphone started comes in a burst.
const WARM_UP: f64 = 2.0;
/// The same after a pause: media this much newer than the last packet
/// means the sender's camera or microphone stopped and started again (on
/// demand), not a stall, whose late packets carry the old timestamps.
const RESUME: f64 = 1.0;
/// How many recent sequence numbers to remember, to skip duplicates.
const SEEN: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    start: f64,
    min: f64,
    max: f64,
}

/// A run of late buckets: one stall, when its latest packet came in, and
/// how late that was.
#[derive(Debug, Clone, Copy)]
struct Stall {
    last: f64,
    at: f64,
    late: f64,
}

/// What one stream's delay must cover (see the module docs), from the last
/// two [`WINDOW`]s: the stalls in the last one, and the ones before that
/// they may pair with.
#[derive(Debug)]
pub struct Spread {
    clock_rate: f64,
    /// The last RTP timestamp, extended past wrap-arounds.
    last_ts: Option<i64>,
    /// Arrival and extended timestamp of the first packet (the origin).
    origin: Option<(f64, i64)>,
    /// Packets arriving before this are the (re)start's burst.
    warm_until: f64,
    buckets: VecDeque<Bucket>,
    /// Recent sequence numbers: a packet sent again (a bandwidth probe
    /// re-sending old packets, a retransmission that came after the
    /// original) says nothing about when media arrives.
    seen: HashSet<u16>,
    seen_order: VecDeque<u16>,
}

impl Spread {
    pub fn new(clock_rate: u32) -> Self {
        Self {
            clock_rate: clock_rate as f64,
            last_ts: None,
            origin: None,
            warm_until: 0.0,
            buckets: VecDeque::new(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
        }
    }

    /// A media packet with sequence number `seq` and RTP timestamp `ts`
    /// arrived at `arrival` (seconds, any monotonic origin). Padding-only
    /// packets (bandwidth probes) carry an old frame's timestamp: leave them
    /// out.
    pub fn observe(&mut self, seq: u16, ts: u32, arrival: f64) {
        if !self.seen.insert(seq) {
            return;
        }
        self.seen_order.push_back(seq);
        if self.seen_order.len() > SEEN {
            if let Some(old) = self.seen_order.pop_front() {
                self.seen.remove(&old);
            }
        }
        let ext = match self.last_ts {
            None => ts as i64,
            Some(last) => last + ts.wrapping_sub(last as u32) as i32 as i64,
        };
        if self.last_ts.is_none_or(|last| (ext - last) as f64 / self.clock_rate > RESUME) {
            self.warm_until = arrival + WARM_UP;
        }
        // Only forward: a retransmission or reordering mustn't move the base.
        self.last_ts = Some(self.last_ts.map_or(ext, |l| l.max(ext)));
        let (t0, ts0) = *self.origin.get_or_insert((arrival, ext));
        if arrival < self.warm_until {
            return;
        }
        let delay = (arrival - t0) - (ext - ts0) as f64 / self.clock_rate;
        match self.buckets.back_mut() {
            Some(b) if arrival < b.start + BUCKET => {
                b.min = b.min.min(delay);
                b.max = b.max.max(delay);
            }
            _ => self.buckets.push_back(Bucket { start: arrival, min: delay, max: delay }),
        }
        while self.buckets.front().is_some_and(|b| b.start < arrival - 2.0 * WINDOW) {
            self.buckets.pop_front();
        }
    }

    /// How much later than the luckiest packet the delay must let packets
    /// come; `None` before any.
    pub fn spread(&self) -> Option<f64> {
        let base = self.buckets.iter().map(|b| b.min).reduce(f64::min)?;
        let now = self.buckets.back()?.start;
        let mut calm = 0.0f64;
        let mut stalls: Vec<Stall> = Vec::new();
        for b in &self.buckets {
            let late = b.max - base;
            if late <= STALL {
                if b.start >= now - WINDOW {
                    calm = calm.max(late);
                }
                continue;
            }
            match stalls.last_mut() {
                Some(s) if b.start - s.last <= MERGE => {
                    s.last = b.start;
                    if late > s.late {
                        s.late = late;
                        s.at = b.start;
                    }
                }
                _ => stalls.push(Stall { last: b.start, at: b.start, late }),
            }
        }
        let covered = stalls
            .iter()
            .enumerate()
            .filter(|(_, s)| s.at >= now - WINDOW)
            .map(|(i, s)| {
                let partner = stalls
                    .iter()
                    .enumerate()
                    .filter(|(j, o)| *j != i && (o.at - s.at).abs() <= WINDOW)
                    .map(|(_, o)| o.late)
                    .fold(0.0, f64::max);
                s.late.min(partner)
            })
            .fold(0.0, f64::max);
        Some(calm.max(covered))
    }
}

/// How the delay may fall on an update, when the spreads allow less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fall {
    /// A step now: the mic has caught up with the last one, or nobody
    /// listens.
    Step,
    /// Not now: the mic is still catching up.
    Hold,
    /// Slowly, for audio that doesn't catch up (the Windows receiver's).
    #[allow(dead_code)]
    Slow,
}

/// The delay itself, following the spreads.
#[derive(Debug)]
pub struct PlayoutDelay {
    /// Where the delay is heading, moved a little on every update...
    current: f64,
    /// ...and what was last applied.
    applied: f64,
}

impl PlayoutDelay {
    pub fn new(initial: Duration) -> Self {
        let d = initial.clamp(MIN, MAX).as_secs_f64();
        Self { current: d, applied: d }
    }

    /// The delay in effect.
    pub fn current(&self) -> Duration {
        Duration::from_secs_f64(self.applied)
    }

    /// What the spreads ask for.
    pub fn target(spreads: &[f64]) -> Duration {
        let need = spreads.iter().copied().fold(0.0, f64::max) + MARGIN;
        Duration::from_secs_f64(need).clamp(MIN, MAX)
    }

    /// Moves toward the spreads' target, `elapsed` after the last call.
    /// Returns the new delay when it changed enough to apply.
    pub fn update(&mut self, spreads: &[f64], elapsed: Duration, fall: Fall) -> Option<Duration> {
        let target = Self::target(spreads).as_secs_f64();
        if target > self.applied + 1e-9 {
            // Up at once, to a whole step (the epsilon keeps 0.64 from
            // rounding up to 0.65).
            self.current = ((target - 1e-9) / STEP).ceil() * STEP;
        } else if target < self.current {
            let down = match fall {
                Fall::Step => FALL_STEP,
                Fall::Hold => 0.0,
                Fall::Slow => SLOW_FALL_PER_SEC * elapsed.as_secs_f64(),
            };
            self.current = (self.current - down).max(target);
        }
        self.current = self.current.clamp(MIN.as_secs_f64(), MAX.as_secs_f64());
        // Rises apply at once, slow falls 1 ms at a time.
        if self.current > self.applied || self.applied - self.current >= SLOW_FALL_STEP - 1e-9 {
            self.applied = self.current;
            Some(self.current())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audio packets every 20 ms. Those due during a stall `(start, length)`
    /// arrive after it, coming in over `drain` seconds.
    fn feed(spread: &mut Spread, seq: &mut u16, from: f64, to: f64, stalls: &[(f64, f64)], drain: f64) {
        let mut t = from;
        while t < to {
            let arrival = match stalls.iter().find(|(s, len)| t >= *s && t < s + len) {
                Some((s, len)) => s + len + drain * (t - s) / len,
                None => t,
            };
            spread.observe(*seq, (t * 48_000.0) as u32, arrival + 0.003);
            *seq = seq.wrapping_add(1);
            t += 0.02;
        }
    }

    #[test]
    fn a_steady_stream_has_no_spread() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        feed(&mut s, &mut seq, 0.0, 10.0, &[], 0.0);
        assert!(s.spread().unwrap() < 0.001);
    }

    #[test]
    fn a_one_off_stall_is_left_out() {
        for (len, drain) in [(0.4, 0.0), (2.8, 0.0), (2.8, 0.6)] {
            let (mut s, mut seq) = (Spread::new(48_000), 0);
            feed(&mut s, &mut seq, 0.0, 10.0, &[(5.0, len)], drain);
            assert!(s.spread().unwrap() < 0.001, "{len} s stall, drained over {drain} s: {:?}", s.spread());
        }
    }

    #[test]
    fn recurring_stalls_are_covered_for_the_window() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        let stalls = [(5.0, 0.4), (12.0, 0.4)];
        feed(&mut s, &mut seq, 0.0, 6.0, &stalls, 0.0);
        assert!(s.spread().unwrap() < 0.001, "the first one alone is left out");
        feed(&mut s, &mut seq, 6.0, 14.0, &stalls, 0.0);
        let spread = s.spread().unwrap();
        assert!((spread - 0.4).abs() < 0.02, "{spread}");
        feed(&mut s, &mut seq, 14.0, 27.0, &stalls, 0.0);
        assert!(s.spread().unwrap() > 0.35, "still in the window");
        feed(&mut s, &mut seq, 27.0, 29.0, &stalls, 0.0);
        assert!(s.spread().unwrap() < 0.001, "out of the window");
    }

    #[test]
    fn of_two_stalls_the_smaller_is_covered() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        feed(&mut s, &mut seq, 0.0, 12.0, &[(3.0, 1.5), (9.0, 0.4)], 0.0);
        let spread = s.spread().unwrap();
        assert!((spread - 0.4).abs() < 0.02, "{spread}");
    }

    #[test]
    fn stalls_further_apart_than_the_window_are_each_left_out() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        let stalls = [(5.0, 0.4), (25.0, 0.4), (45.0, 0.4)];
        for (from, to) in [(0.0, 10.0), (10.0, 30.0), (30.0, 50.0)] {
            feed(&mut s, &mut seq, from, to, &stalls, 0.0);
            assert!(s.spread().unwrap() < 0.001, "at {to} s: {:?}", s.spread());
        }
    }

    #[test]
    fn timestamps_wrap_around() {
        let mut s = Spread::new(90_000);
        let start = u32::MAX - 90_000;
        for i in 0..90u32 {
            s.observe(i as u16, start.wrapping_add(i * 3000), i as f64 / 30.0);
        }
        assert!(s.spread().unwrap() < 0.001);
    }

    #[test]
    fn a_resumed_streams_burst_doesnt_count() {
        let mut s = Spread::new(90_000);
        for i in 0..300u32 {
            s.observe(i as u16, i * 3000, i as f64 / 30.0);
        }
        // The camera stops for 20 s; when it starts again, its first second
        // of frames comes 300 ms late, then on time.
        for i in 0..300u32 {
            let t = 30.0 + i as f64 / 30.0;
            let late = if i < 30 { 0.3 } else { 0.0 };
            s.observe((300 + i) as u16, (t * 90_000.0) as u32, t + late);
        }
        assert!(s.spread().unwrap() < 0.001, "{:?}", s.spread());
    }

    #[test]
    fn late_retransmissions_count() {
        let mut s = Spread::new(90_000);
        for i in (0..300u32).filter(|i| *i != 100) {
            s.observe(i as u16, i * 3000, i as f64 / 30.0);
        }
        // Frame 100's packet was lost; its retransmission comes 150 ms late.
        s.observe(100, 100 * 3000, 100.0 / 30.0 + 0.15);
        assert!((s.spread().unwrap() - 0.15).abs() < 0.001);
    }

    #[test]
    fn packets_sent_again_dont_count() {
        let mut s = Spread::new(90_000);
        for i in 0..300u32 {
            s.observe(i as u16, i * 3000, i as f64 / 30.0);
        }
        // A bandwidth probe re-sends frame 100's packet much later.
        s.observe(100, 100 * 3000, 9.0);
        assert!(s.spread().unwrap() < 0.001);
    }

    #[test]
    fn the_delay_rises_at_once_and_falls_slowly() {
        let mut d = PlayoutDelay::new(Duration::from_millis(300));
        // A 600 ms stall: up at once.
        assert_eq!(d.update(&[0.6, 0.1], Duration::from_millis(250), Fall::Slow), Some(Duration::from_millis(640)));
        // Calm again: down by 2 ms a second, applied in 1 ms steps.
        let mut applied = Vec::new();
        for _ in 0..40 {
            if let Some(x) = d.update(&[0.01], Duration::from_millis(250), Fall::Slow) {
                applied.push(x);
            }
        }
        // 10 s at 2 ms/s: 20 ms lower, in twenty steps.
        assert_eq!(applied.len(), 20, "{applied:?}");
        assert!((d.current().as_secs_f64() - 0.62).abs() < 0.001, "{:?}", d.current());
        // Never below the minimum, never above the maximum.
        for _ in 0..10_000 {
            d.update(&[0.0], Duration::from_millis(250), Fall::Slow);
        }
        assert_eq!(d.current(), MIN);
        d.update(&[5.0], Duration::from_millis(250), Fall::Slow);
        assert_eq!(d.current(), MAX);
    }

    #[test]
    fn the_delay_falls_in_steps_the_mic_catches_up_with() {
        let mut d = PlayoutDelay::new(MAX);
        let tick = Duration::from_millis(250);
        assert_eq!(d.update(&[0.0], tick, Fall::Step), Some(Duration::from_millis(1950)));
        // While the mic catches up, it waits.
        assert_eq!(d.update(&[0.0], tick, Fall::Hold), None);
        assert_eq!(d.current(), Duration::from_millis(1950));
        for _ in 0..35 {
            d.update(&[0.0], tick, Fall::Step);
        }
        assert_eq!(d.current(), MIN);
    }

    /// The evening of 2026-10-24: one 2.8 s Wi-Fi stall held the delay at
    /// 2 s for a quarter of an hour.
    #[test]
    fn a_one_off_stall_leaves_the_delay_alone() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        let mut d = PlayoutDelay::new(MIN);
        let mut t = 0.0;
        while t < 30.0 {
            feed(&mut s, &mut seq, t, t + 0.25, &[(10.0, 2.8)], 0.3);
            t += 0.25;
            d.update(&[s.spread().unwrap_or(0.0)], Duration::from_millis(250), Fall::Step);
            assert_eq!(d.current(), MIN, "at {t} s");
        }
    }

    #[test]
    fn after_a_bad_stretch_the_delay_comes_back_in_a_pause() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        let mut d = PlayoutDelay::new(MIN);
        let stalls = [(10.0, 1.1), (14.0, 2.8)];
        let mut t = 0.0;
        let mut highest = MIN;
        while t < 40.0 {
            feed(&mut s, &mut seq, t, t + 0.25, &stalls, 0.0);
            t += 0.25;
            // The mic catches up with each step by the next update (as in a
            // pause; in speech it takes two).
            d.update(&[s.spread().unwrap_or(0.0)], Duration::from_millis(250), Fall::Step);
            highest = highest.max(d.current());
        }
        // The smaller stall's lateness, not the larger's...
        assert!((highest.as_secs_f64() - 1.14).abs() < 0.011, "{highest:?}");
        // ...and back down within seconds after the window.
        assert_eq!(d.current(), MIN);
    }
}
