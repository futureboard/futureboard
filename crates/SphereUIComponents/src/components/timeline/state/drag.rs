use super::*;

#[derive(Debug, Clone)]
pub struct ClipDragItem {
    pub clip_id: String,
    pub source_track_id: String,
    pub start_beat: f32,
}

/// In-flight clip edge-resize drag payload (mirrors [`ClipDragItem`]). Carries
/// the clip identity, which edge is dragged, and the original bounds so the
/// handler can resolve the new length from the live cursor position.
///
/// Deliberately identity-only: the pre-gesture clip snapshot that the undo step
/// needs is captured by the timeline root on the first drag-move, before it
/// mutates anything (`Timeline::clip_resize_origin`). Carrying the whole
/// [`ClipState`] here instead meant cloning every note in the clip on each
/// repaint — the payload is built during element construction — and again on
/// each drag-move event.
#[derive(Debug, Clone)]
pub struct ClipResizeDrag {
    pub clip_id: String,
    pub edge: ClipEdge,
    pub start_beat: f32,
    pub duration_beats: f32,
}

#[derive(Debug, Clone)]
pub struct TrackDragItem {
    pub track_id: String,
    pub origin_index: usize,
    pub name: String,
    pub color: gpui::Rgba,
    pub is_group: bool,
}

/// In-flight track row height resize. Heights are resolved live from
/// [`TimelineState::update_track_height_resize`]; this payload only
/// carries identity + the gesture anchor.
#[derive(Debug, Clone)]
pub struct TrackHeightResizeDrag {
    pub anchor_track_id: String,
}

/// In-flight global (conductor) lane height resize. Identity only, for the
/// same reason as [`TrackHeightResizeDrag`]: the height is resolved live from
/// [`TimelineState::update_global_lane_resize`].
#[derive(Debug, Clone)]
pub struct GlobalLaneResizeDrag {
    pub kind: GlobalLaneKind,
}

/// How far the pointer must travel, in lane pixels, before a press on a
/// conductor-lane object becomes a move.
///
/// Without it every click on a flag is also a one-pixel nudge: the lanes seek
/// the playhead on press, so the pointer is already moving when the button
/// comes up.
pub const CONDUCTOR_DRAG_THRESHOLD_PX: f32 = 3.0;

/// In-flight arrangement-marker move on the global Marker lane.
///
/// This is a gesture *session* owned by the timeline root, not a GPUI
/// drag-and-drop payload. Nothing is being transferred to a drop target — the
/// flag follows the pointer inside its own lane — and the root's mouse-move
/// listener is the one path every other in-place timeline gesture already
/// shares (automation, range select, pen, tempo, meter). Markers used to be
/// the odd one out on `on_drag`, which is also the only conductor lane whose
/// move never worked.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineMarkerDrag {
    pub marker_id: String,
    /// Pointer x in lane pixels at mouse-down — the threshold is measured from
    /// here, not from the previous frame, so slow drags still arm.
    pub press_lane_x: f32,
    /// Beats between the marker's own beat and the grab point, so the flag
    /// keeps its offset under the cursor instead of snapping its left edge to
    /// it on the first move.
    pub grab_offset_beats: f64,
    /// Set once the marker has actually moved, so a plain click never marks the
    /// project dirty or writes an undo entry.
    pub moved: bool,
}
