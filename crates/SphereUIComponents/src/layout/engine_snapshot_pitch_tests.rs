//! Acceptance tests for the first half of the continuous-pitch path: does the
//! curve the user drew reach the engine snapshot, as the right frequency, at
//! the right beat?
//!
//! The second half — does the snapshot actually change the *sound* — is
//! measured on rendered audio in
//! `sphere_directaudioengine::engine::render::solfege_pitch_tests`. Splitting
//! it there is not a convenience: `DirectAudio`'s `runtime` module is private,
//! so a render cannot be driven from this crate, and asserting on a snapshot is
//! only meaningful if something else proves the snapshot is heard.

use super::engine_snapshot::build_engine_project_snapshot;
use crate::components::edit::edit_commands::EditCommand;
use crate::components::timeline::timeline_state::{
    self, CreateTrackOptions, PitchCurve, PitchPoint, PitchSegmentShape, PitchTrajectory,
    TimelineState, TrackType,
};

const A4_HZ: f32 = 440.0;
const A4_PLUS_100_CENTS_HZ: f32 = 466.163_76;

fn cents(hz: f32, reference: f32) -> f32 {
    1200.0 * (hz / reference).log2()
}

fn instrument_state_with_clip() -> (TimelineState, String) {
    let mut state = TimelineState::default();
    state.tracks.clear();
    let track_id = state.create_track(CreateTrackOptions {
        track_type: TrackType::Instrument,
        name: "Solfege".to_string(),
        color: gpui::Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
        volume: 1.0,
        pan: 0.0,
        armed: false,
        input_monitor: timeline_state::InputMonitorMode::Off,
    });
    let clip = state.build_midi_clip(&track_id, 0.0, 8.0).expect("clip");
    let clip_id = clip.id.clone();
    EditCommand::CreateClip { track_id, clip }.execute(&mut state);
    (state, clip_id)
}

/// Write a curve the way the Pitch editor's `write_pitch_curve` does.
fn draw(state: &mut TimelineState, clip_id: &str, note_id: u64, curve: PitchCurve) {
    let notes = state.midi_clip_notes_mut(clip_id).expect("midi clip");
    let note = notes.iter_mut().find(|n| n.id == note_id).expect("note");
    note.pitch_curve = (!curve.is_empty()).then_some(curve);
}

/// A note nobody has drawn on must cost nothing: no points, and therefore no
/// continuous-pitch work in the engine at all.
#[test]
fn an_untouched_note_emits_no_pitch_points() {
    let (mut state, clip_id) = instrument_state_with_clip();
    state.add_midi_note(&clip_id, 69, 0.0, 2.0, 100).unwrap();
    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    assert!(
        snap.midi_clips[0].notes[0].pitch_points.is_empty(),
        "a flat note must not carry a trajectory"
    );
}

/// The headline requirement, checked at the snapshot boundary: drawing A4 up by
/// 100 cents must put 466.16 Hz into what the engine receives.
#[test]
fn a_drawn_pitch_curve_reaches_the_snapshot_as_hz() {
    let (mut state, clip_id) = instrument_state_with_clip();
    let note_id = state.add_midi_note(&clip_id, 69, 0.0, 4.0, 100).unwrap();
    draw(
        &mut state,
        &clip_id,
        note_id,
        PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(2.0, 100.0, PitchSegmentShape::Linear),
        ]),
    );

    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    let points = &snap.midi_clips[0].notes[0].pitch_points;
    assert!(
        points.len() >= 2,
        "a drawn ramp must reach the engine as breakpoints, got {}",
        points.len()
    );

    let first = points.first().unwrap();
    assert!(
        cents(first.hz, A4_HZ).abs() < 2.0,
        "the curve starts at A4, got {} Hz",
        first.hz
    );
    let top = points
        .iter()
        .min_by(|a, b| (a.beat - 2.0).abs().total_cmp(&(b.beat - 2.0).abs()))
        .unwrap();
    assert!(
        cents(top.hz, A4_PLUS_100_CENTS_HZ).abs() < 2.0,
        "at the top of the ramp the engine must be told 466.16 Hz, got {} Hz",
        top.hz
    );
}

/// Decimation must *reconstruct* the drawn shape, not merely subsample it.
/// Verified against the editor's own evaluator at 400 probes the decimator
/// never saw, so a decimator that dropped a peak would be caught.
#[test]
fn emitted_points_reconstruct_the_curve_within_a_few_cents() {
    let (mut state, clip_id) = instrument_state_with_clip();
    let note_id = state.add_midi_note(&clip_id, 60, 0.0, 4.0, 100).unwrap();
    let drawn = (0..=32)
        .map(|i| {
            let beat = i as f32 * 0.125;
            PitchPoint::new(
                beat,
                60.0 * (beat * 2.2).sin() - 20.0 * (beat * 5.0).cos(),
                PitchSegmentShape::Smooth,
            )
        })
        .collect();
    draw(
        &mut state,
        &clip_id,
        note_id,
        PitchCurve::from_points(drawn),
    );

    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    let emitted = &snap.midi_clips[0].notes[0].pitch_points;
    assert!(emitted.len() >= 4, "expected a real polyline");

    let notes = state.midi_clip_notes(&clip_id).unwrap();
    let trajectory = PitchTrajectory::build(notes, &[]);
    let mut worst = 0.0f64;
    for step in 0..400 {
        let beat = step as f32 * 4.0 / 400.0;
        let Some(expected) = trajectory.sample(notes, 0, beat) else {
            continue;
        };
        let beat = beat as f64;
        // Linear interpolation between emitted breakpoints — what the engine
        // converges to between two pitch targets.
        let reconstructed = match emitted.iter().position(|p| p.beat >= beat) {
            None => emitted.last().unwrap().hz,
            Some(0) => emitted[0].hz,
            Some(index) => {
                let (a, b) = (&emitted[index - 1], &emitted[index]);
                let span = b.beat - a.beat;
                let t = if span > 1e-12 {
                    ((beat - a.beat) / span).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                };
                a.hz + (b.hz - a.hz) * t
            }
        };
        let expected_hz = timeline_state::midi_pitch_to_hz(expected) as f64;
        worst = worst.max((1200.0 * (reconstructed as f64 / expected_hz).log2()).abs());
    }
    assert!(
        worst < 4.0,
        "decimated trajectory drifted {worst:.2} cents from the drawn curve"
    );
}

/// Brief item 50: transposing the note moves the whole trajectory with it, all
/// the way through to what the engine is told.
#[test]
fn transposing_a_note_moves_its_engine_trajectory_by_the_interval() {
    let (mut state, clip_id) = instrument_state_with_clip();
    let note_id = state.add_midi_note(&clip_id, 60, 0.0, 3.0, 100).unwrap();
    draw(
        &mut state,
        &clip_id,
        note_id,
        PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 20.0, PitchSegmentShape::Linear),
            PitchPoint::new(1.0, -10.0, PitchSegmentShape::Linear),
            PitchPoint::new(2.0, 5.0, PitchSegmentShape::Linear),
        ]),
    );
    let hz_of = |state: &TimelineState| -> Vec<f32> {
        build_engine_project_snapshot(state, 48_000, None, None).midi_clips[0].notes[0]
            .pitch_points
            .iter()
            .map(|p| p.hz)
            .collect()
    };
    let before = hz_of(&state);
    for note in state.midi_clip_notes_mut(&clip_id).unwrap() {
        note.pitch += 2;
    }
    let after = hz_of(&state);

    assert_eq!(
        before.len(),
        after.len(),
        "the drawn shape must survive transposition unchanged"
    );
    assert!(!before.is_empty(), "expected a trajectory to compare");
    for (a, b) in before.iter().zip(&after) {
        let interval = cents(*b, *a);
        assert!(
            (interval - 200.0).abs() < 1.0,
            "every point must rise exactly a whole tone, got {interval:.2} cents"
        );
    }
}

/// A muted note emits nothing at all, trajectory included — otherwise a muted
/// note would still retune the voice its pitch id addresses.
#[test]
fn a_muted_note_contributes_no_trajectory() {
    let (mut state, clip_id) = instrument_state_with_clip();
    let note_id = state.add_midi_note(&clip_id, 69, 0.0, 2.0, 100).unwrap();
    draw(
        &mut state,
        &clip_id,
        note_id,
        PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(2.0, 100.0, PitchSegmentShape::Linear),
        ]),
    );
    state.set_midi_notes_muted(&clip_id, &[note_id], true);
    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    assert!(
        snap.midi_clips[0].notes.is_empty(),
        "a muted note must not reach the engine at all"
    );
}

/// Two abutting notes glide into each other in the editor's evaluator; that
/// glide must be what the engine is told too, otherwise the sounding
/// transition and the drawn one disagree.
#[test]
fn a_note_to_note_transition_is_carried_into_the_snapshot() {
    let (mut state, clip_id) = instrument_state_with_clip();
    state.add_midi_note(&clip_id, 60, 0.0, 1.0, 100).unwrap();
    state.add_midi_note(&clip_id, 64, 1.0, 1.0, 100).unwrap();

    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    let first = &snap.midi_clips[0].notes[0];
    assert!(
        !first.pitch_points.is_empty(),
        "an abutting pair must carry the transition out of the first note"
    );
    let last = first.pitch_points.last().unwrap();
    let c4 = timeline_state::midi_pitch_to_hz(60.0);
    let e4 = timeline_state::midi_pitch_to_hz(64.0);
    assert!(
        last.hz > c4 && last.hz < e4,
        "the first note must end mid-glide between C4 and E4, got {} Hz",
        last.hz
    );
}

/// Brief item 49: draw a nontrivial curve, save the project, reload it, and the
/// same pitch trajectory must reach the engine.
///
/// The project format already has an encode/decode round-trip test for the
/// note record. This one covers the two hops that test cannot see — the
/// `TimelineState` <-> `FutureboardProject` mapping in both directions, and the
/// snapshot built from the reloaded state — because a curve that survives the
/// bytes and is dropped on the way back into the timeline is just as silent.
#[test]
fn a_drawn_curve_survives_a_project_save_and_reload_and_still_reaches_the_engine() {
    use crate::project::{apply_to_timeline, format, FutureboardProject};

    let (mut state, clip_id) = instrument_state_with_clip();
    let note_id = state.add_midi_note(&clip_id, 69, 0.0, 4.0, 100).unwrap();
    // Deliberately not a straight line: several shapes, a negative excursion,
    // and a non-12-TET value, so a lossy hop shows up.
    draw(
        &mut state,
        &clip_id,
        note_id,
        PitchCurve::from_points(vec![
            PitchPoint::new(0.0, -37.0, PitchSegmentShape::Smooth),
            PitchPoint::new(0.5, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(1.25, 64.5, PitchSegmentShape::Hold),
            PitchPoint::new(2.0, -18.25, PitchSegmentShape::Smooth),
            PitchPoint::new(3.5, 100.0, PitchSegmentShape::Linear),
        ]),
    );
    let before = build_engine_project_snapshot(&state, 48_000, None, None).midi_clips[0].notes[0]
        .pitch_points
        .clone();
    assert!(before.len() >= 4, "expected a real trajectory to save");

    let bytes = format::encode_project(&FutureboardProject::from(&state));
    let decoded = format::decode_project(&bytes).expect("project decodes");
    let mut restored = TimelineState::default();
    let _ = apply_to_timeline(&decoded, &mut restored);

    let after = build_engine_project_snapshot(&restored, 48_000, None, None).midi_clips[0].notes[0]
        .pitch_points
        .clone();

    assert_eq!(
        before.len(),
        after.len(),
        "the reloaded project produced a different number of pitch breakpoints"
    );
    for (index, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(
            (a.beat - b.beat).abs() < 1e-6,
            "breakpoint {index} moved in time: {} vs {}",
            a.beat,
            b.beat
        );
        assert!(
            cents(b.hz, a.hz).abs() < 0.05,
            "breakpoint {index} changed pitch by {:.3} cents across save/load",
            cents(b.hz, a.hz)
        );
    }

    // The stored points themselves must also come back, not just their
    // evaluation — otherwise the curve would render correctly but be
    // uneditable afterwards.
    let original = state.note_pitch_curve(&clip_id, note_id);
    let reloaded_clip = restored.tracks[0].clips[0].id.clone();
    let reloaded_note = restored.midi_clip_notes(&reloaded_clip).unwrap()[0].id;
    let reloaded = restored.note_pitch_curve(&reloaded_clip, reloaded_note);
    assert_eq!(original.len(), reloaded.len(), "control points were lost");
    for (a, b) in original.points.iter().zip(&reloaded.points) {
        assert_eq!(
            a.id, b.id,
            "point identity must survive so undo/selection do"
        );
        assert_eq!(a.shape, b.shape, "segment shape was not persisted");
        assert!((a.beat - b.beat).abs() < 1e-6);
        assert!((a.cents - b.cents).abs() < 1e-3);
    }
}

// ── Articulation ────────────────────────────────────────────────────────────

/// A note's articulation must reach the engine as a *recorded* articulation id,
/// not as the score marking the editor shows.
///
/// Before this path existed, per-note articulation was persisted, undoable
/// project state that changed no sound: every Solfage note played whatever the
/// instrument's default recording was, whatever the score said.
#[test]
fn a_notes_articulation_reaches_the_engine_as_a_recorded_articulation() {
    use timeline_state::ArticulationId;

    // Sustained markings choose the sustained recording; short, separated
    // markings choose the short bowed one.
    for (marking, expected) in [
        (ArticulationId::Sustain, 0u16),
        (ArticulationId::Legato, 0),
        (ArticulationId::Tenuto, 0),
        (ArticulationId::Staccato, 2),
        (ArticulationId::Staccatissimo, 2),
        (ArticulationId::Accent, 2),
        (ArticulationId::Marcato, 2),
    ] {
        let (mut state, clip_id) = instrument_state_with_clip();
        let note_id = state.add_midi_note(&clip_id, 60, 0.0, 1.0, 100).unwrap();
        let note = state
            .midi_clip_notes_mut(&clip_id)
            .unwrap()
            .iter_mut()
            .find(|n| n.id == note_id)
            .unwrap();
        note.articulation = Some(marking);

        let snap = build_engine_project_snapshot(&state, 48_000, None, None);
        assert_eq!(
            snap.midi_clips[0].notes[0].articulation,
            Some(expected),
            "{marking:?} must select recorded articulation {expected}"
        );
    }
}

/// A note with no marking of its own follows the clip's direction lane, the
/// same way playback resolves it — otherwise the direction lane would be
/// visible in the editor and inaudible in the instrument.
#[test]
fn an_unmarked_note_follows_the_clip_direction() {
    use timeline_state::{ArticulationId, MidiArticulationEvent};

    let (mut state, clip_id) = instrument_state_with_clip();
    state.add_midi_note(&clip_id, 60, 1.0, 1.0, 100).unwrap();
    state.set_midi_articulations(
        &clip_id,
        vec![MidiArticulationEvent::new(0.0, ArticulationId::Staccato)],
    );

    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    assert_eq!(
        snap.midi_clips[0].notes[0].articulation,
        Some(2),
        "the clip's direction lane must reach the instrument"
    );
}

/// No marking anywhere leaves the instrument on its own default, which is what
/// happened for every note before this path existed.
#[test]
fn a_note_with_no_articulation_anywhere_leaves_the_instrument_alone() {
    let (mut state, clip_id) = instrument_state_with_clip();
    state.add_midi_note(&clip_id, 60, 0.0, 1.0, 100).unwrap();
    let snap = build_engine_project_snapshot(&state, 48_000, None, None);
    assert_eq!(snap.midi_clips[0].notes[0].articulation, None);
}
