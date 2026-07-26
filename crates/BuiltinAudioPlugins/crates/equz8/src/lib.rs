//! Equz8 — 8-band parametric EQ.
//!
//! Phase 1 (easy). Filter coefficients and runtime state use the MIT/Apache
//! [`biquad`] crate. No DirectAudioEngine dependency.

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

pub const PLUGIN_ID: &str = "futureboard.equz8";
pub const BAND_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BandType {
    HighPass,
    LowShelf,
    Bell,
    Notch,
    HighShelf,
    LowPass,
}

impl BandType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HighPass => "highpass",
            Self::LowShelf => "lowshelf",
            Self::Bell => "bell",
            Self::Notch => "notch",
            Self::HighShelf => "highshelf",
            Self::LowPass => "lowpass",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "highpass" | "hp" => Some(Self::HighPass),
            "lowshelf" | "ls" => Some(Self::LowShelf),
            "bell" | "peak" | "peaking" => Some(Self::Bell),
            "notch" => Some(Self::Notch),
            "highshelf" | "hs" => Some(Self::HighShelf),
            "lowpass" | "lp" => Some(Self::LowPass),
            _ => None,
        }
    }

    pub const fn to_wire(self) -> f32 {
        match self {
            Self::HighPass => 0.0,
            Self::LowShelf => 1.0,
            Self::Bell => 2.0,
            Self::Notch => 3.0,
            Self::HighShelf => 4.0,
            Self::LowPass => 5.0,
        }
    }

    pub fn from_wire(value: f32) -> Self {
        match value.round() as i32 {
            0 => Self::HighPass,
            1 => Self::LowShelf,
            3 => Self::Notch,
            4 => Self::HighShelf,
            5 => Self::LowPass,
            _ => Self::Bell,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandParams {
    pub active: bool,
    pub band_type: BandType,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub power: bool,
    pub output_db: f32,
    pub mix: f32,
    pub bands: [BandParams; BAND_COUNT],
}

pub fn default_params() -> Params {
    Params {
        power: true,
        output_db: 0.0,
        mix: 100.0,
        bands: [
            BandParams {
                active: true,
                band_type: BandType::HighPass,
                freq: 50.0,
                gain_db: 0.0,
                q: 0.7,
            },
            BandParams {
                active: true,
                band_type: BandType::LowShelf,
                freq: 120.0,
                gain_db: 0.0,
                q: 0.8,
            },
            BandParams {
                active: true,
                band_type: BandType::Bell,
                freq: 250.0,
                gain_db: 2.5,
                q: 1.2,
            },
            BandParams {
                active: true,
                band_type: BandType::Bell,
                freq: 750.0,
                gain_db: -1.5,
                q: 1.4,
            },
            BandParams {
                active: true,
                band_type: BandType::Bell,
                freq: 1_500.0,
                gain_db: 1.0,
                q: 1.0,
            },
            BandParams {
                active: true,
                band_type: BandType::Bell,
                freq: 3_500.0,
                gain_db: 0.0,
                q: 1.1,
            },
            BandParams {
                active: true,
                band_type: BandType::HighShelf,
                freq: 8_000.0,
                gain_db: 1.5,
                q: 0.8,
            },
            BandParams {
                active: true,
                band_type: BandType::LowPass,
                freq: 16_000.0,
                gain_db: 0.0,
                q: 0.7,
            },
        ],
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PLUGIN_ID,
        name: "Equz8",
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
                id: "mix",
                name: "Mix",
                default_value: 100.0,
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
        ],
    }
}

#[derive(Debug, Clone)]
pub struct Dsp {
    sample_rate: f32,
    params: Params,
    left: [Option<DirectForm1<f32>>; BAND_COUNT],
    right: [Option<DirectForm1<f32>>; BAND_COUNT],
    output_gain: f32,
}

impl Dsp {
    pub fn new(sample_rate: f32) -> Self {
        let mut dsp = Self {
            sample_rate: sample_rate.max(1.0),
            params: default_params(),
            left: [None, None, None, None, None, None, None, None],
            right: [None, None, None, None, None, None, None, None],
            output_gain: 1.0,
        };
        dsp.rebuild();
        dsp
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn set_params(&mut self, params: Params) {
        self.params = params;
        ipc::sanitize_params(&mut self.params);
        self.rebuild();
    }

    /// Apply a compact wire update already resolved by the UI/control thread.
    ///
    /// The audio path never parses JSON or looks up string parameter ids.
    /// A future host bridge can drain its bounded parameter ring between
    /// blocks and call this method directly.
    pub fn apply_wire_param(&mut self, wire_index: u32, value: f32) -> bool {
        if !ipc::apply_wire_param(&mut self.params, wire_index, value) {
            return false;
        }

        match wire_index {
            ipc::POWER_INDEX => {}
            ipc::MIX_INDEX => {}
            ipc::OUTPUT_INDEX => {
                self.output_gain = db_to_linear(self.params.output_db);
            }
            _ => {
                if let Some((band, _)) = ipc::decode_band_wire(wire_index) {
                    self.rebuild_band(band);
                }
            }
        }
        true
    }

    fn rebuild(&mut self) {
        self.output_gain = db_to_linear(self.params.output_db);
        for i in 0..BAND_COUNT {
            self.rebuild_band(i);
        }
    }

    fn rebuild_band(&mut self, index: usize) {
        let band = self.params.bands[index];
        if !band.active {
            self.left[index] = None;
            self.right[index] = None;
            return;
        }
        let filter = make_eq_biquad(
            band.band_type.as_str(),
            band.freq,
            band.gain_db,
            band.q,
            self.sample_rate,
        );
        self.left[index] = filter;
        self.right[index] = filter;
    }
}

impl StereoEffect for Dsp {
    fn reset(&mut self) {
        for filter in self.left.iter_mut().flatten() {
            filter.reset_state();
        }
        for filter in self.right.iter_mut().flatten() {
            filter.reset_state();
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.rebuild();
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        if !self.params.power {
            return (left, right);
        }

        let mut wet_l = left;
        let mut wet_r = right;
        for filter in self.left.iter_mut().flatten() {
            wet_l = filter.run(wet_l);
        }
        for filter in self.right.iter_mut().flatten() {
            wet_r = filter.run(wet_r);
        }
        wet_l *= self.output_gain;
        wet_r *= self.output_gain;

        let amount = self.params.mix / 100.0;
        (mix(left, wet_l, amount), mix(right, wet_r, amount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_id() {
        assert_eq!(descriptor().id, PLUGIN_ID);
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
    fn processes_without_nan() {
        let mut dsp = Dsp::new(48_000.0);
        let (l, r) = dsp.process_stereo(0.5, -0.5);
        assert!(l.is_finite() && r.is_finite());
    }

    #[test]
    fn wire_update_changes_only_authoritative_params() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::band_wire_index(2, ipc::BAND_GAIN), 6.0));
        assert_eq!(dsp.params().bands[2].gain_db, 6.0);
        assert!(!dsp.apply_wire_param(u32::MAX, 0.0));
    }
}
