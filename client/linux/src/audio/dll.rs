//! A delay-locked loop, as PipeWire's own network modules use it to steer an
//! adaptive resampler: a port of spa/utils/dll.h.

use std::f64::consts::PI;

/// The narrowest bandwidth PipeWire uses (SPA_DLL_BW_MIN): slow, smooth
/// corrections, right for clocks that drift by a few ppm.
pub const BW_MIN: f64 = 0.016;

#[derive(Debug, Clone, Default)]
pub struct Dll {
    z1: f64,
    z2: f64,
    z3: f64,
    w0: f64,
    w1: f64,
    w2: f64,
}

impl Dll {
    pub fn new(bw: f64, period: u32, rate: u32) -> Self {
        let mut dll = Self::default();
        dll.set_bw(bw, period, rate);
        dll
    }

    pub fn set_bw(&mut self, bw: f64, period: u32, rate: u32) {
        let w = 2.0 * PI * bw * period as f64 / rate as f64;
        self.w0 = 1.0 - (-20.0 * w).exp();
        self.w1 = w * 1.5 / period as f64;
        self.w2 = w / 1.5;
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
        self.z3 = 0.0;
    }

    /// Feeds an error (here: target fill minus actual fill, in frames) and
    /// returns the correction factor, around 1.0.
    pub fn update(&mut self, err: f64) -> f64 {
        self.z1 += self.w0 * (self.w1 * err - self.z1);
        self.z2 += self.w0 * (self.z1 - self.z2);
        self.z3 += self.w2 * self.z2;
        1.0 - (self.z2 + self.z3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates a source whose clock runs 200 ppm fast against the sink and
    /// checks that the loop settles the buffer near the target.
    #[test]
    fn settles_a_drifting_clock() {
        let (rate, quantum, target) = (48_000u32, 1024usize, 1440.0f64);
        let mut dll = Dll::new(BW_MIN, 128, rate);
        let mut fill = target;
        let incoming_per_cycle = quantum as f64 * 1.0002;
        let mut corr = 1.0;
        let mut measured = fill;
        for _ in 0..20_000 {
            fill += incoming_per_cycle;
            // Like the process callback: measure what's queued, then let the
            // resampler consume quantum / rate input frames (rate = 1 / corr).
            measured = fill;
            let error = (target - measured).clamp(-256.0, 256.0);
            corr = dll.update(error);
            fill -= quantum as f64 * corr;
        }
        assert!((measured - target).abs() < 16.0, "fill {measured} vs target {target}");
        assert!((corr - 1.0002).abs() < 1e-5, "corr {corr}");
    }
}
