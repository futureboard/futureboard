//! Stable editor/DSP parameter wire contract for 67Clipper.
//!
//! String ids live at the UI/control boundary only. The native host resolves
//! them to compact indices before publishing edits to an audio-side bounded
//! queue; [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the
//! numeric form without allocation, serialization, locking, or string lookup.

use serde::{Deserialize, Serialize};

use crate::{Mode, Params, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const MODE_INDEX: u32 = 1;
pub const THRESHOLD_INDEX: u32 = 2;
pub const SHAPE_INDEX: u32 = 3;
pub const CEILING_INDEX: u32 = 4;
pub const MIX_INDEX: u32 = 5;
pub const STEREO_LINK_INDEX: u32 = 6;
pub const DC_FILTER_INDEX: u32 = 7;

pub const PARAM_COUNT: usize = 8;

/// Wire index *is* the position in this table; the editor and the host both
/// resolve through it, so the order is part of the persisted contract. Append
/// only.
pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = [
    "power",
    "mode",
    "thresholdDb",
    "shape",
    "ceilingDb",
    "mix",
    "stereoLink",
    "dcFilter",
];

/// Inclusive `(min, max)` for every continuous parameter, indexed by wire
/// index. Booleans and the mode enum carry `(0, 0)` and are handled by their
/// own arms in [`apply_wire_param`]. Single source of truth for clamping, so
/// [`sanitize_params`] and the wire path cannot drift apart.
const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),    // power
    (0.0, 0.0),    // mode
    (-24.0, 0.0),  // thresholdDb
    (0.0, 100.0),  // shape
    (-6.0, 0.0),   // ceilingDb
    (0.0, 100.0),  // mix
    (0.0, 0.0),    // stereoLink
    (0.0, 0.0),    // dcFilter
];

#[inline]
fn clamp_wire(index: u32, value: f32) -> f32 {
    let (min, max) = RANGES[index as usize];
    clamp(value, min, max)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clipper67State {
    pub version: u32,
    pub params: Params,
}

impl Clipper67State {
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

impl Default for Clipper67State {
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

/// Clamp a whole `Params` into range. Used after deserializing a project blob,
/// which may predate a range change or have been edited by hand.
pub fn sanitize_params(params: &mut Params) {
    params.threshold_db = clamp_wire(THRESHOLD_INDEX, params.threshold_db);
    params.shape = clamp_wire(SHAPE_INDEX, params.shape);
    params.ceiling_db = clamp_wire(CEILING_INDEX, params.ceiling_db);
    params.mix = clamp_wire(MIX_INDEX, params.mix);
}

/// Apply one compact UI/control update. Allocation-free and total: invalid
/// indices are rejected, non-finite values are rejected, and every continuous
/// value is clamped to its declared range.
pub fn apply_wire_param(params: &mut Params, index: u32, value: f32) -> bool {
    if !value.is_finite() || index as usize >= PARAM_COUNT {
        return false;
    }
    match index {
        POWER_INDEX => params.power = value >= 0.5,
        MODE_INDEX => params.mode = Mode::from_wire(value),
        THRESHOLD_INDEX => params.threshold_db = clamp_wire(index, value),
        SHAPE_INDEX => params.shape = clamp_wire(index, value),
        CEILING_INDEX => params.ceiling_db = clamp_wire(index, value),
        MIX_INDEX => params.mix = clamp_wire(index, value),
        STEREO_LINK_INDEX => params.stereo_link = value >= 0.5,
        DC_FILTER_INDEX => params.dc_filter = value >= 0.5,
        _ => return false,
    }
    true
}

/// Resolve a string id off the realtime path and apply it to a state mirror.
pub fn apply_ui_param(params: &mut Params, id: &str, value: f32) -> bool {
    let Some(index) = ui_param_index(id) else {
        return false;
    };
    apply_wire_param(params, index, value)
}

/// Every parameter as `(id, raw value)`, in wire order. Drives project replay
/// and the descriptor-vs-defaults check.
pub fn ui_values(params: &Params) -> Vec<(&'static str, f32)> {
    vec![
        ("power", f32::from(params.power)),
        ("mode", params.mode.to_wire()),
        ("thresholdDb", params.threshold_db),
        ("shape", params.shape),
        ("ceilingDb", params.ceiling_db),
        ("mix", params.mix),
        ("stereoLink", f32::from(params.stereo_link)),
        ("dcFilter", f32::from(params.dc_filter)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;

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
        assert!(apply_ui_param(&mut params, "thresholdDb", -12.0));
        assert!(apply_ui_param(&mut params, "mode", Mode::Limit.to_wire()));
        let json = Clipper67State::new(params).to_json().unwrap();
        let decoded = Clipper67State::from_json(&json).unwrap();
        assert_eq!(decoded.params.threshold_db, -12.0);
        assert_eq!(decoded.params.mode, Mode::Limit);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "thresholdDb", -40.0));
        assert_eq!(params.threshold_db, -24.0);
        assert!(apply_ui_param(&mut params, "shape", 150.0));
        assert_eq!(params.shape, 100.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, THRESHOLD_INDEX, f32::NAN));
        assert!(!apply_wire_param(&mut params, THRESHOLD_INDEX, f32::INFINITY));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    #[test]
    fn sanitize_pulls_a_hand_edited_blob_back_into_range() {
        let mut params = default_params();
        params.threshold_db = -40.0;
        params.ceiling_db = -12.0;
        params.shape = 200.0;
        sanitize_params(&mut params);
        assert_eq!(params.threshold_db, -24.0);
        assert_eq!(params.ceiling_db, -6.0);
        assert_eq!(params.shape, 100.0);
    }

    #[test]
    fn mode_wire_values_round_trip() {
        for mode in [Mode::Clip, Mode::Hybrid, Mode::Limit] {
            assert_eq!(Mode::from_wire(mode.to_wire()), mode);
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
    }
}
