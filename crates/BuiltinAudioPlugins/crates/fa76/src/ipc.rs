//! Stable editor/DSP parameter wire contract for FA-76.
//!
//! String ids live at the UI/control boundary only. The native host resolves
//! them to compact indices before publishing edits to an audio-side bounded
//! queue; [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the
//! numeric form without allocation, serialization, locking, or string lookup.

use serde::{Deserialize, Serialize};

use crate::{Params, RatioButton, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const RATIO_INDEX: u32 = 1;
pub const INPUT_INDEX: u32 = 2;
pub const OUTPUT_INDEX: u32 = 3;
pub const ATTACK_INDEX: u32 = 4;
pub const RELEASE_INDEX: u32 = 5;
pub const MIX_INDEX: u32 = 6;
pub const SIDECHAIN_INDEX: u32 = 7;

pub const PARAM_COUNT: usize = 8;

/// Wire index *is* the position in this table; the editor and the host both
/// resolve through it, so the order is part of the persisted contract. Append
/// only.
pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = [
    "power",
    "ratio",
    "inputDb",
    "outputDb",
    "attackUs",
    "releaseMs",
    "mix",
    "sidechainHpfHz",
];

/// Inclusive `(min, max)` for every continuous parameter, indexed by wire
/// index. Booleans and the ratio enum carry `(0, 0)` and are handled by their
/// own arms in [`apply_wire_param`]. Single source of truth for clamping, so
/// [`sanitize_params`] and the wire path cannot drift apart.
const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),     // power
    (0.0, 0.0),     // ratio
    (-12.0, 36.0),  // inputDb
    (-36.0, 12.0),  // outputDb
    (20.0, 800.0),  // attackUs
    (50.0, 1_100.0), // releaseMs
    (0.0, 100.0),   // mix
    (0.0, 500.0),   // sidechainHpfHz
];

#[inline]
fn clamp_wire(index: u32, value: f32) -> f32 {
    let (min, max) = RANGES[index as usize];
    clamp(value, min, max)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fa76State {
    pub version: u32,
    pub params: Params,
}

impl Fa76State {
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

impl Default for Fa76State {
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
    params.input_db = clamp_wire(INPUT_INDEX, params.input_db);
    params.output_db = clamp_wire(OUTPUT_INDEX, params.output_db);
    params.attack_us = clamp_wire(ATTACK_INDEX, params.attack_us);
    params.release_ms = clamp_wire(RELEASE_INDEX, params.release_ms);
    params.mix = clamp_wire(MIX_INDEX, params.mix);
    params.sidechain_hpf_hz = clamp_wire(SIDECHAIN_INDEX, params.sidechain_hpf_hz);
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
        RATIO_INDEX => params.ratio = RatioButton::from_wire(value),
        INPUT_INDEX => params.input_db = clamp_wire(index, value),
        OUTPUT_INDEX => params.output_db = clamp_wire(index, value),
        ATTACK_INDEX => params.attack_us = clamp_wire(index, value),
        RELEASE_INDEX => params.release_ms = clamp_wire(index, value),
        MIX_INDEX => params.mix = clamp_wire(index, value),
        SIDECHAIN_INDEX => params.sidechain_hpf_hz = clamp_wire(index, value),
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
        ("ratio", params.ratio.to_wire()),
        ("inputDb", params.input_db),
        ("outputDb", params.output_db),
        ("attackUs", params.attack_us),
        ("releaseMs", params.release_ms),
        ("mix", params.mix),
        ("sidechainHpfHz", params.sidechain_hpf_hz),
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
        assert!(apply_ui_param(&mut params, "inputDb", 24.0));
        assert!(apply_ui_param(
            &mut params,
            "ratio",
            RatioButton::All.to_wire()
        ));
        let json = Fa76State::new(params).to_json().unwrap();
        let decoded = Fa76State::from_json(&json).unwrap();
        assert_eq!(decoded.params.input_db, 24.0);
        assert_eq!(decoded.params.ratio, RatioButton::All);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "inputDb", 900.0));
        assert_eq!(params.input_db, 36.0);
        assert!(apply_ui_param(&mut params, "attackUs", 1.0));
        assert_eq!(params.attack_us, 20.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, INPUT_INDEX, f32::NAN));
        assert!(!apply_wire_param(&mut params, INPUT_INDEX, f32::INFINITY));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    /// A blob written by an older build (or edited by hand) must not reach the
    /// DSP with values the coefficient math cannot survive.
    #[test]
    fn sanitize_pulls_a_hand_edited_blob_back_into_range() {
        let mut params = default_params();
        params.input_db = 900.0;
        params.output_db = -80.0;
        params.attack_us = 1.0;
        params.sidechain_hpf_hz = 900.0;
        sanitize_params(&mut params);
        assert_eq!(params.input_db, 36.0);
        assert_eq!(params.output_db, -36.0);
        assert_eq!(params.attack_us, 20.0);
        assert_eq!(params.sidechain_hpf_hz, 500.0);
    }

    #[test]
    fn ratio_wire_values_round_trip() {
        for ratio in [
            RatioButton::R4,
            RatioButton::R8,
            RatioButton::R12,
            RatioButton::R20,
            RatioButton::All,
        ] {
            assert_eq!(RatioButton::from_wire(ratio.to_wire()), ratio);
            assert_eq!(RatioButton::parse(ratio.as_str()), Some(ratio));
        }
    }
}
