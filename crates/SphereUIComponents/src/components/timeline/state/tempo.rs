use super::*;

/// In-flight tempo-point move on the global Tempo Track lane.
#[derive(Debug, Clone)]
pub struct TempoPointDrag {
    pub point_id: String,
    /// Set once the point has actually moved so a pure click never marks dirty.
    pub moved: bool,
}

// ── Tempo map ─────────────────────────────────────────────────────────────────

/// How the timeline maps musical time to horizontal pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineTimebase {
    /// Beat positions are spaced uniformly; tempo affects playback only.
    #[default]
    MusicalBeats,
    /// Beat positions map through TempoMap seconds; faster sections shrink.
    AbsoluteSeconds,
}

/// Interpolation shape between a tempo point and the next one. Mirrors the
/// audio engine's tempo concept. `Smooth` is stored/round-tripped even though
/// it currently evaluates as `Linear` until the curve math lands engine-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TempoCurve {
    #[default]
    Hold,
    Linear,
    Smooth,
}

impl TempoCurve {
    pub fn to_tag(self) -> u8 {
        match self {
            TempoCurve::Hold => 0,
            TempoCurve::Linear => 1,
            TempoCurve::Smooth => 2,
        }
    }

    pub fn from_tag(tag: u8) -> Self {
        match tag {
            1 => TempoCurve::Linear,
            2 => TempoCurve::Smooth,
            _ => TempoCurve::Hold,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TempoCurve::Hold => "Hold",
            TempoCurve::Linear => "Linear",
            TempoCurve::Smooth => "Smooth",
        }
    }
}

/// A tempo change anchored at a musical beat. The `curve` describes how tempo
/// moves from this point to the next one. Marker labels and the tempo-track
/// editor read `bpm` directly — transport BPM uses [`TempoMap::bpm_at_beat`].
#[derive(Debug, Clone, PartialEq)]
pub struct TempoPoint {
    pub id: String,
    pub beat: f64,
    pub bpm: f64,
    pub curve: TempoCurve,
}

impl TempoPoint {
    pub fn new(beat: f64, bpm: f64, curve: TempoCurve) -> Self {
        Self {
            id: next_tempo_point_id(),
            beat: beat.max(0.0),
            bpm: bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX),
            curve,
        }
    }

    pub fn with_id(id: impl Into<String>, beat: f64, bpm: f64, curve: TempoCurve) -> Self {
        Self {
            id: id.into(),
            beat: beat.max(0.0),
            bpm: bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX),
            curve,
        }
    }
}

/// Project-level tempo automation. This is global state owned by the project,
/// not by any track — the TempoTrack is only a view/controller over this map.
/// When `points` is empty the project plays at the timeline's base BPM; the
/// base BPM is supplied by the caller (`TimelineState::bpm`) so this map stays
/// self-contained and cheap to clone.
/// Cached hold-mode segment for beat/time conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TempoHoldSegment {
    start_beat: f64,
    start_seconds: f64,
    bpm: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TempoMap {
    /// Sorted (by beat) tempo markers in addition to the implicit base point at
    /// beat 0. Kept small; UI-thread only, so a linear scan is fine.
    pub points: Vec<TempoPoint>,
    /// Bumped on every edit so UI/engine caches can invalidate.
    revision: u64,
}

impl TempoMap {
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            revision: 0,
        }
    }

    pub fn with_points(points: Vec<TempoPoint>) -> Self {
        let mut map = Self::new();
        map.points = points;
        map.sort();
        map.bump_revision();
        map
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// True when the map carries any tempo automation (one or more markers).
    /// With no markers the project is a single static tempo.
    pub fn has_automation(&self) -> bool {
        !self.points.is_empty()
    }

    /// Hold-mode seconds at `beat` using step-hold segments between markers.
    pub fn seconds_at_beat(&self, beat: f64, base_bpm: f64) -> f64 {
        let beat = beat.max(0.0);
        let segments = self.hold_segments(base_bpm);
        let seg = hold_segment_at_beat(&segments, beat);
        seg.start_seconds + (beat - seg.start_beat) * 60.0 / seg.bpm.max(TEMPO_BPM_MIN)
    }

    /// Inverse of [`Self::seconds_at_beat`] for hold-mode segments.
    pub fn beat_at_seconds(&self, seconds: f64, base_bpm: f64) -> f64 {
        let seconds = seconds.max(0.0);
        let segments = self.hold_segments(base_bpm);
        if segments.is_empty() {
            return 0.0;
        }
        if seconds <= segments[0].start_seconds {
            return 0.0;
        }
        let idx = segments
            .partition_point(|seg| seg.start_seconds <= seconds)
            .saturating_sub(1);
        let seg = &segments[idx.min(segments.len() - 1)];
        let elapsed = seconds - seg.start_seconds;
        seg.start_beat + elapsed * seg.bpm.max(TEMPO_BPM_MIN) / 60.0
    }

    pub fn samples_at_beat(&self, beat: f64, base_bpm: f64, sample_rate: f64) -> u64 {
        (self.seconds_at_beat(beat, base_bpm) * sample_rate.max(1.0))
            .round()
            .max(0.0) as u64
    }

    pub fn beat_at_samples(&self, samples: u64, base_bpm: f64, sample_rate: f64) -> f64 {
        let seconds = samples as f64 / sample_rate.max(1.0);
        self.beat_at_seconds(seconds, base_bpm)
    }

    /// Effective BPM at `beat`, evaluating curves between markers. `base_bpm`
    /// is the implicit tempo at beat 0 (the timeline's nominal BPM).
    pub fn bpm_at_beat(&self, beat: f64, base_bpm: f64) -> f64 {
        if self.points.is_empty() {
            return base_bpm;
        }
        let beat = beat.max(0.0);
        // Build the effective point preceding `beat` and its successor without
        // allocating: walk the implicit base point followed by the markers.
        let first = &self.points[0];
        if beat < first.beat {
            // Before the first marker we sit on the implicit base point. The
            // base point holds (Hold) up to the first marker.
            return base_bpm;
        }
        // Find the last marker at or before `beat`.
        let mut idx = 0usize;
        for (i, p) in self.points.iter().enumerate() {
            if p.beat <= beat {
                idx = i;
            } else {
                break;
            }
        }
        let cur = &self.points[idx];
        let next = self.points.get(idx + 1);
        match (cur.curve, next) {
            (TempoCurve::Hold, _) | (_, None) => cur.bpm,
            (curve, Some(next)) => {
                let span = (next.beat - cur.beat).max(1e-9);
                let t = ((beat - cur.beat) / span).clamp(0.0, 1.0);
                let t = match curve {
                    TempoCurve::Smooth => t * t * (3.0 - 2.0 * t),
                    _ => t,
                };
                cur.bpm + (next.bpm - cur.bpm) * t
            }
        }
    }

    /// Ruler/tempo-track label for a stored marker BPM — never the transport
    /// playhead-evaluated tempo.
    pub fn format_marker_label(bpm: f64) -> String {
        if bpm.fract().abs() < 0.05 {
            format!("{bpm:.0}")
        } else {
            format!("{bpm:.1}")
        }
    }

    /// Assign generated ids to legacy points loaded without one.
    pub fn ensure_point_ids(&mut self) {
        for point in &mut self.points {
            if point.id.is_empty() {
                point.id = next_tempo_point_id();
            }
        }
    }

    /// Id of the marker governing `beat` (last point at or before `beat`).
    pub fn point_id_at_or_before_beat(&self, beat: f64) -> Option<&str> {
        let beat = beat.max(0.0);
        self.points
            .iter()
            .filter(|p| p.beat <= beat)
            .last()
            .map(|p| p.id.as_str())
    }

    /// Update only the matching tempo point's stored BPM by stable id.
    pub fn update_point_bpm_by_id(&mut self, id: &str, bpm: f64) -> bool {
        let bpm = bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        if let Some(point) = self.points.iter_mut().find(|p| p.id == id) {
            point.bpm = bpm;
            self.bump_revision();
            true
        } else {
            false
        }
    }

    /// Insert a tempo marker, replacing any existing marker within a small beat
    /// epsilon. Keeps `points` sorted by beat.
    pub fn add_or_update_point(&mut self, beat: f64, bpm: f64, curve: TempoCurve) {
        let beat = beat.max(0.0);
        let bpm = bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        if let Some(existing) = self
            .points
            .iter_mut()
            .find(|p| (p.beat - beat).abs() < 1e-6)
        {
            existing.bpm = bpm;
            existing.curve = curve;
        } else {
            self.points.push(TempoPoint::new(beat, bpm, curve));
        }
        self.sort();
        self.bump_revision();
    }

    /// Remove the marker nearest `beat` within `epsilon` beats. Returns whether
    /// a marker was removed.
    pub fn remove_point_near(&mut self, beat: f64, epsilon: f64) -> bool {
        if let Some(idx) = self
            .points
            .iter()
            .position(|p| (p.beat - beat).abs() <= epsilon)
        {
            self.points.remove(idx);
            self.bump_revision();
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.points.clear();
        self.bump_revision();
    }

    /// Replace the map with exactly one marker (fixed tempo automation).
    pub fn reset_to_single_point(&mut self, beat: f64, bpm: f64, curve: TempoCurve) {
        self.points.clear();
        self.points.push(TempoPoint::new(beat, bpm, curve));
        self.sort();
        self.bump_revision();
    }

    pub fn remove_point_by_id(&mut self, id: &str) -> bool {
        if let Some(idx) = self.points.iter().position(|p| p.id == id) {
            self.points.remove(idx);
            self.bump_revision();
            true
        } else {
            false
        }
    }

    pub fn move_point_by_id(&mut self, id: &str, beat: f64, bpm: f64) -> bool {
        let beat = beat.max(0.0);
        let bpm = bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        if self
            .points
            .iter()
            .any(|p| p.id != id && (p.beat - beat).abs() < TEMPO_BEAT_EPSILON)
        {
            return false;
        }
        if let Some(point) = self.points.iter_mut().find(|p| p.id == id) {
            point.beat = beat;
            point.bpm = bpm;
            self.sort();
            self.bump_revision();
            true
        } else {
            false
        }
    }

    pub fn update_point_curve_by_id(&mut self, id: &str, curve: TempoCurve) -> bool {
        if let Some(point) = self.points.iter_mut().find(|p| p.id == id) {
            point.curve = curve;
            self.bump_revision();
            true
        } else {
            false
        }
    }

    /// Hold a constant tempo from `beat` onward by removing later markers.
    pub fn set_fixed_from_beat(&mut self, beat: f64, bpm: f64, base_bpm: f64) {
        let beat = beat.max(0.0);
        let bpm = bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        self.points.retain(|p| p.beat < beat - TEMPO_BEAT_EPSILON);
        if beat <= TEMPO_BEAT_EPSILON {
            self.reset_to_single_point(0.0, bpm, TempoCurve::Hold);
            return;
        }
        if !self
            .points
            .iter()
            .any(|p| (p.beat - beat).abs() < TEMPO_BEAT_EPSILON)
        {
            if self.points.is_empty() {
                self.add_or_update_point(0.0, base_bpm, TempoCurve::Hold);
            }
            self.add_or_update_point(beat, bpm, TempoCurve::Hold);
        } else {
            self.add_or_update_point(beat, bpm, TempoCurve::Hold);
        }
    }

    fn sort(&mut self) {
        self.points.sort_by(|a, b| {
            a.beat
                .partial_cmp(&b.beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Overwrite the marker list from an undo/redo snapshot, bumping the
    /// revision forward so downstream caches still invalidate.
    pub fn restore_points(&mut self, points: Vec<TempoPoint>) {
        self.points = points;
        self.bump_revision();
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn hold_segments(&self, base_bpm: f64) -> Vec<TempoHoldSegment> {
        let base_bpm = base_bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        let mut markers: Vec<(f64, f64)> = Vec::new();
        if self.points.is_empty() {
            markers.push((0.0, base_bpm));
        } else {
            if self.points[0].beat > 0.0 {
                markers.push((0.0, base_bpm));
            }
            for point in &self.points {
                markers.push((point.beat, point.bpm));
            }
        }
        let mut segments = Vec::with_capacity(markers.len());
        let mut start_seconds = 0.0;
        for (i, (beat, bpm)) in markers.iter().enumerate() {
            segments.push(TempoHoldSegment {
                start_beat: *beat,
                start_seconds,
                bpm: *bpm,
            });
            if let Some((next_beat, _)) = markers.get(i + 1) {
                start_seconds += (next_beat - beat) * 60.0 / bpm.max(TEMPO_BPM_MIN);
            }
        }
        segments
    }
}

fn hold_segment_at_beat(segments: &[TempoHoldSegment], beat: f64) -> TempoHoldSegment {
    if segments.is_empty() {
        return TempoHoldSegment {
            start_beat: 0.0,
            start_seconds: 0.0,
            bpm: TEMPO_BPM_MIN,
        };
    }
    let idx = segments
        .partition_point(|seg| seg.start_beat <= beat)
        .saturating_sub(1);
    segments[idx.min(segments.len() - 1)]
}

/// BPM clamp range for tempo points (matches the audio engine spec).
pub const TEMPO_BPM_MIN: f64 = 20.0;

pub const TEMPO_BPM_MAX: f64 = 999.0;

/// Default expanded height for the global Tempo Track lane (px).
///
/// The conductor lanes are secondary utility rows: they say what the tempo and
/// meter are, not what the music is. At 72 the Tempo lane took more vertical
/// space than a whole compact track row, so it now sits in the same 40-44 band
/// as the other global lanes and gives the height back to the arrangement.
pub const TEMPO_TRACK_HEIGHT: f32 = 44.0;

/// Collapsed/minimal Tempo Track lane height (px).
pub const TEMPO_TRACK_HEIGHT_COLLAPSED: f32 = 32.0;

/// Vertical padding inside the tempo lane curve area (px).
pub const TEMPO_LANE_PAD: f32 = 5.0;

/// Two tempo points within this many beats are treated as the same slot.
pub const TEMPO_BEAT_EPSILON: f64 = 1e-6;

impl TimelineState {
    /// Effective BPM at a given beat, honoring tempo automation. Falls back to
    /// the static `bpm` when the tempo map has no markers.
    pub fn effective_bpm_at_beat(&self, beat: f64) -> f64 {
        self.tempo_map.bpm_at_beat(beat, self.bpm as f64)
    }

    /// Effective BPM at the current playhead position.
    pub fn effective_bpm_at_playhead(&self) -> f64 {
        self.effective_bpm_at_beat(self.transport.playhead_beats as f64)
    }

    /// Whether tempo automation is active (one or more markers present).
    pub fn tempo_has_automation(&self) -> bool {
        self.tempo_map.has_automation()
    }

    /// Height of the global Tempo Track lane when visible, else 0.
    pub fn tempo_track_height(&self) -> f32 {
        if !self.show_tempo_track {
            return 0.0;
        }
        self.global_lane_height(GlobalLaneKind::Tempo)
    }

    /// Replace the whole tempo state from an undo/redo snapshot.
    ///
    /// Restores the marker list *and* the fixed project BPM together: clearing
    /// automation folds the effective BPM back into `bpm`, so undoing it has to
    /// put both halves back. The map revision keeps moving forward (it is a
    /// cache-invalidation counter, not part of the value) and a selection that
    /// no longer names a live marker is dropped.
    pub fn restore_tempo_state(&mut self, points: Vec<TempoPoint>, bpm: f32) {
        self.tempo_map.restore_points(points);
        self.bpm = bpm;
        if let Some(id) = self.selected_tempo_point_id.clone() {
            if !self.tempo_map.points.iter().any(|p| p.id == id) {
                self.selected_tempo_point_id = None;
            }
        }
    }

    /// Secondary label for the Tempo lane header (fixed BPM or automation range).
    pub fn tempo_lane_header_subtitle(&self) -> String {
        let bpm = self.effective_bpm_at_playhead();
        if self.tempo_map.points.len() <= 1 {
            if bpm.fract().abs() < 0.05 {
                format!("Fixed {:.0} BPM", bpm)
            } else {
                format!("Fixed {:.1} BPM", bpm)
            }
        } else {
            let mut min = bpm;
            let mut max = bpm;
            for p in &self.tempo_map.points {
                min = min.min(p.bpm);
                max = max.max(p.bpm);
            }
            if (max - min).abs() < 0.5 {
                if bpm.fract().abs() < 0.05 {
                    format!("{:.0} BPM", bpm)
                } else {
                    format!("{:.1} BPM", bpm)
                }
            } else {
                format!("{:.0}–{:.0} BPM", min.round(), max.round())
            }
        }
    }

    /// Scroll/zoom the arrangement so all tempo automation points are visible.
    pub fn fit_tempo_automation_in_view(&mut self) {
        if self.tempo_map.points.is_empty() {
            return;
        }
        let min_beat = self
            .tempo_map
            .points
            .iter()
            .map(|p| p.beat)
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let max_beat = self
            .tempo_map
            .points
            .iter()
            .map(|p| p.beat)
            .fold(0.0, f64::max);
        let pad = 8.0;
        let span_beats = (max_beat - min_beat + pad * 2.0).max(16.0);
        let width = self.viewport.viewport_width.max(200.0);
        let needed_ppb = width / span_beats as f32;
        let current_ppb = self.pixels_per_beat().max(0.0001);
        if needed_ppb < current_ppb {
            let factor = (needed_ppb / current_ppb).clamp(0.05, 1.0);
            self.zoom_by(factor, width * 0.5);
        }
        let scroll = ((min_beat - pad).max(0.0) as f32 * self.pixels_per_beat()).max(0.0);
        self.viewport.scroll_x = scroll;
        self.viewport.target_scroll_x = scroll;
    }

    /// Auto-fit BPM range for the Tempo Track curve with padding.
    pub fn tempo_lane_bpm_range(&self) -> (f64, f64) {
        let mut min = self.bpm as f64;
        let mut max = self.bpm as f64;
        for p in &self.tempo_map.points {
            min = min.min(p.bpm);
            max = max.max(p.bpm);
        }
        let pad = ((max - min) * 0.15).max(10.0);
        let mut min_bpm = (min - pad).clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        let mut max_bpm = (max + pad).clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        if (max_bpm - min_bpm) < 20.0 {
            let mid = (min_bpm + max_bpm) * 0.5;
            min_bpm = (mid - 10.0).max(TEMPO_BPM_MIN);
            max_bpm = (mid + 10.0).min(TEMPO_BPM_MAX);
        }
        (min_bpm, max_bpm)
    }

    /// Show the Tempo Track lane and ensure at least one anchor point exists.
    pub fn show_tempo_track_lane(&mut self) {
        self.show_tempo_track = true;
        self.ensure_tempo_anchor_point();
    }

    pub fn hide_tempo_track_lane(&mut self) {
        self.show_tempo_track = false;
        self.selected_tempo_point_id = None;
    }

    /// Seed beat-0 marker when the map is empty so the lane always has data.
    pub fn ensure_tempo_anchor_point(&mut self) {
        if self.tempo_map.points.is_empty() {
            let bpm = self.bpm as f64;
            self.tempo_map
                .add_or_update_point(0.0, bpm, TempoCurve::Hold);
        }
        self.tempo_map.ensure_point_ids();
    }

    pub fn select_tempo_point(&mut self, id: &str) {
        self.selected_tempo_point_id = Some(id.to_string());
    }

    pub fn clear_tempo_point_selection(&mut self) {
        self.selected_tempo_point_id = None;
    }

    pub fn tempo_point_at(
        &self,
        beat: f64,
        bpm: f64,
        beat_tol: f64,
        bpm_tol: f64,
    ) -> Option<String> {
        self.tempo_map
            .points
            .iter()
            .find(|p| (p.beat - beat).abs() <= beat_tol && (p.bpm - bpm).abs() <= bpm_tol)
            .map(|p| p.id.clone())
    }

    pub fn add_tempo_point(&mut self, beat: f64, bpm: f64) -> Option<String> {
        let beat = beat.max(0.0);
        let bpm = bpm.clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX);
        if self.tempo_map.points.is_empty() && beat > TEMPO_BEAT_EPSILON {
            self.tempo_map
                .add_or_update_point(0.0, self.bpm as f64, TempoCurve::Hold);
        }
        self.tempo_map
            .add_or_update_point(beat, bpm, TempoCurve::Hold);
        self.tempo_map.ensure_point_ids();
        self.tempo_map
            .points
            .iter()
            .find(|p| (p.beat - beat).abs() < TEMPO_BEAT_EPSILON)
            .map(|p| p.id.clone())
    }

    pub fn move_tempo_point(&mut self, id: &str, beat: f64, bpm: f64) -> bool {
        self.tempo_map.move_point_by_id(id, beat, bpm)
    }

    pub fn delete_tempo_point(&mut self, id: &str) -> bool {
        if self.tempo_map.remove_point_by_id(id) {
            if self.selected_tempo_point_id.as_deref() == Some(id) {
                self.selected_tempo_point_id = None;
            }
            true
        } else {
            false
        }
    }

    pub fn set_tempo_point_curve(&mut self, id: &str, curve: TempoCurve) -> bool {
        self.tempo_map.update_point_curve_by_id(id, curve)
    }

    pub fn set_fixed_tempo_from_beat(&mut self, beat: f64, bpm: f64) {
        let base = self.bpm as f64;
        self.tempo_map.set_fixed_from_beat(beat, bpm, base);
        self.tempo_map.ensure_point_ids();
    }

    /// BPM values rendered as tempo-track point handles (for tests/debug).
    pub fn tempo_track_render_bpm_values(&self) -> Vec<f64> {
        if self.tempo_map.points.is_empty() {
            vec![self.bpm as f64]
        } else {
            self.tempo_map.points.iter().map(|p| p.bpm).collect()
        }
    }

    /// Effective BPM across the visible beat range (flat line check for tests).
    pub fn tempo_track_bpm_samples(&self, viewport_width: f32) -> Vec<f64> {
        let (start, end) = self.visible_beat_range(viewport_width);
        let cols = viewport_width.ceil().max(1.0) as usize;
        (0..=cols)
            .map(|col| {
                let beat = start as f64 + (end - start) as f64 * (col as f64 / cols as f64);
                self.effective_bpm_at_beat(beat)
            })
            .collect()
    }
}
