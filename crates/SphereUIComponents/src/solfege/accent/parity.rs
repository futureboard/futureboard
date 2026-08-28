//! The training pipeline and this runtime must compute the same features.
//!
//! Two independent implementations of thirty-three musical features, in two
//! languages, in two repositories. They agree today; nothing but a test keeps
//! them agreeing, and a disagreement would not fail loudly — it would produce
//! plausible accents from a model that was asked a slightly different question
//! than the one it was trained on.
//!
//! The fixture is written by `neural/scripts/export_accent.py --parity-out` and
//! carries real phrases from the held-out split rather than random vectors:
//! random inputs would prove the arithmetic agrees while missing the place two
//! implementations actually drift, which is the feature extraction.

use serde::Deserialize;

use crate::components::timeline::timeline_state::MidiNoteState;

use super::features::{
    contexts_from_notes, note_feature_vector, ACCENT_INPUT_FEATURES, ACCENT_INPUT_SIZE,
};
use super::meter::Meter;
use super::rule::rule;

const FIXTURE: &str = include_str!("accent_parity.json");

/// Largest per-feature disagreement allowed.
///
/// The fixture is written to six decimal places from `f32` values that were
/// computed in `float64` on the Python side and `f32` here, so a few units in
/// the last place of an `f32` are expected. Anything larger is a difference in
/// what was computed, not in how it was rounded.
const FEATURE_TOLERANCE: f32 = 2.0e-4;

/// The rule is a dot product of nine `f32` in a different order on each side,
/// so its output accumulates a little more error than a single feature.
const ACCENT_TOLERANCE: f32 = 1.0e-3;

#[derive(Debug, Deserialize)]
struct Fixture {
    input_features: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    part_id: String,
    tempo_bpm: f32,
    time_signature: (u16, u16),
    notes: Vec<FixtureNote>,
    features: Vec<Vec<f32>>,
    rule_components: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct FixtureNote {
    pitch: u8,
    onset_beats: f32,
    duration_beats: f32,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("the parity fixture is valid JSON")
}

#[test]
fn the_feature_order_matches_the_training_pipeline() {
    let fixture = fixture();
    assert_eq!(
        fixture.input_features, ACCENT_INPUT_FEATURES,
        "the two implementations disagree about which feature is in which slot; \
         every weight in the shipped model is addressed by position"
    );
}

#[test]
fn every_feature_matches_the_training_pipeline_on_real_phrases() {
    let fixture = fixture();
    assert!(!fixture.cases.is_empty(), "the fixture carries no phrases");

    for case in &fixture.cases {
        // The fixture stores onsets from the start of the piece, which is what
        // a clip at beat zero containing notes at those beats reproduces.
        let notes: Vec<MidiNoteState> = case
            .notes
            .iter()
            .map(|note| MidiNoteState::new(note.pitch, note.onset_beats, note.duration_beats, 96))
            .collect();
        let contexts = contexts_from_notes(&notes, 0.0, case.tempo_bpm);
        let meter = Meter::from_signature(case.time_signature.0, case.time_signature.1);
        let durations: Vec<f32> = contexts.iter().map(|c| c.duration_beats).collect();
        let pitches: Vec<f32> = contexts.iter().map(|c| c.pitch as f32).collect();

        for (index, expected) in case.features.iter().enumerate() {
            assert_eq!(expected.len(), ACCENT_INPUT_SIZE);
            let actual = note_feature_vector(&contexts, index, &meter, &durations, &pitches);
            for (slot, name) in ACCENT_INPUT_FEATURES.iter().enumerate() {
                let difference = (actual[slot] - expected[slot]).abs();
                assert!(
                    difference <= FEATURE_TOLERANCE,
                    "{} note {index} of {}: {name} is {} here and {} in training",
                    case.part_id,
                    case.notes.len(),
                    actual[slot],
                    expected[slot]
                );
            }
        }
    }
}

/// The rule is what runs when no model is loaded, so its arithmetic has to
/// agree too — not only the features it reads.
#[test]
fn the_rule_reproduces_the_training_pipelines_components() {
    let fixture = fixture();
    let rule = rule();
    for case in &fixture.cases {
        for (index, expected) in case.rule_components.iter().enumerate() {
            let actual = rule.components(&case.features[index]);
            for target in 0..4 {
                let difference = (actual[target] - expected[target]).abs();
                assert!(
                    difference <= ACCENT_TOLERANCE,
                    "{} note {index}: rule component {target} is {} here and {} in training",
                    case.part_id,
                    actual[target],
                    expected[target]
                );
            }
        }
    }
}
