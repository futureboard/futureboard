//! Wah stage — a resonant band-pass sweep, either pedal-position driven
//! ("Cry Wah") or envelope driven ("Touch Wah").
//!
//! The filter is a Chamberlin state-variable filter per channel: cheap,
//! stable over the wah sweep range, and its band-pass output is exactly the
//! vocal formant a wah is. The swept frequency is smoothed like a physical
//! pedal so knob edits (or a jumpy envelope) never zipper.

use super::smooth::Smoothed;
use crate::dsp::WahModel;

/// Sweep bounds in Hz — the classic crybaby range.
const SWEEP_LO_HZ: f32 = 350.0;
const SWEEP_HI_HZ: f32 = 2_200.0;

/// Pedal glide: how fast the swept corner may move (seconds).
const PEDAL_SMOOTH_SECONDS: f32 = 0.030;

/// Knob glide for resonance/level style edits.
const SMOOTH_SECONDS: f32 = 0.010;

/// Envelope follower times for the touch wah (seconds).
const ENV_ATTACK_SECONDS: f32 = 0.004;
const ENV_RELEASE_SECONDS: f32 = 0.120;

/// One Chamberlin SVF channel. Band-pass output only.
#[derive(Debug, Clone, Copy, Default)]
struct Svf {
    low: f32,
    band: f32,
}

impl Svf {
    fn reset(&mut self) {
        self.low = 0.0;
        self.band = 0.0;
    }

    /// `f` is the SVF frequency coefficient, `q_inv` the damping (1/Q).
    #[inline]
    fn band_pass(&mut self, input: f32, f: f32, q_inv: f32) -> f32 {
        self.low = super::flush_denormal(self.low + f * self.band);
        let high = input - self.low - q_inv * self.band;
        self.band = super::flush_denormal(self.band + f * high);
        self.band
    }
}

#[derive(Debug, Clone)]
pub(super) struct Wah {
    sample_rate: f32,
    model: WahModel,
    svf_l: Svf,
    svf_r: Svf,
    /// Normalized pedal position 0..1 (smoothed like a real pedal).
    position: Smoothed,
    /// Resonance as 1/Q (smoothed).
    q_inv: Smoothed,
    /// Envelope sensitivity 0..1 (touch model only).
    sensitivity: f32,
    env: f32,
    env_attack: f32,
    env_release: f32,
}

impl Wah {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            model: WahModel::CryWah,
            svf_l: Svf::default(),
            svf_r: Svf::default(),
            position: Smoothed::new(sr, PEDAL_SMOOTH_SECONDS, 0.45),
            q_inv: Smoothed::new(sr, SMOOTH_SECONDS, 1.0 / 6.0),
            sensitivity: 0.5,
            env: 0.0,
            env_attack: env_coeff(sr, ENV_ATTACK_SECONDS),
            env_release: env_coeff(sr, ENV_RELEASE_SECONDS),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        self.position.set_time(sr, PEDAL_SMOOTH_SECONDS);
        self.q_inv.set_time(sr, SMOOTH_SECONDS);
        self.env_attack = env_coeff(sr, ENV_ATTACK_SECONDS);
        self.env_release = env_coeff(sr, ENV_RELEASE_SECONDS);
        self.reset();
    }

    pub(super) fn reset(&mut self) {
        self.svf_l.reset();
        self.svf_r.reset();
        self.position.snap();
        self.q_inv.snap();
        self.env = 0.0;
    }

    /// `pos`, `res` and `sens` are 0..10 knobs (see `data.ts`).
    pub(super) fn configure(&mut self, model: WahModel, pos: f32, res: f32, sens: f32) {
        if self.model != model {
            self.model = model;
            // A model swap changes what drives the sweep; old filter energy
            // at the previous corner would smear into the new sound.
            self.svf_l.reset();
            self.svf_r.reset();
            self.env = 0.0;
        }
        self.position.set_target((pos / 10.0).clamp(0.0, 1.0));
        // Q from 2 (broad) to 12 (screaming vowel).
        let q = 2.0 + (res / 10.0).clamp(0.0, 1.0) * 10.0;
        self.q_inv.set_target(1.0 / q);
        self.sensitivity = (sens / 10.0).clamp(0.0, 1.0);
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let pedal = self.position.tick();
        let q_inv = self.q_inv.tick();

        // Where in the sweep 0..1 the filter sits this sample.
        let sweep = match self.model {
            WahModel::CryWah => pedal,
            WahModel::TouchWah => {
                // Envelope follower on the stereo peak; sensitivity scales
                // how far a given level pushes the sweep above the heel
                // position set by the pedal knob.
                let level = left.abs().max(right.abs());
                let coeff = if level > self.env {
                    self.env_attack
                } else {
                    self.env_release
                };
                self.env = level + coeff * (self.env - level);
                (pedal + self.env * (2.0 + 6.0 * self.sensitivity)).min(1.0)
            }
        };

        let freq = SWEEP_LO_HZ * (SWEEP_HI_HZ / SWEEP_LO_HZ).powf(sweep);
        // Chamberlin coefficient; clamped for stability at high rates/corners.
        let f = (2.0 * (std::f32::consts::PI * freq / self.sample_rate).sin()).clamp(0.0, 0.9);

        // A touch of dry keeps low-string body under the vocal peak.
        let wet_l = self.svf_l.band_pass(left, f, q_inv);
        let wet_r = self.svf_r.band_pass(right, f, q_inv);
        (wet_l * 0.9 + left * 0.15, wet_r * 0.9 + right * 0.15)
    }
}

/// One-pole envelope coefficient (same shape as `builtin_dsp_core::time_constant`).
fn env_coeff(sample_rate: f32, seconds: f32) -> f32 {
    (-1.0 / (sample_rate.max(1.0) * seconds.max(1.0e-4))).exp()
}
