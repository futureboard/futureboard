use super::*;

/// Which MIDI controller stream a lane edits. CC carries its 0..=127 number;
/// the others are single global streams per channel. `PolyPressure` is modeled
/// for completeness but deferred — it needs per-note association the editor does
/// not yet provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiControllerKind {
    CC(u8),
    PitchBend,
    ChannelPressure,
    PolyPressure,
}

/// A single point in a controller lane. `value` is normalized `0.0..=1.0` in
/// state; the UI maps it to the controller's display range (e.g. 0..127 for CC).
#[derive(Debug, Clone, PartialEq)]
pub struct MidiControllerPoint {
    /// Stable identity. Persisted in the project file (v26+) so selection and
    /// drag targets survive save/load. Moves keep this id; copies mint a new one.
    pub id: u64,
    /// Beats relative to the clip start.
    pub beat: f32,
    /// Normalized `0.0..=1.0`.
    pub value: f32,
}

impl MidiControllerPoint {
    /// Construct a point with a freshly minted stable id. `beat` clamps to
    /// `>= 0`, `value` to `0.0..=1.0`.
    pub fn new(beat: f32, value: f32) -> Self {
        Self {
            id: next_controller_point_id(),
            beat: beat.max(0.0),
            value: value.clamp(0.0, 1.0),
        }
    }

    /// Restore a point from persisted project data, observing the id so later
    /// mints cannot collide.
    pub fn from_persisted(id: u64, beat: f32, value: f32) -> Self {
        let id = if id == 0 {
            next_controller_point_id()
        } else {
            observe_controller_point_id(id);
            id
        };
        Self {
            id,
            beat: beat.max(0.0),
            value: value.clamp(0.0, 1.0),
        }
    }
}

/// One controller lane inside a MIDI clip. Points travel with the clip in
/// clip-local beats. (Lane create / edit helpers land with the lane editor UI.)
#[derive(Debug, Clone, PartialEq)]
pub struct MidiControllerLane {
    pub kind: MidiControllerKind,
    pub points: Vec<MidiControllerPoint>,
    pub visible: bool,
    pub height: f32,
    pub collapsed: bool,
}

impl TimelineState {
    // ── MIDI controller lanes ─────────────────────────────────────────────
    pub fn midi_clip_controller_lanes(&self, clip_id: &str) -> Option<&Vec<MidiControllerLane>> {
        for track in &self.tracks {
            for clip in &track.clips {
                if clip.id == clip_id {
                    if let ClipType::Midi {
                        controller_lanes, ..
                    } = &clip.clip_type
                    {
                        return Some(controller_lanes);
                    }
                }
            }
        }
        None
    }

    fn controller_lanes_mut(&mut self, clip_id: &str) -> Option<&mut Vec<MidiControllerLane>> {
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.id == clip_id {
                    if let ClipType::Midi {
                        controller_lanes, ..
                    } = &mut clip.clip_type
                    {
                        return Some(controller_lanes);
                    }
                }
            }
        }
        None
    }

    /// Points of a specific controller lane, if the lane exists.
    pub fn controller_lane_points(
        &self,
        clip_id: &str,
        kind: MidiControllerKind,
    ) -> Option<&Vec<MidiControllerPoint>> {
        self.midi_clip_controller_lanes(clip_id)?
            .iter()
            .find(|l| l.kind == kind)
            .map(|l| &l.points)
    }

    /// Clone of a lane's points (for undo prev/next snapshots).
    pub fn controller_points_snapshot(
        &self,
        clip_id: &str,
        kind: MidiControllerKind,
    ) -> Vec<MidiControllerPoint> {
        self.controller_lane_points(clip_id, kind)
            .cloned()
            .unwrap_or_default()
    }

    /// Ensure a visible lane of `kind` exists. Returns true if newly created.
    pub fn ensure_controller_lane(&mut self, clip_id: &str, kind: MidiControllerKind) -> bool {
        let Some(lanes) = self.controller_lanes_mut(clip_id) else {
            return false;
        };
        if lanes.iter().any(|l| l.kind == kind) {
            return false;
        }
        lanes.push(MidiControllerLane {
            kind,
            points: Vec::new(),
            visible: true,
            height: 80.0,
            collapsed: false,
        });
        true
    }

    /// Remove a controller lane only when it has no points. This backs the MIDI
    /// editor's safe "Remove lane" action and prevents accidental data loss.
    pub fn remove_empty_controller_lane(
        &mut self,
        clip_id: &str,
        kind: MidiControllerKind,
    ) -> bool {
        let Some(lanes) = self.controller_lanes_mut(clip_id) else {
            return false;
        };
        let Some(index) = lanes
            .iter()
            .position(|lane| lane.kind == kind && lane.points.is_empty())
        else {
            return false;
        };
        lanes.remove(index);
        true
    }

    fn sort_lane_points(points: &mut [MidiControllerPoint]) {
        points.sort_by(|a, b| {
            a.beat
                .partial_cmp(&b.beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Overwrite a lane's points wholesale (used by undo). Creates the lane if
    /// missing so undo can restore points into a removed lane.
    pub fn set_controller_lane_points(
        &mut self,
        clip_id: &str,
        kind: MidiControllerKind,
        mut points: Vec<MidiControllerPoint>,
    ) {
        if points.is_empty() {
            if let Some(lanes) = self.controller_lanes_mut(clip_id) {
                lanes.retain(|lane| lane.kind != kind);
            }
            return;
        }
        self.ensure_controller_lane(clip_id, kind);
        Self::sort_lane_points(&mut points);
        if let Some(lanes) = self.controller_lanes_mut(clip_id) {
            if let Some(lane) = lanes.iter_mut().find(|l| l.kind == kind) {
                lane.points = points;
            }
        }
    }

    /// Add or update a point at `beat` (merging within ~1e-3 beats). `value`
    /// clamps to `0.0..=1.0`. Creates the lane if missing.
    pub fn put_controller_point(
        &mut self,
        clip_id: &str,
        kind: MidiControllerKind,
        beat: f32,
        value: f32,
    ) {
        self.ensure_controller_lane(clip_id, kind);
        let beat = beat.max(0.0);
        let value = value.clamp(0.0, 1.0);
        if let Some(lanes) = self.controller_lanes_mut(clip_id) {
            if let Some(lane) = lanes.iter_mut().find(|l| l.kind == kind) {
                if let Some(p) = lane
                    .points
                    .iter_mut()
                    .find(|p| (p.beat - beat).abs() < 1.0e-3)
                {
                    p.value = value;
                } else {
                    lane.points.push(MidiControllerPoint::new(beat, value));
                    Self::sort_lane_points(&mut lane.points);
                }
            }
        }
    }

    /// Move an existing point (by id) to a new beat/value, re-sorting the lane.
    /// `beat` clamps to `>= 0`, `value` to `0.0..=1.0`. Returns true if found.
    pub fn set_controller_point(
        &mut self,
        clip_id: &str,
        kind: MidiControllerKind,
        id: u64,
        beat: f32,
        value: f32,
    ) -> bool {
        if let Some(lanes) = self.controller_lanes_mut(clip_id) {
            if let Some(lane) = lanes.iter_mut().find(|l| l.kind == kind) {
                if let Some(p) = lane.points.iter_mut().find(|p| p.id == id) {
                    p.beat = beat.max(0.0);
                    p.value = value.clamp(0.0, 1.0);
                    Self::sort_lane_points(&mut lane.points);
                    return true;
                }
            }
        }
        false
    }

    /// Delete points within `tol` beats of `beat`. Returns how many were removed.
    /// Remove one controller point by id. Returns `true` when it was there.
    ///
    /// By id, not by beat: the right-click delete acts on the point the cursor
    /// is actually over, and [`Self::delete_controller_points_near`] would take
    /// every point sharing that beat with it.
    pub fn delete_controller_point(
        &mut self,
        clip_id: &str,
        kind: MidiControllerKind,
        point_id: u64,
    ) -> bool {
        let Some(lanes) = self.controller_lanes_mut(clip_id) else {
            return false;
        };
        let Some(lane) = lanes.iter_mut().find(|l| l.kind == kind) else {
            return false;
        };
        let before = lane.points.len();
        lane.points.retain(|point| point.id != point_id);
        before != lane.points.len()
    }

    pub fn delete_controller_points_near(
        &mut self,
        clip_id: &str,
        kind: MidiControllerKind,
        beat: f32,
        tol: f32,
    ) -> usize {
        if let Some(lanes) = self.controller_lanes_mut(clip_id) {
            if let Some(lane) = lanes.iter_mut().find(|l| l.kind == kind) {
                let before = lane.points.len();
                lane.points.retain(|p| (p.beat - beat).abs() > tol);
                return before - lane.points.len();
            }
        }
        0
    }
}
