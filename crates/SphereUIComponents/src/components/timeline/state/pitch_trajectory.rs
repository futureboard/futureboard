//! The evaluated continuous pitch trajectory of a MIDI clip.
//!
//! [`PitchCurve`] stores what the *user drew*. This module answers the
//! different question the Pitch editor and the engine both ask: **what pitch is
//! actually sounding at this instant?**
//!
//! ```text
//! stored points
//!       ↓  interpolation
//! note baseline
//!       + note-to-note transition (legato / abutting)
//!       + manual pitch offset
//!       + vibrato (stored as curve points today)
//!       + tuning offset            (no source yet — contributes 0)
//!       + generated performance    (no source yet — contributes 0)
//!       ↓
//! final pitch trajectory
//! ```
//!
//! Two properties matter:
//!
//! 1. **A note with no manual edits still has a trajectory.** An empty
//!    [`PitchCurve`] means "sounds at the notated pitch", not "draws nothing",
//!    so every note contributes a visible line.
//! 2. **Connected notes share one continuous line.** Where an articulation
//!    connects two notes (or they simply abut), the trajectory eases from one
//!    pitch into the next instead of breaking. Detached notes stay separate.
//!
//! Polyphony is handled by partitioning notes into non-overlapping *voices*;
//! each voice is one continuous line. A monophonic instrument yields exactly
//! one.
//!
//! Everything here is `f32` pitch in fractional MIDI note numbers — the model
//! never quantizes to semitones, so non-12-TET tunings and continuous
//! instruments (bowed strings, Thai bowed instruments) are representable.
//!
//! Cost: [`PitchTrajectory::build`] is `O(n log n)` in the note count and
//! [`PitchTrajectory::sample_columns`] is a single merged walk, `O(columns +
//! log spans)` — it binary-searches its starting span rather than walking to
//! it, so sampling a clip note by note stays linear in the note count instead
//! of quadratic. At realistic clip sizes that is far below a frame budget, so
//! there is deliberately no cache to invalidate.

use super::*;

/// Longest silent gap a connecting articulation still bridges, in beats.
/// Beyond this the notes are separate phrases even under legato.
pub const LEGATO_BRIDGE_BEATS: f32 = 0.5;

/// Nominal half-width of a note-to-note pitch transition, in beats. The real
/// width is additionally clamped to a fraction of each note so a transition
/// can never swallow a short note.
pub const TRANSITION_HALF_BEATS: f32 = 1.0 / 16.0;

/// Largest fraction of a note's duration one transition may consume at each
/// end. Two transitions therefore leave at least 20% of the note flat.
const TRANSITION_MAX_NOTE_FRACTION: f32 = 0.4;

/// Two notes closer than this are treated as abutting (i.e. connected).
pub const ABUT_EPS_BEATS: f32 = 1.0 / 256.0;

/// Convert a fractional MIDI note number to Hz (A4 = MIDI 69 = 440 Hz).
///
/// Provided because a physical model drives a resonator in Hz, not in note
/// numbers; the trajectory is continuous precisely so this conversion is
/// meaningful at any point, not only at note boundaries.
#[inline]
pub fn midi_pitch_to_hz(pitch: f32) -> f32 {
    440.0 * ((pitch - 69.0) / 12.0).exp2()
}

/// Inverse of [`midi_pitch_to_hz`].
#[inline]
pub fn hz_to_midi_pitch(hz: f32) -> f32 {
    69.0 + 12.0 * (hz.max(f32::MIN_POSITIVE) / 440.0).log2()
}

/// One stretch of a voice's timeline: either a note sounding on its own, or the
/// transition that carries one note's pitch into the next. Spans are disjoint
/// and ordered, which is what makes sampling a single forward walk.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Span {
    /// `index` is a position in the owning voice's `notes` list.
    Note { index: usize, start: f32, end: f32 },
    /// A transition from the note at `index` to the one at `index + 1`.
    Transition {
        start: f32,
        end: f32,
        from_pitch: f32,
        to_pitch: f32,
    },
}

impl Span {
    #[inline]
    fn start(self) -> f32 {
        match self {
            Span::Note { start, .. } | Span::Transition { start, .. } => start,
        }
    }

    #[inline]
    fn end(self) -> f32 {
        match self {
            Span::Note { end, .. } | Span::Transition { end, .. } => end,
        }
    }
}

/// One monophonic line through the clip.
#[derive(Debug, Clone, Default)]
pub struct PitchVoice {
    /// Indices into the source note slice, ordered by start beat and
    /// guaranteed not to overlap.
    pub notes: Vec<usize>,
    /// Note bodies and transitions, disjoint and ordered by start beat.
    spans: Vec<Span>,
}

impl PitchVoice {
    /// Beat range this voice covers, including its transitions.
    pub fn beat_range(&self) -> Option<(f32, f32)> {
        Some((self.spans.first()?.start(), self.spans.last()?.end()))
    }
}

/// The evaluated trajectory of one clip.
#[derive(Debug, Clone, Default)]
pub struct PitchTrajectory {
    voices: Vec<PitchVoice>,
}

impl PitchTrajectory {
    pub fn voices(&self) -> &[PitchVoice] {
        &self.voices
    }

    pub fn is_empty(&self) -> bool {
        self.voices.is_empty()
    }

    /// Build the trajectory for a clip.
    ///
    /// `direction_events` is the clip's articulation lane, so a note with no
    /// explicit articulation still chases the clip direction when deciding
    /// whether it connects to its neighbour — the same resolution playback uses
    /// ([`resolve_note_articulation`]).
    pub fn build(notes: &[MidiNoteState], direction_events: &[MidiArticulationEvent]) -> Self {
        let voices = assign_voices(notes)
            .into_iter()
            .map(|indices| build_voice(notes, direction_events, indices))
            .collect();
        Self { voices }
    }

    /// Whether this note's sounding pitch ever departs from its notated pitch.
    ///
    /// A note sounds exactly as written unless it carries drawn points or a
    /// transition reaches into it from a neighbour. Callers that only need the
    /// trajectory when it says something new — the engine snapshot emits
    /// nothing for a plain note — can ask this first instead of sampling the
    /// note at audio-control resolution and discovering afterwards that every
    /// sample equalled the note number they already had.
    pub fn note_departs_from_notated_pitch(
        &self,
        notes: &[MidiNoteState],
        voice: usize,
        note_index: usize,
    ) -> bool {
        if notes
            .get(note_index)
            .and_then(|note| note.pitch_curve.as_ref())
            .is_some_and(|curve| !curve.is_empty())
        {
            return true;
        }
        let Some(voice) = self.voices.get(voice) else {
            return false;
        };
        let Some(note) = notes.get(note_index) else {
            return false;
        };
        let (start, end) = (note.start, note.start + note.duration);
        // Spans are disjoint and ordered, so only the ones overlapping this
        // note can matter. Scanning the whole voice instead would make this
        // O(spans) per note, and a caller asking it once per note — which is
        // the whole point of it existing — quadratic in the note count: the
        // exact cost it was added to avoid.
        let first = match voice
            .spans
            .binary_search_by(|span| span.start().total_cmp(&start))
        {
            Ok(hit) => hit,
            Err(0) => 0,
            Err(insert) => insert - 1,
        };
        voice.spans[first..]
            .iter()
            .take_while(|span| span.start() < end)
            .any(|span| match span {
                Span::Transition {
                    start: span_start,
                    end: span_end,
                    ..
                } => *span_start < end && *span_end > start,
                Span::Note { .. } => false,
            })
    }

    /// Sounding pitch of `voice` at clip-local `beat`, in fractional MIDI note
    /// numbers. `None` means the voice is silent there.
    ///
    /// This is the engine-facing entry point: it is resolution-independent, so
    /// a renderer can ask per pixel column and a physical model can ask per
    /// audio block (or per sample) without a different code path.
    pub fn sample(&self, notes: &[MidiNoteState], voice: usize, beat: f32) -> Option<f32> {
        let voice = self.voices.get(voice)?;
        let index = match voice
            .spans
            .binary_search_by(|span| span.start().total_cmp(&beat))
        {
            Ok(hit) => hit,
            Err(0) => return None,
            Err(insert) => insert - 1,
        };
        let span = voice.spans[index];
        if beat > span.end() {
            return None;
        }
        Some(evaluate_span(notes, voice, span, beat))
    }

    /// Sample `columns` evenly spaced points starting at `from_beat`, stepping
    /// `beats_per_column`. `out` is cleared and filled with one entry per
    /// column; `None` marks a pen-up column where the voice is silent.
    ///
    /// A single forward walk over the (ordered, disjoint) spans, so cost is
    /// `O(columns + spans)` rather than `O(columns × notes)`.
    pub fn sample_columns(
        &self,
        notes: &[MidiNoteState],
        voice: usize,
        from_beat: f32,
        beats_per_column: f32,
        columns: usize,
        out: &mut Vec<Option<f32>>,
    ) {
        out.clear();
        out.reserve(columns);
        let Some(voice) = self.voices.get(voice) else {
            out.resize(columns, None);
            return;
        };
        // Start where `from_beat` actually lands instead of at the head of the
        // voice. The walk below is forward-only, so beginning at zero made a
        // caller that samples note by note — which is exactly what the engine
        // snapshot does — rescan every span it had already walked, turning the
        // emission of one clip's pitch into quadratic work in its note count.
        let mut cursor = match voice
            .spans
            .binary_search_by(|span| span.start().total_cmp(&from_beat))
        {
            Ok(hit) => hit,
            Err(0) => 0,
            Err(insert) => insert - 1,
        };
        for column in 0..columns {
            let beat = from_beat + beats_per_column * column as f32;
            // Advance on the *next* span's start, matching the half-open
            // convention `sample`'s binary search uses. Advancing on this
            // span's end instead would resolve a shared boundary beat to the
            // earlier span here and the later one there, so the two entry
            // points would disagree by a whole segment at every abutment.
            while cursor + 1 < voice.spans.len() && voice.spans[cursor + 1].start() <= beat {
                cursor += 1;
            }
            match voice.spans.get(cursor) {
                Some(&span) if beat >= span.start() => {
                    out.push(Some(evaluate_span(notes, voice, span, beat)));
                }
                _ => out.push(None),
            }
        }
    }
}

/// Pitch of one note at an absolute clip-local beat, summing every pitch layer.
///
/// The layers are enumerated explicitly so a future source is a term added
/// here — not a rewrite of the evaluator or the editor that renders it. Layers
/// with no producer yet contribute exactly zero and are named in the comments
/// rather than stored as dead fields.
#[inline]
pub fn note_pitch_at(note: &MidiNoteState, beat: f32) -> f32 {
    let beat_in_note = beat - note.start;
    // Layer 1 — the notated pitch.
    let base = note.pitch as f32;
    // Layer 2 — manual expression: drawn pitch, vibrato, scoops, falls. Stored
    // as cent deviations on the note, so it survives transposition.
    let manual_cents = note
        .pitch_curve
        .as_ref()
        .map(|curve| curve.cents_at(beat_in_note))
        .unwrap_or(0.0);
    // Layer 3 — FBMX-generated performance baseline. No producer yet; when one
    // exists it adds a second curve read here and Reset returns to it.
    let generated_cents = 0.0;
    // Layer 4 — tuning offset (scala / per-degree temperament). No producer yet.
    let tuning_cents = 0.0;
    base + (manual_cents + generated_cents + tuning_cents) / 100.0
}

fn evaluate_span(notes: &[MidiNoteState], voice: &PitchVoice, span: Span, beat: f32) -> f32 {
    match span {
        Span::Note { index, .. } => {
            let note = &notes[voice.notes[index]];
            note_pitch_at(note, beat)
        }
        Span::Transition {
            start,
            end,
            from_pitch,
            to_pitch,
        } => {
            let width = end - start;
            if width <= f32::EPSILON {
                return to_pitch;
            }
            // Cosine ease: flat where it leaves the first note and where it
            // arrives at the second, which is what a bowed or sung transition
            // looks like. A linear ramp reads as a synthetic pitch-bend.
            let t = ((beat - start) / width).clamp(0.0, 1.0);
            let eased = 0.5 - 0.5 * (t * std::f32::consts::PI).cos();
            from_pitch + (to_pitch - from_pitch) * eased
        }
    }
}

/// Greedy interval partition: each note joins the first voice whose last note
/// has already ended. Monophonic input yields one voice; overlapping notes fan out
/// into as many voices as the maximum simultaneity.
fn assign_voices(notes: &[MidiNoteState]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|&a, &b| {
        notes[a]
            .start
            .total_cmp(&notes[b].start)
            .then_with(|| notes[a].pitch.cmp(&notes[b].pitch))
    });

    let mut voices: Vec<Vec<usize>> = Vec::new();
    let mut voice_end: Vec<f32> = Vec::new();
    for index in order {
        let note = &notes[index];
        let start = note.start;
        let end = note.start + note.duration;
        match voice_end
            .iter()
            .position(|&last_end| last_end <= start + ABUT_EPS_BEATS)
        {
            Some(voice) => {
                voices[voice].push(index);
                voice_end[voice] = end;
            }
            None => {
                voices.push(vec![index]);
                voice_end.push(end);
            }
        }
    }
    voices
}

/// Whether the trajectory carries continuously from `a` into `b`.
///
/// Decided from the articulation registry rather than by matching specific
/// articulation ids, so adding an articulation cannot silently bypass this.
fn connects(
    a: &MidiNoteState,
    b: &MidiNoteState,
    direction_events: &[MidiArticulationEvent],
) -> bool {
    let playback = resolve_note_articulation(a, direction_events)
        .map(|articulation| articulation.definition().playback);
    // A detached articulation releases before the next note, so its pitch line
    // ends with it. "Detached" is measured against the transition width rather
    // than against a fixed `gate_ratio` cutoff: Sustain gates at 0.98 and
    // Marcato at 0.85, and treating those as detached would strip the glide
    // from a phrase the moment the user marked it Sustain — the one
    // articulation whose whole purpose is to hold.
    if let Some(playback) = playback {
        let released_early = a.duration * (1.0 - playback.gate_ratio.clamp(0.0, 1.0));
        if released_early > TRANSITION_HALF_BEATS {
            return false;
        }
    }
    let gap = b.start - (a.start + a.duration);
    if gap <= ABUT_EPS_BEATS {
        return true;
    }
    playback.is_some_and(|p| p.legato_overlap_beats > 0.0) && gap <= LEGATO_BRIDGE_BEATS
}

fn build_voice(
    notes: &[MidiNoteState],
    direction_events: &[MidiArticulationEvent],
    indices: Vec<usize>,
) -> PitchVoice {
    let count = indices.len();
    // `trim_in[i]` / `trim_out[i]` are how much of note `i` the neighbouring
    // transitions consume, so note spans and transition spans stay disjoint.
    let mut trim_in = vec![0.0f32; count];
    let mut trim_out = vec![0.0f32; count];
    let mut linked = vec![false; count.saturating_sub(1).max(0)];

    for i in 0..count.saturating_sub(1) {
        let a = &notes[indices[i]];
        let b = &notes[indices[i + 1]];
        if !connects(a, b, direction_events) {
            continue;
        }
        linked[i] = true;
        trim_out[i] = TRANSITION_HALF_BEATS.min(a.duration * TRANSITION_MAX_NOTE_FRACTION);
        trim_in[i + 1] = TRANSITION_HALF_BEATS.min(b.duration * TRANSITION_MAX_NOTE_FRACTION);
    }

    let mut spans: Vec<Span> = Vec::with_capacity(count * 2);
    for i in 0..count {
        let note = &notes[indices[i]];
        let note_start = note.start + trim_in[i];
        let note_end = note.start + note.duration - trim_out[i];
        spans.push(Span::Note {
            index: i,
            start: note_start,
            end: note_end.max(note_start),
        });
        if i + 1 < count && linked[i] {
            let a = &notes[indices[i]];
            let b = &notes[indices[i + 1]];
            let start = a.start + a.duration - trim_out[i];
            let end = b.start + trim_in[i + 1];
            spans.push(Span::Transition {
                start,
                end: end.max(start),
                from_pitch: note_pitch_at(a, start),
                to_pitch: note_pitch_at(b, end),
            });
        }
    }

    PitchVoice {
        notes: indices,
        spans,
    }
}

impl TimelineState {
    /// Evaluated pitch trajectory for a MIDI clip, or an empty trajectory when
    /// the clip is missing or not MIDI.
    pub fn clip_pitch_trajectory(&self, clip_id: &str) -> PitchTrajectory {
        let Some(notes) = self.midi_clip_notes(clip_id) else {
            return PitchTrajectory::default();
        };
        let directions = self
            .midi_clip_articulations(clip_id)
            .map(|events| events.as_slice())
            .unwrap_or(&[]);
        PitchTrajectory::build(notes, directions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, start: f32, duration: f32) -> MidiNoteState {
        MidiNoteState::new(pitch, start, duration, 100)
    }

    fn legato(pitch: u8, start: f32, duration: f32) -> MidiNoteState {
        let mut n = note(pitch, start, duration);
        n.articulation = Some(ArticulationId::Legato);
        n
    }

    #[test]
    fn an_untouched_note_still_has_a_visible_trajectory() {
        let notes = vec![note(60, 0.0, 2.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        assert_eq!(t.voices().len(), 1);
        // Flat at the notated pitch across the whole note — never "nothing".
        for beat in [0.0f32, 0.5, 1.0, 1.99] {
            assert_eq!(t.sample(&notes, 0, beat), Some(60.0));
        }
    }

    #[test]
    fn silence_outside_the_note_is_pen_up() {
        let notes = vec![note(60, 1.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        assert_eq!(t.sample(&notes, 0, 0.5), None);
        assert_eq!(t.sample(&notes, 0, 2.5), None);
    }

    #[test]
    fn abutting_notes_share_one_continuous_line() {
        let notes = vec![note(60, 0.0, 1.0), note(62, 1.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        assert_eq!(t.voices().len(), 1, "sequential notes are one voice");
        // The boundary is bridged, and the value there sits between the pitches.
        let at_boundary = t.sample(&notes, 0, 1.0).expect("bridged");
        assert!(
            at_boundary > 60.0 && at_boundary < 62.0,
            "expected a transition through the boundary, got {at_boundary}"
        );
        // No pen-up anywhere across the pair.
        let mut out = Vec::new();
        t.sample_columns(&notes, 0, 0.0, 0.02, 100, &mut out);
        assert!(out.iter().all(|v| v.is_some()));
    }

    #[test]
    fn a_legato_note_bridges_a_short_rest() {
        let notes = vec![legato(60, 0.0, 1.0), note(64, 1.25, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        let in_gap = t.sample(&notes, 0, 1.125).expect("legato bridges the rest");
        assert!(in_gap > 60.0 && in_gap < 64.0);
    }

    #[test]
    fn a_hold_articulation_still_bridges() {
        // Sustain gates at 0.98 — a 2% shortening, far below the transition
        // width and inaudible as detachment. Marking a phrase Sustain, the one
        // articulation whose purpose is to hold, must not cost it its glide.
        let mut a = note(60, 0.0, 1.0);
        a.articulation = Some(ArticulationId::Sustain);
        let notes = vec![a, note(62, 1.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        let at_boundary = t.sample(&notes, 0, 1.0).expect("Sustain still bridges");
        assert!(
            at_boundary > 60.0 && at_boundary < 62.0,
            "expected a transition, got {at_boundary}"
        );
    }

    #[test]
    fn detachment_is_judged_against_the_transition_width_not_a_fixed_gate() {
        // Marcato gates at 0.85. On a one-beat note that releases 0.15 beats
        // early — wider than the transition itself — so the line ends with the
        // note, which is what a marked, separated articulation should do.
        let mut a = note(60, 0.0, 1.0);
        a.articulation = Some(ArticulationId::Marcato);
        let notes = vec![a, note(62, 1.0, 1.0)];
        assert_eq!(
            PitchTrajectory::build(&notes, &[]).sample(&notes, 0, 1.0),
            Some(62.0),
            "a note released well before the next must not glide into it"
        );

        // The same articulation on a very short note releases by an amount
        // smaller than the transition, so there is nothing to detach.
        let mut a = note(60, 0.0, 0.25);
        a.articulation = Some(ArticulationId::Marcato);
        let notes = vec![a, note(62, 0.25, 0.25)];
        let t = PitchTrajectory::build(&notes, &[]);
        let at_boundary = t.sample(&notes, 0, 0.25).expect("sounding");
        assert!(at_boundary > 60.0 && at_boundary < 62.0);
    }

    #[test]
    fn a_sustain_direction_event_does_not_delink_the_clip() {
        let notes = vec![note(60, 0.0, 1.0), note(62, 1.0, 1.0)];
        let directions = vec![MidiArticulationEvent::new(0.0, ArticulationId::Sustain)];
        let t = PitchTrajectory::build(&notes, &directions);
        let at_boundary = t.sample(&notes, 0, 1.0).expect("still bridged");
        assert!(at_boundary > 60.0 && at_boundary < 62.0);
    }

    #[test]
    fn the_two_sampling_entry_points_agree_on_a_shared_boundary() {
        // Detached notes give two spans that abut exactly at beat 1.0.
        let mut a = note(60, 0.0, 1.0);
        a.articulation = Some(ArticulationId::Staccato);
        let notes = vec![a, note(62, 1.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        let mut out = Vec::new();
        t.sample_columns(&notes, 0, 0.0, 0.25, 9, &mut out);
        // Column 4 lands exactly on beat 1.0.
        assert_eq!(out[4], t.sample(&notes, 0, 1.0));
    }

    #[test]
    fn a_detached_note_does_not_bridge() {
        let mut a = note(60, 0.0, 1.0);
        a.articulation = Some(ArticulationId::Staccato);
        let notes = vec![a, note(62, 1.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        // Right at the boundary the first note has ended and the second begins;
        // there is no transition span carrying between them.
        assert_eq!(t.sample(&notes, 0, 0.999), Some(60.0));
        assert_eq!(t.sample(&notes, 0, 1.0), Some(62.0));
    }

    #[test]
    fn a_long_rest_is_not_bridged_even_under_legato() {
        let notes = vec![legato(60, 0.0, 1.0), note(62, 4.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        assert_eq!(t.sample(&notes, 0, 2.5), None);
    }

    #[test]
    fn a_transition_never_swallows_a_short_note() {
        // A 1/32 note between two neighbours must keep some flat body.
        let notes = vec![
            note(60, 0.0, 1.0),
            note(62, 1.0, 1.0 / 32.0),
            note(64, 1.0 + 1.0 / 32.0, 1.0),
        ];
        let t = PitchTrajectory::build(&notes, &[]);
        let middle = &notes[1];
        let centre = middle.start + middle.duration * 0.5;
        let value = t.sample(&notes, 0, centre).expect("sounding");
        assert!(
            (value - 62.0).abs() < 0.5,
            "the short note must still read as its own pitch, got {value}"
        );
    }

    #[test]
    fn overlapping_notes_become_separate_voices() {
        let notes = vec![note(60, 0.0, 2.0), note(64, 0.5, 2.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        assert_eq!(t.voices().len(), 2);
        assert_eq!(t.sample(&notes, 0, 1.0), Some(60.0));
        assert_eq!(t.sample(&notes, 1, 1.0), Some(64.0));
    }

    #[test]
    fn manual_expression_rides_on_top_of_the_baseline() {
        let mut n = note(60, 0.0, 2.0);
        n.pitch_curve = Some(PitchCurve::from_points(vec![
            PitchPoint::new(0.0, -100.0, PitchSegmentShape::Smooth),
            PitchPoint::new(0.5, 0.0, PitchSegmentShape::Linear),
        ]));
        let notes = vec![n];
        let t = PitchTrajectory::build(&notes, &[]);
        assert!((t.sample(&notes, 0, 0.0).unwrap() - 59.0).abs() < 0.001);
        assert!((t.sample(&notes, 0, 1.5).unwrap() - 60.0).abs() < 0.001);
    }

    #[test]
    fn transposing_shifts_the_whole_trajectory_by_the_interval() {
        let mut n = note(60, 0.0, 2.0);
        n.pitch_curve = Some(PitchCurve::from_points(vec![PitchPoint::new(
            0.0,
            -37.0,
            PitchSegmentShape::Linear,
        )]));
        let before = PitchTrajectory::build(std::slice::from_ref(&n), &[])
            .sample(std::slice::from_ref(&n), 0, 1.0)
            .unwrap();
        n.pitch += 2;
        let after = PitchTrajectory::build(std::slice::from_ref(&n), &[])
            .sample(std::slice::from_ref(&n), 0, 1.0)
            .unwrap();
        assert!((after - before - 2.0).abs() < 0.0001);
    }

    #[test]
    fn sample_columns_matches_point_sampling() {
        let notes = vec![note(60, 0.0, 1.0), note(63, 1.0, 1.0)];
        let t = PitchTrajectory::build(&notes, &[]);
        let mut out = Vec::new();
        t.sample_columns(&notes, 0, 0.0, 0.05, 40, &mut out);
        for (column, value) in out.iter().enumerate() {
            let beat = 0.05 * column as f32;
            assert_eq!(*value, t.sample(&notes, 0, beat), "column {column}");
        }
    }

    #[test]
    fn a_direction_articulation_can_make_notes_connect() {
        // Neither note carries a per-note articulation; the clip's direction
        // lane supplies Legato, and the trajectory must honour it.
        let notes = vec![note(60, 0.0, 1.0), note(64, 1.25, 1.0)];
        let directions = vec![MidiArticulationEvent::new(0.0, ArticulationId::Legato)];
        let bridged = PitchTrajectory::build(&notes, &directions);
        assert!(bridged.sample(&notes, 0, 1.125).is_some());
        let plain = PitchTrajectory::build(&notes, &[]);
        assert!(plain.sample(&notes, 0, 1.125).is_none());
    }

    #[test]
    fn hz_conversion_round_trips_at_continuous_pitches() {
        for pitch in [60.0f32, 69.0, 60.37, 71.5] {
            let hz = midi_pitch_to_hz(pitch);
            assert!((hz_to_midi_pitch(hz) - pitch).abs() < 0.001);
        }
        assert!((midi_pitch_to_hz(69.0) - 440.0).abs() < 0.001);
    }

    /// The emission predicate is an optimisation, and an optimisation that
    /// answers "no" when the answer is "yes" silently drops a drawn curve on
    /// the floor. Check it against the thing it is a shortcut for: sampling the
    /// note and looking at what came back.
    #[test]
    fn skipping_a_note_is_only_ever_correct() {
        let curved = |pitch: u8, start: f32, cents: f32| {
            let mut n = note(pitch, start, 1.0);
            n.pitch_curve = Some(PitchCurve {
                points: vec![
                    PitchPoint {
                        id: 1,
                        beat: 0.0,
                        cents: 0.0,
                        shape: PitchSegmentShape::Smooth,
                    },
                    PitchPoint {
                        id: 2,
                        beat: 0.5,
                        cents,
                        shape: PitchSegmentShape::Smooth,
                    },
                ],
            });
            n
        };

        let cases: Vec<(&str, Vec<MidiNoteState>, Vec<MidiArticulationEvent>)> = vec![
            (
                "plain detached notes",
                vec![note(60, 0.0, 1.0), note(64, 2.0, 1.0)],
                vec![],
            ),
            (
                "one drawn note among plain ones",
                vec![
                    note(60, 0.0, 1.0),
                    curved(62, 2.0, 40.0),
                    note(64, 4.0, 1.0),
                ],
                vec![],
            ),
            (
                "legato, so transitions reach into plain notes",
                vec![note(60, 0.0, 1.0), note(64, 1.25, 1.0), note(67, 2.5, 1.0)],
                vec![MidiArticulationEvent::new(0.0, ArticulationId::Legato)],
            ),
            (
                "abutting notes",
                vec![note(60, 0.0, 1.0), note(62, 1.0, 1.0)],
                vec![],
            ),
        ];

        for (label, notes, directions) in cases {
            let trajectory = PitchTrajectory::build(&notes, &directions);
            let mut voice_of_note = vec![0_usize; notes.len()];
            for (voice_index, voice) in trajectory.voices().iter().enumerate() {
                for &note_index in &voice.notes {
                    voice_of_note[note_index] = voice_index;
                }
            }
            let mut out = Vec::new();
            for (index, n) in notes.iter().enumerate() {
                let voice = voice_of_note[index];
                if trajectory.note_departs_from_notated_pitch(&notes, voice, index) {
                    continue;
                }
                // Predicate said "nothing to emit". Sampling must agree.
                let columns = 64;
                let step = n.duration / columns as f32;
                trajectory.sample_columns(&notes, voice, n.start, step, columns, &mut out);
                for (column, value) in out.iter().enumerate() {
                    let sounding = value.unwrap_or(n.pitch as f32);
                    assert!(
                        (sounding - n.pitch as f32).abs() <= 0.01,
                        "{label}: note {index} was skipped but sounds {sounding} at column                          {column}, not its notated {}",
                        n.pitch
                    );
                }
            }
        }
    }

    #[test]
    fn an_empty_clip_produces_no_voices() {
        let t = PitchTrajectory::build(&[], &[]);
        assert!(t.is_empty());
        assert_eq!(t.sample(&[], 0, 0.0), None);
    }
}
