//! Turning accents into a performance, inside the project.
//!
//! Analyze Accent produces a *reading*. This is the separate, explicit step
//! that acts on it — and it is separate on purpose. Section 56 of the brief and
//! plain good manners agree: a tool that silently rewrote a musician's note
//! timings the moment it finished analysing would be a tool nobody could leave
//! switched on.
//!
//! ```text
//! notes ──> Analyze Accent ──> accents ──> [user inspects / edits] ──> Apply
//! ```
//!
//! # What it writes, and where that lands in the engine
//!
//! | accent component | written to                | reaches the instrument as              |
//! |------------------|---------------------------|----------------------------------------|
//! | agogic           | `start`, `duration`       | when the note sounds and for how long   |
//! | attack           | `velocity`                | `VoicebankRenderer::note_on` layer + level |
//! | prominence       | Dynamics lane (CC 1)      | `VoicebankRenderer::set_dynamic` glide  |
//! | timbre           | *folded into velocity*    | which dynamic layer is chosen           |
//!
//! The last row is the honest one. This instrument's renderer has no separate
//! brightness control: CC 74 exists but only reaches the physical fallback
//! engine, which runs when the model has *failed to load*. On a sampled violin
//! the real brightness path is dynamic-layer selection — a forte recording is
//! brighter because it was played brighter — so timbral accent rides the same
//! velocity that chooses that layer, at a reduced weight, rather than being
//! written to a controller that would be discarded. The four components stay
//! separable in [`AccentGesture`]; what varies is how many distinct paths a
//! given instrument has to realise them through.
//!
//! # What it refuses to touch
//!
//! A Dynamics lane that already has points in it. Accent is not Dynamics, and a
//! musician who has drawn a dynamic shape has said something this has no right
//! to overwrite. [`AccentApplication::dynamics_skipped`] reports it so the
//! refusal is visible rather than silent.

use crate::components::timeline::timeline_state::{MidiControllerPoint, MidiNoteState};

use super::gesture::AccentGesture;

/// Breakpoints per note in the generated dynamics contour.
///
/// Three: a rise into the note, its peak a third of the way in, and a fall back
/// to the surrounding level. That is a swell, which is what section 24 asks for
/// and what an instant gain jump is not. More points would describe a shape the
/// accent value does not contain.
const CONTOUR_POINTS: usize = 3;

/// Where the peak of the swell sits inside the note, as a fraction of it.
///
/// A third rather than the middle: a bowed accent arrives early in the note and
/// releases across the rest of it.
const CONTOUR_PEAK: f32 = 0.33;

/// Level the dynamics lane sits at where no accent asks otherwise.
///
/// Matches the `dynamics` value a fresh Solfege track carries
/// (`SolfegeTrackState::violin`), so applying accent to a phrase of neutral
/// notes writes the line the track already plays at rather than a step to
/// somewhere new.
const DYNAMICS_BASE: f32 = 0.78;

/// What one application changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccentApplication {
    pub notes_moved: usize,
    pub notes_revoiced: usize,
    pub dynamics_points: usize,
    /// `true` when a hand-drawn Dynamics lane was left alone.
    pub dynamics_skipped: bool,
}

impl AccentApplication {
    pub fn changed_anything(&self) -> bool {
        self.notes_moved > 0 || self.notes_revoiced > 0 || self.dynamics_points > 0
    }
}

/// Apply each note's accent to its timing and voicing, in place.
///
/// `seconds_per_beat` converts the gesture's measured milliseconds into the
/// clip's beats. `clip_beats` bounds the result: a gesture may not push a note
/// past the end of its clip or before its start, because the note would then be
/// silently retimed or clipped by something other than this function.
///
/// Notes with no accent are left exactly as they are — not "applied with a
/// neutral accent", which would still round their start and duration.
pub fn apply_to_notes(
    notes: &mut [MidiNoteState],
    seconds_per_beat: f32,
    clip_beats: f32,
) -> AccentApplication {
    let seconds_per_beat = if seconds_per_beat.is_finite() && seconds_per_beat > 1.0e-6 {
        seconds_per_beat
    } else {
        0.5
    };
    let mut applied = AccentApplication::default();

    // Written positions, captured before anything moves: a gesture is relative
    // to the score, and reading a start that a previous note's gap has already
    // shifted would compound the deviations along the phrase.
    let written: Vec<(f32, f32)> = notes
        .iter()
        .map(|note| (note.start, note.duration))
        .collect();

    for (index, note) in notes.iter_mut().enumerate() {
        let Some(accent) = note.accent else {
            continue;
        };
        let gesture = AccentGesture::for_articulation(accent, note.articulation);
        if gesture.is_neutral() {
            continue;
        }
        let (start, duration) = written[index];

        let shift = (gesture.onset_shift_seconds + gesture.gap_before_seconds) / seconds_per_beat;
        let next_start = (start + shift).clamp(0.0, (clip_beats - 1.0 / 32.0).max(0.0));
        let next_duration = (duration * gesture.duration_scale)
            .max(1.0 / 32.0)
            .min((clip_beats - next_start).max(1.0 / 32.0));
        if (next_start - note.start).abs() > 1.0e-5
            || (next_duration - note.duration).abs() > 1.0e-5
        {
            note.start = next_start;
            note.duration = next_duration;
            applied.notes_moved += 1;
        }

        // Attack and timbre both land on velocity: the first because it *is*
        // how hard the note is started, the second because dynamic-layer
        // selection is this instrument's only brightness path. Timbre is
        // weighted down so a bright-but-not-emphatic note does not read as a
        // loud one.
        let velocity_delta = gesture.attack_gain + 0.4 * gesture.brightness;
        let next_velocity = ((f32::from(note.velocity) / 127.0 + velocity_delta) * 127.0)
            .round()
            .clamp(1.0, 127.0) as u8;
        if next_velocity != note.velocity {
            note.velocity = next_velocity;
            applied.notes_revoiced += 1;
        }
    }
    applied
}

/// The dynamics contour a set of accented notes implies.
///
/// Returns `None` when nothing in the clip asks for one, so a neutral phrase
/// does not gain a flat lane full of points.
///
/// Each accented note contributes a rise, a peak and a fall. Where two notes
/// overlap in time the later point wins, which is what a monophonic instrument
/// does anyway — a violin plays one dynamic at a time.
pub fn dynamics_contour(
    notes: &[MidiNoteState],
    next_point_id: &mut u64,
) -> Option<Vec<MidiControllerPoint>> {
    let mut points: Vec<(f32, f32)> = Vec::new();
    for note in notes {
        let Some(accent) = note.accent else {
            continue;
        };
        let gesture = AccentGesture::for_articulation(accent, note.articulation);
        if gesture.level_bump.abs() < 1.0e-4 {
            continue;
        }
        let peak = (DYNAMICS_BASE + gesture.level_bump).clamp(0.0, 1.0);
        let span = note.duration.max(1.0 / 32.0);
        // Rise, peak, fall. The rise starts inside the note rather than before
        // it: a swell that begins during the previous note is a crescendo, and
        // this is an accent.
        points.push((note.start, DYNAMICS_BASE));
        points.push((note.start + span * CONTOUR_PEAK, peak));
        points.push((note.start + span, DYNAMICS_BASE));
    }
    if points.is_empty() {
        return None;
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    points.dedup_by(|a, b| (a.0 - b.0).abs() < 1.0e-4);
    debug_assert!(points.len() % CONTOUR_POINTS <= points.len());

    Some(
        points
            .into_iter()
            .map(|(beat, value)| {
                let point = MidiControllerPoint::from_persisted(*next_point_id, beat, value);
                *next_point_id += 1;
                point
            })
            .collect(),
    )
}

/// Whether an existing Dynamics lane is safe to overwrite.
///
/// Only an empty one is. There is no provenance on a controller point — the
/// lane stores beats and values and nothing about who put them there — so the
/// only honest test is whether anything is there at all.
pub fn dynamics_lane_is_writable(existing: &[MidiControllerPoint]) -> bool {
    existing.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::timeline::timeline_state::{AccentState, ArticulationId};

    fn note(pitch: u8, start: f32, duration: f32) -> MidiNoteState {
        MidiNoteState::new(pitch, start, duration, 96)
    }

    #[test]
    fn a_note_with_no_accent_is_left_exactly_alone() {
        let mut notes = vec![note(60, 1.0, 1.0)];
        let applied = apply_to_notes(&mut notes, 0.5, 8.0);
        assert_eq!(applied, AccentApplication::default());
        assert_eq!(notes[0].start, 1.0);
        assert_eq!(notes[0].duration, 1.0);
        assert_eq!(notes[0].velocity, 96);
    }

    #[test]
    fn an_accented_note_moves_later_lengthens_and_gets_louder() {
        let mut notes = vec![note(60, 1.0, 1.0)];
        notes[0].accent = Some(AccentState::generated(0.95, 0.95, 0.95, 0.95, 0.9));
        let applied = apply_to_notes(&mut notes, 0.5, 8.0);
        assert_eq!(applied.notes_moved, 1);
        assert_eq!(applied.notes_revoiced, 1);
        assert!(notes[0].start > 1.0);
        assert!(notes[0].duration > 1.0);
        assert!(notes[0].velocity > 96);
        // ...and stays inside what a violinist was measured doing: 42 ms of
        // onset plus 53 ms of gap is 0.19 beats at 0.5 s per beat.
        assert!(notes[0].start < 1.20, "moved to {}", notes[0].start);
    }

    #[test]
    fn a_de_emphasised_note_moves_earlier_and_shortens() {
        let mut notes = vec![note(60, 1.0, 1.0)];
        notes[0].accent = Some(AccentState::generated(0.05, 0.05, 0.05, 0.05, 0.9));
        apply_to_notes(&mut notes, 0.5, 8.0);
        assert!(notes[0].start < 1.0);
        assert!(notes[0].duration < 1.0);
        assert!(notes[0].velocity < 96);
    }

    /// Deviations must be relative to the *written* position. Reading a start
    /// that an earlier note already moved would let them accumulate along a
    /// phrase until the last note is half a bar out.
    #[test]
    fn deviations_do_not_accumulate_along_a_phrase() {
        let mut notes: Vec<MidiNoteState> = (0..16)
            .map(|index| note(60, index as f32 * 0.5, 0.5))
            .collect();
        for note in &mut notes {
            note.accent = Some(AccentState::generated(1.0, 1.0, 1.0, 1.0, 0.9));
        }
        apply_to_notes(&mut notes, 0.5, 32.0);
        for (index, note) in notes.iter().enumerate() {
            let written = index as f32 * 0.5;
            assert!(
                (note.start - written).abs() < 0.25,
                "note {index} drifted from {written} to {}",
                note.start
            );
        }
    }

    #[test]
    fn nothing_is_pushed_outside_its_clip() {
        let mut notes = vec![note(60, 3.9, 0.1)];
        notes[0].accent = Some(AccentState::generated(1.0, 1.0, 1.0, 1.0, 0.9));
        apply_to_notes(&mut notes, 2.0, 4.0);
        assert!(notes[0].start >= 0.0);
        assert!(notes[0].start + notes[0].duration <= 4.0 + 1.0e-4);
    }

    #[test]
    fn a_neutral_phrase_produces_no_dynamics_contour() {
        let mut notes = vec![note(60, 0.0, 1.0), note(62, 1.0, 1.0)];
        for note in &mut notes {
            note.accent = Some(AccentState::neutral());
        }
        let mut ids = 1;
        assert!(dynamics_contour(&notes, &mut ids).is_none());
    }

    /// A swell, not a step: the contour must rise and fall inside the note.
    #[test]
    fn an_accented_note_gets_a_swell_rather_than_a_gain_jump() {
        let mut notes = vec![note(60, 0.0, 2.0)];
        notes[0].accent = Some(AccentState::generated(0.95, 0.5, 0.5, 0.95, 0.9));
        let mut ids = 1;
        let contour = dynamics_contour(&notes, &mut ids).expect("a contour");
        assert_eq!(contour.len(), 3);
        assert!(contour[1].value > contour[0].value);
        assert!(contour[1].value > contour[2].value);
        assert!(contour[0].beat < contour[1].beat && contour[1].beat < contour[2].beat);
        // The peak is inside the note, not at its edge.
        assert!(contour[1].beat > 0.0 && contour[1].beat < 2.0);
    }

    /// A pizzicato has no swell to give, so it writes no dynamics at all — the
    /// same accent value on a sustain does.
    #[test]
    fn articulation_decides_whether_a_dynamics_contour_exists_at_all() {
        let accent = AccentState::generated(0.95, 0.95, 0.95, 0.95, 0.9);
        let mut ids = 1;

        let mut pizzicato = vec![note(60, 0.0, 1.0)];
        pizzicato[0].accent = Some(accent);
        pizzicato[0].articulation = Some(ArticulationId::Pizzicato);
        assert!(dynamics_contour(&pizzicato, &mut ids).is_none());

        let mut sustain = vec![note(60, 0.0, 1.0)];
        sustain[0].accent = Some(accent);
        sustain[0].articulation = Some(ArticulationId::Sustain);
        assert!(dynamics_contour(&sustain, &mut ids).is_some());
    }

    #[test]
    fn a_hand_drawn_dynamics_lane_is_not_writable() {
        assert!(dynamics_lane_is_writable(&[]));
        assert!(!dynamics_lane_is_writable(&[
            MidiControllerPoint::from_persisted(1, 0.0, 0.5)
        ]));
    }
}
