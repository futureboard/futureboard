//! VerbSpace — feedback-delay-network algorithmic reverb.
//!
//! Engine-agnostic like the other `BuiltinAudioPlugins` cores. The signal path
//! is
//!
//! ```txt
//! pre-delay -> input diffusion (4 allpass/channel) -> 8-line FDN tank
//!   (damping + bass multiplier + modulated taps, Householder feedback)
//!   -> low cut / high cut -> width -> mix -> output trim
//! ```
//!
//! Every buffer is sized at construction for the widest reachable delay, so a
//! parameter edit only moves read offsets — no realtime allocation. Filter
//! coefficients come from the MIT/Apache [`biquad`] crate.

use biquad::{Biquad, DirectForm1};
use builtin_dsp_core::{
    ParamDescriptor, PluginCategory, PluginDescriptor, StereoEffect, clamp, db_to_linear,
    make_eq_biquad, mix,
};
use serde::{Deserialize, Serialize};

pub mod ipc;
pub mod ui;

/// Editor-facing parameter id table, re-exported at the crate root so the host
/// resolves ids the same way for every built-in (`<plugin>::ui_param_index`).
pub use ipc::{UI_PARAM_IDS, ui_param_id, ui_param_index};

pub const PLUGIN_ID: &str = "futureboard.verbspace";

/// Delay lines in the tank. Eight is the smallest count where the Householder
/// reflection below produces a tail dense enough for a hall without audible
/// flutter, and it keeps the per-sample cost at 8 reads + 8 writes.
pub const LINE_COUNT: usize = 8;

/// Allpass diffusers per channel ahead of the tank.
pub const DIFFUSER_COUNT: usize = 4;

/// Longest pre-delay the ring is sized for.
pub const MAX_PREDELAY_MS: f32 = 500.0;

/// Longest tank line reachable through `size` × `mode` scaling, plus headroom
/// for the modulation excursion. Sizing here rather than per-edit is what keeps
/// [`Dsp::apply_wire_param`] allocation-free.
const MAX_LINE_MS: f32 = 110.0;

/// Nominal line lengths in milliseconds at size 100 % in `Hall`. Mutually
/// prime-ish so the modal density does not collapse into a comb.
const BASE_LINE_MS: [f32; LINE_COUNT] = [26.1, 31.7, 38.9, 44.3, 52.7, 59.9, 67.1, 73.7];

/// Fixed diffuser lengths in milliseconds, offset between channels so the two
/// input paths decorrelate before they reach the shared tank.
const DIFFUSER_MS_L: [f32; DIFFUSER_COUNT] = [4.77, 7.31, 10.13, 14.71];
const DIFFUSER_MS_R: [f32; DIFFUSER_COUNT] = [5.19, 8.03, 11.27, 15.83];

/// Peak modulation excursion in milliseconds at `mod_depth` 100 %.
const MAX_MOD_MS: f32 = 0.25;

/// Crossover between the damped band and the band `bass_mult` rescales.
const BASS_SPLIT_HZ: f32 = 400.0;

/// Feedback ceiling. An FDN with a unit-gain feedback matrix is only marginally
/// stable at exactly 1.0, so every gain — including `freeze` — stays below it.
const MAX_FEEDBACK: f32 = 0.9995;

/// Reverb character. Selects the tank's length scaling and how much diffusion
/// the mode adds on top of the user's `diffusion` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReverbMode {
    Room,
    Chamber,
    Hall,
    Plate,
    Ambience,
}

impl ReverbMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::Chamber => "chamber",
            Self::Hall => "hall",
            Self::Plate => "plate",
            Self::Ambience => "ambience",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "room" => Some(Self::Room),
            "chamber" => Some(Self::Chamber),
            "hall" => Some(Self::Hall),
            "plate" => Some(Self::Plate),
            "ambience" | "ambient" => Some(Self::Ambience),
            _ => None,
        }
    }

    pub const fn to_wire(self) -> f32 {
        match self {
            Self::Room => 0.0,
            Self::Chamber => 1.0,
            Self::Hall => 2.0,
            Self::Plate => 3.0,
            Self::Ambience => 4.0,
        }
    }

    pub fn from_wire(value: f32) -> Self {
        match value.round() as i32 {
            0 => Self::Room,
            1 => Self::Chamber,
            3 => Self::Plate,
            4 => Self::Ambience,
            _ => Self::Hall,
        }
    }

    /// Multiplier on [`BASE_LINE_MS`]. Short and dense for `Plate`, long and
    /// sparse for `Hall`.
    pub const fn line_scale(self) -> f32 {
        match self {
            Self::Room => 0.62,
            Self::Chamber => 0.78,
            Self::Hall => 1.0,
            Self::Plate => 0.42,
            Self::Ambience => 0.30,
        }
    }

    /// Diffusion the mode contributes regardless of the user's setting, as a
    /// floor the `diffusion` control scales up from.
    pub const fn diffusion_bias(self) -> f32 {
        match self {
            Self::Room => 0.10,
            Self::Chamber => 0.18,
            Self::Hall => 0.22,
            Self::Plate => 0.45,
            Self::Ambience => 0.06,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub power: bool,
    pub mode: ReverbMode,
    /// Silence before the tank is fed, in milliseconds.
    pub predelay_ms: f32,
    /// Tank line-length scaling, in percent.
    pub size: f32,
    /// RT60 in seconds.
    pub decay_sec: f32,
    /// Input allpass coefficient, in percent.
    pub diffusion: f32,
    /// High-frequency absorption inside the tank, in percent.
    pub damping: f32,
    /// Decay multiplier below [`BASS_SPLIT_HZ`].
    pub bass_mult: f32,
    /// Tank tap modulation depth, in percent of [`MAX_MOD_MS`].
    pub mod_depth: f32,
    /// Tank tap modulation rate, in hertz.
    pub mod_rate_hz: f32,
    /// Wet-path high-pass corner, in hertz.
    pub low_cut_hz: f32,
    /// Wet-path low-pass corner, in hertz.
    pub high_cut_hz: f32,
    /// Wet-path stereo width, in percent (100 = unchanged).
    pub width: f32,
    /// Dry/wet blend, in percent.
    pub mix: f32,
    /// Wet-path trim, in decibels.
    pub output_db: f32,
    /// Hold the tail: feedback pinned, input muted, damping bypassed.
    pub freeze: bool,
}

pub fn default_params() -> Params {
    Params {
        power: true,
        mode: ReverbMode::Hall,
        predelay_ms: 20.0,
        size: 60.0,
        decay_sec: 2.4,
        diffusion: 72.0,
        damping: 45.0,
        bass_mult: 1.1,
        mod_depth: 25.0,
        mod_rate_hz: 0.6,
        low_cut_hz: 90.0,
        high_cut_hz: 9_500.0,
        width: 110.0,
        mix: 28.0,
        output_db: 0.0,
        freeze: false,
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PLUGIN_ID,
        name: "VerbSpace",
        vendor: "Futureboard",
        category: PluginCategory::Effect,
        version: env!("CARGO_PKG_VERSION"),
        params: &[
            ParamDescriptor {
                id: "power",
                name: "Power",
                default_value: 1.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
            ParamDescriptor {
                id: "mode",
                name: "Mode",
                default_value: 2.0,
                min: 0.0,
                max: 4.0,
                unit: "enum",
            },
            ParamDescriptor {
                id: "predelayMs",
                name: "Pre-Delay",
                default_value: 20.0,
                min: 0.0,
                max: MAX_PREDELAY_MS,
                unit: "ms",
            },
            ParamDescriptor {
                id: "size",
                name: "Size",
                default_value: 60.0,
                min: 10.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "decaySec",
                name: "Decay",
                default_value: 2.4,
                min: 0.1,
                max: 20.0,
                unit: "s",
            },
            ParamDescriptor {
                id: "diffusion",
                name: "Diffusion",
                default_value: 72.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "damping",
                name: "Damping",
                default_value: 45.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "bassMult",
                name: "Bass",
                default_value: 1.1,
                min: 0.2,
                max: 2.0,
                unit: "x",
            },
            ParamDescriptor {
                id: "modDepth",
                name: "Mod Depth",
                default_value: 25.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "modRateHz",
                name: "Mod Rate",
                default_value: 0.6,
                min: 0.05,
                max: 5.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "lowCutHz",
                name: "Low Cut",
                default_value: 90.0,
                min: 20.0,
                max: 1_000.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "highCutHz",
                name: "High Cut",
                default_value: 9_500.0,
                min: 1_000.0,
                max: 20_000.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "width",
                name: "Width",
                default_value: 110.0,
                min: 0.0,
                max: 200.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "mix",
                name: "Mix",
                default_value: 28.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "outputDb",
                name: "Output",
                default_value: 0.0,
                min: -24.0,
                max: 12.0,
                unit: "dB",
            },
            ParamDescriptor {
                id: "freeze",
                name: "Freeze",
                default_value: 0.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
        ],
    }
}

/// Fixed-length Schroeder allpass. `buffer.len()` *is* the delay, so there is no
/// read offset to validate on the hot path.
#[derive(Debug, Clone)]
struct Allpass {
    buffer: Vec<f32>,
    write: usize,
}

impl Allpass {
    fn new(length: usize) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            write: 0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    #[inline]
    fn process(&mut self, input: f32, gain: f32) -> f32 {
        let delayed = self.buffer[self.write];
        let stored = input + delayed * gain;
        let output = delayed - stored * gain;
        self.buffer[self.write] = stored;
        self.write += 1;
        if self.write >= self.buffer.len() {
            self.write = 0;
        }
        output
    }
}

/// One tank line: a ring read at a fractional, modulated offset, plus the two
/// one-pole states that shape its feedback.
#[derive(Debug, Clone)]
struct TankLine {
    buffer: Vec<f32>,
    write: usize,
    damp_state: f32,
    bass_state: f32,
}

impl TankLine {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0.0; capacity.max(4)],
            write: 0,
            damp_state: 0.0,
            bass_state: 0.0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
        self.damp_state = 0.0;
        self.bass_state = 0.0;
    }

    /// Linear-interpolated read `delay` samples behind the write head. `delay`
    /// is clamped into the ring, so a stale offset can never index out of
    /// bounds after a sample-rate change.
    #[inline]
    fn read(&self, delay: f32) -> f32 {
        let len = self.buffer.len();
        let max = (len - 2) as f32;
        let delay = if delay.is_finite() {
            clamp(delay, 1.0, max)
        } else {
            1.0
        };
        let whole = delay.floor();
        let frac = delay - whole;
        let newer = (self.write + len - whole as usize) % len;
        let older = (newer + len - 1) % len;
        // The integer tap is the newer endpoint. The fractional part walks
        // backward toward the sample one frame older.
        self.buffer[newer] * (1.0 - frac) + self.buffer[older] * frac
    }

    #[inline]
    fn write_sample(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write += 1;
        if self.write >= self.buffer.len() {
            self.write = 0;
        }
    }
}

/// Integer-tap pre-delay ring shared by both channels' offsets.
#[derive(Debug, Clone)]
struct PreDelay {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
}

impl PreDelay {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        Self {
            left: vec![0.0; capacity],
            right: vec![0.0; capacity],
            write: 0,
        }
    }

    fn clear(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
    }

    #[inline]
    fn process(&mut self, left: f32, right: f32, delay: usize) -> (f32, f32) {
        let len = self.left.len();
        let delay = delay.min(len - 1);
        let out = if delay == 0 {
            (left, right)
        } else {
            let read = (self.write + len - delay) % len;
            (self.left[read], self.right[read])
        };
        self.left[self.write] = left;
        self.right[self.write] = right;
        self.write += 1;
        if self.write >= len {
            self.write = 0;
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct Dsp {
    sample_rate: f32,
    params: Params,

    predelay: PreDelay,
    diffusers_l: [Allpass; DIFFUSER_COUNT],
    diffusers_r: [Allpass; DIFFUSER_COUNT],
    lines: [TankLine; LINE_COUNT],

    // Resolved from `params` by `apply_params_scalars`; the hot path reads only
    // these, never `params` (except the two plain flags below).
    predelay_samples: usize,
    line_delay: [f32; LINE_COUNT],
    /// Per-pass gain above [`BASS_SPLIT_HZ`].
    line_feedback: [f32; LINE_COUNT],
    /// Per-pass gain below [`BASS_SPLIT_HZ`], derived from `bass_mult` as a
    /// *decay-time* multiplier — see [`Dsp::apply_params_scalars`].
    line_feedback_low: [f32; LINE_COUNT],
    diffusion_gain: f32,
    damp_coeff: f32,
    bass_coeff: f32,
    mod_depth_samples: f32,
    mod_increment: f32,
    input_gain: f32,
    output_gain: f32,
    mid_gain: f32,
    side_gain: f32,

    mod_phase: [f32; LINE_COUNT],
    low_cut_l: Option<DirectForm1<f32>>,
    low_cut_r: Option<DirectForm1<f32>>,
    high_cut_l: Option<DirectForm1<f32>>,
    high_cut_r: Option<DirectForm1<f32>>,
}

/// Starting LFO phases, spread evenly so the tank never sweeps in unison.
const MOD_PHASES: [f32; LINE_COUNT] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875];

impl Dsp {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut dsp = Self {
            sample_rate: sr,
            params: default_params(),
            predelay: PreDelay::new(Self::predelay_capacity(sr)),
            diffusers_l: Self::build_diffusers(sr, &DIFFUSER_MS_L),
            diffusers_r: Self::build_diffusers(sr, &DIFFUSER_MS_R),
            lines: Self::build_lines(sr),
            predelay_samples: 0,
            line_delay: [1.0; LINE_COUNT],
            line_feedback: [0.0; LINE_COUNT],
            line_feedback_low: [0.0; LINE_COUNT],
            diffusion_gain: 0.0,
            damp_coeff: 0.0,
            bass_coeff: 0.0,
            mod_depth_samples: 0.0,
            mod_increment: 0.0,
            input_gain: 1.0,
            output_gain: 1.0,
            mid_gain: 1.0,
            side_gain: 1.0,
            mod_phase: MOD_PHASES,
            low_cut_l: None,
            low_cut_r: None,
            high_cut_l: None,
            high_cut_r: None,
        };
        dsp.apply_params();
        dsp
    }

    fn predelay_capacity(sample_rate: f32) -> usize {
        ((sample_rate * MAX_PREDELAY_MS * 0.001).ceil() as usize).max(2) + 2
    }

    fn build_diffusers(
        sample_rate: f32,
        lengths: &[f32; DIFFUSER_COUNT],
    ) -> [Allpass; DIFFUSER_COUNT] {
        std::array::from_fn(|i| {
            Allpass::new(((lengths[i] * 0.001 * sample_rate).round() as usize).max(1))
        })
    }

    fn build_lines(sample_rate: f32) -> [TankLine; LINE_COUNT] {
        let capacity = ((sample_rate * MAX_LINE_MS * 0.001).ceil() as usize).max(4) + 4;
        std::array::from_fn(|_| TankLine::new(capacity))
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn set_params(&mut self, params: Params) {
        self.params = params;
        ipc::sanitize_params(&mut self.params);
        self.apply_params();
    }

    /// Apply a compact wire update already resolved by the UI/control thread.
    ///
    /// The audio path never parses JSON or looks up string parameter ids. Every
    /// arm below only recomputes derived scalars or biquad coefficients, so this
    /// stays allocation-free and safe to call from the producer thread between
    /// blocks.
    pub fn apply_wire_param(&mut self, wire_index: u32, value: f32) -> bool {
        if !ipc::apply_wire_param(&mut self.params, wire_index, value) {
            return false;
        }
        match wire_index {
            ipc::LOW_CUT_INDEX | ipc::HIGH_CUT_INDEX => self.rebuild_filters(),
            // Everything else feeds the resolved scalar table, which is a few
            // dozen flops — cheaper than fanning out a branch per index.
            _ => self.apply_params_scalars(),
        }
        true
    }

    /// Resolve a string id off the realtime path (project restore, tests).
    pub fn apply_ui_param(&mut self, id: &str, value: f32) -> bool {
        match ipc::ui_param_index(id) {
            Some(index) => self.apply_wire_param(index, value),
            None => false,
        }
    }

    /// VerbSpace introduces no lookahead: the pre-delay is a musical parameter,
    /// not a processing latency the graph should compensate for.
    pub fn latency_samples(&self) -> usize {
        0
    }

    fn apply_params(&mut self) {
        self.apply_params_scalars();
        self.rebuild_filters();
    }

    fn apply_params_scalars(&mut self) {
        let p = &self.params;
        self.predelay_samples = ((p.predelay_ms * 0.001 * self.sample_rate).round() as usize)
            .min(self.predelay.left.len() - 1);

        let scale = p.mode.line_scale() * (0.35 + (p.size / 100.0) * 0.85);
        let max_delay = (self.lines[0].buffer.len() - 3) as f32;
        // `bass_mult` stretches the low band's RT60, which is a *shorter* per
        // pass loss — never extra loop gain. Multiplying the feedback sample
        // instead would put the low band above unity for any multiplier > 1 and
        // the tank would build without bound.
        let bass_mult = clamp(p.bass_mult, 0.2, 2.0);
        for i in 0..LINE_COUNT {
            let delay = clamp(
                BASE_LINE_MS[i] * scale * 0.001 * self.sample_rate,
                2.0,
                max_delay,
            );
            self.line_delay[i] = delay;
            // RT60: the line must lose 60 dB after `decay_sec`, so one pass
            // costs `-3 * t_line / rt60` decades of amplitude.
            let seconds = delay / self.sample_rate;
            let (gain, gain_low) = if p.freeze {
                (MAX_FEEDBACK, MAX_FEEDBACK)
            } else {
                let decades = -3.0 * seconds / p.decay_sec.max(0.05);
                (10.0f32.powf(decades), 10.0f32.powf(decades / bass_mult))
            };
            self.line_feedback[i] = clamp(gain, 0.0, MAX_FEEDBACK);
            self.line_feedback_low[i] = clamp(gain_low, 0.0, MAX_FEEDBACK);
        }

        let diffusion = clamp(p.diffusion / 100.0, 0.0, 1.0);
        let bias = p.mode.diffusion_bias();
        self.diffusion_gain = clamp(bias + diffusion * (0.78 - bias), 0.0, 0.78);

        // Damping is bypassed while frozen: an absorbing tank would still bleed
        // the top off a tail that is supposed to hold indefinitely.
        let damping = if p.freeze {
            0.0
        } else {
            clamp(p.damping / 100.0, 0.0, 1.0)
        };
        self.damp_coeff = damping * 0.88;

        // One-pole split at `BASS_SPLIT_HZ`, used to give the two bands their
        // own feedback gains.
        let omega = std::f32::consts::TAU * BASS_SPLIT_HZ / self.sample_rate;
        self.bass_coeff = clamp(1.0 - (-omega).exp(), 0.0, 1.0);

        self.mod_depth_samples =
            clamp(p.mod_depth / 100.0, 0.0, 1.0) * MAX_MOD_MS * 0.001 * self.sample_rate;
        self.mod_increment = p.mod_rate_hz.max(0.0) / self.sample_rate;

        self.input_gain = if p.freeze { 0.0 } else { 1.0 };
        self.output_gain = db_to_linear(p.output_db);

        // Width as a mid/side trim. The two gains sum in power, so 0 % and
        // 200 % stay in the same loudness neighbourhood as 100 %.
        let width = clamp(p.width / 100.0, 0.0, 2.0);
        self.mid_gain = (2.0 - width).sqrt() * std::f32::consts::FRAC_1_SQRT_2;
        self.side_gain = width.sqrt() * std::f32::consts::FRAC_1_SQRT_2;
    }

    fn rebuild_filters(&mut self) {
        let nyquist_guard = self.sample_rate * 0.45;
        let hpf = make_eq_biquad(
            "highpass",
            clamp(self.params.low_cut_hz, 20.0, nyquist_guard),
            0.0,
            0.707,
            self.sample_rate,
        );
        self.low_cut_l = hpf;
        self.low_cut_r = hpf;
        let lpf = make_eq_biquad(
            "lowpass",
            clamp(self.params.high_cut_hz, 200.0, nyquist_guard),
            0.0,
            0.707,
            self.sample_rate,
        );
        self.high_cut_l = lpf;
        self.high_cut_r = lpf;
    }

    /// One line's feedback sample: damping low-pass, then the two-band split
    /// where each band carries its own RT60-derived gain. Both gains are below
    /// unity by construction, which is what bounds the tank.
    #[inline]
    fn shape_feedback(
        line: &mut TankLine,
        sample: f32,
        damp: f32,
        bass_coeff: f32,
        gain_high: f32,
        gain_low: f32,
    ) -> f32 {
        line.damp_state = sample * (1.0 - damp) + line.damp_state * damp;
        let damped = line.damp_state;
        line.bass_state += bass_coeff * (damped - line.bass_state);
        let low = line.bass_state;
        (damped - low) * gain_high + low * gain_low
    }
}

impl StereoEffect for Dsp {
    fn reset(&mut self) {
        self.predelay.clear();
        for ap in self.diffusers_l.iter_mut() {
            ap.clear();
        }
        for ap in self.diffusers_r.iter_mut() {
            ap.clear();
        }
        for line in self.lines.iter_mut() {
            line.clear();
        }
        self.mod_phase = MOD_PHASES;
        for filter in [
            self.low_cut_l.as_mut(),
            self.low_cut_r.as_mut(),
            self.high_cut_l.as_mut(),
            self.high_cut_r.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            filter.reset_state();
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        if (sr - self.sample_rate).abs() < f32::EPSILON {
            return;
        }
        self.sample_rate = sr;
        self.predelay = PreDelay::new(Self::predelay_capacity(sr));
        self.diffusers_l = Self::build_diffusers(sr, &DIFFUSER_MS_L);
        self.diffusers_r = Self::build_diffusers(sr, &DIFFUSER_MS_R);
        self.lines = Self::build_lines(sr);
        self.apply_params();
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        if !self.params.power {
            return (left, right);
        }

        let (pre_l, pre_r) = self.predelay.process(left, right, self.predelay_samples);

        let mut diff_l = pre_l * self.input_gain;
        let mut diff_r = pre_r * self.input_gain;
        for i in 0..DIFFUSER_COUNT {
            diff_l = self.diffusers_l[i].process(diff_l, self.diffusion_gain);
            diff_r = self.diffusers_r[i].process(diff_r, self.diffusion_gain);
        }

        // Read every line before any of them advances: the Householder
        // reflection below mixes the whole tank state at one instant.
        let mut taps = [0.0f32; LINE_COUNT];
        let mut sum = 0.0f32;
        for i in 0..LINE_COUNT {
            let phase = self.mod_phase[i];
            // Triangle LFO — cheap, and unlike a sine it has no stationary
            // point where the pitch modulation parks at an extreme.
            let tri = 1.0 - 4.0 * (phase - 0.5).abs();
            let tap = self.lines[i].read(self.line_delay[i] + tri * self.mod_depth_samples);
            taps[i] = tap;
            sum += tap;

            let mut next = phase + self.mod_increment;
            if next >= 1.0 {
                next -= 1.0;
            }
            self.mod_phase[i] = next;
        }

        // Householder reflection: y = x - (2/N) * Σx. Unitary, so the matrix
        // itself neither adds nor removes energy — decay comes only from the
        // per-line RT60 gains.
        let correction = sum * (2.0 / LINE_COUNT as f32);
        for i in 0..LINE_COUNT {
            let mixed = taps[i] - correction;
            let shaped = Self::shape_feedback(
                &mut self.lines[i],
                mixed,
                self.damp_coeff,
                self.bass_coeff,
                self.line_feedback[i],
                self.line_feedback_low[i],
            );
            let injected = if i % 2 == 0 { diff_l } else { diff_r } * 0.5;
            self.lines[i].write_sample(injected + shaped);
        }

        // Alternating tap polarity decorrelates the two output sums, so the
        // tail is genuinely stereo rather than a widened mono image.
        let mut wet_l = 0.0f32;
        let mut wet_r = 0.0f32;
        for i in 0..LINE_COUNT {
            let signed = if (i / 2) % 2 == 0 { taps[i] } else { -taps[i] };
            if i % 2 == 0 {
                wet_l += signed;
            } else {
                wet_r += signed;
            }
        }
        let norm = 1.0 / (LINE_COUNT as f32 / 2.0).sqrt();
        wet_l *= norm;
        wet_r *= norm;

        if let Some(f) = self.low_cut_l.as_mut() {
            wet_l = f.run(wet_l);
        }
        if let Some(f) = self.low_cut_r.as_mut() {
            wet_r = f.run(wet_r);
        }
        if let Some(f) = self.high_cut_l.as_mut() {
            wet_l = f.run(wet_l);
        }
        if let Some(f) = self.high_cut_r.as_mut() {
            wet_r = f.run(wet_r);
        }

        let mid = (wet_l + wet_r) * std::f32::consts::FRAC_1_SQRT_2 * self.mid_gain;
        let side = (wet_l - wet_r) * std::f32::consts::FRAC_1_SQRT_2 * self.side_gain;
        wet_l = (mid + side) * self.output_gain;
        wet_r = (mid - side) * self.output_gain;

        let amount = self.params.mix / 100.0;
        (mix(left, wet_l, amount), mix(right, wet_r, amount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_impulse(dsp: &mut Dsp, tail: usize) -> f32 {
        let (first, _) = dsp.process_stereo(1.0, 1.0);
        let mut peak = first.abs();
        for _ in 0..tail {
            let (l, r) = dsp.process_stereo(0.0, 0.0);
            assert!(l.is_finite() && r.is_finite());
            peak = peak.max(l.abs()).max(r.abs());
        }
        peak
    }

    #[test]
    fn descriptor_ids_are_unique_and_match_defaults() {
        let d = descriptor();
        assert_eq!(d.id, PLUGIN_ID);
        assert_eq!(d.category, PluginCategory::Effect);

        let mut ids: Vec<_> = d.params.iter().map(|p| p.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(count, ids.len(), "duplicate parameter id in descriptor");

        let defaults = ipc::ui_values(&default_params());
        for param in d.params {
            let (_, actual) = defaults
                .iter()
                .find(|(id, _)| *id == param.id)
                .copied()
                .unwrap_or_else(|| panic!("`{}` is missing from ui_values", param.id));
            assert!(
                (param.default_value - actual).abs() < 1.0e-6,
                "`{}`: descriptor says {}, default_params() says {actual}",
                param.id,
                param.default_value,
            );
            assert!(
                param.default_value >= param.min && param.default_value <= param.max,
                "`{}`: default {} is outside {}..{}",
                param.id,
                param.default_value,
                param.min,
                param.max,
            );
        }
    }

    #[test]
    fn bypass_when_power_off() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.power = false;
        dsp.set_params(params);
        assert_eq!(dsp.process_stereo(0.25, -0.25), (0.25, -0.25));
    }

    #[test]
    fn impulse_builds_a_tail_at_every_rate_and_mode() {
        for &sr in &[44_100.0f32, 48_000.0, 96_000.0] {
            for mode in [
                ReverbMode::Room,
                ReverbMode::Chamber,
                ReverbMode::Hall,
                ReverbMode::Plate,
                ReverbMode::Ambience,
            ] {
                let mut dsp = Dsp::new(sr);
                let mut params = default_params();
                params.mode = mode;
                params.mix = 100.0;
                dsp.set_params(params);
                let peak = run_impulse(&mut dsp, sr as usize / 2);
                assert!(peak > 1.0e-4, "{mode:?} @ {sr} produced no tail");
            }
        }
    }

    /// The tank is a feedback loop around a unit-gain mixing matrix; the
    /// longest decay at the largest size is where a coefficient slip shows up
    /// as a slow build to infinity rather than as a tail.
    #[test]
    fn longest_decay_stays_bounded() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.decay_sec = 20.0;
        params.size = 100.0;
        params.mode = ReverbMode::Hall;
        params.mix = 100.0;
        params.damping = 0.0;
        params.bass_mult = 2.0;
        dsp.set_params(params);

        let mut peak = 0.0f32;
        for n in 0..(48_000 * 10) {
            let x = if n < 4_800 {
                (n as f32 * 0.01).sin() * 0.7
            } else {
                0.0
            };
            let (l, r) = dsp.process_stereo(x, x);
            assert!(l.is_finite() && r.is_finite(), "non-finite at sample {n}");
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak < 8.0, "tank ran away: peak {peak}");
    }

    #[test]
    fn freeze_holds_the_tail_after_the_input_stops() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.mix = 100.0;
        params.decay_sec = 2.0;
        dsp.set_params(params);

        for n in 0..24_000 {
            let x = (n as f32 * 0.03).sin() * 0.5;
            let _ = dsp.process_stereo(x, x);
        }
        assert!(dsp.apply_ui_param("freeze", 1.0));

        let mut early = 0.0f32;
        for _ in 0..4_800 {
            let (l, r) = dsp.process_stereo(0.0, 0.0);
            early = early.max(l.abs()).max(r.abs());
        }
        let mut late = 0.0f32;
        for _ in 0..(48_000 * 4) {
            let (l, r) = dsp.process_stereo(0.0, 0.0);
            late = late.max(l.abs()).max(r.abs());
        }
        assert!(early > 1.0e-4, "nothing in the tank to freeze");
        assert!(
            late > early * 0.25,
            "freeze decayed away: early {early}, late {late}"
        );
    }

    #[test]
    fn reset_clears_the_tail() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.mix = 100.0;
        dsp.set_params(params);
        for _ in 0..4_800 {
            let _ = dsp.process_stereo(0.5, -0.5);
        }
        dsp.reset();
        let (l, r) = dsp.process_stereo(0.0, 0.0);
        assert!(l.abs() < 1.0e-6 && r.abs() < 1.0e-6);
    }

    #[test]
    fn sample_rate_change_keeps_delays_inside_the_new_rings() {
        let mut dsp = Dsp::new(96_000.0);
        let mut params = default_params();
        params.size = 100.0;
        params.mix = 100.0;
        dsp.set_params(params);
        dsp.set_sample_rate(44_100.0);
        let peak = run_impulse(&mut dsp, 44_100);
        assert!(peak.is_finite());
        let capacity = dsp.lines[0].buffer.len();
        for delay in dsp.line_delay {
            assert!(delay < (capacity - 2) as f32);
        }
    }

    #[test]
    fn wire_update_changes_only_authoritative_params() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::DECAY_INDEX, 6.0));
        assert_eq!(dsp.params().decay_sec, 6.0);
        assert!(!dsp.apply_wire_param(u32::MAX, 0.0));
        assert!(!dsp.apply_wire_param(ipc::DECAY_INDEX, f32::NAN));
    }

    #[test]
    fn zero_predelay_is_sample_accurate_and_does_not_wrap_the_ring() {
        let mut predelay = PreDelay::new(8);
        assert_eq!(predelay.process(0.75, -0.25, 0), (0.75, -0.25));
        for _ in 0..8 {
            assert_eq!(predelay.process(0.0, 0.0, 0), (0.0, 0.0));
        }
    }

    #[test]
    fn fractional_delay_interpolates_between_the_correct_samples() {
        let mut line = TankLine::new(16);
        for sample in 0..12 {
            line.write_sample(sample as f32);
        }
        // At this point the 2- and 3-sample-old values are 10 and 9.
        assert!((line.read(2.25) - 9.75).abs() < 1.0e-6);
    }
}
