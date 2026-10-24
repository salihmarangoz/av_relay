//! Playing audio a little faster without changing its pitch, to catch up
//! when more is queued than the mic wants (after the playout delay drops,
//! or after a stall's backlog).
//!
//! Like WebRTC's NetEq (its "accelerate"), a cycle leaves out a stretch of
//! audio as long as some whole pitch periods: where the audio repeats itself
//! that closely, crossfading from one period into a later one is inaudible
//! (WSOLA). Speech that doesn't repeat (consonants, onsets) is played as it
//! is. Quiet audio needs no match: any stretch of it can go.

/// Shortest and longest stretch left out: pitch periods of 2.5-20 ms
/// (400-50 Hz).
pub const MIN_CUT: usize = 120;
pub const MAX_CUT: usize = 960;
/// How long the two stretches are compared over (10 ms)...
const COMPARE: usize = 480;
/// ...and crossfaded over (5 ms).
pub const CROSSFADE: usize = 240;
/// How alike the two must be to cut between them in loud audio: voiced
/// speech, not a consonant.
const MIN_LIKENESS: f32 = 0.9;
/// The search goes in steps of this, then looks around the best.
const COARSE: usize = 4;

/// How many frames of input [`shorten`] needs for `out` frames of output and
/// a cut of up to `cut`.
pub fn input_needed(out: usize, cut: usize) -> usize {
    (out + cut).max(cut + COMPARE)
}

/// Fills `out` from the start of `input`, leaving out up to `max_cut`
/// frames (at least [`MIN_CUT`]): any amount when `quiet`, else only where
/// the audio repeats a pitch period. `input` holds at least
/// [`input_needed`]`(out.len(), max_cut)` frames. Returns how many frames of
/// `input` it used: `out.len()` plus the cut.
pub fn shorten(input: &[f32], out: &mut [f32], max_cut: usize, quiet: bool) -> usize {
    let n = out.len();
    let max_cut = max_cut.min(MAX_CUT);
    let cut = if max_cut < MIN_CUT {
        None
    } else if quiet {
        Some(max_cut)
    } else {
        best_cut(input, max_cut)
    };
    let Some(cut) = cut else {
        out.copy_from_slice(&input[..n]);
        return n;
    };
    let fade = CROSSFADE.min(n);
    for (i, o) in out[..fade].iter_mut().enumerate() {
        // Raised cosine: smooth at both ends.
        let w = 0.5 - 0.5 * (std::f32::consts::PI * (i as f32 + 0.5) / fade as f32).cos();
        *o = input[i] * (1.0 - w) + input[cut + i] * w;
    }
    out[fade..].copy_from_slice(&input[cut + fade..cut + n]);
    n + cut
}

/// The cut, up to `max_cut`, after which the audio is most like its start;
/// `None` when nothing is alike enough.
fn best_cut(input: &[f32], max_cut: usize) -> Option<usize> {
    let likeness = |cut: usize| {
        let (a, b) = (&input[..COMPARE], &input[cut..cut + COMPARE]);
        let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
        for (x, y) in a.iter().zip(b) {
            ab += x * y;
            aa += x * x;
            bb += y * y;
        }
        if aa * bb > 0.0 { ab / (aa * bb).sqrt() } else { 0.0 }
    };
    let pick = |range: std::iter::StepBy<std::ops::RangeInclusive<usize>>| {
        range.map(|c| (c, likeness(c))).max_by(|x, y| x.1.total_cmp(&y.1))
    };
    let (coarse, _) = pick((MIN_CUT..=max_cut).step_by(COARSE))?;
    let around = coarse.saturating_sub(COARSE - 1).max(MIN_CUT)..=(coarse + COARSE - 1).min(max_cut);
    let (cut, alike) = pick(around.step_by(1))?;
    (alike >= MIN_LIKENESS).then_some(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, frames: usize, amplitude: f32) -> Vec<f32> {
        (0..frames).map(|i| (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin() * amplitude).collect()
    }

    /// Frequency from upward zero crossings.
    fn frequency(x: &[f32]) -> f32 {
        let ups = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * 48_000.0 / x.len() as f32
    }

    #[test]
    fn a_tone_loses_whole_periods_and_keeps_its_pitch() {
        // 200 Hz: a period is 240 frames.
        let input = tone(200.0, 4096, 0.5);
        let mut out = vec![0.0; 1024];
        let used = shorten(&input, &mut out, 900, false);
        let cut = used - 1024;
        assert!(cut % 240 <= 1 || cut % 240 >= 239, "cut {cut}");
        // Continuous: no step bigger than the tone's own.
        let worst = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.02, "a jump of {worst}");
        // Played many times over, still 200 Hz.
        let (mut pos, mut played) = (0, Vec::new());
        let long = tone(200.0, 96_000, 0.5);
        while pos + input_needed(1024, 900) <= long.len() {
            pos += shorten(&long[pos..], &mut out, 900, false);
            played.extend_from_slice(&out);
        }
        assert!(played.len() < pos * 9 / 10, "{} of {pos} played", played.len());
        let hz = frequency(&played);
        assert!((hz - 200.0).abs() < 2.0, "{hz} Hz");
    }

    #[test]
    fn loud_audio_that_doesnt_repeat_is_played_as_it_is() {
        // White noise: nothing repeats.
        let mut seed = 1u32;
        let input: Vec<f32> = (0..4096)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let mut out = vec![0.0; 1024];
        assert_eq!(shorten(&input, &mut out, 900, false), 1024);
        assert_eq!(out, input[..1024]);
    }

    #[test]
    fn quiet_audio_loses_as_much_as_allowed() {
        let input = tone(1234.0, 4096, 0.001);
        let mut out = vec![0.0; 1024];
        assert_eq!(shorten(&input, &mut out, 700, true), 1724);
        // Too little to cut: played as it is.
        assert_eq!(shorten(&input, &mut out, MIN_CUT - 1, true), 1024);
    }
}
