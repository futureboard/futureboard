pub mod edit_commands;
pub mod edit_interaction;

pub use edit_commands::{
    ClipSnapshot, EditCommand, EditHistory, EditImpact, TempoStateSnapshot,
    TimeSignatureStateSnapshot, TrackSnapshot,
};
pub use edit_interaction::{
    normalize_range, pointer_intent_on_empty, rects_intersect, EditTool, PointerEditIntent,
    EDIT_DRAG_THRESHOLD_PX,
};
