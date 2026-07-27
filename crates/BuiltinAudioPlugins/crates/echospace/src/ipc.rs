//! Stable editor/DSP parameter wire contract for EchoSpace.
//!
//! String ids live at the UI/control boundary only. The native host resolves
//! them to compact indices before publishing edits to an audio-side bounded
//! queue; [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the
//! numeric form without allocation, serialization, locking, or string lookup.

use serde::{Deserialize, Serialize};

use crate::{DelayMode, MAX_DELAY_MS, Params, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const MODE_INDEX: u32 = 1;
pub const TIME_L_INDEX: u32 = 2;
pub const TIME_R_INDEX: u32 = 3;
pub const FEEDBACK_INDEX: u32 = 4;
pub const CROSS_INDEX: u32 = 5;
pub const LOW_CUT_INDEX: u32 = 6;
pub const HIGH_CUT_INDEX: u32 = 7;
pub const SATURATION_INDEX: u32 = 8;
pub const MIX_INDEX: u32 = 9;
pub const OUTPUT_INDEX: u32 = 10;
pub const FREEZE_INDEX: u32 = 11;

pub const PARAM_COUNT: usize = 12;

/// Wire index *is* the position in this table; the editor and the host both
/// resolve through it, so the order is part of the persisted contract. Append
/// only.
pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = [
    "power",
    "mode",
    "timeMsL",
    "timeMsR",
    "feedback",
    "crossFeedback",
    "lowCutHz",
    "highCutHz",
    "saturation",
    "mix",
    "outputDb",
    "freeze",
];

/// Inclusive `(min, max)` for every continuous parameter, indexed by wire
/// index. Booleans and the mode enum carry `(0, 0)` and are handled by their
/// own arms in [`apply_wire_param`]. Single source of truth for clamping, so
/// [`sanitize_params`] and the wire path cannot drift apart.
const RANGES: [(f32, f32); PARAM_COUNT] = [
    (0.0, 0.0),          // power
    (0.0, 0.0),          // mode
    (1.0, MAX_DELAY_MS), // timeMsL
    (1.0, MAX_DELAY_MS), // timeMsR
    (0.0, 98.0),         // feedback
    (0.0, 100.0),        // crossFeedback
    (20.0, 2_000.0),     // lowCutHz
    (1_000.0, 20_000.0), // highCutHz
    (0.0, 100.0),        // saturation
    (0.0, 100.0),        // mix
    (-24.0, 12.0),       // outputDb
    (0.0, 0.0),          // freeze
];

#[inline]
fn clamp_wire(index: u32, value: f32) -> f32 {
    let (min, max) = RANGES[index as usize];
    clamp(value, min, max)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EchospaceState {
    pub version: u32,
    pub params: Params,
}

impl EchospaceState {
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

impl Default for EchospaceState {
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
    params.time_ms_l = clamp_wire(TIME_L_INDEX, params.time_ms_l);
    params.time_ms_r = clamp_wire(TIME_R_INDEX, params.time_ms_r);
    params.feedback = clamp_wire(FEEDBACK_INDEX, params.feedback);
    params.cross_feedback = clamp_wire(CROSS_INDEX, params.cross_feedback);
    params.low_cut_hz = clamp_wire(LOW_CUT_INDEX, params.low_cut_hz);
    params.high_cut_hz = clamp_wire(HIGH_CUT_INDEX, params.high_cut_hz);
    params.saturation = clamp_wire(SATURATION_INDEX, params.saturation);
    params.mix = clamp_wire(MIX_INDEX, params.mix);
    params.output_db = clamp_wire(OUTPUT_INDEX, params.output_db);
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
        MODE_INDEX => params.mode = DelayMode::from_wire(value),
        FREEZE_INDEX => params.freeze = value >= 0.5,
        TIME_L_INDEX => params.time_ms_l = clamp_wire(index, value),
        TIME_R_INDEX => params.time_ms_r = clamp_wire(index, value),
        FEEDBACK_INDEX => params.feedback = clamp_wire(index, value),
        CROSS_INDEX => params.cross_feedback = clamp_wire(index, value),
        LOW_CUT_INDEX => params.low_cut_hz = clamp_wire(index, value),
        HIGH_CUT_INDEX => params.high_cut_hz = clamp_wire(index, value),
        SATURATION_INDEX => params.saturation = clamp_wire(index, value),
        MIX_INDEX => params.mix = clamp_wire(index, value),
        OUTPUT_INDEX => params.output_db = clamp_wire(index, value),
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
        ("timeMsL", params.time_ms_l),
        ("timeMsR", params.time_ms_r),
        ("feedback", params.feedback),
        ("crossFeedback", params.cross_feedback),
        ("lowCutHz", params.low_cut_hz),
        ("highCutHz", params.high_cut_hz),
        ("saturation", params.saturation),
        ("mix", params.mix),
        ("outputDb", params.output_db),
        ("freeze", f32::from(params.freeze)),
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
        assert!(apply_ui_param(&mut params, "timeMsL", 250.0));
        assert!(apply_ui_param(
            &mut params,
            "mode",
            DelayMode::Mono.to_wire()
        ));
        let json = EchospaceState::new(params).to_json().unwrap();
        let decoded = EchospaceState::from_json(&json).unwrap();
        assert_eq!(decoded.params.time_ms_l, 250.0);
        assert_eq!(decoded.params.mode, DelayMode::Mono);
        assert_eq!(decoded.version, STATE_VERSION);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "timeMsL", 99_000.0));
        assert_eq!(params.time_ms_l, MAX_DELAY_MS);
        // The feedback ceiling is below 100 % on purpose — the delay line is a
        // feedback loop and unity would never decay.
        assert!(apply_ui_param(&mut params, "feedback", 100.0));
        assert_eq!(params.feedback, 98.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, TIME_L_INDEX, f32::NAN));
        assert!(!apply_wire_param(&mut params, TIME_L_INDEX, f32::INFINITY));
        assert!(!apply_ui_param(&mut params, "notAParam", 1.0));
    }

    /// A blob written by an older build (or edited by hand) must not reach the
    /// DSP with values the coefficient math cannot survive.
    #[test]
    fn sanitize_pulls_a_hand_edited_blob_back_into_range() {
        let mut params = default_params();
        params.time_ms_l = -20.0;
        params.feedback = 400.0;
        params.high_cut_hz = 96_000.0;
        params.output_db = 60.0;
        sanitize_params(&mut params);
        assert_eq!(params.time_ms_l, 1.0);
        assert_eq!(params.feedback, 98.0);
        assert_eq!(params.high_cut_hz, 20_000.0);
        assert_eq!(params.output_db, 12.0);
    }

    #[test]
    fn mode_wire_values_round_trip() {
        for mode in [DelayMode::Stereo, DelayMode::PingPong, DelayMode::Mono] {
            assert_eq!(DelayMode::from_wire(mode.to_wire()), mode);
            assert_eq!(DelayMode::parse(mode.as_str()), Some(mode));
        }
    }
}
