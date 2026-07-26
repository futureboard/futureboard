//! Swept-allpass stereo phasers — one engine, several voiced circuits.
//!
//! A phaser is not the allpass cascade on its own. The cascade has a flat
//! magnitude response by construction, so what you hear is the *sum* of the
//! dry path and the cascade: where the two arrive in antiphase the sum
//! cancels, and those moving nulls are the effect. Feeding back around the
//! cascade lifts the peaks between the nulls, which is the resonant "vowel"
//! character of an OTA pedal.
//!
//! Two consequences drive everything below:
//!
//! * The blend has a maximum. Notch depth peaks at an equal dry/wet sum and
//!   falls away on either side, so a Mix control has to reach 50% wet and
//!   stop. Running it to 100% wet returns the bare cascade — flat, and very
//!   nearly inaudible.
//! * Notch *count* is the stage count. Every two allpass stages buys one
//!   null, so a four-stage circuit has two and a twelve-stage has six. That,
//!   not depth or rate, is what separates a discreet Phase 90 from the
//!   swirling multi-notch boxes.
//!
//! Voices differ in stage count, regeneration, sweep span, whether the stages
//! share a corner or are staggered like a Univibe's unequal capacitors, LFO
//! symmetry and stereo spread. All state is fixed-size; nothing here
//! allocates or branches on unbounded data.

use builtin_dsp_core::mix;

use super::Lfo;
use super::smooth::Smoothed;

/// Glide time for depth/mix edits (see `smooth.rs`).
const SMOOTH_SECONDS: f32 = 0.010;

/// Largest cascade any voice uses (six notches).
const MAX_STAGES: usize = 12;

/// Deepest useful wet blend: an equal dry/wet sum, where the nulls are total.
const MAX_BLEND: f32 = 0.5;

/// Where the resonant peak is allowed to land, as a linear gain (+1 dB).
///
/// Regeneration does not only sharpen the peaks between the nulls, it raises
/// them: at the frequency where the loop comes back in phase the cascade sees
/// a gain of `1 / (1 - feedback)`, so a voice sitting at 0.8 handed the rest
/// of the chain a 9.5 dB lift. Into an already-driven amp that is not
/// "resonant", it is clipping. A pedal's output stage does not do this, and
/// neither should these — a slight lift at the peak, never a shove.
const PEAK_TARGET: f32 = 1.122;

#[inline]
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-8.0, 8.0)
    } else {
        0.0
    }
}

/// Which voiced phaser circuit the Mod slot is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PhaserVoice {
    /// "Vibe Phase 90" — the original four-stage script pedal.
    Phase90,
    /// "Molam Swirl" — four stages with the regeneration wide open, for the
    /// big vowel-like sweep that carries an Isan lead line.
    MolamSwirl,
    /// "Phin Vibe" — staggered stages and no feedback at all: a throb rather
    /// than a notch sweep, sitting under a phin-style melody.
    PhinVibe,
    /// "Khaen Swirl" — eight stages, four nulls, lush and continuous.
    KhaenSwirl,
    /// "Bi-Lam" — twelve stages in one cascade, slow and very wide.
    BiLam,
    /// "Isan Jet" — six stages, hard regeneration, fast and narrow up top.
    IsanJet,
}

#[derive(Debug, Clone, Copy)]
struct Profile {
    stages: usize,
    /// Regeneration around the cascade. Approaching 1.0 rings.
    feedback: f32,
    sweep_lo: f32,
    sweep_hi: f32,
    /// Ratio between adjacent stage corners. 1.0 puts every stage on the same
    /// corner (a true notch phaser); above that the stages spread out the way
    /// a Univibe's four unequal capacitors do, which trades the sharp nulls
    /// for a broad wobble.
    stagger: f32,
    /// LFO shape exponent. 1.0 is a plain sine; above it the sweep dwells at
    /// the bottom and snaps back, which is the lopsided Univibe throb.
    skew: f32,
    /// Right-channel LFO offset in cycles. 0.5 is fully counter-phase.
    spread: f32,
    /// Multiplies the rate knob, so a "fast" voice reaches further.
    rate_scale: f32,
    /// Ceiling on the wet blend. Only the vibe voice sits below a full sum.
    max_blend: f32,
}

impl PhaserVoice {
    fn profile(self) -> Profile {
        match self {
            // Two notches, moderate regeneration: present without swamping
            // the note. The original build ran this at 0.35 feedback and a
            // blend that reached full wet, which is why it read as a faint
            // wobble instead of a phaser.
            Self::Phase90 => Profile {
                stages: 4,
                feedback: 0.50,
                sweep_lo: 220.0,
                sweep_hi: 2_600.0,
                stagger: 1.0,
                skew: 1.0,
                spread: 0.25,
                rate_scale: 1.0,
                max_blend: MAX_BLEND,
            },
            Self::MolamSwirl => Profile {
                stages: 4,
                feedback: 0.62,
                sweep_lo: 190.0,
                sweep_hi: 2_400.0,
                stagger: 1.0,
                skew: 1.0,
                spread: 0.25,
                rate_scale: 0.85,
                max_blend: MAX_BLEND,
            },
            // No feedback and widely staggered corners: the nulls never line
            // up, so nothing "swooshes" — it pulses.
            Self::PhinVibe => Profile {
                stages: 4,
                feedback: 0.0,
                sweep_lo: 320.0,
                sweep_hi: 1_700.0,
                stagger: 1.80,
                skew: 1.40,
                spread: 0.28,
                rate_scale: 1.15,
                max_blend: 0.44,
            },
            Self::KhaenSwirl => Profile {
                stages: 8,
                feedback: 0.52,
                sweep_lo: 230.0,
                sweep_hi: 3_000.0,
                stagger: 1.0,
                skew: 1.0,
                spread: 0.28,
                rate_scale: 0.75,
                max_blend: MAX_BLEND,
            },
            Self::BiLam => Profile {
                stages: 12,
                feedback: 0.38,
                sweep_lo: 210.0,
                sweep_hi: 2_800.0,
                stagger: 1.0,
                skew: 1.0,
                spread: 0.33,
                rate_scale: 0.50,
                max_blend: MAX_BLEND,
            },
            Self::IsanJet => Profile {
                stages: 6,
                feedback: 0.62,
                sweep_lo: 450.0,
                sweep_hi: 4_000.0,
                stagger: 1.0,
                skew: 1.0,
                spread: 0.20,
                rate_scale: 1.70,
                max_blend: MAX_BLEND,
            },
        }
    }
}

/// One channel: allpass state plus the regeneration sample.
#[derive(Debug, Clone)]
struct Channel {
    z: [f32; MAX_STAGES],
    feedback: f32,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            z: [0.0; MAX_STAGES],
            feedback: 0.0,
        }
    }
}

impl Channel {
    fn reset(&mut self) {
        self.z = [0.0; MAX_STAGES];
        self.feedback = 0.0;
    }

    /// Run `stages` allpass sections whose coefficients are already resolved.
    #[inline]
    fn run(&mut self, input: f32, coeffs: &[f32; MAX_STAGES], stages: usize, feedback: f32) -> f32 {
        let mut x = input + self.feedback * feedback;
        for (z, &a) in self.z.iter_mut().zip(coeffs.iter()).take(stages) {
            // First-order allpass H(z) = (a - z^-1) / (1 - a*z^-1), as a
            // transposed direct form:
            //   y[n] = a*x[n] + s ;  s = a*y[n] - x[n]
            //
            // The sign pattern is the whole effect. Written with both signs
            // positive — H(z) = (a + z^-1) / (1 + a*z^-1), which is what this
            // file used to contain — the section is still an allpass, but its
            // quadrature point sits up near Nyquist instead of at the corner
            // the coefficient was solved for. At a = 0.9 that is 0.3 degrees
            // of shift where 90 was intended, so the cascade never reached
            // antiphase, no null ever formed, and the "phaser" was audible
            // only as the broad shelf its own feedback produced.
            let y = a * x + *z;
            *z = a * y - x;
            x = y;
        }
        // The loop is bounded and the coefficients stay inside (-1, 1), but a
        // hard-regenerating cascade still has to be kept from running away
        // across a rate or model change.
        self.feedback = finite(x);
        self.feedback
    }
}

#[derive(Debug, Clone)]
pub(super) struct Phaser {
    sample_rate: f32,
    voice: PhaserVoice,
    profile: Profile,
    /// Per-stage corner multipliers, resolved on the control path.
    stagger: [f32; MAX_STAGES],
    lfo_l: Lfo,
    lfo_r: Lfo,
    left: Channel,
    right: Channel,
    depth: Smoothed,
    blend: Smoothed,
    /// Compensates the regeneration's peak gain. Derived from the blend, so
    /// the effect never adds level at any Mix setting.
    makeup: Smoothed,
    /// Geometric centre of the sweep — depth widens around it rather than
    /// dragging the whole sweep up off the floor.
    centre_hz: f32,
    span: f32,
}

impl Phaser {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let voice = PhaserVoice::Phase90;
        let mut phaser = Self {
            sample_rate: sr,
            voice,
            profile: voice.profile(),
            stagger: [1.0; MAX_STAGES],
            lfo_l: Lfo::new(),
            lfo_r: Lfo::new(),
            left: Channel::default(),
            right: Channel::default(),
            depth: Smoothed::new(sr, SMOOTH_SECONDS, 0.7),
            blend: Smoothed::new(sr, SMOOTH_SECONDS, 0.5),
            makeup: Smoothed::new(sr, SMOOTH_SECONDS, 1.0),
            centre_hz: 800.0,
            span: 1.0,
        };
        phaser.adopt(voice);
        phaser
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        self.depth.set_time(sr, SMOOTH_SECONDS);
        self.blend.set_time(sr, SMOOTH_SECONDS);
        self.makeup.set_time(sr, SMOOTH_SECONDS);
    }

    pub(super) fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.lfo_l.reset();
        self.lfo_r.reset();
        self.lfo_r.set_phase(self.profile.spread);
        self.depth.snap();
        self.blend.snap();
        self.makeup.snap();
    }

    /// Resolve everything about a voice that does not change per sample.
    fn adopt(&mut self, voice: PhaserVoice) {
        self.voice = voice;
        let p = voice.profile();
        self.profile = p;
        // Centre the stagger on the sweep so a staggered voice covers the same
        // band overall as an unstaggered one.
        let mid = (p.stages as f32 - 1.0) * 0.5;
        for (i, slot) in self.stagger.iter_mut().enumerate() {
            *slot = if i < p.stages {
                p.stagger.powf(i as f32 - mid)
            } else {
                1.0
            };
        }
        self.centre_hz = (p.sweep_lo * p.sweep_hi).sqrt();
        self.span = p.sweep_hi / p.sweep_lo;
        self.lfo_r.set_phase(p.spread);
    }

    /// `rate` and `depth` are 0..10; `mix` is 0..100 %.
    pub(super) fn configure(&mut self, voice: PhaserVoice, rate: f32, depth: f32, mix: f32) {
        if voice != self.voice {
            self.adopt(voice);
            // Allpass state and the regeneration sample belong to the old
            // cascade; carrying them into a different stage count is a click.
            self.left.reset();
            self.right.reset();
        }
        // 0.05 Hz → 8 Hz over the knob, biased slow like a pedal, then scaled
        // by the voice.
        let t = (rate / 10.0).clamp(0.0, 1.0);
        let rate_hz = (0.05 + t * t * 7.95) * self.profile.rate_scale;
        self.lfo_l.set_rate(rate_hz, self.sample_rate);
        self.lfo_r.set_rate(rate_hz, self.sample_rate);
        self.depth.set_target((depth / 10.0).clamp(0.0, 1.0));
        // Mix reaches the deepest sum and stops. Past an equal blend the nulls
        // fill back in, so letting the knob run to bare wet would make the
        // effect quietly disappear at the top of its travel.
        let blend = (mix / 100.0).clamp(0.0, 1.0) * self.profile.max_blend;
        self.blend.set_target(blend);
        // Where the loop returns in phase the cascade reaches 1/(1 - feedback),
        // so the summed path peaks at (1 - blend) + blend/(1 - feedback). Undo
        // all but a decibel of that. Never above unity: at Mix 0 the dry signal
        // has to pass through untouched.
        let peak = (1.0 - blend) + blend / (1.0 - self.profile.feedback).max(0.05);
        self.makeup
            .set_target((PEAK_TARGET / peak.max(1.0e-3)).min(1.0));
    }

    /// Allpass coefficient for a corner at `freq`.
    ///
    /// `a = (1 - tan(w)) / (1 + tan(w))`, with tan taken from its first two
    /// series terms — accurate to well under a percent across the sweep and
    /// far closer than dropping the tan entirely, which is what the previous
    /// build did and which drifts badly once the sweep reaches a few kHz.
    #[inline]
    fn coeff(w: f32) -> f32 {
        let t = w * (1.0 + w * w * (1.0 / 3.0));
        (1.0 - t) / (1.0 + t)
    }

    #[inline]
    fn coeffs_for(&self, freq: f32, out: &mut [f32; MAX_STAGES]) {
        let base = std::f32::consts::PI * freq / self.sample_rate;
        for (slot, &ratio) in out.iter_mut().zip(self.stagger.iter()) {
            *slot = Self::coeff((base * ratio).clamp(1.0e-4, 1.2));
        }
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let depth = self.depth.tick();
        let blend = self.blend.tick();
        let makeup = self.makeup.tick();
        let p = self.profile;

        // Sweep symmetrically about the voice's centre, so turning Depth down
        // narrows the sweep instead of parking it on the bottom of the range.
        let shape = |raw: f32| {
            let unit = (raw * 0.5 + 0.5).clamp(0.0, 1.0);
            let skewed = if p.skew == 1.0 {
                unit
            } else {
                unit.powf(p.skew)
            };
            self.centre_hz * self.span.powf((skewed - 0.5) * depth)
        };
        let freq_l = shape(self.lfo_l.tick());
        let freq_r = shape(self.lfo_r.tick());

        let mut coeffs = [0.0f32; MAX_STAGES];
        self.coeffs_for(freq_l, &mut coeffs);
        let wet_l = self.left.run(left, &coeffs, p.stages, p.feedback);
        self.coeffs_for(freq_r, &mut coeffs);
        let wet_r = self.right.run(right, &coeffs, p.stages, p.feedback);

        (
            finite(mix(left, wet_l, blend) * makeup),
            finite(mix(right, wet_r, blend) * makeup),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [PhaserVoice; 6] = [
        PhaserVoice::Phase90,
        PhaserVoice::MolamSwirl,
        PhaserVoice::PhinVibe,
        PhaserVoice::KhaenSwirl,
        PhaserVoice::BiLam,
        PhaserVoice::IsanJet,
    ];

    /// Peak-to-null depth of the parked cascade's magnitude response, in dB.
    ///
    /// Chasing a moving null with an envelope follower measures the follower:
    /// at a playable rate the notch crosses a given frequency in a couple of
    /// milliseconds, so any window long enough to give a stable RMS averages
    /// the dip away. Parking the sweep — Depth at zero holds the corner on the
    /// voice's centre — turns the question back into a static one, and the
    /// spread between the response's peak and its deepest null is exactly the
    /// spectral bite the sweep will drag across the signal.
    fn notch_depth(voice: PhaserVoice, mix_pct: f32) -> f32 {
        let mut lo = f32::INFINITY;
        let mut hi = 0.0f32;
        // Fine enough to land in the bottom of a four-stage null, which is
        // narrow: a coarse grid straddles it and reports a shallow dip.
        const POINTS: usize = 400;
        for step in 0..POINTS {
            let freq = 90.0 * (9_000.0f32 / 90.0).powf(step as f32 / (POINTS - 1) as f32);
            let mut p = Phaser::new(48_000.0);
            p.configure(voice, 0.0, 0.0, mix_pct);
            p.reset();
            let tone = |n: usize| (n as f32 * freq * std::f32::consts::TAU / 48_000.0).sin();
            for n in 0..4_800 {
                p.process(tone(n), tone(n));
            }
            // Goertzel-style magnitude at the probe frequency itself.
            let n_win = 4_096;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for k in 0..n_win {
                let n = 4_800 + k;
                let y = p.process(tone(n), tone(n)).0 as f64;
                let w = k as f32 * freq * std::f32::consts::TAU / 48_000.0;
                re += y * w.cos() as f64;
                im -= y * w.sin() as f64;
            }
            let mag = ((re * re + im * im).sqrt() * 2.0 / n_win as f64) as f32;
            lo = lo.min(mag);
            hi = hi.max(mag);
        }
        20.0 * (hi / lo.max(1.0e-9)).log10()
    }

    /// The bug this file was rewritten for: the blend ran to bare wet, and an
    /// allpass cascade is flat, so the top of the Mix knob switched the effect
    /// off. Full Mix must be the *strongest* setting, not the weakest.
    #[test]
    fn full_mix_is_the_deepest_setting_not_the_shallowest() {
        for voice in ALL {
            let half = notch_depth(voice, 50.0);
            let full = notch_depth(voice, 100.0);
            assert!(
                full > half,
                "{voice:?}: mix 100% ({full:.1} dB) is weaker than 50% ({half:.1} dB)"
            );
            assert!(
                full > 12.0,
                "{voice:?}: only {full:.1} dB peak-to-null — that is a wobble, not a phaser"
            );
        }
    }

    /// Regeneration raises the peaks between the nulls as well as sharpening
    /// them, and the first cut of these voices shipped that raw: at feedback
    /// 0.8 the resonance handed everything downstream a 9.5 dB lift, which
    /// into a driven amp is not resonance, it is clipping. No voice may make
    /// the signal louder than a hair over unity, at any Mix setting.
    #[test]
    fn no_voice_boosts_the_signal() {
        for voice in ALL {
            for mix_pct in [0.0, 25.0, 50.0, 75.0, 100.0] {
                let peak = peak_gain(voice, mix_pct);
                assert!(
                    peak < 1.6,
                    "{voice:?} at mix {mix_pct}%: peaks {:.1} dB above the input",
                    20.0 * peak.log10()
                );
            }
        }
        // ...and Mix at zero has to be a true bypass, not a quiet one.
        for voice in ALL {
            let unity = peak_gain(voice, 0.0);
            assert!(
                (unity - 1.0).abs() < 0.02,
                "{voice:?} at mix 0% is not unity: {unity:.3}"
            );
        }
    }

    /// Largest steady-state gain the parked cascade applies to any tone.
    fn peak_gain(voice: PhaserVoice, mix_pct: f32) -> f32 {
        let mut hi = 0.0f32;
        for step in 0..80 {
            let freq = 90.0 * (9_000.0f32 / 90.0).powf(step as f32 / 79.0);
            let mut p = Phaser::new(48_000.0);
            p.configure(voice, 0.0, 0.0, mix_pct);
            p.reset();
            let tone = |n: usize| (n as f32 * freq * std::f32::consts::TAU / 48_000.0).sin();
            for n in 0..4_800 {
                p.process(tone(n), tone(n));
            }
            let n_win = 4_096;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for k in 0..n_win {
                let y = p.process(tone(4_800 + k), tone(4_800 + k)).0 as f64;
                let w = k as f32 * freq * std::f32::consts::TAU / 48_000.0;
                re += y * w.cos() as f64;
                im -= y * w.sin() as f64;
            }
            hi = hi.max(((re * re + im * im).sqrt() * 2.0 / n_win as f64) as f32);
        }
        hi
    }

    /// Six voices that measure the same are one voice with six names.
    #[test]
    fn the_voices_are_audibly_distinct() {
        let render = |voice: PhaserVoice| {
            let mut p = Phaser::new(48_000.0);
            p.configure(voice, 5.0, 7.0, 100.0);
            p.reset();
            (0..24_000)
                .map(|n| {
                    let t = n as f32 / 48_000.0;
                    let x = (t * 220.0 * std::f32::consts::TAU).sin() * 0.3
                        + (t * 987.0 * std::f32::consts::TAU).sin() * 0.2;
                    p.process(x, x).0
                })
                .collect::<Vec<_>>()
        };
        let rendered: Vec<_> = ALL.iter().map(|v| render(*v)).collect();
        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                let rms = (rendered[i]
                    .iter()
                    .zip(rendered[j].iter())
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    / rendered[i].len() as f32)
                    .sqrt();
                assert!(rms > 0.01, "{:?} == {:?}: {rms}", ALL[i], ALL[j]);
            }
        }
    }

    /// More allpass stages must buy more nulls, which is what separates the
    /// discreet voices from the swirling ones.
    #[test]
    fn stage_count_tracks_the_voice() {
        assert_eq!(PhaserVoice::Phase90.profile().stages, 4);
        assert_eq!(PhaserVoice::KhaenSwirl.profile().stages, 8);
        assert_eq!(PhaserVoice::BiLam.profile().stages, 12);
        for voice in ALL {
            let p = voice.profile();
            assert!(p.stages <= MAX_STAGES, "{voice:?} overruns the cascade");
            assert!(p.stages % 2 == 0, "{voice:?} has a half-formed notch");
            assert!(p.feedback < 1.0, "{voice:?} would ring");
        }
    }

    #[test]
    fn stays_finite_and_bounded_under_abuse() {
        for voice in ALL {
            for &sr in &[44_100.0, 48_000.0, 96_000.0, 192_000.0] {
                let mut p = Phaser::new(sr);
                p.configure(voice, 10.0, 10.0, 100.0);
                p.reset();
                let mut peak = 0.0f32;
                for n in 0..8_000 {
                    // Full-scale square: the worst case for a regenerating
                    // cascade.
                    let x = if n % 41 < 20 { 1.0 } else { -1.0 };
                    let (l, r) = p.process(x, -x);
                    assert!(l.is_finite() && r.is_finite(), "{voice:?} sr={sr}");
                    peak = peak.max(l.abs()).max(r.abs());
                }
                assert!(peak < 6.0, "{voice:?} sr={sr} ran away: {peak}");
            }
        }
    }

    /// Switching voice mid-signal must not hand the new cascade the old one's
    /// charge.
    #[test]
    fn changing_voice_does_not_click() {
        let mut p = Phaser::new(48_000.0);
        p.configure(PhaserVoice::BiLam, 4.0, 8.0, 100.0);
        p.reset();
        let mut previous = 0.0;
        let mut worst = 0.0f32;
        for n in 0..16_000 {
            if n == 8_000 {
                p.configure(PhaserVoice::PhinVibe, 4.0, 8.0, 100.0);
            }
            let x = (n as f32 * 0.05).sin() * 0.4;
            let y = p.process(x, x).0;
            if n > 100 {
                worst = worst.max((y - previous).abs());
            }
            previous = y;
        }
        assert!(worst < 0.5, "voice change clicked: {worst}");
    }
}
