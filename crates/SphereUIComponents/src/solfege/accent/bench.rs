//! How long an analysis actually takes.
//!
//! Reported rather than asserted where the number is a measurement, and
//! asserted only where a regression would be a real problem. The threshold is
//! deliberately loose — this runs on whatever machine happens to be building —
//! and it is there to catch an accidental quadratic, not to police
//! microseconds.
//!
//! Run it with output:
//!
//! ```text
//! cargo test -p sphere_ui_components --lib accent::bench -- --nocapture
//! ```

use std::time::Instant;

use crate::components::timeline::timeline_state::MidiNoteState;

use super::analyzer::AccentAnalyzer;
use super::meter::Meter;

/// A phrase of `count` notes with a plausible mixture of lengths and contour,
/// rather than a run of identical quarters: several features compare a note
/// with a window of its neighbours, and a constant phrase would let a
/// pathological median implementation look fast.
fn phrase(count: usize) -> Vec<MidiNoteState> {
    let mut notes = Vec::with_capacity(count);
    let mut beat = 0.0_f32;
    for index in 0..count {
        let duration = match index % 7 {
            0 => 1.0,
            1 | 2 => 0.5,
            3 => 1.5,
            _ => 0.25,
        };
        let pitch = 55 + ((index * 7) % 25) as u8;
        notes.push(MidiNoteState::new(pitch, beat, duration, 96));
        beat += duration;
    }
    notes
}

fn time_analysis(count: usize) -> (f64, usize) {
    let notes = phrase(count);
    let analyzer = AccentAnalyzer::rule_only();
    let meter = Meter::from_signature(4, 4);
    // One warm pass so the rule's embedded coefficients are parsed and the
    // first-call JSON cost does not land on the measurement.
    let _ = analyzer.analyze(&notes, 0.0, 120.0, &meter);

    const REPEATS: usize = 5;
    let started = Instant::now();
    let mut produced = 0;
    for _ in 0..REPEATS {
        produced = analyzer.analyze(&notes, 0.0, 120.0, &meter).0.len();
    }
    (
        started.elapsed().as_secs_f64() * 1000.0 / REPEATS as f64,
        produced,
    )
}

#[test]
fn analysis_of_a_realistic_phrase_is_far_below_a_frame() {
    for count in [16usize, 64, 256, 1024] {
        let (milliseconds, produced) = time_analysis(count);
        assert_eq!(produced, count);
        println!("  {count:5} notes  {milliseconds:8.3} ms");
        // Measured at 0.83 ms for a thousand notes in release on the machine
        // this was written on, and this does not run on the frame thread
        // anyway. The debug budget is separate and much wider because the
        // difference between the profiles here is an order of magnitude — the
        // pass is small arithmetic in tight loops, which is exactly what
        // optimisation is good at — and a threshold set for release would fail
        // every `cargo test` run without one.
        //
        // Either number still catches the regression they were written after:
        // rebuilding the metrical grid once per note instead of once per meter
        // cost 9.3 ms in release and far more in debug.
        let budget = if cfg!(debug_assertions) { 40.0 } else { 5.0 };
        assert!(
            milliseconds < budget,
            "{count} notes took {milliseconds:.3} ms, budget {budget} ms"
        );
    }
}

/// The cost must grow with the number of notes, not with its square.
///
/// Every "compared with its neighbours" feature walks a fixed window, so
/// sixteen times the notes should cost roughly sixteen times as much. A
/// quadratic would cost 256 times as much and would make a long clip
/// unusable while a short one looked fine.
#[test]
fn analysis_cost_grows_with_the_note_count_not_its_square() {
    let (small, _) = time_analysis(64);
    let (large, _) = time_analysis(1024);
    let ratio = large / small.max(1.0e-6);
    println!("  64 -> 1024 notes: {ratio:.1}x for 16x the notes");
    assert!(
        ratio < 64.0,
        "1024 notes cost {ratio:.1} times 64 notes; that is not linear"
    );
}
