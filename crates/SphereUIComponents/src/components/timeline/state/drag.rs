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
#[derive(Debug, Clone)]
pub struct ClipResizeDrag {
    pub clip_id: String,
    pub edge: ClipEdge,
    pub start_beat: f32,
    pub duration_beats: f32,
    /// Complete pre-gesture snapshot so trim can create one exact undo step.
    pub original: ClipState,
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
