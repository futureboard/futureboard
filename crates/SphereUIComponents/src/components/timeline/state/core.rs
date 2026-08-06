use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct TransportState {
    pub playing: bool,
    pub recording: bool,
    pub metronome_enabled: bool,
    pub playhead_beats: f32,
    pub loop_enabled: bool,
    pub loop_start_beats: f32,
    pub loop_end_beats: f32,
    pub last_engine_frame: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MasterBusState {
    pub volume: f32,
    /// Master-bus insert chain. Uses the same slot model as track inserts,
    /// owned by the synthetic `"master"` route in the engine graph.
    pub inserts: Vec<InsertSlotState>,
    pub meter_level_l: f32,
    pub meter_level_r: f32,
    /// Held peak levels (slow release) for the master peak-hold tick. UI-only.
    pub meter_peak_hold_l: f32,
    pub meter_peak_hold_r: f32,
    /// Latched master clip indicator. UI-only.
    pub meter_clip: bool,
}

/// Which signal the Control Room monitors. Mirrors the engine's
/// `MonitorSource`; the engine remains authoritative for routing.
///
/// The default is [`MonitorSourceKind::MasterBus`] — the complete internal mix
/// (audio tracks, instruments, aux/returns, group buses, master processing).
/// A hardware input is selectable but never the default, and selecting it is
/// what makes the engine read a capture device at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MonitorSourceKind {
    #[default]
    MasterBus,
    Bus(String),
    TrackPreFader(String),
    TrackAfterFader(String),
    HardwareInput(String),
}

impl MonitorSourceKind {
    /// Stable tag shared with the engine.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::MasterBus => "master",
            Self::Bus(_) => "bus",
            Self::TrackPreFader(_) => "track-pfl",
            Self::TrackAfterFader(_) => "track-afl",
            Self::HardwareInput(_) => "hardware-input",
        }
    }

    pub fn target_id(&self) -> Option<&str> {
        match self {
            Self::MasterBus => None,
            Self::Bus(id)
            | Self::TrackPreFader(id)
            | Self::TrackAfterFader(id)
            | Self::HardwareInput(id) => Some(id.as_str()),
        }
    }
}

/// Control Room bus — the monitoring path fed from the master bus, shown as its
/// own pinned mixer strip.
///
/// Everything here affects playback monitoring only. None of it reaches the
/// master mix, an offline export, a stem export, or recorded audio: the engine
/// applies the Control Room inside the device callback, which export never
/// enters. This is session/hardware state, so it is deliberately not persisted
/// with the project and never marks it dirty.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorBusState {
    /// What the Control Room listens to when no channel PFL/AFL is engaged.
    pub source: MonitorSourceKind,
    /// Human-readable name of [`Self::source`], resolved against the project
    /// when the selection changes.
    pub source_display: String,
    /// Name of the hardware output pair the Control Room feeds.
    pub output_name: String,
    /// 0-based left channel of that pair on the active output device.
    pub output_left_channel: u16,
    /// Output pairs the active device can offer.
    pub available_outputs: Vec<(String, u16)>,
    /// Monitor level as a normalized fader position.
    pub volume: f32,
    pub mute: bool,
    /// -20 dB monitoring reference cut.
    pub dim: bool,
    /// Fold to mono for mono-compatibility checks.
    pub mono: bool,
    /// Post-monitor-processing level actually leaving for the monitoring
    /// output — after monitor inserts and the control processor. UI-only.
    pub meter_level_l: f32,
    pub meter_level_r: f32,
    pub meter_peak_hold_l: f32,
    pub meter_peak_hold_r: f32,
    pub meter_clip: bool,
    /// True while any channel has PFL/AFL engaged, so the strip can show that
    /// the Control Room is on a Listen tap rather than its selected source.
    pub listen_active: bool,
}

impl MonitorBusState {
    /// Short label for the current source, shown in the Source selector.
    /// Resolved from the project when the source is set, so the chip shows a
    /// bus name rather than an internal track id.
    pub fn source_label(&self) -> String {
        self.source_display.clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineState {
    pub bpm: f32,
    /// Project-level tempo automation. Always active and owned by the project;
    /// the TempoTrack (when shown) is only a view/editor over this. When empty
    /// the project plays at the static `bpm`.
    pub tempo_map: TempoMap,
    /// Global time signature markers (authoritative for bar/beat layout).
    pub time_signature_map: TimeSignatureMap,
    /// Timeline markers shown on the arrangement ruler.
    pub markers: Vec<TimelineMarkerState>,
    /// Named timeline regions spanning a beat range.
    pub regions: Vec<TimelineRegionState>,
    /// Project-owned chord, lyric, and section events in canonical beat space.
    pub song_text_events: Vec<SongTextEvent>,
    /// UI lookup accelerator rebuilt whenever `song_text_events` changes.
    pub song_text_index: SongTextIndex,
    /// Non-persisted mutation revision used by virtualized Song Text views.
    pub song_text_revision: u64,
    /// Legacy single signature — kept in sync with the marker at beat 0 for
    /// templates and engine fallbacks.
    pub time_signature_num: u32,
    pub time_signature_den: u32,
    pub viewport: TimelineViewport,
    pub transport: TransportState,
    pub tracks: Vec<TrackState>,
    pub master: MasterBusState,
    /// Project Audio Connections — the single source of truth for logical
    /// audio input/output buses. Tracks reference entries by stable id; nothing
    /// outside this registry maps a bus to physical ports.
    pub audio_connections: crate::audio_connections::AudioConnectionRegistry,
    /// Input-monitoring bus rendered as the pinned Monitor strip. Session
    /// state — see [`MonitorBusState`].
    pub monitor: MonitorBusState,
    pub selection: TimelineSelection,
    pub active_tool: TimelineTool,
    pub snap_to_grid: bool,
    pub grid_division: SnapDivision,
    /// Straight / dotted / triplet shaping applied on top of [`Self::grid_division`].
    pub snap_shape: SnapShape,
    pub dragging_track_id: Option<TrackId>,
    pub drag_origin_index: Option<usize>,
    pub drag_current_y: f32,
    pub drag_target_index: Option<usize>,
    /// True when the timeline viewport should follow the playhead during
    /// playback. Toggled off temporarily when the user manually scrolls or
    /// drags the viewport; can be re-enabled from the Follow button.
    pub follow_playhead: bool,
    pub auto_scroll_mode: AutoScrollMode,
    /// Arrangement time-range selection in beats. UI-only; never marks the
    /// project or engine dirty by itself.
    pub arrangement_range: Option<TimelineRangeSelection>,
    /// When true, the global Tempo Track lane is shown below the ruler.
    pub show_tempo_track: bool,
    /// Compact collapsed height for the Tempo Track lane header/curve.
    pub tempo_track_collapsed: bool,
    /// Selected tempo marker on the Tempo Track (stable persisted id).
    pub selected_tempo_point_id: Option<String>,
    pub show_time_signature_track: bool,
    pub time_signature_track_collapsed: bool,
    pub selected_time_signature_point_id: Option<String>,
    /// Per-track arrangement row heights (layout/view state, persisted in project).
    pub track_view_layout: TrackViewLayout,
    /// Active track-height resize gesture, if any.
    pub track_height_resize: Option<TrackHeightResizeSession>,
    /// Armed at pointer-down on a resize handle; promoted to
    /// [`Self::track_height_resize`] on the first drag-move delta.
    pub track_height_resize_arm: Option<(String, f32, bool, bool)>,
    /// Last VST3 parameter touched inside a plugin editor (UI-only, not saved).
    pub last_touched_plugin_param: Option<LastTouchedPluginParam>,
    /// Mixer tree sidebar — expanded nodes, pins, hidden channels (persisted).
    pub mixer_tree: MixerTreeViewState,
    /// Transient fader values while the user drags. UI-only: these do not mark the
    /// project dirty, enter undo history, or trigger engine graph sync. Both the
    /// arrangement track headers and mixer strips render from this cache so they
    /// stay visually locked during a drag.
    pub track_volume_previews: std::collections::HashMap<TrackId, f32>,
    /// Base volume captured at fader pointer-down so commit can emit one undo entry.
    pub track_volume_gesture_origin: std::collections::HashMap<TrackId, f32>,
    pub master_volume_preview: Option<f32>,
    /// Base master volume captured at fader pointer-down for one undo entry.
    pub master_volume_gesture_origin: Option<f32>,
    /// Base pan captured at pointer-down so one completed scrub becomes one
    /// undo entry. Preview updates remain transient until release.
    pub track_pan_gesture_origin: std::collections::HashMap<TrackId, f32>,
}

impl Default for TimelineState {
    /// Clean, empty project. No tracks, no clips, no MIDI — the real runtime
    /// startup state. Use [`TimelineState::demo_project`] when you explicitly
    /// want the seeded demo content (development / screenshots).
    fn default() -> Self {
        Self {
            bpm: 120.0,
            tempo_map: TempoMap::new(),
            time_signature_map: TimeSignatureMap::with_default_4_4(),
            markers: Vec::new(),
            regions: Vec::new(),
            song_text_events: Vec::new(),
            song_text_index: SongTextIndex::default(),
            song_text_revision: 0,
            time_signature_num: 4,
            time_signature_den: 4,
            viewport: TimelineViewport {
                scroll_x: 0.0,
                scroll_y: 0.0,
                target_scroll_x: 0.0,
                target_scroll_y: 0.0,
                pixels_per_second: 150.0,
                pixels_per_beat: 75.0,
                viewport_width: 0.0,
                viewport_height: 500.0,
                track_area_height: 500.0,
                panel_origin_x: 0.0,
            },
            transport: TransportState {
                playing: false,
                recording: false,
                metronome_enabled: false,
                playhead_beats: 0.0,
                loop_enabled: false,
                loop_start_beats: 0.0,
                loop_end_beats: 16.0,
                last_engine_frame: 0,
            },
            tracks: Vec::new(),
            audio_connections: crate::audio_connections::AudioConnectionRegistry::new(),
            master: MasterBusState {
                volume: volume::db_to_norm(0.0),
                inserts: Vec::new(),
                meter_level_l: 0.0,
                meter_level_r: 0.0,
                meter_peak_hold_l: 0.0,
                meter_peak_hold_r: 0.0,
                meter_clip: false,
            },
            monitor: MonitorBusState {
                source: MonitorSourceKind::MasterBus,
                source_display: "Master Bus".to_string(),
                output_name: "Out 1-2".to_string(),
                output_left_channel: 0,
                available_outputs: vec![("Out 1-2".to_string(), 0)],
                volume: volume::db_to_norm(0.0),
                mute: false,
                dim: false,
                mono: false,
                meter_level_l: 0.0,
                meter_level_r: 0.0,
                meter_peak_hold_l: 0.0,
                meter_peak_hold_r: 0.0,
                meter_clip: false,
                listen_active: false,
            },
            selection: TimelineSelection {
                selected_track_id: None,
                selected_track_ids: Vec::new(),
                track_selection_anchor_id: None,
                selected_clip_ids: Vec::new(),
                selected_song_text_event_ids: Vec::new(),
            },
            active_tool: TimelineTool::Pointer,
            snap_to_grid: true,
            grid_division: SnapDivision::Div1_16,
            snap_shape: SnapShape::Straight,
            dragging_track_id: None,
            drag_origin_index: None,
            drag_current_y: 0.0,
            drag_target_index: None,
            follow_playhead: true,
            auto_scroll_mode: AutoScrollMode::Page,
            arrangement_range: None,
            show_tempo_track: false,
            tempo_track_collapsed: false,
            selected_tempo_point_id: None,
            show_time_signature_track: false,
            time_signature_track_collapsed: false,
            selected_time_signature_point_id: None,
            track_view_layout: TrackViewLayout::default(),
            track_height_resize: None,
            track_height_resize_arm: None,
            last_touched_plugin_param: None,
            mixer_tree: MixerTreeViewState::default(),
            track_volume_previews: std::collections::HashMap::new(),
            track_volume_gesture_origin: std::collections::HashMap::new(),
            master_volume_preview: None,
            master_volume_gesture_origin: None,
            track_pan_gesture_origin: std::collections::HashMap::new(),
        }
    }
}

// ── Time conversions and coordinate helpers ───────────────────────────────────────

pub const HEADER_WIDTH: f32 = 320.0; // Keep it slightly wider for native controls

pub const RULER_HEIGHT: f32 = 30.0;

pub type TrackId = String;

impl TimelineState {
    pub fn seconds_per_beat(&self) -> f32 {
        60.0 / self.bpm.max(1.0)
    }

    pub fn seconds_to_beats(&self, seconds: f64) -> f32 {
        (seconds * self.bpm.max(1.0) as f64 / 60.0) as f32
    }

    pub fn beats_to_seconds(&self, beats: f32) -> f32 {
        beats * self.seconds_per_beat()
    }

    /// Y offset from the timeline top to the track-list content area.
    pub fn arrangement_content_top(&self) -> f32 {
        RULER_HEIGHT + self.global_lanes_height()
    }
}
