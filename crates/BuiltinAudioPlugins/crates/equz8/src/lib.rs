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
    /// Index of the band being auditioned in isolation, or [`ipc::SOLO_NONE`].
    ///
    /// `serde(default)` so projects saved before band solo existed still load —
    /// they deserialize with solo off, which is the pre-existing behavior.
    #[serde(default = "solo_none")]
    pub solo_band: i32,
}

fn solo_none() -> i32 {
    ipc::SOLO_NONE
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
        solo_band: ipc::SOLO_NONE,
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
    /// Audition bandpass for the soloed band, per channel. `None` when no band
    /// is soloed — built on the control thread so the audio path only runs it.
    solo_left: Option<DirectForm1<f32>>,
    solo_right: Option<DirectForm1<f32>>,
    output_gain: f32,
}

/// Q used to audition band shapes that have no meaningful centre width.
///
/// Shelves and pass filters are broad by construction, so reusing their own Q
/// would open an audition window either far too wide to isolate anything or so
/// narrow it sits on a slope. A moderate fixed Q gives a consistent "what lives
/// around this corner frequency" listen.
const SOLO_WIDE_Q: f32 = 0.9;

impl Dsp {
    pub fn new(sample_rate: f32) -> Self {
        let mut dsp = Self {
            sample_rate: sample_rate.max(1.0),
            params: default_params(),
            left: [None, None, None, None, None, None, None, None],
            right: [None, None, None, None, None, None, None, None],
            solo_left: None,
            solo_right: None,
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
            ipc::SOLO_INDEX => self.rebuild_solo(),
            _ => {
                if let Some((band, _)) = ipc::decode_band_wire(wire_index) {
                    self.rebuild_band(band);
                    // Editing the soloed band must retune what you are hearing,
                    // otherwise the audition window stays where the band used
                    // to be while you drag it.
                    if self.params.solo_band == band as i32 {
                        self.rebuild_solo();
                    }
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
        self.rebuild_solo();
    }

    /// Build the audition bandpass for the soloed band, if any.
    fn rebuild_solo(&mut self) {
        let index = self.params.solo_band;
        let filter = if index >= 0 && (index as usize) < BAND_COUNT {
            let band = self.params.bands[index as usize];
            let q = match band.band_type {
                BandType::Bell | BandType::Notch => band.q,
                _ => SOLO_WIDE_Q,
            };
            make_eq_biquad("bandpass", band.freq, 0.0, q, self.sample_rate)
        } else {
            None
        };
        self.solo_left = filter;
        self.solo_right = filter;
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
        for filter in self.solo_left.iter_mut().chain(self.solo_right.iter_mut()) {
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

        // Band solo replaces the chain rather than adding to it, and
        // deliberately ignores Mix: auditioning a band means hearing that
        // frequency region alone, so blending the dry signal back in would
        // defeat the point. Output level still applies so the audition can be
        // matched in loudness.
        if let (Some(solo_l), Some(solo_r)) = (self.solo_left.as_mut(), self.solo_right.as_mut()) {
            return (
                solo_l.run(left) * self.output_gain,
                solo_r.run(right) * self.output_gain,
            );
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

    /// Measure output level at one frequency by running a sine through the DSP.
    fn level_at(dsp: &mut Dsp, freq_hz: f32) -> f32 {
        dsp.reset();
        let sr = 48_000.0f32;
        let step = std::f32::consts::TAU * freq_hz / sr;
        // Settle the filter state before measuring.
        for i in 0..4_000 {
            let _ = dsp.process_stereo((i as f32 * step).sin(), 0.0);
        }
        let mut peak = 0.0f32;
        for i in 4_000..8_000 {
            let (l, _) = dsp.process_stereo((i as f32 * step).sin(), 0.0);
            peak = peak.max(l.abs());
        }
        peak
    }

    #[test]
    fn solo_isolates_the_band_frequency_region() {
        let mut params = default_params();
        // A bell at 1 kHz, soloed. Everything far from it must drop away.
        params.bands[4].band_type = BandType::Bell;
        params.bands[4].freq = 1_000.0;
        params.bands[4].q = 2.0;
        params.solo_band = 4;

        let mut dsp = Dsp::new(48_000.0);
        dsp.set_params(params);

        let in_band = level_at(&mut dsp, 1_000.0);
        let below = level_at(&mut dsp, 100.0);
        let above = level_at(&mut dsp, 10_000.0);

        assert!(
            in_band > 0.5,
            "the soloed region must pass through, got {in_band}"
        );
        assert!(
            below < in_band * 0.2,
            "content an octave-plus below must be rejected: {below} vs {in_band}"
        );
        assert!(
            above < in_band * 0.2,
            "content well above must be rejected: {above} vs {in_band}"
        );
    }

    #[test]
    fn solo_ignores_mix_so_the_dry_signal_cannot_mask_it() {
        let mut params = default_params();
        params.bands[4].band_type = BandType::Bell;
        params.bands[4].freq = 1_000.0;
        params.bands[4].q = 2.0;
        params.solo_band = 4;
        // Fully dry would normally bypass the chain entirely.
        params.mix = 0.0;

        let mut dsp = Dsp::new(48_000.0);
        dsp.set_params(params);
        let below = level_at(&mut dsp, 100.0);
        assert!(
            below < 0.2,
            "solo must still reject out-of-band content at mix=0, got {below}"
        );
    }

    #[test]
    fn clearing_solo_restores_the_full_chain() {
        let mut params = default_params();
        params.solo_band = 2;
        let mut dsp = Dsp::new(48_000.0);
        dsp.set_params(params);
        assert!(level_at(&mut dsp, 10_000.0) < 0.2, "soloed: highs rejected");

        assert!(dsp.apply_wire_param(ipc::SOLO_INDEX, ipc::SOLO_NONE as f32));
        assert!(
            level_at(&mut dsp, 10_000.0) > 0.5,
            "clearing solo must let the full chain through again"
        );
    }

    /// Dragging the soloed band has to retune what you hear, or the audition
    /// window stays where the band used to be.
    #[test]
    fn editing_the_soloed_band_retunes_the_audition() {
        let mut params = default_params();
        params.bands[4].band_type = BandType::Bell;
        params.bands[4].freq = 1_000.0;
        params.bands[4].q = 2.0;
        params.solo_band = 4;
        let mut dsp = Dsp::new(48_000.0);
        dsp.set_params(params);
        assert!(
            level_at(&mut dsp, 5_000.0) < 0.2,
            "5 kHz starts out rejected"
        );

        assert!(dsp.apply_wire_param(ipc::band_wire_index(4, ipc::BAND_FREQ), 5_000.0));
        assert!(
            level_at(&mut dsp, 5_000.0) > 0.5,
            "moving the soloed band to 5 kHz must move the audition with it"
        );
    }

    #[test]
    fn out_of_range_solo_index_clears_instead_of_wedging() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::SOLO_INDEX, 99.0));
        assert_eq!(dsp.params().solo_band, ipc::SOLO_NONE);
        assert!(
            level_at(&mut dsp, 10_000.0) > 0.5,
            "no band should be soloed"
        );
    }

    #[test]
    fn wire_update_changes_only_authoritative_params() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::band_wire_index(2, ipc::BAND_GAIN), 6.0));
        assert_eq!(dsp.params().bands[2].gain_db, 6.0);
        assert!(!dsp.apply_wire_param(u32::MAX, 0.0));
    }
}
