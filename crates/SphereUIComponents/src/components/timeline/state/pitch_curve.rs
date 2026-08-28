//! Continuous pitch performance attached to a MIDI note.
//!
//! A pitch curve stores **cent deviations relative to the note's own
//! `pitch`**, sampled at beats measured from the note start. Storing the
//! deviation (not an absolute frequency or an absolute MIDI note) is what makes
//! the expression survive musical edits: transposing `C4 -> D4`, dragging the
//! note to another bar, or copying it to a new clip all keep the same shape
//! because both anchors (`pitch`, `start`) move with the note.
//!
//! The final sounding pitch is composed, never baked:
//!
//! ```text
//! final_cents = base_note_pitch * 100
//!             + pitch_curve.cents_at(beat_from_note_start)
//!             + tuning_offset
//!             + performance_adjustment
//! ```
//!
//! `cents` is an `f32`, so the model is continuous and has no 12-TET
//! assumption baked in — a Thai bowed-string scale degree that sits 37 cents
//! under an equal-tempered neighbour is representable exactly.
//!
//! The curve lives on [`MidiNoteState::pitch_curve`], the same place
//! per-note articulation lives, so it copies / moves / splits / deletes with
//! its note for free and rides the existing `EditMidiNotes` undo entry.

use super::*;

/// Breakpoints used to resample a Smooth segment that a split cuts through.
/// Enough to keep the eased shape visually identical without turning one split
/// into a dense polyline.
const SMOOTH_SPLIT_STEPS: usize = 6;

/// Widest deviation a single pitch point may carry (two octaves either way).
/// Keeps a bad gesture from producing an unrenderable curve while leaving
/// plenty of room for scoops, falls, and wide portamento.
pub const PITCH_CURVE_MAX_CENTS: f32 = 2400.0;

/// Ceiling on the spline's slope, in cents per beat.
///
/// Two points a thousandth of a beat apart and a hundred cents apart imply a
/// slope of 100 000 cents per beat, and a Hermite segment built on that
/// excursion swings far outside both of them before it comes back. A stroke
/// drawn quickly at a high zoom produces exactly that pair. The monotone limit
/// already bounds the curve *between* two points; this bounds what one
/// pathologically steep pair can do to the segments either side of it.
///
/// Four octaves per beat is far steeper than any sung or bowed portamento and
/// still finite.
const MAX_TANGENT_CENTS_PER_BEAT: f32 = 4800.0;

/// How the curve travels from one point to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PitchSegmentShape {
    /// Straight ramp. The default for drawn and generated points.
    #[default]
    Linear = 0,
    /// Flowing spline — the natural shape for a sung or bowed line, and what
    /// the Draw tool writes.
    ///
    /// A run of these is interpolated as one monotone cubic through all of
    /// them, so the slope carries *through* each point rather than restarting
    /// at it. See `PitchCurve::tangent_at`. The persisted tag is unchanged;
    /// what changed is how the shape is evaluated, so an existing project reads
    /// as the same points drawn with a smoother line.
    Smooth = 1,
    /// Step: hold this point's value until the next point.
    Hold = 2,
}

impl PitchSegmentShape {
    /// Persisted tag. Unknown tags decode to [`PitchSegmentShape::Linear`] so a
    /// newer file degrades instead of failing to load.
    pub fn to_tag(self) -> u8 {
        self as u8
    }

    pub fn from_tag(tag: u8) -> Self {
        match tag {
            1 => Self::Smooth,
            2 => Self::Hold,
            _ => Self::Linear,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Smooth => "Smooth",
            Self::Hold => "Hold",
        }
    }
}

/// One breakpoint of a note's pitch curve.
#[derive(Debug, Clone, PartialEq)]
pub struct PitchPoint {
    /// Stable identity, persisted with the project so selection and drag
    /// targets survive save/load and undo.
    pub id: u64,
    /// Beats from the **note start**. May exceed the note duration; the value
    /// is preserved so shortening and re-lengthening a note is lossless.
    pub beat: f32,
    /// Deviation from the note's `pitch`, in cents.
    pub cents: f32,
    /// Interpolation used from this point to the next.
    pub shape: PitchSegmentShape,
}

impl PitchPoint {
    pub fn new(beat: f32, cents: f32, shape: PitchSegmentShape) -> Self {
        Self {
            id: next_pitch_point_id(),
            beat: beat.max(0.0),
            cents: cents.clamp(-PITCH_CURVE_MAX_CENTS, PITCH_CURVE_MAX_CENTS),
            shape,
        }
    }

    /// Restore from persisted project data, observing the id so later mints
    /// cannot collide.
    pub fn from_persisted(id: u64, beat: f32, cents: f32, shape: PitchSegmentShape) -> Self {
        let id = if id == 0 {
            next_pitch_point_id()
        } else {
            observe_pitch_point_id(id);
            id
        };
        Self {
            id,
            beat: beat.max(0.0),
            cents: cents.clamp(-PITCH_CURVE_MAX_CENTS, PITCH_CURVE_MAX_CENTS),
            shape,
        }
    }
}

/// The continuous pitch performance of one note.
///
/// Invariant: `points` is sorted by `beat` ascending. Every mutating helper
/// re-establishes it, so readers may binary-search / walk in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PitchCurve {
    pub points: Vec<PitchPoint>,
}

impl PitchCurve {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Build from raw points, sorting and clamping. Used by load and by the
    /// generative tools.
    pub fn from_points(mut points: Vec<PitchPoint>) -> Self {
        for point in &mut points {
            point.beat = point.beat.max(0.0);
            point.cents = point
                .cents
                .clamp(-PITCH_CURVE_MAX_CENTS, PITCH_CURVE_MAX_CENTS);
        }
        points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Self { points }
    }

    /// Deviation in cents at `beat` (beats from the note start).
    ///
    /// Before the first point and after the last the curve holds that
    /// endpoint's value, so a curve never introduces a discontinuity at the
    /// note boundaries. An empty curve is flat `0.0` — i.e. exactly the note's
    /// notated pitch.
    pub fn cents_at(&self, beat: f32) -> f32 {
        let points = &self.points;
        match points.len() {
            0 => 0.0,
            1 => points[0].cents,
            _ => {
                if beat <= points[0].beat {
                    return points[0].cents;
                }
                let last = &points[points.len() - 1];
                if beat >= last.beat {
                    return last.cents;
                }
                // Points are sorted; find the segment containing `beat`.
                let idx = match points.binary_search_by(|p| p.beat.total_cmp(&beat)) {
                    Ok(hit) => return points[hit].cents,
                    Err(insert) => insert - 1,
                };
                let a = &points[idx];
                let b = &points[idx + 1];
                let span = b.beat - a.beat;
                if span <= f32::EPSILON {
                    return b.cents;
                }
                let t = ((beat - a.beat) / span).clamp(0.0, 1.0);
                match a.shape {
                    PitchSegmentShape::Hold => a.cents,
                    PitchSegmentShape::Linear => a.cents + (b.cents - a.cents) * t,
                    PitchSegmentShape::Smooth => {
                        // Monotone cubic Hermite through the neighbouring
                        // points, not a per-segment ease. See `tangent_at`.
                        let (start, end) = (self.tangent_at(idx), self.tangent_at(idx + 1));
                        let (t2, t3) = (t * t, t * t * t);
                        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
                        let h10 = t3 - 2.0 * t2 + t;
                        let h01 = -2.0 * t3 + 3.0 * t2;
                        let h11 = t3 - t2;
                        h00 * a.cents + h10 * span * start + h01 * b.cents + h11 * span * end
                    }
                }
            }
        }
    }

    /// Slope at point `index`, in cents per beat, for the smooth spline.
    ///
    /// This is what makes a drawn line *flow* instead of bending. A per-segment
    /// cosine ease — what `Smooth` used to mean — starts and ends every segment
    /// at zero slope, so the line flattens at every single control point and
    /// each span reads as its own little S. Densely drawn, that is a visible
    /// ripple; sparsely drawn, it is a row of plateaus joined by steep ramps.
    /// Neither is what a pitch line looks like when a voice does it.
    ///
    /// Carrying a slope *through* each point instead makes the curve C¹
    /// continuous, which is the whole difference. The slope is limited by the
    /// **Fritsch–Carlson** rule, so the spline is monotone between points: it
    /// passes exactly through everything drawn and never overshoots past it. An
    /// unlimited Catmull-Rom would bulge above a peak the user drew, inventing
    /// pitch nobody asked for — on a control that decides what a note sounds
    /// like, that is not a cosmetic difference.
    ///
    /// A neighbour is only consulted when the segment joining it is itself
    /// `Smooth`. A `Hold` or `Linear` segment is a deliberate corner, and a
    /// spline that reached across one would round off the shape the user chose.
    fn tangent_at(&self, index: usize) -> f32 {
        let points = &self.points;
        if index >= points.len() {
            return 0.0;
        }

        let secant = |from: usize, to: usize| -> Option<f32> {
            let (a, b) = (points.get(from)?, points.get(to)?);
            // The segment's shape lives on its *left* endpoint.
            if points.get(from)?.shape != PitchSegmentShape::Smooth {
                return None;
            }
            let span = b.beat - a.beat;
            (span > f32::EPSILON).then(|| (b.cents - a.cents) / span)
        };

        let incoming = index.checked_sub(1).and_then(|prev| secant(prev, index));
        let outgoing = secant(index, index + 1);

        match (incoming, outgoing) {
            (Some(before), Some(after)) => {
                // A local maximum or minimum: the slope there is zero, which is
                // what stops the curve overshooting the peak.
                if before * after <= 0.0 {
                    return 0.0;
                }
                let average = (before + after) * 0.5;
                // Fritsch-Carlson: no steeper than three times the shallower
                // of the two secants, which is the condition for monotonicity.
                let limit = 3.0 * before.abs().min(after.abs());
                average.clamp(-limit, limit)
            }
            (Some(only), None) | (None, Some(only)) => only,
            (None, None) => 0.0,
        }
        .max(-MAX_TANGENT_CENTS_PER_BEAT)
        .min(MAX_TANGENT_CENTS_PER_BEAT)
    }

    /// Insert or replace a point at `beat`. Points closer than `merge_beats`
    /// collapse into one so a drawn stroke does not accumulate thousands of
    /// coincident breakpoints. Returns the id of the resulting point.
    pub fn set_point(
        &mut self,
        beat: f32,
        cents: f32,
        shape: PitchSegmentShape,
        merge_beats: f32,
    ) -> u64 {
        let beat = beat.max(0.0);
        let cents = cents.clamp(-PITCH_CURVE_MAX_CENTS, PITCH_CURVE_MAX_CENTS);
        let merge = merge_beats.max(0.0);

        // `points` is sorted by beat and every mutation keeps it that way, so
        // the insertion point is a binary search rather than a scan, and the
        // nearest existing point can only be one of the two straddling it.
        //
        // This used to scan every point and then re-sort the whole vector. Both
        // are linear in the points already drawn, and a Draw stroke calls this
        // once per mouse-move sample — so one stroke was quadratic in its own
        // length and got visibly slower the longer it was drawn. Measured, four
        // times the samples cost ten times the time; it is now close to four.
        let insert = self.points.partition_point(|p| p.beat < beat);
        let nearest = [insert.checked_sub(1), Some(insert)]
            .into_iter()
            .flatten()
            .filter_map(|index| self.points.get(index).map(|p| (index, p)))
            .filter(|(_, p)| (p.beat - beat).abs() <= merge)
            .min_by(|(_, a), (_, b)| (a.beat - beat).abs().total_cmp(&(b.beat - beat).abs()))
            .map(|(index, _)| index);

        if let Some(index) = nearest {
            let existing = &mut self.points[index];
            existing.beat = beat;
            existing.cents = cents;
            existing.shape = shape;
            let id = existing.id;
            // Overwriting in place cannot break the ordering: the point being
            // replaced was within `merge` of `beat` and is therefore still
            // between the same two neighbours — unless `merge` is wide enough
            // to reach past one of them, which only the erase paths do.
            debug_assert!(self.is_sorted(), "set_point left the curve unsorted");
            return id;
        }

        let point = PitchPoint::new(beat, cents, shape);
        let id = point.id;
        self.points.insert(insert, point);
        id
    }

    /// Whether the points are in ascending beat order.
    ///
    /// Every method here maintains that, and the binary searches in
    /// [`Self::set_point`] and [`Self::cents_at`] depend on it. Cheap enough to
    /// assert in debug builds at the points where it could plausibly break.
    pub fn is_sorted(&self) -> bool {
        self.points
            .windows(2)
            .all(|pair| pair[0].beat <= pair[1].beat)
    }

    /// Remove every point inside `[from, to]`. Returns how many were removed.
    pub fn erase_range(&mut self, from: f32, to: f32) -> usize {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        let before = self.points.len();
        self.points.retain(|p| p.beat < lo || p.beat > hi);
        before - self.points.len()
    }

    pub fn remove_point(&mut self, id: u64) -> bool {
        let before = self.points.len();
        self.points.retain(|p| p.id != id);
        before != self.points.len()
    }

    pub fn point(&self, id: u64) -> Option<&PitchPoint> {
        self.points.iter().find(|p| p.id == id)
    }

    pub fn point_mut(&mut self, id: u64) -> Option<&mut PitchPoint> {
        self.points.iter_mut().find(|p| p.id == id)
    }

    /// Re-establish the sorted invariant after a drag moved a point in time.
    pub fn sort(&mut self) {
        self.points.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    }

    /// Replace `[from, to]` with `generated`, keeping everything outside it.
    /// The shared primitive behind the Line / Smooth / Transition / Vibrato
    /// tools and, later, model-generated pitch.
    pub fn replace_range(&mut self, from: f32, to: f32, generated: Vec<PitchPoint>) {
        self.erase_range(from, to);
        self.points.extend(generated);
        self.sort();
    }

    /// Straight ramp from `start_cents` to `end_cents` across `[from, to]`.
    pub fn line(from: f32, to: f32, start_cents: f32, end_cents: f32) -> Vec<PitchPoint> {
        vec![
            PitchPoint::new(from, start_cents, PitchSegmentShape::Linear),
            PitchPoint::new(to, end_cents, PitchSegmentShape::Linear),
        ]
    }

    /// Eased note transition (portamento / slide) across `[from, to]`.
    pub fn transition(from: f32, to: f32, start_cents: f32, end_cents: f32) -> Vec<PitchPoint> {
        vec![
            PitchPoint::new(from, start_cents, PitchSegmentShape::Smooth),
            PitchPoint::new(to, end_cents, PitchSegmentShape::Linear),
        ]
    }

    /// Periodic vibrato across `[from, to]`. `depth_cents` is the peak
    /// deviation, `rate_hz_in_beats` the cycle length in beats. Rendered as
    /// smooth breakpoints so it stays editable by hand afterwards — this is
    /// generated *data*, not a hidden modulator.
    pub fn vibrato(
        from: f32,
        to: f32,
        center_cents: f32,
        depth_cents: f32,
        cycle_beats: f32,
    ) -> Vec<PitchPoint> {
        let cycle = cycle_beats.max(0.01);
        let span = (to - from).max(0.0);
        // Four breakpoints per cycle keeps the drawn shape recognisable
        // without flooding the curve with points on a long note.
        let steps = ((span / cycle) * 4.0).round().max(2.0) as usize;
        (0..=steps)
            .map(|i| {
                let t = i as f32 / steps as f32;
                let beat = from + span * t;
                let phase = (span * t / cycle) * std::f32::consts::TAU;
                PitchPoint::new(
                    beat,
                    center_cents + depth_cents * phase.sin(),
                    PitchSegmentShape::Smooth,
                )
            })
            .collect()
    }

    /// Split into the parts before and after `at` (beats from the note start),
    /// re-basing the second half to its own new note start. The value at the
    /// cut is sampled and inserted into both halves so a split is audibly
    /// seamless — the trajectory is unchanged, only its ownership is.
    ///
    /// Used when the piano-roll split tool cuts a note in two, so per-note
    /// expression follows the parts instead of being dropped.
    pub fn split_at(&self, at: f32) -> (PitchCurve, PitchCurve) {
        if self.points.is_empty() {
            return (PitchCurve::default(), PitchCurve::default());
        }
        let at = at.max(0.0);
        let boundary = self.cents_at(at);
        let shape_at_cut = self
            .points
            .iter()
            .rev()
            .find(|p| p.beat <= at)
            .map(|p| p.shape)
            .unwrap_or_default();

        let mut left: Vec<PitchPoint> = self
            .points
            .iter()
            .filter(|p| p.beat < at)
            .cloned()
            .collect();
        left.push(PitchPoint::new(at, boundary, shape_at_cut));

        let mut right = vec![PitchPoint::new(0.0, boundary, shape_at_cut)];
        right.extend(
            self.points
                .iter()
                .filter(|p| p.beat > at)
                .map(|p| PitchPoint::new(p.beat - at, p.cents, p.shape)),
        );

        // A Smooth segment eases across its *whole* span, so cutting it and
        // re-issuing Smooth on each half would ease twice and change the shape
        // the user drew. Resample the straddled segment into Linear
        // breakpoints instead, which reproduces the original curve on both
        // sides to within a breakpoint.
        if shape_at_cut == PitchSegmentShape::Smooth {
            if let Some(index) = self.points.iter().rposition(|p| p.beat <= at) {
                if let Some(next) = self.points.get(index + 1) {
                    let start = self.points[index].beat;
                    let end = next.beat;
                    if end > start && at > start && at < end {
                        // Drop the two endpoints this resample replaces.
                        left.retain(|p| p.beat <= start);
                        right.retain(|p| p.beat >= end - at);
                        for step in 1..SMOOTH_SPLIT_STEPS {
                            let t = step as f32 / SMOOTH_SPLIT_STEPS as f32;
                            let before = start + (at - start) * t;
                            left.push(PitchPoint::new(
                                before,
                                self.cents_at(before),
                                PitchSegmentShape::Linear,
                            ));
                            let after = at + (end - at) * t;
                            right.push(PitchPoint::new(
                                after - at,
                                self.cents_at(after),
                                PitchSegmentShape::Linear,
                            ));
                        }
                        left.push(PitchPoint::new(at, boundary, PitchSegmentShape::Linear));
                        right.push(PitchPoint::new(0.0, boundary, PitchSegmentShape::Linear));
                    }
                }
            }
        }

        (
            PitchCurve::from_points(left),
            PitchCurve::from_points(right),
        )
    }

    /// Deep copy with fresh point identities, for duplicate / paste. The shape
    /// is identical; only the ids differ, so the copy is independently editable.
    pub fn cloned_with_new_ids(&self) -> PitchCurve {
        PitchCurve::from_points(
            self.points
                .iter()
                .map(|p| PitchPoint::new(p.beat, p.cents, p.shape))
                .collect(),
        )
    }

    /// Moving-average smoothing of the points inside `[from, to]`.
    pub fn smooth_range(&mut self, from: f32, to: f32) {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        let indices: Vec<usize> = self
            .points
            .iter()
            .enumerate()
            .filter(|(_, p)| p.beat >= lo && p.beat <= hi)
            .map(|(i, _)| i)
            .collect();
        if indices.len() < 3 {
            return;
        }
        let smoothed: Vec<f32> = indices
            .iter()
            .map(|&i| {
                let prev = self.points[i.saturating_sub(1)].cents;
                let cur = self.points[i].cents;
                let next = self.points[(i + 1).min(self.points.len() - 1)].cents;
                (prev + cur * 2.0 + next) / 4.0
            })
            .collect();
        for (&i, value) in indices.iter().zip(smoothed) {
            self.points[i].cents = value;
            self.points[i].shape = PitchSegmentShape::Smooth;
        }
    }
}

impl TimelineState {
    /// Read one note by stable id inside a MIDI clip.
    pub fn midi_note(&self, clip_id: &str, note_id: u64) -> Option<&MidiNoteState> {
        self.midi_clip_notes(clip_id)?
            .iter()
            .find(|note| note.id == note_id)
    }

    /// Clone of a note (undo `prev`/`next` snapshots for pitch edits).
    pub fn midi_note_snapshot(&self, clip_id: &str, note_id: u64) -> Option<MidiNoteState> {
        self.midi_note(clip_id, note_id).cloned()
    }

    /// The pitch curve of one note, or an empty curve when it has none.
    pub fn note_pitch_curve(&self, clip_id: &str, note_id: u64) -> PitchCurve {
        self.midi_note(clip_id, note_id)
            .and_then(|note| note.pitch_curve.clone())
            .unwrap_or_default()
    }

    /// Sounding pitch of `note` at `beat` beats from the note start, expressed
    /// in **fractional MIDI note numbers** so callers stay free of 12-TET
    /// snapping. This is the composition point named in the module docs; the
    /// engine snapshot builder consumes the same function.
    pub fn note_sounding_pitch(note: &MidiNoteState, beat_from_note_start: f32) -> f32 {
        let cents = note
            .pitch_curve
            .as_ref()
            .map(|curve| curve.cents_at(beat_from_note_start))
            .unwrap_or(0.0);
        note.pitch as f32 + cents / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> PitchCurve {
        PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(1.0, 100.0, PitchSegmentShape::Linear),
        ])
    }

    #[test]
    fn empty_curve_is_the_notated_pitch() {
        assert_eq!(PitchCurve::default().cents_at(0.5), 0.0);
    }

    #[test]
    fn linear_segment_interpolates() {
        assert!((curve().cents_at(0.25) - 25.0).abs() < 0.001);
    }

    #[test]
    fn curve_holds_outside_its_endpoints() {
        let c = curve();
        assert_eq!(c.cents_at(-3.0), 0.0);
        assert_eq!(c.cents_at(9.0), 100.0);
    }

    #[test]
    fn hold_segment_steps() {
        let c = PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Hold),
            PitchPoint::new(1.0, 100.0, PitchSegmentShape::Linear),
        ]);
        assert_eq!(c.cents_at(0.99), 0.0);
    }

    #[test]
    fn transposing_a_note_preserves_the_curve_shape() {
        let mut note = MidiNoteState::new(60, 0.0, 1.0, 100);
        note.pitch_curve = Some(curve());
        let before = TimelineState::note_sounding_pitch(&note, 0.5);
        note.pitch += 2; // C4 -> D4
        let after = TimelineState::note_sounding_pitch(&note, 0.5);
        assert!((after - before - 2.0).abs() < 0.0001);
        // The deviation itself is untouched.
        assert!((note.pitch_curve.as_ref().unwrap().cents_at(0.5) - 50.0).abs() < 0.001);
    }

    #[test]
    fn set_point_merges_within_tolerance() {
        let mut c = curve();
        let id = c.set_point(1.001, -20.0, PitchSegmentShape::Linear, 0.01);
        assert_eq!(c.len(), 2);
        assert_eq!(c.point(id).unwrap().cents, -20.0);
    }

    #[test]
    fn erase_range_removes_only_inside() {
        let mut c = PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(0.5, 50.0, PitchSegmentShape::Linear),
            PitchPoint::new(1.0, 100.0, PitchSegmentShape::Linear),
        ]);
        assert_eq!(c.erase_range(0.4, 0.6), 1);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn vibrato_is_editable_breakpoints() {
        let points = PitchCurve::vibrato(0.0, 2.0, 0.0, 30.0, 0.5);
        assert!(points.len() >= 8);
        assert!(points.iter().all(|p| p.cents.abs() <= 30.001));
    }

    #[test]
    fn split_keeps_the_trajectory_continuous_across_both_parts() {
        let c = PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(2.0, 200.0, PitchSegmentShape::Linear),
        ]);
        let (left, right) = c.split_at(1.0);
        // The cut value is identical on both sides.
        assert!((left.cents_at(1.0) - 100.0).abs() < 0.001);
        assert!((right.cents_at(0.0) - 100.0).abs() < 0.001);
        // The right half is re-based to its own note start.
        assert!((right.cents_at(1.0) - 200.0).abs() < 0.001);
    }

    #[test]
    fn splitting_a_smooth_segment_preserves_its_shape() {
        // A cosine ease from 0 to 200 cents across two beats.
        let c = PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Smooth),
            PitchPoint::new(2.0, 200.0, PitchSegmentShape::Linear),
        ]);
        let (left, right) = c.split_at(1.0);
        for probe in [0.1f32, 0.25, 0.5, 0.75, 0.9] {
            let expected = c.cents_at(probe);
            assert!(
                (left.cents_at(probe) - expected).abs() < 8.0,
                "left half drifted at {probe}: {} vs {expected}",
                left.cents_at(probe)
            );
            let expected = c.cents_at(1.0 + probe);
            assert!(
                (right.cents_at(probe) - expected).abs() < 8.0,
                "right half drifted at {probe}: {} vs {expected}",
                right.cents_at(probe)
            );
        }
        // The cut value still agrees exactly on both sides.
        assert!((left.cents_at(1.0) - right.cents_at(0.0)).abs() < 0.001);
    }

    #[test]
    fn split_of_an_empty_curve_stays_empty() {
        let (left, right) = PitchCurve::default().split_at(1.0);
        assert!(left.is_empty() && right.is_empty());
    }

    #[test]
    fn copies_get_independent_point_ids() {
        let c = curve();
        let copy = c.cloned_with_new_ids();
        assert_eq!(copy.len(), c.len());
        for (a, b) in c.points.iter().zip(&copy.points) {
            assert_ne!(a.id, b.id);
            assert_eq!(a.cents, b.cents);
            assert_eq!(a.beat, b.beat);
        }
    }

    #[test]
    fn continuous_non_12tet_pitch_round_trips() {
        let mut note = MidiNoteState::new(60, 0.0, 1.0, 100);
        note.pitch_curve = Some(PitchCurve::from_points(vec![PitchPoint::new(
            0.0,
            -37.0,
            PitchSegmentShape::Linear,
        )]));
        let pitch = TimelineState::note_sounding_pitch(&note, 0.0);
        assert!((pitch - 59.63).abs() < 0.001);
    }

    fn smooth(points: &[(f32, f32)]) -> PitchCurve {
        PitchCurve::from_points(
            points
                .iter()
                .map(|&(beat, cents)| PitchPoint::new(beat, cents, PitchSegmentShape::Smooth))
                .collect(),
        )
    }

    /// The property that makes a drawn line read as a line and not as a chain
    /// of separate bends: the slope carries through each control point.
    ///
    /// The cosine ease this replaced returned to zero slope at *every* point,
    /// so a straight run of evenly rising points came out as a staircase of
    /// flat spots. Sampled either side of an interior point, a flowing curve
    /// has nearly the same slope; an easing one has nearly none.
    #[test]
    fn the_curve_does_not_flatten_at_every_control_point() {
        let curve = smooth(&[(0.0, 0.0), (1.0, 100.0), (2.0, 200.0), (3.0, 300.0)]);
        let step = 0.01;
        let before = (curve.cents_at(1.0) - curve.cents_at(1.0 - step)) / step;
        let after = (curve.cents_at(1.0 + step) - curve.cents_at(1.0)) / step;

        // A constant-slope run must stay a constant-slope run.
        assert!(
            (before - 100.0).abs() < 5.0,
            "slope into the point is {before} cents/beat, expected ~100"
        );
        assert!(
            (after - 100.0).abs() < 5.0,
            "slope out of the point is {after} cents/beat, expected ~100"
        );
    }

    /// A straight ramp drawn as a run of smooth points must stay straight.
    #[test]
    fn evenly_rising_points_interpolate_as_a_straight_line() {
        let curve = smooth(&[(0.0, 0.0), (1.0, 100.0), (2.0, 200.0)]);
        for step in 0..=20 {
            let beat = step as f32 * 0.1;
            let expected = beat * 100.0;
            assert!(
                (curve.cents_at(beat) - expected).abs() < 1.0,
                "at {beat} the curve reads {} but the ramp is {expected}",
                curve.cents_at(beat)
            );
        }
    }

    /// Monotonicity: the curve must never leave the range of the two points it
    /// is travelling between. An unlimited spline bulges past a peak, which on
    /// a control that decides what a note sounds like is inventing pitch.
    #[test]
    fn the_curve_never_overshoots_the_points_it_passes_through() {
        // A sharp peak and a sharp trough, the shapes that provoke overshoot.
        let curve = smooth(&[
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 400.0),
            (3.0, 0.0),
            (4.0, -400.0),
            (5.0, 0.0),
        ]);
        for step in 0..=500 {
            let beat = step as f32 * 0.01;
            let value = curve.cents_at(beat);
            assert!(
                (-400.5..=400.5).contains(&value),
                "at {beat} the curve reached {value}, outside the drawn range"
            );
        }
        // ...and it still passes exactly through what was drawn.
        assert!((curve.cents_at(2.0) - 400.0).abs() < 0.001);
        assert!((curve.cents_at(4.0) + 400.0).abs() < 0.001);
    }

    /// A `Hold` or `Linear` segment is a deliberate corner. The spline must not
    /// reach across one and round it off.
    #[test]
    fn a_hold_segment_stays_a_step() {
        let curve = PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Hold),
            PitchPoint::new(1.0, 200.0, PitchSegmentShape::Smooth),
            PitchPoint::new(2.0, 200.0, PitchSegmentShape::Smooth),
        ]);
        assert_eq!(curve.cents_at(0.5), 0.0);
        assert_eq!(curve.cents_at(0.999), 0.0);
        assert!((curve.cents_at(1.0) - 200.0).abs() < 0.001);
    }

    #[test]
    fn a_linear_segment_stays_straight() {
        let curve = PitchCurve::from_points(vec![
            PitchPoint::new(0.0, 0.0, PitchSegmentShape::Linear),
            PitchPoint::new(1.0, 100.0, PitchSegmentShape::Smooth),
            PitchPoint::new(2.0, 0.0, PitchSegmentShape::Smooth),
        ]);
        assert!((curve.cents_at(0.5) - 50.0).abs() < 0.001);
        assert!((curve.cents_at(0.25) - 25.0).abs() < 0.001);
    }

    /// Two points almost on top of each other imply an enormous slope, and a
    /// fast stroke at a high zoom produces exactly that pair. The neighbouring
    /// segments must not swing off the screen because of it.
    #[test]
    fn a_near_vertical_pair_does_not_blow_up_its_neighbours() {
        let curve = smooth(&[(0.0, 0.0), (1.0, 0.0), (1.001, 300.0), (2.0, 300.0)]);
        for step in 0..=200 {
            let beat = step as f32 * 0.01;
            let value = curve.cents_at(beat);
            assert!(
                value.is_finite() && (-700.0..=1000.0).contains(&value),
                "at {beat} the curve reached {value}"
            );
        }
    }

    /// `set_point` keeps the points sorted without a full re-sort, and the
    /// binary search it uses depends on that. Inserted out of order, they must
    /// still come out in order.
    #[test]
    fn set_point_keeps_the_curve_sorted_whatever_order_it_is_given() {
        let mut curve = PitchCurve::default();
        for beat in [2.0_f32, 0.5, 3.5, 1.0, 0.0, 2.75] {
            curve.set_point(beat, beat * 10.0, PitchSegmentShape::Smooth, 0.0);
        }
        assert!(curve.is_sorted(), "{:?}", curve.points);
        let beats: Vec<f32> = curve.points.iter().map(|p| p.beat).collect();
        assert_eq!(beats, vec![0.0, 0.5, 1.0, 2.0, 2.75, 3.5]);
    }

    /// A sample within the merge distance replaces the nearest point rather
    /// than adding one, and does not disturb the ordering.
    #[test]
    fn set_point_merges_into_the_nearest_point_within_range() {
        let mut curve = PitchCurve::default();
        let first = curve.set_point(1.0, 0.0, PitchSegmentShape::Smooth, 0.0);
        curve.set_point(2.0, 0.0, PitchSegmentShape::Smooth, 0.0);

        // Closer to the point at 1.0 than to the one at 2.0.
        let merged = curve.set_point(1.05, 50.0, PitchSegmentShape::Smooth, 0.2);
        assert_eq!(merged, first, "merged into the wrong point");
        assert_eq!(curve.len(), 2);
        assert!(curve.is_sorted());
        assert!((curve.cents_at(1.05) - 50.0).abs() < 0.001);
    }
}
