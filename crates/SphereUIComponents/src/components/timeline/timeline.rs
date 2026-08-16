mod methods;
mod render;
pub(crate) use render::*;

use crate::assets;
use crate::components::edit::{
    normalize_range, ClipSnapshot, EditCommand, EditHistory, TrackSnapshot,
};
use crate::components::sidebar::BrowserDragItem;
use crate::components::timeline::floating_tools_bar::floating_tools_bar;
use crate::components::timeline::song_text_track::{
    song_text_drag_positions, song_text_track_lane, SongTextDragPreview, SongTextDragSession,
    SongTextMarkerDown,
};
use crate::components::timeline::tempo_track::tempo_track_lane;
use crate::components::timeline::time_signature_track::time_signature_track_lane;
use crate::components::timeline::timeline_ruler::{
    timeline_ruler, TimelineLoopDragUpdate, TimelineRegionDragUpdate,
};
use crate::components::timeline::timeline_state::{
    hit_test_arrangement, ArrangementCoordinateContext, ArrangementHitTarget, ClipDragItem,
    ClipResizeDrag, ClipState, ClipType, SnapDivision, TempoPointDrag, TimeSignaturePointDrag,
    TimelineRangeSelection, TimelineState, TimelineTool, TrackDragItem, TrackHeightResizeDrag,
    TrackType, DEFAULT_TRACK_HEIGHT, HEADER_WIDTH, RULER_HEIGHT, TEMPO_LANE_PAD,
};
use crate::components::timeline::track_list::track_list;
use crate::theme::Colors;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, pulsating_between, px, svg, Animation, AnimationExt, AppContext, Context, Empty,
    ExternalPaths, InteractiveElement, IntoElement, ParentElement, PinchEvent, Render, ScrollDelta,
    StatefulInteractiveElement, Styled, Subscription, Window,
};
use std::time::Duration;

/// App chrome (top titlebar/menu strip) — used to convert window-space y into
/// the timeline track area. Mirrors the value used by app_chrome.
const APP_CHROME_HEIGHT: f32 = 36.0;
const MARQUEE_DRAG_THRESHOLD: f32 = 4.0;

/// Sizes of the surrounding chrome panels that the timeline's scroll/grid
/// math has to subtract from the window to know the actual timeline body
/// rect. Pushed by `StudioLayout` each render so resizing the bottom
/// panel, toggling browser/inspector, and maximizing the window all stay
/// in sync — no hardcoded constants.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimelineChromeMetrics {
    pub browser_width: f32,
    pub inspector_width: f32,
    pub bottom_panel_height: f32,
    pub status_bar_height: f32,
}

/// Live pen-tool MIDI clip draw. Held only while the gesture is in flight
/// (mouse-down → mouse-up); the real clip is created once on release. `start_beat`
/// is snapped at mouse-down; `current_beat` tracks the snapped cursor while
/// dragging so the ghost preview and the committed clip share one set of bounds.
#[derive(Clone, Debug)]
struct ClipDrawPreview {
    track_id: String,
    start_beat: f32,
    current_beat: f32,
    /// `true` once the cursor has moved past the start — distinguishes a plain
    /// click (default-length clip) from a drag (sized clip).
    dragging: bool,
    /// Whether a plain click (no drag) should still commit a default-length
    /// clip. Always `true` for the Pen tool. For the Pointer tool's
    /// empty-lane-creates-a-clip gesture, a plain single click is a no-op
    /// (matches modern DAW marquee-vs-create conventions) — only a drag or a
    /// double-click (`click_count >= 2`) commits.
    commit_on_click: bool,
}

#[derive(Clone, Debug)]
struct RangeSelectDrag {
    start_beat: f32,
    current_beat: f32,
    start_track_id: String,
    additive: bool,
    dragging: bool,
}

#[derive(Clone, Debug)]
struct FileDropHint {
    position: gpui::Point<gpui::Pixels>,
    label: &'static str,
}

/// UI-only ghost for Alt-drag clip cloning. The real duplicate is still
/// created only on drop; this state exists solely to show its exact target
/// track and snapped start before committing the edit command.
#[derive(Clone, Debug)]
struct ClipCloneHint {
    clip_id: String,
    target_track_index: usize,
    start_beat: f32,
}

fn is_supported_audio_ext(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("wav")
            | Some("wave")
            | Some("mp3")
            | Some("flac")
            | Some("ogg")
            | Some("oga")
            | Some("m4a")
            | Some("aiff")
            | Some("aif")
    )
}

fn is_supported_midi_ext(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("mid") | Some("midi")
    )
}

use std::collections::HashSet;

pub struct Timeline {
    pub state: TimelineState,
    edit_history: EditHistory,
    /// Exact clip snapshot captured at Inspector scrub start. Preview changes
    /// stay out of history; release records one `UpdateClip` command.
    inspector_clip_gesture_origin: Option<ClipSnapshot>,
    on_seek_beats:
        Option<std::sync::Arc<dyn Fn(f32, f32, crate::layout::SeekReason) + Send + Sync + 'static>>,
    on_track_param_change:
        Option<std::sync::Arc<dyn Fn(String, String, f32) + Send + Sync + 'static>>,
    on_track_input_state_change: Option<
        std::sync::Arc<dyn Fn(String, bool, bool) -> Result<(), String> + Send + Sync + 'static>,
    >,
    on_project_changed: Option<TimelineProjectChangedCb>,
    /// Fired for persisted MIDI edits so the owner can publish the new note
    /// schedule immediately rather than waiting for gesture throttling.
    on_midi_changed: Option<TimelineProjectChangedCb>,
    /// Fired for live mixer-control edits (track mute/solo from the header)
    /// that are persisted in the project but reach the engine through the
    /// realtime command path — the owner marks view-only dirty instead of the
    /// full engine-graph dirty that `on_project_changed` implies.
    on_control_state_changed: Option<TimelineProjectChangedCb>,
    on_loop_changed: Option<TimelineProjectChangedCb>,
    on_tempo_map_changed: Option<TimelineProjectChangedCb>,
    on_time_signature_map_changed: Option<TimelineProjectChangedCb>,
    on_media_changed: Option<TimelineProjectChangedCb>,
    on_add_track: Option<TimelineAddTrackCb>,
    on_plugin_preset_drop: Option<TimelinePluginPresetDropCb>,
    /// Asks the owner to confirm what a dropped MIDI file should bring in
    /// besides its notes. Unset (tests, embedded editors) imports everything,
    /// which is what a drop did before the dialog existed.
    on_midi_import_prompt: Option<TimelineMidiImportPromptCb>,
    /// Window-space position of the last drag-move event while files are
    /// being dragged. We need this because `on_drop::<ExternalPaths>` does
    /// not carry the drop position itself — gpui translates the submit into
    /// a synthetic MouseUp, so we have to remember the last cursor position
    /// observed during the drag.
    last_drag_position: Option<gpui::Point<gpui::Pixels>>,
    file_drop_hint: Option<FileDropHint>,
    clip_clone_hint: Option<ClipCloneHint>,
    /// UI-only Song Text positions calculated from the drag-start snapshot.
    song_text_drag_preview: Option<SongTextDragPreview>,
    /// Blocks further drag-move/drop events after Escape or focus-loss cancellation.
    song_text_drag_cancelled: bool,
    clip_drag_origin: Option<gpui::Point<gpui::Pixels>>,
    clip_drag_target_track_index: Option<usize>,
    clip_clone_drag_id: Option<String>,
    /// Pen-tool click-drag MIDI clip preview, live until mouse-up creates the clip.
    pen_clip_draw: Option<ClipDrawPreview>,
    /// Pointer-tool empty-lane marquee. Rule: Pointer + empty lane drag starts
    /// replace-marquee; Ctrl/Cmd + Pointer + empty lane drag starts additive
    /// marquee. Clips, rulers, toolbar controls, and non-pointer tools never
    /// start this gesture.
    range_select_drag: Option<RangeSelectDrag>,
    /// Right-drag erase: clip ids already queued for deletion this gesture.
    erase_clip_drag: Option<HashSet<String>>,
    /// Live preview of clip ids marked for erase (mirrors `erase_clip_drag`).
    erase_preview_ids: HashSet<String>,
    /// In-flight automation point move. Mutated live; committed once on release.
    automation_drag: Option<crate::components::timeline::timeline_state::AutomationPointDrag>,
    /// In-flight automation curve-tension drag (Alt+drag on a segment). Mutated
    /// live; committed once on release. Never moves the points themselves.
    automation_curve_drag: Option<crate::components::timeline::timeline_state::AutomationCurveDrag>,
    /// In-flight automation marquee (rubber-band) selection. UI-only.
    automation_marquee: Option<crate::components::timeline::timeline_state::AutomationMarquee>,
    /// Hovered automation point / curve segment under the cursor. UI-only; drives
    /// the per-segment highlight + hover cursor. Self-corrects on mouse-move and
    /// is cleared on hover-out, so it is never persisted or reset on gesture end.
    automation_hover: Option<crate::components::timeline::timeline_state::AutomationHover>,
    /// Automation control-lane actions that need studio-level handling (picker,
    /// clear-all confirmation, last-touched). Set by `StudioLayout`.
    on_automation_control:
        Option<crate::components::timeline::automation_control_lane::AutomationControlCallback>,
    /// In-flight tempo-point drag on the global Tempo Track lane.
    tempo_drag: Option<TempoPointDrag>,
    /// In-flight time-signature marker drag on the global Time Signature lane.
    ts_drag: Option<TimeSignaturePointDrag>,
    pan_last_position: Option<gpui::Point<gpui::Pixels>>,
    /// View-only floating-toolbar placement. It deliberately never enters the
    /// project snapshot: moving tools must not make a session dirty.
    floating_toolbar_position: Option<(f32, f32)>,
    floating_toolbar_drag_anchor: Option<(gpui::Point<gpui::Pixels>, (f32, f32))>,
    on_context_menu: Option<TimelineContextMenuCb>,
    on_playhead_scrub_begin:
        Option<std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + Send + Sync + 'static>>,
    on_playhead_scrub_end:
        Option<std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + Send + Sync + 'static>>,
    /// Invoked when the user double-clicks a MIDI clip — `StudioLayout` uses it
    /// to switch the bottom panel to the piano-roll Editor tab.
    on_open_editor: Option<TimelineOpenEditorCb>,
    /// Invoked when the user double-clicks a Song Text marker.
    on_open_song_text_editor: Option<TimelineOpenEditorCb>,
    chrome_metrics: TimelineChromeMetrics,
    /// Absolute root folder of the saved project, pushed by `StudioLayout` each
    /// render. `None` for an Untitled (unsaved) project. Used to eagerly copy
    /// dropped audio into the project's `Assets/Audio` folder.
    project_root: Option<std::path::PathBuf>,
    focus_lost_subscription: Option<Subscription>,
}

pub type TimelineOpenEditorCb = std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static>;

#[derive(Clone, Debug)]
pub enum TimelineContextTarget {
    TimelineEmpty,
    TrackLane {
        track_id: String,
        beat: f64,
    },
    TrackHeader(String),
    AudioClip {
        track_id: String,
        clip_id: String,
        beat: f64,
        local_beat: f64,
    },
    MidiClip {
        track_id: String,
        clip_id: String,
        beat: f64,
        local_beat: f64,
    },
    Clip(String),
    Marker {
        marker_id: String,
        beat: f64,
    },
    SongTextMarker {
        event_id: String,
        beat: f64,
    },
    AutomationLane {
        track_id: String,
        lane_id: String,
        beat: f64,
    },
    /// Right-click on the arrangement ruler. Carries the beat under the cursor.
    Ruler(f64),
    /// Right-click on the global Tempo Track lane.
    TempoTrack {
        beat: f64,
        bpm: f64,
        point_id: Option<String>,
    },
    /// Right-click on the global Time Signature Track lane.
    TimeSignatureTrack {
        beat: f64,
        point_id: Option<String>,
    },
    /// Lane header menu button on the Tempo track.
    TempoLaneHeader,
    /// Lane header menu button on the Time Signature track.
    TimeSignatureLaneHeader,
    /// Automation target picker opened from the control lane "+ Add" button.
    AutomationTargetPicker {
        track_id: String,
    },
}

pub type TimelineContextMenuCb = std::sync::Arc<
    dyn Fn(&(TimelineContextTarget, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

#[derive(Clone, Copy, Debug)]
pub struct TimelineAddTrackRequest {
    pub track_count: usize,
    pub has_master_track: bool,
}

pub type TimelineAddTrackCb =
    std::sync::Arc<dyn Fn(&TimelineAddTrackRequest, &mut gpui::Window, &mut gpui::App) + 'static>;

pub type TimelinePluginPresetDropCb = std::sync::Arc<
    dyn Fn(&(std::path::PathBuf, String), &mut gpui::Window, &mut gpui::App) + 'static,
>;

/// A dropped MIDI file that carries optional payload (markers, controller
/// lanes, SysEx), parked until the user says what to bring in.
///
/// Carries the resolved lane coordinates rather than relying on
/// `last_drag_position`, which is cleared as soon as the drop is handled — the
/// clip still has to land where it was dropped when the dialog closes.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineMidiImportPrompt {
    pub path: std::path::PathBuf,
    pub file_name: String,
    pub summary: crate::components::timeline::midi_import::MidiImportSummary,
    pub drop_x: f32,
    pub drop_y: f32,
}

pub type TimelineMidiImportPromptCb =
    std::sync::Arc<dyn Fn(&TimelineMidiImportPrompt, &mut gpui::Window, &mut gpui::App) + 'static>;

pub type TimelineProjectChangedCb = std::sync::Arc<dyn Fn(&mut gpui::App) + 'static>;

#[derive(Clone, Debug)]
struct ScrollbarDrag {
    axis: ScrollAxis,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScrollAxis {
    Horizontal,
    Vertical,
}

impl Render for ScrollbarDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

// Clip edge-resize uses GPUI's drag system with no visible drag image, so the
// payload renders as `Empty` (same as the scrollbar thumb drag).
impl Render for ClipResizeDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl Render for TrackHeightResizeDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

// ── Timeline scrollbars ─────────────────────────────────────────────────
//
// Both scrollbars are rendered as absolute overlays on top of the
// arrangement area. The thumb is sized by `viewport / content` and
// positioned by `scroll / max_scroll`. Mouse-down on the track jumps
// the scroll position so the click point becomes the new thumb top
// (vertical) or thumb left (horizontal). The wheel handler on the
// Timeline div continues to handle smooth scrolling and zoom; the
// scrollbar is the visible indicator + a coarse jump target.

const SCROLLBAR_THICKNESS: f32 = 8.0;
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
