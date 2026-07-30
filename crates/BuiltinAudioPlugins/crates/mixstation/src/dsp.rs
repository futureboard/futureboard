//! Allocation-free signal-processing building blocks for MixStation.

use builtin_dsp_core::{clamp, db_to_linear, flush_denormal, time_constant};

const SMOOTH_SECONDS: f32 = 0.020;

#[derive(Debug, Clone, Copy)]
pub struct SmoothedGain {
    value: f32,
    target: f32,
    coeff: f32,
}

impl SmoothedGain {
    pub fn new(sample_rate: f32, db: f32) -> Self {
        let value = db_to_linear(db);
        Self {
            value,
            target: value,
            coeff: time_constant(sample_rate.max(1.0), SMOOTH_SECONDS),
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.coeff = time_constant(sample_rate.max(1.0), SMOOTH_SECONDS);
    }

    pub fn set_db(&mut self, db: f32) {
        self.target = db_to_linear(db);
    }

    pub fn snap_db(&mut self, db: f32) {
        self.target = db_to_linear(db);
        self.value = self.target;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        self.value = flush_denormal(self.coeff * self.value + (1.0 - self.coeff) * self.target);
        self.value
    }
}

/// Transposed direct-form-II biquad. Coefficients are replaced without
/// clearing history, avoiding a state discontinuity when an EQ control moves.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
}

impl Biquad {
    pub const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: [0.0; 2],
            z2: [0.0; 2],
        }
    }

    fn set_normalized(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inverse = 1.0 / a0;
        self.b0 = b0 * inverse;
        self.b1 = b1 * inverse;
        self.b2 = b2 * inverse;
        self.a1 = a1 * inverse;
        self.a2 = a2 * inverse;
    }

    pub fn set_high_pass(&mut self, sample_rate: f32, frequency: f32) {
        let (cos, _, alpha) = common(sample_rate, frequency, std::f32::consts::FRAC_1_SQRT_2);
        self.set_normalized(
            (1.0 + cos) * 0.5,
            -(1.0 + cos),
            (1.0 + cos) * 0.5,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        );
    }

    pub fn set_low_pass(&mut self, sample_rate: f32, frequency: f32) {
        let (cos, _, alpha) = common(sample_rate, frequency, std::f32::consts::FRAC_1_SQRT_2);
        self.set_normalized(
            (1.0 - cos) * 0.5,
            1.0 - cos,
            (1.0 - cos) * 0.5,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        );
    }

    pub fn set_peak(&mut self, sample_rate: f32, frequency: f32, gain_db: f32, q: f32) {
        let a = 10.0f32.powf(gain_db / 40.0);
        let (cos, _, alpha) = common(sample_rate, frequency, q);
        self.set_normalized(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        );
    }

    pub fn set_low_shelf(&mut self, sample_rate: f32, frequency: f32, gain_db: f32) {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w = omega(sample_rate, frequency);
        let cos = w.cos();
        let sin = w.sin();
        let root = a.sqrt();
        let alpha = sin * std::f32::consts::FRAC_1_SQRT_2;
        let two = 2.0 * root * alpha;
        self.set_normalized(
            a * ((a + 1.0) - (a - 1.0) * cos + two),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - two),
            (a + 1.0) + (a - 1.0) * cos + two,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - two,
        );
    }

    pub fn set_high_shelf(&mut self, sample_rate: f32, frequency: f32, gain_db: f32) {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w = omega(sample_rate, frequency);
        let cos = w.cos();
        let sin = w.sin();
        let root = a.sqrt();
        let alpha = sin * std::f32::consts::FRAC_1_SQRT_2;
        let two = 2.0 * root * alpha;
        self.set_normalized(
            a * ((a + 1.0) + (a - 1.0) * cos + two),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - two),
            (a + 1.0) - (a - 1.0) * cos + two,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - two,
        );
    }

    #[inline]
    pub fn process(&mut self, channel: usize, input: f32) -> f32 {
        let output = self.b0 * input + self.z1[channel];
        self.z1[channel] = flush_denormal(self.b1 * input - self.a1 * output + self.z2[channel]);
        self.z2[channel] = flush_denormal(self.b2 * input - self.a2 * output);
        output
    }

    pub fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }
}

fn omega(sample_rate: f32, frequency: f32) -> f32 {
    2.0 * std::f32::consts::PI * clamp(frequency, 10.0, sample_rate.max(1.0) * 0.49)
        / sample_rate.max(1.0)
}

fn common(sample_rate: f32, frequency: f32, q: f32) -> (f32, f32, f32) {
    let w = omega(sample_rate, frequency);
    let sin = w.sin();
    (w.cos(), sin, sin / (2.0 * q.max(0.1)))
}

#[derive(Debug, Clone)]
pub struct Filters {
    pub high_pass: Biquad,
    pub low_pass: Biquad,
    pub eq: [Biquad; 4],
}

impl Filters {
    pub fn new() -> Self {
        Self {
            high_pass: Biquad::identity(),
            low_pass: Biquad::identity(),
            eq: [Biquad::identity(); 4],
        }
    }

    pub fn reset(&mut self) {
        self.high_pass.reset();
        self.low_pass.reset();
        for band in &mut self.eq {
            band.reset();
        }
    }
}

/// One-pole DC blocker for the biased saturation stage.
#[derive(Debug, Clone, Copy)]
pub struct DcBlocker {
    coefficient: f32,
    previous_input: [f32; 2],
    previous_output: [f32; 2],
}

impl DcBlocker {
    pub fn new(sample_rate: f32) -> Self {
        let mut blocker = Self {
            coefficient: 0.0,
            previous_input: [0.0; 2],
            previous_output: [0.0; 2],
        };
        blocker.set_sample_rate(sample_rate);
        blocker
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.coefficient = (-2.0 * std::f32::consts::PI * 5.0 / sample_rate.max(1.0)).exp();
    }

    #[inline]
    pub fn process(&mut self, channel: usize, input: f32) -> f32 {
        let output =
            input - self.previous_input[channel] + self.coefficient * self.previous_output[channel];
        self.previous_input[channel] = input;
        self.previous_output[channel] = flush_denormal(output);
        self.previous_output[channel]
    }

    pub fn reset(&mut self) {
        self.previous_input = [0.0; 2];
        self.previous_output = [0.0; 2];
    }
}

/// Level-matched soft saturation. Character moves from symmetric to a biased
/// transfer curve, adding even harmonics. The channel strip follows this with
/// [`DcBlocker`] so the asymmetry cannot offset the output waveform.
#[inline]
pub fn saturate(sample: f32, drive: f32, character: f32) -> f32 {
    if drive <= 1.000_001 {
        sample
    } else {
        let bias = (character.clamp(0.0, 1.0) - 0.5) * 0.5;
        let offset = (bias * drive).tanh();
        let shaped = ((sample + bias) * drive).tanh() - offset;
        let normalization = if sample >= 0.0 {
            (((1.0 + bias) * drive).tanh() - offset).max(1.0e-6)
        } else {
            (offset - ((-1.0 + bias) * drive).tanh()).max(1.0e-6)
        };
        shaped / normalization
    }
}

#[inline]
pub fn stereo_width(left: f32, right: f32, width: f32) -> (f32, f32) {
    let mid = (left + right) * 0.5;
    let side = (left - right) * 0.5 * width;
    (mid + side, mid - side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_stay_finite_at_extreme_settings() {
        let mut filter = Biquad::identity();
        for frequency in [10.0, 20_000.0, 23_500.0] {
            filter.set_peak(48_000.0, frequency, 18.0, 12.0);
            for n in 0..10_000 {
                let output = filter.process(0, if n & 1 == 0 { 1.0 } else { -1.0 });
                assert!(output.is_finite());
            }
        }
    }

    #[test]
    fn width_preserves_mid_and_scales_side() {
        assert_eq!(stereo_width(0.5, -0.5, 0.0), (0.0, 0.0));
        assert_eq!(stereo_width(0.5, -0.5, 2.0), (1.0, -1.0));
        assert_eq!(stereo_width(0.25, 0.25, 2.0), (0.25, 0.25));
    }

    #[test]
    fn biased_saturation_preserves_both_polarities_and_zero() {
        let positive = saturate(1.0, 12.0, 1.0);
        let negative = saturate(-1.0, 12.0, 1.0);
        assert!((positive - 1.0).abs() < 1.0e-6);
        assert!((negative + 1.0).abs() < 1.0e-6);
        assert!(saturate(0.0, 12.0, 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn dc_blocker_removes_biased_saturation_offset() {
        let mut blocker = DcBlocker::new(48_000.0);
        let mut sum = 0.0;
        let mut count = 0;
        for n in 0..96_000 {
            let input = (n as f32 * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.8;
            let output = blocker.process(0, saturate(input, 12.0, 1.0));
            if n >= 48_000 {
                sum += output;
                count += 1;
            }
        }
        assert!((sum / count as f32).abs() < 1.0e-4);
    }
}
