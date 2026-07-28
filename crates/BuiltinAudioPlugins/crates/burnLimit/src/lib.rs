//! BurnLimit — modern brickwall limiter (Pro-L / Ozone–inspired).
//!
//! Lookahead peak limiting with style curves. True Peak adds a fixed ISP-style
//! headroom margin (not full oversampled inter-sample peak detection).

use builtin_dsp_core::{
    ParamDescriptor, PluginCategory, PluginDescriptor, StereoEffect, clamp, db_to_linear,
    linear_to_db, mix, time_constant,
};
use serde::{Deserialize, Serialize};

pub mod ipc;
pub mod ui;

pub use ipc::{UI_PARAM_IDS, ui_param_id, ui_param_index};

pub const PLUGIN_ID: &str = "futureboard.burnlimit";

const CLIP_THRESHOLD: f32 = 1.0;
const RMS_WINDOW_SECONDS: f32 = 0.300;
const PEAK_FALL_SECONDS: f32 = 0.400;

/// Maximum lookahead buffer. 10 ms at 96 kHz is 960 samples; pad for safety.
const MAX_LOOKAHEAD_SAMPLES: usize = 1_024;

/// Extra headroom when True Peak is engaged (approximation of ISP margin).
const TRUE_PEAK_MARGIN_DB: f32 = 1.0;

/// Limiter character. Wire order is the persisted contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    Clean,
    Punch,
    Modern,
    Clip,
}

impl Style {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Punch => "punch",
            Self::Modern => "modern",
            Self::Clip => "clip",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "clean" => Some(Self::Clean),
            "punch" => Some(Self::Punch),
            "modern" => Some(Self::Modern),
            "clip" => Some(Self::Clip),
            _ => None,
        }
    }

    pub const fn to_wire(self) -> f32 {
        match self {
            Self::Clean => 0.0,
            Self::Punch => 1.0,
            Self::Modern => 2.0,
            Self::Clip => 3.0,
        }
    }

    pub fn from_wire(value: f32) -> Self {
        match value.round() as i32 {
            1 => Self::Punch,
            2 => Self::Modern,
            3 => Self::Clip,
            _ => Self::Clean,
        }
    }

    /// Soft-knee width in dB — Clean is softest, Clip is nearly hard.
    pub fn knee_db(self) -> f32 {
        match self {
            Self::Clean => 4.0,
            Self::Punch => 2.0,
            Self::Modern => 1.5,
            Self::Clip => 0.2,
        }
    }

    /// Attack seconds layered under the user's release / lookahead.
    pub fn attack_sec(self) -> f32 {
        match self {
            Self::Clean => 0.002,
            Self::Punch => 0.0008,
            Self::Modern => 0.0004,
            Self::Clip => 0.00005,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterFrame {
    pub in_peak: f32,
    pub in_rms: f32,
    pub out_peak: f32,
    pub out_rms: f32,
    pub gain_reduction_db: f32,
    pub in_clip: bool,
    pub out_clip: bool,
}

#[derive(Debug, Clone)]
struct Meters {
    in_peak: f32,
    out_peak: f32,
    in_ms: f32,
    out_ms: f32,
    rms_coeff: f32,
    peak_coeff: f32,
    in_clip: bool,
    out_clip: bool,
}

impl Meters {
    fn new(sample_rate: f32) -> Self {
        Self {
            in_peak: 0.0,
            out_peak: 0.0,
            in_ms: 0.0,
            out_ms: 0.0,
            rms_coeff: time_constant(sample_rate, RMS_WINDOW_SECONDS),
            peak_coeff: time_constant(sample_rate, PEAK_FALL_SECONDS),
            in_clip: false,
            out_clip: false,
        }
    }

    fn reset(&mut self) {
        self.in_peak = 0.0;
        self.out_peak = 0.0;
        self.in_ms = 0.0;
        self.out_ms = 0.0;
        self.in_clip = false;
        self.out_clip = false;
    }

    #[inline]
    fn push(&mut self, input: f32, output: f32) {
        let in_abs = input.abs();
        let out_abs = output.abs();

        self.in_peak = if in_abs > self.in_peak {
            in_abs
        } else {
            self.in_peak * self.peak_coeff
        };
        self.out_peak = if out_abs > self.out_peak {
            out_abs
        } else {
            self.out_peak * self.peak_coeff
        };

        self.in_ms = self.rms_coeff * self.in_ms + (1.0 - self.rms_coeff) * input * input;
        self.out_ms = self.rms_coeff * self.out_ms + (1.0 - self.rms_coeff) * output * output;

        if in_abs >= CLIP_THRESHOLD {
            self.in_clip = true;
        }
        if out_abs >= CLIP_THRESHOLD {
            self.out_clip = true;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub power: bool,
    pub style: Style,
    pub gain_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    pub lookahead_ms: f32,
    pub true_peak: bool,
    pub mix: f32,
    pub stereo_link: bool,
}

pub fn default_params() -> Params {
    Params {
        power: true,
        style: Style::Modern,
        gain_db: 0.0,
        ceiling_db: -0.3,
        release_ms: 200.0,
        lookahead_ms: 2.0,
        true_peak: true,
        mix: 100.0,
        stereo_link: true,
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PLUGIN_ID,
        name: "BurnLimit",
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
                id: "style",
                name: "Style",
                default_value: 2.0,
                min: 0.0,
                max: 3.0,
                unit: "enum",
            },
            ParamDescriptor {
                id: "gainDb",
                name: "Gain",
                default_value: 0.0,
                min: -12.0,
                max: 24.0,
                unit: "dB",
            },
            ParamDescriptor {
                id: "ceilingDb",
                name: "Ceiling",
                default_value: -0.3,
                min: -6.0,
                max: 0.0,
                unit: "dB",
            },
            ParamDescriptor {
                id: "releaseMs",
                name: "Release",
                default_value: 200.0,
                min: 20.0,
                max: 2_000.0,
                unit: "ms",
            },
            ParamDescriptor {
                id: "lookaheadMs",
                name: "Lookahead",
                default_value: 2.0,
                min: 0.0,
                max: 10.0,
                unit: "ms",
            },
            ParamDescriptor {
                id: "truePeak",
                name: "True Peak",
                default_value: 1.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
            ParamDescriptor {
                id: "mix",
                name: "Mix",
                default_value: 100.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "stereoLink",
                name: "Link",
                default_value: 1.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
        ],
    }
}

#[derive(Debug, Clone)]
struct LookaheadLine {
    left: [f32; MAX_LOOKAHEAD_SAMPLES],
    right: [f32; MAX_LOOKAHEAD_SAMPLES],
    write: usize,
    delay: usize,
}

impl LookaheadLine {
    fn new() -> Self {
        Self {
            left: [0.0; MAX_LOOKAHEAD_SAMPLES],
            right: [0.0; MAX_LOOKAHEAD_SAMPLES],
            write: 0,
            delay: 0,
        }
    }

    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
    }

    fn set_delay(&mut self, delay: usize) {
        let delay = delay.min(MAX_LOOKAHEAD_SAMPLES - 1);
        if delay != self.delay {
            self.reset();
            self.delay = delay;
        }
    }

    #[inline]
    fn push_read(&mut self, left: f32, right: f32) -> (f32, f32) {
        if self.delay == 0 {
            return (left, right);
        }
        let read = (self.write + MAX_LOOKAHEAD_SAMPLES - self.delay) % MAX_LOOKAHEAD_SAMPLES;
        let out_l = self.left[read];
        let out_r = self.right[read];
        self.left[self.write] = left;
        self.right[self.write] = right;
        self.write += 1;
        if self.write >= MAX_LOOKAHEAD_SAMPLES {
            self.write = 0;
        }
        (out_l, out_r)
    }
}

#[derive(Debug, Clone)]
pub struct Dsp {
    params: Params,
    sample_rate: f32,
    input_gain: f32,
    ceiling_linear: f32,
    attack_coeff: f32,
    release_coeff: f32,
    knee_db: f32,
    envelope_l: f32,
    envelope_r: f32,
    gr_db: f32,
    delay: LookaheadLine,
    meters: Meters,
}

impl Dsp {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut dsp = Self {
            params: default_params(),
            sample_rate: sr,
            input_gain: 1.0,
            ceiling_linear: 1.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            knee_db: 1.5,
            envelope_l: 0.0,
            envelope_r: 0.0,
            gr_db: 0.0,
            delay: LookaheadLine::new(),
            meters: Meters::new(sr),
        };
        dsp.apply_params();
        dsp
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_db.max(0.0)
    }

    pub fn set_params(&mut self, params: Params) {
        self.params = params;
        ipc::sanitize_params(&mut self.params);
        self.apply_params();
    }

    pub fn meter_frame(&self) -> MeterFrame {
        MeterFrame {
            in_peak: self.meters.in_peak,
            in_rms: self.meters.in_ms.max(0.0).sqrt(),
            out_peak: self.meters.out_peak,
            out_rms: self.meters.out_ms.max(0.0).sqrt(),
            gain_reduction_db: if self.params.power {
                self.gr_db.max(0.0)
            } else {
                0.0
            },
            in_clip: self.meters.in_clip,
            out_clip: self.meters.out_clip,
        }
    }

    pub fn clear_clip(&mut self) {
        self.meters.in_clip = false;
        self.meters.out_clip = false;
    }

    pub fn apply_wire_param(&mut self, wire_index: u32, value: f32) -> bool {
        if !ipc::apply_wire_param(&mut self.params, wire_index, value) {
            return false;
        }
        self.apply_params();
        true
    }

    pub fn apply_ui_param(&mut self, id: &str, value: f32) -> bool {
        match ipc::ui_param_index(id) {
            Some(index) => self.apply_wire_param(index, value),
            None => false,
        }
    }

    pub fn latency_samples(&self) -> usize {
        self.delay.delay
    }

    fn apply_params(&mut self) {
        let style = self.params.style;
        self.knee_db = style.knee_db();
        self.attack_coeff = time_constant(self.sample_rate, style.attack_sec());
        self.release_coeff = time_constant(self.sample_rate, self.params.release_ms * 0.001);
        self.input_gain = db_to_linear(self.params.gain_db);

        let mut ceiling_db = self.params.ceiling_db;
        if self.params.true_peak {
            ceiling_db -= TRUE_PEAK_MARGIN_DB;
        }
        self.ceiling_linear = db_to_linear(ceiling_db);

        let delay = ((self.params.lookahead_ms * 0.001) * self.sample_rate).round() as usize;
        self.delay.set_delay(delay.min(MAX_LOOKAHEAD_SAMPLES - 1));
    }

    /// Gain computer: how much linear gain keeps `level` at or under the ceiling.
    #[inline]
    fn compute_gain(&self, level: f32) -> f32 {
        if level <= 1.0e-12 {
            return 1.0;
        }
        let ceiling = self.ceiling_linear.max(1.0e-6);
        let over_db = linear_to_db(level) - linear_to_db(ceiling);
        if over_db <= 0.0 {
            return 1.0;
        }
        let half_knee = self.knee_db * 0.5;
        let gr_db = if over_db >= half_knee {
            // Brickwall: take everything above the ceiling off.
            over_db
        } else {
            // Soft approach into the brickwall.
            let t = over_db + half_knee;
            (t * t) / (2.0 * self.knee_db.max(1.0e-6))
        };
        db_to_linear(-gr_db)
    }

    #[inline]
    fn smooth_envelope(envelope: &mut f32, target: f32, attack: f32, release: f32) {
        let coeff = if target > *envelope { attack } else { release };
        *envelope = coeff * *envelope + (1.0 - coeff) * target;
    }
}

impl StereoEffect for Dsp {
    fn reset(&mut self) {
        self.envelope_l = 0.0;
        self.envelope_r = 0.0;
        self.gr_db = 0.0;
        self.delay.reset();
        self.meters.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.meters = Meters::new(self.sample_rate);
        self.apply_params();
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        let in_sum = (left + right) * 0.5;
        if !self.params.power {
            self.meters.push(in_sum, in_sum);
            self.gr_db = 0.0;
            return (left, right);
        }

        let driven_l = left * self.input_gain;
        let driven_r = right * self.input_gain;

        // Detect on the undelayed path so GR can start before the peak exits.
        let peak_l = driven_l.abs();
        let peak_r = driven_r.abs();
        if self.params.stereo_link {
            let linked = peak_l.max(peak_r);
            Self::smooth_envelope(
                &mut self.envelope_l,
                linked,
                self.attack_coeff,
                self.release_coeff,
            );
            self.envelope_r = self.envelope_l;
        } else {
            Self::smooth_envelope(
                &mut self.envelope_l,
                peak_l,
                self.attack_coeff,
                self.release_coeff,
            );
            Self::smooth_envelope(
                &mut self.envelope_r,
                peak_r,
                self.attack_coeff,
                self.release_coeff,
            );
        }

        let gain_l = self.compute_gain(self.envelope_l);
        let gain_r = self.compute_gain(self.envelope_r);
        let gain = if self.params.stereo_link {
            gain_l.min(gain_r)
        } else {
            // Still report the deeper of the two for the GR meter.
            gain_l.min(gain_r)
        };
        self.gr_db = -linear_to_db(gain.max(1.0e-12));

        let (delayed_l, delayed_r) = self.delay.push_read(driven_l, driven_r);
        let wet_l = delayed_l * if self.params.stereo_link { gain } else { gain_l };
        let wet_r = delayed_r * if self.params.stereo_link { gain } else { gain_r };

        let amount = self.params.mix / 100.0;
        let out_l = mix(left, wet_l, amount);
        let out_r = mix(right, wet_r, amount);
        self.meters.push(in_sum, (out_l + out_r) * 0.5);
        (out_l, out_r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_tone(dsp: &mut Dsp, amplitude: f32, samples: usize) -> MeterFrame {
        for n in 0..samples {
            let x = (n as f32 * 0.05).sin() * amplitude;
            let (l, r) = dsp.process_stereo(x, x);
            assert!(l.is_finite() && r.is_finite());
        }
        dsp.meter_frame()
    }

    #[test]
    fn descriptor_ids_are_unique_and_match_defaults() {
        let d = descriptor();
        assert_eq!(d.id, PLUGIN_ID);

        let mut ids: Vec<_> = d.params.iter().map(|p| p.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(count, ids.len());

        let defaults = ipc::ui_values(&default_params());
        for param in d.params {
            let (_, actual) = defaults
                .iter()
                .find(|(id, _)| *id == param.id)
                .copied()
                .unwrap_or_else(|| panic!("`{}` is missing from ui_values", param.id));
            assert!(
                (actual - param.default_value).abs() < 1.0e-5,
                "{} default drifted",
                param.id
            );
        }
    }

    #[test]
    fn hot_signal_is_held_under_ceiling() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.gain_db = 12.0;
        params.ceiling_db = -1.0;
        params.true_peak = false;
        params.style = Style::Clip;
        params.lookahead_ms = 3.0;
        params.release_ms = 50.0;
        dsp.set_params(params);

        let mut peak = 0.0f32;
        for _ in 0..8_000 {
            let (l, r) = dsp.process_stereo(0.9, 0.9);
            peak = peak.max(l.abs()).max(r.abs());
        }
        let ceiling = db_to_linear(-1.0);
        assert!(
            peak <= ceiling * 1.05,
            "peak {peak} exceeded ceiling {ceiling}"
        );
        assert!(dsp.gain_reduction_db() > 1.0);
    }

    #[test]
    fn lookahead_reports_latency() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.lookahead_ms = 5.0;
        dsp.set_params(params);
        assert_eq!(dsp.latency_samples(), 240);
    }

    #[test]
    fn power_off_reports_zero_reduction() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.gain_db = 18.0;
        dsp.set_params(params);
        let _ = run_tone(&mut dsp, 0.9, 1_000);
        assert!(dsp.apply_ui_param("power", 0.0));
        let frame = run_tone(&mut dsp, 0.9, 256);
        assert_eq!(frame.gain_reduction_db, 0.0);
    }
}
