//! How long to hold media back before playing it: the playout delay, set
//! from what the network actually does.
//!
//! For every packet, arrival time minus capture time (its RTP timestamp) is
//! the network delay plus a constant offset between the two clocks. Its
//! spread over the last minute is how much later than the luckiest packet
//! the unluckiest one came: a Wi-Fi stall, a retransmission, a keyframe
//! paced out over many packets. Holding everything back by that spread
//! (plus a margin) means every packet of the last minute would have been on
//! time. Audio and video share one delay, so they stay in step.
//!
//! The delay rises at once when a stall needs more, and falls back slowly
//! once the bigger stalls have left the window.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

/// How far back the spread looks: Wi-Fi stalls tend to recur within it
/// (every ~25 s, in the field).
const WINDOW: f64 = 60.0;
/// Bucket length for the window's minimum and maximum.
const BUCKET: f64 = 1.0;
/// The delay never goes below this: normal jitter, a retransmission, a
/// keyframe's packets, and the occasional Wi-Fi hiccup of 0.1-0.2 s even
/// after a calm minute (the host: no stutter first, delay is fine)...
pub const MIN: Duration = Duration::from_millis(200);
/// ...or above this (the host's limit).
pub const MAX: Duration = Duration::from_millis(2000);
/// Added to the spread.
const MARGIN: f64 = 0.04;
/// How fast the delay falls once the window allows less: 2 ms per second
/// (a 0.2% speed-up, inaudible).
const FALL_PER_SEC: f64 = 0.002;
/// Rises go up in steps of this...
const STEP: f64 = 0.01;
/// ...falls in steps of this: each one makes the sinks hand over that much
/// audio at once, which the mic's rate matching then has to absorb; 1 ms
/// at a time it sees a smooth speed-up.
const FALL_STEP: f64 = 0.001;
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

/// One stream's delay spread over the last [`WINDOW`].
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
        while self.buckets.front().is_some_and(|b| b.start < arrival - WINDOW) {
            self.buckets.pop_front();
        }
    }

    /// Latest packet minus earliest, over the window; `None` before any.
    pub fn spread(&self) -> Option<f64> {
        let min = self.buckets.iter().map(|b| b.min).reduce(f64::min)?;
        let max = self.buckets.iter().map(|b| b.max).reduce(f64::max)?;
        Some(max - min)
    }
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
    /// Starts at `initial` (e.g. what the last session ended with).
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
    pub fn update(&mut self, spreads: &[f64], elapsed: Duration) -> Option<Duration> {
        let target = Self::target(spreads).as_secs_f64();
        if target > self.applied + 1e-9 {
            // Up at once, to a whole step (the epsilon keeps 0.64 from
            // rounding up to 0.65).
            self.current = ((target - 1e-9) / STEP).ceil() * STEP;
        } else if target < self.current {
            self.current = (self.current - FALL_PER_SEC * elapsed.as_secs_f64()).max(target);
        }
        self.current = self.current.clamp(MIN.as_secs_f64(), MAX.as_secs_f64());
        // Rises apply at once, falls a step at a time.
        if self.current > self.applied || self.applied - self.current >= FALL_STEP - 1e-9 {
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

    /// Audio packets every 20 ms; those due during a stall arrive at its end.
    fn feed(spread: &mut Spread, seq: &mut u16, from: f64, to: f64, stall: Option<(f64, f64)>) {
        let mut t = from;
        while t < to {
            let arrival = match stall {
                Some((s, len)) if t >= s && t < s + len => s + len,
                _ => t,
            };
            spread.observe(*seq, (t * 48_000.0) as u32, arrival + 0.003);
            *seq = seq.wrapping_add(1);
            t += 0.02;
        }
    }

    #[test]
    fn a_steady_stream_has_no_spread() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        feed(&mut s, &mut seq, 0.0, 10.0, None);
        assert!(s.spread().unwrap() < 0.001);
    }

    #[test]
    fn a_stall_shows_as_spread_for_a_minute() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        feed(&mut s, &mut seq, 0.0, 10.0, Some((5.0, 0.4)));
        let spread = s.spread().unwrap();
        assert!((spread - 0.4).abs() < 0.02, "{spread}");
        feed(&mut s, &mut seq, 10.0, 64.0, None);
        assert!(s.spread().unwrap() > 0.35, "still in the window");
        feed(&mut s, &mut seq, 64.0, 70.0, None);
        assert!(s.spread().unwrap() < 0.001, "out of the window");
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
    fn a_long_stall_still_counts() {
        let (mut s, mut seq) = (Spread::new(48_000), 0);
        feed(&mut s, &mut seq, 0.0, 10.0, Some((5.0, 1.5)));
        assert!((s.spread().unwrap() - 1.5).abs() < 0.02, "{:?}", s.spread());
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
        assert_eq!(d.update(&[0.6, 0.1], Duration::from_millis(250)), Some(Duration::from_millis(640)));
        // Calm again: down by 2 ms a second, applied in 1 ms steps.
        let mut applied = Vec::new();
        for _ in 0..40 {
            if let Some(x) = d.update(&[0.01], Duration::from_millis(250)) {
                applied.push(x);
            }
        }
        // 10 s at 2 ms/s: 20 ms lower, in twenty steps.
        assert_eq!(applied.len(), 20, "{applied:?}");
        assert!((d.current().as_secs_f64() - 0.62).abs() < 0.001, "{:?}", d.current());
        // Never below the minimum, never above the maximum.
        for _ in 0..10_000 {
            d.update(&[0.0], Duration::from_millis(250));
        }
        assert_eq!(d.current(), MIN);
        d.update(&[5.0], Duration::from_millis(250));
        assert_eq!(d.current(), MAX);
    }
}
