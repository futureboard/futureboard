//! Auto Accent Analyze: estimating which notes a player would lean on.
//!
//! ```text
//! clip notes ──> phrase context ──> Accent Analyzer ──> AccentState per note
//!                                                            │
//!                                                            ├──> Accent lane (editable)
//!                                                            └──> Studio Performer
//! ```
//!
//! This is **semantic musical analysis**, not audio and not a generator. It
//! reads a score the way a player reads one for the first time — where the bar
//! lines fall, which notes are long, where the line turns over, what is held
//! across a strong beat — and produces a per-note reading of how much emphasis
//! each note wants and by which means. What that emphasis *sounds* like is
//! decided downstream by the Performer and the instrument, and depends on the
//! articulation: a pizzicato accent and a sustain accent share a semantic value
//! and share almost nothing acoustically.
//!
//! ## What is here
//!
//! - [`meter`] — the metrical grid, from the project's own time signature and
//!   its user-editable accent grouping.
//! - [`features`] — the thirty-three score features, mirroring the training
//!   pipeline exactly.
//! - [`rule`] — a fitted nine-feature linear rule; the analyser of record when
//!   no model is loaded, and the baseline the trained one is measured against.
//! - [`analyzer`] — the two combined: rule plus an optional learned correction.
//!
//! ## What is deliberately not here
//!
//! Velocity. The analyser never reads it, so "the same velocity can produce
//! different accents" is a property of the design rather than a hope about the
//! training. Articulation, for a duller reason: the corpus has no articulation
//! labels, so it could only ever be a constant column.
//!
//! ## Where the result lives
//!
//! In `MidiNoteState::accent`, as ordinary project data. There is no separate
//! AI-owned store, no regeneration on load, and nothing marks a generated
//! accent as untouchable — a person can drag it, reset it, and save it, and the
//! next analysis will respect the fact that they did.

pub mod analyzer;
pub mod apply;
pub mod features;
pub mod gesture;
pub mod meter;
pub mod rule;

#[cfg(test)]
mod acceptance;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod parity;

pub use analyzer::{AccentAnalysisStats, AccentAnalyzer};
pub use apply::{apply_to_notes, dynamics_contour, dynamics_lane_is_writable, AccentApplication};
pub use features::{contexts_from_notes, NoteContext, ACCENT_INPUT_FEATURES, ACCENT_INPUT_SIZE};
pub use gesture::AccentGesture;
pub use meter::Meter;

use crate::components::timeline::timeline_state::{AccentSource, AccentState, MidiNoteState};

/// What a re-analysis does to notes a person has already edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccentReplacePolicy {
    /// Overwrite every analysed note, hand-edited or not.
    ReplaceAll,
    /// Leave notes whose accent is [`AccentSource::Manual`] exactly as they
    /// are. The default, because the alternative silently discards work and
    /// the user asked for an analysis, not a reset.
    #[default]
    KeepManual,
}

impl AccentReplacePolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReplaceAll => "Replace all accents",
            Self::KeepManual => "Keep manual edits",
        }
    }
}

/// Write analysed accents onto a note list, honouring the replace policy.
///
/// Returns how many notes changed. Zero means the analysis agreed with what was
/// already there, which is a real outcome and not a failure — re-running an
/// analysis on an unchanged clip should be a no-op, and an undo entry is only
/// recorded when this is non-zero.
pub fn apply_accents(
    notes: &mut [MidiNoteState],
    accents: &[AccentState],
    policy: AccentReplacePolicy,
) -> usize {
    let mut changed = 0;
    for (note, accent) in notes.iter_mut().zip(accents) {
        if policy == AccentReplacePolicy::KeepManual
            && note
                .accent
                .is_some_and(|existing| existing.source == AccentSource::Manual)
        {
            continue;
        }
        let next = Some(accent.sanitized());
        if note.accent != next {
            note.accent = next;
            changed += 1;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8) -> MidiNoteState {
        MidiNoteState::new(pitch, 0.0, 1.0, 96)
    }

    #[test]
    fn keeping_manual_edits_leaves_hand_set_notes_alone() {
        let mut notes = vec![note(60), note(62)];
        notes[1].accent = Some(AccentState::neutral().with_prominence(0.9));
        let analysed = vec![
            AccentState::generated(0.2, 0.2, 0.2, 0.2, 0.5),
            AccentState::generated(0.3, 0.3, 0.3, 0.3, 0.5),
        ];

        let changed = apply_accents(&mut notes, &analysed, AccentReplacePolicy::KeepManual);
        assert_eq!(changed, 1);
        assert_eq!(notes[0].accent.unwrap().prominence, 0.2);
        assert_eq!(
            notes[1].accent.unwrap().prominence,
            0.9,
            "the hand-set accent survived a re-analysis"
        );
    }

    #[test]
    fn replace_all_overwrites_hand_set_notes() {
        let mut notes = vec![note(60)];
        notes[0].accent = Some(AccentState::neutral().with_prominence(0.9));
        let analysed = vec![AccentState::generated(0.2, 0.2, 0.2, 0.2, 0.5)];
        assert_eq!(
            apply_accents(&mut notes, &analysed, AccentReplacePolicy::ReplaceAll),
            1
        );
        assert_eq!(notes[0].accent.unwrap().prominence, 0.2);
    }

    /// Re-running an analysis that changes nothing must report nothing changed,
    /// so it cannot add an empty step to the undo history.
    #[test]
    fn re_applying_the_same_analysis_changes_nothing() {
        let mut notes = vec![note(60), note(62)];
        let analysed = vec![
            AccentState::generated(0.2, 0.3, 0.4, 0.5, 0.6),
            AccentState::generated(0.6, 0.5, 0.4, 0.3, 0.2),
        ];
        assert_eq!(
            apply_accents(&mut notes, &analysed, AccentReplacePolicy::KeepManual),
            2
        );
        assert_eq!(
            apply_accents(&mut notes, &analysed, AccentReplacePolicy::KeepManual),
            0
        );
    }
}
