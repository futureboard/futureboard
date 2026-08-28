//! Running the analysis: score in, one [`AccentState`] per note out.
//!
//! Two analysers, and the trained one is built *on top of* the rule rather than
//! beside it:
//!
//! ```text
//! score features ──┬──> fitted linear rule ──> base accent
//!                  │                              │
//!                  └──> FBMX correction ──────────┴──> accent
//! ```
//!
//! The correction formulation is not decoration. The corpus behind this is
//! 4958 training notes across twenty-one violin parts, and a bidirectional GRU
//! given that much data and a free hand overfits inside one epoch — measured:
//! every configuration of a fifteen-point capacity sweep selected its epoch-0
//! or epoch-1 checkpoint, and none beat the nine-feature linear rule. Learning
//! the *residual* puts the network's floor at the rule's answer: it starts at
//! zero correction and can only earn its way away from it, so the analyser is
//! never worse than the baseline it is reported against.
//!
//! ## What the correction is worth on this corpus: nothing
//!
//! Stated here rather than left for someone to discover. Under leave-one-
//! session-out cross-validation over all fourteen URMP session clusters, with
//! the training of each fold stopped by a further held-out cluster, the learned
//! correction beat the rule on **one fold of fourteen** for prominence, agogic
//! and timbre, and **none** for attack. Trained for shipping the same way, it
//! converged to a correction of *exactly zero*: the early stop never found an
//! epoch better than the untrained state, which for a residual model is the
//! rule itself.
//!
//! So the shipped Solo Violin package carries **no `ACNT` section**, and this
//! runs rule-only. The model path below is not dead code kept for tidiness —
//! it is the path a larger corpus would arrive through, it is exercised by the
//! export pipeline, and shipping a correction that fourteen folds say is noise
//! would be shipping a control that changes the sound for no reason.
//!
//! What *is* worth something is the rule, and the two forbidden shortcuts are
//! measurably worse than it on the held-out split: metrical strength alone
//! reaches a rank correlation of **-0.15** on prominence — actively wrong —
//! and score velocity alone reaches **0.04**, against the rule's **0.11** and,
//! on the head the corpus really supports, **0.42** for agogic emphasis.
//!
//! A model that fails to load is therefore not a failure either. The rule runs,
//! the editor shows accents, and the instrument is the one it was before.

use std::sync::Arc;

use crate::components::timeline::timeline_state::{AccentState, MidiNoteState};

use super::features::{contexts_from_notes, phrase_feature_matrix, ACCENT_INPUT_SIZE};
use super::meter::Meter;
use super::rule::rule;

/// Outputs of the trained analyser, in head order.
const ACCENT_OUTPUT_SIZE: usize = 5;
const LOG_VARIANCE_INDEX: usize = 4;

/// Clamp on the log-variance head. Mirrors `LOG_VARIANCE_RANGE` in
/// `neural/accent/model.py`; a value outside it came from a model asked about
/// music unlike anything it trained on, and clamping is what keeps that from
/// becoming an infinite or zero confidence.
const LOG_VARIANCE_RANGE: (f32, f32) = (-6.0, 2.0);

/// A standard deviation this large, on a `0..1` scale, is no useful opinion.
const NO_OPINION_SIGMA: f32 = 0.25;

/// A loaded Accent Analyzer.
///
/// Cheap to clone (the weights are behind an `Arc`) so a background analysis
/// task can take one without holding a lock on the model cache.
#[derive(Clone)]
pub struct AccentAnalyzer {
    model: Option<Arc<fbmx_runtime::PerformerRuntime>>,
}

impl std::fmt::Debug for AccentAnalyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccentAnalyzer")
            .field("model", &self.model.as_ref().map(|_| "loaded"))
            .finish()
    }
}

/// What one analysis pass produced, beyond the accents themselves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccentAnalysisStats {
    pub notes: usize,
    /// `true` when a trained correction was applied; `false` when the rule ran
    /// alone. Surfaced in the status line so "Analyze Accent" never silently
    /// means two different things.
    pub used_model: bool,
    /// Mean and spread of the produced `prominence`, for the status line and
    /// for the distribution check the acceptance tests make.
    pub mean_prominence: f32,
    pub prominence_spread: f32,
}

impl AccentAnalyzer {
    /// The rule-only analyser. Always available.
    pub fn rule_only() -> Self {
        Self { model: None }
    }

    /// Wrap a loaded `accent-gru` model.
    pub fn with_model(model: Arc<fbmx_runtime::PerformerRuntime>) -> Self {
        Self { model: Some(model) }
    }

    /// Build from an FBMX container, falling back to the rule if it is not an
    /// accent model this build can run.
    ///
    /// A model that does not load is reported as absent rather than as an
    /// error: the instrument still plays and the editor still analyses, so
    /// refusing the whole operation would trade a working feature for a
    /// message.
    pub fn from_fbmx(bytes: &[u8]) -> Self {
        fbmx_runtime::FbmxModel::from_bytes(bytes)
            .ok()
            .and_then(|model| model.instantiate_accent_analyzer().ok())
            .filter(|runtime| {
                // A model whose feature vocabulary is not this build's would
                // read every column as a different quantity. Silently running
                // it would produce confident nonsense.
                runtime.input_size() == ACCENT_INPUT_SIZE
                    && runtime.output_size() == ACCENT_OUTPUT_SIZE
                    && runtime.is_bidirectional()
            })
            .map(|runtime| Self::with_model(Arc::new(runtime)))
            .unwrap_or_else(Self::rule_only)
    }

    pub fn has_model(&self) -> bool {
        self.model.is_some()
    }

    /// Analyse one clip's notes.
    ///
    /// `notes` must be in score order. `clip_start_beat` places the clip on the
    /// project timeline so metrical strength is measured against the project's
    /// bars, and `meter` is the signature in force there.
    ///
    /// Runs the whole phrase in one pass and allocates freely — this is called
    /// from a background task, never from a render or an audio callback.
    pub fn analyze(
        &self,
        notes: &[MidiNoteState],
        clip_start_beat: f32,
        tempo_bpm: f32,
        meter: &Meter,
    ) -> (Vec<AccentState>, AccentAnalysisStats) {
        if notes.is_empty() {
            return (
                Vec::new(),
                AccentAnalysisStats {
                    notes: 0,
                    used_model: false,
                    mean_prominence: 0.0,
                    prominence_spread: 0.0,
                },
            );
        }

        let contexts = contexts_from_notes(notes, clip_start_beat, tempo_bpm);
        let features = phrase_feature_matrix(&contexts, meter);
        let rule = rule();

        let correction = self
            .model
            .as_ref()
            .and_then(|model| model.run(&features).ok())
            .filter(|out| out.len() == notes.len() * ACCENT_OUTPUT_SIZE);
        let used_model = correction.is_some();

        let mut accents = Vec::with_capacity(notes.len());
        for index in 0..notes.len() {
            let row = &features[index * ACCENT_INPUT_SIZE..(index + 1) * ACCENT_INPUT_SIZE];
            let base = rule.components(row);
            let accent = match correction.as_ref() {
                Some(values) => {
                    let offset = index * ACCENT_OUTPUT_SIZE;
                    // Correction first, spread match second. Calibrating the
                    // rule and then adding an uncalibrated correction would add
                    // two quantities on different scales.
                    let corrected = [
                        base[0] + values[offset],
                        base[1] + values[offset + 1],
                        base[2] + values[offset + 2],
                        base[3] + values[offset + 3],
                    ];
                    let final_components = rule.calibration().apply(corrected);
                    let sigma = (0.5
                        * values[offset + LOG_VARIANCE_INDEX]
                            .clamp(LOG_VARIANCE_RANGE.0, LOG_VARIANCE_RANGE.1))
                    .exp();
                    AccentState::generated(
                        final_components[0],
                        final_components[1],
                        final_components[2],
                        final_components[3],
                        (1.0 - sigma / NO_OPINION_SIGMA).clamp(0.0, 1.0),
                    )
                }
                None => rule.accent(row),
            };
            accents.push(accent);
        }

        let mean = accents.iter().map(|a| a.prominence).sum::<f32>() / accents.len() as f32;
        let variance = accents
            .iter()
            .map(|a| (a.prominence - mean).powi(2))
            .sum::<f32>()
            / accents.len() as f32;
        (
            accents,
            AccentAnalysisStats {
                notes: notes.len(),
                used_model,
                mean_prominence: mean,
                prominence_spread: variance.sqrt(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn even_quarters(pitch: u8, count: usize) -> Vec<MidiNoteState> {
        (0..count)
            .map(|index| MidiNoteState::new(pitch, index as f32, 1.0, 96))
            .collect()
    }

    /// Acceptance test 38 / 43: identical pitch, identical duration, identical
    /// velocity. If the output were velocity or a constant, this would be flat.
    #[test]
    fn identical_notes_still_receive_different_accents_from_meter_alone() {
        let notes = even_quarters(60, 8);
        let (accents, stats) =
            AccentAnalyzer::rule_only().analyze(&notes, 0.0, 120.0, &Meter::from_signature(4, 4));
        assert_eq!(accents.len(), 8);
        assert!(
            stats.prominence_spread > 1.0e-3,
            "eight identical notes produced a flat accent contour"
        );
        // Bar 1: beat 1 > beat 3 > beats 2 and 4.
        assert!(accents[0].prominence > accents[2].prominence);
        assert!(accents[2].prominence > accents[1].prominence);
        // ...and the metrical ordering repeats in bar 2. The *values* do not
        // repeat exactly, and must not: note 0 opens the phrase and note 4 does
        // not, which is a real difference the analysis is entitled to read.
        assert!(accents[4].prominence > accents[6].prominence);
        assert!(accents[6].prominence > accents[5].prominence);
    }

    /// Acceptance test 39: the same notes under a different meter must be read
    /// differently, or the meter features are not reaching the analysis.
    #[test]
    fn the_same_notes_under_a_different_meter_produce_a_different_reading() {
        let notes = even_quarters(60, 12);
        let analyzer = AccentAnalyzer::rule_only();
        let (four_four, _) = analyzer.analyze(&notes, 0.0, 120.0, &Meter::from_signature(4, 4));
        let (three_four, _) = analyzer.analyze(&notes, 0.0, 120.0, &Meter::from_signature(3, 4));

        let differing = four_four
            .iter()
            .zip(&three_four)
            .filter(|(a, b)| (a.prominence - b.prominence).abs() > 1.0e-4)
            .count();
        assert!(
            differing >= 4,
            "only {differing} of 12 notes changed when the meter did"
        );
        // In 3/4 every third note is a downbeat; in 4/4 every fourth.
        assert!(three_four[3].prominence > three_four[4].prominence);
        assert!(four_four[4].prominence > four_four[3].prominence);
    }

    /// Acceptance test 42: a passing note that happens to be higher than its
    /// neighbours, on a weak subdivision, must not outrank the downbeat.
    #[test]
    fn a_high_weak_beat_passing_note_does_not_outrank_the_downbeat() {
        // C4 on the downbeat, then eighths rising through a higher passing note.
        let notes = vec![
            MidiNoteState::new(60, 0.0, 1.0, 96),
            MidiNoteState::new(64, 1.0, 0.5, 96),
            MidiNoteState::new(67, 1.5, 0.5, 96), // higher, but on an offbeat
            MidiNoteState::new(64, 2.0, 1.0, 96),
            MidiNoteState::new(60, 3.0, 1.0, 96),
        ];
        let (accents, _) =
            AccentAnalyzer::rule_only().analyze(&notes, 0.0, 120.0, &Meter::from_signature(4, 4));
        assert!(
            accents[0].prominence > accents[2].prominence,
            "the offbeat passing note ({}) outranked the downbeat ({})",
            accents[2].prominence,
            accents[0].prominence
        );
    }

    #[test]
    fn an_empty_clip_analyses_to_nothing_rather_than_panicking() {
        let (accents, stats) =
            AccentAnalyzer::rule_only().analyze(&[], 0.0, 120.0, &Meter::from_signature(4, 4));
        assert!(accents.is_empty());
        assert_eq!(stats.notes, 0);
    }

    #[test]
    fn every_produced_accent_is_in_range_and_finite() {
        let notes = vec![
            MidiNoteState::new(0, 0.0, 0.001, 1),
            MidiNoteState::new(127, 0.001, 400.0, 127),
            MidiNoteState::new(60, 400.0, 0.03, 64),
        ];
        let (accents, _) =
            AccentAnalyzer::rule_only().analyze(&notes, -3.5, 1.0, &Meter::from_signature(7, 8));
        for accent in accents {
            for value in [
                accent.prominence,
                accent.attack,
                accent.agogic,
                accent.timbre,
                accent.confidence,
            ] {
                assert!(value.is_finite() && (0.0..=1.0).contains(&value), "{value}");
            }
        }
    }
}
