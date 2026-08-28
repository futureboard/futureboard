//! What a loaded Solfege instrument can actually perform.
//!
//! # How the family is decided
//!
//! This module does **not** query the loaded model. [`family_for_instrument`]
//! lowercases the track's `SolfegeTrackState::instrument` *name string* and
//! looks for hardcoded keywords ("violin", "flute", "saw u", ...) inside it.
//! The `.sfm` on disk is never opened here and never asked what it can do, so a
//! track named "Violin Pad" is classified as a bowed string and a genuine
//! bowed-string model named "Model 3" is not.
//!
//! That is only tolerable because the lane table below no longer varies by
//! family: what the renderer honours is the same for every Solfege instrument,
//! so a misclassified name costs nothing but the articulation list. Treat the
//! family as an articulation-vocabulary hint, not as a capability query.
//!
//! A real query would need the loader to publish, per model, the controller
//! numbers that model's renderer acts on. Today that list is not data at all —
//! it is the `match` arms of
//! `solfege_acoustic::VoicebankRenderer::control_change` and
//! `solfege_engine::SamplerEngine::handle_compatibility_control`. Making it
//! answerable means exporting it from the engine, carrying it through
//! [`crate::solfege::SolfegeModelInfo`], and letting a track with no loaded
//! model advertise nothing. That is a cross-crate change, not a table edit.
//!
//! # Why the lane list is short
//!
//! Performance lanes are backed by the DAW's existing MIDI data:
//!
//! - `LaneSource::NoteVelocity` edits [`MidiNoteState::velocity`] directly.
//! - `LaneSource::Controller(kind)` edits a `MidiControllerLane` on the clip.
//!
//! That is deliberate. It means every Solfege lane already has persistence,
//! undo/redo, clip-relative timing, and engine delivery — no parallel store.
//! It also means a lane the engine ignores is not harmless: it writes real,
//! saved, undoable project data that never becomes sound.
//!
//! Controller points leave the DAW as `RuntimeMidiEventKind::ControlChange`
//! (`sphere_direct_audio_engine::runtime`), reach the track as
//! `Vst3MidiEvent::control_change`, and become
//! `solfege_event::Event::ControlChange` in
//! `RuntimeSolfegeEngine::handle_midi_event`. `SamplerEngine::handle_event`
//! then splits them two ways:
//!
//! - `handle_voicebank_event` forwards every control change to
//!   `VoicebankRenderer::control_change`, which acts on **CC 1, CC 11 and
//!   CC 64** and drops every other controller;
//! - `handle_compatibility_control` turns CC 1, 2, 11 and 74 into gestures on
//!   `SamplerEngine::voices`. Those voices only render while
//!   `SamplerEngine::instrument` is `Some`, and `prepare_sfm_staged` sets it to
//!   `None` for `SfmMode::VoicebankOnly` — the one mode the DAW ever loads an
//!   `.sfm` with (`sphere_direct_audio_engine::runtime::prepare_sfm_runtime`).
//!
//! ## Removed lanes
//!
//! Vibrato (CC 76), Bow Pressure (CC 20), Bow Velocity (CC 21), Bow Position
//! (CC 22), Finger Position (CC 23), Breath Pressure (CC 2), Embouchure
//! (CC 24), Tonguing (CC 25), Finger Slide (CC 26), Ornament (CC 27) and
//! Continuous Expression (CC 28) are gone. Traced individually:
//!
//! - CC 20-28 and CC 76 match no arm of either function above. They are
//!   discarded in every engine configuration the DAW can reach.
//! - CC 2 does reach `GestureControl::BreathPressure`, and only on the physical
//!   fallback engine. `PhysicalModel` has exactly one variant, `BowedString`,
//!   and `solfege_dsp::BowedString::process` never reads that gesture. It is
//!   stored in `GestureState::instrument[4]` and left there.
//!
//! They are deleted rather than disabled because a lane that cannot be drawn
//! into is clearer than one that draws a curve and changes nothing.
//!
//! ## Real controllers still not offered
//!
//! - **CC 64** (sustain) is honoured by `VoicebankRenderer::set_sustain`, but
//!   it is a pedal, not a curve. It needs a switch lane this editor does not
//!   have; drawing it on a 0..127 ramp would misrepresent it.
//! - **CC 74** moves `GestureControl::BowPosition`, which
//!   `solfege_dsp::BowedString::process` does read — but only on the fallback
//!   engine, the one you get when the model failed to load. A control that
//!   works only while the instrument is missing is not a capability.
//!
//! ## Existing projects
//!
//! A project saved with one of the removed lanes open keeps its
//! `SolfegeLaneVisibility` row. `SolfegeLaneStack::visible_lanes` resolves lane
//! ids against this table and skips the ones that no longer resolve, and the
//! unresolved row is still written back on save, so nothing is silently
//! rewritten.

use crate::components::timeline::timeline_state::{ArticulationId, MidiControllerKind};

/// Where a performance lane's data lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneSource {
    /// The per-note velocity field. Drawn as one bar per note, not a curve.
    NoteVelocity,
    /// The per-note [`AccentState`](crate::components::timeline::timeline_state::AccentState).
    /// Also one bar per note: accent is an event-level reading of a note, not a
    /// signal that exists between notes, and drawing it as a curve would invite
    /// the user to shape something that has nowhere to go.
    NoteAccent,
    /// A continuous controller lane on the MIDI clip.
    Controller(MidiControllerKind),
}

/// Menu grouping in the `+ Lane` picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneGroup {
    /// Available on every Solfege instrument.
    Performance,
    /// Reserved for lanes a specific model declares for itself.
    ///
    /// Nothing produces this today: the renderer's controller list does not
    /// vary by model, so no per-instrument lane can be honestly advertised
    /// until the engine exports one (see the module docs). The variant stays
    /// because `solfege_editor::lanes` iterates both groups and already skips
    /// an empty one, so this is the seam a real capability query would fill.
    Instrument,
}

impl LaneGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Performance => "Performance",
            Self::Instrument => "Instrument",
        }
    }
}

/// How a lane labels its value axis in the left gutter.
///
/// The gutter carries the *scale*, not the lane name — the name is an overlay
/// inside the lane body. That keeps the gutter the same narrow width as the
/// pitch keyboard above it (so every surface stays column-aligned) while still
/// telling the user what the vertical axis means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaneScale {
    /// `127 / 64 / 0` — MIDI-valued lanes.
    #[default]
    Midi,
    /// `ff / mp / ppp` — loudness-shaped lanes.
    Dynamics,
    /// `max / mid / min` — normalized values with no MIDI meaning the performer
    /// thinks in.
    ///
    /// Used by the Accent lane, whose 0..1 reading is a musical judgement
    /// rather than a controller value: labelling its axis `127 / 64 / 0` would
    /// tell the user it is a CC, and `ff / mp / ppp` would tell them it is
    /// loudness. It is neither.
    Amount,
}

impl LaneScale {
    /// Top, middle, and bottom labels for the lane gutter.
    pub fn marks(self) -> [&'static str; 3] {
        match self {
            Self::Midi => ["127", "64", "0"],
            Self::Dynamics => ["ff", "mp", "ppp"],
            Self::Amount => ["max", "mid", "min"],
        }
    }
}

/// One performance lane the editor may show under the piano roll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneSpec {
    /// Stable identifier, persisted in the project's visible-lane list. Never
    /// reuse an id for a different meaning.
    pub id: &'static str,
    pub label: &'static str,
    pub group: LaneGroup,
    pub source: LaneSource,
    pub scale: LaneScale,
}

impl LaneSpec {
    pub const fn new(
        id: &'static str,
        label: &'static str,
        group: LaneGroup,
        source: LaneSource,
        scale: LaneScale,
    ) -> Self {
        Self {
            id,
            label,
            group,
            source,
            scale,
        }
    }

    /// The MIDI controller this lane writes, if it writes one.
    pub fn controller(&self) -> Option<MidiControllerKind> {
        match self.source {
            LaneSource::Controller(kind) => Some(kind),
            LaneSource::NoteVelocity | LaneSource::NoteAccent => None,
        }
    }
}

/// Instrument families Solfege models are grouped into.
///
/// The family selects the articulation vocabulary only. It used to select
/// gesture lanes as well; those lanes had no consumer (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstrumentFamily {
    BowedString,
    Wind,
    ThaiBowed,
    #[default]
    Generic,
}

impl InstrumentFamily {
    pub fn label(self) -> &'static str {
        match self {
            Self::BowedString => "Bowed String",
            Self::Wind => "Wind",
            Self::ThaiBowed => "Thai Bowed",
            Self::Generic => "Generic",
        }
    }
}

// ── Lane vocabulary ──────────────────────────────────────────────────────
// CC assignments are the instrument's contract with the engine. They are
// constants here (not user-editable strings) so a saved project's lane ids and
// the controller data underneath stay in agreement.
//
// Every entry below names the engine function that reads it. A lane with no
// such function does not belong in this file.

/// **Velocity** — per-note, and the only performance value the voicebank reads
/// at the attack.
///
/// Consumed by `solfege_acoustic::VoicebankRenderer::note_on`, which hands it
/// to `solfege_model::voicebank::VoicebankModel::resolve` (dynamic-layer and
/// round-robin selection) and then to `VoicebankRenderer::start_voice` (the
/// voice's opening level). On the physical fallback engine it reaches
/// `solfege_dsp::BowedString::process` as `gesture.velocity`.
const LANE_VELOCITY: LaneSpec = LaneSpec::new(
    "velocity",
    "Velocity",
    LaneGroup::Performance,
    LaneSource::NoteVelocity,
    LaneScale::Midi,
);

/// **Dynamics** — CC 1, the per-voice dynamic level.
///
/// Consumed by `solfege_acoustic::VoicebankRenderer::control_change`, whose
/// `1 =>` arm calls `VoicebankRenderer::set_dynamic` and moves the level the
/// sounding voice glides toward. That is the same value
/// `solfege_engine::SamplerEngine::handle_voicebank_event` applies for
/// `Event::Expression`, so the lane's label and its effect agree.
///
/// On the physical fallback engine the same CC instead reaches
/// `SamplerEngine::handle_compatibility_control`, which maps it to
/// `GestureControl::VibratoDepth`. That path only runs when the model failed to
/// load; the lane is named for the shipping path.
const LANE_DYNAMICS: LaneSpec = LaneSpec::new(
    "dynamics",
    "Dynamics",
    LaneGroup::Performance,
    LaneSource::Controller(MidiControllerKind::CC(1)),
    LaneScale::Dynamics,
);

/// **Expression** — CC 11, a level trim over the whole instrument.
///
/// Consumed by `solfege_acoustic::VoicebankRenderer::control_change`, whose
/// `11 =>` arm calls `VoicebankRenderer::set_expression`; `render_frame`
/// multiplies the summed voices by the glided value. On the physical fallback
/// engine `SamplerEngine::handle_compatibility_control` maps it to
/// `GestureControl::Expression`, which `solfege_dsp::BowedString::process`
/// applies to the radiated signal — the same meaning either way.
const LANE_EXPRESSION: LaneSpec = LaneSpec::new(
    "expression",
    "Expression",
    LaneGroup::Performance,
    LaneSource::Controller(MidiControllerKind::CC(11)),
    LaneScale::Dynamics,
);

/// **Accent** — per-note musical prominence, from Analyze Accent or by hand.
///
/// The only lane here whose consumer is not a controller number. Accent does
/// not reach the engine as a CC and is not a second dynamics lane: it is
/// *analysis*, read by the Studio Performer when a performance is generated and
/// realised there as attack, timing, level and brightness together, according
/// to the note's articulation. A project whose owner never generates a
/// performance still keeps its accents, and they still mean something the next
/// time one is generated.
///
/// It is in the `+ Lane` menu rather than shown by default because it is empty
/// until analysed, and a lane that opens empty on every clip reads as a broken
/// surface rather than an unused one.
const LANE_ACCENT: LaneSpec = LaneSpec::new(
    "accent",
    "Accent",
    LaneGroup::Performance,
    LaneSource::NoteAccent,
    LaneScale::Amount,
);

/// The lanes every Solfege instrument exposes — and, at present, the only
/// lanes any of them exposes.
const PERFORMANCE_LANES: [LaneSpec; 4] =
    [LANE_VELOCITY, LANE_DYNAMICS, LANE_EXPRESSION, LANE_ACCENT];

/// Everything the editor needs to know about the loaded instrument.
#[derive(Debug, Clone, PartialEq)]
pub struct InstrumentCapabilities {
    pub family: InstrumentFamily,
    /// Lanes offered in the `+ Lane` menu, in menu order.
    pub lanes: Vec<LaneSpec>,
    /// Articulations this instrument can play, in display order.
    pub articulations: Vec<ArticulationId>,
    /// Whether the Pitch tab's continuous editing applies. Every current
    /// family supports it; a future sample-playback-only model may not.
    pub continuous_pitch: bool,
    /// Lanes shown the first time a Solfege clip is opened, by lane id.
    pub default_visible_lanes: Vec<&'static str>,
}

impl InstrumentCapabilities {
    pub fn lane(&self, id: &str) -> Option<&LaneSpec> {
        self.lanes.iter().find(|lane| lane.id == id)
    }

    pub fn lanes_in_group(&self, group: LaneGroup) -> impl Iterator<Item = &LaneSpec> {
        self.lanes.iter().filter(move |lane| lane.group == group)
    }
}

/// Classify an instrument name into a family.
///
/// Matching is case-insensitive and substring-based over the track's instrument
/// *name*, so preset names like "VSCO Solo Violin" resolve without an
/// exact-name registry — and so does any unrelated name that happens to contain
/// one of these words. Nothing here inspects the loaded model; see the module
/// docs for what a real capability query would require.
pub fn family_for_instrument(instrument: &str) -> InstrumentFamily {
    let name = instrument.to_ascii_lowercase();
    const THAI: [&str; 6] = ["saw u", "saw duang", "sor", "salo", "thai", "ซอ"];
    const BOWED: [&str; 6] = ["violin", "viola", "cello", "contrabass", "erhu", "fiddle"];
    const WIND: [&str; 8] = [
        "flute", "clarinet", "oboe", "bassoon", "sax", "trumpet", "horn", "pi ",
    ];
    if THAI.iter().any(|k| name.contains(k)) {
        return InstrumentFamily::ThaiBowed;
    }
    if BOWED.iter().any(|k| name.contains(k)) {
        return InstrumentFamily::BowedString;
    }
    if WIND.iter().any(|k| name.contains(k)) {
        return InstrumentFamily::Wind;
    }
    InstrumentFamily::Generic
}

/// Capabilities for a family. The editor calls
/// [`super::SolfegeTrackState::capabilities`] rather than this directly.
pub fn capabilities_for_family(family: InstrumentFamily) -> InstrumentCapabilities {
    // The lane set is family-independent on purpose: the controllers the
    // renderer honours do not vary by instrument, so neither may this. That
    // leaves `LaneGroup::Instrument` empty for every family, which
    // `solfege_editor::lanes` already handles by skipping the group heading.
    let lanes = PERFORMANCE_LANES.to_vec();

    let articulations = match family {
        // Pizzicato and tremolo are bowed-string techniques and are recorded
        // as their own articulations in a bank like Solo Violin. Leaving them
        // out of this list is what made those recordings unreachable from the
        // arrangement: the assign row only ever offers what the family claims.
        InstrumentFamily::BowedString | InstrumentFamily::ThaiBowed => vec![
            ArticulationId::Sustain,
            ArticulationId::Legato,
            ArticulationId::Staccato,
            ArticulationId::Tenuto,
            ArticulationId::Accent,
            ArticulationId::Marcato,
            ArticulationId::Pizzicato,
            ArticulationId::Tremolo,
        ],
        InstrumentFamily::Wind => vec![
            ArticulationId::Sustain,
            ArticulationId::Legato,
            ArticulationId::Staccato,
            ArticulationId::Tenuto,
            ArticulationId::Accent,
        ],
        InstrumentFamily::Generic => ArticulationId::ALL.to_vec(),
    };

    let default_visible_lanes = match family {
        InstrumentFamily::Generic => vec!["velocity"],
        _ => vec!["velocity", "dynamics"],
    };

    InstrumentCapabilities {
        family,
        lanes,
        articulations,
        continuous_pitch: true,
        default_visible_lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILIES: [InstrumentFamily; 4] = [
        InstrumentFamily::BowedString,
        InstrumentFamily::Wind,
        InstrumentFamily::ThaiBowed,
        InstrumentFamily::Generic,
    ];

    /// Controllers with a named consumer that a *continuous* lane may drive.
    ///
    /// Both arms live in `solfege_acoustic::VoicebankRenderer::control_change`:
    /// CC 1 calls `set_dynamic`, CC 11 calls `set_expression`. CC 64 is honoured
    /// there too (`set_sustain`) but is a pedal, so no curve lane claims it.
    const CONSUMED_CONTROLLERS: [u8; 2] = [1, 11];

    /// Controllers this table used to advertise and no renderer reads. Listed
    /// explicitly so re-adding one has to argue with the engine again.
    const DISCARDED_CONTROLLERS: [u8; 11] = [2, 20, 21, 22, 23, 24, 25, 26, 27, 28, 76];

    fn lane_controllers(caps: &InstrumentCapabilities) -> Vec<MidiControllerKind> {
        caps.lanes.iter().filter_map(LaneSpec::controller).collect()
    }

    #[test]
    fn every_advertised_controller_has_a_named_consumer() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            for kind in lane_controllers(&caps) {
                let MidiControllerKind::CC(cc) = kind else {
                    panic!("{family:?} advertises {kind:?}, which reaches no Solfege renderer");
                };
                assert!(
                    CONSUMED_CONTROLLERS.contains(&cc),
                    "{family:?} advertises CC {cc}, which no engine function reads"
                );
            }
        }
    }

    #[test]
    fn discarded_controllers_are_not_offered() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            let offered = lane_controllers(&caps);
            for cc in DISCARDED_CONTROLLERS {
                assert!(
                    !offered.contains(&MidiControllerKind::CC(cc)),
                    "{family:?} offers CC {cc}, which the engine discards"
                );
            }
        }
    }

    #[test]
    fn no_family_advertises_an_instrument_specific_lane() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            assert_eq!(
                caps.lanes_in_group(LaneGroup::Instrument).count(),
                0,
                "{family:?} advertises a per-instrument lane with no model behind it"
            );
        }
    }

    #[test]
    fn every_instrument_gets_the_same_performance_lanes() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            for id in ["velocity", "dynamics", "expression", "accent"] {
                assert!(caps.lane(id).is_some(), "{family:?} missing {id}");
            }
            assert_eq!(caps.lanes.len(), 4, "{family:?} lane count drifted");
        }
    }

    #[test]
    fn dynamics_is_cc1_and_expression_is_cc11() {
        // Named separately from the allow-list test: the two are not
        // interchangeable, and swapping them would still satisfy that one.
        let caps = capabilities_for_family(InstrumentFamily::BowedString);
        assert_eq!(
            caps.lane("dynamics").and_then(LaneSpec::controller),
            Some(MidiControllerKind::CC(1))
        );
        assert_eq!(
            caps.lane("expression").and_then(LaneSpec::controller),
            Some(MidiControllerKind::CC(11))
        );
        assert_eq!(
            caps.lane("velocity").map(|lane| lane.source),
            Some(LaneSource::NoteVelocity)
        );
    }

    /// Accent is note data, not a controller. If it ever gained a CC number the
    /// "every advertised controller has a named consumer" test would start
    /// demanding an engine function for it — which is the right failure, since
    /// nothing in the engine reads an accent CC.
    #[test]
    fn accent_writes_note_data_rather_than_a_controller() {
        let caps = capabilities_for_family(InstrumentFamily::BowedString);
        assert_eq!(
            caps.lane("accent").map(|lane| lane.source),
            Some(LaneSource::NoteAccent)
        );
        assert_eq!(caps.lane("accent").and_then(LaneSpec::controller), None);
    }

    /// Accent opens empty on a clip that has never been analysed, so it is
    /// opt-in from `+ Lane` rather than part of the default stack.
    #[test]
    fn accent_is_not_shown_by_default() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            assert!(!caps.default_visible_lanes.contains(&"accent"));
        }
    }

    #[test]
    fn default_visible_lanes_all_resolve() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            for id in &caps.default_visible_lanes {
                assert!(
                    caps.lane(id).is_some(),
                    "{family:?} defaults to {id}, which is not in its table"
                );
            }
        }
    }

    #[test]
    fn lane_ids_are_unique_per_instrument() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            let mut ids: Vec<&str> = caps.lanes.iter().map(|lane| lane.id).collect();
            ids.sort_unstable();
            let count = ids.len();
            ids.dedup();
            assert_eq!(ids.len(), count, "{family:?} has duplicate lane ids");
        }
    }

    #[test]
    fn controller_lanes_do_not_collide() {
        for family in FAMILIES {
            let caps = capabilities_for_family(family);
            let mut kinds: Vec<String> = lane_controllers(&caps)
                .into_iter()
                .map(|kind| format!("{kind:?}"))
                .collect();
            kinds.sort();
            let count = kinds.len();
            kinds.dedup();
            assert_eq!(kinds.len(), count, "{family:?} maps two lanes to one CC");
        }
    }

    #[test]
    fn family_comes_from_the_instrument_name_only() {
        // Documents what `family_for_instrument` really does, so the next
        // reader does not mistake it for a model query.
        assert_eq!(
            family_for_instrument("VSCO Solo Violin"),
            InstrumentFamily::BowedString
        );
        assert_eq!(family_for_instrument("Saw U"), InstrumentFamily::ThaiBowed);
        assert_eq!(family_for_instrument("Solo Flute"), InstrumentFamily::Wind);
        // A name with no keyword falls through, however real the model is.
        assert_eq!(family_for_instrument("Model 3"), InstrumentFamily::Generic);
        // ...and a keyword in an unrelated name still matches.
        assert_eq!(
            family_for_instrument("Violin Pad Synth"),
            InstrumentFamily::BowedString
        );
    }

    #[test]
    fn family_only_changes_the_articulation_vocabulary() {
        let bowed = capabilities_for_family(InstrumentFamily::BowedString);
        let wind = capabilities_for_family(InstrumentFamily::Wind);
        assert_eq!(bowed.lanes, wind.lanes);
        assert_ne!(bowed.articulations, wind.articulations);
    }
}
