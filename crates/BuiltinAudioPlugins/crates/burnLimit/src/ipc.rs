//! Stable editor/DSP parameter wire contract for BurnLimit.
//!
//! String ids live at the UI/control boundary only. The native host resolves
//! them to compact indices before publishing edits to an audio-side bounded
//! queue; [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the
//! numeric form without allocation, serialization, locking, or string lookup.

use serde::{Deserialize, Serialize};

use crate::{Params, Style, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const STYLE_INDEX: u32 = 1;
pub const GAIN_INDEX: u32 = 2;
pub const CEILING_INDEX: u32 = 3;
pub const RELEASE_INDEX: u32 = 4;
pub const LOOKAHEAD_INDEX: u32 = 5;
pub const TRUE_PEAK_INDEX: u32 = 6;
pub const MIX_INDEX: u32 = 7;
pub const STEREO_LINK_INDEX: u32 = 8;

pub const PARAM_COUNT: usize = 9;

pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = [
    "power",
    "style",
    "gainDb",
    "ceilingDb",
    "releaseMs",
    "lookaheadMs",
    "truePeak",
    "mix",
    "stereoLink",
];

const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),      // power
    (0.0, 0.0),      // style
    (-12.0, 24.0),   // gainDb
    (-6.0, 0.0),     // ceilingDb
    (20.0, 2_000.0), // releaseMs
    (0.0, 10.0),     // lookaheadMs
    (0.0, 0.0),      // truePeak
    (0.0, 100.0),    // mix
    (0.0, 0.0),      // stereoLink
];

#[inline]
fn clamp_wire(index: u32, value: f32) -> f32 {
    let (min, max) = RANGES[index as usize];
    clamp(value, min, max)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BurnLimitState {
    pub version: u32,
    pub params: Params,
}

impl BurnLimitState {
    pub fn new(params: Params) -> Self {
        Self {
            version: STATE_VERSION,
            params,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

impl Default for BurnLimitState {
    fn default() -> Self {
        Self::new(default_params())
    }
}

pub fn ui_param_index(id: &str) -> Option<u32> {
    UI_PARAM_IDS
        .iter()
        .position(|candidate| *candidate == id)
        .map(|index| index as u32)
}

pub fn ui_param_id(index: u32) -> Option<&'static str> {
    UI_PARAM_IDS.get(index as usize).copied()
}

pub fn sanitize_params(params: &mut Params) {
    params.gain_db = clamp_wire(GAIN_INDEX, params.gain_db);
    params.ceiling_db = clamp_wire(CEILING_INDEX, params.ceiling_db);
    params.release_ms = clamp_wire(RELEASE_INDEX, params.release_ms);
    params.lookahead_ms = clamp_wire(LOOKAHEAD_INDEX, params.lookahead_ms);
    params.mix = clamp_wire(MIX_INDEX, params.mix);
}

pub fn apply_wire_param(params: &mut Params, index: u32, value: f32) -> bool {
    if !value.is_finite() || index as usize >= PARAM_COUNT {
        return false;
    }
    match index {
        POWER_INDEX => params.power = value >= 0.5,
        STYLE_INDEX => params.style = Style::from_wire(value),
        GAIN_INDEX => params.gain_db = clamp_wire(index, value),
        CEILING_INDEX => params.ceiling_db = clamp_wire(index, value),
        RELEASE_INDEX => params.release_ms = clamp_wire(index, value),
        LOOKAHEAD_INDEX => params.lookahead_ms = clamp_wire(index, value),
        TRUE_PEAK_INDEX => params.true_peak = value >= 0.5,
        MIX_INDEX => params.mix = clamp_wire(index, value),
        STEREO_LINK_INDEX => params.stereo_link = value >= 0.5,
        _ => return false,
    }
    true
}

pub fn apply_ui_param(params: &mut Params, id: &str, value: f32) -> bool {
    let Some(index) = ui_param_index(id) else {
        return false;
    };
    apply_wire_param(params, index, value)
}

pub fn ui_values(params: &Params) -> Vec<(&'static str, f32)> {
    vec![
        ("power", f32::from(params.power)),
        ("style", params.style.to_wire()),
        ("gainDb", params.gain_db),
        ("ceilingDb", params.ceiling_db),
        ("releaseMs", params.release_ms),
        ("lookaheadMs", params.lookahead_ms),
        ("truePeak", f32::from(params.true_peak)),
        ("mix", params.mix),
        ("stereoLink", f32::from(params.stereo_link)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_to_wire_indices() {
        assert_eq!(UI_PARAM_IDS.len(), PARAM_COUNT);
        for (index, id) in UI_PARAM_IDS.iter().enumerate() {
            assert_eq!(ui_param_index(id), Some(index as u32));
            assert_eq!(ui_param_id(index as u32), Some(*id));
        }
    }

    #[test]
    fn ui_values_covers_every_id_in_wire_order() {
        let values = ui_values(&default_params());
        assert_eq!(values.len(), PARAM_COUNT);
        for (index, (id, _)) in values.iter().enumerate() {
            assert_eq!(*id, UI_PARAM_IDS[index]);
        }
    }

    #[test]
    fn state_round_trips() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "gainDb", 6.0));
        assert!(apply_ui_param(&mut params, "style", Style::Clip.to_wire()));
        let json = BurnLimitState::new(params).to_json().unwrap();
        let decoded = BurnLimitState::from_json(&json).unwrap();
        assert_eq!(decoded.params.gain_db, 6.0);
        assert_eq!(decoded.params.style, Style::Clip);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "gainDb", 900.0));
        assert_eq!(params.gain_db, 24.0);
        assert!(apply_ui_param(&mut params, "ceilingDb", 5.0));
        assert_eq!(params.ceiling_db, 0.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, GAIN_INDEX, f32::NAN));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    #[test]
    fn style_wire_values_round_trip() {
        for style in [Style::Clean, Style::Punch, Style::Modern, Style::Clip] {
            assert_eq!(Style::from_wire(style.to_wire()), style);
            assert_eq!(Style::parse(style.as_str()), Some(style));
        }
    }
}
