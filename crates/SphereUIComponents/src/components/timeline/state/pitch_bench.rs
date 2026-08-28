//! Cost of the pitch editor's per-frame and per-pointer-event data work.
//!
//! These are not micro-benchmarks for their own sake. The Pitch tab stutters
//! while a Draw stroke is in flight, and the question "what is it actually
//! spending the frame on" has to be answered with numbers before anything is
//! changed, because two of the plausible culprits (curve evaluation, the
//! interpolation maths) turn out to be nearly free and two others (validating
//! the render cache, rebuilding the trajectory) turn out to dominate.
//!
//! Run with:
//!
//! ```text
//! cargo test -p sphere_ui_components --lib pitch_bench -- --nocapture --ignored
//! ```
//!
//! They are `#[ignore]` because they are measurements, not assertions: a
//! machine under load would fail a wall-clock threshold for reasons that have
//! nothing to do with the code. The one exception is
//! [`draw_stroke_cost_does_not_grow_with_clip_length`], which asserts a
//! *shape* rather than a duration — it is the property the fix has to have,
//! and it holds regardless of how fast the machine is.

use std::time::{Duration, Instant};

use super::*;

/// A clip that looks like something a person would actually be editing: a
/// melody line, most notes plain, some carrying a drawn pitch curve of the
/// density `simplify_stroke` leaves behind.
fn realistic_clip(notes: usize, curved_every: usize, points_per_curve: usize) -> Vec<MidiNoteState> {
    let mut out = Vec::with_capacity(notes);
    let scale = [0_i32, 2, 4, 5, 7, 9, 11];
    for index in 0..notes {
        let mut note = MidiNoteState::new(
            (60 + scale[index % scale.len()] + 12 * ((index / 24) as i32 % 2)) as u8,
            index as f32 * 0.5,
            0.45,
            96,
        );
        if index % curved_every == 0 {
            let mut points = Vec::with_capacity(points_per_curve);
            for p in 0..points_per_curve {
                let t = p as f32 / points_per_curve.max(1) as f32;
                points.push(PitchPoint {
                    id: (index * 1000 + p) as u64,
                    beat: t * 0.45,
                    // A shape with real curvature, so evaluation cannot be
                    // short-circuited by every segment being flat.
                    cents: 40.0 * (t * std::f32::consts::TAU * 1.5).sin(),
                    shape: PitchSegmentShape::Smooth,
                });
            }
            note.pitch_curve = Some(PitchCurve { points });
        }
        out.push(note);
    }
    out
}

fn percentiles(mut samples: Vec<Duration>) -> (f64, f64, f64, f64) {
    samples.sort_unstable();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| {
        let i = ((samples.len() as f64 - 1.0) * q).round() as usize;
        ms(samples[i])
    };
    let mean = samples.iter().map(|d| ms(*d)).sum::<f64>() / samples.len() as f64;
    (mean, at(0.5), at(0.95), at(0.99))
}

fn report(label: &str, samples: Vec<Duration>) {
    let (mean, p50, p95, p99) = percentiles(samples);
    println!("  {label:<44} mean {mean:7.3} ms   p50 {p50:7.3}   p95 {p95:7.3}   p99 {p99:7.3}");
}

/// What one Draw pointer event costs today, broken down by stage.
///
/// The stages are the ones `SolfegeEditorPanel::pitch_canvas_data` and the
/// canvas paint closure actually run, in the order they run them.
#[test]
#[ignore = "measurement, not an assertion"]
fn draw_stroke_frame_cost_breakdown() {
    const ITERATIONS: usize = 200;
    const COLUMNS: usize = 1600; // a maximised editor on a 1080p display

    for (label, note_count) in [("64-note clip", 64), ("256-note clip", 256), ("1024-note clip", 1024)] {
        let notes = realistic_clip(note_count, 4, 48);
        let directions: Vec<MidiArticulationEvent> = Vec::new();
        println!("\n{label} ({} notes, {COLUMNS} columns):", notes.len());

        // 1. Cache validation. Runs every frame, whether or not anything moved.
        let cached = notes.clone();
        let mut samples = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let start = Instant::now();
            let equal = std::hint::black_box(&cached) == std::hint::black_box(&notes);
            samples.push(start.elapsed());
            assert!(equal);
        }
        report("cache validity (deep Vec compare)", samples);

        // 2. The clone the cache miss path does before rebuilding.
        let mut samples = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let start = Instant::now();
            let cloned = std::hint::black_box(notes.clone());
            samples.push(start.elapsed());
            std::hint::black_box(cloned);
        }
        report("clone whole note vector", samples);

        // 3. Rebuilding the whole clip's trajectory: the cache miss path.
        let mut samples = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let start = Instant::now();
            let built = PitchTrajectory::build(&notes, &directions);
            samples.push(start.elapsed());
            std::hint::black_box(built);
        }
        report("PitchTrajectory::build (whole clip)", samples);

        // 4. Resampling one column per pixel, for every voice.
        let trajectory = PitchTrajectory::build(&notes, &directions);
        let mut scratch: Vec<Option<f32>> = Vec::with_capacity(COLUMNS + 1);
        let mut samples = Vec::with_capacity(ITERATIONS);
        for _ in 0..ITERATIONS {
            let start = Instant::now();
            for voice in 0..trajectory.voices().len() {
                trajectory.sample_columns(&notes, voice, 0.0, 0.02, COLUMNS + 1, &mut scratch);
                std::hint::black_box(&scratch);
            }
            samples.push(start.elapsed());
        }
        report(
            &format!("sample_columns x {} voice(s)", trajectory.voices().len()),
            samples,
        );
    }
}

/// What the engine snapshot pays to emit drawn pitch for a whole clip.
///
/// `layout::engine_snapshot` samples every non-muted note's trajectory at 5 ms
/// and then decides whether the result was worth emitting, so a clip of plain
/// notes pays the full sampling cost to discover it had nothing to say. Each
/// call also restarts `sample_columns`' span cursor at the beginning of the
/// voice, so walking the clip note by note rescans the spans already walked —
/// quadratic in the number of notes in one voice.
///
/// This runs on every project sync, and a pitch drag marks the project dirty
/// on every pointer event.
#[test]
#[ignore = "measurement, not an assertion"]
fn engine_snapshot_pitch_emission_cost() {
    // 120 bpm: 5 ms is 0.01 beats, matching PITCH_SAMPLE_SECONDS at that tempo.
    const STEP_BEATS: f32 = 0.01;

    for note_count in [64_usize, 256, 1024] {
        let notes = realistic_clip(note_count, 4, 48);
        let directions: Vec<MidiArticulationEvent> = Vec::new();
        let trajectory = PitchTrajectory::build(&notes, &directions);
        let mut scratch: Vec<Option<f32>> = Vec::new();

        // The snapshot builds this table once and indexes it; resolving a
        // note's voice by scanning the voice lists would be the benchmark's
        // own quadratic, not the product's.
        let mut voice_of_note = vec![0_usize; notes.len()];
        for (voice_index, voice) in trajectory.voices().iter().enumerate() {
            for &note_index in &voice.notes {
                voice_of_note[note_index] = voice_index;
            }
        }

        for ask_first in [false, true] {
            let mut samples = Vec::with_capacity(24);
            for _ in 0..24 {
                let start = Instant::now();
                for (index, note) in notes.iter().enumerate() {
                    let voice = voice_of_note[index];
                    // `ask_first == false` is the order the snapshot used
                    // before: sample the note at control resolution, then look
                    // at the samples to decide it had nothing to say.
                    if ask_first
                        && !trajectory.note_departs_from_notated_pitch(&notes, voice, index)
                    {
                        continue;
                    }
                    let columns =
                        ((note.duration / STEP_BEATS).ceil() as usize).clamp(1, 1 << 16) + 1;
                    trajectory.sample_columns(
                        &notes,
                        voice,
                        note.start,
                        STEP_BEATS,
                        columns,
                        &mut scratch,
                    );
                    std::hint::black_box(&scratch);
                }
                samples.push(start.elapsed());
            }
            let order = if ask_first { "ask first" } else { "sample all" };
            report(
                &format!("snapshot pitch emission, {note_count} notes, {order}"),
                samples,
            );
        }
    }
}
