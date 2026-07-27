//! Stable editor/DSP parameter wire contract for FA-2A.
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
pub const PEAK_REDUCTION_INDEX: u32 = 2;
pub const GAIN_INDEX: u32 = 3;
pub const EMPHASIS_INDEX: u32 = 4;
pub const MIX_INDEX: u32 = 5;
pub const COLOR_INDEX: u32 = 6;
pub const SIDECHAIN_INDEX: u32 = 7;
pub const OUTPUT_TRIM_INDEX: u32 = 8;

pub const PARAM_COUNT: usize = 9;

/// Wire index *is* the position in this table; the editor and the host both
/// resolve through it, so the order is part of the persisted contract. Append
/// only.
pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = [
    "power",
    "mode",
    "peakReduction",
    "gainDb",
    "emphasis",
    "mix",
    "color",
    "sidechainLowCutHz",
    "outputTrimDb",
];

/// Inclusive `(min, max)` for every continuous parameter, indexed by wire
/// index. Booleans and the mode enum carry `(0, 0)` and are handled by their
/// own arms in [`apply_wire_param`]. Single source of truth for clamping, so
/// [`sanitize_params`] and the wire path cannot drift apart.
const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),    // power
    (0.0, 0.0),    // mode
    (0.0, 100.0),  // peakReduction
    (-12.0, 24.0), // gainDb
    (0.0, 100.0),  // emphasis
    (0.0, 100.0),  // mix
    (0.0, 100.0),  // color
    (20.0, 500.0), // sidechainLowCutHz
    (-12.0, 12.0), // outputTrimDb
];

#[inline]
fn clamp_wire(index: u32, value: f32) -> f32 {
    let (min, max) = RANGES[index as usize];
    clamp(value, min, max)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fa2aState {
    pub version: u32,
    pub params: Params,
}

impl Fa2aState {
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

impl Default for Fa2aState {
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
    params.peak_reduction = clamp_wire(PEAK_REDUCTION_INDEX, params.peak_reduction);
    params.gain_db = clamp_wire(GAIN_INDEX, params.gain_db);
    params.emphasis = clamp_wire(EMPHASIS_INDEX, params.emphasis);
    params.mix = clamp_wire(MIX_INDEX, params.mix);
    params.color = clamp_wire(COLOR_INDEX, params.color);
    params.sidechain_low_cut_hz = clamp_wire(SIDECHAIN_INDEX, params.sidechain_low_cut_hz);
    params.output_trim_db = clamp_wire(OUTPUT_TRIM_INDEX, params.output_trim_db);
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
        PEAK_REDUCTION_INDEX => params.peak_reduction = clamp_wire(index, value),
        GAIN_INDEX => params.gain_db = clamp_wire(index, value),
        EMPHASIS_INDEX => params.emphasis = clamp_wire(index, value),
        MIX_INDEX => params.mix = clamp_wire(index, value),
        COLOR_INDEX => params.color = clamp_wire(index, value),
        SIDECHAIN_INDEX => params.sidechain_low_cut_hz = clamp_wire(index, value),
        OUTPUT_TRIM_INDEX => params.output_trim_db = clamp_wire(index, value),
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
        ("peakReduction", params.peak_reduction),
        ("gainDb", params.gain_db),
        ("emphasis", params.emphasis),
        ("mix", params.mix),
        ("color", params.color),
        ("sidechainLowCutHz", params.sidechain_low_cut_hz),
        ("outputTrimDb", params.output_trim_db),
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

    /// `ui_values` is what project replay pushes back through the wire, so a
    /// missing or misordered entry would silently drop a parameter on reload.
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
        assert!(apply_ui_param(&mut params, "peakReduction", 72.0));
        assert!(apply_ui_param(&mut params, "mode", Mode::Limit.to_wire()));
        let json = Fa2aState::new(params).to_json().unwrap();
        let decoded = Fa2aState::from_json(&json).unwrap();
        assert_eq!(decoded.params.peak_reduction, 72.0);
        assert_eq!(decoded.params.mode, Mode::Limit);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "gainDb", 900.0));
        assert_eq!(params.gain_db, 24.0);
        assert!(apply_ui_param(&mut params, "peakReduction", -5.0));
        assert_eq!(params.peak_reduction, 0.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, GAIN_INDEX, f32::NAN));
        assert!(!apply_wire_param(&mut params, GAIN_INDEX, f32::INFINITY));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    /// A blob written by an older build (or edited by hand) must not reach the
    /// DSP with values the coefficient math cannot survive.
    #[test]
    fn sanitize_pulls_a_hand_edited_blob_back_into_range() {
        let mut params = default_params();
        params.peak_reduction = 900.0;
        params.gain_db = -80.0;
        params.sidechain_low_cut_hz = 5.0;
        params.output_trim_db = 40.0;
        sanitize_params(&mut params);
        assert_eq!(params.peak_reduction, 100.0);
        assert_eq!(params.gain_db, -12.0);
        assert_eq!(params.sidechain_low_cut_hz, 20.0);
        assert_eq!(params.output_trim_db, 12.0);
    }

    #[test]
    fn mode_wire_values_round_trip() {
        for mode in [Mode::Compress, Mode::Limit] {
            assert_eq!(Mode::from_wire(mode.to_wire()), mode);
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
    }
}
