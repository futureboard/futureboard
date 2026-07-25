//! Dedicated processing topologies for the four modern drive models
//! (`ds_one`, `super_drive`, `metal_core`, `tight_rift`).
//!
//! The six legacy voicings share `Drive`'s single generic path; these four
//! each own a full chain — DC block → model pre-EQ → envelope/sag →
//! oversampled nonlinear stage(s) with interstage EQ → post-EQ → fizz control
//! → fixed-ceiling makeup → equal-power dry/wet — so their character comes
//! from topology, not just gain and a low-pass.
//!
//! Level rule: every clipper here is its own level regulator, so nothing on
//! the output path may scale *inversely* with the Drive knob. Makeup gain
//! ([`drive_makeup`]), clip ceilings and sag detectors ([`sag_supply`]) are
//! all drive-independent — turning Drive up adds saturation and a little
//! level, and turning it down can never make the pedal louder.
//!
//! Realtime rules: every filter/oversampler/envelope is preallocated; the
//! per-sample path is arithmetic + biquads only. `configure` (control thread)
//! maps the three editor knobs onto many internal targets; continuous values
//! glide through [`Smoothed`]. Interstage filters that run inside the
//! oversampled domain are configured at the oversampled rate.
//!
//! Filter coefficient swaps go through `StereoBiquad::set`, which retunes in
//! place and keeps the filter's history — necessary here, because `configure`
//! re-sets every filter on any parameter change, and a zeroed history ahead of
//! this much gain steps the signal hard enough to flip the clipped sign.

use builtin_dsp_core::{db_to_linear, make_eq_coefficients, time_constant};

use super::StereoBiquad;
use super::smooth::{Oversampler4x, Oversampler8x, Smoothed};

/// Glide time for all smoothed drive internals.
const SMOOTH_SECONDS: f32 = 0.010;

/// Perceptual drive taper: more resolution in the low half of the knob,
/// faster growth up top.
#[inline]
fn drive_curve(g01: f32) -> f32 {
    g01.clamp(0.0, 1.0).powf(1.6)
}

/// Flush a possibly-denormal/non-finite intermediate back to safe territory.
#[inline]
fn sanitize(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

// ---------------------------------------------------------------------------
// Shared primitives
// ---------------------------------------------------------------------------

/// One-pole DC blocker (~18 Hz), stereo. `y = x - x1 + r·y1`.
#[derive(Debug, Clone)]
pub(super) struct DcBlock {
    r: f32,
    x1_l: f32,
    y1_l: f32,
    x1_r: f32,
    y1_r: f32,
}

impl DcBlock {
    pub(super) fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            r: 0.0,
            x1_l: 0.0,
            y1_l: 0.0,
            x1_r: 0.0,
            y1_r: 0.0,
        };
        s.set_sample_rate(sample_rate);
        s
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.r = (1.0 - (2.0 * std::f32::consts::PI * 18.0) / sr).clamp(0.9, 0.999_99);
    }

    pub(super) fn reset(&mut self) {
        self.x1_l = 0.0;
        self.y1_l = 0.0;
        self.x1_r = 0.0;
        self.y1_r = 0.0;
    }

    #[inline]
    pub(super) fn run(&mut self, l: f32, r: f32) -> (f32, f32) {
        let yl = l - self.x1_l + self.r * self.y1_l;
        let yr = r - self.x1_r + self.r * self.y1_r;
        self.x1_l = l;
        self.y1_l = sanitize(yl);
        self.x1_r = r;
        self.y1_r = sanitize(yr);
        (self.y1_l, self.y1_r)
    }
}

/// Per-channel envelope follower with independent attack/release, for sag and
/// dynamic asymmetry. Stable for silence, impulses, DC and hostile input:
/// the state is sanitized every tick and can only decay toward the rectified
/// input.
#[derive(Debug, Clone)]
pub(super) struct EnvFollower {
    attack_secs: f32,
    release_secs: f32,
    attack: f32,
    release: f32,
    env_l: f32,
    env_r: f32,
}

impl EnvFollower {
    pub(super) fn new(sample_rate: f32, attack_secs: f32, release_secs: f32) -> Self {
        let mut e = Self {
            attack_secs,
            release_secs,
            attack: 0.0,
            release: 0.0,
            env_l: 0.0,
            env_r: 0.0,
        };
        e.set_sample_rate(sample_rate);
        e
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.attack = time_constant(sr, self.attack_secs);
        self.release = time_constant(sr, self.release_secs);
    }

    pub(super) fn reset(&mut self) {
        self.env_l = 0.0;
        self.env_r = 0.0;
    }

    #[inline]
    fn follow(env: f32, x: f32, attack: f32, release: f32) -> f32 {
        let a = x.abs();
        let coeff = if a > env { attack } else { release };
        let next = a + (env - a) * coeff;
        if next.is_finite() && next > 1.0e-20 {
            next
        } else {
            0.0
        }
    }

    #[inline]
    pub(super) fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.env_l = Self::follow(self.env_l, l, self.attack, self.release);
        self.env_r = Self::follow(self.env_r, r, self.attack, self.release);
        (self.env_l, self.env_r)
    }
}

/// Hard clip with a small tanh knee and independent positive/negative
/// thresholds (both given as positive numbers). `knee = 0` degenerates to a
/// pure clamp.
#[inline]
fn hard_clip_asym(x: f32, t_pos: f32, t_neg: f32, knee: f32) -> f32 {
    let (sign, mag, th) = if x >= 0.0 {
        (1.0, x, t_pos)
    } else {
        (-1.0, -x, t_neg)
    };
    if knee <= 1.0e-6 {
        return sign * mag.min(th);
    }
    let knee_start = (th - knee).max(0.0);
    if mag <= knee_start {
        x
    } else {
        sign * (knee_start + knee * ((mag - knee_start) / knee).tanh())
    }
}

/// Soft asymmetric saturation: separate drive per half-cycle, normalized so
/// small signals pass at ~unity.
#[inline]
fn soft_asym(x: f32, k_pos: f32, k_neg: f32) -> f32 {
    if x >= 0.0 {
        (x * k_pos).tanh() / k_pos.max(1.0e-6)
    } else {
        (x * k_neg).tanh() / k_neg.max(1.0e-6)
    }
}

/// Equal-power dry/wet: keeps perceived level steady across the mix knob
/// (the legacy models' linear crossfade dips in the middle).
#[inline]
fn equal_power_mix(dry: f32, wet: f32, mix: f32) -> f32 {
    let m = mix.clamp(0.0, 1.0);
    dry * (1.0 - m).sqrt() + wet * m.sqrt()
}

/// Makeup trim applied *after* a clipper, given the drive curve.
///
/// A clipper is its own level regulator: past the threshold the output peak is
/// the threshold, no matter how hard it is driven. Compensating the *added
/// pre-gain* on the output — `-(gain_db) * 0.6`, the shape this file used to
/// have — therefore subtracts level that the clipper already removed, and the
/// pedal ends up loudest with the knob at zero and ~20 dB quieter wide open.
/// Drive must only ever add: this trim rises slightly so a harder setting also
/// reads a little hotter, and the Level knob stays the one level control.
#[inline]
fn drive_makeup(d: f32) -> f32 {
    1.0 + d.clamp(0.0, 1.0) * 0.28
}

/// Fixed output ceiling of the stacked overdrive stage.
///
/// Deliberately a constant and not `1/sqrt(drive)`: normalizing by the stage's
/// own drive collapses its ceiling exactly as it starts to saturate, which is
/// the same "turn it up, it gets quieter" trap as an inverse makeup gain.
const OD_CEILING: f32 = 0.62;

/// Overdrive soft clip stacked *in front* of the main distortion: an
/// asymmetric tanh (hotter on the negative half for even harmonics) into a
/// fixed ceiling, so raising its drive raises both level and grit. This is the
/// tube screamer trick — tighten, thicken and grit the signal, then let the
/// model's own pre-gain slam it into the real clipper. Runs inside the model's
/// oversampler (memoryless), so its harmonics don't alias.
#[inline]
fn od_clip(x: f32, drive: f32) -> f32 {
    let d = drive.max(1.0);
    let g = x * d;
    let y = if g >= 0.0 {
        g.tanh()
    } else {
        // Negative half saturates ~25% hotter → even harmonics + a little DC
        // (each model's `dc_out` strips the offset downstream).
        (g * 0.8).tanh() * (1.0 / 0.8)
    };
    y * OD_CEILING
}

/// Bias offset for a clipper's *input*, driven by a pre-gain envelope.
///
/// Unequal clip thresholds alone do not survive DC blocking: once a clipper is
/// fully engaged its two halves differ only by a constant, which is exactly
/// what the blocker removes — leaving a purely odd-harmonic square, thin and
/// fizzy no matter how hard it is driven (measured: h2 sixty-odd dB below the
/// fundamental at full drive). Offsetting the *input* changes the pulse width
/// instead, and pulse-width asymmetry is real even-harmonic content that
/// survives DC blocking. Driving it from the envelope keeps the offset
/// proportional to what is being played, so the duty shift is level-invariant
/// and blooms with pick attack rather than sitting there as a static buzz.
#[inline]
fn duty_bias(env: f32, depth: f32) -> f32 {
    depth * env.min(1.5)
}

/// Supply sag from an envelope of the *pre-gain* signal, bounded.
///
/// Detecting on the post-gain signal turns sag into an automatic gain control:
/// the envelope scales with the drive knob, so the harder the model is driven
/// the more the sag term takes back — up to 10 dB of the gain just added,
/// which is why the high-gain models never reached a real wall. The detector
/// therefore sees playing dynamics only, and the supply can never collapse
/// below `floor`.
#[inline]
fn sag_supply(env: f32, amount: f32, floor: f32) -> f32 {
    (1.0 / (1.0 + env.min(3.0) * amount)).max(floor)
}

/// Tube-screamer-style overdrive placed ahead of a high-gain distortion. A
/// low-tighten high-pass and a mid-emphasis bell voice the signal on the base
/// path; [`od_clip`] then boosts and softly saturates it inside the model's
/// oversampler, before the model's own pre-gain drives the main clipper.
///
/// Its job is grit, low-mid body and tightness — the classic fix for a thin,
/// fizzy high-gain tone — not raw level (the model's pre-gain owns that).
#[derive(Debug, Clone)]
pub(super) struct OdBoost {
    tighten_hpf: StereoBiquad,
    mid_push: StereoBiquad,
    drive: Smoothed,
}

impl OdBoost {
    pub(super) fn new(sample_rate: f32) -> Self {
        Self {
            tighten_hpf: StereoBiquad::none(),
            mid_push: StereoBiquad::none(),
            drive: Smoothed::new(sample_rate.max(1.0), SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.drive.set_time(sample_rate.max(1.0), SMOOTH_SECONDS);
    }

    pub(super) fn reset(&mut self) {
        self.tighten_hpf.reset();
        self.mid_push.reset();
        self.drive.snap();
    }

    /// `amount` (0..1, typically the drive curve) scales both the soft-clip
    /// hardness and the mid push; `mid_hz` voices the honk that cuts through.
    pub(super) fn configure(&mut self, amount: f32, mid_hz: f32, sample_rate: f32) {
        let a = amount.clamp(0.0, 1.0);
        // Gentle grit at low drive, screaming at full.
        self.drive.set_target(1.5 + a * 7.5);
        // Tighten the lows entering the clipper so bass doesn't turn to mush;
        // the corner climbs with drive so the wall stays articulate.
        self.tighten_hpf.set(make_eq_coefficients(
            "highpass",
            115.0 + a * 75.0,
            0.0,
            0.707,
            sample_rate,
        ));
        // Mid emphasis is the tube-screamer honk — grows with drive.
        self.mid_push.set(make_eq_coefficients(
            "bell",
            mid_hz,
            4.0 + a * 6.5,
            0.7,
            sample_rate,
        ));
    }

    /// Base-rate voicing EQ, run before the oversampled [`od_clip`].
    #[inline]
    pub(super) fn pre_eq(&mut self, l: f32, r: f32) -> (f32, f32) {
        let (l, r) = self.tighten_hpf.run(l, r);
        self.mid_push.run(l, r)
    }

    /// This block-sample's soft-clip drive (glides with the knob).
    #[inline]
    pub(super) fn tick_drive(&mut self) -> f32 {
        self.drive.tick()
    }
}

// ---------------------------------------------------------------------------
// DS Classic — raw orange-box hard clipper (4×)
// ---------------------------------------------------------------------------

/// Dry, rude, compressed grit: pre-emphasized upper mids into an asymmetric
/// hard clip with a small knee, then a resonant edge and a firm low-pass.
#[derive(Debug, Clone)]
pub(super) struct DsClassic {
    sample_rate: f32,
    dc: DcBlock,
    dc_out: DcBlock,
    input_hpf: StereoBiquad,
    pre_emph: StereoBiquad,
    edge: StereoBiquad,
    post_lpf: StereoBiquad,
    os: Oversampler4x,
    env: EnvFollower,
    pre_gain: Smoothed,
    t_pos: Smoothed,
    t_neg: Smoothed,
    knee: Smoothed,
    bias_depth: Smoothed,
    out_gain: Smoothed,
    mix: Smoothed,
}

impl DsClassic {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            dc: DcBlock::new(sr),
            dc_out: DcBlock::new(sr),
            input_hpf: StereoBiquad::none(),
            pre_emph: StereoBiquad::none(),
            edge: StereoBiquad::none(),
            post_lpf: StereoBiquad::none(),
            os: Oversampler4x::new(),
            env: EnvFollower::new(sr, 0.003, 0.070),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            t_pos: Smoothed::new(sr, SMOOTH_SECONDS, 0.6),
            t_neg: Smoothed::new(sr, SMOOTH_SECONDS, 0.75),
            knee: Smoothed::new(sr, SMOOTH_SECONDS, 0.12),
            bias_depth: Smoothed::new(sr, SMOOTH_SECONDS, 0.1),
            out_gain: Smoothed::new(sr, SMOOTH_SECONDS, 0.5),
            mix: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.dc.set_sample_rate(self.sample_rate);
        self.dc_out.set_sample_rate(self.sample_rate);
        self.env.set_sample_rate(self.sample_rate);
        for s in [
            &mut self.pre_gain,
            &mut self.t_pos,
            &mut self.t_neg,
            &mut self.knee,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.set_time(self.sample_rate, SMOOTH_SECONDS);
        }
    }

    pub(super) fn reset(&mut self) {
        self.dc.reset();
        self.dc_out.reset();
        self.input_hpf.reset();
        self.pre_emph.reset();
        self.edge.reset();
        self.post_lpf.reset();
        self.os.reset();
        self.env.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.t_pos,
            &mut self.t_neg,
            &mut self.knee,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Editor knobs 0..10. Drive maps onto gain, thresholds, knee hardness,
    /// pre-emphasis and compensation together; Tone rides the resonant edge
    /// and the post low-pass as one gesture (dark mid-grind ↔ sharp bite).
    pub(super) fn configure(&mut self, gain: f32, tone: f32, level: f32) {
        let d = drive_curve(gain / 10.0);
        let t = (tone / 10.0).clamp(0.0, 1.0);
        let lvl = (level / 10.0).clamp(0.0, 1.0);
        let sr = self.sample_rate;

        let gain_db = 10.0 + d * 30.0; // 10..40 dB into the clipper
        self.pre_gain.set_target(db_to_linear(gain_db));
        // Thresholds stay near-fixed (they are what sets the output level);
        // the halves stay deliberately unequal for the rude even-harmonic bite.
        self.t_pos.set_target(0.60 - d * 0.05);
        self.t_neg.set_target(0.76 - d * 0.04);
        // Knee hardens as drive rises — full drive is a bare clamp.
        self.knee.set_target(0.17 - d * 0.16);
        // ...which is exactly when unequal thresholds stop producing even
        // harmonics, so the duty-cycle bias takes over as drive climbs.
        self.bias_depth.set_target(0.06 + d * 0.15);
        // No inverse compensation: the clamp already sets the level, so the
        // knob only has to nudge it up. See `drive_makeup`.
        self.out_gain
            .set_target(drive_makeup(d) * (0.40 + lvl * 1.25));
        self.mix.set_target(1.0);

        self.dc.set_sample_rate(sr);
        self.dc_out.set_sample_rate(sr);
        self.input_hpf
            .set(make_eq_coefficients("highpass", 85.0, 0.0, 0.707, sr));
        // Pre-emphasis grows with drive: what screams is chosen before it clips.
        let emph_hz = 750.0 + t * 750.0; // 750..1500
        self.pre_emph.set(make_eq_coefficients(
            "bell",
            emph_hz,
            4.5 + d * 6.5,
            0.9,
            sr,
        ));
        // Tone: resonant edge sweeps 1.2..1.8 kHz while the ceiling opens.
        self.edge.set(make_eq_coefficients(
            "bell",
            1_200.0 + t * 600.0,
            2.5 + t * 2.0,
            1.4,
            sr,
        ));
        let lpf = (5_500.0 + t * 2_500.0).min(sr * 0.45); // 5.5..8 kHz
        self.post_lpf
            .set(make_eq_coefficients("lowpass", lpf, 0.0, 0.707, sr));
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let pre = self.pre_gain.tick();
        let t_pos = self.t_pos.tick();
        let t_neg = self.t_neg.tick();
        let knee = self.knee.tick().max(0.0);
        let depth = self.bias_depth.tick();
        let out = self.out_gain.tick();
        let mix = self.mix.tick();

        let (l, r) = self.dc.run(left, right);
        let (l, r) = self.input_hpf.run(l, r);
        let (l, r) = self.pre_emph.run(l, r);
        // Bias the clipper's input (pre-gain, so the duty shift is the same at
        // any playing level) — see `duty_bias`; `dc_out` strips the offset.
        let (el, er) = self.env.tick(l, r);
        let bias_l = duty_bias(el, depth);
        let bias_r = duty_bias(er, depth);
        let (mut wl, mut wr) =
            self.os
                .process_stereo((l + bias_l) * pre, (r + bias_r) * pre, |a, b| {
                    (
                        hard_clip_asym(a, t_pos, t_neg, knee),
                        hard_clip_asym(b, t_pos, t_neg, knee),
                    )
                });
        (wl, wr) = self.edge.run(wl, wr);
        (wl, wr) = self.post_lpf.run(wl, wr);
        // Asymmetric clipping generates DC — strip it before it eats
        // headroom or thumps the mix.
        (wl, wr) = self.dc_out.run(wl, wr);
        wl = sanitize(wl) * out;
        wr = sanitize(wr) * out;
        (
            equal_power_mix(left, wl, mix),
            equal_power_mix(right, wr, mix),
        )
    }
}

// ---------------------------------------------------------------------------
// Super Drive — dynamic asymmetric overdrive (4×)
// ---------------------------------------------------------------------------

/// Touch-sensitive: an envelope modulates the clip asymmetry (more even
/// harmonics as you dig in) and a sag term compresses hard playing; a clean
/// low band bypasses the clipper to keep the body.
#[derive(Debug, Clone)]
pub(super) struct SuperDrive {
    sample_rate: f32,
    dc: DcBlock,
    dc_out: DcBlock,
    input_hpf: StereoBiquad,
    mid_hump: StereoBiquad,
    low_keep: StereoBiquad,
    post_lpf: StereoBiquad,
    os: Oversampler4x,
    env: EnvFollower,
    pre_gain: Smoothed,
    sag_amount: Smoothed,
    low_blend: Smoothed,
    bias_depth: Smoothed,
    out_gain: Smoothed,
    mix: Smoothed,
}

impl SuperDrive {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            dc: DcBlock::new(sr),
            dc_out: DcBlock::new(sr),
            input_hpf: StereoBiquad::none(),
            mid_hump: StereoBiquad::none(),
            low_keep: StereoBiquad::none(),
            post_lpf: StereoBiquad::none(),
            os: Oversampler4x::new(),
            env: EnvFollower::new(sr, 0.005, 0.090),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            sag_amount: Smoothed::new(sr, SMOOTH_SECONDS, 0.0),
            low_blend: Smoothed::new(sr, SMOOTH_SECONDS, 0.18),
            bias_depth: Smoothed::new(sr, SMOOTH_SECONDS, 0.05),
            out_gain: Smoothed::new(sr, SMOOTH_SECONDS, 0.6),
            mix: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.dc.set_sample_rate(self.sample_rate);
        self.dc_out.set_sample_rate(self.sample_rate);
        self.env.set_sample_rate(self.sample_rate);
        for s in [
            &mut self.pre_gain,
            &mut self.sag_amount,
            &mut self.low_blend,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.set_time(self.sample_rate, SMOOTH_SECONDS);
        }
    }

    pub(super) fn reset(&mut self) {
        self.dc.reset();
        self.dc_out.reset();
        self.input_hpf.reset();
        self.mid_hump.reset();
        self.low_keep.reset();
        self.post_lpf.reset();
        self.os.reset();
        self.env.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.sag_amount,
            &mut self.low_blend,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Tone shifts the mid hump up and opens the top together.
    pub(super) fn configure(&mut self, gain: f32, tone: f32, level: f32) {
        let d = drive_curve(gain / 10.0);
        let t = (tone / 10.0).clamp(0.0, 1.0);
        let lvl = (level / 10.0).clamp(0.0, 1.0);
        let sr = self.sample_rate;

        let gain_db = 6.0 + d * 26.0; // 6..32 dB
        self.pre_gain.set_target(db_to_linear(gain_db));
        // Sag reads playing dynamics, not the drive knob (see `sag_supply`),
        // so this is a fixed dynamic character rather than a gain giveback.
        self.sag_amount.set_target(0.20 + d * 0.30);
        // Clean-low blend stays subtle and eases off as drive saturates.
        self.low_blend.set_target(0.22 - d * 0.10);
        // Unequal half-cycle ceilings alone read as symmetric once both halves
        // flat-top and the DC blocker centres them, so the touch-sensitive even
        // harmonics this model is built around need a duty shift too — gentler
        // than the hard-clipping models, since this one is meant to stay smooth.
        self.bias_depth.set_target(0.05 + d * 0.13);
        self.out_gain
            .set_target(drive_makeup(d) * (0.38 + lvl * 1.15));
        self.mix.set_target(1.0);

        self.dc.set_sample_rate(sr);
        self.dc_out.set_sample_rate(sr);
        self.input_hpf
            .set(make_eq_coefficients("highpass", 115.0, 0.0, 0.707, sr));
        let hump_hz = 650.0 + t * 300.0; // 650..950
        self.mid_hump.set(make_eq_coefficients(
            "bell",
            hump_hz,
            4.5 + d * 4.0,
            0.8,
            sr,
        ));
        // The clean body band that skips the clipper entirely.
        self.low_keep
            .set(make_eq_coefficients("lowpass", 190.0, 0.0, 0.707, sr));
        let lpf = (7_000.0 + t * 3_000.0).min(sr * 0.45); // 7..10 kHz
        self.post_lpf
            .set(make_eq_coefficients("lowpass", lpf, 0.0, 0.707, sr));
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let pre = self.pre_gain.tick();
        let sag = self.sag_amount.tick();
        let low_amt = self.low_blend.tick();
        let depth = self.bias_depth.tick();
        let out = self.out_gain.tick();
        let mix = self.mix.tick();

        let (l, r) = self.dc.run(left, right);
        let (low_l, low_r) = self.low_keep.run(l, r);
        let (l, r) = self.input_hpf.run(l, r);

        // Envelope drives both the sag and the asymmetry: dig in → more
        // compression, more even harmonics. The detector sits *before* the
        // pre-gain so playing dynamics move it, not the Drive knob.
        //
        // `soft_asym` normalizes by `1/k`, so a *lower* k is a *higher*
        // ceiling. The asymmetry therefore has to stay in a narrow band around
        // `SOFT_K`: an SD-1's extra diode offsets one half by a couple of dB,
        // and a wide range here instead produces a wildly lopsided wave
        // (+0.7/-2.0 at the extreme) that slews hard enough to read as a click.
        const SOFT_K: f32 = 1.45;
        // Metered before the mid hump, so dynamics move it and pitch does not.
        let (el, er) = self.env.tick(l, r);
        let sag_l = sag_supply(el, sag, 0.55);
        let sag_r = sag_supply(er, sag, 0.55);
        let asym_l = (SOFT_K - el * 0.55).clamp(1.0, SOFT_K);
        let asym_r = (SOFT_K - er * 0.55).clamp(1.0, SOFT_K);
        let bias_l = duty_bias(el, depth);
        let bias_r = duty_bias(er, depth);

        let (l, r) = self.mid_hump.run(l, r);
        let (mut wl, mut wr) = self.os.process_stereo(
            (l + bias_l) * pre * sag_l,
            (r + bias_r) * pre * sag_r,
            |a, b| (soft_asym(a, SOFT_K, asym_l), soft_asym(b, SOFT_K, asym_r)),
        );
        (wl, wr) = self.post_lpf.run(wl, wr);
        wl = sanitize(wl + low_l * low_amt);
        wr = sanitize(wr + low_r * low_amt);
        // Asymmetric clipping generates DC — strip it before it eats
        // headroom or thumps the mix.
        (wl, wr) = self.dc_out.run(wl, wr);
        wl *= out;
        wr *= out;
        (
            equal_power_mix(left, wl, mix),
            equal_power_mix(right, wr, mix),
        )
    }
}

// ---------------------------------------------------------------------------
// Metal Core — two-stage scooped metal distortion (8×)
// ---------------------------------------------------------------------------

/// True two-stage topology: asymmetric body stage → interstage bass cut +
/// presence (inside the 8× domain) → harder symmetric stage with a fast-knee
/// ceiling → scoop/resonance/fizz post section. Subtle sag only — tightness
/// wins over pump.
#[derive(Debug, Clone)]
pub(super) struct MetalCore {
    sample_rate: f32,
    dc: DcBlock,
    dc_out: DcBlock,
    input_hpf: StereoBiquad,
    tighten: StereoBiquad,
    // Oversampled-domain filters (configured at 8× rate).
    inter_hpf: StereoBiquad,
    inter_presence: StereoBiquad,
    // Base-rate post section.
    scoop: StereoBiquad,
    resonance: StereoBiquad,
    fizz_lpf: StereoBiquad,
    os: Oversampler8x,
    env: EnvFollower,
    od: OdBoost,
    pre_gain: Smoothed,
    inter_gain: Smoothed,
    ceiling: Smoothed,
    bias_depth: Smoothed,
    out_gain: Smoothed,
    mix: Smoothed,
}

impl MetalCore {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            dc: DcBlock::new(sr),
            dc_out: DcBlock::new(sr),
            input_hpf: StereoBiquad::none(),
            tighten: StereoBiquad::none(),
            inter_hpf: StereoBiquad::none(),
            inter_presence: StereoBiquad::none(),
            scoop: StereoBiquad::none(),
            resonance: StereoBiquad::none(),
            fizz_lpf: StereoBiquad::none(),
            os: Oversampler8x::new(),
            env: EnvFollower::new(sr, 0.002, 0.060),
            od: OdBoost::new(sr),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            inter_gain: Smoothed::new(sr, SMOOTH_SECONDS, 2.0),
            ceiling: Smoothed::new(sr, SMOOTH_SECONDS, 0.85),
            bias_depth: Smoothed::new(sr, SMOOTH_SECONDS, 0.1),
            out_gain: Smoothed::new(sr, SMOOTH_SECONDS, 0.4),
            mix: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.dc.set_sample_rate(self.sample_rate);
        self.dc_out.set_sample_rate(self.sample_rate);
        self.env.set_sample_rate(self.sample_rate);
        self.od.set_sample_rate(self.sample_rate);
        for s in [
            &mut self.pre_gain,
            &mut self.inter_gain,
            &mut self.ceiling,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.set_time(self.sample_rate, SMOOTH_SECONDS);
        }
    }

    pub(super) fn reset(&mut self) {
        self.dc.reset();
        self.dc_out.reset();
        self.input_hpf.reset();
        self.tighten.reset();
        self.inter_hpf.reset();
        self.inter_presence.reset();
        self.scoop.reset();
        self.resonance.reset();
        self.fizz_lpf.reset();
        self.os.reset();
        self.env.reset();
        self.od.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.inter_gain,
            &mut self.ceiling,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Tone rebalances scoop depth ↔ presence ↔ fizz ceiling in one gesture:
    /// low = cavernous scooped wall, high = tighter, more forward.
    pub(super) fn configure(&mut self, gain: f32, tone: f32, level: f32) {
        let d = drive_curve(gain / 10.0);
        let t = (tone / 10.0).clamp(0.0, 1.0);
        let lvl = (level / 10.0).clamp(0.0, 1.0);
        let sr = self.sample_rate;
        let osr = sr * 8.0; // interstage filters live in the 8× domain

        // Gain is split across two stages rather than one 60× wall, but both
        // ends are hotter: wide open, stage 2 is fed a hard square.
        let stage1_db = 10.0 + d * 27.0; // 10..37 dB
        let stage2_db = 6.0 + d * 21.0; // 6..27 dB more
        self.pre_gain.set_target(db_to_linear(stage1_db));
        self.inter_gain.set_target(db_to_linear(stage2_db));
        // The ceiling is this model's level reference — it must not move with
        // the knob, or the wall gets quieter exactly as it gets meaner. Even
        // harmonics come from the duty-cycle bias instead (see `duty_bias`),
        // which is the only asymmetry a fully-engaged clipper keeps.
        self.ceiling.set_target(0.82);
        self.bias_depth.set_target(0.06 + d * 0.22);
        self.out_gain
            .set_target(drive_makeup(d) * (0.45 + lvl * 1.45));
        self.mix.set_target(1.0);
        // Overdrive stacked in front: mid-focused grit that tightens the lows
        // and thickens the body before the metal clipper — kills the fizz.
        self.od.configure(d, 750.0, sr);

        self.dc.set_sample_rate(sr);
        self.dc_out.set_sample_rate(sr);
        self.input_hpf
            .set(make_eq_coefficients("highpass", 65.0, 0.0, 0.707, sr));
        // Pre-clip low-shelf keeps bass out of the saturation; deepens with drive.
        self.tighten.set(make_eq_coefficients(
            "lowshelf",
            150.0,
            -(2.0 + d * 5.0),
            0.707,
            sr,
        ));
        // Interstage: strip low-mud, push presence into stage 2 (8× rate!).
        self.inter_hpf
            .set(make_eq_coefficients("highpass", 160.0, 0.0, 0.707, osr));
        self.inter_presence.set(make_eq_coefficients(
            "bell",
            2_200.0,
            3.0 + t * 2.0,
            0.9,
            osr,
        ));
        // Post: the metal V. Tone trades scoop depth for presence and air.
        self.scoop.set(make_eq_coefficients(
            "bell",
            680.0,
            -(9.0 - t * 5.0),
            0.9,
            sr,
        ));
        self.resonance.set(make_eq_coefficients(
            "bell",
            3_100.0,
            1.5 + t * 1.5,
            1.2,
            sr,
        ));
        let fizz = (7_000.0 + t * 4_000.0).min(sr * 0.45); // 7..11 kHz
        self.fizz_lpf
            .set(make_eq_coefficients("lowpass", fizz, 0.0, 0.707, sr));
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let pre = self.pre_gain.tick();
        let inter = self.inter_gain.tick();
        let ceiling = self.ceiling.tick().max(0.2);
        let depth = self.bias_depth.tick();
        let out = self.out_gain.tick();
        let mix = self.mix.tick();
        let od_drive = self.od.tick_drive();

        let (l, r) = self.dc.run(left, right);
        let (l, r) = self.input_hpf.run(l, r);
        let (l, r) = self.tighten.run(l, r);

        // Detectors read the signal *before* the voicing bells. The OD mid push
        // is up to +10 dB at its centre, so metering after it would make sag
        // and duty bias swing with the pitch of the note rather than how hard
        // it was played (measured: 12 dB of h2 between a 375 and a 750 Hz tone).
        // Subtle sag only: enough to feel alive, never enough to pump — and
        // detected pre-gain, so it never claws back what Drive just added.
        let (el, er) = self.env.tick(l, r);
        let sag_l = sag_supply(el, 0.22, 0.7);
        let sag_r = sag_supply(er, 0.22, 0.7);
        let bias_l = duty_bias(el, depth);
        let bias_r = duty_bias(er, depth);

        // Overdrive voicing EQ (base rate) ahead of the clipper.
        let (l, r) = self.od.pre_eq(l, r);

        let inter_hpf = &mut self.inter_hpf;
        let inter_presence = &mut self.inter_presence;
        // The OD clip runs at near-unity level *inside* the oversampler, then
        // the model's pre-gain slams its output into the two clip stages.
        let (mut wl, mut wr) = self.os.process_stereo(l * sag_l, r * sag_r, |a, b| {
            // Overdrive stacked in front: boost + grit + tighten. The bias
            // goes in ahead of the first clipper so the pulse-width asymmetry
            // it creates is carried by every stage after it.
            let a = od_clip(a + bias_l, od_drive) * pre;
            let b = od_clip(b + bias_r, od_drive) * pre;
            // Stage 1: asymmetric body — keeps transient shape.
            let a1 = soft_asym(a, 1.2, 0.78);
            let b1 = soft_asym(b, 1.2, 0.78);
            // Interstage EQ at 8×: no bass into stage 2, presence in.
            let (a2, b2) = inter_hpf.run(a1, b1);
            let (a3, b3) = inter_presence.run(a2, b2);
            // Stage 2: harder, symmetric, near-bare clamp — the wall.
            let a4 = hard_clip_asym((a3 * inter).tanh() * 1.4, ceiling, ceiling, 0.03);
            let b4 = hard_clip_asym((b3 * inter).tanh() * 1.4, ceiling, ceiling, 0.03);
            (a4, b4)
        });
        (wl, wr) = self.scoop.run(wl, wr);
        (wl, wr) = self.resonance.run(wl, wr);
        (wl, wr) = self.fizz_lpf.run(wl, wr);
        // Asymmetric clipping generates DC — strip it before it eats
        // headroom or thumps the mix.
        (wl, wr) = self.dc_out.run(wl, wr);
        wl = sanitize(wl) * out;
        wr = sanitize(wr) * out;
        (
            equal_power_mix(left, wl, mix),
            equal_power_mix(right, wr, mix),
        )
    }
}

// ---------------------------------------------------------------------------
// Tight Rift — transient-aware modern high-gain (8×)
// ---------------------------------------------------------------------------

/// Djent-style tightness without the old fixed 220 Hz body-ectomy: a gentle
/// real high-pass plus a drive-dependent low shelf, a fast-recovery envelope
/// (detector is high-passed so lows never pump the gain), and a parallel
/// transient path that sharpens pick attack.
#[derive(Debug, Clone)]
pub(super) struct TightRift {
    sample_rate: f32,
    dc: DcBlock,
    dc_out: DcBlock,
    input_hpf: StereoBiquad,
    tighten_shelf: StereoBiquad,
    // Transient path.
    trans_hpf: StereoBiquad,
    // Oversampled-domain interstage (8× rate).
    inter_hpf: StereoBiquad,
    // Post section.
    definition: StereoBiquad,
    pick_bite: StereoBiquad,
    fizz_lpf: StereoBiquad,
    os: Oversampler8x,
    /// Sag detector — high-passed, so lows never pump the gain.
    env: EnvFollower,
    /// Bias detector — full band, because duty-cycle asymmetry has to track
    /// the note being played, not just its pick attack.
    bias_env: EnvFollower,
    od: OdBoost,
    pre_gain: Smoothed,
    ceiling: Smoothed,
    trans_amt: Smoothed,
    bias_depth: Smoothed,
    out_gain: Smoothed,
    mix: Smoothed,
}

impl TightRift {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            dc: DcBlock::new(sr),
            dc_out: DcBlock::new(sr),
            input_hpf: StereoBiquad::none(),
            tighten_shelf: StereoBiquad::none(),
            trans_hpf: StereoBiquad::none(),
            inter_hpf: StereoBiquad::none(),
            definition: StereoBiquad::none(),
            pick_bite: StereoBiquad::none(),
            fizz_lpf: StereoBiquad::none(),
            os: Oversampler8x::new(),
            env: EnvFollower::new(sr, 0.001, 0.030),
            bias_env: EnvFollower::new(sr, 0.002, 0.055),
            od: OdBoost::new(sr),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            ceiling: Smoothed::new(sr, SMOOTH_SECONDS, 0.85),
            trans_amt: Smoothed::new(sr, SMOOTH_SECONDS, 0.12),
            bias_depth: Smoothed::new(sr, SMOOTH_SECONDS, 0.08),
            out_gain: Smoothed::new(sr, SMOOTH_SECONDS, 0.4),
            mix: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.dc.set_sample_rate(self.sample_rate);
        self.dc_out.set_sample_rate(self.sample_rate);
        self.env.set_sample_rate(self.sample_rate);
        self.bias_env.set_sample_rate(self.sample_rate);
        self.od.set_sample_rate(self.sample_rate);
        for s in [
            &mut self.pre_gain,
            &mut self.ceiling,
            &mut self.trans_amt,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.set_time(self.sample_rate, SMOOTH_SECONDS);
        }
    }

    pub(super) fn reset(&mut self) {
        self.dc.reset();
        self.dc_out.reset();
        self.input_hpf.reset();
        self.tighten_shelf.reset();
        self.trans_hpf.reset();
        self.inter_hpf.reset();
        self.definition.reset();
        self.pick_bite.reset();
        self.fizz_lpf.reset();
        self.os.reset();
        self.env.reset();
        self.bias_env.reset();
        self.od.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.ceiling,
            &mut self.trans_amt,
            &mut self.bias_depth,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Tone rebalances definition ↔ pick bite ↔ high damping.
    pub(super) fn configure(&mut self, gain: f32, tone: f32, level: f32) {
        let d = drive_curve(gain / 10.0);
        let t = (tone / 10.0).clamp(0.0, 1.0);
        let lvl = (level / 10.0).clamp(0.0, 1.0);
        let sr = self.sample_rate;
        let osr = sr * 8.0;

        let gain_db = 14.0 + d * 29.0; // 14..43 dB
        self.pre_gain.set_target(db_to_linear(gain_db));
        // Fixed ceiling: this model's level reference, not a drive-dependent
        // one (see `drive_makeup`).
        self.ceiling.set_target(0.80);
        self.trans_amt.set_target(0.08 + d * 0.14);
        // Tight means controlled, not sterile: less duty-cycle bias than the
        // Metal Core wall, but enough that the top of the knob keeps some even
        // harmonic weight instead of collapsing to a bare odd-harmonic square.
        self.bias_depth.set_target(0.06 + d * 0.20);
        self.out_gain
            .set_target(drive_makeup(d) * (0.28 + lvl * 0.82));
        self.mix.set_target(1.0);
        // Overdrive stacked in front: mid focus + tight lows before the tight
        // high-gain clipper, so the tone thickens instead of thinning to fizz.
        self.od.configure(d, 800.0, sr);

        self.dc.set_sample_rate(sr);
        self.dc_out.set_sample_rate(sr);
        // Real body-preserving high-pass; tightening is the drive-dependent
        // shelf, not a fixed 220 Hz wall.
        self.input_hpf
            .set(make_eq_coefficients("highpass", 70.0, 0.0, 0.707, sr));
        let shelf_hz = 150.0 + d * 70.0; // 150..220, only at full drive
        self.tighten_shelf.set(make_eq_coefficients(
            "lowshelf",
            shelf_hz,
            -(1.5 + d * 8.5),
            0.707,
            sr,
        ));
        self.trans_hpf
            .set(make_eq_coefficients("highpass", 1_200.0, 0.0, 0.707, sr));
        self.inter_hpf
            .set(make_eq_coefficients("highpass", 200.0, 0.0, 0.707, osr));
        self.definition.set(make_eq_coefficients(
            "bell",
            1_900.0,
            2.0 + t * 3.0,
            0.9,
            sr,
        ));
        self.pick_bite.set(make_eq_coefficients(
            "bell",
            3_800.0,
            1.5 + t * 2.5,
            1.1,
            sr,
        ));
        let fizz = (8_000.0 + t * 4_000.0).min(sr * 0.45); // 8..12 kHz
        self.fizz_lpf
            .set(make_eq_coefficients("lowpass", fizz, 0.0, 0.707, sr));
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let pre = self.pre_gain.tick();
        let ceiling = self.ceiling.tick().max(0.2);
        let trans_amt = self.trans_amt.tick();
        let depth = self.bias_depth.tick();
        let out = self.out_gain.tick();
        let mix = self.mix.tick();
        let od_drive = self.od.tick_drive();

        let (l, r) = self.dc.run(left, right);
        let (l, r) = self.input_hpf.run(l, r);
        // Transient path taps the un-tightened signal for pick attack.
        let (tl, tr) = self.trans_hpf.run(l, r);
        let (l, r) = self.tighten_shelf.run(l, r);

        // Fast-recovery sag on a high-passed, pre-gain detector: palm mutes
        // recover instantly, lows never pump the gain, and the Drive knob is
        // not silently undone by its own envelope. The bias detector is full
        // band but still reads the signal before the OD's mid push, so the duty
        // shift tracks playing dynamics rather than the pitch of the note.
        let (el, er) = self.env.tick(tl, tr);
        let sag_l = sag_supply(el, 0.18, 0.75);
        let sag_r = sag_supply(er, 0.18, 0.75);
        let (bl, br) = self.bias_env.tick(l, r);
        let bias_l = duty_bias(bl, depth);
        let bias_r = duty_bias(br, depth);

        // Overdrive voicing EQ (base rate) ahead of the clipper.
        let (l, r) = self.od.pre_eq(l, r);

        let inter_hpf = &mut self.inter_hpf;
        // OD clip at near-unity level inside the oversampler, then pre-gain
        // drives its output into the tight clipper stages.
        let (mut wl, mut wr) = self.os.process_stereo(l * sag_l, r * sag_r, |a, b| {
            // Overdrive stacked in front: boost + grit + tighten, biased ahead
            // of the first clipper so the duty-cycle asymmetry carries through.
            let a = od_clip(a + bias_l, od_drive) * pre;
            let b = od_clip(b + bias_r, od_drive) * pre;
            // Stage 1: light asym for body...
            let a1 = soft_asym(a, 1.15, 0.83);
            let b1 = soft_asym(b, 1.15, 0.83);
            // ...then keep stage 2 tight...
            let (a2, b2) = inter_hpf.run(a1, b1);
            // ...into a near-bare clamp with a hard ceiling.
            let a3 = hard_clip_asym((a2 * 3.2).tanh() * 1.3, ceiling, ceiling, 0.025);
            let b3 = hard_clip_asym((b2 * 3.2).tanh() * 1.3, ceiling, ceiling, 0.025);
            (a3, b3)
        });
        // Subtle transient recombination: sharpened attack, not a click layer.
        wl += soft_asym(tl * 2.0, 1.0, 1.0) * trans_amt;
        wr += soft_asym(tr * 2.0, 1.0, 1.0) * trans_amt;
        (wl, wr) = self.definition.run(wl, wr);
        (wl, wr) = self.pick_bite.run(wl, wr);
        (wl, wr) = self.fizz_lpf.run(wl, wr);
        // Asymmetric clipping generates DC — strip it before it eats
        // headroom or thumps the mix.
        (wl, wr) = self.dc_out.run(wl, wr);
        wl = sanitize(wl) * out;
        wr = sanitize(wr) * out;
        (
            equal_power_mix(left, wl, mix),
            equal_power_mix(right, wr, mix),
        )
    }
}
