//! Stable editor/DSP parameter wire contract for Z-Comp.
//!
//! String ids live at the UI/control boundary only. The native host resolves
//! them to compact indices before publishing edits to an audio-side bounded
//! queue; [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the
//! numeric form without allocation, serialization, locking, or string lookup.

use serde::{Deserialize, Serialize};

use crate::{CompModel, Params, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const MODEL_INDEX: u32 = 1;
pub const THRESHOLD_INDEX: u32 = 2;
pub const RATIO_INDEX: u32 = 3;
pub const ATTACK_INDEX: u32 = 4;
pub const RELEASE_INDEX: u32 = 5;
pub const KNEE_INDEX: u32 = 6;
pub const MAKEUP_INDEX: u32 = 7;
pub const MIX_INDEX: u32 = 8;
pub const SIDECHAIN_INDEX: u32 = 9;
pub const STEREO_LINK_INDEX: u32 = 10;
pub const COLOR_INDEX: u32 = 11;
pub const AUTO_RELEASE_INDEX: u32 = 12;

pub const PARAM_COUNT: usize = 13;

/// Wire index *is* the position in this table. Append only.
pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = [
    "power",
    "model",
    "thresholdDb",
    "ratio",
    "attackMs",
    "releaseMs",
    "kneeDb",
    "makeupDb",
    "mix",
    "sidechainHpfHz",
    "stereoLink",
    "color",
    "autoRelease",
];

const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),     // power
    (0.0, 0.0),     // model
    (-60.0, 0.0),   // thresholdDb
    (1.0, 20.0),    // ratio
    (0.01, 120.0),  // attackMs
    (10.0, 2500.0), // releaseMs
    (0.0, 24.0),    // kneeDb
    (-24.0, 24.0),  // makeupDb
    (0.0, 100.0),   // mix
    (20.0, 500.0),  // sidechainHpfHz
    (0.0, 100.0),   // stereoLink
    (0.0, 100.0),   // color
    (0.0, 0.0),     // autoRelease
];

#[inline]
fn clamp_wire(index: u32, value: f32) -> f32 {
    let (min, max) = RANGES[index as usize];
    clamp(value, min, max)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZcompState {
    pub version: u32,
    pub params: Params,
}

impl ZcompState {
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

impl Default for ZcompState {
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
    params.threshold_db = clamp_wire(THRESHOLD_INDEX, params.threshold_db);
    params.ratio = clamp_wire(RATIO_INDEX, params.ratio);
    params.attack_ms = clamp_wire(ATTACK_INDEX, params.attack_ms);
    params.release_ms = clamp_wire(RELEASE_INDEX, params.release_ms);
    params.knee_db = clamp_wire(KNEE_INDEX, params.knee_db);
    params.makeup_db = clamp_wire(MAKEUP_INDEX, params.makeup_db);
    params.mix = clamp_wire(MIX_INDEX, params.mix);
    params.sidechain_hpf_hz = clamp_wire(SIDECHAIN_INDEX, params.sidechain_hpf_hz);
    params.stereo_link = clamp_wire(STEREO_LINK_INDEX, params.stereo_link);
    params.color = clamp_wire(COLOR_INDEX, params.color);
}

pub fn apply_wire_param(params: &mut Params, index: u32, value: f32) -> bool {
    if !value.is_finite() || index as usize >= PARAM_COUNT {
        return false;
    }
    match index {
        POWER_INDEX => params.power = value >= 0.5,
        MODEL_INDEX => params.model = CompModel::from_wire(value),
        THRESHOLD_INDEX => params.threshold_db = clamp_wire(index, value),
        RATIO_INDEX => params.ratio = clamp_wire(index, value),
        ATTACK_INDEX => params.attack_ms = clamp_wire(index, value),
        RELEASE_INDEX => params.release_ms = clamp_wire(index, value),
        KNEE_INDEX => params.knee_db = clamp_wire(index, value),
        MAKEUP_INDEX => params.makeup_db = clamp_wire(index, value),
        MIX_INDEX => params.mix = clamp_wire(index, value),
        SIDECHAIN_INDEX => params.sidechain_hpf_hz = clamp_wire(index, value),
        STEREO_LINK_INDEX => params.stereo_link = clamp_wire(index, value),
        COLOR_INDEX => params.color = clamp_wire(index, value),
        AUTO_RELEASE_INDEX => params.auto_release = value >= 0.5,
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
        ("model", params.model.to_wire()),
        ("thresholdDb", params.threshold_db),
        ("ratio", params.ratio),
        ("attackMs", params.attack_ms),
        ("releaseMs", params.release_ms),
        ("kneeDb", params.knee_db),
        ("makeupDb", params.makeup_db),
        ("mix", params.mix),
        ("sidechainHpfHz", params.sidechain_hpf_hz),
        ("stereoLink", params.stereo_link),
        ("color", params.color),
        ("autoRelease", f32::from(params.auto_release)),
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
        assert!(apply_ui_param(&mut params, "thresholdDb", -28.0));
        assert!(apply_ui_param(
            &mut params,
            "model",
            CompModel::Distressor.to_wire()
        ));
        let json = ZcompState::new(params).to_json().unwrap();
        let decoded = ZcompState::from_json(&json).unwrap();
        assert_eq!(decoded.params.threshold_db, -28.0);
        assert_eq!(decoded.params.model, CompModel::Distressor);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "ratio", 900.0));
        assert_eq!(params.ratio, 20.0);
        assert!(apply_ui_param(&mut params, "thresholdDb", 12.0));
        assert_eq!(params.threshold_db, 0.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, RATIO_INDEX, f32::NAN));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    #[test]
    fn sanitize_pulls_a_hand_edited_blob_back_into_range() {
        let mut params = default_params();
        params.threshold_db = -90.0;
        params.ratio = 0.2;
        params.attack_ms = 0.0;
        params.release_ms = 9_000.0;
        sanitize_params(&mut params);
        assert_eq!(params.threshold_db, -60.0);
        assert_eq!(params.ratio, 1.0);
        assert_eq!(params.attack_ms, 0.01);
        assert_eq!(params.release_ms, 2500.0);
    }

    #[test]
    fn model_wire_values_round_trip() {
        for model in [
            CompModel::Comp2500,
            CompModel::Distressor,
            CompModel::Avalon,
            CompModel::Ssl,
        ] {
            assert_eq!(CompModel::from_wire(model.to_wire()), model);
            assert_eq!(CompModel::parse(model.as_str()), Some(model));
        }
    }
}
