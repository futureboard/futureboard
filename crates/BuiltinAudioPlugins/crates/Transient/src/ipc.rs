//! Stable editor/DSP parameter wire contract for Transient.
//!
//! String ids live at the UI/control boundary only. The native host resolves
//! them to compact indices before publishing edits to an audio-side bounded
//! queue; [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the
//! numeric form without allocation, serialization, locking, or string lookup.

use serde::{Deserialize, Serialize};

use crate::{Params, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const ATTACK_INDEX: u32 = 1;
pub const SUSTAIN_INDEX: u32 = 2;
pub const SPEED_INDEX: u32 = 3;
pub const MIX_INDEX: u32 = 4;
pub const STEREO_LINK_INDEX: u32 = 5;

pub const PARAM_COUNT: usize = 6;

/// Wire index *is* the position in this table; the editor and the host both
/// resolve through it, so the order is part of the persisted contract. Append
/// only.
pub const UI_PARAM_IDS: [&str; PARAM_COUNT] =
    ["power", "attack", "sustain", "speed", "mix", "stereoLink"];

/// Inclusive `(min, max)` for every continuous parameter, indexed by wire
/// index. Booleans carry `(0, 0)` and are handled by their own arms in
/// [`apply_wire_param`]. Single source of truth for clamping, so
/// [`sanitize_params`] and the wire path cannot drift apart.
const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),      // power
    (-100.0, 100.0), // attack
    (-100.0, 100.0), // sustain
    (0.0, 100.0),    // speed
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
pub struct TransientState {
    pub version: u32,
    pub params: Params,
}

impl TransientState {
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

impl Default for TransientState {
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
    params.attack = clamp_wire(ATTACK_INDEX, params.attack);
    params.sustain = clamp_wire(SUSTAIN_INDEX, params.sustain);
    params.speed = clamp_wire(SPEED_INDEX, params.speed);
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
        ATTACK_INDEX => params.attack = clamp_wire(index, value),
        SUSTAIN_INDEX => params.sustain = clamp_wire(index, value),
        SPEED_INDEX => params.speed = clamp_wire(index, value),
        MIX_INDEX => params.mix = clamp_wire(index, value),
        STEREO_LINK_INDEX => params.stereo_link = value >= 0.5,
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
        ("attack", params.attack),
        ("sustain", params.sustain),
        ("speed", params.speed),
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
        assert!(apply_ui_param(&mut params, "attack", 40.0));
        assert!(apply_ui_param(&mut params, "sustain", -25.0));
        let json = TransientState::new(params).to_json().unwrap();
        let decoded = TransientState::from_json(&json).unwrap();
        assert_eq!(decoded.params.attack, 40.0);
        assert_eq!(decoded.params.sustain, -25.0);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "attack", 200.0));
        assert_eq!(params.attack, 100.0);
        assert!(apply_ui_param(&mut params, "sustain", -200.0));
        assert_eq!(params.sustain, -100.0);
        assert!(apply_ui_param(&mut params, "speed", -10.0));
        assert_eq!(params.speed, 0.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, ATTACK_INDEX, f32::NAN));
        assert!(!apply_wire_param(&mut params, ATTACK_INDEX, f32::INFINITY));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    #[test]
    fn sanitize_pulls_a_hand_edited_blob_back_into_range() {
        let mut params = default_params();
        params.attack = 150.0;
        params.sustain = -150.0;
        params.speed = 200.0;
        params.mix = -10.0;
        sanitize_params(&mut params);
        assert_eq!(params.attack, 100.0);
        assert_eq!(params.sustain, -100.0);
        assert_eq!(params.speed, 100.0);
        assert_eq!(params.mix, 0.0);
    }
}
