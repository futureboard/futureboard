//! Multi-stage guitar amplifier model.
//!
//! The classic amp path models the electrical order of a real amplifier:
//! input loading → cascaded, AC-coupled preamp stages → a coupled passive
//! tone network → phase inverter → power stage and supply sag → output
//! transformer → reactive speaker load/negative feedback.  Clean and crunch
//! models run the complete nonlinear core at 4×; the two high-gain models run
//! at 8×.  Model changes use two preallocated lanes and an equal-power
//! crossfade, so no allocation or coefficient construction occurs here.
//!
//! Every nonlinearity is a circuit stage from [`super::nonlinear`], never a
//! transfer curve. Each preamp stage is a self-biasing triode with a cathode
//! bypass, Miller pole and grid conduction; the output stage is a real push-pull
//! pair against a sagging rail. The previous build shaped a low band and a high
//! band through two static curves and summed them, which both muddied the low
//! end (bass hit the same ceiling as the pick attack) and produced the flat,
//! non-guitar break-up of a memoryless clipper.

use builtin_dsp_core::time_constant;

use super::AmpModel;
use super::nonlinear::{PushPullParams, TriodeParams, TriodeState};
use super::smooth::{Oversampler4x, Oversampler8x, Smoothed};

const MAX_PREAMP_STAGES: usize = 4;
const CONTROL_SMOOTH_SECONDS: f32 = 0.012;
const MODEL_FADE_SECONDS: f32 = 0.012;

#[inline]
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-8.0, 8.0)
    } else {
        0.0
    }
}

#[inline]
fn pole(freq: f32, sample_rate: f32) -> f32 {
    (-std::f32::consts::TAU * freq.clamp(1.0, sample_rate * 0.45) / sample_rate.max(1.0)).exp()
}

#[inline]
fn alpha(freq: f32, sample_rate: f32) -> f32 {
    1.0 - pole(freq, sample_rate)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToneFamily {
    American,
    British,
    Modern,
    Bass,
}

#[derive(Debug, Clone, Copy)]
struct AmpProfile {
    stages: usize,
    oversample: usize,
    tone: ToneFamily,
    input_hpf: f32,
    input_lpf: f32,
    coupling_hpf: f32,
    stage_lpf: f32,
    pre_gain: f32,
    inter_gain: f32,
    bias: f32,
    bias_move: f32,
    pre_compression: f32,
    tightness: f32,
    pi_drive: f32,
    pi_compression: f32,
    power_drive: f32,
    power_headroom: f32,
    power_asymmetry: f32,
    feedback: f32,
    presence_depth: f32,
    resonance: f32,
    damping: f32,
    sag_amount: f32,
    sag_attack: f32,
    sag_release: f32,
    transformer_memory: f32,
    transformer_compression: f32,
    output_gain: f32,
    shape_seed: u8,
    clean_low_blend: f32,
}

impl AmpProfile {
    fn for_model(model: AmpModel) -> Self {
        match model {
            // Warm British crunch: three stages, strong low-mid transformer
            // memory and enough supply movement to make Gain/Master interact.
            AmpModel::Mandarin => Self {
                stages: 3,
                oversample: 4,
                tone: ToneFamily::British,
                input_hpf: 38.0,
                input_lpf: 17_000.0,
                coupling_hpf: 82.0,
                stage_lpf: 7_800.0,
                pre_gain: 2.5,
                inter_gain: 1.75,
                bias: 0.09,
                bias_move: 0.17,
                pre_compression: 0.25,
                tightness: 0.25,
                pi_drive: 1.55,
                pi_compression: 0.32,
                power_drive: 2.1,
                power_headroom: 1.05,
                power_asymmetry: 0.08,
                feedback: 0.28,
                presence_depth: 0.42,
                resonance: 0.24,
                damping: 0.42,
                sag_amount: 0.27,
                sag_attack: 0.010,
                sag_release: 0.130,
                transformer_memory: 0.21,
                transformer_compression: 0.25,
                output_gain: 0.72,
                shape_seed: 1,
                clean_low_blend: 0.0,
            },
            AmpModel::Plexi => Self {
                stages: 3,
                oversample: 4,
                tone: ToneFamily::British,
                input_hpf: 48.0,
                input_lpf: 19_000.0,
                coupling_hpf: 105.0,
                stage_lpf: 9_500.0,
                pre_gain: 2.15,
                inter_gain: 1.65,
                bias: 0.07,
                bias_move: 0.13,
                pre_compression: 0.18,
                tightness: 0.32,
                pi_drive: 1.45,
                pi_compression: 0.25,
                power_drive: 2.45,
                power_headroom: 1.16,
                power_asymmetry: 0.05,
                feedback: 0.24,
                presence_depth: 0.50,
                resonance: 0.18,
                damping: 0.38,
                sag_amount: 0.22,
                sag_attack: 0.012,
                sag_release: 0.105,
                transformer_memory: 0.16,
                transformer_compression: 0.20,
                output_gain: 0.75,
                shape_seed: 2,
                clean_low_blend: 0.0,
            },
            // High-headroom American clean. Gain mostly changes stage loading,
            // bias movement and bright-cap balance before clipping becomes mild.
            AmpModel::Twin => Self {
                stages: 2,
                oversample: 4,
                tone: ToneFamily::American,
                input_hpf: 28.0,
                input_lpf: 20_000.0,
                coupling_hpf: 58.0,
                stage_lpf: 12_500.0,
                pre_gain: 1.25,
                inter_gain: 1.18,
                bias: 0.035,
                bias_move: 0.055,
                pre_compression: 0.08,
                tightness: 0.08,
                pi_drive: 1.12,
                pi_compression: 0.11,
                power_drive: 1.45,
                power_headroom: 1.55,
                power_asymmetry: 0.025,
                feedback: 0.46,
                presence_depth: 0.34,
                resonance: 0.12,
                damping: 0.58,
                sag_amount: 0.09,
                sag_attack: 0.018,
                sag_release: 0.075,
                transformer_memory: 0.10,
                transformer_compression: 0.10,
                output_gain: 0.86,
                shape_seed: 0,
                clean_low_blend: 0.0,
            },
            AmpModel::TopBoost => Self {
                stages: 3,
                oversample: 4,
                tone: ToneFamily::British,
                input_hpf: 55.0,
                input_lpf: 20_000.0,
                coupling_hpf: 125.0,
                stage_lpf: 12_000.0,
                pre_gain: 1.75,
                inter_gain: 1.42,
                bias: 0.075,
                bias_move: 0.11,
                pre_compression: 0.17,
                tightness: 0.24,
                pi_drive: 1.30,
                pi_compression: 0.20,
                power_drive: 1.85,
                power_headroom: 1.20,
                power_asymmetry: 0.06,
                feedback: 0.20,
                presence_depth: 0.46,
                resonance: 0.14,
                damping: 0.37,
                sag_amount: 0.17,
                sag_attack: 0.014,
                sag_release: 0.090,
                transformer_memory: 0.12,
                transformer_compression: 0.16,
                output_gain: 0.78,
                shape_seed: 3,
                clean_low_blend: 0.0,
            },
            // Four tightly-coupled stages, low sag and fast recovery. The high
            // coupling corner rises with Gain, so a boost increases saturation
            // while palm-muted lows stay controlled.
            AmpModel::Recto => Self {
                stages: 4,
                oversample: 8,
                tone: ToneFamily::Modern,
                input_hpf: 54.0,
                input_lpf: 18_000.0,
                coupling_hpf: 145.0,
                stage_lpf: 8_600.0,
                pre_gain: 3.2,
                inter_gain: 2.05,
                bias: 0.105,
                bias_move: 0.16,
                pre_compression: 0.22,
                tightness: 0.78,
                pi_drive: 1.75,
                pi_compression: 0.24,
                power_drive: 2.25,
                power_headroom: 1.08,
                power_asymmetry: 0.045,
                // Little global feedback — that looseness and raw upper-order
                // content is the voice. Presence keeps its authority because
                // it rides `presence_depth`, not the damping term.
                feedback: 0.20,
                presence_depth: 0.95,
                resonance: 0.30,
                damping: 0.62,
                sag_amount: 0.07,
                sag_attack: 0.004,
                sag_release: 0.038,
                transformer_memory: 0.09,
                transformer_compression: 0.15,
                output_gain: 0.57,
                shape_seed: 2,
                clean_low_blend: 0.0,
            },
            AmpModel::Jcm => Self {
                stages: 3,
                oversample: 4,
                tone: ToneFamily::British,
                input_hpf: 62.0,
                input_lpf: 18_000.0,
                coupling_hpf: 118.0,
                stage_lpf: 8_800.0,
                pre_gain: 2.75,
                inter_gain: 1.85,
                bias: 0.10,
                bias_move: 0.18,
                pre_compression: 0.25,
                tightness: 0.44,
                pi_drive: 1.60,
                pi_compression: 0.30,
                power_drive: 2.15,
                power_headroom: 1.08,
                power_asymmetry: 0.07,
                feedback: 0.34,
                presence_depth: 0.50,
                resonance: 0.20,
                damping: 0.48,
                sag_amount: 0.20,
                sag_attack: 0.009,
                sag_release: 0.090,
                transformer_memory: 0.15,
                transformer_compression: 0.22,
                output_gain: 0.67,
                shape_seed: 1,
                clean_low_blend: 0.0,
            },
            AmpModel::Slate => Self {
                stages: 4,
                oversample: 8,
                tone: ToneFamily::Modern,
                input_hpf: 60.0,
                input_lpf: 18_500.0,
                coupling_hpf: 170.0,
                stage_lpf: 9_200.0,
                pre_gain: 3.55,
                inter_gain: 2.18,
                bias: 0.12,
                bias_move: 0.20,
                pre_compression: 0.27,
                tightness: 0.88,
                pi_drive: 1.85,
                pi_compression: 0.27,
                power_drive: 2.40,
                power_headroom: 1.02,
                power_asymmetry: 0.055,
                // As Recto: minimal global feedback so the power section stays
                // nonlinear at the top of the Gain knob.
                feedback: 0.24,
                presence_depth: 1.00,
                resonance: 0.26,
                damping: 0.66,
                sag_amount: 0.055,
                sag_attack: 0.003,
                sag_release: 0.030,
                transformer_memory: 0.075,
                transformer_compression: 0.17,
                output_gain: 0.53,
                shape_seed: 0,
                clean_low_blend: 0.0,
            },
            AmpModel::Bassman => Self {
                stages: 3,
                oversample: 4,
                tone: ToneFamily::Bass,
                input_hpf: 18.0,
                input_lpf: 16_000.0,
                coupling_hpf: 34.0,
                stage_lpf: 8_500.0,
                pre_gain: 1.85,
                inter_gain: 1.40,
                bias: 0.065,
                bias_move: 0.09,
                pre_compression: 0.16,
                tightness: 0.05,
                pi_drive: 1.35,
                pi_compression: 0.19,
                power_drive: 1.75,
                power_headroom: 1.35,
                power_asymmetry: 0.04,
                feedback: 0.31,
                presence_depth: 0.28,
                resonance: 0.34,
                damping: 0.50,
                sag_amount: 0.14,
                sag_attack: 0.016,
                sag_release: 0.115,
                transformer_memory: 0.18,
                transformer_compression: 0.17,
                output_gain: 0.79,
                shape_seed: 3,
                clean_low_blend: 0.24,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Settings {
    gain: f32,
    bass: f32,
    middle: f32,
    treble: f32,
    presence: f32,
    master: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gain: 5.0,
            bass: 5.0,
            middle: 5.0,
            treble: 5.0,
            presence: 5.0,
            master: 5.0,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct InputChannel {
    dc_x: f32,
    dc_y: f32,
    low: f32,
}

#[derive(Debug, Clone)]
struct InputFilter {
    left: InputChannel,
    right: InputChannel,
    hp_pole: f32,
    lp_alpha: f32,
}

impl InputFilter {
    fn new() -> Self {
        Self {
            left: InputChannel::default(),
            right: InputChannel::default(),
            hp_pole: 0.99,
            lp_alpha: 1.0,
        }
    }

    fn configure(&mut self, profile: AmpProfile, sample_rate: f32) {
        self.hp_pole = pole(profile.input_hpf, sample_rate);
        self.lp_alpha = alpha(profile.input_lpf, sample_rate);
    }

    fn reset(&mut self) {
        self.left = InputChannel::default();
        self.right = InputChannel::default();
    }

    #[inline]
    fn one(channel: &mut InputChannel, x: f32, hp_pole: f32, lp_alpha: f32) -> f32 {
        let hp = x - channel.dc_x + hp_pole * channel.dc_y;
        channel.dc_x = x;
        channel.dc_y = finite(hp);
        channel.low += lp_alpha * (channel.dc_y - channel.low);
        finite(channel.low)
    }

    #[inline]
    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        (
            Self::one(&mut self.left, left, self.hp_pole, self.lp_alpha),
            Self::one(&mut self.right, right, self.hp_pole, self.lp_alpha),
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ToneState {
    low: f32,
    high_lp: f32,
}

#[derive(Debug, Clone, Default)]
struct ChannelCore {
    stages: [TriodeState; MAX_PREAMP_STAGES],
    inverter: TriodeState,
    tone: ToneState,
    sag: f32,
    transformer_flux: f32,
    speaker_low: f32,
    feedback_high_lp: f32,
    previous_load: f32,
}

impl ChannelCore {
    /// Start every valve at its own operating point instead of letting the
    /// cathode RCs charge up audibly over the first few milliseconds.
    fn prime(&mut self, voicing: &AmpVoicing) {
        *self = Self::default();
        for (state, params) in self.stages.iter_mut().zip(voicing.stages.iter()) {
            state.prime(params);
        }
        self.inverter.prime(&voicing.inverter);
    }
}

#[derive(Debug, Clone, Default)]
struct AmpCore {
    left: ChannelCore,
    right: ChannelCore,
}

impl AmpCore {
    fn prime(&mut self, voicing: &AmpVoicing) {
        self.left.prime(voicing);
        self.right.prime(voicing);
    }
}

/// The valve complement of one amp lane, solved on the control path.
///
/// Operating points, plate-load normalization and grid-conduction thresholds
/// all take iteration to settle, so they are resolved here whenever the model
/// or a knob changes and only read from the audio callback.
#[derive(Debug, Clone)]
struct AmpVoicing {
    stages: [TriodeParams; MAX_PREAMP_STAGES],
    inverter: TriodeParams,
    power: PushPullParams,
}

impl Default for AmpVoicing {
    fn default() -> Self {
        let mut stage = TriodeParams {
            grid_offset: 1.35,
            knee: 0.05,
            headroom: 3.4,
            sat_knee: 0.06,
            cathode_r: 0.20,
            cathode_alpha: 0.01,
            miller_alpha: 0.5,
            grid_headroom: 1.2,
            grid_soft: 0.22,
            grid_attack: 0.02,
            grid_release: 0.0002,
            grid_clamp: 0.0,
            plate_r: 1.0,
            quiescent: 0.0,
        };
        stage.solve_operating_point();
        let mut power = PushPullParams {
            bias: 0.55,
            knee: 0.05,
            headroom: 2.4,
            sat_knee: 0.06,
            asymmetry: 0.06,
            scale: 1.0,
        };
        power.normalize();
        Self {
            stages: [stage; MAX_PREAMP_STAGES],
            inverter: stage,
            power,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HeldControls {
    stage_gain: f32,
    inter_gain: f32,
    coupling_pole: f32,
    tone_low_alpha: f32,
    tone_high_alpha: f32,
    bass: f32,
    middle: f32,
    treble: f32,
    presence: f32,
    pi_drive: f32,
    master_drive: f32,
    master_level: f32,
    sag_attack: f32,
    sag_release: f32,
    transformer_alpha: f32,
    speaker_alpha: f32,
    feedback_high_alpha: f32,
}

#[inline]
fn envelope_tick(state: &mut f32, input: f32, attack: f32, release: f32) -> f32 {
    let target = input.abs().min(8.0);
    let coeff = if target > *state { attack } else { release };
    *state = target + (*state - target) * coeff;
    if !state.is_finite() {
        *state = 0.0;
    }
    *state
}

impl AmpCore {
    #[inline]
    fn tone_stack(state: &mut ToneState, x: f32, family: ToneFamily, held: HeldControls) -> f32 {
        state.low += held.tone_low_alpha * (x - state.low);
        state.high_lp += held.tone_high_alpha * (x - state.high_lp);
        let low = state.low;
        let high = x - state.high_lp;
        let mid = state.high_lp - state.low;
        let b = held.bass;
        let m = held.middle;
        let t = held.treble;

        // These are coupled passive-network mappings, not three independent
        // boosts. Turning one control changes the loading/gain of the others.
        let (lg, mg, hg, insertion) = match family {
            ToneFamily::American => (
                0.50 + b * 1.18 - t * 0.18,
                0.23 + m * 0.96 - b * 0.28 - t * 0.20,
                0.45 + t * 1.24 - b * 0.12,
                0.72,
            ),
            ToneFamily::British => (
                0.48 + b * 1.02 - m * 0.10,
                0.52 + m * 1.08 - t * 0.12,
                0.42 + t * 1.08 - b * 0.12,
                0.70,
            ),
            ToneFamily::Modern => (
                0.42 + b * 0.94 - t * 0.10,
                0.30 + m * 1.02 - b * 0.14 + t * 0.10,
                0.50 + t * 1.28 - m * 0.10,
                0.68,
            ),
            ToneFamily::Bass => (
                0.62 + b * 1.25 - t * 0.08,
                0.38 + m * 0.92 - b * 0.12,
                0.35 + t * 0.90 - b * 0.08,
                0.76,
            ),
        };
        finite((low * lg + mid * mg + high * hg) * insertion)
    }

    #[inline]
    fn process_channel(
        channel: &mut ChannelCore,
        input: f32,
        profile: AmpProfile,
        voicing: &AmpVoicing,
        held: HeldControls,
    ) -> f32 {
        let mut x = input;
        let clean_low = if profile.clean_low_blend > 0.0 {
            channel.tone.low += held.tone_low_alpha * (x - channel.tone.low);
            channel.tone.low
        } else {
            0.0
        };

        // Cascaded valve stages. Each one carries its own cathode charge, grid
        // charge and Miller state, so the cascade's break-up depends on what it
        // has been fed — and the cathode bypass corner keeps the low end from
        // being amplified into the next stage's cutoff, which is what used to
        // turn a driven cascade to mud.
        for i in 0..profile.stages {
            let gain = if i == 0 {
                held.stage_gain
            } else {
                held.inter_gain * (1.0 + i as f32 * 0.08)
            };
            x = finite(channel.stages[i].process(x, &voicing.stages[i], gain, held.coupling_pole));
        }

        x = Self::tone_stack(&mut channel.tone, x, profile.tone, held);

        // The phase inverter is its own valve: it compresses, shifts bias and
        // blocks on hard transients independently of the preamp.
        let pi = finite(channel.inverter.process(
            x,
            &voicing.inverter,
            held.pi_drive,
            held.coupling_pole,
        ));

        // Reactive speaker/load feedback. Presence changes the high-frequency
        // amount returned around the power stage; resonance changes the
        // low-frequency back-EMF. Neither is a post-EQ shelf.
        channel.speaker_low += held.speaker_alpha * (channel.previous_load - channel.speaker_low);
        channel.feedback_high_lp +=
            held.feedback_high_alpha * (channel.previous_load - channel.feedback_high_lp);
        let feedback_high = channel.previous_load - channel.feedback_high_lp;
        let feedback = profile.feedback
            * (channel.previous_load * profile.damping + channel.speaker_low * profile.resonance
                - feedback_high * held.presence * profile.presence_depth);

        let demand = (pi * held.master_drive).abs() + channel.previous_load.abs() * 0.25;
        let sag_env = envelope_tick(&mut channel.sag, demand, held.sag_attack, held.sag_release);
        let supply = (1.0 - profile.sag_amount * sag_env.min(1.4)).max(0.42);
        // The sagging rail lowers the pair's *ceiling* rather than scaling the
        // signal, so quiet passages keep their level and only the peaks give.
        let power = voicing
            .power
            .process(pi * held.master_drive - feedback, supply);

        // Transformer flux provides low-frequency memory and demand-dependent
        // compression before the electrical speaker load.
        channel.transformer_flux += held.transformer_alpha * (power - channel.transformer_flux);
        let transformer_gain = 1.0
            / (1.0
                + channel.transformer_flux.abs() * profile.transformer_compression
                + demand * 0.035);
        let transformed =
            (power - channel.transformer_flux * profile.transformer_memory) * transformer_gain;
        let load = transformed + channel.speaker_low * profile.resonance * 0.12;
        channel.previous_load = finite(load);

        finite(
            (channel.previous_load + clean_low * profile.clean_low_blend)
                * profile.output_gain
                * held.master_level,
        )
    }

    #[inline]
    fn process_stereo(
        &mut self,
        left: f32,
        right: f32,
        profile: AmpProfile,
        voicing: &AmpVoicing,
        held: HeldControls,
    ) -> (f32, f32) {
        (
            Self::process_channel(&mut self.left, left, profile, voicing, held),
            Self::process_channel(&mut self.right, right, profile, voicing, held),
        )
    }
}

#[derive(Debug, Clone)]
struct AmpLane {
    sample_rate: f32,
    model: AmpModel,
    profile: AmpProfile,
    settings: Settings,
    input: InputFilter,
    core: AmpCore,
    voicing: AmpVoicing,
    os4: Oversampler4x,
    os8: Oversampler8x,
    stage_gain: Smoothed,
    inter_gain: Smoothed,
    coupling_pole: Smoothed,
    tone_low_alpha: Smoothed,
    tone_high_alpha: Smoothed,
    bass: Smoothed,
    middle: Smoothed,
    treble: Smoothed,
    presence: Smoothed,
    pi_drive: Smoothed,
    master_drive: Smoothed,
    master_level: Smoothed,
    sag_attack_coeff: f32,
    sag_release_coeff: f32,
    transformer_alpha_coeff: f32,
    speaker_alpha_coeff: f32,
    feedback_high_alpha_coeff: f32,
}

impl AmpLane {
    fn new(sample_rate: f32, model: AmpModel) -> Self {
        let sr = sample_rate.max(1.0);
        let p = AmpProfile::for_model(model);
        let mut lane = Self {
            sample_rate: sr,
            model,
            profile: p,
            settings: Settings::default(),
            input: InputFilter::new(),
            core: AmpCore::default(),
            voicing: AmpVoicing::default(),
            os4: Oversampler4x::new(),
            os8: Oversampler8x::new(),
            stage_gain: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 1.0),
            inter_gain: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 1.0),
            coupling_pole: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.99),
            tone_low_alpha: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.1),
            tone_high_alpha: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.1),
            bass: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.5),
            middle: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.5),
            treble: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.5),
            presence: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.5),
            pi_drive: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, p.pi_drive),
            master_drive: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 1.0),
            master_level: Smoothed::new(sr, CONTROL_SMOOTH_SECONDS, 0.5),
            sag_attack_coeff: 0.0,
            sag_release_coeff: 0.0,
            transformer_alpha_coeff: 0.0,
            speaker_alpha_coeff: 0.0,
            feedback_high_alpha_coeff: 0.0,
        };
        lane.configure(model, Settings::default());
        lane.snap_controls();
        lane
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        for value in [
            &mut self.stage_gain,
            &mut self.inter_gain,
            &mut self.coupling_pole,
            &mut self.tone_low_alpha,
            &mut self.tone_high_alpha,
            &mut self.bass,
            &mut self.middle,
            &mut self.treble,
            &mut self.presence,
            &mut self.pi_drive,
            &mut self.master_drive,
            &mut self.master_level,
        ] {
            value.set_time(self.sample_rate, CONTROL_SMOOTH_SECONDS);
        }
        self.configure(self.model, self.settings);
    }

    fn configure(&mut self, model: AmpModel, settings: Settings) {
        self.model = model;
        self.profile = AmpProfile::for_model(model);
        self.settings = settings;
        self.input.configure(self.profile, self.sample_rate);

        let g = (settings.gain / 10.0).clamp(0.0, 1.0).powf(1.45);
        let m = (settings.master / 10.0).clamp(0.0, 1.0);
        let osr = self.sample_rate * self.profile.oversample as f32;

        // Gain moves stage gain, interstage drive and the coupling corner
        // together. It is deliberately not an input multiply. Wide open every
        // valve is well past its knee, so the cascade reaches a real saturated
        // wall instead of asymptoting a few dB above clean.
        self.stage_gain
            .set_target(self.profile.pre_gain * (0.22 + g * 2.60));
        self.inter_gain
            .set_target(self.profile.inter_gain * (0.30 + g * 1.95));
        self.pi_drive.set_target(self.profile.pi_drive);
        let coupling_hz = self.profile.coupling_hpf * (1.0 + g * self.profile.tightness * 1.45);
        self.coupling_pole.set_target(pole(coupling_hz, osr));
        self.resolve_voicing(g, osr);

        let b = (settings.bass / 10.0).clamp(0.0, 1.0);
        let mid = (settings.middle / 10.0).clamp(0.0, 1.0);
        let t = (settings.treble / 10.0).clamp(0.0, 1.0);
        self.bass.set_target(b);
        self.middle.set_target(mid);
        self.treble.set_target(t);
        self.presence
            .set_target((settings.presence / 10.0).clamp(0.0, 1.0));

        let (low_hz, high_hz) = match self.profile.tone {
            ToneFamily::American => (150.0 + t * 28.0, 2_250.0 + mid * 420.0),
            ToneFamily::British => (175.0 + t * 38.0, 1_850.0 + mid * 480.0),
            ToneFamily::Modern => (125.0 + t * 45.0, 2_050.0 + mid * 620.0),
            ToneFamily::Bass => (105.0 + t * 22.0, 1_550.0 + mid * 330.0),
        };
        self.tone_low_alpha.set_target(alpha(low_hz, osr));
        self.tone_high_alpha.set_target(alpha(high_hz, osr));

        // Master drives the phase inverter/power stage independently from
        // preamp Gain; only a modest level taper follows it.
        self.master_drive
            .set_target((0.04 + m.powf(1.35) * 2.45) * self.profile.power_drive);
        self.master_level.set_target(0.10 + m.sqrt() * 0.90);

        // All time constants are prepared on the control path. The audio
        // callback only reads these scalars.
        self.sag_attack_coeff = time_constant(osr, self.profile.sag_attack);
        self.sag_release_coeff = time_constant(osr, self.profile.sag_release);
        self.transformer_alpha_coeff = alpha(72.0, osr);
        self.speaker_alpha_coeff = alpha(115.0, osr);
        self.feedback_high_alpha_coeff = alpha(3_100.0, osr);
    }

    /// Resolve the lane's valve complement: component values, operating points
    /// and the normalizations that depend on them.
    ///
    /// Everything here needs iteration or a square root chain, so it happens
    /// once per configure and never in the audio callback.
    fn resolve_voicing(&mut self, g: f32, osr: f32) {
        let p = self.profile;

        // Cathode bypass corner. Below it the cathode follows the signal and
        // degenerates the stage's own gain, so the low end reaches the next
        // grid smaller than the mids do. This is the amp's real tightness
        // mechanism and the reason a driven cascade stays articulate instead
        // of turning to mud — no band split required.
        let cathode_alpha = alpha(70.0 + p.tightness * 320.0 + g * 60.0, osr);
        // Grid current builds fast and leaks away over tens of milliseconds.
        let grid_attack = alpha(400.0 + p.bias_move * 2_600.0, osr);
        let grid_release = alpha(5.0 + g * 12.0, osr);
        let grid_headroom = (1.95 - p.pre_compression * 3.0).max(0.55);
        let grid_soft = (0.32 - p.pre_compression * 0.40).clamp(0.10, 0.40);
        let cathode_r = 0.12 + p.pre_compression * 0.50;

        for i in 0..MAX_PREAMP_STAGES {
            // Adjacent stages are deliberately not identical valves: the seed
            // walks the operating point, cutoff softness and plate ceiling.
            let variant = (p.shape_seed.wrapping_add(i as u8) & 3) as f32;
            let mut stage = TriodeParams {
                grid_offset: 1.22 + p.bias * 2.2 + variant * 0.06,
                knee: 0.030 + variant * 0.008,
                headroom: 2.45 - variant * 0.14,
                sat_knee: 0.05 + variant * 0.01,
                cathode_r,
                cathode_alpha,
                // Miller capacitance rolls the plate off inside the stage, so
                // the next grid never receives a raw clipping edge to re-clip
                // into fizz. Later stages sit darker, as they do in a real amp.
                miller_alpha: alpha(
                    p.stage_lpf * (1.0 - g * 0.12) * (1.0 - i as f32 * 0.12),
                    osr,
                ),
                grid_headroom,
                grid_soft,
                grid_attack,
                grid_release,
                grid_clamp: 0.0,
                plate_r: 1.0,
                quiescent: 0.0,
            };
            stage.solve_operating_point();
            self.voicing.stages[i] = stage;
        }

        // The phase inverter is a long-tailed pair: a large unbypassed tail
        // resistor, so it degenerates hard and compresses on its own.
        let mut inverter = TriodeParams {
            grid_offset: 1.30 + p.power_asymmetry * 1.4,
            knee: 0.040,
            headroom: 2.20 + p.power_headroom * 0.35,
            sat_knee: 0.05,
            cathode_r: cathode_r * (1.2 + p.pi_compression * 1.4),
            cathode_alpha: alpha(22.0, osr),
            miller_alpha: alpha(p.stage_lpf * 1.15, osr),
            grid_headroom: grid_headroom + 0.45,
            grid_soft,
            grid_attack: grid_attack * 0.6,
            grid_release,
            grid_clamp: 0.0,
            plate_r: 1.0,
            quiescent: 0.0,
        };
        inverter.solve_operating_point();
        self.voicing.inverter = inverter;

        // Output pair. A low idle current is class B: the halves hand over
        // through a real crossover region, which is the grind of a cranked
        // output stage rather than a curve pretending to have one.
        let mut power = PushPullParams {
            bias: 0.30 + p.power_headroom * 0.28,
            knee: 0.045,
            headroom: 1.60 + p.power_headroom * 1.20,
            sat_knee: (0.10 / p.power_headroom.max(0.1)).clamp(0.02, 0.20),
            asymmetry: p.power_asymmetry.clamp(0.0, 0.4),
            scale: 1.0,
        };
        power.normalize();
        self.voicing.power = power;
    }

    fn snap_controls(&mut self) {
        for value in [
            &mut self.stage_gain,
            &mut self.inter_gain,
            &mut self.coupling_pole,
            &mut self.tone_low_alpha,
            &mut self.tone_high_alpha,
            &mut self.bass,
            &mut self.middle,
            &mut self.treble,
            &mut self.presence,
            &mut self.pi_drive,
            &mut self.master_drive,
            &mut self.master_level,
        ] {
            value.snap();
        }
    }

    fn reset(&mut self) {
        self.input.reset();
        self.core.prime(&self.voicing);
        self.os4.reset();
        self.os8.reset();
        self.snap_controls();
    }

    #[inline]
    fn held_controls(&mut self) -> HeldControls {
        HeldControls {
            stage_gain: self.stage_gain.tick(),
            inter_gain: self.inter_gain.tick(),
            coupling_pole: self.coupling_pole.tick(),
            tone_low_alpha: self.tone_low_alpha.tick(),
            tone_high_alpha: self.tone_high_alpha.tick(),
            bass: self.bass.tick(),
            middle: self.middle.tick(),
            treble: self.treble.tick(),
            presence: self.presence.tick(),
            pi_drive: self.pi_drive.tick(),
            master_drive: self.master_drive.tick(),
            master_level: self.master_level.tick(),
            sag_attack: self.sag_attack_coeff,
            sag_release: self.sag_release_coeff,
            transformer_alpha: self.transformer_alpha_coeff,
            speaker_alpha: self.speaker_alpha_coeff,
            feedback_high_alpha: self.feedback_high_alpha_coeff,
        }
    }

    #[inline]
    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (left, right) = self.input.process(left, right);
        let held = self.held_controls();
        let profile = self.profile;
        let voicing = &self.voicing;
        let core = &mut self.core;
        if profile.oversample == 8 {
            self.os8.process_stereo(left, right, |l, r| {
                core.process_stereo(l, r, profile, voicing, held)
            })
        } else {
            self.os4.process_stereo(left, right, |l, r| {
                core.process_stereo(l, r, profile, voicing, held)
            })
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Amp {
    sample_rate: f32,
    active: AmpLane,
    standby: AmpLane,
    switching: bool,
    switch_position: f32,
    switch_step: f32,
}

impl Amp {
    pub(super) fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        Self {
            sample_rate: sr,
            active: AmpLane::new(sr, AmpModel::Mandarin),
            standby: AmpLane::new(sr, AmpModel::Mandarin),
            switching: false,
            switch_position: 0.0,
            switch_step: 1.0 / (sr * MODEL_FADE_SECONDS).max(1.0),
        }
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.switch_step = 1.0 / (self.sample_rate * MODEL_FADE_SECONDS).max(1.0);
        self.active.set_sample_rate(self.sample_rate);
        self.standby.set_sample_rate(self.sample_rate);
    }

    pub(super) fn reset(&mut self) {
        // A host commonly applies state and immediately resets before first
        // audio. Commit a prepared model in that case instead of discarding it.
        if self.switching {
            std::mem::swap(&mut self.active, &mut self.standby);
        }
        self.active.reset();
        self.standby.reset();
        self.switching = false;
        self.switch_position = 0.0;
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn configure(
        &mut self,
        model: AmpModel,
        gain: f32,
        bass: f32,
        middle: f32,
        treble: f32,
        presence: f32,
        master: f32,
    ) {
        let settings = Settings {
            gain,
            bass,
            middle,
            treble,
            presence,
            master,
        };
        if model == self.active.model && !self.switching {
            self.active.configure(model, settings);
        } else if self.switching && model == self.standby.model {
            self.standby.configure(model, settings);
        } else {
            self.standby.configure(model, settings);
            self.standby.reset();
            self.switching = true;
            self.switch_position = 0.0;
        }
    }

    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let active = self.active.process(left, right);
        if !self.switching {
            return active;
        }
        let next = self.standby.process(left, right);
        let p = self.switch_position.clamp(0.0, 1.0);
        let old_gain = (1.0 - p).sqrt();
        let new_gain = p.sqrt();
        let output = (
            finite(active.0 * old_gain + next.0 * new_gain),
            finite(active.1 * old_gain + next.1 * new_gain),
        );
        self.switch_position += self.switch_step;
        if self.switch_position >= 1.0 {
            std::mem::swap(&mut self.active, &mut self.standby);
            self.switching = false;
            self.switch_position = 0.0;
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(model: AmpModel, gain: f32, master: f32, tone: [f32; 3]) -> Vec<f32> {
        let mut amp = Amp::new(48_000.0);
        amp.configure(model, gain, tone[0], tone[1], tone[2], 5.0, master);
        amp.reset();
        let mut out = Vec::with_capacity(16_000);
        for n in 0..16_000 {
            let t = n as f32 / 48_000.0;
            let x = ((t * 110.0 * std::f32::consts::TAU).sin() * 0.24)
                + ((t * 293.0 * std::f32::consts::TAU).sin() * 0.08);
            out.push(amp.process(x, x).0);
        }
        out
    }

    fn harmonic_ratio(output: &[f32]) -> f32 {
        // 375 Hz completes exactly 64 cycles in this 8192-sample window, so
        // filter phase and spectral leakage are not mistaken for harmonics.
        let window = &output[output.len() - 8_192..];
        let magnitude = |harmonic: usize| {
            let frequency = 375.0 * harmonic as f32;
            let mut real = 0.0;
            let mut imag = 0.0;
            for (n, &sample) in window.iter().enumerate() {
                let phase = n as f32 * frequency * std::f32::consts::TAU / 48_000.0;
                real += sample * phase.cos();
                imag -= sample * phase.sin();
            }
            (real * real + imag * imag).sqrt()
        };
        let fundamental = magnitude(1).max(1.0e-9);
        (2..=8).map(magnitude).sum::<f32>() / fundamental
    }

    #[test]
    fn all_models_are_finite_bounded_and_distinct() {
        let mut rendered = Vec::new();
        for model in AmpModel::ALL {
            let output = render(*model, 8.0, 6.0, [5.0; 3]);
            let peak = output.iter().fold(0.0f32, |p, x| p.max(x.abs()));
            assert!(output.iter().all(|x| x.is_finite()), "{model:?}");
            assert!(peak > 0.005 && peak < 4.0, "{model:?} peak={peak}");
            rendered.push(output);
        }
        for i in 0..rendered.len() {
            for j in (i + 1)..rendered.len() {
                let rms = (rendered[i]
                    .iter()
                    .skip(8_000)
                    .zip(rendered[j].iter().skip(8_000))
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    / 8_000.0)
                    .sqrt();
                assert!(
                    rms > 0.001,
                    "{:?} == {:?}",
                    AmpModel::ALL[i],
                    AmpModel::ALL[j]
                );
            }
        }
    }

    #[test]
    fn gain_and_master_are_not_equivalent_controls() {
        let high_gain = render(AmpModel::Jcm, 9.0, 3.0, [5.0; 3]);
        let high_master = render(AmpModel::Jcm, 3.0, 9.0, [5.0; 3]);
        let rms = (high_gain
            .iter()
            .skip(8_000)
            .zip(high_master.iter().skip(8_000))
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            / 8_000.0)
            .sqrt();
        assert!(
            rms > 0.01,
            "Gain and Master collapsed to the same behavior: {rms}"
        );
    }

    #[test]
    fn harmonic_generation_progresses_with_gain() {
        let render_sine = |gain: f32| {
            let mut amp = Amp::new(48_000.0);
            amp.configure(AmpModel::Recto, gain, 5.0, 5.0, 5.0, 5.0, 5.0);
            amp.reset();
            (0..16_000)
                .map(|n| {
                    let x = (n as f32 * 375.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.12;
                    amp.process(x, x).0
                })
                .collect::<Vec<_>>()
        };
        let low = harmonic_ratio(&render_sine(1.0));
        let high = harmonic_ratio(&render_sine(9.0));
        assert!(
            high > low * 1.08,
            "harmonics did not progress: low={low} high={high}"
        );
    }

    #[test]
    fn tone_controls_are_coupled() {
        let bass_low = render(AmpModel::Twin, 3.0, 5.0, [0.0, 5.0, 5.0]);
        let bass_high = render(AmpModel::Twin, 3.0, 5.0, [10.0, 5.0, 5.0]);
        // Coupling means a bass edit changes a mixed two-tone signal, including
        // its upper component, rather than behaving as an isolated low shelf.
        let delta = bass_low
            .iter()
            .skip(8_000)
            .zip(bass_high.iter().skip(8_000))
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            / 8_000.0;
        assert!(delta.sqrt() > 0.005);
    }

    #[test]
    fn sag_recovers_after_a_loud_burst() {
        let mut amp = Amp::new(48_000.0);
        amp.configure(AmpModel::Mandarin, 7.0, 5.0, 5.0, 5.0, 5.0, 8.0);
        amp.reset();
        for n in 0..8_000 {
            let x = (n as f32 * 0.07).sin() * 0.9;
            let _ = amp.process(x, x);
        }
        let measure = |amp: &mut Amp, start: usize| {
            let mut energy = 0.0;
            for n in start..(start + 2_000) {
                let x = (n as f32 * 0.07).sin() * 0.08;
                energy += amp.process(x, x).0.powi(2);
            }
            (energy / 2_000.0).sqrt()
        };
        let depressed = measure(&mut amp, 8_000);
        for _ in 0..24_000 {
            let _ = amp.process(0.0, 0.0);
        }
        let recovered = measure(&mut amp, 34_000);
        assert!(
            recovered > depressed * 1.01,
            "sag did not recover: {depressed} -> {recovered}"
        );
    }

    #[test]
    fn stable_at_required_rates_and_block_groupings() {
        for &sr in &[44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            for &block in &[1usize, 16, 64, 128, 512, 2_048] {
                let mut amp = Amp::new(sr);
                amp.configure(AmpModel::Slate, 9.0, 5.0, 5.0, 6.0, 6.0, 6.0);
                amp.reset();
                let mut peak = 0.0f32;
                let mut n = 0usize;
                while n < 4_096 {
                    for _ in 0..block.min(4_096 - n) {
                        let x = (n as f32 * 220.0 * std::f32::consts::TAU / sr).sin() * 0.7;
                        let (l, r) = amp.process(x, -x);
                        assert!(l.is_finite() && r.is_finite(), "sr={sr} block={block}");
                        peak = peak.max(l.abs()).max(r.abs());
                        n += 1;
                    }
                }
                assert!(peak < 4.0, "sr={sr} block={block} peak={peak}");
            }
        }
    }

    #[test]
    fn model_switch_is_click_bounded_and_instances_are_isolated() {
        let mut switched = Amp::new(48_000.0);
        let mut untouched = switched.clone();
        let mut control = switched.clone();
        let mut previous = 0.0;
        let mut max_step = 0.0f32;
        for n in 0..24_000 {
            if n == 8_000 {
                switched.configure(AmpModel::Recto, 8.0, 5.0, 4.0, 6.0, 6.0, 5.0);
            }
            let x = (n as f32 * 0.045).sin() * 0.3;
            let y = switched.process(x, x).0;
            let a = untouched.process(x, x).0;
            let b = control.process(x, x).0;
            assert!((a - b).abs() < 1.0e-7, "instance state leaked");
            if n > 100 {
                max_step = max_step.max((y - previous).abs());
            }
            previous = y;
        }
        assert!(max_step < 0.35, "model switch clicked: {max_step}");
    }
}
