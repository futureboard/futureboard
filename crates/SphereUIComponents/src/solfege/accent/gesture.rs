//! From a semantic accent to something an instrument can actually do.
//!
//! [`AccentState`] says *how much* emphasis a note wants and *by which means*.
//! It says nothing about bow force, pluck energy, or bow position, because
//! nothing observed those: URMP is audio and annotation, and a "bow pressure"
//! target derived from a loudness envelope would be a fabricated label wearing
//! a physical name. What is measured is what the ear hears — energy, brightness,
//! attack sharpness, placement, length — and the mapping from those to a
//! gesture is a stated modelling choice made here, in one place, where it can be
//! argued with.
//!
//! # The ranges are measured, not chosen
//!
//! Every number below is the median difference between the least and most
//! accented quartiles of the 7113 aligned URMP violin notes — the notes scoring
//! ≤0.25 against those scoring ≥0.75 on the relevant evidence:
//!
//! | quantity                | low accent | high accent | delta      |
//! |-------------------------|-----------:|------------:|-----------:|
//! | onset deviation         |  -18.5 ms  |   +23.2 ms  |  +41.7 ms  |
//! | duration ratio          |     0.883  |      0.971  |   +0.088   |
//! | unwritten gap before    |   -1.0 ms  |   +52.0 ms  |  +53.0 ms  |
//! | attack slope            |   23 dB/s  |  196 dB/s   | +172 dB/s  |
//! | spectral centroid       |  1.64·f0   |   2.44·f0   |   +0.80·f0 |
//!
//! So a fully accented note is placed about 40 ms later than an unaccented one,
//! held about 9% longer, and preceded by about 50 ms of extra air. Those are
//! the numbers, and they are small — which is the point. Section 23 asks for
//! deviations inside human ranges, and a violinist's agogic emphasis is tens of
//! milliseconds, not the quarter-second a "make it obvious" mapping would
//! reach for.
//!
//! # Articulation is not a modifier, it is the realisation
//!
//! An accent of 0.9 is one musical instruction and four different physical
//! events. A sustained note takes it as bow attack plus a swell plus brightness;
//! a pizzicato has no bow and no swell — a plucked string's level after the
//! attack is not something the player controls — so it takes almost all of it
//! in the pluck; a staccatissimo takes it as a firmer bounce that gets
//! *shorter*, not longer, because lengthening a bounced stroke stops it being
//! one; a tremolo has no single attack to sharpen and takes it as intensity and
//! brightness across the note.
//!
//! That is why there is no universal envelope here and no single `accent → gain`
//! line. [`AccentGesture::for_articulation`] is the whole point of the file.
//!
//! The vocabulary is the editor's — notation markings — not the voicebank's
//! techniques, and the two do not correspond one to one: `engine_snapshot`
//! sends Staccato, Staccatissimo, Accent and Marcato all to the bank's
//! *spiccato* recording. That is deliberate there and it is why the rows below
//! still differ from one another: the recording they select is the same, and
//! what a player does with an accent on a marcato is not what they do with one
//! on a staccatissimo.

use crate::components::timeline::timeline_state::{AccentState, ArticulationId};

/// Latest a fully agogic-accented note may be placed, in seconds.
///
/// The measured quartile difference is 41.7 ms and the measured p1..p99 range
/// of onset deviation across the whole corpus is ±180 ms. This is the former:
/// the corpus extreme includes notes whose alignment is doubtful, and a
/// generator that reaches for the extreme produces a performance no violinist
/// played.
const ONSET_SHIFT_SECONDS: f32 = 0.042;

/// Fractional change in a note's length at full agogic accent. Measured 0.088.
const DURATION_SCALE: f32 = 0.088;

/// Extra silence in front of a fully accented note, in seconds. Measured 53 ms.
const GAP_SECONDS: f32 = 0.053;

/// Loudest a fully accented note may be pushed, as a fraction of the engine's
/// dynamic range.
///
/// Not measured from the corpus the way the timing figures are, and it says so.
/// The measured `intensity level` difference between accent quartiles is 0.24 →
/// 0.81, but `level` is the quantity the *dynamic evidence itself* is built
/// from, so quoting it as a mapping range would be quoting the definition back.
/// This is a stated choice: a quarter of the dynamic range, which at the
/// engine's ~20 dB working window is about 5 dB — an audible accent, well short
/// of a new dynamic marking.
const LEVEL_BUMP: f32 = 0.25;

/// Brightness push at full timbral accent, normalized.
///
/// The measured centroid difference is 1.64 to 2.44 harmonics, a 49% rise. The
/// engine's brightness control is not calibrated in harmonics, so this is that
/// proportion expressed on its 0..1 scale rather than a claim about partials.
const BRIGHTNESS: f32 = 0.49;

/// One note's accent, resolved into what the performance should do.
///
/// Every field is a *delta* or a *scale*, never an absolute: a gesture is
/// something applied to a note that already has a velocity, a start and a
/// length, so a neutral accent must produce a gesture that changes nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccentGesture {
    /// Seconds to move the note's onset. Positive is later.
    pub onset_shift_seconds: f32,
    /// Multiplier on the written duration.
    pub duration_scale: f32,
    /// Extra silence to take from the end of the previous note, in seconds.
    pub gap_before_seconds: f32,
    /// Added to the note's velocity, in `0..1` engine units.
    pub attack_gain: f32,
    /// Peak of the local dynamics swell over this note, in `0..1`.
    ///
    /// A *contour*, not a step: the caller renders it as a rise and fall across
    /// the note rather than a gain change at the onset, which is what section 24
    /// asks for and what stops an accent sounding like an automation jump.
    pub level_bump: f32,
    /// Added to the brightness / bow-energy proxy, in `0..1`.
    pub brightness: f32,
}

impl AccentGesture {
    /// The gesture that changes nothing.
    pub const NEUTRAL: Self = Self {
        onset_shift_seconds: 0.0,
        duration_scale: 1.0,
        gap_before_seconds: 0.0,
        attack_gain: 0.0,
        level_bump: 0.0,
        brightness: 0.0,
    };

    /// Resolve an accent for the articulation it will be played with.
    ///
    /// Each component is first re-centred: an accent of 0.5 is "like its
    /// neighbours" and must produce no gesture at all, so what drives the
    /// mapping is `2 * (value - 0.5)`, signed, in `-1..=1`. A note *below*
    /// neutral is therefore de-emphasised — placed a little early, shortened a
    /// little, played a little softer — which is a real instruction and not
    /// merely the absence of one.
    pub fn for_articulation(accent: AccentState, articulation: Option<ArticulationId>) -> Self {
        let accent = accent.sanitized();
        let signed = |value: f32| (value - 0.5) * 2.0;
        let prominence = signed(accent.prominence);
        let attack = signed(accent.attack);
        let agogic = signed(accent.agogic);
        let timbre = signed(accent.timbre);

        // How much of the emphasis each articulation can express through each
        // channel. Not "how loud is this articulation" — how much of an accent
        // it is physically able to carry that way.
        let weights = ArticulationWeights::for_articulation(articulation);

        Self {
            onset_shift_seconds: agogic * ONSET_SHIFT_SECONDS * weights.agogic,
            duration_scale: 1.0 + agogic * DURATION_SCALE * weights.duration,
            gap_before_seconds: (agogic * GAP_SECONDS * weights.agogic).max(0.0),
            // Attack takes the note's own attack component plus a share of its
            // overall prominence: a note marked prominent with an unremarkable
            // attack component still starts more decisively than a neutral one.
            attack_gain: (0.7 * attack + 0.3 * prominence) * weights.attack * LEVEL_BUMP,
            level_bump: (0.5 * prominence + 0.5 * signed(accent.timbre.max(accent.prominence)))
                * weights.sustain
                * LEVEL_BUMP,
            brightness: (0.6 * timbre + 0.4 * prominence) * weights.brightness * BRIGHTNESS,
        }
    }

    /// `true` when this gesture would change nothing audible.
    ///
    /// Used to skip writing a performance for a note whose accent is neutral,
    /// so a generated performance carries expression only where there is some.
    pub fn is_neutral(&self) -> bool {
        const EPSILON: f32 = 1.0e-4;
        self.onset_shift_seconds.abs() < EPSILON
            && (self.duration_scale - 1.0).abs() < EPSILON
            && self.gap_before_seconds.abs() < EPSILON
            && self.attack_gain.abs() < EPSILON
            && self.level_bump.abs() < EPSILON
            && self.brightness.abs() < EPSILON
    }
}

/// How much of an accent each articulation can carry through each channel.
#[derive(Debug, Clone, Copy)]
struct ArticulationWeights {
    /// Placement and separation.
    agogic: f32,
    /// Length. Signed separately from `agogic` because a spiccato accent
    /// *shortens*: a longer spiccato is a different stroke, not a stronger one.
    duration: f32,
    /// The transient at the note's start.
    attack: f32,
    /// A swell across the body of the note.
    sustain: f32,
    brightness: f32,
}

impl ArticulationWeights {
    fn for_articulation(articulation: Option<ArticulationId>) -> Self {
        match articulation {
            // A plucked string. All the player controls is the pluck: after it,
            // the note decays on its own, so there is no swell to give it and
            // brightness follows the pluck rather than being separable from it.
            // Placement is still fully available — a pizzicato can be leaned on
            // by arriving late.
            Some(ArticulationId::Pizzicato) => Self {
                agogic: 1.0,
                duration: 0.2,
                attack: 1.3,
                sustain: 0.0,
                brightness: 0.5,
            },
            // The shortest bowed stroke the editor names, and the one that
            // reaches the bank's spiccato recording most literally. The accent
            // is in the firmness of the bounce, and a firmer bounce is
            // *shorter*, not longer — hence the negative duration weight.
            Some(ArticulationId::Staccatissimo) => Self {
                agogic: 0.8,
                duration: -0.6,
                attack: 1.2,
                sustain: 0.15,
                brightness: 0.8,
            },
            // Rapid repeated bow strokes. There is no single attack to sharpen,
            // so the emphasis lives in the intensity and colour of the whole
            // note.
            Some(ArticulationId::Tremolo) => Self {
                agogic: 0.7,
                duration: 0.6,
                attack: 0.35,
                sustain: 1.2,
                brightness: 1.1,
            },
            // Detached and short: some attack, little room for a swell.
            Some(ArticulationId::Staccato) => Self {
                agogic: 0.9,
                duration: -0.3,
                attack: 1.0,
                sustain: 0.2,
                brightness: 0.7,
            },
            // A written accent or marcato is already an emphasis instruction.
            // The analysis adds to it rather than replacing it, at a reduced
            // weight — the note is loud because it is marked, and stacking a
            // full analysed accent on top would double-count the same idea.
            Some(ArticulationId::Accent) | Some(ArticulationId::Marcato) => Self {
                agogic: 0.6,
                duration: 0.5,
                attack: 0.6,
                sustain: 0.6,
                brightness: 0.6,
            },
            // Held to its full length, so the emphasis goes into time and body
            // rather than into the start.
            Some(ArticulationId::Tenuto) => Self {
                agogic: 1.2,
                duration: 1.2,
                attack: 0.5,
                sustain: 1.0,
                brightness: 0.7,
            },
            // A slurred line has no separate bow start to sharpen, and taking
            // extra time inside one is the main way to lean on a note.
            Some(ArticulationId::Legato) => Self {
                agogic: 1.1,
                duration: 1.0,
                attack: 0.35,
                sustain: 1.0,
                brightness: 0.9,
            },
            // Sustain, and anything the instrument does not name: a bow start,
            // a swell, and colour, in equal measure. This is the reference row
            // every other one is scaled against.
            _ => Self {
                agogic: 1.0,
                duration: 1.0,
                attack: 1.0,
                sustain: 1.0,
                brightness: 1.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accent(prominence: f32) -> AccentState {
        AccentState::generated(prominence, prominence, prominence, prominence, 0.8)
    }

    #[test]
    fn a_neutral_accent_produces_no_gesture_at_all() {
        for articulation in [
            None,
            Some(ArticulationId::Pizzicato),
            Some(ArticulationId::Staccatissimo),
            Some(ArticulationId::Tremolo),
        ] {
            let gesture = AccentGesture::for_articulation(AccentState::neutral(), articulation);
            assert!(
                gesture.is_neutral(),
                "{articulation:?} moved a neutral accent: {gesture:?}"
            );
        }
    }

    /// Acceptance test 46: each component must reach its own dimension and no
    /// other. This is the test that catches a mapping where every field
    /// secretly controls gain.
    #[test]
    fn each_accent_component_moves_only_what_it_should() {
        let base = AccentState::neutral();
        let neutral = AccentGesture::for_articulation(base, None);

        let agogic_only = AccentGesture::for_articulation(
            AccentState {
                agogic: 1.0,
                ..base
            },
            None,
        );
        assert!(agogic_only.onset_shift_seconds > neutral.onset_shift_seconds);
        assert!(agogic_only.duration_scale > neutral.duration_scale);
        assert_eq!(
            agogic_only.attack_gain, neutral.attack_gain,
            "agogic reached the attack"
        );
        assert_eq!(
            agogic_only.brightness, neutral.brightness,
            "agogic reached brightness"
        );

        let attack_only = AccentGesture::for_articulation(
            AccentState {
                attack: 1.0,
                ..base
            },
            None,
        );
        assert!(attack_only.attack_gain > neutral.attack_gain);
        assert_eq!(
            attack_only.onset_shift_seconds, neutral.onset_shift_seconds,
            "attack reached the timing"
        );
        assert_eq!(
            attack_only.brightness, neutral.brightness,
            "attack reached brightness"
        );

        let timbre_only = AccentGesture::for_articulation(
            AccentState {
                timbre: 1.0,
                ..base
            },
            None,
        );
        assert!(timbre_only.brightness > neutral.brightness);
        assert_eq!(
            timbre_only.onset_shift_seconds, neutral.onset_shift_seconds,
            "timbre reached the timing"
        );
        assert_eq!(
            timbre_only.attack_gain, neutral.attack_gain,
            "timbre reached the attack"
        );
    }

    /// Acceptance test 44: one accent value, four articulations, four different
    /// physical answers.
    #[test]
    fn the_same_accent_is_realised_differently_per_articulation() {
        let strong = accent(0.95);
        let sustain = AccentGesture::for_articulation(strong, Some(ArticulationId::Sustain));
        let pizzicato = AccentGesture::for_articulation(strong, Some(ArticulationId::Pizzicato));
        let bounced = AccentGesture::for_articulation(strong, Some(ArticulationId::Staccatissimo));
        let tremolo = AccentGesture::for_articulation(strong, Some(ArticulationId::Tremolo));

        // A plucked note has no swell to give.
        assert_eq!(pizzicato.level_bump, 0.0);
        assert!(sustain.level_bump > 0.0);
        // ...and puts more into the pluck than a bow start gets.
        assert!(pizzicato.attack_gain > sustain.attack_gain);
        // A firmer bounced stroke is shorter, not longer.
        assert!(bounced.duration_scale < 1.0);
        assert!(sustain.duration_scale > 1.0);
        // A tremolo has no single attack to sharpen; its accent is intensity.
        assert!(tremolo.attack_gain < sustain.attack_gain);
        assert!(tremolo.level_bump > sustain.level_bump);
        // All four are genuinely different, not one value with a scale factor.
        let shapes = [sustain, pizzicato, bounced, tremolo];
        for (index, a) in shapes.iter().enumerate() {
            for b in shapes.iter().skip(index + 1) {
                assert_ne!(a, b);
            }
        }
    }

    /// Below neutral is a real instruction, not the absence of one.
    #[test]
    fn a_de_emphasised_note_is_placed_early_and_shortened() {
        let weak = AccentGesture::for_articulation(accent(0.05), Some(ArticulationId::Sustain));
        assert!(weak.onset_shift_seconds < 0.0);
        assert!(weak.duration_scale < 1.0);
        assert!(weak.attack_gain < 0.0);
        // A negative gap would mean overlapping the previous note, which is not
        // what de-emphasis means; it clamps.
        assert_eq!(weak.gap_before_seconds, 0.0);
    }

    /// Section 23: inside the ranges a violinist was actually observed in.
    #[test]
    fn no_gesture_leaves_the_measured_human_range() {
        for value in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            for articulation in ArticulationId::ALL {
                let gesture = AccentGesture::for_articulation(accent(value), Some(articulation));
                assert!(
                    gesture.onset_shift_seconds.abs() <= 0.060,
                    "{articulation:?} at {value} shifts the onset by {} s",
                    gesture.onset_shift_seconds
                );
                assert!(
                    (0.85..=1.15).contains(&gesture.duration_scale),
                    "{articulation:?} at {value} scales duration by {}",
                    gesture.duration_scale
                );
                assert!(gesture.gap_before_seconds <= 0.070);
                assert!(gesture.attack_gain.abs() <= 0.40);
                assert!(gesture.brightness.abs() <= 0.60);
            }
        }
    }
}
