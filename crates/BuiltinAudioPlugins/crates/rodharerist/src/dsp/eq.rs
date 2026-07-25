//! Studio parametric EQ stage — four fixed-role bands (low shelf, two
//! sweepable bells, high shelf) on the crate's [`StereoBiquad`], same
//! cascade pattern as the cabinet sim. Flat settings are bit-transparent by
//! construction: a band at 0 dB installs no filter at all.

use builtin_dsp_core::make_eq_coefficients;

use super::StereoBiquad;

const LOW_SHELF_HZ: f32 = 120.0;
const HIGH_SHELF_HZ: f32 = 6_000.0;
const BELL_Q: f32 = 0.9;

#[derive(Debug, Clone)]
pub(super) struct EqStage {
    sample_rate: f32,
    low: StereoBiquad,
    mid1: StereoBiquad,
    mid2: StereoBiquad,
    high: StereoBiquad,
}

impl EqStage {
    pub(super) fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            low: StereoBiquad::none(),
            mid1: StereoBiquad::none(),
            mid2: StereoBiquad::none(),
            high: StereoBiquad::none(),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
    }

    pub(super) fn reset(&mut self) {
        self.low.reset();
        self.mid1.reset();
        self.mid2.reset();
        self.high.reset();
    }

    /// Editor units: gains ±15 dB, `mid1_freq` 100..1000 Hz,
    /// `mid2_freq` 600..6000 Hz.
    pub(super) fn configure(
        &mut self,
        low_gain: f32,
        mid1_freq: f32,
        mid1_gain: f32,
        mid2_freq: f32,
        mid2_gain: f32,
        high_gain: f32,
    ) {
        let sr = self.sample_rate;
        // A 0 dB band is a true bypass (no biquad), so a flat EQ adds zero
        // filter state and is exactly transparent.
        let band = |kind: &str, freq: f32, gain: f32, q: f32| {
            if gain.abs() < 0.05 {
                None
            } else {
                make_eq_coefficients(kind, freq, gain.clamp(-15.0, 15.0), q, sr)
            }
        };
        self.low
            .set(band("lowshelf", LOW_SHELF_HZ, low_gain, 0.707));
        self.mid1.set(band(
            "bell",
            mid1_freq.clamp(100.0, 1_000.0),
            mid1_gain,
            BELL_Q,
        ));
        self.mid2.set(band(
            "bell",
            mid2_freq.clamp(600.0, 6_000.0),
            mid2_gain,
            BELL_Q,
        ));
        self.high
            .set(band("highshelf", HIGH_SHELF_HZ, high_gain, 0.707));
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (mut l, mut r) = self.low.run(left, right);
        (l, r) = self.mid1.run(l, r);
        (l, r) = self.mid2.run(l, r);
        self.high.run(l, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_eq_is_bit_transparent() {
        let mut eq = EqStage::new(48_000.0);
        eq.configure(0.0, 400.0, 0.0, 2_000.0, 0.0, 0.0);
        for n in 0..1_000 {
            let x = (n as f32 * 0.05).sin() * 0.7;
            assert_eq!(eq.process(x, -x), (x, -x));
        }
    }

    #[test]
    fn boosts_change_the_signal_and_stay_finite() {
        let mut eq = EqStage::new(48_000.0);
        eq.configure(6.0, 400.0, -4.0, 2_000.0, 3.0, 5.0);
        let mut differs = false;
        for n in 0..4_000 {
            let x = (n as f32 * 0.05).sin() * 0.5;
            let (l, r) = eq.process(x, x);
            assert!(l.is_finite() && r.is_finite());
            if (l - x).abs() > 1.0e-4 {
                differs = true;
            }
        }
        assert!(differs, "non-flat EQ left the signal untouched");
    }
}
