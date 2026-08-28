//! Per-note musical accent: how prominent a note should feel, and how.
//!
//! Accent is **not** velocity, and it is not a second dynamics lane. Velocity
//! selects a recorded dynamic layer at the attack; dynamics draws a continuous
//! level over time. Accent answers a different question — *which notes of this
//! phrase should a listener register as important* — and it answers it in four
//! semantic components rather than one scalar, because a violinist emphasising
//! a note does not have one knob either:
//!
//! ```text
//! prominence   how important the note should feel overall
//! attack       how much of that is in the bow's bite at the start
//! agogic       how much of it is in taking time over the note
//! timbre       how much of it is in playing brighter
//! confidence   how sure the analyser was
//! ```
//!
//! All five are normalised `0.0..=1.0`, and **0.5 is neutral, not zero**. The
//! analyser's targets are locally normalised — every value says "compared with
//! the notes around it" — so a phrase of evenly played notes is a phrase of
//! 0.5s. A note at 0.0 is not "unaccented", it is *de-emphasised relative to
//! its neighbours*, which is a real and different musical instruction.
//!
//! Like [`PitchCurve`], accent lives on [`MidiNoteState`] itself. That is what
//! gives it persistence, `EditMidiNotes` undo, and travel through move / copy /
//! split / delete without a single line of code in any of those paths. There is
//! deliberately no parallel AI-owned store: once analysed, an accent is
//! ordinary project data a user can drag, and a project whose owner never runs
//! the analyser behaves exactly as it did before this type existed.

/// The neutral value of every accent component.
///
/// Not zero. See the module docs: the analyser's scale is relative to a note's
/// neighbourhood, so "like everything around it" is the middle.
pub const ACCENT_NEUTRAL: f32 = 0.5;

/// Where a note's accent came from.
///
/// Tracked so that re-running Analyze Accent can offer to leave hand-edited
/// notes alone. Without it the only honest options would be "always replace
/// everything" (which silently discards work) or "never replace anything"
/// (which makes re-analysis useless after one edit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum AccentSource {
    /// Written by Analyze Accent and untouched since.
    #[default]
    Generated = 0,
    /// A person has moved this note's accent. Protected by the "keep manual
    /// edits" analysis option.
    Manual = 1,
}

impl AccentSource {
    /// Persisted tag. Unknown tags decode to `Manual`, which is the
    /// conservative direction: a value this build does not understand is
    /// treated as something a person may have meant, and so is not overwritten
    /// by a re-analysis that preserves manual edits.
    pub fn to_tag(self) -> u8 {
        self as u8
    }

    pub fn from_tag(tag: u8) -> Self {
        match tag {
            0 => Self::Generated,
            _ => Self::Manual,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Generated => "Generated",
            Self::Manual => "Manual",
        }
    }
}

/// One note's accent, as project data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccentState {
    /// How important the note should feel. This is the value the Accent lane
    /// draws and the one the user-facing 0-100% reading comes from.
    pub prominence: f32,
    /// Share of the emphasis carried by the attack transient.
    pub attack: f32,
    /// Share carried by timing: a little more length, a little more space.
    pub agogic: f32,
    /// Share carried by brightness.
    pub timbre: f32,
    /// How sure the analyser was about `prominence`, `0..1`.
    ///
    /// Zero from a hand-drawn accent: a person editing a value is not making a
    /// probabilistic claim, and reporting a confidence for them would be
    /// inventing one. The lane renders a manual accent by its provenance
    /// instead.
    pub confidence: f32,
    pub source: AccentSource,
}

impl Default for AccentState {
    fn default() -> Self {
        Self::neutral()
    }
}

impl AccentState {
    /// The value a note has when nothing has been analysed or drawn.
    ///
    /// Never stored: a note with no accent carries `None`, exactly as a note at
    /// its notated pitch carries no [`PitchCurve`]. This exists so callers that
    /// need *a* value — the rule fallback, the Performer's feature builder —
    /// have one place to get the neutral reading rather than four literals.
    pub const fn neutral() -> Self {
        Self {
            prominence: ACCENT_NEUTRAL,
            attack: ACCENT_NEUTRAL,
            agogic: ACCENT_NEUTRAL,
            timbre: ACCENT_NEUTRAL,
            confidence: 0.0,
            source: AccentSource::Generated,
        }
    }

    /// A generated accent, clamped into range.
    pub fn generated(
        prominence: f32,
        attack: f32,
        agogic: f32,
        timbre: f32,
        confidence: f32,
    ) -> Self {
        Self {
            prominence,
            attack,
            agogic,
            timbre,
            confidence,
            source: AccentSource::Generated,
        }
        .sanitized()
    }

    /// Clamp every component into `0..=1`, replacing non-finite values with the
    /// neutral reading.
    ///
    /// A NaN reaching the lane renderer paints nothing and a NaN reaching the
    /// Performer's feature vector poisons the whole phrase's hidden state, so
    /// this runs on every value read from a file, a model, or a gesture.
    pub fn sanitized(self) -> Self {
        let clamp = |value: f32| {
            if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                ACCENT_NEUTRAL
            }
        };
        Self {
            prominence: clamp(self.prominence),
            attack: clamp(self.attack),
            agogic: clamp(self.agogic),
            timbre: clamp(self.timbre),
            confidence: if self.confidence.is_finite() {
                self.confidence.clamp(0.0, 1.0)
            } else {
                0.0
            },
            source: self.source,
        }
    }

    /// `true` when every component is neutral.
    ///
    /// Used to drop an accent back to `None` rather than store a row of 0.5s,
    /// so "Reset" leaves a note in the state it had before it was ever
    /// analysed and the lane can tell "no accent here" from "neutral accent
    /// here" — which matters, because the second one survives a re-analysis
    /// that preserves manual edits and the first does not.
    pub fn is_neutral(&self) -> bool {
        const EPSILON: f32 = 1.0e-4;
        [self.prominence, self.attack, self.agogic, self.timbre]
            .iter()
            .all(|value| (value - ACCENT_NEUTRAL).abs() < EPSILON)
    }

    /// The single 0-100% figure the editor shows.
    pub fn percent(&self) -> u8 {
        (self.prominence.clamp(0.0, 1.0) * 100.0).round() as u8
    }

    /// Move `prominence` to `value`, marking the note hand-edited.
    ///
    /// The three sub-components are carried along in proportion rather than
    /// left where the analyser put them. Dragging a note from 0.3 to 0.9 and
    /// having it stay soft-attacked would make the lane a control that changes
    /// a number and not a sound; scaling the components keeps the *character*
    /// the analyser found while changing the *degree* the user asked for.
    /// A user who wants a different character edits the components directly.
    pub fn with_prominence(self, value: f32) -> Self {
        let target = value.clamp(0.0, 1.0);
        let previous = self.prominence.clamp(0.0, 1.0);
        let shift = target - previous;
        Self {
            prominence: target,
            attack: (self.attack + shift).clamp(0.0, 1.0),
            agogic: (self.agogic + shift).clamp(0.0, 1.0),
            timbre: (self.timbre + shift).clamp(0.0, 1.0),
            // A hand-placed value is not a prediction and carries no
            // uncertainty of its own.
            confidence: 0.0,
            source: AccentSource::Manual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_the_middle_of_the_range_not_the_bottom() {
        let neutral = AccentState::neutral();
        assert_eq!(neutral.prominence, 0.5);
        assert!(neutral.is_neutral());
        assert_eq!(neutral.percent(), 50);
    }

    #[test]
    fn non_finite_values_become_neutral_rather_than_reaching_the_renderer() {
        let poisoned = AccentState {
            prominence: f32::NAN,
            attack: f32::INFINITY,
            agogic: -3.0,
            timbre: 7.0,
            confidence: f32::NAN,
            source: AccentSource::Generated,
        }
        .sanitized();
        assert_eq!(poisoned.prominence, ACCENT_NEUTRAL);
        assert_eq!(poisoned.attack, ACCENT_NEUTRAL);
        assert_eq!(poisoned.agogic, 0.0);
        assert_eq!(poisoned.timbre, 1.0);
        assert_eq!(poisoned.confidence, 0.0);
    }

    #[test]
    fn dragging_prominence_carries_the_character_and_marks_the_note_manual() {
        let generated = AccentState::generated(0.30, 0.60, 0.20, 0.50, 0.8);
        let dragged = generated.with_prominence(0.50);
        assert_eq!(dragged.prominence, 0.50);
        // Everything moved by the same +0.20, so the note is still the
        // sharply-attacked, timing-neutral note the analyser described.
        assert!((dragged.attack - 0.80).abs() < 1e-6);
        assert!((dragged.agogic - 0.40).abs() < 1e-6);
        assert_eq!(dragged.source, AccentSource::Manual);
        assert_eq!(dragged.confidence, 0.0, "a drawn value predicts nothing");
    }

    #[test]
    fn an_unknown_persisted_source_tag_is_treated_as_hand_edited() {
        // The conservative direction: a re-analysis that preserves manual edits
        // must not overwrite a value it does not understand.
        assert_eq!(AccentSource::from_tag(0), AccentSource::Generated);
        assert_eq!(AccentSource::from_tag(1), AccentSource::Manual);
        assert_eq!(AccentSource::from_tag(200), AccentSource::Manual);
    }
}
