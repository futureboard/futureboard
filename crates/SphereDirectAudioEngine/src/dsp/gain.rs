#![allow(dead_code)]

/// Apply a linear gain to a sample buffer in-place.
#[inline]
pub fn apply_gain(buffer: &mut [f32], gain: f32) {
    for s in buffer.iter_mut() {
        *s *= gain;
    }
}

/// Convert dBFS to linear amplitude.  Clamps to 0.0 for very negative dB.
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        10.0f32.powf(db / 20.0)
    }
}

/// Convert linear amplitude to dBFS.
#[inline]
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 1e-6 {
        -120.0
    } else {
        20.0 * linear.log10()
    }
}

/// Soft-knee master limiter for a single sample.
///
/// Replaces a hard `clamp(-1.0, 1.0)` on the master bus. Below `THRESHOLD` the
/// signal passes through at unity (transparent for normal levels); above it the
/// excess is smoothly compressed with a `tanh` knee that asymptotes to ±1.0, so
/// a hot bus is *limited* like a brick-wall limiter instead of hard-clipped into
/// harsh digital distortion. The output is still guaranteed to stay within
/// ±1.0, so nothing overflows the audio device.
///
/// Stateless and branch-cheap — safe to call per sample on the audio thread.
#[inline]
pub fn soft_limit(sample: f32) -> f32 {
    // Knee starts at ~ -1.9 dBFS. Below this the curve is exactly unity.
    const THRESHOLD: f32 = 0.8;
    let mag = sample.abs();
    if mag <= THRESHOLD {
        return sample;
    }
    let over = (mag - THRESHOLD) / (1.0 - THRESHOLD);
    let limited = THRESHOLD + (1.0 - THRESHOLD) * over.tanh();
    // `tanh` asymptotes below 1.0, but clamp defensively against FP edge cases.
    limited.copysign(sample).clamp(-1.0, 1.0)
}

/// Equal-power stereo pan, unity at center — the sin/cos pan-pot law of an
/// analog console, compensated so a centered channel passes at 0 dB.
///
/// `pan`: -1.0 = full left, 0.0 = center, 1.0 = full right.
/// Returns `(left_gain, right_gain)`:
///
/// * center → `(1.0, 1.0)` — existing mixes keep their level;
/// * the sweep holds constant power (`l² + r² == 2`), so a source does not dip
///   by 3 dB in the middle the way a linear balance law does;
/// * the extremes are exactly `(√2, 0)` / `(0, √2)`: hard-panned material
///   leaves the far speaker completely, +3 dB on the near one keeps the
///   perceived level.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    use std::f32::consts::{FRAC_PI_4, SQRT_2};
    let pan = pan.clamp(-1.0, 1.0);
    // Exact end points: `sin`/`cos` land a few ulps off zero at π/2, which is
    // still a bleed into the far channel.
    if pan <= -1.0 {
        return (SQRT_2, 0.0);
    }
    if pan >= 1.0 {
        return (0.0, SQRT_2);
    }
    if pan == 0.0 {
        return (1.0, 1.0);
    }
    let angle = (pan + 1.0) * FRAC_PI_4; // 0..π/2
    let (r, l) = angle.sin_cos();
    (l * SQRT_2, r * SQRT_2)
}

#[cfg(test)]
mod pan_tests {
    use super::pan_gains;

    #[test]
    fn center_is_unity_and_extremes_isolate_a_channel() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-1.0), (std::f32::consts::SQRT_2, 0.0));
        assert_eq!(pan_gains(1.0), (0.0, std::f32::consts::SQRT_2));
        // Out-of-range input clamps instead of overshooting.
        assert_eq!(pan_gains(-3.0), pan_gains(-1.0));
        assert_eq!(pan_gains(3.0), pan_gains(1.0));
    }

    #[test]
    fn sweep_holds_constant_power() {
        for step in -20..=20 {
            let (l, r) = pan_gains(step as f32 / 20.0);
            assert!((l * l + r * r - 2.0).abs() < 1.0e-5, "pan {step}: {l} {r}");
            assert!(l >= 0.0 && r >= 0.0);
        }
        let (l, r) = pan_gains(-0.5);
        assert!(l > r, "left of center favors the left channel");
    }
}
