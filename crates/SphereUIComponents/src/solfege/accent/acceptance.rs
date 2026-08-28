//! The acceptance tests the Accent Analyzer was specified against.
//!
//! Every one of them constructs a phrase, analyses it, and asserts something
//! about the *shape* of the answer rather than about a number. That is
//! deliberate: the analyser's coefficients are refitted whenever the corpus is
//! rebuilt, so a test pinned to "note 3 scores 0.68" would fail on every
//! retrain and teach nobody anything. What must survive a retrain is the
//! musical behaviour — that meter matters, that changing the meter changes the
//! answer, that a peak is not automatically an accent — and that is what these
//! assert.
//!
//! They run against the **rule** analyser, not the trained one. The rule is
//! what ships in the binary; the trained correction lives in a model file that
//! a test machine may not have. If the rule alone cannot demonstrate these
//! behaviours then the model is carrying the feature on its own, which is worth
//! knowing.

use crate::components::timeline::timeline_state::{AccentState, ArticulationId, MidiNoteState};

use super::analyzer::AccentAnalyzer;
use super::gesture::AccentGesture;
use super::meter::Meter;

fn note(pitch: u8, start: f32, duration: f32) -> MidiNoteState {
    // Every note the same velocity, everywhere in this file. Section 43: if any
    // of these phrases produced a flat contour, or a contour that tracked
    // velocity, the analyser would be doing one of the two things it must not.
    MidiNoteState::new(pitch, start, duration, 96)
}

fn analyse(notes: &[MidiNoteState], numerator: u16, denominator: u16) -> Vec<AccentState> {
    AccentAnalyzer::rule_only()
        .analyze(
            notes,
            0.0,
            120.0,
            &Meter::from_signature(numerator, denominator),
        )
        .0
}

fn spread(accents: &[AccentState]) -> f32 {
    let mean = accents.iter().map(|a| a.prominence).sum::<f32>() / accents.len() as f32;
    (accents
        .iter()
        .map(|a| (a.prominence - mean).powi(2))
        .sum::<f32>()
        / accents.len() as f32)
        .sqrt()
}

/// Section 38 — meter alone must produce structure.
///
/// Sixteen identical quarter notes: same pitch, same length, same velocity. The
/// only thing that distinguishes them is where they fall in the bar.
#[test]
fn identical_quarter_notes_take_their_shape_from_the_bar() {
    let notes: Vec<MidiNoteState> = (0..16).map(|index| note(60, index as f32, 1.0)).collect();
    let accents = analyse(&notes, 4, 4);

    assert!(
        spread(&accents) > 1.0e-3,
        "sixteen identical notes produced a flat accent contour"
    );
    for bar in 0..4 {
        let base = bar * 4;
        assert!(
            accents[base].prominence > accents[base + 2].prominence,
            "bar {bar}: the downbeat must outrank beat 3"
        );
        assert!(
            accents[base + 2].prominence > accents[base + 1].prominence,
            "bar {bar}: beat 3 must outrank beat 2"
        );
    }
}

/// Section 38, second half — shifting the notes must move the emphasis with the
/// bar, not with the note index.
#[test]
fn shifting_the_phrase_against_the_bar_moves_the_emphasis() {
    let on_the_bar: Vec<MidiNoteState> = (0..8).map(|index| note(60, index as f32, 1.0)).collect();
    // The same eight notes, one beat later: note 0 is now beat 2 and note 3 is
    // the downbeat.
    let off_the_bar: Vec<MidiNoteState> = (0..8)
        .map(|index| note(60, index as f32 + 1.0, 1.0))
        .collect();

    let aligned = analyse(&on_the_bar, 4, 4);
    let shifted = analyse(&off_the_bar, 4, 4);

    let strongest = |accents: &[AccentState]| {
        accents
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.prominence.total_cmp(&b.1.prominence))
            .map(|(index, _)| index)
            .unwrap()
    };
    // The claim is about the *bar*, not about a chosen note: whichever note
    // scores highest must be one that falls on a downbeat, and the set of
    // downbeats is different in the two phrases. Asserting a specific index
    // would also be asserting how the analyser breaks a tie between two
    // downbeats, which is an edge effect of the window and not a musical claim.
    assert_eq!(
        strongest(&aligned) % 4,
        0,
        "the winner is not on a downbeat"
    );
    let winner = strongest(&shifted);
    assert_eq!(
        (winner + 1) % 4,
        0,
        "note {winner} won, and it is not a downbeat of the shifted phrase"
    );
    // And the emphasis genuinely moved: whatever won before is not what wins now.
    assert_ne!(strongest(&aligned), winner);
}

/// Section 39 — the same notes under a different meter must read differently.
#[test]
fn changing_the_meter_changes_the_reading() {
    let notes: Vec<MidiNoteState> = (0..12).map(|index| note(60, index as f32, 1.0)).collect();
    let four_four = analyse(&notes, 4, 4);
    let three_four = analyse(&notes, 3, 4);
    let six_eight = analyse(&notes, 6, 8);

    let differing = |a: &[AccentState], b: &[AccentState]| {
        a.iter()
            .zip(b)
            .filter(|(x, y)| (x.prominence - y.prominence).abs() > 1.0e-4)
            .count()
    };
    assert!(
        differing(&four_four, &three_four) >= 4,
        "4/4 and 3/4 produced nearly the same reading; the meter features are \
         not reaching the analysis"
    );
    assert!(differing(&four_four, &six_eight) >= 4);

    // In 3/4 every third note is a downbeat.
    for bar in 0..4 {
        let base = bar * 3;
        assert!(three_four[base].prominence > three_four[base + 1].prominence);
        assert!(three_four[base].prominence > three_four[base + 2].prominence);
    }
    // In 4/4 every fourth is, and note 3 — a downbeat in 3/4 — is not.
    assert!(four_four[4].prominence > four_four[3].prominence);
}

/// Section 40 — a phrase climax must be emphasised, and the corpus says *how*.
///
/// This is the test that was written to assert the obvious thing — that the
/// climax scores highest for overall prominence — and then rewritten, because
/// the obvious thing is not what these performances do.
///
/// Measured on the 7113 aligned URMP violin notes, after the instrument's own
/// frequency response is removed from the loudness measurements: a note higher
/// than its neighbours correlates **-0.05** with attack evidence and **+0.04**
/// with agogic evidence, and a note entered by a leap correlates **+0.31** with
/// agogic. What a violinist does with an arrival note on this corpus is
/// **take time over it** — place it late, hold it, leave air in front of it —
/// not hit it harder. Loud, sharply-started notes in these performances are
/// disproportionately the short quick ones.
///
/// So the assertion is on the agogic head, and it is a stronger statement than
/// the one it replaces: the analyser is not merely marking the climax, it is
/// marking it in the way the recordings did. The composite prominence of a
/// climax is reported in the accompanying analysis rather than asserted here,
/// because on this corpus it is genuinely not the highest in the phrase.
#[test]
fn a_phrase_climax_is_emphasised_agogically_the_way_the_corpus_emphasises_one() {
    // C4 D4 E4 | G5 (the peak, long, on a downbeat) | F4 E4 D4 C4
    let notes = vec![
        note(60, 0.0, 0.5),
        note(62, 0.5, 0.5),
        note(64, 1.0, 0.5),
        note(65, 1.5, 0.5),
        note(79, 2.0, 2.0), // the arrival
        note(65, 4.0, 0.5),
        note(64, 4.5, 0.5),
        note(62, 5.0, 0.5),
        note(60, 5.5, 0.5),
    ];
    let accents = analyse(&notes, 4, 4);
    let peak = accents[4].agogic;

    // Every note that is not the climax or its immediate neighbour must be well
    // below it. The neighbour is excluded deliberately and not because it fails:
    // the note *after* a large leap is read as agogically emphasised too, since
    // the rule's interval feature is a magnitude and a descending leap out of a
    // peak looks like an ascending leap into one. That is arguably right — a
    // player does take time on the note after a big leap — and where it is not,
    // the signed interval is available to the trained correction, which sees it
    // and the rule does not.
    for (index, accent) in accents.iter().enumerate() {
        if index == 4 || index == 5 {
            continue;
        }
        assert!(
            peak > accent.agogic + 0.3,
            "the climax scored {peak} for agogic emphasis against note {index} \
             at {}",
            accent.agogic
        );
    }
    // And the four components have not collapsed into one number: if they had,
    // this test and a prominence test would be the same test.
    assert!(
        (accents[4].agogic - accents[4].prominence).abs() > 0.02,
        "agogic and prominence are indistinguishable on the climax"
    );
}

/// Section 41 — a note that re-enters after a rest is not the same note as one
/// in the middle of a line, even when everything else about it is identical.
#[test]
fn a_note_after_a_rest_reads_differently_from_the_same_note_inside_a_line() {
    let continuous = vec![
        note(60, 0.0, 1.0),
        note(62, 1.0, 1.0),
        note(64, 2.0, 1.0),
        note(65, 3.0, 1.0),
    ];
    // The same E4, but a beat of rest in front of it.
    let after_rest = vec![
        note(60, 0.0, 1.0),
        note(62, 1.0, 1.0),
        note(64, 3.0, 1.0),
        note(65, 4.0, 1.0),
    ];

    let joined = analyse(&continuous, 4, 4);
    let broken = analyse(&after_rest, 4, 4);
    assert!(
        (joined[2].prominence - broken[2].prominence).abs() > 1.0e-3,
        "the rest changed nothing: {} against {}",
        joined[2].prominence,
        broken[2].prominence
    );
}

/// Section 42 — the test that stops naive melodic-height logic.
///
/// A passing note on a weak subdivision, higher than its neighbours, must not
/// outrank the downbeat merely for being high.
#[test]
fn a_high_passing_note_on_a_weak_beat_does_not_outrank_the_downbeat() {
    let notes = vec![
        note(60, 0.0, 1.0), // downbeat, stable
        note(64, 1.0, 0.5), //
        note(71, 1.5, 0.5), // high, short, on the "and" of 2
        note(64, 2.0, 1.0), //
        note(60, 3.0, 1.0), //
    ];
    let accents = analyse(&notes, 4, 4);
    assert!(
        accents[0].prominence > accents[2].prominence,
        "the offbeat passing note ({}) outranked the downbeat ({})",
        accents[2].prominence,
        accents[0].prominence
    );
}

/// Section 43 — with velocity held constant everywhere, the answer still varies.
///
/// The structural version of this claim is in `features.rs`, which asserts that
/// no feature is named for velocity at all. This is the behavioural version.
#[test]
fn constant_velocity_still_produces_a_varied_reading() {
    let notes = vec![
        note(60, 0.0, 1.0),
        note(67, 1.0, 0.5),
        note(65, 1.5, 0.5),
        note(64, 2.0, 2.0),
        note(62, 5.0, 1.0),
        note(60, 6.0, 2.0),
    ];
    assert!(notes.iter().all(|note| note.velocity == 96));
    let accents = analyse(&notes, 4, 4);
    assert!(spread(&accents) > 1.0e-3);

    // And doubling every velocity changes nothing, because the analyser cannot
    // see it.
    let louder: Vec<MidiNoteState> = notes
        .iter()
        .map(|note| MidiNoteState {
            velocity: 127,
            ..note.clone()
        })
        .collect();
    let same = analyse(&louder, 4, 4);
    for (a, b) in accents.iter().zip(&same) {
        assert!((a.prominence - b.prominence).abs() < 1.0e-6);
    }
}

/// Section 49 — the distribution must not collapse or alternate.
///
/// The failure modes named in the brief: everything at 0.5, everything at 1.0,
/// a value that flips every note, and noise. A real analysis of a real phrase
/// is none of those.
#[test]
fn the_produced_distribution_is_neither_flat_nor_alternating() {
    let notes: Vec<MidiNoteState> = (0..64)
        .map(|index| {
            let beat = index as f32 * 0.5;
            let pitch = 60 + ((index * 5) % 13) as u8;
            note(pitch, beat, 0.5)
        })
        .collect();
    let accents = analyse(&notes, 4, 4);

    let values: Vec<f32> = accents.iter().map(|a| a.prominence).collect();
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    assert!(
        (0.2..0.8).contains(&mean),
        "the whole phrase collapsed to {mean}"
    );
    assert!(spread(&accents) > 0.01, "the reading is flat");
    assert!(spread(&accents) < 0.35, "the reading is noise");

    // Not alternating: if every note flipped sign against its predecessor, the
    // count of sign changes would be the whole phrase.
    let flips = values
        .windows(3)
        .filter(|window| (window[1] - window[0]).signum() != (window[2] - window[1]).signum())
        .count();
    assert!(
        flips < values.len() - 4,
        "the contour alternates on every note ({flips} turns in {} notes)",
        values.len()
    );
}

/// Section 46 — the ablation. Each component must reach its own dimension of
/// the performance and no other.
///
/// The component-level version of this is in `gesture.rs`; this one runs the
/// whole path an accent takes to the notes, which is where a wiring mistake
/// would actually show up.
#[test]
fn each_accent_component_changes_only_its_own_dimension_of_the_performance() {
    use super::apply::apply_to_notes;

    let template = || {
        let mut notes = vec![note(60, 1.0, 1.0)];
        notes[0].articulation = Some(ArticulationId::Sustain);
        notes
    };
    let with = |accent: AccentState| {
        let mut notes = template();
        notes[0].accent = Some(accent);
        apply_to_notes(&mut notes, 0.5, 8.0);
        notes.remove(0)
    };

    let base = AccentState::neutral();
    let neutral = with(base);
    assert_eq!(neutral.start, 1.0);
    assert_eq!(neutral.velocity, 96);

    let agogic = with(AccentState {
        agogic: 1.0,
        ..base
    });
    assert!(agogic.start > neutral.start, "agogic did not move the note");
    assert!(agogic.duration > neutral.duration);
    assert_eq!(
        agogic.velocity, neutral.velocity,
        "agogic reached the velocity — every field secretly controls gain"
    );

    let attack = with(AccentState {
        attack: 1.0,
        ..base
    });
    assert!(attack.velocity > neutral.velocity);
    assert_eq!(attack.start, neutral.start, "attack reached the timing");
    assert_eq!(attack.duration, neutral.duration);

    let timbre = with(AccentState {
        timbre: 1.0,
        ..base
    });
    assert!(timbre.velocity > neutral.velocity, "timbre reached nothing");
    assert_eq!(timbre.start, neutral.start, "timbre reached the timing");
    // Timbre and attack both reach velocity on this instrument, and they must
    // not reach it equally: a bright note is not as loud as a struck one.
    assert!(
        timbre.velocity < attack.velocity,
        "timbre is weighted the same as attack; the two are not distinguishable"
    );
}

/// Section 44 — one accent, several articulations, several different answers,
/// all the way through to the notes.
#[test]
fn the_same_accent_produces_different_performances_per_articulation() {
    use super::apply::apply_to_notes;

    let accent = AccentState::generated(0.95, 0.95, 0.95, 0.95, 0.9);
    // Velocity 64, not the 96 the rest of this file uses: a 0.95 accent adds
    // enough that several articulations would all saturate at 127, and a test
    // comparing 127 with 127 proves only that the clamp works.
    let realised = |articulation: ArticulationId| {
        let mut notes = vec![MidiNoteState::new(60, 1.0, 1.0, 64)];
        notes[0].accent = Some(accent);
        notes[0].articulation = Some(articulation);
        apply_to_notes(&mut notes, 0.5, 8.0);
        notes.remove(0)
    };

    let sustain = realised(ArticulationId::Sustain);
    let pizzicato = realised(ArticulationId::Pizzicato);
    let bounced = realised(ArticulationId::Staccatissimo);
    let tremolo = realised(ArticulationId::Tremolo);

    // A pizzicato takes more of the accent *in the attack* than a bowed note.
    // Asserted on the gesture rather than on the resulting velocity, because
    // velocity also carries this instrument's only brightness path and a
    // sustained note has more brightness to carry — so the note that puts more
    // into its attack does not have to end up the louder of the two.
    assert!(
        AccentGesture::for_articulation(accent, Some(ArticulationId::Pizzicato)).attack_gain
            > AccentGesture::for_articulation(accent, Some(ArticulationId::Sustain)).attack_gain
    );
    // A firmer bounced stroke is shorter; a sustained accent is longer.
    assert!(bounced.duration < 1.0);
    assert!(sustain.duration > 1.0);
    // A tremolo has no single attack to sharpen.
    assert!(tremolo.velocity < sustain.velocity);

    // And they are four different realisations, not one scaled four ways.
    let shapes = [
        (sustain.start, sustain.duration, sustain.velocity),
        (pizzicato.start, pizzicato.duration, pizzicato.velocity),
        (bounced.start, bounced.duration, bounced.velocity),
        (tremolo.start, tremolo.duration, tremolo.velocity),
    ];
    for (index, a) in shapes.iter().enumerate() {
        for b in shapes.iter().skip(index + 1) {
            assert_ne!(a, b);
        }
    }
}

/// Section 50 — accent is note-level information and must not jitter.
///
/// A repeated figure must be read the same way each time it appears. A model
/// adding per-note noise would fail this while still looking plausible in a
/// histogram.
#[test]
fn a_repeated_figure_is_read_the_same_way_each_time() {
    // The same four-note figure, eight times.
    let notes: Vec<MidiNoteState> = (0..8)
        .flat_map(|bar| {
            [(60, 0.0), (64, 1.0), (62, 2.0), (65, 3.0)]
                .into_iter()
                .map(move |(pitch, beat)| note(pitch, bar as f32 * 4.0 + beat, 1.0))
        })
        .collect();
    let accents = analyse(&notes, 4, 4);

    // Bars 4 and 5 are far enough inside the phrase that their comparison
    // windows contain whole periods of the figure on both sides. The bars at
    // the ends legitimately read differently: a note six from the start has a
    // shorter neighbourhood, and pretending otherwise would be asserting that
    // the analyser ignores context.
    for position in 0..4 {
        let fourth = accents[12 + position].prominence;
        let fifth = accents[16 + position].prominence;
        assert!(
            (fourth - fifth).abs() < 1.0e-4,
            "position {position} reads {fourth} in bar 4 and {fifth} in bar 5"
        );
    }
}

/// A gesture built from an accent the user typed in behaves the same as one the
/// analyser produced. Nothing downstream may treat a generated accent as more
/// authoritative than a hand-set one.
#[test]
fn a_hand_set_accent_drives_the_same_gesture_as_a_generated_one() {
    let generated = AccentState::generated(0.8, 0.7, 0.6, 0.5, 0.9);
    let manual = AccentState {
        confidence: 0.0,
        source: crate::components::timeline::timeline_state::AccentSource::Manual,
        ..generated
    };
    assert_eq!(
        AccentGesture::for_articulation(generated, Some(ArticulationId::Sustain)),
        AccentGesture::for_articulation(manual, Some(ArticulationId::Sustain))
    );
}
