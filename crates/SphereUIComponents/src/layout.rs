use gpui::{
    div, px, AppContext, Bounds, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, Window, WindowHandle,
};

pub use crate::shutdown::ShutdownState;
pub use close_ops::PendingCloseAction;
pub use project_ops::ProjectOpenOptions;
pub use project_switch::{ProjectSwitchConfirmDecision, ProjectSwitchRequest, ProjectSwitchSource};
pub use session_load::PreparedWorkspaceFinish;

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use crate::components;
use crate::components::add_track_dialog::AddTrackKind;
use crate::components::edit::ClipSnapshot;
use crate::components::file_browser::FileBrowserState;
use crate::components::plugin_picker::{
    compute_filter_result, ensure_default_highlight, plugin_picker_overlay,
    CatalogStatus as PluginCatalogStatus, PickerFilter, PluginPickerCallbacks, PluginPickerPrefs,
    PluginPickerScrollHandles, PluginPickerState, PluginSearchIndex,
};
use crate::components::project_switcher::ProjectSwitcherState;
use crate::components::text_input::{
    text_input_context_entries, TextInputCallbacks, TextInputState,
};
use crate::components::timeline::timeline::TimelineContextTarget;
use crate::components::timeline::timeline_state::{ClipType, TempoCurve};
use crate::components::BottomPanelState;
use crate::components::MixerWindow;
use crate::components::{BackgroundTaskStore, CommandPaletteState};
use crate::overlay::{project_title_anchor, titlebar_label_anchor};
use crate::paths::FutureboardPaths;
use crate::project::recent::RecentProjectsStore;
use crate::settings::{GlobalSettingsModel, SettingsModel};
use crate::theme::{self, Colors};
use SpherePluginHost::load_au_cache_state;

mod audio_transport;
mod bottom_panel_ops;
mod browser_ops;
mod close_ops;
mod context_menu_ops;
pub(crate) mod engine_snapshot;
mod export_ops;
mod frame_diagnostics;
mod helpers;
mod input_ops;
mod inspector_ops;
mod midi_input_router;
mod mixer_ops;
mod stem_extract_ops;
pub(crate) use mixer_ops::clone_track_for_mixer;
pub(crate) mod plugin_bridge_runtime;
mod plugin_ops;
mod plugin_picker_window;
mod plugin_restore;
mod project_ops;
mod project_switch;
mod recording_ops;
mod sample_rate_ops;
mod session_load;
mod stretch_tempo_ops;
mod studio_render;
mod studio_state;
mod track_clip_ops;
mod transport_freeze_debug;
mod transport_ops;
mod window_ops;

pub use audio_transport::SeekReason;
pub use context_menu_ops::{ContextMenuRequest, ContextMenuTarget};
use engine_snapshot::volume_norm_to_linear;
use frame_diagnostics::FrameDiagnostics;
use helpers::{
    edit_command_debug, find_clip_summary, is_midi_routable_edit_command, is_supported_audio_ext,
    is_tap_tempo_command, is_text_input_key, key_debug, normalize_command_id, reveal_path,
    should_handle_global_transport_shortcut, transport_command_from_id, FocusContext,
};
use project_ops::LifecycleAction;
pub use studio_state::{
    ContextTarget, MenuBarUiState, OpenPopover, RightDockTab, StudioPanelVisibility,
    WorkspaceActivePanel,
};
use studio_state::{TextContextMenu, TextMenuTarget, TransportCommand};

/// Demo content is opt-in only. The real runtime starts empty and renders
/// project state loaded or created by the user.
fn use_demo_project() -> bool {
    std::env::var_os("FUTUREBOARD_DEMO_PROJECT").is_some_and(|value| value == "1")
}

/// Map the saved Settings renderer choice onto the process-wide timeline
/// renderer preference. Idempotent — the underlying setters use `OnceLock`, so
/// it's safe to call at app launch and again when the studio is built.
fn apply_renderer_preference(schema: &crate::settings::SettingsSchema) {
    use crate::components::timeline::render::{
        set_preferred_backend, set_preferred_gpu_device_id, TimelineRendererBackend,
    };
    use crate::settings::RenderMode;
    let chosen = match schema.performance.render_mode {
        RenderMode::Auto | RenderMode::CpuRender => TimelineRendererBackend::GpuiPaint,
        #[cfg(feature = "gpu-renderer")]
        RenderMode::GpuAcceleration => TimelineRendererBackend::Wgpu,
        #[cfg(not(feature = "gpu-renderer"))]
        RenderMode::GpuAcceleration => TimelineRendererBackend::GpuiPaint,
    };
    set_preferred_backend(chosen);

    // Mixer primitive layer. Slice 1: only the explicit "GPU (Experimental)"
    // choice turns the batched-`canvas` path on; Auto stays on the legacy `div`
    // mixer until the GPU path is visually verified. The backend itself is always
    // GPUI-paint today (offscreen WGPU is parked / falls back).
    {
        use crate::components::mixer_render::{set_preferred_mixer_backend, MixerRendererBackend};
        use crate::components::mixer_surface::set_mixer_gpu_primitives_enabled;
        let mixer_backend = match schema.performance.render_mode {
            #[cfg(feature = "gpu-renderer")]
            RenderMode::GpuAcceleration => MixerRendererBackend::Wgpu,
            _ => MixerRendererBackend::GpuiPaint,
        };
        set_preferred_mixer_backend(mixer_backend);
        set_mixer_gpu_primitives_enabled(matches!(
            schema.performance.render_mode,
            RenderMode::GpuAcceleration
        ));
    }
    // Saved GPU device id (empty string == Auto).
    let device_id = match &schema.performance.gpu_device {
        crate::settings::GpuDevicePreference::Auto => "",
        crate::settings::GpuDevicePreference::DeviceId(id) => id.as_str(),
    };
    set_preferred_gpu_device_id(device_id);
    if std::env::var_os("FUTUREBOARD_GPU_RENDERER_DEBUG").is_some() {
        eprintln!(
            "[gpu-renderer] startup: render_mode={:?} gpu_device={:?}",
            schema.performance.render_mode, schema.performance.gpu_device
        );
    }
}

/// Load saved settings and apply the renderer preference early — before the
/// studio window exists — so the GPU renderer can be warmed on the loading
/// screen instead of stalling the first studio frame. Called at app launch.
pub fn apply_saved_renderer_preference(cx: &mut gpui::App) {
    let settings = SettingsModel::load_or_create(cx);
    let schema = settings.read(cx).current.clone();
    apply_renderer_preference(&schema);
}

/// Eagerly initialize the timeline renderer (creating the GPU adapter/device
/// for the WGPU backend) on the current thread. Call on the main UI thread
/// during the loading screen, after [`apply_saved_renderer_preference`].
/// Returns the active backend label for status display.
pub fn warm_up_renderer() -> &'static str {
    crate::components::timeline::timeline_surface::warm_up_timeline_renderer()
}

/// Outcome of an early renderer warm-up: what backend was requested vs. what is
/// actually active, so the Welcome screen can report "GPU ready" vs. a CPU
/// fallback honestly (Part A).
#[derive(Debug, Clone, Copy)]
pub struct RendererWarmup {
    pub backend_label: &'static str,
    /// The user/preference asked for the GPU (WGPU) backend.
    pub gpu_requested: bool,
    /// The GPU backend is actually active (adapter/device created OK).
    pub gpu_active: bool,
}

#[derive(Debug, Clone, Default)]
struct ShortcutDiagnostics {
    last_key_event: String,
    last_key_target: String,
    last_key_consumed_by: String,
    focused_widget_kind: String,
    is_text_editing_context: bool,
    transport_toggle_shortcut_count: u64,
}

impl RendererWarmup {
    /// Status text for the Welcome renderer row.
    pub fn status_text(&self) -> &'static str {
        if self.gpu_active {
            "GPU ready"
        } else if self.gpu_requested {
            "CPU fallback"
        } else {
            "CPU render"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingUiState {
    Idle,
    Preparing,
    CountingIn { bars: u32 },
    Recording,
    Finalizing,
    Failed { reason: String },
}

impl RecordingUiState {
    fn status_text(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Preparing => Some("Recording: preparing...".to_string()),
            Self::CountingIn { bars } => Some(format!("Recording: count-in {bars} bars")),
            Self::Recording => Some("Recording".to_string()),
            Self::Finalizing => Some("Recording: finalizing...".to_string()),
            Self::Failed { reason } => Some(format!("Recording failed: {reason}")),
        }
    }
}

/// Warm the renderer and report whether the GPU backend was requested and
/// whether it came up. Logs start/end (and any fallback) under
/// `FUTUREBOARD_GPU_RENDERER_DEBUG=1`. Non-fatal: a failed GPU init falls back
/// to CPU paint inside [`warm_up_renderer`].
pub fn warm_up_renderer_status() -> RendererWarmup {
    use crate::components::timeline::render::TimelineRendererBackend;

    let preferred = TimelineRendererBackend::from_env();
    #[cfg(feature = "gpu-renderer")]
    let gpu_requested = matches!(preferred, TimelineRendererBackend::Wgpu);
    #[cfg(not(feature = "gpu-renderer"))]
    let gpu_requested = false;

    let gpu_debug = std::env::var_os("FUTUREBOARD_GPU_RENDERER_DEBUG").is_some();
    if gpu_debug {
        eprintln!(
            "[gpu-renderer] warm-up start (requested backend={})",
            preferred.label()
        );
    }

    let backend_label = warm_up_renderer();

    #[cfg(feature = "gpu-renderer")]
    let gpu_active = backend_label == TimelineRendererBackend::Wgpu.label();
    #[cfg(not(feature = "gpu-renderer"))]
    let gpu_active = false;

    if gpu_debug {
        if gpu_requested && !gpu_active {
            eprintln!("[gpu-renderer] warm-up end: GPU requested but fell back to CPU paint");
        } else {
            eprintln!(
                "[gpu-renderer] warm-up end: backend={backend_label} gpu_active={gpu_active}"
            );
        }
    }

    RendererWarmup {
        backend_label,
        gpu_requested,
        gpu_active,
    }
}

/// Notify a satellite window's root view without calling `Entity::update` (which
/// can re-enter the main studio entity and trip GPUI's lease checks).
pub(crate) fn notify_window_root<T: gpui::Render>(app: &mut gpui::App, handle: &WindowHandle<T>) {
    if let Ok(entity) = handle.entity(app) {
        app.notify(entity.entity_id());
    }
}

/// Map a persisted `driver_type` string to an engine backend.
///
/// `settings.json` is user-editable, so the mapped backend is sanitized rather
/// than trusted: a driver this build cannot drive (e.g. `"ASIO"` with no
/// licensed ASIO host registered) falls back to `Auto` instead of reaching the
/// engine.
fn native_audio_backend_from_driver_type(driver_type: &str) -> DirectAudio::AudioBackend {
    let backend = match driver_type {
        "WASAPI Exclusive" => DirectAudio::AudioBackend::WasapiExclusive,
        "WDM-KS" => DirectAudio::AudioBackend::WdmKs,
        "ASIO" => DirectAudio::AudioBackend::Asio,
        _ => DirectAudio::AudioBackend::Auto,
    };
    backend.sanitize_for_current_build()
}

/// Map the persisted Dropout Protection setting to the engine's mode enum.
fn engine_dropout_mode(
    mode: crate::settings::DropoutProtectionMode,
) -> DirectAudio::DropoutProtectionMode {
    use crate::settings::DropoutProtectionMode as Ui;
    match mode {
        Ui::Off => DirectAudio::DropoutProtectionMode::Off,
        Ui::Light => DirectAudio::DropoutProtectionMode::Light,
        Ui::Medium => DirectAudio::DropoutProtectionMode::Medium,
        Ui::High => DirectAudio::DropoutProtectionMode::High,
    }
}

fn resolve_output_device_for_backend(
    engine: &DirectAudio::AudioEngine,
    backend: DirectAudio::AudioBackend,
    wanted: &str,
) -> Option<DirectAudio::AudioDeviceId> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return None;
    }
    engine
        .list_output_devices_for_backend(backend)
        .into_iter()
        .find(|device| device.name == wanted || device.id == wanted)
        .map(|device| device.device_id)
}

pub(crate) fn build_and_warm_audio_engine(
    schema: crate::settings::SettingsSchema,
) -> Result<(DirectAudio::AudioEngine, DirectAudio::EngineStats), String> {
    let backend = native_audio_backend_from_driver_type(&schema.hardware.audio.driver_type);
    let audio_config = DirectAudio::EngineConfig {
        sample_rate: schema.general.project_defaults.sample_rate,
        buffer_size: schema.general.project_defaults.buffer_size,
        channels: 2,
        backend,
        input_device: None,
        output_device: None,
    };

    let mut engine =
        DirectAudio::AudioEngine::new(audio_config).map_err(|error| error.to_string())?;
    eprintln!(
        "[audio] sphere_directaudioengine v{} ready (backend={:?}, sr={}, buf={})",
        engine.version(),
        engine.config().backend,
        engine.config().sample_rate,
        engine.config().buffer_size
    );
    let devices = engine.list_output_devices();
    eprintln!("[audio] {} output device(s) discovered", devices.len());
    for d in devices.iter().take(8) {
        eprintln!(
            "[audio]   - {} ({} ch @ {} Hz){}",
            d.name,
            d.channels,
            d.default_sample_rate,
            if d.is_default { "  [default]" } else { "" }
        );
    }

    engine.set_pdc_enabled(schema.playback.latency_compensation);
    engine.set_dropout_protection_mode(engine_dropout_mode(schema.playback.dropout_protection));
    match engine.start() {
        Ok(()) => {
            let stats = engine.stats();
            eprintln!(
                "[audio] stream warmed: backend={} sr={} buf={}",
                stats.backend_name, stats.sample_rate, stats.buffer_size
            );
            Ok((engine, stats))
        }
        Err(error) => {
            let message = format!("warm-up failed; will retry on first Play: {error}");
            eprintln!("[audio] {message}");
            let stats = engine.stats();
            Ok((engine, stats))
        }
    }
}

/// Fixed clip id for the temporary live-recording preview clip.
pub(crate) const RECORDING_PREVIEW_CLIP_ID: &str = "__recording_preview__";

/// UI-side bookkeeping for the realtime recording waveform preview (Part 1).
/// Holds the streamed peak bins and where they live in the arrangement; the
/// growing preview clip itself lives in timeline state under
/// [`RECORDING_PREVIEW_CLIP_ID`].
pub(crate) struct RecordingPreviewUi {
    pub clip_id: String,
    pub recording_id: u64,
    pub track_id: String,
    pub start_beat: f32,
    pub sample_rate: u32,
    pub peaks_per_second: u32,
    /// Number of preview bins already drained from the engine.
    pub drained: u64,
    pub peaks: Vec<crate::components::timeline::waveform_cache::WaveformPeak>,
}

pub struct StudioLayout {
    active_panel: WorkspaceActivePanel,
    right_dock_tab: RightDockTab,
    active_bottom_tab: components::BottomTab,
    bottom_panel_state: BottomPanelState,
    timeline: Entity<components::timeline::Timeline>,
    /// Piano-roll editor for MIDI clips in the bottom panel router.
    piano_roll: Entity<components::piano_roll::PianoRoll>,
    /// Audio clip editor for the bottom panel router.
    audio_editor: Entity<components::AudioEditorHost>,
    /// Routes bottom Editor tab between audio / MIDI / empty state.
    clip_editor_panel: Entity<components::ClipEditorPanel>,
    /// Compact Logic-style musical typing / virtual MIDI keyboard.
    virtual_keyboard: Entity<components::VirtualKeyboardPanel>,
    /// Last routed virtual-keyboard target. A change releases active notes.
    virtual_keyboard_last_target: Option<String>,
    /// Last observed main-window active state for virtual-keyboard cleanup.
    virtual_keyboard_window_active: bool,
    /// Second piano-roll instance for the floating MIDI editor (same timeline).
    piano_roll_floating: Entity<components::piano_roll::PianoRoll>,
    /// Floating MIDI editor window (one instance; switches clip on open) + the
    /// owner bounds for a deferred open. Grouped into
    /// [`window_ops::MidiEditorWindowState`] (decomposition slice).
    midi_editor: window_ops::MidiEditorWindowState,
    file_browser: FileBrowserState,
    /// Stable scroll handle for the browser tree. Lives on the layout
    /// (not in `FileBrowserState`) so the state stays free of gpui types
    /// and so the handle survives across renders.
    browser_scroll: UniformListScrollHandle,
    menu_bar: MenuBarUiState,
    command_palette: CommandPaletteState,
    command_palette_input: TextInputState,
    background_tasks: BackgroundTaskStore,
    project_switcher: ProjectSwitcherState,
    project_switcher_search_input: TextInputState,
    browser_search_input: TextInputState,
    mixer_tree_filter_input: TextInputState,
    /// Inspector track-name + clip-name inline edit fields (focus-handle-backed
    /// so keys route through the main-window text machinery) and the ids they
    /// are currently bound to. Grouped into
    /// [`input_ops::InspectorNameEditState`] (decomposition slice).
    inspector_name_edit: input_ops::InspectorNameEditState,
    /// UI-only selected plugin insert `(track_id, insert_id)` driving the
    /// Plugin Insert inspector target. Pure selection — never marks dirty.
    selected_insert: Option<(String, String)>,
    /// Phase 2b insert plugin picker overlay state.
    plugin_picker: PluginPickerState,
    plugin_picker_search_input: TextInputState,
    plugin_picker_prefs: PluginPickerPrefs,
    plugin_picker_scroll: PluginPickerScrollHandles,
    // Shared via `Arc` so the picker window snapshot and per-keypress handlers
    // bump a refcount instead of deep-cloning the whole 1000+ plugin index.
    plugin_search_index: Option<std::sync::Arc<PluginSearchIndex>>,
    plugin_picker_au_error: Option<String>,
    plugin_picker_window: Option<WindowHandle<plugin_picker_window::InsertPickerWindow>>,
    /// Automation target picker search query + input state.
    automation_picker_query: String,
    automation_picker_search_input: TextInputState,
    /// Detached / external window handles (settings, mixer, add-track,
    /// plugin-manager, export) + deferred external-mixer open bounds. Grouped
    /// into [`window_ops::ExternalWindows`] (decomposition slice).
    external_windows: window_ops::ExternalWindows,
    /// Plugin catalog / registry-scan state backing the insert picker (cached
    /// scan result, preset-cache presence, catalog load phase). Grouped into
    /// [`plugin_ops::PluginCatalogState`] (decomposition slice).
    plugin_catalog: plugin_ops::PluginCatalogState,
    /// Plugin-editor window handles — GPUI-hosted editor shells, native
    /// external-bridge editor sessions, the shared bridge runtime, and editor
    /// opens deferred while an insert runtime was loading. Grouped into
    /// [`plugin_ops::PluginEditorWindows`] (decomposition slice).
    plugin_editors: plugin_ops::PluginEditorWindows,
    panels: StudioPanelVisibility,
    settings: gpui::Entity<SettingsModel>,

    /// Transient overlay state (text context menu, open popover, inspector
    /// routing combo + anchor). Grouped into [`studio_state::OverlayState`]
    /// (decomposition slice).
    overlay: studio_state::OverlayState,
    /// Audio-engine bridge / sync state (engine handle, stats, last error, dirty
    /// flags, background-sync handshake). Grouped into
    /// [`audio_transport::AudioBridgeState`] (decomposition slice).
    audio_bridge: audio_transport::AudioBridgeState,
    /// Control-thread scheduler for MIDI tracks routed to hardware/virtual MIDI
    /// outputs. Kept out of the realtime audio callback.
    hardware_midi_playback: sphere_midi_service::HardwareMidiPlayback,
    /// Active recording-session UI state (take start position, UI phase, live
    /// growing-waveform preview). Grouped into
    /// [`recording_ops::RecordingSessionState`] (decomposition slice).
    recording: recording_ops::RecordingSessionState,
    /// Async tempo-detection jobs for the Audio Stretch inspector.
    stretch_tempo: stretch_tempo_ops::StretchTempoState,
    /// Throttle / sync timestamps for engine ↔ UI bridging (playhead, snapshot
    /// sync, meter push, tempo commit). Grouped into
    /// [`audio_transport::EngineSyncState`] (decomposition slice).
    engine_sync: audio_transport::EngineSyncState,
    /// Transient BPM vertical-drag gesture state (FL Studio–style infinite
    /// scrub). Mutated only by the transport BPM drag handler. Grouped into
    /// [`audio_transport::BpmDragState`] so this god-struct carries one cohesive
    /// field instead of seven loose ones (first decomposition slice).
    bpm_drag: audio_transport::BpmDragState,
    /// Inline BPM + time-signature numeric editors opened from the transport
    /// bar. Grouped into [`transport_ops::TempoEditState`] (decomposition slice).
    tempo_edit: transport_ops::TempoEditState,
    /// Transient tap-tempo session (not serialized; calculated BPM applies immediately).
    tap_tempo: crate::tap_tempo::TapTempo,
    /// Owns keyboard focus for the studio surface. Without a focused
    /// element GPUI never dispatches key events to `capture_key_down`,
    /// so we focus this handle on first render — that is what makes
    /// Spacebar, Enter, L, K, R, Home reach `shortcut_command`.
    focus_handle: FocusHandle,
    /// Arrangement clip snapshots copied by Ctrl/Cmd+C/X. Kept in-memory to
    /// avoid serializing the full project clip model into the system clipboard.
    clip_clipboard: Vec<ClipSnapshot>,
    /// Menu/key command IDs we've already logged as unsupported. Keeps
    /// the unified dispatcher quiet after the first miss per command.
    logged_unsupported_commands: HashSet<String>,
    /// Repaint-rate diagnostics. Ticks once per `Render`, smoothed
    /// EMA frame time, exposed in the status bar.
    frame_diag: FrameDiagnostics,
    /// Deterministic display-synced frame pacing. Owns the resolved
    /// [`crate::frame_scheduler::FrameRateMode`] + detected refresh rate and
    /// publishes the continuous poll cadence the audio loop reads.
    frame_scheduler: crate::frame_scheduler::FrameScheduler,
    /// Last root key-routing decision. Exposed through debug/perf counters and
    /// kept UI-only so shortcut diagnostics never touch the audio graph.
    shortcut_diagnostics: ShortcutDiagnostics,
    /// Mixer-panel view state (scroll, insert/send section heights, splitter-drag
    /// anchors). Grouped into [`mixer_ops::MixerViewState`] (decomposition slice).
    mixer_view: mixer_ops::MixerViewState,
    /// Stable mixer-tree callbacks + resize hooks (built once per layout).
    mixer_tree_ui_hooks: Option<mixer_ops::MixerTreeUiHooks>,
    mixer_tree_sidebar: gpui::Entity<components::MixerTreeSidebar>,
    mixer_master_strip: gpui::Entity<components::MixerMasterStripView>,
    mixer_panel: gpui::Entity<components::MixerPanelView>,
    bottom_panel_shell: gpui::Entity<components::BottomPanelShell>,
    chord_display_panel: gpui::Entity<components::SongTextPanelView>,
    lyric_display_panel: gpui::Entity<components::SongTextPanelView>,
    lyric_editor_panel: gpui::Entity<components::SongTextPanelView>,
    status_bar: gpui::Entity<components::StatusBarView>,
    effect_editor_tab: gpui::Entity<components::EffectEditorTabView>,

    // ── Project file system ───────────────────────────────────────────────────
    /// Centralized filesystem paths for the entire application.
    paths: FutureboardPaths,
    /// Canonical project session — single source of truth for name, paths,
    /// untitled/dirty flags, and lifecycle binding.
    project_session: crate::project::ProjectSession,
    /// Absolute path to the currently open `.fbproj` file, if any.
    project_path: Option<PathBuf>,
    /// Root folder of the current project (contains Media/, Cache/, etc.).
    project_folder: Option<PathBuf>,
    /// Persistent recent-projects list backed by `<AppData>/Futureboard Studio/recent.json`.
    recent_projects: RecentProjectsStore,
    /// Studio-window / app-integration hooks (own window handle, last known
    /// window bounds, app-level re-open-Welcome hook). Grouped into
    /// [`window_ops::StudioWindowHooks`] (decomposition slice).
    window_hooks: window_ops::StudioWindowHooks,
    /// Pending project-lifecycle dialog state (unsaved-changes guard + parked
    /// close/new/open action + last failed open path). Grouped into
    /// [`close_ops::LifecycleGuardState`] (decomposition slice).
    lifecycle_guard: close_ops::LifecycleGuardState,
    project_switch: project_switch::ProjectSwitchGuardState,
    /// Active keyboard shortcut profile manager. Builtin profiles ship in
    /// `packages/keymaps/`; user overrides live in `<app_data>/Keymaps/`.
    keymap_manager: crate::keymap::KeymapManager,
    /// Authoritative project-lifecycle state (Part G). Drives the window title;
    /// the dirty bit is still tracked on `project_switcher.current_project`.
    project_state: crate::app_state::ProjectState,
    /// Last OS window title applied in render, to avoid redundant set calls.
    last_window_title: Option<String>,
    /// Whether this workspace session is safe for arrangement/mixer/inspector UI.
    session_install_status: crate::app_state::SessionInstallStatus,
    /// In-window loading gate detail while [`session_install_status`] is Loading.
    session_install_detail: String,
    session_install_progress: crate::components::progress_dialog::ProgressBarValue,
    /// Non-fatal plugin restore warnings collected during session install.
    session_install_warnings: Vec<String>,
    /// Last time an autosave was attempted for the current workspace.
    last_autosave_at: std::time::Instant,
    /// Guards the background autosave job so render/poll frames cannot enqueue duplicates.
    autosave_in_flight: bool,
    /// Monotonic counter bumped every time the live session is torn down or
    /// replaced (project reset / in-studio switch). Async project-load
    /// completions capture the value at spawn time and self-reject if it has
    /// advanced, so a completion from a superseded session can never install
    /// over the session that replaced it. See
    /// [`Self::advance_session_generation`].
    session_generation: u64,
    /// Last time a meter-driven snapshot was pushed to the detached mixer window.
    /// The audio poll fires at display refresh (up to 120/144 Hz); rebuilding the
    /// whole external-mixer element tree that often is the pop-out lag, so the
    /// high-frequency meter push is capped here. Structural edits push
    /// immediately via [`Self::push_mixer_snapshot_to_window`] and are never
    /// throttled. See [`Self::push_mixer_meter_snapshot_throttled`].
    last_external_mixer_meter_push: std::time::Instant,
}

impl StudioLayout {
    pub(crate) fn defer_update(
        owner: &Entity<Self>,
        cx: &mut gpui::App,
        f: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let owner = owner.clone();
        cx.defer(move |cx| {
            let _ = owner.update(cx, f);
        });
    }

    pub(crate) fn defer_update_in_window(
        owner: &Entity<Self>,
        window: &Window,
        cx: &mut gpui::App,
        f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let owner = owner.clone();
        window.defer(cx, move |window, cx| {
            let _ = owner.update(cx, |this, cx| f(this, window, cx));
        });
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        // ── Centralized path resolution ───────────────────────────────────
        let paths = FutureboardPaths::resolve();
        if let Err(e) = paths.ensure_user_dirs() {
            eprintln!("[paths] failed to create user directories: {e}");
        }

        let settings = SettingsModel::load_or_create(cx);
        cx.set_global(GlobalSettingsModel(settings.clone()));
        crate::boot::log("settings loaded");

        let schema = settings.read(cx).current.clone();
        let frame_rate_mode = schema.performance.frame_rate;

        // Apply saved Renderer choice — Settings is "* Restart required", so
        // this only takes effect at process start. Idempotent: the same
        // preference is also applied at app launch (before the Welcome window)
        // so the GPU renderer can be warmed on the loading screen. The env var
        // `FUTUREBOARD_WGPU_TIMELINE=1` still wins as a dev override.
        apply_renderer_preference(&schema);

        crate::boot::log("audio engine warm-up deferred");

        let timeline = cx.new(|_| {
            if use_demo_project() {
                components::timeline::Timeline::with_demo_content()
            } else {
                components::timeline::Timeline::new()
            }
        });
        let metronome_enabled = schema.recording.metronome.enabled;
        let _ = timeline.update(cx, |t, _cx| {
            t.state.transport.metronome_enabled = metronome_enabled;
        });

        let piano_roll = {
            let timeline = timeline.clone();
            cx.new(|cx| components::piano_roll::PianoRoll::new(timeline, cx))
        };
        let audio_editor = {
            let timeline = timeline.clone();
            cx.new(|cx| components::AudioEditorHost::new(timeline, cx))
        };
        let clip_editor_panel = cx.new(|_| {
            components::ClipEditorPanel::new(
                timeline.clone(),
                piano_roll.clone(),
                audio_editor.clone(),
            )
        });
        let studio_entity = cx.entity();
        let mixer_tree_sidebar = cx.new(|cx| {
            components::MixerTreeSidebar::new(
                studio_entity.clone(),
                timeline.clone(),
                cx.focus_handle(),
                cx,
            )
        });
        let mixer_callbacks = components::mixer_panel::noop_mixer_callbacks();
        let mixer_split = components::mixer_panel::MixerSplit::inert();
        let mixer_master_strip = cx.new(|_cx| {
            components::MixerMasterStripView::new(
                timeline.clone(),
                mixer_callbacks,
                mixer_split,
                components::mixer_panel::STRIP_MIN_HEIGHT,
            )
        });
        let mixer_panel = cx.new(|_cx| {
            components::MixerPanelView::new(
                studio_entity.clone(),
                timeline.clone(),
                mixer_tree_sidebar.clone(),
                mixer_master_strip.clone(),
            )
        });
        let effect_editor_tab = cx
            .new(|_| components::EffectEditorTabView::new(studio_entity.clone(), timeline.clone()));
        let status_bar = cx.new(|_| components::StatusBarView::new(studio_entity.clone()));
        let chord_display_panel = cx.new(|cx| {
            components::SongTextPanelView::new(
                timeline.clone(),
                components::SongTextPanelKind::ChordDisplay,
                cx,
            )
        });
        let lyric_display_panel = cx.new(|cx| {
            components::SongTextPanelView::new(
                timeline.clone(),
                components::SongTextPanelKind::LyricDisplay,
                cx,
            )
        });
        let lyric_editor_panel = cx.new(|cx| {
            components::SongTextPanelView::new(
                timeline.clone(),
                components::SongTextPanelKind::LyricEditor,
                cx,
            )
        });
        let bottom_panel_shell = cx.new(|_| {
            components::BottomPanelShell::new(
                studio_entity.clone(),
                mixer_panel.clone(),
                clip_editor_panel.clone(),
                effect_editor_tab.clone(),
            )
        });
        let virtual_keyboard = cx.new(components::VirtualKeyboardPanel::new);
        let piano_roll_floating = {
            let timeline = timeline.clone();
            cx.new(|cx| {
                let mut pr = components::piano_roll::PianoRoll::new(timeline, cx);
                pr.midi_editor_sink = true;
                pr
            })
        };
        {
            let target = cx.entity().clone();
            let sink = Arc::new(
                move |event: sphere_midi_service::VirtualKeyboardEvent, cx: &mut gpui::App| {
                    target.update(cx, |layout, cx| {
                        matches!(
                            layout.route_virtual_keyboard_event(event, cx),
                            sphere_midi_service::MidiInputRouteStatus::Routed
                        )
                    })
                },
            );
            let _ = virtual_keyboard.update(cx, |keyboard, _cx| {
                keyboard.set_event_sink(Some(sink));
            });
        }
        {
            let target = cx.entity().clone();
            let preview_handler = Arc::new(
                move |command: components::piano_roll::UiMidiPreviewCommand, cx: &mut gpui::App| {
                    // PianoRoll is a child entity and may emit while StudioLayout
                    // already owns its parent lease (notably editor close). Never
                    // synchronously update the parent from this child callback.
                    // Queue the control-path MIDI command until the current GPUI
                    // update stack has unwound, avoiding a double lease while
                    // preserving AllNotesOff ordering before later UI work.
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |layout, cx| {
                            layout.dispatch_midi_preview_command(command, cx);
                        });
                    });
                },
            );
            let _ = piano_roll.update(cx, |roll, _cx| {
                roll.set_midi_preview_handler(Some(preview_handler.clone()));
            });
            let _ = piano_roll_floating.update(cx, |roll, _cx| {
                roll.set_midi_preview_handler(Some(preview_handler));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_loop_changed_callback(Some(Arc::new(move |cx| {
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |this, cx| {
                            // Loop region is transport/control state, NOT audio
                            // graph structure. It reaches the engine live through
                            // `sync_loop_controls` (`engine.set_loop` → atomics)
                            // and is persisted in timeline transport state for
                            // save. Dragging the loop handles fires this per
                            // mouse-move, so it must NOT call `mark_dirty()` —
                            // that sets `audio_bridge.project_dirty`, which the
                            // ~16ms poll turns into a full `load_project` engine
                            // sync (route-graph rebuild) per move and stutters
                            // playback. Use view-only dirty (session-dirty for
                            // save, no engine sync). Mirrors the mixer fader fix.
                            this.mark_dirty_view_only();
                            this.sync_loop_controls(cx);
                        });
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_tempo_map_changed_callback(Some(Arc::new(move |cx| {
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |this, cx| {
                            this.mark_dirty();
                            this.sync_tempo_map_to_engine(cx);
                        });
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_time_signature_map_changed_callback(Some(Arc::new(move |cx| {
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |this, cx| {
                            this.mark_dirty();
                            this.sync_time_signature_map_to_engine(cx);
                        });
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_control_state_changed_callback(Some(Arc::new(move |cx| {
                    // Track mute/solo from the timeline header: persisted state
                    // that reaches the engine live through the realtime command
                    // path. Deferred for the same nested-update reason as the
                    // project-changed callback below; view-only dirty so the
                    // audio poll never rebuilds the graph for it.
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |this, _cx| {
                            this.mark_dirty_view_only();
                        });
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_project_changed_callback(Some(Arc::new(move |cx| {
                    // DEFER the parent update. This callback runs from inside
                    // `Timeline::update` (gesture commits) AND from inside
                    // `StudioLayout::update → timeline.update` (keyboard command
                    // dispatch). In the latter case updating StudioLayout here
                    // would be a nested lease on an entity already being updated
                    // → GPUI double-lease panic. `cx.defer` runs the dirty mark
                    // after the current update stack unwinds, which is safe for
                    // both call paths (dirty is a flag the audio poll reads on
                    // its own cadence). See PART B of the shortcuts task.
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |this, _cx| {
                            this.mark_dirty();
                        });
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_media_changed_callback(Some(Arc::new(move |cx| {
                    // Deferred for the same nested-update reason as the project
                    // changed callback above. Only marks engine-media dirty here —
                    // never read/sync Timeline from this callback.
                    let target = target.clone();
                    cx.defer(move |cx| {
                        let _ = target.update(cx, |this, _cx| {
                            this.mark_engine_media_dirty();
                        });
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_open_editor_callback(Some(Arc::new(move |_window, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.panels.mixer_docked = true;
                        this.set_active_bottom_tab(components::BottomTab::Editor, cx);
                        cx.notify();
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_open_song_text_editor_callback(Some(Arc::new(move |_window, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.panels.inspector = true;
                        this.right_dock_tab = RightDockTab::LyricEditor;
                        this.set_active_panel(WorkspaceActivePanel::LyricEditor, cx);
                        cx.notify();
                    });
                })));
            });
        }
        {
            let target = cx.entity().clone();
            let _ = timeline.update(cx, |timeline, _cx| {
                timeline.set_plugin_preset_drop_callback(Some(Arc::new(
                    move |(preset_path, track_id), _window, cx| {
                        let preset_path = preset_path.clone();
                        let track_id = track_id.clone();
                        let _ = target.update(cx, |this, cx| {
                            this.apply_dropped_plugin_preset(&track_id, &preset_path, cx);
                        });
                    },
                )));
            });
        }

        Self::spawn_audio_poll(cx);

        let studio_entity = cx.entity();
        {
            let pop_owner = studio_entity.clone();
            let _ = piano_roll.update(cx, |pr, _cx| {
                pr.set_pop_out_handler(Some(Arc::new(move |_window, cx| {
                    let _ = pop_owner.update(cx, |layout, cx| {
                        layout.open_midi_editor_external_window(None, cx);
                    });
                })));
            });
        }
        crate::platform_chrome::register_studio_menu_dispatcher(studio_entity, cx);

        // Ordered studio teardown before GPUI/thread-local destruction.
        let _ = cx.on_app_quit(|layout, cx| {
            layout.shutdown_studio(cx);
            async {}
        });

        // settings and paths are loaded and registered at the top of this function

        let app_data = paths.app_data.clone();
        let mut layout = Self {
            active_panel: WorkspaceActivePanel::Mixer,
            right_dock_tab: RightDockTab::Inspector,
            active_bottom_tab: components::BottomTab::Mixer,
            bottom_panel_state: BottomPanelState::default(),
            timeline,
            piano_roll,
            audio_editor,
            clip_editor_panel,
            virtual_keyboard,
            virtual_keyboard_last_target: None,
            virtual_keyboard_window_active: true,
            piano_roll_floating,
            midi_editor: window_ops::MidiEditorWindowState::default(),
            file_browser: FileBrowserState::default(),
            browser_scroll: UniformListScrollHandle::new(),
            menu_bar: MenuBarUiState::default(),
            command_palette: CommandPaletteState::default(),
            command_palette_input: TextInputState::new(
                "command-palette-search-input",
                cx.focus_handle(),
            )
            .with_placeholder("Run command..."),
            background_tasks: BackgroundTaskStore::default(),
            stretch_tempo: stretch_tempo_ops::StretchTempoState::default(),
            project_switcher: ProjectSwitcherState::default(),
            project_switcher_search_input: TextInputState::new(
                "project-switcher-search-input",
                cx.focus_handle(),
            )
            .with_placeholder("Search projects..."),
            browser_search_input: TextInputState::new("browser-search-input", cx.focus_handle())
                .with_placeholder("Search...")
                .blur_on_outside_click(true),
            mixer_tree_filter_input: TextInputState::new("mixer-tree-filter", cx.focus_handle())
                .with_placeholder("Filter channels…"),
            inspector_name_edit: input_ops::InspectorNameEditState::new(cx),
            selected_insert: None,
            plugin_picker: PluginPickerState::closed(),
            plugin_picker_search_input: TextInputState::new(
                "plugin-picker-search-input",
                cx.focus_handle(),
            )
            .with_placeholder("Search plugins by name, vendor, category, or format…"),
            plugin_picker_prefs: PluginPickerPrefs::load(),
            plugin_picker_scroll: PluginPickerScrollHandles::default(),
            plugin_search_index: None,
            plugin_picker_au_error: load_au_cache_state().last_error,
            plugin_picker_window: None,
            automation_picker_query: String::new(),
            automation_picker_search_input: TextInputState::new(
                "automation-picker-search-input",
                cx.focus_handle(),
            )
            .with_placeholder("Search parameters…"),
            external_windows: window_ops::ExternalWindows::default(),
            plugin_catalog: plugin_ops::PluginCatalogState::default(),
            plugin_editors: plugin_ops::PluginEditorWindows::default(),
            panels: StudioPanelVisibility::default(),
            settings,

            overlay: studio_state::OverlayState::default(),
            audio_bridge: audio_transport::AudioBridgeState::default(),
            hardware_midi_playback: sphere_midi_service::HardwareMidiPlayback::new(),
            recording: recording_ops::RecordingSessionState::default(),
            engine_sync: audio_transport::EngineSyncState::default(),
            bpm_drag: audio_transport::BpmDragState::default(),
            tempo_edit: transport_ops::TempoEditState::new(cx),
            tap_tempo: crate::tap_tempo::TapTempo::new(),
            focus_handle: cx.focus_handle(),
            clip_clipboard: Vec::new(),
            logged_unsupported_commands: HashSet::new(),
            frame_diag: FrameDiagnostics::new(),
            frame_scheduler: crate::frame_scheduler::FrameScheduler::new(frame_rate_mode),
            shortcut_diagnostics: ShortcutDiagnostics::default(),
            mixer_view: mixer_ops::MixerViewState::default(),
            mixer_tree_ui_hooks: None,
            mixer_tree_sidebar,
            mixer_master_strip,
            chord_display_panel,
            lyric_display_panel,
            lyric_editor_panel,
            mixer_panel,
            bottom_panel_shell,
            status_bar,
            effect_editor_tab,
            paths,
            project_session: crate::project::ProjectSession::default(),
            project_path: None,
            project_folder: None,
            recent_projects: RecentProjectsStore::load(),
            window_hooks: window_ops::StudioWindowHooks::default(),
            lifecycle_guard: close_ops::LifecycleGuardState::default(),
            project_switch: project_switch::ProjectSwitchGuardState::default(),
            keymap_manager: crate::keymap::KeymapManager::new(app_data),
            project_state: crate::app_state::ProjectState::NoProject,
            last_window_title: None,
            session_install_status: crate::app_state::SessionInstallStatus::Ready,
            session_install_detail: String::new(),
            session_install_progress:
                crate::components::progress_dialog::ProgressBarValue::Indeterminate,
            session_install_warnings: Vec::new(),
            last_autosave_at: std::time::Instant::now(),
            autosave_in_flight: false,
            session_generation: 0,
            last_external_mixer_meter_push: std::time::Instant::now(),
        };

        layout.ensure_mixer_tree_defaults_once(cx);
        layout.ensure_mixer_tree_ui_hooks(cx.entity().clone(), cx);
        layout.refresh_mixer_tree_sidebar_entity(cx);
        // Audio-engine warm-up is preload, not mount work. The pre-studio
        // Loading Session dialog already builds + warms the engine and hands it
        // off during workspace install. Running the warm-up synchronously here
        // built a *second* engine on the first studio frame — stalling the first
        // paint (black screen at init) and then clobbering the pre-warmed handoff
        // engine when the background build finished. Defer it past the current
        // effect cycle (which includes the workspace install): the
        // `engine.is_some()` guard then skips the redundant build for handoff
        // mounts, while rollback / no-handoff mounts still get warmed.
        let self_entity = cx.entity();
        cx.defer(move |app| {
            let _ = self_entity.update(app, |this, cx| this.spawn_audio_engine_warmup(cx));
        });
        layout.sync_timeline_chrome_metrics(cx);

        layout
    }

    fn spawn_audio_engine_warmup(&mut self, cx: &mut Context<Self>) {
        if self.audio_bridge.engine.is_some() {
            return;
        }
        let schema = self.settings.read(cx).current.clone();
        self.audio_bridge.last_error = Some("Initializing audio...".to_string());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { build_and_warm_audio_engine(schema) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((engine, stats)) => {
                    if this.audio_bridge.engine.is_some() {
                        // A pre-studio handoff engine (already carrying the loaded
                        // project) was installed while this warm-up ran. Keep it and
                        // discard the redundant engine so the graph is not reset.
                        crate::boot::log("audio engine warm-up superseded by handoff engine");
                    } else {
                        this.install_audio_callbacks(&engine, cx);
                        this.audio_bridge.running = stats.running;
                        this.audio_bridge.stats = Some(stats);
                        this.audio_bridge.last_error = None;
                        this.audio_bridge.engine = Some(engine);
                        this.sync_plugin_bridge_sinks_to_engine(cx, "studio_audio_ready");
                        this.schedule_audio_project_sync(cx, true, "studio_audio_ready");
                        crate::boot::log("audio engine handle ready");
                        cx.notify();
                    }
                }
                Err(error) => {
                    eprintln!("[audio] failed to initialize engine: {error}");
                    this.audio_bridge.last_error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn install_audio_callbacks(
        &mut self,
        engine: &DirectAudio::AudioEngine,
        cx: &mut Context<Self>,
    ) {
        let seek_engine = engine.clone();
        let param_engine = engine.clone();
        let input_engine = engine.clone();
        let owner = cx.entity().clone();
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.set_native_audio_callbacks(
                Some(Arc::new(move |beats, bpm, reason| {
                    match reason {
                        SeekReason::UserDragging => {
                            let _ = seek_engine.set_metronome_suspended(true);
                        }
                        SeekReason::TimelineClick | SeekReason::Programmatic => {
                            let _ = seek_engine.set_metronome_suspended(false);
                        }
                        _ => {}
                    }
                    let seconds = beats.max(0.0) as f64 * 60.0 / bpm.max(1.0) as f64;
                    if let Err(error) = seek_engine.seek(seconds) {
                        eprintln!("[audio] seek failed: {error}");
                    }
                })),
                Some(Arc::new(move |track_id, param_id, value| {
                    let engine_value = match param_id.as_str() {
                        "volume" => volume_norm_to_linear(value) as f64,
                        "muted" | "solo" => {
                            if value >= 0.5 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                        _ => value as f64,
                    };
                    if let Err(error) =
                        param_engine.update_track_param(&track_id, &param_id, engine_value)
                    {
                        if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                            eprintln!(
                                "[audio] track param update failed: track={} param={} error={}",
                                track_id, param_id, error
                            );
                        }
                    }
                })),
                Some(Arc::new(move |track_id, armed, monitor| {
                    input_engine
                        .update_track_input_flags(&track_id, armed, monitor)
                        .map_err(|error| error.to_string())
                })),
            );
            let owner_begin = owner.clone();
            let owner_end = owner;
            timeline.set_playhead_scrub_callbacks(
                Some(Arc::new(move |_, cx| {
                    StudioLayout::defer_update(&owner_begin, cx, |this, cx| {
                        this.set_playhead_scrub_active(true, cx);
                    });
                })),
                Some(Arc::new(move |_, cx| {
                    StudioLayout::defer_update(&owner_end, cx, |this, cx| {
                        this.set_playhead_scrub_active(false, cx);
                    });
                })),
            );
        });
    }

    /// Switch the active keyboard shortcut profile. `"default"` restores the
    /// bundled map; any other id loads `<app dir>/Keymaps/<id>.json`. A missing
    /// or invalid profile file leaves the current map untouched. Returns the
    /// active profile id after the call.
    pub fn set_keymap_profile(&mut self, id: &str) -> &str {
        if let Err(error) = self.keymap_manager.set_active_profile(id) {
            if crate::keymap::shortcut_debug_enabled() {
                eprintln!("[shortcut] profile id={id} unavailable: {error}");
            }
        }
        self.keymap_manager.active_profile_id()
    }

    /// Id of the active keyboard shortcut profile (for the preferences UI).
    pub fn active_keymap_id(&self) -> &str {
        self.keymap_manager.active_profile_id()
    }

    /// Wire this layout to its own window handle so `close_project` can close
    /// the workspace window when returning to Welcome.
    pub fn set_self_window(&mut self, handle: WindowHandle<StudioLayout>) {
        self.window_hooks.self_window = Some(handle);
    }

    pub fn has_self_window(&self) -> bool {
        self.window_hooks.self_window.is_some()
    }

    /// Wire the app-level "return to Welcome" hook used by `close_project`.
    pub fn set_request_welcome_callback(
        &mut self,
        callback: Arc<dyn Fn(&mut gpui::App) + 'static>,
    ) {
        self.window_hooks.on_request_welcome = Some(callback);
    }

    pub fn set_request_session_shutdown_callback(
        &mut self,
        callback: Arc<
            dyn Fn(
                    crate::session_shutdown::SessionShutdownReason,
                    Option<gpui::Bounds<gpui::Pixels>>,
                    gpui::WindowHandle<Self>,
                    &mut gpui::App,
                ) + 'static,
        >,
    ) {
        self.window_hooks.on_request_session_shutdown = Some(callback);
    }

    /// Wire the app-level project-load hook used for in-studio open/replace.
    pub fn set_request_project_load_callback(
        &mut self,
        callback: Arc<dyn Fn(PathBuf, project_ops::ProjectOpenOptions, &mut gpui::App) + 'static>,
    ) {
        self.window_hooks.on_request_project_load = Some(callback);
    }

    /// Whether the session is fully installed and safe for UI components.
    pub fn session_install_status(&self) -> crate::app_state::SessionInstallStatus {
        self.session_install_status
    }
}

impl StudioLayout {
    pub(super) fn start_background_task(
        &mut self,
        id: impl Into<String>,
        kind: components::BackgroundTaskKind,
        title: impl Into<String>,
        detail: Option<String>,
        progress: Option<components::BackgroundTaskProgress>,
        cancellable: bool,
    ) {
        self.background_tasks.add_or_update(
            id,
            components::BackgroundTaskUpdate {
                kind,
                title: title.into(),
                detail,
                status: components::BackgroundTaskStatus::Running,
                progress,
                error: None,
                cancellable,
                parent_id: None,
            },
        );
    }

    pub(super) fn queue_background_task(
        &mut self,
        id: impl Into<String>,
        kind: components::BackgroundTaskKind,
        title: impl Into<String>,
        detail: Option<String>,
    ) {
        self.background_tasks.add_or_update(
            id,
            components::BackgroundTaskUpdate {
                kind,
                title: title.into(),
                detail,
                status: components::BackgroundTaskStatus::Queued,
                progress: None,
                error: None,
                cancellable: false,
                parent_id: None,
            },
        );
    }

    pub(super) fn complete_background_task(&mut self, id: &str, detail: Option<String>) {
        self.background_tasks.complete(id, detail);
    }

    pub(super) fn fail_background_task(&mut self, id: &str, error: impl Into<String>) {
        self.background_tasks.fail(id, error);
    }

    /// Single entry point for menu items, keyboard shortcuts, and chrome
    /// buttons. `command_id` matches the Electron/shared menu manifest
    /// IDs (e.g. `transport:play-pause`). Unknown IDs are logged once
    /// and then ignored — this is the contract that lets future menu
    /// entries appear in the chrome without crashing the dispatcher.
    pub fn dispatch_command_id(&mut self, command_id: &str, cx: &mut Context<Self>) {
        let studio_bounds = self.studio_window_bounds(cx);
        let owner_bounds =
            crate::window_position::resolve_owner_bounds_with_preferred(None, studio_bounds, cx);
        self.dispatch_command_id_from_bounds(command_id, owner_bounds, cx);
    }

    /// Main workspace window bounds — preferred owner for dialogs on Windows.
    pub(super) fn studio_window_bounds(&self, _cx: &mut gpui::App) -> Option<Bounds<gpui::Pixels>> {
        self.window_hooks
            .cached_bounds
            .filter(|bounds| crate::window_position::is_valid_owner_bounds(*bounds))
    }

    fn context_track_id_or_selected(&self, cx: &Context<Self>) -> Option<String> {
        match &self.overlay.open_popover {
            Some(OpenPopover::Context { request }) => match &request.target {
                ContextMenuTarget::TrackHeader(track_id) => Some(track_id.clone()),
                ContextMenuTarget::MixerStrip(track_id) => Some(track_id.clone()),
                ContextMenuTarget::Extended(ContextTarget::Track(track_id))
                | ContextMenuTarget::Extended(ContextTarget::TrackLane { track_id, .. })
                | ContextMenuTarget::Extended(ContextTarget::Mixer(track_id)) => {
                    Some(track_id.clone())
                }
                _ => None,
            },
            _ => None,
        }
        .or_else(|| {
            self.timeline
                .read(cx)
                .state
                .selection
                .selected_track_id
                .clone()
        })
    }

    fn context_clip_id_or_selected(&self, cx: &Context<Self>) -> Option<String> {
        match &self.overlay.open_popover {
            Some(OpenPopover::Context { request }) => match &request.target {
                ContextMenuTarget::Clip(clip_id) => Some(clip_id.clone()),
                ContextMenuTarget::Extended(ContextTarget::Clip(clip_id)) => Some(clip_id.clone()),
                _ => None,
            },
            _ => None,
        }
        .or_else(|| {
            self.timeline
                .read(cx)
                .state
                .selection
                .selected_clip_ids
                .first()
                .cloned()
        })
    }

    fn resolved_audio_clip_source_path(
        &self,
        clip_id: &str,
        cx: &Context<Self>,
    ) -> Option<PathBuf> {
        let source = self
            .timeline
            .read(cx)
            .state
            .find_clip(clip_id)
            .and_then(|(_, clip)| match &clip.clip_type {
                ClipType::Audio {
                    source_path: Some(path),
                    ..
                } if !path.is_empty() => Some(PathBuf::from(path)),
                _ => None,
            })?;

        if source.is_absolute() {
            Some(source)
        } else {
            self.project_folder
                .as_ref()
                .map(|folder| folder.join(&source))
                .or(Some(source))
        }
    }

    fn song_text_editor_accepts_input_commands(&mut self, cx: &mut Context<Self>) -> bool {
        if self.panels.inspector && self.right_dock_tab == RightDockTab::LyricEditor {
            return true;
        }
        self.panels.inspector = true;
        self.right_dock_tab = RightDockTab::LyricEditor;
        self.set_active_panel(WorkspaceActivePanel::LyricEditor, cx);
        cx.notify();
        false
    }

    pub(super) fn dispatch_command_id_from_bounds(
        &mut self,
        command_id: &str,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let normalized = normalize_command_id(command_id);
        let command_id = normalized.as_str();
        // While the session is not fully installed, block every command so the
        // UI cannot mutate a half-loaded workspace.
        if !self.session_install_status.is_ready() {
            eprintln!("[SessionLoad] command blocked during install: {command_id}");
            return;
        }
        if command_id == "overlay:theme" {
            self.command_palette.open();
            self.command_palette_input.set_value("overlay:theme");
            self.command_palette.query = "overlay:theme".to_string();
            self.overlay.open_popover = None;
            cx.notify();
            return;
        }
        if let Some(theme_id) = command_id.strip_prefix("theme:select:") {
            if crate::theme::activate_theme_by_id(theme_id) {
                let theme_id = theme_id.to_string();
                let _ = self.settings.update(cx, |settings, cx| {
                    settings.update_setting(move |schema| schema.appearance.theme = theme_id, cx);
                });
                self.command_palette.close();
                cx.notify();
            }
            return;
        }
        if edit_command_debug() && is_midi_routable_edit_command(command_id) {
            eprintln!("[edit-command] command={command_id} target=Timeline");
        }
        if let Some(command) = transport_command_from_id(command_id) {
            self.dispatch_transport_command(command, cx);
            return;
        }
        if let Some(rest) = command_id.strip_prefix("mixer:add-send-to:") {
            if let Some((track_id, target_track_id)) = rest.rsplit_once(':') {
                let added = self.timeline.update(cx, |timeline, _cx| {
                    timeline.state.add_send_to_target(track_id, target_track_id)
                });
                self.overlay.open_popover = None;
                if added.is_some() {
                    self.mark_dirty();
                    self.audio_bridge.project_dirty = true;
                    self.schedule_audio_project_sync(cx, false, "mixer_add_send");
                }
                cx.notify();
            }
            return;
        }
        if let Some((track_id, target)) =
            crate::components::timeline::timeline_state::parse_automation_target_menu_command(
                command_id,
            )
        {
            self.add_automation_target_for_track(&track_id, target, cx);
            return;
        }
        if let Some(track_id) = command_id.strip_prefix("mixer:create-return-send:") {
            let added = self.timeline.update(cx, |timeline, _cx| {
                timeline.state.create_return_and_send(track_id)
            });
            self.overlay.open_popover = None;
            if added.is_some() {
                self.mark_dirty();
                self.audio_bridge.project_dirty = true;
                self.schedule_audio_project_sync(cx, false, "mixer_create_return_send");
            }
            cx.notify();
            return;
        }
        match command_id {
            "noop" => {}

            "tools:command-palette" => {
                self.command_palette.open();
                self.command_palette_input.set_value("");
                self.overlay.open_popover = None;
                self.project_switcher.is_open = false;
                self.plugin_picker.is_open = false;
                cx.notify();
            }

            "tempo:tap" => {
                self.tap_tempo_now(cx);
            }
            "tempo:reset-tap" => {
                self.reset_tap_tempo(cx);
            }
            "tempo:add-tap-marker" => {
                self.add_tempo_marker_from_current_tempo_at_playhead(cx);
            }
            "tempo:add-marker" => {
                self.add_tempo_marker_at_playhead(cx);
            }
            "tempo:create" => {
                self.create_tempo_automation(cx);
            }
            "tempo:edit-bpm" => {
                self.begin_bpm_edit(cx);
            }
            "tempo:clear" => {
                self.clear_tempo_automation(cx);
            }
            "tempo:open-track" => {
                self.show_tempo_track(cx);
            }
            "tempo:hide-track" => {
                self.hide_tempo_track(cx);
            }
            "tempo:fit-range" => {
                self.timeline.update(cx, |timeline, cx| {
                    timeline.state.fit_tempo_automation_in_view();
                    cx.notify();
                });
                cx.notify();
            }
            "tempo:add-point-here" => {
                if let Some((beat, bpm)) = self.tempo_track_context_position() {
                    self.add_tempo_point_at_lane(beat, bpm, cx);
                }
            }
            "tempo:set-fixed-here" => {
                if let Some((beat, bpm)) = self.tempo_track_context_position() {
                    self.set_fixed_tempo_from_lane(beat, bpm, cx);
                }
            }
            "tempo:delete-point" => {
                if let Some(id) = self.tempo_track_context_point_id() {
                    self.delete_tempo_point(&id, cx);
                }
            }
            "tempo:curve-hold" => {
                if let Some(id) = self.tempo_track_context_point_id() {
                    self.set_tempo_point_curve(&id, TempoCurve::Hold, cx);
                }
            }
            "tempo:curve-linear" => {
                if let Some(id) = self.tempo_track_context_point_id() {
                    self.set_tempo_point_curve(&id, TempoCurve::Linear, cx);
                }
            }
            "tempo:curve-smooth" => {
                if let Some(id) = self.tempo_track_context_point_id() {
                    self.set_tempo_point_curve(&id, TempoCurve::Smooth, cx);
                }
            }
            "ruler:create-tempo-here" => {
                if let Some(beat) = self.ruler_context_beat() {
                    self.add_tempo_point_at_beat(beat, true, cx);
                }
            }
            "ruler:add-tempo-marker" => {
                if let Some(beat) = self.ruler_context_beat() {
                    self.add_tempo_point_at_beat(beat, false, cx);
                }
            }
            "ruler:add-marker" => {
                if let Some(beat) = self.ruler_context_beat() {
                    let _ = self.timeline.update(cx, |timeline, cx| {
                        timeline.state.add_marker_at_beat(beat);
                        timeline.mark_project_changed(cx);
                        cx.notify();
                    });
                    self.mark_dirty();
                    cx.notify();
                }
            }
            "ruler:add-region" => {
                if let Some(beat) = self.ruler_context_beat() {
                    let _ = self.timeline.update(cx, |timeline, cx| {
                        timeline.state.add_region_at_beat(beat);
                        timeline.mark_project_changed(cx);
                        cx.notify();
                    });
                    self.mark_dirty();
                    cx.notify();
                }
            }

            "ts:add-marker" => {
                self.add_time_signature_marker_at_playhead(cx);
            }
            "ts:edit" => {
                self.begin_ts_edit(self.ts_track_context_point_id(), cx);
            }
            "ts:clear" => {
                self.clear_time_signature_markers(cx);
            }
            "ts:open-track" => {
                self.show_time_signature_track(cx);
            }
            "ts:hide-track" => {
                self.hide_time_signature_track(cx);
            }
            "ts:add-point-here" => {
                if let Some(beat) = self.ts_track_context_position() {
                    self.add_time_signature_point_at_beat(beat, cx);
                }
            }
            "ts:delete-point" => {
                if let Some(id) = self.ts_track_context_point_id() {
                    self.delete_time_signature_point(&id, cx);
                }
            }
            "ts:move-to-playhead" => {
                if let Some(id) = self.ts_track_context_point_id() {
                    self.move_time_signature_point_to_playhead(&id, cx);
                }
            }
            "ruler:add-ts-marker" => {
                if let Some(beat) = self.ruler_context_beat() {
                    self.add_time_signature_point_at_beat(beat, cx);
                }
            }

            "browser:import" => {
                let path = match &self.overlay.open_popover {
                    Some(OpenPopover::Context { request }) => match &request.target {
                        ContextMenuTarget::Extended(ContextTarget::Browser(path)) => path.clone(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(path) = path {
                    let ext = path
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_ascii_lowercase())
                        .unwrap_or_default();
                    if is_supported_audio_ext(&ext) {
                        let timeline = self.timeline.clone();
                        let layout = cx.entity().clone();
                        let path_for_decode = path.clone();
                        let timeline_for_decode = timeline.clone();
                        timeline.update(cx, |t, cx| {
                            let path_key = path.to_string_lossy().to_string();
                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "Imported Audio".to_string());
                            t.state
                                .import_audio_to_selected_or_new_track(path_key, name);
                            cx.notify();
                        });
                        let _ = layout.update(cx, |this, cx| {
                            this.mark_dirty();
                            this.mark_engine_media_dirty();
                            this.schedule_audio_project_sync(cx, false, "browser_import");
                        });
                        let path_key = path_for_decode.to_string_lossy().to_string();
                        let _ = layout.update(cx, move |this, cx| {
                            this.spawn_timeline_audio_import_jobs(
                                cx,
                                timeline_for_decode,
                                path_for_decode,
                                path_key,
                            );
                        });
                    }
                }
            }
            "browser:reveal" => {
                let path = match &self.overlay.open_popover {
                    Some(OpenPopover::Context { request }) => match &request.target {
                        ContextMenuTarget::Extended(ContextTarget::Browser(path)) => path.clone(),
                        ContextMenuTarget::Clip(clip_id) => self
                            .resolved_audio_clip_source_path(clip_id, cx)
                            .and_then(|path| path.parent().map(PathBuf::from).or(Some(path))),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(path) = path {
                    reveal_path(&path);
                }
            }
            "browser:refresh" => {
                let path = match &self.overlay.open_popover {
                    Some(OpenPopover::Context { request }) => match &request.target {
                        ContextMenuTarget::Extended(ContextTarget::Browser(path)) => path.clone(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(path) = path {
                    self.file_browser.mark_loading(path.clone());
                    self.spawn_directory_load(cx, path);
                } else {
                    let pending = self.file_browser.expanded_paths.clone();
                    for p in pending {
                        self.file_browser.mark_loading(p.clone());
                        self.spawn_directory_load(cx, p);
                    }
                }
            }
            "browser:copy-path" => {
                let path = match &self.overlay.open_popover {
                    Some(OpenPopover::Context { request }) => match &request.target {
                        ContextMenuTarget::Extended(ContextTarget::Browser(path)) => path.clone(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(path) = path {
                    let path_str = path.to_string_lossy().to_string();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(path_str));
                }
            }
            "browser:open" => {
                let path = match &self.overlay.open_popover {
                    Some(OpenPopover::Context { request }) => match &request.target {
                        ContextMenuTarget::Extended(ContextTarget::Browser(path)) => path.clone(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(path) = path {
                    let id = path.to_string_lossy().to_string();
                    let expanded = self.file_browser.toggle_node(&id, Some(&path));
                    if expanded {
                        let pending = self.file_browser.paths_needing_load();
                        for p in pending {
                            self.file_browser.mark_loading(p.clone());
                            self.spawn_directory_load(cx, p);
                        }
                    }
                }
            }
            "browser:new-folder" => {
                eprintln!("[browser] TODO: new folder action");
            }
            "browser:rename" => {
                eprintln!("[browser] TODO: rename action");
            }

            // ── View / zoom ──────────────────────────────────────────────
            "view:zoom-in" => self.zoom_timeline_by(cx, 1.25),
            "view:zoom-out" => self.zoom_timeline_by(cx, 0.8),
            "view:reset-zoom" => self.reset_timeline_zoom(cx),
            "view:toggle-virtual-keyboard" | "midi:toggle-virtual-keyboard" => {
                // Deferred + panel-only: toggling off releases active notes
                // through the sink, which re-enters StudioLayout::update. We are
                // inside a StudioLayout lease here (command dispatch), so flushing
                // synchronously would double-lease and panic.
                let panel = self.virtual_keyboard.clone();
                let owner = cx.entity();
                cx.defer(move |cx| {
                    let _ = panel.update(cx, |keyboard, cx| keyboard.toggle(cx));
                    let _ = owner.update(cx, |this, cx| {
                        this.update_virtual_keyboard_target_status(cx);
                        cx.notify();
                    });
                });
            }

            // ── Project / track / edit commands available in native shell ─
            // New Project no longer opens a modal wizard — it drops straight
            // into a fresh, empty, unsaved workspace. All four lifecycle
            // entry points share one unsaved-changes guard (Save / Don't Save /
            // Cancel) before replacing or unloading the current project.
            "project:new" | "project:new-from-template" => {
                self.guard_dirty_then_lifecycle(LifecycleAction::NewProject, owner_bounds, cx)
            }
            "project:close" => self.request_close(
                close_ops::PendingCloseAction::CloseProject,
                owner_bounds,
                cx,
            ),
            // Quit the whole application — distinct from `project:close`, which
            // only unloads the session and returns to Welcome.
            "app:quit" => {
                self.request_close(close_ops::PendingCloseAction::QuitApp, owner_bounds, cx)
            }
            "project:open" => {
                self.guard_dirty_then_lifecycle(LifecycleAction::OpenProject, owner_bounds, cx)
            }
            "project:save" => self.cmd_save_project(cx),
            "project:save-as" => self.cmd_save_project_as(cx),
            "project:save-copy" => self.cmd_save_project_copy(cx),
            "project:open-recent" => self.cmd_open_recent_project(owner_bounds, cx),
            "project:recent-clear" => {
                self.recent_projects.clear();
                self.sync_recent_to_switcher();
            }
            "project:reveal-folder" => self.cmd_reveal_project_folder(cx),
            "project:switch-current" => self.handle_project_switch_current_row(cx),

            // ── Dev stress-test commands (not in release menus) ──────────────
            "dev:tracks-32" => self.stress_add_tracks(32, cx),
            "dev:tracks-64" => self.stress_add_tracks(64, cx),
            "dev:tracks-128" => self.stress_add_tracks(128, cx),
            "dev:tracks-500" => self.stress_add_tracks(500, cx),

            "help:keyboard-shortcuts" => {
                self.open_keymap_window(owner_bounds, cx);
            }
            "help:documentation" => {
                crate::user_manual::open_section(crate::user_manual::INDEX_FILE);
            }
            "help:quick-start" => {
                crate::user_manual::open_section(crate::user_manual::QUICK_START_FILE);
            }
            "app:about" | "app:check-for-updates" => {
                self.open_settings_dialog(owner_bounds, cx);
            }

            "app:preferences" | "edit:preferences" | "project:settings" => {
                self.open_settings_dialog(owner_bounds, cx);
            }

            "panel:toggle-browser" | "window.show_browser" => self.toggle_browser_panel(cx),
            "panel:toggle-inspector" | "view:toggle-inspector" | "window.show_inspector" => {
                self.toggle_inspector_panel(cx)
            }
            "panel:toggle-mixer" | "view:toggle-mixer" | "window.show_mixer" => {
                self.toggle_mixer_panel(cx)
            }
            "panel:show-chord-display" => {
                self.panels.inspector = true;
                self.right_dock_tab = RightDockTab::ChordDisplay;
                self.set_active_panel(WorkspaceActivePanel::ChordDisplay, cx);
            }
            "panel:show-lyric-display" => {
                self.panels.inspector = true;
                self.right_dock_tab = RightDockTab::LyricDisplay;
                self.set_active_panel(WorkspaceActivePanel::LyricDisplay, cx);
            }
            "panel:show-lyric-editor" => {
                self.panels.inspector = true;
                self.right_dock_tab = RightDockTab::LyricEditor;
                self.set_active_panel(WorkspaceActivePanel::LyricEditor, cx);
            }
            "song_text.add_chord_at_playhead" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_add_chord_at_playhead(cx));
                }
            }
            "song_text.add_lyric_at_playhead" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_add_lyric_at_playhead(cx));
                }
            }
            "song_text.add_both_at_playhead" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_add_both_at_playhead(cx));
                }
            }
            "song_text.commit" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_commit(cx));
                }
            }
            "song_text.commit_next_grid" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_commit_next_grid(cx));
                }
            }
            "song_text.commit_next_beat" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_commit_next_beat(cx));
                }
            }
            "song_text.commit_next_bar" => {
                if self.song_text_editor_accepts_input_commands(cx) {
                    let _ = self
                        .lyric_editor_panel
                        .update(cx, |panel, cx| panel.command_commit_next_bar(cx));
                }
            }
            "song_text.previous_event" => {
                let _ = self
                    .lyric_editor_panel
                    .update(cx, |panel, cx| panel.command_previous_event(cx));
            }
            "song_text.next_event" => {
                let _ = self
                    .lyric_editor_panel
                    .update(cx, |panel, cx| panel.command_next_event(cx));
            }
            "song_text.move_to_playhead" => {
                let _ = self
                    .lyric_editor_panel
                    .update(cx, |panel, cx| panel.command_move_to_playhead(cx));
            }
            "song_text.delete_selected" => {
                let _ = self
                    .lyric_editor_panel
                    .update(cx, |panel, cx| panel.command_delete_selected(cx));
            }
            "window:chord-display" => self.open_song_text_external_window(
                components::SongTextPanelKind::ChordDisplay,
                owner_bounds,
                cx,
            ),
            "window:lyric-display" => self.open_song_text_external_window(
                components::SongTextPanelKind::LyricDisplay,
                owner_bounds,
                cx,
            ),
            "window:lyric-editor" => self.open_song_text_external_window(
                components::SongTextPanelKind::LyricEditor,
                owner_bounds,
                cx,
            ),
            "view:toggle-perf-metrics" => self.toggle_status_performance_metrics(cx),
            "view:toggle-perf-overlay" => self.toggle_performance_overlay(cx),
            "panel:mixer-float" | "floatingwindow:mixer" => {
                self.open_mixer_external_window(owner_bounds, cx);
            }
            "floatingwindow:routing-matrix" | "window:audio-connections" => {
                self.open_routing_matrix_window(owner_bounds, cx);
            }

            "track:add" | "track:show-add-dialog" | "project:add-track" => {
                self.open_add_track_external_window(AddTrackKind::Audio, owner_bounds, cx)
            }
            "track:add-audio" => {
                self.open_add_track_external_window(AddTrackKind::Audio, owner_bounds, cx)
            }
            "track:add-midi" => {
                self.open_add_track_external_window(AddTrackKind::Midi, owner_bounds, cx)
            }
            "track:add-instrument" => {
                self.open_add_track_external_window(AddTrackKind::Instrument, owner_bounds, cx)
            }
            "track:add-plugin" => {
                self.open_add_track_external_window(AddTrackKind::Plugin, owner_bounds, cx)
            }
            "track:add-bus" => {
                self.open_add_track_external_window(AddTrackKind::Bus, owner_bounds, cx)
            }
            "track:add-return" => {
                self.open_add_track_external_window(AddTrackKind::Return, owner_bounds, cx)
            }
            "track:add-group" => {
                self.open_add_track_external_window(AddTrackKind::Group, owner_bounds, cx)
            }
            "track:add-master" => {
                self.open_add_track_external_window(AddTrackKind::Master, owner_bounds, cx)
            }
            "plugins:manager" => self.open_plugin_manager_external_window(owner_bounds, cx),
            // Audio ▸ Audio Plug-in Scanner. The scanner UI (rescan / rescan-all
            // / AU rescan controls) lives inside the Plug-in Manager window.
            // TODO: auto-start a rescan once the manager handle exposes begin_scan.
            "plugins:scan" => self.open_plugin_manager_external_window(owner_bounds, cx),
            "plugins:insert" => {
                let track_id = self
                    .timeline
                    .read(cx)
                    .state
                    .selection
                    .selected_track_id
                    .clone();
                if let Some(track_id) = track_id {
                    self.open_insert_picker(&track_id, None, cx);
                }
            }
            "file:export-arrangement" => {
                self.open_export_arrangement_external_window(owner_bounds, cx)
            }
            "audio:stem-extractor" | "tools:stem-extractor" => {
                self.open_stem_extractor_external_window(owner_bounds, cx)
            }
            "track:rename" => {
                if let Some(track_id) = self.context_track_id_or_selected(cx) {
                    self.panels.inspector = true;
                    self.timeline.update(cx, |timeline, cx| {
                        timeline.state.select_track(&track_id);
                        cx.notify();
                    });
                    self.overlay.pending_text_focus = Some(TextMenuTarget::InspectorName);
                    self.set_active_panel(WorkspaceActivePanel::Inspector, cx);
                    cx.notify();
                }
            }
            "track:settings" | "track:color" => {
                if let Some(track_id) = self.context_track_id_or_selected(cx) {
                    self.panels.inspector = true;
                    self.timeline.update(cx, |timeline, cx| {
                        timeline.state.select_track(&track_id);
                        cx.notify();
                    });
                    self.set_active_panel(WorkspaceActivePanel::Inspector, cx);
                    cx.notify();
                }
            }
            "track:delete" => self.delete_selected_track(cx),
            "track:height-small" => self.set_context_track_height_preset(
                crate::components::timeline::timeline_state::TrackHeightPreset::Small,
                cx,
            ),
            "track:height-normal" => self.set_context_track_height_preset(
                crate::components::timeline::timeline_state::TrackHeightPreset::Normal,
                cx,
            ),
            "track:height-large" => self.set_context_track_height_preset(
                crate::components::timeline::timeline_state::TrackHeightPreset::Large,
                cx,
            ),
            "track:height-huge" => self.set_context_track_height_preset(
                crate::components::timeline::timeline_state::TrackHeightPreset::Huge,
                cx,
            ),
            "track:height-reset" => self.reset_context_track_height(cx),
            "track:height-reset-all" => self.reset_all_track_heights(cx),
            "track:mute" => self.toggle_selected_track_mute(cx),
            "track:solo" => self.toggle_selected_track_solo(cx),
            "track:arm" => self.toggle_selected_track_arm(cx),
            "mixer:reset-volume" => self.reset_selected_track_volume(cx),
            "mixer:reset-pan" => self.reset_selected_track_pan(cx),
            "edit:delete"
            | "edit:delete-backspace"
            | "clip:delete"
            | "clip:erase"
            | "automation:delete-selected-points" => self.delete_selected_clip_or_track(cx),
            // Automation editor commands are automation-aware. General edit
            // shortcuts fall back to arrangement clip selection/clipboard.
            "edit:select-all" => self.select_all_timeline_items(cx),
            "automation:select-all-points" => self.select_all_automation_points(cx),
            "edit:copy" => self.copy_selected_clips(cx),
            "edit:cut" => self.cut_selected_clips(cx),
            "edit:paste" => self.paste_clips_at_playhead(cx),
            "edit:deselect-all" | "automation:clear-selection" => {
                self.clear_automation_selection(cx)
            }
            "automation:toggle-mode" => self.toggle_selected_track_automation_mode(cx),
            "automation:cycle-target" => self.cycle_selected_track_automation_target(cx),
            "edit:undo" => {
                let _ = self
                    .timeline
                    .update(cx, |timeline, cx| timeline.undo_edit(cx));
            }
            "edit:redo" => {
                let _ = self
                    .timeline
                    .update(cx, |timeline, cx| timeline.redo_edit(cx));
            }
            "edit:duplicate" | "clip:duplicate" => self.duplicate_selected_clip(cx),
            "clip:rename" => {
                if let Some(clip_id) = self.context_clip_id_or_selected(cx) {
                    self.panels.inspector = true;
                    self.timeline.update(cx, |timeline, cx| {
                        timeline.state.select_clip(&clip_id);
                        cx.notify();
                    });
                    self.overlay.pending_text_focus = Some(TextMenuTarget::InspectorClipName);
                    self.set_active_panel(WorkspaceActivePanel::Inspector, cx);
                    cx.notify();
                }
            }
            "clip:properties" => {
                if let Some(clip_id) = self.context_clip_id_or_selected(cx) {
                    self.panels.inspector = true;
                    self.timeline.update(cx, |timeline, cx| {
                        timeline.state.select_clip(&clip_id);
                        cx.notify();
                    });
                    self.set_active_panel(WorkspaceActivePanel::Inspector, cx);
                    cx.notify();
                }
            }
            "clip:split-at-playhead" => self.split_selected_audio_clip_at_playhead(cx),

            // ── Tools — switch the active timeline tool. UI-only; never dirties
            // the engine. The piano roll owns its own tool keys when focused.
            "tools:select-pointer"
            | "tools:select-pen"
            | "tools:select-cut"
            | "tools:select-glue"
            | "tools:select-mute"
            | "tools:select-time"
            | "tools:select-automation" => {
                use components::timeline::timeline_state::TimelineTool;
                let tool = match command_id {
                    "tools:select-pen" => TimelineTool::Pen,
                    "tools:select-cut" => TimelineTool::Cut,
                    "tools:select-glue" => TimelineTool::Glue,
                    "tools:select-mute" => TimelineTool::Mute,
                    "tools:select-time" => TimelineTool::Time,
                    "tools:select-automation" => TimelineTool::Automation,
                    _ => TimelineTool::Pointer,
                };
                let _ = self.timeline.update(cx, |timeline, cx| {
                    if timeline.state.active_tool != tool {
                        timeline.reset_input_state();
                        timeline.state.active_tool = tool;
                        cx.notify();
                    }
                });
                self.set_active_panel(
                    if matches!(tool, TimelineTool::Automation) {
                        WorkspaceActivePanel::Automation
                    } else {
                        WorkspaceActivePanel::Arrangement
                    },
                    cx,
                );
            }

            "editor:open-bottom" => self.open_midi_editor_bottom_panel(cx),
            "midi:open-editor" | "editor:open-midi-window" => {
                self.open_midi_editor_external_window(owner_bounds, cx)
            }
            "midi:select-all"
            | "midi:delete-selected"
            | "midi:duplicate-selected"
            | "midi:quantize"
            | "midi:fit-notes"
            | "midi:nudge-left"
            | "midi:nudge-right"
            | "midi:transpose-up"
            | "midi:transpose-down"
            | "midi:transpose-octave-up"
            | "midi:transpose-octave-down"
            | "midi:velocity-increase"
            | "midi:velocity-decrease"
            | "midi:toggle-snap"
            | "midi:tool-select"
            | "midi:tool-draw"
            | "midi:tool-line" => self.dispatch_midi_editor_menu_command(command_id, cx),
            // Articulation assignment / lane commands route to whichever MIDI
            // editor (docked or floating) is active, like the entries above.
            cmd if cmd.starts_with("midi:articulation-") || cmd.starts_with("midi:lane-") => {
                self.dispatch_midi_editor_menu_command(command_id, cx)
            }

            // ── Transport extras (shared menu IDs) ───────────────────────
            "transport:go-to-end" => {
                let end = self.project_end_beat(cx);
                self.seek_native_playhead(cx, end);
            }
            "transport:rewind" => self.nudge_playhead_bars(cx, -1.0),
            "transport:fast-forward" => self.nudge_playhead_bars(cx, 1.0),

            other => {
                if self.logged_unsupported_commands.insert(other.to_string()) {
                    eprintln!("[command] unsupported in native: {}", other);
                }
            }
        }
    }

    pub(crate) fn toggle_browser_panel(&mut self, cx: &mut Context<Self>) {
        self.panels.browser = !self.panels.browser;
        if self.panels.browser {
            self.set_active_panel(WorkspaceActivePanel::Browser, cx);
        }
        self.sync_timeline_chrome_metrics(cx);
        cx.notify();
    }

    pub(crate) fn toggle_inspector_panel(&mut self, cx: &mut Context<Self>) {
        self.panels.inspector = !self.panels.inspector;
        if self.panels.inspector {
            self.right_dock_tab = RightDockTab::Inspector;
            self.set_active_panel(WorkspaceActivePanel::Inspector, cx);
        }
        self.sync_timeline_chrome_metrics(cx);
        cx.notify();
    }

    pub(crate) fn toggle_mixer_panel(&mut self, cx: &mut Context<Self>) {
        if self.external_windows.mixer.is_some() {
            self.close_mixer_window(cx);
            self.panels.mixer_docked = true;
        } else {
            self.panels.mixer_docked = !self.panels.mixer_docked;
        }
        if self.panels.mixer_docked {
            self.ensure_mixer_tree_defaults_once(cx);
            self.ensure_mixer_tree_ui_hooks(cx.entity().clone(), cx);
            self.set_active_panel(WorkspaceActivePanel::Mixer, cx);
        }
        self.sync_timeline_chrome_metrics(cx);
        self.notify_bottom_panel_shell(cx);
        cx.notify();
    }

    pub(crate) fn toggle_status_performance_metrics(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, cx| {
            settings.update_setting(
                |schema| {
                    schema.performance.show_status_performance_metrics =
                        !schema.performance.show_status_performance_metrics;
                },
                cx,
            );
        });
        if !self
            .settings
            .read(cx)
            .current
            .performance
            .show_status_performance_metrics
        {
            self.overlay.perf_metrics_popover_open = false;
        }
        self.notify_status_bar_if_changed(cx);
        cx.notify();
    }

    pub(crate) fn toggle_performance_overlay(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, cx| {
            settings.update_setting(
                |schema| {
                    schema.performance.show_performance_overlay =
                        !schema.performance.show_performance_overlay;
                },
                cx,
            );
        });
        cx.notify();
    }

    fn selected_midi_clip_id(&self, cx: &Context<Self>) -> Option<String> {
        let tl = self.timeline.read(cx);
        let clip_id = tl.state.selection.selected_clip_ids.first()?.clone();
        tl.state
            .find_clip(&clip_id)
            .filter(|(_, c)| matches!(c.clip_type, ClipType::Midi { .. }))
            .map(|_| clip_id)
    }

    fn select_midi_clip(&mut self, clip_id: &str, cx: &mut Context<Self>) {
        let _ = self.timeline.update(cx, |tl, cx| {
            tl.state.select_clip(clip_id);
            cx.notify();
        });
    }

    pub(crate) fn open_editor_bottom_panel(&mut self, cx: &mut Context<Self>) {
        self.panels.mixer_docked = true;
        self.sync_timeline_chrome_metrics(cx);
        self.set_active_bottom_tab(components::BottomTab::Editor, cx);
        cx.notify();
    }

    pub(crate) fn open_midi_editor_bottom_panel(&mut self, cx: &mut Context<Self>) {
        self.open_editor_bottom_panel(cx);
    }

    fn dispatch_midi_editor_menu_command(&mut self, command_id: &str, cx: &mut Context<Self>) {
        let roll = if self.midi_editor.window.is_some() {
            self.piano_roll_floating.clone()
        } else {
            self.piano_roll.clone()
        };
        let cmd = command_id.to_string();
        let _ = roll.update(cx, |pr, cx| pr.run_menu_command(&cmd, cx));
        cx.notify();
    }

    fn panel_chrome_state(&self, cx: &mut Context<Self>) -> components::PanelChromeState {
        let make_handler = |command_id: &'static str| {
            let this = cx.entity().clone();
            Arc::new(move |_: &(), window: &mut Window, cx: &mut gpui::App| {
                let bounds = window.bounds();
                let _ = this.update(cx, |this, cx| {
                    this.dispatch_command_id_from_bounds(command_id, Some(bounds), cx);
                    cx.notify();
                });
            })
        };
        components::PanelChromeState {
            browser_visible: self.panels.browser,
            inspector_visible: self.panels.inspector,
            mixer_visible: self.mixer_panel_chrome_visible(),
            on_toggle_browser: make_handler("panel:toggle-browser"),
            on_toggle_mixer: make_handler("panel:toggle-mixer"),
            on_toggle_inspector: make_handler("panel:toggle-inspector"),
        }
    }

    fn sync_settings_to_systems(&mut self, cx: &mut Context<Self>) {
        let schema = self.settings.read(cx).current.clone();

        if let Some(ref engine) = self.audio_bridge.engine {
            // Dropout Protection is a lightweight atomic — push it every sync; no
            // engine graph rebuild (it never changes the audio graph). Done before
            // the PDC block so the (immutable) engine borrow ends before the
            // `schedule_audio_project_sync` mutable-self call below.
            engine.set_dropout_protection_mode(engine_dropout_mode(
                schema.playback.dropout_protection,
            ));

            let desired_pdc = schema.playback.latency_compensation;
            if engine.pdc_enabled() != desired_pdc {
                engine.set_pdc_enabled(desired_pdc);
                self.schedule_audio_project_sync(cx, false, "pdc_setting");
            }
        }

        // 1. Sync metronome enabled state
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.state.transport.metronome_enabled = schema.recording.metronome.enabled;
        });
        self.sync_metronome_controls(cx);

        // 2. Sync audio engine settings
        self.sync_audio_engine_settings(cx);
    }

    fn sync_audio_engine_settings(&mut self, cx: &mut Context<Self>) {
        let schema = self.settings.read(cx).current.clone();
        let backend = native_audio_backend_from_driver_type(&schema.hardware.audio.driver_type);

        // Do not claim the first ASIO driver while Settings is still enumerating
        // the driver list. ASIO drivers are commonly single-client; opening one
        // here would make the immediately-following enumeration return empty.
        // Selecting the unified ASIO Device row fills `device_out` (and mirrors
        // `device_in`), then the normal sync below reopens the chosen driver.
        if backend == DirectAudio::AudioBackend::Asio
            && schema.hardware.audio.device_out.trim().is_empty()
        {
            return;
        }

        if let Some(engine) = self.audio_bridge.engine.as_mut() {
            let output_device = resolve_output_device_for_backend(
                engine,
                backend,
                &schema.hardware.audio.device_out,
            );
            // A *live* engine must never silently swap its timing rate from a
            // generic settings sync — sample-rate changes go exclusively through
            // the explicit "Re-open Project" flow (`restart_audio_for_sample_rate`).
            // So while the stream is open we pin the current active rate here and
            // let other device-shaping changes (backend / device / buffer size)
            // reopen without touching timing. When the stream is closed there is
            // no live timing to disrupt, so the schema rate may apply directly.
            let sample_rate = if engine.stats().stream_open {
                engine.config().sample_rate
            } else {
                schema.general.project_defaults.sample_rate
            };
            let desired_config = DirectAudio::EngineConfig {
                sample_rate,
                buffer_size: schema.general.project_defaults.buffer_size,
                channels: 2,
                backend,
                input_device: None,
                output_device,
            };
            let stats_before = engine.stats();
            let needs_reopen = engine.requires_restart(&desired_config)
                || stats_before.device_state.eq_ignore_ascii_case("DeviceLost");
            if needs_reopen {
                eprintln!(
                    "[audio] settings changed, reopening DirectAudio stream backend={:?} sr={} buf={}",
                    desired_config.backend, desired_config.sample_rate, desired_config.buffer_size
                );
                match engine.reopen_with_config(desired_config) {
                    Ok(()) => {
                        let stats = engine.stats();
                        self.audio_bridge.stats = Some(stats.clone());
                        self.audio_bridge.running = true;
                        self.audio_bridge.last_error = None;
                        eprintln!(
                            "[audio] settings sync: stream reopened. backend={} sr={} buf={}",
                            stats.backend_name, stats.sample_rate, stats.buffer_size
                        );
                        self.audio_bridge.project_dirty = true;
                        self.schedule_audio_project_sync(cx, true, "audio_settings_reopen");
                    }
                    Err(error) => {
                        let message = error.to_string();
                        eprintln!("[audio] settings sync: reopen failed: {message}");
                        self.audio_bridge.stats = Some(engine.stats());
                        self.audio_bridge.last_error = Some(message);
                    }
                }
            }
            return;
        }

        let rebuild = true;

        if rebuild {
            eprintln!("[audio] settings changed, rebuilding audio engine stream...");

            // Stop and release active engine
            if let Some(mut engine) = self.audio_bridge.engine.take() {
                let _ = engine.stop();
            }

            // Construct new config
            let config = DirectAudio::EngineConfig {
                sample_rate: schema.general.project_defaults.sample_rate,
                buffer_size: schema.general.project_defaults.buffer_size,
                channels: 2,
                backend,
                input_device: None,
                output_device: None,
            };

            // Build new engine
            match DirectAudio::AudioEngine::new(config) {
                Ok(mut engine) => {
                    match engine.start() {
                        Ok(()) => {
                            let stats = engine.stats();
                            eprintln!(
                                "[audio] settings sync: stream rebuilt and started. backend={} sr={} buf={}",
                                stats.backend_name, stats.sample_rate, stats.buffer_size
                            );

                            // Re-bind timeline callbacks to the replacement engine.
                            self.install_audio_callbacks(&engine, cx);

                            self.audio_bridge.engine = Some(engine);
                            self.audio_bridge.running = true;
                            self.audio_bridge.last_error = None;
                        }
                        Err(error) => {
                            eprintln!("[audio] settings sync: warm-up failed: {error}");
                            self.audio_bridge.last_error = Some(error.to_string());
                        }
                    }
                }
                Err(error) => {
                    eprintln!("[audio] settings sync: failed to initialize engine: {error}");
                    self.audio_bridge.last_error = Some(error.to_string());
                }
            }
        }
    }

    /// Map a keystroke to a shared menu command ID. Keys mirror the
    /// `transport:*` IDs from `packages/shared/generated/native-menu.json`
    /// so the keyboard and menu paths fan into the same dispatcher.
    /// Text-input guarding is N/A here because GPUI delivers key events
    /// only when nothing focusable consumes them; if/when text inputs
    /// land in the studio surface, gate this on `event.bubble_phase`.
    /// Resolve a key event to a command id under the active shortcut profile.
    /// Profiles are data-driven (`packages/keymaps/*.json`). `Ctrl+Shift+P` keeps
    /// a special case for the command palette since that command is not always
    /// present in a user-supplied map.
    fn shortcut_command_id(&self, event: &KeyDownEvent) -> Option<String> {
        if let Some(command) = self.keymap_manager.command_for_event(event) {
            return Some(command.to_string());
        }
        let mods = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        if (mods.control || mods.platform)
            && mods.shift
            && !mods.alt
            && !mods.function
            && matches!(key, "p" | "P")
        {
            return Some("tools:command-palette".to_string());
        }
        None
    }

    fn spawn_timeline_audio_import_jobs(
        &mut self,
        cx: &mut Context<Self>,
        timeline: Entity<components::timeline::Timeline>,
        path: PathBuf,
        _path_key: String,
    ) {
        // Read `folder_path` from the already-leased `self` rather than
        // `owner.read(cx)`. Every caller invokes this from inside the
        // StudioLayout entity lease (a `this.update(cx, …)` closure or a
        // `&mut self` method such as `commit_recording_results`), so reading or
        // updating the entity again would double-lease panic
        // ("cannot read StudioLayout while it is already being updated").
        let project_root = self.project_session.folder_path.clone();
        // Unsaved workspaces intentionally import from the real source path.
        // The import pipeline keeps peaks in memory until a project folder
        // exists; saving later copies the audio into the project asset folder.
        // `cx.entity()` only clones the handle (no lease); the downstream
        // `spawn_timeline_import_from_layout` merely downgrades it and does all
        // real work in `cx.spawn`, so handing it the entity here is safe.
        let owner = cx.entity().clone();
        components::timeline::audio_import::spawn_timeline_import_from_layout(
            path,
            project_root,
            timeline,
            owner,
            cx,
        );
    }
}
