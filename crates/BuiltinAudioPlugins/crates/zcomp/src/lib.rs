//! Z-Comp — ultimate multi-model dynamics processor.
//!
//! Four circuit models share one realtime-safe gain computer and colour stage:
//!
//! - **Comp 2500** — feed-forward VCA with soft dual-slope knee and THD colour
//! - **Distressor** — aggressive detector with British-mode grit and hard ratios
//! - **Avalon** — feedback Class-A optical leveling with slow musical recovery
//! - **SSL** — bus-compressor glue with program-dependent auto-release

use builtin_dsp_core::{
    ParamDescriptor, PluginCategory, PluginDescriptor, StereoEffect, clamp, db_to_linear,
    flush_denormal, linear_to_db, mix, time_constant,
};
use serde::{Deserialize, Serialize};

pub mod ipc;
pub mod ui;

pub use ipc::{UI_PARAM_IDS, ui_param_id, ui_param_index};

pub const PLUGIN_ID: &str = "futureboard.zcomp";

const CLIP_THRESHOLD: f32 = 1.0;
const RMS_WINDOW_SECONDS: f32 = 0.300;
const PEAK_FALL_SECONDS: f32 = 0.400;

/// Circuit model selected by the editor. Wire order is part of the persisted
/// contract — append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompModel {
    Comp2500,
    Distressor,
    Avalon,
    Ssl,
}

impl CompModel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comp2500 => "comp2500",
            Self::Distressor => "distressor",
            Self::Avalon => "avalon",
            Self::Ssl => "ssl",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Comp2500 => "2500",
            Self::Distressor => "Distress",
            Self::Avalon => "Avalon",
            Self::Ssl => "SSL",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "comp2500" | "2500" | "api2500" => Some(Self::Comp2500),
            "distressor" | "distress" => Some(Self::Distressor),
            "avalon" => Some(Self::Avalon),
            "ssl" | "bus" => Some(Self::Ssl),
            _ => None,
        }
    }

    pub const fn to_wire(self) -> f32 {
        match self {
            Self::Comp2500 => 0.0,
            Self::Distressor => 1.0,
            Self::Avalon => 2.0,
            Self::Ssl => 3.0,
        }
    }

    pub fn from_wire(value: f32) -> Self {
        match value.round() as i32 {
            1 => Self::Distressor,
            2 => Self::Avalon,
            3 => Self::Ssl,
            _ => Self::Comp2500,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterFrame {
    pub in_peak: f32,
    pub in_rms: f32,
    pub out_peak: f32,
    pub out_rms: f32,
    /// Positive: how many dB the cell is taking off right now.
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
    fn push(&mut self, input: (f32, f32), output: (f32, f32)) {
        let in_abs = input.0.abs().max(input.1.abs());
        let out_abs = output.0.abs().max(output.1.abs());

        self.in_peak = if in_abs >= self.in_peak {
            in_abs
        } else {
            self.in_peak * self.peak_coeff
        };
        self.out_peak = if out_abs >= self.out_peak {
            out_abs
        } else {
            self.out_peak * self.peak_coeff
        };

        let in_square = (input.0 * input.0 + input.1 * input.1) * 0.5;
        let out_square = (output.0 * output.0 + output.1 * output.1) * 0.5;
        self.in_ms = self.rms_coeff * self.in_ms + (1.0 - self.rms_coeff) * in_square;
        self.out_ms = self.rms_coeff * self.out_ms + (1.0 - self.rms_coeff) * out_square;

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
    pub model: CompModel,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
    pub mix: f32,
    pub sidechain_hpf_hz: f32,
    pub stereo_link: f32,
    pub color: f32,
    pub auto_release: bool,
}

pub fn default_params() -> Params {
    Params {
        power: true,
        model: CompModel::Ssl,
        threshold_db: -18.0,
        ratio: 4.0,
        attack_ms: 10.0,
        release_ms: 100.0,
        knee_db: 6.0,
        makeup_db: 0.0,
        mix: 100.0,
        sidechain_hpf_hz: 60.0,
        stereo_link: 100.0,
        color: 18.0,
        auto_release: true,
    }
}

/// Resolved runtime coefficients for the selected circuit model.
#[derive(Debug, Clone, Copy)]
struct ModelCoeffs {
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack_sec: f32,
    release_sec: f32,
    release_tail_sec: f32,
    /// 0 = pure feed-forward, 1 = pure feedback.
    feedback: f32,
    /// Detector RMS blend (0 = peak, 1 = RMS).
    rms_blend: f32,
    /// Soft clip / harmonic drive after the gain cell.
    drive: f32,
    /// Extra detector boost (Distressor British / 2500 THD sense).
    detector_boost: f32,
}

fn model_coeffs(params: &Params) -> ModelCoeffs {
    let color = clamp(params.color, 0.0, 100.0) / 100.0;
    let ratio = params.ratio.max(1.0);
    let knee = params.knee_db.max(0.0);
    let attack_ms = params.attack_ms.max(0.01);
    let release_ms = params.release_ms.max(10.0);

    match params.model {
        CompModel::Comp2500 => {
            // API 2500: feed-forward VCA, dual soft knee, THD colouring.
            ModelCoeffs {
                threshold_db: params.threshold_db,
                ratio: ratio * (1.0 + color * 0.08),
                knee_db: (knee + 4.0 + color * 4.0).min(24.0),
                attack_sec: (attack_ms * 0.001).clamp(0.0002, 0.080),
                release_sec: (release_ms * 0.001).clamp(0.020, 1.200),
                release_tail_sec: (release_ms * 0.001 * 1.8).clamp(0.050, 2.0),
                feedback: 0.08,
                rms_blend: 0.25,
                drive: color * 0.55,
                detector_boost: 1.0 + color * 0.22,
            }
        }
        CompModel::Distressor => {
            // Distressor: punchy detector, hard ratios, British grit.
            let british = color;
            ModelCoeffs {
                threshold_db: params.threshold_db - british * 1.5,
                ratio: (ratio * (1.0 + british * 0.45)).min(40.0),
                knee_db: (knee * (1.0 - british * 0.55)).max(0.5),
                attack_sec: (attack_ms * 0.001 * (1.0 - british * 0.35)).clamp(0.00005, 0.050),
                release_sec: (release_ms * 0.001).clamp(0.010, 1.500),
                release_tail_sec: (release_ms * 0.001 * (2.4 + british)).clamp(0.040, 3.0),
                feedback: 0.18 + british * 0.22,
                rms_blend: 0.10,
                drive: 0.20 + british * 0.95,
                detector_boost: 1.15 + british * 0.85,
            }
        }
        CompModel::Avalon => {
            // Avalon: Class-A optical leveling — soft, slow, musical.
            ModelCoeffs {
                threshold_db: params.threshold_db,
                ratio: (2.0 + (ratio - 1.0) * 0.55).clamp(1.5, 8.0),
                knee_db: (knee + 8.0 + color * 4.0).min(24.0),
                attack_sec: (attack_ms * 0.001).max(0.008).clamp(0.008, 0.080),
                release_sec: (release_ms * 0.001).max(0.080).clamp(0.080, 1.800),
                release_tail_sec: (release_ms * 0.001 * 3.5 + color * 1.2).clamp(0.400, 5.0),
                feedback: 0.82,
                rms_blend: 0.55,
                drive: color * 0.28,
                detector_boost: 1.0 + color * 0.08,
            }
        }
        CompModel::Ssl => {
            // SSL bus compressor: soft knee glue, optional auto-release.
            let auto = params.auto_release;
            let release_base = if auto {
                0.120
            } else {
                (release_ms * 0.001).clamp(0.050, 1.200)
            };
            ModelCoeffs {
                threshold_db: params.threshold_db,
                ratio: ratio.clamp(1.5, 10.0),
                knee_db: (knee + 3.0).clamp(2.0, 18.0),
                attack_sec: (attack_ms * 0.001).clamp(0.0001, 0.030),
                release_sec: release_base,
                release_tail_sec: if auto {
                    0.850 + color * 0.4
                } else {
                    release_base * 2.2
                },
                feedback: 0.12,
                rms_blend: 0.35 + color * 0.15,
                drive: color * 0.32,
                detector_boost: 1.0 + color * 0.12,
            }
        }
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PLUGIN_ID,
        name: "Z-Comp",
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
                id: "model",
                name: "Model",
                default_value: 3.0, // CompModel::Ssl
                min: 0.0,
                max: 3.0,
                unit: "enum",
            },
            ParamDescriptor {
                id: "thresholdDb",
                name: "Threshold",
                default_value: -18.0,
                min: -60.0,
                max: 0.0,
                unit: "dB",
            },
            ParamDescriptor {
                id: "ratio",
                name: "Ratio",
                default_value: 4.0,
                min: 1.0,
                max: 20.0,
                unit: ":1",
            },
            ParamDescriptor {
                id: "attackMs",
                name: "Attack",
                default_value: 10.0,
                min: 0.01,
                max: 120.0,
                unit: "ms",
            },
            ParamDescriptor {
                id: "releaseMs",
                name: "Release",
                default_value: 100.0,
                min: 10.0,
                max: 2500.0,
                unit: "ms",
            },
            ParamDescriptor {
                id: "kneeDb",
                name: "Knee",
                default_value: 6.0,
                min: 0.0,
                max: 24.0,
                unit: "dB",
            },
            ParamDescriptor {
                id: "makeupDb",
                name: "Makeup",
                default_value: 0.0,
                min: -24.0,
                max: 24.0,
                unit: "dB",
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
                id: "sidechainHpfHz",
                name: "Sidechain HPF",
                default_value: 60.0,
                min: 20.0,
                max: 500.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "stereoLink",
                name: "Stereo Link",
                default_value: 100.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "color",
                name: "Color",
                default_value: 18.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "autoRelease",
                name: "Auto Release",
                default_value: 1.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
        ],
    }
}

/// Multi-model stereo gain cell. Topology (FF/FB blend), detector ballistics,
/// and colour are all coefficient-driven so model changes stay allocation-free.
#[derive(Debug, Clone)]
struct GainCell {
    sample_rate: f32,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack_coeff: f32,
    release_fast_coeff: f32,
    release_tail_coeff: f32,
    feedback: f32,
    rms_blend: f32,
    drive: f32,
    detector_boost: f32,
    envelope_l: f32,
    envelope_r: f32,
    envelope_linked: f32,
    rms_l: f32,
    rms_r: f32,
    rms_linked: f32,
    gr_l_db: f32,
    gr_r_db: f32,
    gr_linked_db: f32,
    sidechain_coeff: f32,
    sidechain_x1: [f32; 2],
    sidechain_y1: [f32; 2],
    rms_coeff: f32,
    stereo_link: f32,
    auto_release: bool,
}

impl GainCell {
    fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut cell = Self {
            sample_rate: sr,
            threshold_db: -18.0,
            ratio: 4.0,
            knee_db: 6.0,
            attack_coeff: 0.0,
            release_fast_coeff: 0.0,
            release_tail_coeff: 0.0,
            feedback: 0.0,
            rms_blend: 0.0,
            drive: 0.0,
            detector_boost: 1.0,
            envelope_l: 0.0,
            envelope_r: 0.0,
            envelope_linked: 0.0,
            rms_l: 0.0,
            rms_r: 0.0,
            rms_linked: 0.0,
            gr_l_db: 0.0,
            gr_r_db: 0.0,
            gr_linked_db: 0.0,
            sidechain_coeff: 0.0,
            sidechain_x1: [0.0; 2],
            sidechain_y1: [0.0; 2],
            rms_coeff: time_constant(sr, 0.012),
            stereo_link: 1.0,
            auto_release: true,
        };
        cell.set_model(
            model_coeffs(&default_params()),
            default_params().sidechain_hpf_hz,
            default_params().stereo_link,
            default_params().auto_release,
        );
        cell
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.rms_coeff = time_constant(self.sample_rate, 0.012);
    }

    fn set_model(
        &mut self,
        coeffs: ModelCoeffs,
        sidechain_cutoff_hz: f32,
        stereo_link: f32,
        auto_release: bool,
    ) {
        self.threshold_db = coeffs.threshold_db;
        self.ratio = coeffs.ratio.max(1.0);
        self.knee_db = coeffs.knee_db.max(0.0);
        self.attack_coeff = time_constant(self.sample_rate, coeffs.attack_sec);
        self.release_fast_coeff = time_constant(self.sample_rate, coeffs.release_sec);
        self.release_tail_coeff = time_constant(self.sample_rate, coeffs.release_tail_sec);
        self.feedback = clamp(coeffs.feedback, 0.0, 1.0);
        self.rms_blend = clamp(coeffs.rms_blend, 0.0, 1.0);
        self.drive = coeffs.drive.max(0.0);
        self.detector_boost = coeffs.detector_boost.max(1.0);
        self.stereo_link = clamp(stereo_link, 0.0, 100.0) / 100.0;
        self.auto_release = auto_release;

        let cutoff = clamp(sidechain_cutoff_hz, 20.0, self.sample_rate * 0.45);
        self.sidechain_coeff = (-2.0 * std::f32::consts::PI * cutoff / self.sample_rate).exp();
    }

    fn reset(&mut self) {
        self.envelope_l = 0.0;
        self.envelope_r = 0.0;
        self.envelope_linked = 0.0;
        self.rms_l = 0.0;
        self.rms_r = 0.0;
        self.rms_linked = 0.0;
        self.gr_l_db = 0.0;
        self.gr_r_db = 0.0;
        self.gr_linked_db = 0.0;
        self.sidechain_x1 = [0.0; 2];
        self.sidechain_y1 = [0.0; 2];
    }

    #[inline]
    fn gain_reduction_db(&self) -> f32 {
        let link = self.stereo_link;
        let dual = self.gr_l_db.max(self.gr_r_db);
        dual * (1.0 - link) + self.gr_linked_db * link
    }

    #[inline]
    fn high_pass(&mut self, input: f32, channel: usize) -> f32 {
        let output = self.sidechain_coeff
            * (self.sidechain_y1[channel] + input - self.sidechain_x1[channel]);
        self.sidechain_x1[channel] = input;
        self.sidechain_y1[channel] = flush_denormal(output);
        self.sidechain_y1[channel]
    }

    #[inline]
    fn soft_knee_gr_db(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold_db;
        let half_knee = self.knee_db * 0.5;
        let curved_over = if over <= -half_knee {
            0.0
        } else if over >= half_knee {
            over
        } else {
            let t = over + half_knee;
            t * t / (2.0 * self.knee_db.max(1.0e-6))
        };
        // Feed-forward slope; feedback path scales this further via topology.
        curved_over * (1.0 - 1.0 / self.ratio)
    }

    #[inline]
    fn follow_gr(current: f32, target: f32, attack: f32, release: f32) -> f32 {
        let coeff = if target > current { attack } else { release };
        flush_denormal(coeff * current + (1.0 - coeff) * target)
    }

    #[inline]
    fn follow_env(current: f32, target: f32, attack: f32, release: f32) -> f32 {
        let coeff = if target > current { attack } else { release };
        flush_denormal(coeff * current + (1.0 - coeff) * target)
    }

    #[inline]
    fn colour(&self, sample: f32) -> f32 {
        if self.drive <= 0.0 {
            return sample;
        }
        // Odd-harmonic soft saturation. Amount scales with model colour.
        let x = sample * (1.0 + self.drive * 0.85);
        let soft = x - (x * x * x) / (3.0 + self.drive * 2.0);
        let wet = soft / (1.0 + self.drive * 0.35);
        mix(sample, wet, clamp(self.drive, 0.0, 1.0))
    }

    #[inline]
    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        // Feedback path observes post-cell signal via previous GR.
        let prev_gain_l = db_to_linear(-self.gr_l_db);
        let prev_gain_r = db_to_linear(-self.gr_r_db);

        let ff_l = left;
        let ff_r = right;
        let fb_l = left * prev_gain_l;
        let fb_r = right * prev_gain_r;

        let sense_l = mix(ff_l, fb_l, self.feedback);
        let sense_r = mix(ff_r, fb_r, self.feedback);

        let sc_l = self.high_pass(sense_l, 0).abs() * self.detector_boost;
        let sc_r = self.high_pass(sense_r, 1).abs() * self.detector_boost;
        // Linked detector from the louder filtered channel (no second HPF pass).
        let sc_link = sc_l.max(sc_r);

        // Peak / RMS hybrid detector.
        let rms_coeff = self.rms_coeff;
        self.rms_l = flush_denormal(rms_coeff * self.rms_l + (1.0 - rms_coeff) * (sc_l * sc_l));
        self.rms_r = flush_denormal(rms_coeff * self.rms_r + (1.0 - rms_coeff) * (sc_r * sc_r));
        self.rms_linked =
            flush_denormal(rms_coeff * self.rms_linked + (1.0 - rms_coeff) * (sc_link * sc_link));

        let peak_l = sc_l;
        let peak_r = sc_r;
        let peak_link = sc_link;
        let det_l = mix(peak_l, self.rms_l.max(0.0).sqrt(), self.rms_blend);
        let det_r = mix(peak_r, self.rms_r.max(0.0).sqrt(), self.rms_blend);
        let det_link = mix(peak_link, self.rms_linked.max(0.0).sqrt(), self.rms_blend);

        self.envelope_l = Self::follow_env(
            self.envelope_l,
            det_l,
            self.attack_coeff,
            self.release_fast_coeff,
        );
        self.envelope_r = Self::follow_env(
            self.envelope_r,
            det_r,
            self.attack_coeff,
            self.release_fast_coeff,
        );
        self.envelope_linked = Self::follow_env(
            self.envelope_linked,
            det_link,
            self.attack_coeff,
            self.release_fast_coeff,
        );

        let target_l = self.soft_knee_gr_db(linear_to_db(self.envelope_l.max(1.0e-12)));
        let target_r = self.soft_knee_gr_db(linear_to_db(self.envelope_r.max(1.0e-12)));
        let target_link = self.soft_knee_gr_db(linear_to_db(self.envelope_linked.max(1.0e-12)));

        // Program-dependent release: deeper GR stretches the tail (SSL Auto /
        // Distressor / Avalon optical memory).
        let depth = clamp(
            self.gr_linked_db.max(self.gr_l_db).max(self.gr_r_db) / 12.0,
            0.0,
            1.0,
        );
        let release = if self.auto_release {
            self.release_fast_coeff
                + (self.release_tail_coeff - self.release_fast_coeff) * (0.35 + depth * 0.65)
        } else {
            self.release_fast_coeff
                + (self.release_tail_coeff - self.release_fast_coeff) * depth * 0.45
        };

        self.gr_l_db = Self::follow_gr(self.gr_l_db, target_l, self.attack_coeff, release);
        self.gr_r_db = Self::follow_gr(self.gr_r_db, target_r, self.attack_coeff, release);
        self.gr_linked_db =
            Self::follow_gr(self.gr_linked_db, target_link, self.attack_coeff, release);

        let link = self.stereo_link;
        let gr_l = self.gr_l_db * (1.0 - link) + self.gr_linked_db * link;
        let gr_r = self.gr_r_db * (1.0 - link) + self.gr_linked_db * link;
        let gain_l = db_to_linear(-gr_l);
        let gain_r = db_to_linear(-gr_r);

        let wet_l = self.colour(left * gain_l);
        let wet_r = self.colour(right * gain_r);
        (wet_l, wet_r)
    }
}

#[derive(Debug, Clone)]
pub struct Dsp {
    params: Params,
    cell: GainCell,
    makeup_gain: f32,
    meters: Meters,
}

impl Dsp {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut dsp = Self {
            params: default_params(),
            cell: GainCell::new(sr),
            makeup_gain: 1.0,
            meters: Meters::new(sr),
        };
        dsp.apply_params();
        dsp
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn gain_reduction_db(&self) -> f32 {
        self.cell.gain_reduction_db()
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
                self.cell.gain_reduction_db().max(0.0)
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
        0
    }

    fn apply_params(&mut self) {
        let coeffs = model_coeffs(&self.params);
        self.cell.set_model(
            coeffs,
            self.params.sidechain_hpf_hz,
            self.params.stereo_link,
            self.params.auto_release,
        );
        self.makeup_gain = db_to_linear(self.params.makeup_db);
    }
}

impl StereoEffect for Dsp {
    fn reset(&mut self) {
        self.cell.reset();
        self.meters.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.cell.set_sample_rate(sr);
        self.meters = Meters::new(sr);
        self.apply_params();
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        if !self.params.power {
            self.meters.push((left, right), (left, right));
            return (left, right);
        }
        let (mut wet_l, mut wet_r) = self.cell.process_stereo(left, right);
        wet_l *= self.makeup_gain;
        wet_r *= self.makeup_gain;
        let amount = self.params.mix / 100.0;
        let out_l = mix(left, wet_l, amount);
        let out_r = mix(right, wet_r, amount);
        self.meters.push((left, right), (out_l, out_r));
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
        assert_eq!(d.category, PluginCategory::Effect);

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
                (param.default_value - actual).abs() < 1.0e-6,
                "`{}`: descriptor {} vs default {actual}",
                param.id,
                param.default_value
            );
        }
    }

    #[test]
    fn all_models_process_finite_and_reduce_loud_signal() {
        for model in [
            CompModel::Comp2500,
            CompModel::Distressor,
            CompModel::Avalon,
            CompModel::Ssl,
        ] {
            let mut params = default_params();
            params.model = model;
            params.threshold_db = -30.0;
            params.ratio = 8.0;
            params.mix = 100.0;
            params.color = 40.0;
            params.auto_release = true;
            let mut dsp = Dsp::new(48_000.0);
            dsp.set_params(params);
            let frame = run_tone(&mut dsp, 0.85, 24_000);
            assert!(
                frame.gain_reduction_db > 0.5,
                "{:?} should reduce a loud tone, got {} dB",
                model,
                frame.gain_reduction_db
            );
            assert!(frame.out_peak < frame.in_peak || frame.gain_reduction_db > 1.0);
        }
    }

    #[test]
    fn distressor_is_more_aggressive_than_avalon() {
        let mut distress = default_params();
        distress.model = CompModel::Distressor;
        distress.threshold_db = -24.0;
        distress.ratio = 6.0;
        distress.color = 70.0;
        distress.mix = 100.0;

        let mut avalon = distress.clone();
        avalon.model = CompModel::Avalon;
        avalon.color = 20.0;

        let mut d_dsp = Dsp::new(48_000.0);
        d_dsp.set_params(distress);
        let d_frame = run_tone(&mut d_dsp, 0.9, 24_000);

        let mut a_dsp = Dsp::new(48_000.0);
        a_dsp.set_params(avalon);
        let a_frame = run_tone(&mut a_dsp, 0.9, 24_000);

        assert!(
            d_frame.gain_reduction_db > a_frame.gain_reduction_db,
            "distress {} vs avalon {}",
            d_frame.gain_reduction_db,
            a_frame.gain_reduction_db
        );
    }

    #[test]
    fn power_off_is_true_bypass() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.power = false;
        dsp.set_params(params);
        for n in 0..256 {
            let x = (n as f32 * 0.1).sin() * 0.5;
            let (l, r) = dsp.process_stereo(x, -x);
            assert_eq!(l, x);
            assert_eq!(r, -x);
        }
        assert_eq!(dsp.meter_frame().gain_reduction_db, 0.0);
    }

    #[test]
    fn wire_param_updates_reconfigure_without_allocating_panic() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::MODEL_INDEX, CompModel::Comp2500.to_wire()));
        assert!(dsp.apply_wire_param(ipc::THRESHOLD_INDEX, -22.0));
        assert!(dsp.apply_wire_param(ipc::RATIO_INDEX, 10.0));
        let _ = run_tone(&mut dsp, 0.7, 2048);
        assert!(dsp.meter_frame().gain_reduction_db >= 0.0);
    }

    #[test]
    fn model_coeffs_ssl_auto_uses_program_release() {
        let mut params = default_params();
        params.model = CompModel::Ssl;
        params.auto_release = true;
        let auto = model_coeffs(&params);
        params.auto_release = false;
        params.release_ms = 50.0;
        let manual = model_coeffs(&params);
        assert!(auto.release_tail_sec > manual.release_sec);
    }
}
