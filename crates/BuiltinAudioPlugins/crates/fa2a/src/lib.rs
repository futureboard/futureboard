//! FA-2A — optical / program-dependent compressor (LA-2A-style).
//!
//! Phase 1 (easy). Parameter model mirrors `plugins/FB2AComp/Core`. Dynamics
//! use `SoftKneeCompressor` (sidechain HPF via [`biquad`]).

use builtin_dsp_core::{
    ParamDescriptor, PluginCategory, PluginDescriptor, SoftKneeCompressor, StereoEffect, clamp,
    db_to_linear, mix, time_constant,
};
use serde::{Deserialize, Serialize};

pub mod ipc;
pub mod ui;

/// Editor-facing parameter id table, re-exported at the crate root so the host
/// resolves ids the same way for every built-in (`<plugin>::ui_param_index`).
pub use ipc::{UI_PARAM_IDS, ui_param_id, ui_param_index};

pub const PLUGIN_ID: &str = "futureboard.fa2a";

/// Full scale. A sample at or beyond this latches the corresponding clip flag.
const CLIP_THRESHOLD: f32 = 1.0;

/// Integration window for the RMS meters. 300 ms is the VU standard, and the
/// editor draws a VU face — matching it here means the needle ballistics are
/// the meter's, not an arbitrary smoothing.
const RMS_WINDOW_SECONDS: f32 = 0.300;

/// Fall time for the peak meters. Rise is instantaneous.
const PEAK_FALL_SECONDS: f32 = 0.400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Compress,
    Limit,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compress => "compress",
            Self::Limit => "limit",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "compress" | "comp" => Some(Self::Compress),
            "limit" => Some(Self::Limit),
            _ => None,
        }
    }

    pub const fn to_wire(self) -> f32 {
        match self {
            Self::Compress => 0.0,
            Self::Limit => 1.0,
        }
    }

    pub fn from_wire(value: f32) -> Self {
        if value.round() as i32 == 1 {
            Self::Limit
        } else {
            Self::Compress
        }
    }
}

/// Input/output telemetry plus the gain reduction the optical cell is applying.
///
/// `gain_reduction_db` is positive: it is how many decibels the compressor is
/// taking off right now, which is what the editor's VU meter reads in its
/// GAIN REDUCTION position.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterFrame {
    pub in_peak: f32,
    pub in_rms: f32,
    pub out_peak: f32,
    pub out_rms: f32,
    pub gain_reduction_db: f32,
    /// Set when an input sample reached full scale. Sticky until the editor
    /// calls [`Dsp::clear_clip`].
    pub in_clip: bool,
    /// Set when an output sample reached full scale. Sticky.
    pub out_clip: bool,
}

/// Meter state owned by the audio thread. Peak rises instantly and falls on a
/// one-pole; RMS keeps a running mean square and defers the `sqrt` to the
/// reader, so the hot path stays multiply-adds.
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
    pub peak_reduction: f32,
    pub gain_db: f32,
    pub mode: Mode,
    pub emphasis: f32,
    pub mix: f32,
    pub color: f32,
    pub sidechain_low_cut_hz: f32,
    pub output_trim_db: f32,
}

pub fn default_params() -> Params {
    Params {
        power: true,
        peak_reduction: 35.0,
        gain_db: 0.0,
        mode: Mode::Compress,
        emphasis: 45.0,
        mix: 100.0,
        color: 12.0,
        sidechain_low_cut_hz: 90.0,
        output_trim_db: 0.0,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OpticalModel {
    pub threshold_db: f32,
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_sec: f32,
    pub release_sec: f32,
}

pub fn optical_model_from_params(params: &Params) -> OpticalModel {
    let amount = clamp(params.peak_reduction, 0.0, 100.0) / 100.0;
    let emphasis = clamp(params.emphasis, 0.0, 100.0) / 100.0;
    let sc_cut = clamp(params.sidechain_low_cut_hz, 20.0, 500.0);
    let sc_relief = ((sc_cut - 20.0) / 480.0) * 5.5;
    let emphasis_push = (emphasis - 0.5) * 7.0;
    let limit = params.mode == Mode::Limit;
    let threshold_db = clamp(
        peak_reduction_to_threshold_db(params.peak_reduction) - emphasis_push + sc_relief,
        -54.0,
        -3.0,
    );
    OpticalModel {
        threshold_db,
        ratio: if limit {
            12.0 + amount * 8.0
        } else {
            2.2 + amount * 1.6
        },
        knee_db: if limit {
            2.5 + (1.0 - amount) * 2.0
        } else {
            8.0 + (1.0 - amount) * 8.0
        },
        attack_sec: clamp(
            (if limit { 0.004 } else { 0.008 }) + (1.0 - amount) * 0.032 - emphasis * 0.003,
            0.002,
            0.07,
        ),
        release_sec: clamp(0.12 + amount * 0.68 + emphasis * 0.12, 0.08, 1.1),
    }
}

pub fn peak_reduction_to_threshold_db(peak_reduction: f32) -> f32 {
    let t = clamp(peak_reduction, 0.0, 100.0) / 100.0;
    -8.0 - t.powf(1.18) * 38.0
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PLUGIN_ID,
        name: "FA-2A",
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
                default_value: 0.0,
                min: 0.0,
                max: 1.0,
                unit: "enum",
            },
            ParamDescriptor {
                id: "peakReduction",
                name: "Peak Reduction",
                default_value: 35.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
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
                id: "emphasis",
                name: "Emphasis",
                default_value: 45.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
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
                id: "color",
                name: "Color",
                default_value: 12.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "sidechainLowCutHz",
                name: "Sidechain HPF",
                default_value: 90.0,
                min: 20.0,
                max: 500.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "outputTrimDb",
                name: "Output Trim",
                default_value: 0.0,
                min: -12.0,
                max: 12.0,
                unit: "dB",
            },
        ],
    }
}

#[derive(Debug, Clone)]
pub struct Dsp {
    params: Params,
    compressor: SoftKneeCompressor,
    output_gain: f32,
    meters: Meters,
}

impl Dsp {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut dsp = Self {
            params: default_params(),
            compressor: SoftKneeCompressor::new(sr),
            output_gain: 1.0,
            meters: Meters::new(sr),
        };
        dsp.apply_params();
        dsp
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn gain_reduction_db(&self) -> f32 {
        self.compressor.gain_reduction_db()
    }

    pub fn set_params(&mut self, params: Params) {
        self.params = params;
        ipc::sanitize_params(&mut self.params);
        self.apply_params();
    }

    /// Full telemetry for the editor's VU meter: input/output levels, the
    /// gain reduction the cell is applying right now, and the sticky clip
    /// latches.
    pub fn meter_frame(&self) -> MeterFrame {
        MeterFrame {
            in_peak: self.meters.in_peak,
            in_rms: self.meters.in_ms.max(0.0).sqrt(),
            out_peak: self.meters.out_peak,
            out_rms: self.meters.out_ms.max(0.0).sqrt(),
            // Reported even while bypassed would be a lie: the cell is not in
            // the path, so it is taking nothing off.
            gain_reduction_db: if self.params.power {
                self.compressor.gain_reduction_db().max(0.0)
            } else {
                0.0
            },
            in_clip: self.meters.in_clip,
            out_clip: self.meters.out_clip,
        }
    }

    /// Clear the sticky clip indicators (editor click-to-reset).
    pub fn clear_clip(&mut self) {
        self.meters.in_clip = false;
        self.meters.out_clip = false;
    }

    /// Apply a compact wire update already resolved by the UI/control thread.
    ///
    /// The audio path never parses JSON or looks up string parameter ids. Every
    /// arm recomputes only the optical model and its coefficients, so this
    /// stays allocation-free and safe to call from the producer thread between
    /// blocks.
    pub fn apply_wire_param(&mut self, wire_index: u32, value: f32) -> bool {
        if !ipc::apply_wire_param(&mut self.params, wire_index, value) {
            return false;
        }
        // Every continuous parameter feeds the optical model (peak reduction,
        // emphasis and the sidechain corner all move the threshold), so there
        // is nothing to gain from fanning out per index.
        self.apply_params();
        true
    }

    /// Resolve a string id off the realtime path (project restore, tests).
    pub fn apply_ui_param(&mut self, id: &str, value: f32) -> bool {
        match ipc::ui_param_index(id) {
            Some(index) => self.apply_wire_param(index, value),
            None => false,
        }
    }

    /// FA-2A is a feed-forward peak compressor with no lookahead, so it adds
    /// no latency for the graph to compensate.
    pub fn latency_samples(&self) -> usize {
        0
    }

    fn apply_params(&mut self) {
        let model = optical_model_from_params(&self.params);
        self.compressor.set_curve(
            model.threshold_db,
            model.ratio,
            model.knee_db,
            self.params.gain_db,
        );
        self.compressor
            .set_timing(model.attack_sec, model.release_sec);
        self.compressor
            .set_sidechain_hpf(self.params.sidechain_low_cut_hz);
        self.output_gain = db_to_linear(self.params.output_trim_db);
    }

    #[inline]
    fn apply_color(sample: f32, drive: f32) -> f32 {
        if drive <= 0.0 {
            return sample;
        }
        let amount = 1.0 + drive * 4.0;
        (sample * amount).tanh() / amount.tanh().max(0.001)
    }
}

impl StereoEffect for Dsp {
    fn reset(&mut self) {
        self.compressor.reset();
        self.meters.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.compressor.set_sample_rate(sr);
        self.meters = Meters::new(sr);
        self.apply_params();
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Metered on both sides of the cell even while bypassed: the editor's
        // meter is how you set gain staging *before* engaging it.
        let in_sum = (left + right) * 0.5;
        if !self.params.power {
            self.meters.push(in_sum, in_sum);
            return (left, right);
        }
        let (mut wet_l, mut wet_r) = self.compressor.process_stereo_linked(left, right);
        let drive = self.params.color / 100.0;
        wet_l = Self::apply_color(wet_l, drive) * self.output_gain;
        wet_r = Self::apply_color(wet_r, drive) * self.output_gain;
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

    /// Drive the compressor with a steady tone loud enough to be over the
    /// threshold, then read its telemetry.
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
    fn optical_model_limit_is_harder() {
        let mut compress = default_params();
        compress.mode = Mode::Compress;
        let mut limit = default_params();
        limit.mode = Mode::Limit;
        let c = optical_model_from_params(&compress);
        let l = optical_model_from_params(&limit);
        assert!(l.ratio > c.ratio);
        assert!(l.knee_db < c.knee_db);
    }

    #[test]
    fn processes_audio() {
        let mut dsp = Dsp::new(48_000.0);
        let (l, r) = dsp.process_stereo(0.8, -0.8);
        assert!(l.is_finite() && r.is_finite());
    }

    #[test]
    fn bypass_when_power_off() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.power = false;
        dsp.set_params(params);
        assert_eq!(dsp.process_stereo(0.25, -0.25), (0.25, -0.25));
    }

    /// The editor's VU meter reads this number directly, so it has to be a
    /// real measurement: nothing over the threshold means no reduction, and a
    /// hot signal means a positive amount that grows with peak reduction.
    #[test]
    fn gain_reduction_tracks_the_signal_and_the_control() {
        let mut quiet = Dsp::new(48_000.0);
        let frame = run_tone(&mut quiet, 0.002, 24_000);
        assert!(
            frame.gain_reduction_db < 1.0,
            "a signal under the threshold must not read as reduction: {}",
            frame.gain_reduction_db
        );

        let mut gentle = Dsp::new(48_000.0);
        let mut params = default_params();
        params.peak_reduction = 20.0;
        gentle.set_params(params.clone());
        let gentle_gr = run_tone(&mut gentle, 0.6, 48_000).gain_reduction_db;

        let mut hard = Dsp::new(48_000.0);
        params.peak_reduction = 90.0;
        hard.set_params(params);
        let hard_gr = run_tone(&mut hard, 0.6, 48_000).gain_reduction_db;

        assert!(gentle_gr > 0.0, "a hot signal must read some reduction");
        assert!(
            hard_gr > gentle_gr + 1.0,
            "more peak reduction must read more: {gentle_gr} vs {hard_gr}"
        );
    }

    /// Bypassed, the cell is out of the path — reporting reduction would put a
    /// needle on a meter for processing that is not happening.
    #[test]
    fn bypassed_reports_no_gain_reduction_but_still_meters_level() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.power = false;
        dsp.set_params(params);
        let frame = run_tone(&mut dsp, 0.6, 24_000);
        assert_eq!(frame.gain_reduction_db, 0.0);
        assert!(frame.in_rms > 0.05, "input must still be measured");
        assert!(frame.out_rms > 0.05, "output must still be measured");
    }

    #[test]
    fn meters_measure_both_sides_and_latch_clipping() {
        let mut dsp = Dsp::new(48_000.0);
        let frame = run_tone(&mut dsp, 0.5, 24_000);
        assert!(frame.in_peak > 0.4 && frame.in_peak <= 1.0);
        assert!(frame.in_rms > 0.0 && frame.in_rms < frame.in_peak + 1.0e-6);
        assert!(frame.out_peak > 0.0);
        assert!(!frame.in_clip && !frame.out_clip);

        let _ = dsp.process_stereo(1.5, 1.5);
        assert!(dsp.meter_frame().in_clip, "clip flag should latch");
        dsp.clear_clip();
        assert!(!dsp.meter_frame().in_clip);
        assert!(!dsp.meter_frame().out_clip);
    }

    #[test]
    fn reset_clears_the_meters() {
        let mut dsp = Dsp::new(48_000.0);
        let _ = run_tone(&mut dsp, 0.7, 4_800);
        dsp.reset();
        let frame = dsp.meter_frame();
        assert_eq!(frame.in_peak, 0.0);
        assert_eq!(frame.in_rms, 0.0);
        assert_eq!(frame.out_peak, 0.0);
        assert_eq!(frame.out_rms, 0.0);
    }

    #[test]
    fn processes_finite_at_multiple_rates() {
        for &sr in &[44_100.0f32, 48_000.0, 96_000.0] {
            let mut dsp = Dsp::new(sr);
            let frame = run_tone(&mut dsp, 0.8, sr as usize / 4);
            assert!(frame.in_rms.is_finite() && frame.out_rms.is_finite());
            assert!(frame.gain_reduction_db.is_finite());
        }
    }

    #[test]
    fn wire_update_changes_only_authoritative_params() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::PEAK_REDUCTION_INDEX, 80.0));
        assert_eq!(dsp.params().peak_reduction, 80.0);
        assert!(dsp.apply_ui_param("mode", Mode::Limit.to_wire()));
        assert_eq!(dsp.params().mode, Mode::Limit);
        assert!(!dsp.apply_wire_param(u32::MAX, 0.0));
        assert!(!dsp.apply_wire_param(ipc::GAIN_INDEX, f32::NAN));
    }
}
