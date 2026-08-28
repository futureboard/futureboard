//! What one Draw stroke in the Pitch editor actually costs.
//!
//! A stroke is not one operation, it is one operation per mouse-move sample,
//! and each of those runs the whole chain: write a point into the curve,
//! invalidate the canvas, rebuild the evaluated trajectory, repaint. Any part of
//! that chain which is linear in the points already drawn makes the stroke
//! quadratic in its own length — the drawing gets slower the longer you draw,
//! which is exactly the shape of "laggy" a user reports.
//!
//! These are measurements, printed. Only the growth guards assert, because the
//! absolute numbers depend on the machine and the profile.
//!
//! ```text
//! cargo test -p sphere_ui_components --release --lib pitch_stroke_bench -- --nocapture
//! ```

use std::time::Instant;

use super::*;

/// Samples in a representative stroke.
///
/// A drag across a couple of bars at a normal zoom emits a mouse-move every
/// frame or two; a few hundred is an ordinary gesture, not a stress test.
const STROKE_SAMPLES: usize = 400;

/// Merge distance the editor uses while drawing, in beats.
const MERGE_BEATS: f32 = 1.0 / 96.0;

/// Beats between consecutive samples of the simulated stroke.
///
/// Comfortably above `MERGE_BEATS`, because a sample closer than the merge
/// distance overwrites the previous point instead of adding one — which is the
/// cheap case and not the one that gets slow. Four hundredths of a beat is a
/// pointer moving at an ordinary speed across an ordinarily zoomed editor.
const SAMPLE_SPACING_BEATS: f32 = 0.04;

fn stroke_curve(samples: usize, merge: f32) -> (PitchCurve, f64) {
    let mut curve = PitchCurve::default();
    let started = Instant::now();
    for index in 0..samples {
        let beat = index as f32 * SAMPLE_SPACING_BEATS;
        curve.set_point(
            beat,
            120.0 * (beat * 3.0).sin(),
            PitchSegmentShape::Smooth,
            merge,
        );
    }
    (curve, started.elapsed().as_secs_f64() * 1000.0)
}

fn clip_with_stroke(notes: usize, curve: &PitchCurve) -> Vec<MidiNoteState> {
    (0..notes)
        .map(|index| {
            let span = STROKE_SAMPLES as f32 * SAMPLE_SPACING_BEATS;
            let mut note =
                MidiNoteState::new(60 + (index % 12) as u8, index as f32 * span, span, 96);
            note.pitch_curve = Some(curve.clone());
            note
        })
        .collect()
}

#[test]
fn one_stroke_does_not_get_slower_the_longer_it_is_drawn() {
    let (short, short_ms) = stroke_curve(STROKE_SAMPLES / 4, MERGE_BEATS);
    let (long, long_ms) = stroke_curve(STROKE_SAMPLES, MERGE_BEATS);
    println!(
        "  {:4} samples -> {:4} points  {short_ms:7.3} ms",
        STROKE_SAMPLES / 4,
        short.len()
    );
    println!(
        "  {:4} samples -> {:4} points  {long_ms:7.3} ms",
        STROKE_SAMPLES,
        long.len()
    );

    // Four times the samples must not cost dramatically more than four times
    // the work. A per-sample linear scan and re-sort would put this at sixteen.
    let ratio = long_ms / short_ms.max(1.0e-6);
    println!("  4x the samples cost {ratio:.1}x");
    assert!(
        ratio < 9.0,
        "a stroke of {STROKE_SAMPLES} samples cost {ratio:.1}x one of \
         {}, which is quadratic growth: drawing gets slower the longer the \
         stroke",
        STROKE_SAMPLES / 4
    );
}

#[test]
fn rebuilding_the_trajectory_mid_stroke_is_affordable() {
    let (curve, _) = stroke_curve(STROKE_SAMPLES, MERGE_BEATS);
    for notes in [1usize, 16, 64] {
        let clip = clip_with_stroke(notes, &curve);
        let started = Instant::now();
        const REPEATS: usize = 20;
        for _ in 0..REPEATS {
            let trajectory = PitchTrajectory::build(&clip, &[]);
            std::hint::black_box(&trajectory);
        }
        let milliseconds = started.elapsed().as_secs_f64() * 1000.0 / REPEATS as f64;
        println!(
            "  {notes:3} notes x {} points  rebuild {milliseconds:7.3} ms",
            curve.len()
        );
    }
}

/// The canvas cache decides, once per frame, whether the trajectory it already
/// has is still valid. If that decision itself walks every point of every note,
/// it costs as much as the rebuild it is trying to avoid — and it pays that
/// cost on *every* frame, including the ones where nothing changed.
#[test]
fn deciding_whether_the_canvas_is_stale_is_cheaper_than_rebuilding_it() {
    let (curve, _) = stroke_curve(STROKE_SAMPLES, MERGE_BEATS);
    let clip = clip_with_stroke(64, &curve);
    let cached = clip.clone();

    const REPEATS: usize = 200;
    let started = Instant::now();
    for _ in 0..REPEATS {
        std::hint::black_box(cached == clip);
    }
    let compare_ms = started.elapsed().as_secs_f64() * 1000.0 / REPEATS as f64;

    let started = Instant::now();
    for _ in 0..20 {
        std::hint::black_box(PitchTrajectory::build(&clip, &[]));
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.0 / 20.0;

    println!("  deep compare {compare_ms:7.3} ms   rebuild {build_ms:7.3} ms");
    println!(
        "  compare is {:.0}% of a rebuild",
        compare_ms / build_ms * 100.0
    );
}
