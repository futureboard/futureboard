//! Stable editor/DSP parameter wire contract for EQ-ZX.
//!
//! Same shape as EQ-Z8's contract: string ids live at the UI/control boundary
//! only. The native host resolves them to compact indices before publishing
//! edits to an audio-side bounded queue;
//! [`Dsp::apply_wire_param`](crate::Dsp::apply_wire_param) consumes the numeric
//! form without allocation, serialization, locking, or string lookup.
//!
//! # Layout
//!
//! ```text
//! 0                       power
//! 1                       outputDb
//! 2                       soloBand
//! 3   .. 3+24*7           per-band static block  (stride 7)
//! 171 .. 171+24*6         per-band dynamics block (stride 6)
//! ```
//!
//! Both per-band blocks are addressed by arithmetic on their base index, so new
//! parameters must be **appended after** the existing blocks, never inserted
//! among them — inserting would renumber every band and silently repoint edits
//! that are already in flight. This is the same rule EQ-Z8 documents on its
//! `SOLO_INDEX`, and the reason solo sits at a fixed global index here rather
//! than being tacked on later.

use serde::{Deserialize, Serialize};

use crate::{BAND_COUNT, BandChannel, BandType, DynMode, Params, clamp, default_params};

pub const PROTOCOL_VERSION: u32 = 1;
pub const STATE_VERSION: u32 = 1;

pub const POWER_INDEX: u32 = 0;
pub const OUTPUT_INDEX: u32 = 1;
pub const SOLO_INDEX: u32 = 2;

pub const BAND_BASE_INDEX: u32 = 3;
pub const BAND_STRIDE: u32 = 7;

pub const BAND_ENABLED: u32 = 0;
pub const BAND_TYPE: u32 = 1;
pub const BAND_FREQ: u32 = 2;
pub const BAND_GAIN: u32 = 3;
pub const BAND_Q: u32 = 4;
pub const BAND_SLOPE: u32 = 5;
pub const BAND_CHANNEL: u32 = 6;

pub const DYN_BASE_INDEX: u32 = BAND_BASE_INDEX + BAND_COUNT as u32 * BAND_STRIDE;
pub const DYN_STRIDE: u32 = 6;

pub const BAND_DYN_ENABLED: u32 = 0;
pub const BAND_DYN_MODE: u32 = 1;
pub const BAND_DYN_THRESHOLD: u32 = 2;
pub const BAND_DYN_RANGE: u32 = 3;
pub const BAND_DYN_ATTACK: u32 = 4;
pub const BAND_DYN_RELEASE: u32 = 5;

pub const PARAM_COUNT: usize = DYN_BASE_INDEX as usize + BAND_COUNT * DYN_STRIDE as usize;

/// `solo_band` value meaning "no band is auditioned".
pub const SOLO_NONE: i32 = -1;

// --- parameter value ranges ---------------------------------------------
//
// One table, used by both `apply_wire_param` and `sanitize_params`, so a value
// arriving over the wire and a value arriving in restored state are clamped
// identically. Anything the editor can produce outside these bounds is pinned
// here rather than reaching the filter design.

pub const FREQ_MIN: f32 = 20.0;
pub const FREQ_MAX: f32 = 20_000.0;
/// Gain and Q stay inside the envelope `builtin_dsp_core` documents as stable
/// (see `MAX_FILTER_FREQUENCY_RATIO`'s note: Q = 12 at ±18 dB already puts the
/// poles near radius 0.998). The shared coefficient builder clamps Q to this
/// range internally regardless, so widening it here would only desynchronise
/// the editor's drawn curve from the filter that actually runs.
pub const GAIN_MIN_DB: f32 = -18.0;
pub const GAIN_MAX_DB: f32 = 18.0;
pub const Q_MIN: f32 = 0.1;
pub const Q_MAX: f32 = 12.0;
pub const THRESHOLD_MIN_DB: f32 = -60.0;
pub const THRESHOLD_MAX_DB: f32 = 0.0;
pub const RANGE_MIN_DB: f32 = -24.0;
pub const RANGE_MAX_DB: f32 = 24.0;
pub const ATTACK_MIN_MS: f32 = 0.1;
pub const ATTACK_MAX_MS: f32 = 500.0;
pub const RELEASE_MIN_MS: f32 = 1.0;
pub const RELEASE_MAX_MS: f32 = 5_000.0;
pub const OUTPUT_MIN_DB: f32 = -24.0;
pub const OUTPUT_MAX_DB: f32 = 12.0;

/// Cut slopes the filter cascade can realize, in dB/oct. Only even filter
/// orders exist, so every entry is a multiple of 12.
pub const SLOPES: [f32; 6] = [12.0, 24.0, 36.0, 48.0, 72.0, 96.0];

/// Snap an arbitrary wire value to the nearest supported slope.
///
/// Rounding rather than rejecting keeps a host that automates the slope from
/// wedging the band: any finite value lands on a realizable cascade.
pub fn snap_slope(value: f32) -> f32 {
    let mut best = SLOPES[0];
    let mut best_distance = f32::INFINITY;
    for slope in SLOPES {
        let distance = (slope - value).abs();
        if distance < best_distance {
            best_distance = distance;
            best = slope;
        }
    }
    best
}

// --- the string id table -------------------------------------------------
//
// 315 ids is far too many to hand-write without a transcription error, and the
// host resolves them by exact string match. Generating them from one band-number
// list makes a typo impossible and keeps the ordering locked to the strides
// above; `ids_round_trip_to_wire_indices` proves the two agree.

macro_rules! band_ids {
    ($($n:literal),* $(,)?) => {
        [$(
            concat!("band", $n, "_enabled"),
            concat!("band", $n, "_type"),
            concat!("band", $n, "_freq"),
            concat!("band", $n, "_gainDb"),
            concat!("band", $n, "_q"),
            concat!("band", $n, "_slope"),
            concat!("band", $n, "_channel"),
        )*]
    };
}

macro_rules! dyn_ids {
    ($($n:literal),* $(,)?) => {
        [$(
            concat!("band", $n, "_dynEnabled"),
            concat!("band", $n, "_dynMode"),
            concat!("band", $n, "_thresholdDb"),
            concat!("band", $n, "_rangeDb"),
            concat!("band", $n, "_attackMs"),
            concat!("band", $n, "_releaseMs"),
        )*]
    };
}

const BAND_IDS: [&str; BAND_COUNT * BAND_STRIDE as usize] = band_ids!(
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24
);

const DYN_IDS: [&str; BAND_COUNT * DYN_STRIDE as usize] = dyn_ids!(
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24
);

const fn build_ui_param_ids() -> [&'static str; PARAM_COUNT] {
    let mut ids = [""; PARAM_COUNT];
    ids[POWER_INDEX as usize] = "power";
    ids[OUTPUT_INDEX as usize] = "outputDb";
    ids[SOLO_INDEX as usize] = "soloBand";

    let mut i = 0;
    while i < BAND_IDS.len() {
        ids[BAND_BASE_INDEX as usize + i] = BAND_IDS[i];
        i += 1;
    }
    let mut i = 0;
    while i < DYN_IDS.len() {
        ids[DYN_BASE_INDEX as usize + i] = DYN_IDS[i];
        i += 1;
    }
    ids
}

pub const UI_PARAM_IDS: [&str; PARAM_COUNT] = build_ui_param_ids();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EquzxState {
    pub version: u32,
    pub params: Params,
}

impl EquzxState {
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

impl Default for EquzxState {
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

pub const fn band_wire_index(band: usize, field: u32) -> u32 {
    BAND_BASE_INDEX + band as u32 * BAND_STRIDE + field
}

pub const fn dyn_wire_index(band: usize, field: u32) -> u32 {
    DYN_BASE_INDEX + band as u32 * DYN_STRIDE + field
}

pub fn decode_band_wire(index: u32) -> Option<(usize, u32)> {
    if index < BAND_BASE_INDEX || index >= DYN_BASE_INDEX {
        return None;
    }
    let offset = index - BAND_BASE_INDEX;
    let band = (offset / BAND_STRIDE) as usize;
    let field = offset % BAND_STRIDE;
    (band < BAND_COUNT).then_some((band, field))
}

pub fn decode_dyn_wire(index: u32) -> Option<(usize, u32)> {
    let offset = index.checked_sub(DYN_BASE_INDEX)?;
    let band = (offset / DYN_STRIDE) as usize;
    let field = offset % DYN_STRIDE;
    (band < BAND_COUNT).then_some((band, field))
}

pub fn sanitize_params(params: &mut Params) {
    params.output_db = clamp(params.output_db, OUTPUT_MIN_DB, OUTPUT_MAX_DB);
    if params.solo_band < 0 || params.solo_band >= BAND_COUNT as i32 {
        params.solo_band = SOLO_NONE;
    }
    for band in &mut params.bands {
        band.freq = clamp(band.freq, FREQ_MIN, FREQ_MAX);
        band.gain_db = clamp(band.gain_db, GAIN_MIN_DB, GAIN_MAX_DB);
        band.q = clamp(band.q, Q_MIN, Q_MAX);
        band.slope = snap_slope(band.slope);
        band.threshold_db = clamp(band.threshold_db, THRESHOLD_MIN_DB, THRESHOLD_MAX_DB);
        band.range_db = clamp(band.range_db, RANGE_MIN_DB, RANGE_MAX_DB);
        band.attack_ms = clamp(band.attack_ms, ATTACK_MIN_MS, ATTACK_MAX_MS);
        band.release_ms = clamp(band.release_ms, RELEASE_MIN_MS, RELEASE_MAX_MS);
        if !band.band_type.has_gain() {
            band.dynamic = false;
        }
    }
}

/// Decode a wire `soloBand` value into a validated band index.
///
/// Anything outside `0..BAND_COUNT` clears solo rather than being rejected, so
/// a UI can turn solo off by sending `-1` and can never wedge the DSP into
/// auditioning a band that does not exist.
pub fn decode_solo_band(value: f32) -> i32 {
    let index = value.round() as i32;
    if index < 0 || index >= BAND_COUNT as i32 {
        SOLO_NONE
    } else {
        index
    }
}

/// Apply one compact UI/control update. This is allocation-free and total:
/// invalid indices are rejected and all continuous values are clamped.
pub fn apply_wire_param(params: &mut Params, index: u32, value: f32) -> bool {
    if !value.is_finite() {
        return false;
    }
    match index {
        POWER_INDEX => params.power = value >= 0.5,
        OUTPUT_INDEX => params.output_db = clamp(value, OUTPUT_MIN_DB, OUTPUT_MAX_DB),
        SOLO_INDEX => params.solo_band = decode_solo_band(value),
        _ => {
            if let Some((band_index, field)) = decode_band_wire(index) {
                let band = &mut params.bands[band_index];
                match field {
                    BAND_ENABLED => band.active = value >= 0.5,
                    BAND_TYPE => {
                        band.band_type = BandType::from_wire(value);
                        if !band.band_type.has_gain() {
                            band.dynamic = false;
                        }
                    }
                    BAND_FREQ => band.freq = clamp(value, FREQ_MIN, FREQ_MAX),
                    BAND_GAIN => band.gain_db = clamp(value, GAIN_MIN_DB, GAIN_MAX_DB),
                    BAND_Q => band.q = clamp(value, Q_MIN, Q_MAX),
                    BAND_SLOPE => band.slope = snap_slope(value),
                    BAND_CHANNEL => band.channel = BandChannel::from_wire(value),
                    _ => return false,
                }
            } else if let Some((band_index, field)) = decode_dyn_wire(index) {
                let band = &mut params.bands[band_index];
                match field {
                    BAND_DYN_ENABLED => {
                        band.dynamic = value >= 0.5 && band.band_type.has_gain();
                    }
                    BAND_DYN_MODE => band.dyn_mode = DynMode::from_wire(value),
                    BAND_DYN_THRESHOLD => {
                        band.threshold_db = clamp(value, THRESHOLD_MIN_DB, THRESHOLD_MAX_DB);
                    }
                    BAND_DYN_RANGE => band.range_db = clamp(value, RANGE_MIN_DB, RANGE_MAX_DB),
                    BAND_DYN_ATTACK => band.attack_ms = clamp(value, ATTACK_MIN_MS, ATTACK_MAX_MS),
                    BAND_DYN_RELEASE => {
                        band.release_ms = clamp(value, RELEASE_MIN_MS, RELEASE_MAX_MS);
                    }
                    _ => return false,
                }
            } else {
                return false;
            }
        }
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

/// Full parameter snapshot in wire order, for pushing initial editor state.
pub fn ui_values(params: &Params) -> Vec<(&'static str, f32)> {
    let mut values = Vec::with_capacity(PARAM_COUNT);
    values.push(("power", f32::from(params.power)));
    values.push(("outputDb", params.output_db));
    values.push(("soloBand", params.solo_band as f32));
    for (index, band) in params.bands.iter().enumerate() {
        let base = BAND_BASE_INDEX as usize + index * BAND_STRIDE as usize;
        values.push((UI_PARAM_IDS[base], f32::from(band.active)));
        values.push((UI_PARAM_IDS[base + 1], band.band_type.to_wire()));
        values.push((UI_PARAM_IDS[base + 2], band.freq));
        values.push((UI_PARAM_IDS[base + 3], band.gain_db));
        values.push((UI_PARAM_IDS[base + 4], band.q));
        values.push((UI_PARAM_IDS[base + 5], band.slope));
        values.push((UI_PARAM_IDS[base + 6], band.channel.to_wire()));
    }
    for (index, band) in params.bands.iter().enumerate() {
        let base = DYN_BASE_INDEX as usize + index * DYN_STRIDE as usize;
        values.push((UI_PARAM_IDS[base], f32::from(band.dynamic)));
        values.push((UI_PARAM_IDS[base + 1], band.dyn_mode.to_wire()));
        values.push((UI_PARAM_IDS[base + 2], band.threshold_db));
        values.push((UI_PARAM_IDS[base + 3], band.range_db));
        values.push((UI_PARAM_IDS[base + 4], band.attack_ms));
        values.push((UI_PARAM_IDS[base + 5], band.release_ms));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_to_wire_indices() {
        assert_eq!(UI_PARAM_IDS.len(), PARAM_COUNT);
        for (index, id) in UI_PARAM_IDS.iter().enumerate() {
            assert!(!id.is_empty(), "index {index} has no id");
            assert_eq!(ui_param_index(id), Some(index as u32));
            assert_eq!(ui_param_id(index as u32), Some(*id));
        }
    }

    /// The generated table has to line up with the arithmetic the decoders use.
    #[test]
    fn generated_ids_match_the_stride_arithmetic() {
        assert_eq!(ui_param_index("band1_enabled"), Some(BAND_BASE_INDEX));
        assert_eq!(
            ui_param_index("band1_channel"),
            Some(band_wire_index(0, BAND_CHANNEL))
        );
        assert_eq!(
            ui_param_index("band24_channel"),
            Some(band_wire_index(BAND_COUNT - 1, BAND_CHANNEL))
        );
        assert_eq!(ui_param_index("band1_dynEnabled"), Some(DYN_BASE_INDEX));
        assert_eq!(
            ui_param_index("band24_releaseMs"),
            Some(dyn_wire_index(BAND_COUNT - 1, BAND_DYN_RELEASE))
        );
    }

    #[test]
    fn band_and_dyn_blocks_do_not_overlap() {
        assert_eq!(
            band_wire_index(BAND_COUNT - 1, BAND_CHANNEL),
            DYN_BASE_INDEX - 1
        );
        for index in 0..PARAM_COUNT as u32 {
            let band = decode_band_wire(index);
            let dynamics = decode_dyn_wire(index);
            assert!(
                band.is_none() || dynamics.is_none(),
                "index {index} decodes as both a band and a dynamics field"
            );
        }
    }

    #[test]
    fn state_round_trips() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "band4_enabled", 1.0));
        assert!(apply_ui_param(&mut params, "band4_gainDb", -4.5));
        assert!(apply_ui_param(&mut params, "band4_channel", 2.0));
        assert!(apply_ui_param(&mut params, "band4_dynEnabled", 1.0));
        assert!(apply_ui_param(&mut params, "band4_rangeDb", -6.0));
        let json = EquzxState::new(params).to_json().unwrap();
        let decoded = EquzxState::from_json(&json).unwrap();
        assert_eq!(decoded.params.bands[3].gain_db, -4.5);
        assert_eq!(decoded.params.bands[3].channel, BandChannel::Side);
        assert!(decoded.params.bands[3].dynamic);
        assert_eq!(decoded.params.bands[3].range_db, -6.0);
    }

    /// Every field the mid/side and slope work added carries `serde(default)`,
    /// so a state written before they existed still loads.
    #[test]
    fn state_without_optional_fields_still_loads() {
        let json = r#"{"version":1,"params":{"power":true,"outputDb":0.0,"bands":[]}}"#;
        let decoded = EquzxState::from_json(json).expect("sparse state must load");
        assert_eq!(decoded.params.solo_band, SOLO_NONE);
        assert!(decoded.params.bands.iter().all(|band| !band.active));
    }

    #[test]
    fn solo_index_is_validated_not_trusted() {
        let mut params = default_params();
        assert!(apply_wire_param(&mut params, SOLO_INDEX, 3.0));
        assert_eq!(params.solo_band, 3);
        for bogus in [-5.0, BAND_COUNT as f32, 999.0] {
            assert!(apply_wire_param(&mut params, SOLO_INDEX, bogus));
            assert_eq!(params.solo_band, SOLO_NONE, "{bogus} should clear solo");
        }
    }

    #[test]
    fn slopes_snap_to_realizable_cascades() {
        assert_eq!(snap_slope(11.0), 12.0);
        assert_eq!(snap_slope(30.0), 24.0);
        assert_eq!(snap_slope(1_000.0), 96.0);
        assert_eq!(snap_slope(-40.0), 12.0);
    }

    #[test]
    fn invalid_and_non_finite_updates_are_rejected() {
        let mut params = default_params();
        assert!(!apply_wire_param(&mut params, u32::MAX, 1.0));
        assert!(!apply_wire_param(&mut params, PARAM_COUNT as u32, 1.0));
        assert!(!apply_wire_param(&mut params, BAND_BASE_INDEX, f32::NAN));
    }

    /// Dynamics only mean something on a shape with a gain stage; turning a
    /// dynamic band into a cut has to drop the flag rather than leave a
    /// detector running against a filter that cannot answer it.
    #[test]
    fn changing_to_a_gainless_shape_clears_dynamics() {
        let mut params = default_params();
        assert!(apply_ui_param(&mut params, "band1_dynEnabled", 1.0));
        assert!(params.bands[0].dynamic);
        assert!(apply_ui_param(
            &mut params,
            "band1_type",
            BandType::LowCut.to_wire()
        ));
        assert!(!params.bands[0].dynamic);
    }
}
