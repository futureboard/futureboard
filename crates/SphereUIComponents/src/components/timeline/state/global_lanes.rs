use super::*;

/// Global/system lanes rendered between the ruler and normal tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalLaneKind {
    Tempo,
    TimeSignature,
    SongText,
    Marker,
    Arranger,
}

/// Shortest a global lane may be dragged. Below this the header controls stop
/// fitting and the lane stops being a usable hit target.
pub const GLOBAL_LANE_MIN_HEIGHT: f32 = 28.0;

/// Tallest a global lane may be dragged. The conductor lanes sit above the
/// arrangement and never scroll, so an unbounded lane could push every track
/// off screen.
pub const GLOBAL_LANE_MAX_HEIGHT: f32 = 240.0;

/// Hit height of the drag strip on a global lane's bottom edge.
pub const GLOBAL_LANE_RESIZE_HANDLE_HITBOX: f32 = 5.0;

/// User-set heights for the resizable global lanes. `None` means "use the
/// lane's default height".
///
/// View state, like the lane visibility and collapse flags: it is not part of
/// the audio graph and never reaches the engine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GlobalLaneHeights {
    pub tempo: Option<f32>,
    pub time_signature: Option<f32>,
    pub song_text: Option<f32>,
}

impl GlobalLaneHeights {
    pub fn get(&self, kind: GlobalLaneKind) -> Option<f32> {
        match kind {
            GlobalLaneKind::Tempo => self.tempo,
            GlobalLaneKind::TimeSignature => self.time_signature,
            GlobalLaneKind::SongText => self.song_text,
            GlobalLaneKind::Marker | GlobalLaneKind::Arranger => None,
        }
    }

    pub fn set(&mut self, kind: GlobalLaneKind, height: Option<f32>) {
        let height = height.map(clamp_global_lane_height);
        match kind {
            GlobalLaneKind::Tempo => self.tempo = height,
            GlobalLaneKind::TimeSignature => self.time_signature = height,
            GlobalLaneKind::SongText => self.song_text = height,
            GlobalLaneKind::Marker | GlobalLaneKind::Arranger => {}
        }
    }
}

pub fn clamp_global_lane_height(height: f32) -> f32 {
    height.clamp(GLOBAL_LANE_MIN_HEIGHT, GLOBAL_LANE_MAX_HEIGHT)
}

/// In-flight global-lane height drag. Mirrors `TrackHeightResizeSession`: the
/// start height is captured once so every move maps an absolute pointer delta
/// instead of accumulating per-frame rounding.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalLaneResizeSession {
    pub kind: GlobalLaneKind,
    /// Rendered height at gesture start — the base every move offsets from.
    pub start_height: f32,
    /// The lane's stored height entry at gesture start. `None` means it was on
    /// its default, and that is what undo restores: pinning an explicit value
    /// equal to the default would silently opt the lane out of future default
    /// changes.
    pub start_custom_height: Option<f32>,
    pub start_mouse_y: f32,
}

/// Map a BPM value to a lane-local y coordinate (high BPM near the top).
pub fn bpm_to_y(bpm: f64, lane_height: f32, min_bpm: f64, max_bpm: f64) -> f32 {
    let pad = TEMPO_LANE_PAD;
    let usable = (lane_height - 2.0 * pad).max(1.0);
    let span = (max_bpm - min_bpm).max(1e-9);
    let t = ((bpm - min_bpm) / span).clamp(0.0, 1.0);
    pad + ((1.0 - t) as f32) * usable
}

/// Inverse of [`bpm_to_y`]: lane-local y → BPM.
pub fn y_to_bpm(y: f32, lane_height: f32, min_bpm: f64, max_bpm: f64) -> f64 {
    let pad = TEMPO_LANE_PAD;
    let usable = (lane_height - 2.0 * pad).max(1.0);
    let t = ((y - pad) / usable).clamp(0.0, 1.0);
    let span = (max_bpm - min_bpm).max(1e-9);
    (max_bpm - t as f64 * span).clamp(TEMPO_BPM_MIN, TEMPO_BPM_MAX)
}

impl TimelineState {
    /// Total height of the visible conductor lanes.
    ///
    /// `arrangement_content_top()` is built from this and drives every
    /// window-y -> arrangement-y conversion, so a lane counted here but not
    /// drawn (or vice versa) offsets all arrangement hit-testing.
    pub fn global_lanes_height(&self) -> f32 {
        self.tempo_track_height()
            + self.time_signature_track_height()
            + self.song_text_track_height()
    }

    /// Height of the global Song Text lane when visible, else 0.
    pub fn song_text_track_height(&self) -> f32 {
        if !self.show_song_text_track {
            return 0.0;
        }
        self.global_lane_height(GlobalLaneKind::SongText)
    }

    /// Default (un-resized) height for a global lane.
    pub fn global_lane_default_height(kind: GlobalLaneKind) -> f32 {
        match kind {
            GlobalLaneKind::Tempo => TEMPO_TRACK_HEIGHT,
            GlobalLaneKind::TimeSignature => Self::TIME_SIGNATURE_TRACK_HEIGHT,
            GlobalLaneKind::SongText => {
                crate::components::timeline::song_text_track::SONG_TEXT_LANE_HEIGHT
            }
            GlobalLaneKind::Marker | GlobalLaneKind::Arranger => DEFAULT_TRACK_HEIGHT,
        }
    }

    /// Live height of one global lane, ignoring visibility. Collapse wins over a
    /// custom height so the collapse button always produces the compact row.
    pub fn global_lane_height(&self, kind: GlobalLaneKind) -> f32 {
        match kind {
            GlobalLaneKind::Tempo if self.tempo_track_collapsed => TEMPO_TRACK_HEIGHT_COLLAPSED,
            GlobalLaneKind::TimeSignature if self.time_signature_track_collapsed => {
                Self::TIME_SIGNATURE_TRACK_HEIGHT_COLLAPSED
            }
            _ => self
                .global_lane_heights
                .get(kind)
                .unwrap_or_else(|| Self::global_lane_default_height(kind)),
        }
    }

    /// Arm a lane resize at pointer-down. Promoted to a live session on the
    /// first drag-move, so a plain click on the handle never resizes.
    pub fn arm_global_lane_resize(&mut self, kind: GlobalLaneKind, start_mouse_y: f32) {
        self.global_lane_resize_arm = Some((kind, start_mouse_y));
    }

    pub fn clear_global_lane_resize_arm(&mut self) {
        self.global_lane_resize_arm = None;
    }

    pub fn ensure_global_lane_resize_from_arm(&mut self, mouse_y: f32) -> bool {
        if self.global_lane_resize.is_some() {
            return true;
        }
        let Some((kind, start_y)) = self.global_lane_resize_arm.take() else {
            return false;
        };
        self.global_lane_resize = Some(GlobalLaneResizeSession {
            kind,
            start_height: self.global_lane_height(kind),
            start_custom_height: self.global_lane_heights.get(kind),
            start_mouse_y: start_y,
        });
        self.update_global_lane_resize(mouse_y)
    }

    pub fn update_global_lane_resize(&mut self, mouse_y: f32) -> bool {
        let Some(session) = self.global_lane_resize.clone() else {
            return false;
        };
        let next =
            clamp_global_lane_height(session.start_height + (mouse_y - session.start_mouse_y));
        if (next - self.global_lane_height(session.kind)).abs() < 0.01 {
            return false;
        }
        // Dragging a lane taller un-collapses it: the collapsed height is a
        // separate presentation, and leaving the flag set would snap the lane
        // back the moment the drag ended.
        match session.kind {
            GlobalLaneKind::Tempo => self.tempo_track_collapsed = false,
            GlobalLaneKind::TimeSignature => self.time_signature_track_collapsed = false,
            _ => {}
        }
        self.global_lane_heights.set(session.kind, Some(next));
        true
    }

    /// Abandon the gesture and put the lane back where it started.
    pub fn cancel_global_lane_resize(&mut self) -> bool {
        self.global_lane_resize_arm = None;
        let Some(session) = self.global_lane_resize.take() else {
            return false;
        };
        self.global_lane_heights
            .set(session.kind, session.start_custom_height);
        true
    }

    /// End the gesture. Returns the before/after height maps for one undo entry,
    /// or `None` when the lane ended up where it started.
    pub fn finish_global_lane_resize(&mut self) -> Option<(GlobalLaneHeights, GlobalLaneHeights)> {
        let session = self.global_lane_resize.take()?;
        let next = self.global_lane_heights.clone();
        let mut prev = next.clone();
        prev.set(session.kind, session.start_custom_height);
        if prev == next {
            return None;
        }
        Some((prev, next))
    }

    /// Double-click on the handle: back to the lane's default height.
    pub fn reset_global_lane_height(
        &mut self,
        kind: GlobalLaneKind,
    ) -> Option<(GlobalLaneHeights, GlobalLaneHeights)> {
        if self.global_lane_heights.get(kind).is_none() {
            return None;
        }
        let prev = self.global_lane_heights.clone();
        self.global_lane_heights.set(kind, None);
        Some((prev, self.global_lane_heights.clone()))
    }

    pub fn show_song_text_track_lane(&mut self) {
        self.show_song_text_track = true;
    }

    pub fn hide_song_text_track_lane(&mut self) {
        self.show_song_text_track = false;
    }

    /// Visible global/system lanes (Tempo then Time Signature when shown).
    pub fn visible_global_lanes(&self) -> Vec<GlobalLaneKind> {
        let mut lanes = Vec::new();
        if self.show_tempo_track {
            lanes.push(GlobalLaneKind::Tempo);
        }
        if self.show_time_signature_track {
            lanes.push(GlobalLaneKind::TimeSignature);
        }
        if self.show_song_text_track {
            lanes.push(GlobalLaneKind::SongText);
        }
        lanes
    }
}
