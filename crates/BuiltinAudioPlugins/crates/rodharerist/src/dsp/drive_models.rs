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
//!
//! Every clipping stage is a [`DiodeClipper`] node solved per sample, never a
//! transfer curve. Asymmetry is an unequal diode pair rather than an envelope
//! offset injected ahead of a symmetric shaper, and each node's capacitor gives
//! the stage memory, so the break-up rounds and moves with playing instead of
//! folding at a fixed level the way a static clipper does.

use builtin_dsp_core::{db_to_linear, make_eq_coefficients, time_constant};

use super::StereoBiquad;
use super::nonlinear::{DiodeClipper, DiodeParams};
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
    clipper: DiodeClipper,
    drive: Smoothed,
}

impl OdBoost {
    pub(super) fn new(sample_rate: f32) -> Self {
        Self {
            tighten_hpf: StereoBiquad::none(),
            mid_push: StereoBiquad::none(),
            clipper: DiodeClipper::new(Self::diodes(sample_rate)),
            drive: Smoothed::new(sample_rate.max(1.0), SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.drive.set_time(sample_rate.max(1.0), SMOOTH_SECONDS);
    }

    pub(super) fn reset(&mut self) {
        self.tighten_hpf.reset();
        self.mid_push.reset();
        self.clipper.reset();
        self.drive.snap();
    }

    /// A soft germanium pair: plenty of grit, no corner. The node runs at the
    /// host model's oversampled rate, so `node_rate` is the caller's rate times
    /// its oversampling factor.
    fn diodes(node_rate: f32) -> DiodeParams {
        DiodeParams::new(
            1.0,
            1.04,
            0.20,
            0.19,
            1.0e-6,
            1.3e-6,
            18_000.0,
            node_rate.max(1.0),
        )
    }

    /// `amount` (0..1, typically the drive curve) scales both the soft-clip
    /// hardness and the mid push; `mid_hz` voices the honk that cuts through.
    /// `oversample` is the host model's factor, so the clipping node's own
    /// capacitor is tuned to the rate it actually runs at.
    pub(super) fn configure(
        &mut self,
        amount: f32,
        mid_hz: f32,
        sample_rate: f32,
        oversample: f32,
    ) {
        let a = amount.clamp(0.0, 1.0);
        self.clipper
            .set_params(Self::diodes(sample_rate.max(1.0) * oversample));
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

    /// Solve the boost's clipping node. Call inside the host model's
    /// oversampler; the fixed ceiling keeps the stage a grit/voicing stage and
    /// leaves raw level to the model's own pre-gain.
    #[inline]
    pub(super) fn clip(&mut self, l: f32, r: f32, drive: f32) -> (f32, f32) {
        let d = drive.max(1.0);
        let (a, b) = self.clipper.process(l * d, r * d);
        (a * OD_CEILING, b * OD_CEILING)
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
    clipper: DiodeClipper,
    pre_gain: Smoothed,
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
            clipper: DiodeClipper::new(Self::diodes(sr)),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            out_gain: Smoothed::new(sr, SMOOTH_SECONDS, 0.5),
            mix: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.dc.set_sample_rate(self.sample_rate);
        self.dc_out.set_sample_rate(self.sample_rate);
        self.clipper.set_params(Self::diodes(self.sample_rate));
        for s in [&mut self.pre_gain, &mut self.out_gain, &mut self.mix] {
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
        self.clipper.reset();
        for s in [&mut self.pre_gain, &mut self.out_gain, &mut self.mix] {
            s.snap();
        }
    }

    /// Silicon pair to ground, two diodes up against one down — the orange
    /// box's actual asymmetry, which is where its rude even-harmonic bite
    /// comes from. Voltages are normalized so a nominal drop is 1.0.
    fn diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            0.84,
            1.16,
            0.048,
            0.046,
            2.5e-7,
            5.5e-7,
            7_000.0,
            sample_rate.max(1.0) * 4.0,
        )
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
        // The diodes set the ceiling and the asymmetry; Drive only decides how
        // hard they are hit. No inverse compensation on the output — see
        // `drive_makeup`.
        self.out_gain
            .set_target(drive_makeup(d) * (0.18 + lvl * 0.57));
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
        let out = self.out_gain.tick();
        let mix = self.mix.tick();

        let (l, r) = self.dc.run(left, right);
        let (l, r) = self.input_hpf.run(l, r);
        let (l, r) = self.pre_emph.run(l, r);
        let clipper = &mut self.clipper;
        let (mut wl, mut wr) = self
            .os
            .process_stereo(l * pre, r * pre, |a, b| clipper.process(a, b));
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
    clipper: DiodeClipper,
    env: EnvFollower,
    pre_gain: Smoothed,
    sag_amount: Smoothed,
    low_blend: Smoothed,
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
            clipper: DiodeClipper::new(Self::diodes(sr)),
            env: EnvFollower::new(sr, 0.005, 0.090),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            sag_amount: Smoothed::new(sr, SMOOTH_SECONDS, 0.0),
            low_blend: Smoothed::new(sr, SMOOTH_SECONDS, 0.18),
            out_gain: Smoothed::new(sr, SMOOTH_SECONDS, 0.6),
            mix: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.dc.set_sample_rate(self.sample_rate);
        self.dc_out.set_sample_rate(self.sample_rate);
        self.env.set_sample_rate(self.sample_rate);
        self.clipper.set_params(Self::diodes(self.sample_rate));
        for s in [
            &mut self.pre_gain,
            &mut self.sag_amount,
            &mut self.low_blend,
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
        self.clipper.reset();
        self.env.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.sag_amount,
            &mut self.low_blend,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Two soft diodes one way against one the other, in the feedback loop of
    /// an overdrive: a gentle knee and a real, static asymmetry. The touch
    /// sensitivity this model is built around comes from the supply sag and
    /// from the node's own capacitor, not from an envelope steering the shape
    /// of a symmetric curve.
    fn diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            0.80,
            1.20,
            0.115,
            0.105,
            6.0e-7,
            1.1e-6,
            13_000.0,
            sample_rate.max(1.0) * 4.0,
        )
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
        self.out_gain
            .set_target(drive_makeup(d) * (0.18 + lvl * 0.54));
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
        let out = self.out_gain.tick();
        let mix = self.mix.tick();

        let (l, r) = self.dc.run(left, right);
        let (low_l, low_r) = self.low_keep.run(l, r);
        let (l, r) = self.input_hpf.run(l, r);

        // The supply sags with playing: dig in and the rail feeding the gain
        // stage droops, so the diodes are reached later and the pedal
        // compresses. Metered before the mid hump, so dynamics move it and
        // pitch does not.
        let (el, er) = self.env.tick(l, r);
        let sag_l = sag_supply(el, sag, 0.55);
        let sag_r = sag_supply(er, sag, 0.55);

        let (l, r) = self.mid_hump.run(l, r);
        let clipper = &mut self.clipper;
        let (mut wl, mut wr) = self
            .os
            .process_stereo(l * pre * sag_l, r * pre * sag_r, |a, b| {
                clipper.process(a, b)
            });
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
    body: DiodeClipper,
    wall: DiodeClipper,
    env: EnvFollower,
    od: OdBoost,
    pre_gain: Smoothed,
    inter_gain: Smoothed,
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
            body: DiodeClipper::new(Self::body_diodes(sr)),
            wall: DiodeClipper::new(Self::wall_diodes(sr)),
            env: EnvFollower::new(sr, 0.002, 0.060),
            od: OdBoost::new(sr),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            inter_gain: Smoothed::new(sr, SMOOTH_SECONDS, 2.0),
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
        self.body.set_params(Self::body_diodes(self.sample_rate));
        self.wall.set_params(Self::wall_diodes(self.sample_rate));
        for s in [
            &mut self.pre_gain,
            &mut self.inter_gain,
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
        self.body.reset();
        self.wall.reset();
        self.env.reset();
        self.od.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.inter_gain,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Stage 1: an unequal soft pair that keeps transient shape and supplies
    /// the even harmonics. A real component mismatch, so the asymmetry no
    /// longer has to be faked with an envelope-driven offset at the input.
    fn body_diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            0.82,
            1.18,
            0.120,
            0.110,
            3.0e-7,
            5.5e-7,
            15_000.0,
            sample_rate.max(1.0) * 8.0,
        )
    }

    /// Stage 2: a matched pair with a very tight knee, which is the wall. Its
    /// capacitor is what keeps that wall from cornering into digital fizz.
    fn wall_diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            1.0,
            0.98,
            0.030,
            0.029,
            1.5e-8,
            2.2e-8,
            6_000.0,
            sample_rate.max(1.0) * 8.0,
        )
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
        // The diodes are this model's level reference and they do not move with
        // the knob, so the wall can never get quieter as it gets meaner.
        self.out_gain
            .set_target(drive_makeup(d) * (0.37 + lvl * 1.19));
        self.mix.set_target(1.0);
        // Overdrive stacked in front: mid-focused grit that tightens the lows
        // and thickens the body before the metal clipper — kills the fizz.
        self.od.configure(d, 750.0, sr, 8.0);

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
        let out = self.out_gain.tick();
        let mix = self.mix.tick();
        let od_drive = self.od.tick_drive();

        let (l, r) = self.dc.run(left, right);
        let (l, r) = self.input_hpf.run(l, r);
        let (l, r) = self.tighten.run(l, r);

        // The sag detector reads the signal *before* the voicing bells. The OD
        // mid push is up to +10 dB at its centre, so metering after it would
        // make sag swing with the pitch of the note rather than how hard it was
        // played. Subtle sag only: enough to feel alive, never enough to pump —
        // and detected pre-gain, so it never claws back what Drive just added.
        let (el, er) = self.env.tick(l, r);
        let sag_l = sag_supply(el, 0.22, 0.7);
        let sag_r = sag_supply(er, 0.22, 0.7);

        // Overdrive voicing EQ (base rate) ahead of the clipper.
        let (l, r) = self.od.pre_eq(l, r);

        let inter_hpf = &mut self.inter_hpf;
        let inter_presence = &mut self.inter_presence;
        let od = &mut self.od;
        let body = &mut self.body;
        let wall = &mut self.wall;
        // The OD node runs at near-unity level *inside* the oversampler, then
        // the model's pre-gain slams its output into the two clip stages.
        let (mut wl, mut wr) = self.os.process_stereo(l * sag_l, r * sag_r, |a, b| {
            // Overdrive stacked in front: boost + grit + tighten.
            let (a, b) = od.clip(a, b, od_drive);
            // Stage 1: asymmetric body — keeps transient shape.
            let (a1, b1) = body.process(a * pre, b * pre);
            // Interstage EQ at 8x: no bass into stage 2, presence in.
            let (a2, b2) = inter_hpf.run(a1, b1);
            let (a3, b3) = inter_presence.run(a2, b2);
            // Stage 2: harder, matched, tight-kneed. The wall.
            wall.process(a3 * inter, b3 * inter)
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
    body: DiodeClipper,
    wall: DiodeClipper,
    /// The transient path gets its own soft node so the sharpened attack is
    /// limited by a circuit rather than clamped by a curve.
    transient_clip: DiodeClipper,
    /// Sag detector — high-passed, so lows never pump the gain.
    env: EnvFollower,
    od: OdBoost,
    pre_gain: Smoothed,
    trans_amt: Smoothed,
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
            body: DiodeClipper::new(Self::body_diodes(sr)),
            wall: DiodeClipper::new(Self::wall_diodes(sr)),
            transient_clip: DiodeClipper::new(Self::transient_diodes(sr)),
            env: EnvFollower::new(sr, 0.001, 0.030),
            od: OdBoost::new(sr),
            pre_gain: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            trans_amt: Smoothed::new(sr, SMOOTH_SECONDS, 0.12),
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
        self.body.set_params(Self::body_diodes(self.sample_rate));
        self.wall.set_params(Self::wall_diodes(self.sample_rate));
        self.transient_clip
            .set_params(Self::transient_diodes(self.sample_rate));
        for s in [
            &mut self.pre_gain,
            &mut self.trans_amt,
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
        self.body.reset();
        self.wall.reset();
        self.transient_clip.reset();
        self.env.reset();
        self.od.reset();
        for s in [
            &mut self.pre_gain,
            &mut self.trans_amt,
            &mut self.out_gain,
            &mut self.mix,
        ] {
            s.snap();
        }
    }

    /// Stage 1: a lightly unequal soft pair for body without smearing the
    /// attack. The mismatch is the asymmetry, so no envelope has to fake one.
    fn body_diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            0.87,
            1.13,
            0.130,
            0.125,
            1.8e-7,
            3.0e-7,
            17_000.0,
            sample_rate.max(1.0) * 8.0,
        )
    }

    /// Stage 2: matched, very tight knee, small capacitor. Tight and modern,
    /// but still a node with memory rather than a clamp.
    fn wall_diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            1.0,
            0.98,
            0.028,
            0.027,
            1.0e-8,
            1.4e-8,
            6_500.0,
            sample_rate.max(1.0) * 8.0,
        )
    }

    /// The parallel transient path runs at base rate and only needs a ceiling
    /// so the sharpened attack cannot become a click layer.
    fn transient_diodes(sample_rate: f32) -> DiodeParams {
        DiodeParams::new(
            1.0,
            1.0,
            0.150,
            0.150,
            2.0e-8,
            2.0e-8,
            20_000.0,
            sample_rate.max(1.0),
        )
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
        self.trans_amt.set_target(0.08 + d * 0.14);
        // The diodes are this model's level reference, not a drive-dependent
        // ceiling (see `drive_makeup`).
        self.out_gain
            .set_target(drive_makeup(d) * (0.22 + lvl * 0.66));
        self.mix.set_target(1.0);
        // Overdrive stacked in front: mid focus + tight lows before the tight
        // high-gain clipper, so the tone thickens instead of thinning to fizz.
        self.od.configure(d, 800.0, sr, 8.0);

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
        let trans_amt = self.trans_amt.tick();
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
        // not silently undone by its own envelope.
        let (el, er) = self.env.tick(tl, tr);
        let sag_l = sag_supply(el, 0.18, 0.75);
        let sag_r = sag_supply(er, 0.18, 0.75);

        // Overdrive voicing EQ (base rate) ahead of the clipper.
        let (l, r) = self.od.pre_eq(l, r);

        let inter_hpf = &mut self.inter_hpf;
        let od = &mut self.od;
        let body = &mut self.body;
        let wall = &mut self.wall;
        // OD node at near-unity level inside the oversampler, then pre-gain
        // drives its output into the tight clipper stages.
        let (mut wl, mut wr) = self.os.process_stereo(l * sag_l, r * sag_r, |a, b| {
            // Overdrive stacked in front: boost + grit + tighten.
            let (a, b) = od.clip(a, b, od_drive);
            // Stage 1: light asymmetry for body...
            let (a1, b1) = body.process(a * pre, b * pre);
            // ...then keep stage 2 tight...
            let (a2, b2) = inter_hpf.run(a1, b1);
            // ...into the tight-kneed node that sets the wall.
            wall.process(a2 * 3.2, b2 * 3.2)
        });
        // Subtle transient recombination: sharpened attack, not a click layer.
        let (trans_l, trans_r) = self.transient_clip.process(tl * 2.0, tr * 2.0);
        wl += trans_l * trans_amt;
        wr += trans_r * trans_amt;
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
