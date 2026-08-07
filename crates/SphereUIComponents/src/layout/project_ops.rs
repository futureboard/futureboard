use gpui::Context;

use std::path::PathBuf;
use std::sync::Arc;

use crate::components::message_box_dialog::{
    open_message_box_window, MessageBoxKind, MessageBoxOptions, MessageBoxResult,
};
use crate::components::project_switcher::ProjectSwitcherState;
use crate::components::timeline::timeline_state::{
    self, CreateTrackOptions, InputMonitorMode, TimelineState, TrackType,
};
use crate::project::{
    io::create_project_folder, io::project_backup_path, io::save_project, io::verify_project_file,
    now_secs, ClipSource, FutureboardProject, ProjectCreateOptions, ProjectSession,
    ProjectTemplate,
};

use super::StudioLayout;

macro_rules! project_lifecycle_log {
    ($($arg:tt)*) => {
        eprintln!("[Project] {}", format!($($arg)*));
    };
}

/// A project-lifecycle action that must be guarded by the unsaved-changes
/// prompt (New / Open). Close / Quit use [`super::close_ops::PendingCloseAction`].
#[derive(Debug, Clone, Copy)]
pub(super) enum LifecycleAction {
    /// Replace the current project with a fresh empty workspace.
    NewProject,
    /// Show the Open Project file picker (replaces the current project).
    OpenProject,
}

#[derive(Debug, Clone)]
enum SaveThenAction {
    PendingClose,
    Lifecycle(LifecycleAction),
    ProjectSwitch(crate::layout::project_switch::ProjectSwitchRequest),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProjectOpenOptions {
    pub from_recent: bool,
}

fn project_save_path_from_picker(path: PathBuf) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path;
    };
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(crate::project::io::sanitize_project_name)
        .unwrap_or_else(|| "Untitled Project".to_string());
    let already_project_folder = parent
        .file_name()
        .and_then(|name| name.to_str())
        .map(|folder| folder == stem)
        .unwrap_or(false);
    if already_project_folder {
        path
    } else {
        parent
            .join(&stem)
            .join(format!("{stem}.{}", crate::project::io::PROJECT_FILE_EXT))
    }
}

impl StudioLayout {
    /// Push the canonical [`ProjectSession`] into legacy workspace fields and UI
    /// chrome so every surface reads the same binding.
    pub(super) fn sync_project_session_to_workspace(&mut self, cx: &mut Context<Self>) {
        let session = self.project_session.clone();
        self.project_path = session.project_file_path.clone();
        self.project_folder = session.folder_path.clone();
        self.file_browser
            .set_project_folder(session.folder_path.clone());

        self.project_state = if session.project_file_path.is_some() && !session.is_untitled {
            crate::app_state::ProjectState::SavedProject {
                path: session.project_file_path.clone().unwrap(),
            }
        } else {
            crate::app_state::ProjectState::UnsavedWorkspace
        };

        self.project_switcher.current_project.name = session.display_name().to_string();
        self.project_switcher.current_project.path = session.project_file_path.clone();
        self.project_switcher.current_project.is_dirty = session.is_dirty;
        self.project_switcher.current_project.subtitle = session.subtitle().to_string();
        cx.notify();
    }

    fn apply_template_tracks(
        timeline: &mut crate::components::timeline::Timeline,
        template: ProjectTemplate,
        cx: &mut Context<crate::components::timeline::Timeline>,
    ) {
        let audio_count = template.audio_tracks();
        let midi_count = template.midi_tracks();
        for i in 0..audio_count {
            let color = timeline.state.track_color_for_index(i as usize);
            timeline.state.create_track(CreateTrackOptions {
                track_type: TrackType::Audio,
                name: format!("Audio {}", i + 1),
                color,
                volume: timeline_state::volume::db_to_norm(0.0),
                pan: 0.0,
                armed: false,
                input_monitor: InputMonitorMode::Off,
            });
        }
        for i in 0..midi_count {
            let color = timeline
                .state
                .track_color_for_index((audio_count + i) as usize);
            timeline.state.create_track(CreateTrackOptions {
                track_type: TrackType::Midi,
                name: format!("MIDI {}", i + 1),
                color,
                volume: timeline_state::volume::db_to_norm(0.0),
                pan: 0.0,
                armed: false,
                input_monitor: InputMonitorMode::Off,
            });
        }
        cx.notify();
    }

    fn initialize_timeline_for_new_workspace(
        &mut self,
        template: Option<ProjectTemplate>,
        bpm: f32,
        time_signature_num: u32,
        time_signature_den: u32,
        cx: &mut Context<Self>,
    ) {
        // Copy the application-level Default Audio Connections template. The
        // copy gets fresh project-local ids, so the new project is never linked
        // back to the template.
        let (connection_template, input_device, output_device) = {
            let settings = self.settings.read(cx);
            (
                settings.current.hardware.default_audio_connections.clone(),
                settings.current.hardware.audio.device_in.trim().to_string(),
                settings
                    .current
                    .hardware
                    .audio
                    .device_out
                    .trim()
                    .to_string(),
            )
        };
        let ports = crate::audio_connections::current_available_ports();

        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.reset_input_state();
            timeline.state = TimelineState::default();
            timeline.state.bpm = bpm;
            timeline.state.time_signature_num = time_signature_num;
            timeline.state.time_signature_den = time_signature_den;
            timeline.state.audio_connections =
                crate::audio_connections::AudioConnectionRegistry::from_default_template(
                    &connection_template,
                    &ports,
                    &input_device,
                    &output_device,
                );
            // Give Master the copied output where that is unambiguous; Monitor
            // stays on Follow Master Output. The latch then stops the
            // compatibility bootstrap from running again on the first save.
            let bootstrap = crate::output_routing::bootstrap_master_output(
                &mut timeline.state.audio_connections,
                &ports,
                false,
            );
            if let Some(id) = bootstrap.assigned() {
                timeline.state.master_output_connection_id = Some(id.clone());
            }
            timeline.state.output_routing_initialized = true;
            timeline.state.refresh_output_labels();
            if let Some(template) = template {
                Self::apply_template_tracks(timeline, template, cx);
            }
            cx.notify();
        });
        self.publish_audio_connection_routing(cx);
    }

    fn show_project_lifecycle_error(&mut self, title: &str, message: &str, cx: &mut Context<Self>) {
        self.show_project_open_failed_dialog(
            title,
            message,
            None,
            None,
            ProjectOpenOptions::default(),
            cx,
        );
    }

    pub fn show_project_open_failed_dialog(
        &mut self,
        title: &str,
        message: &str,
        detail: Option<String>,
        failed_path: Option<PathBuf>,
        options: ProjectOpenOptions,
        cx: &mut Context<Self>,
    ) {
        project_lifecycle_log!("error: {title}: {message}");
        self.lifecycle_guard.pending_failed_open_path = failed_path.clone();
        let owner_bounds = crate::window_position::resolve_owner_bounds_with_preferred(
            self.window_hooks.cached_bounds,
            self.studio_window_bounds(cx),
            cx,
        );
        let mut buttons = Vec::new();
        let mut backup_index = None;
        let mut remove_recent_index = None;
        let mut locate_index = None;

        if let Some(path) = failed_path.as_ref() {
            let backup = project_backup_path(path);
            if backup.exists() {
                backup_index = Some(buttons.len());
                buttons.push("Open Backup".to_string());
            }
        }
        if options.from_recent {
            remove_recent_index = Some(buttons.len());
            buttons.push("Remove from Recent".to_string());
            locate_index = Some(buttons.len());
            buttons.push("Locate Project".to_string());
        }
        buttons.push("OK".to_string());
        let ok_index = buttons.len() - 1;

        let dialog = MessageBoxOptions {
            kind: MessageBoxKind::Error,
            title: title.to_string(),
            message: message.to_string(),
            detail,
            buttons,
            default_id: ok_index,
            cancel_id: Some(ok_index),
        };

        let owner = cx.entity().clone();
        let failed_path_for_dialog = failed_path.clone();
        let on_response: Arc<
            dyn Fn(MessageBoxResult, &mut gpui::Window, &mut gpui::App) + Send + Sync,
        > = Arc::new(move |result, _window, cx| {
            let _ = owner.update(cx, |this, cx| {
                this.lifecycle_guard.pending_failed_open_path = None;
                let Some(path) = failed_path_for_dialog.clone() else {
                    return;
                };
                if backup_index == Some(result.response) {
                    this.load_project_from_path_with_options(
                        project_backup_path(&path),
                        ProjectOpenOptions::default(),
                        cx,
                    );
                    return;
                }
                if remove_recent_index == Some(result.response) {
                    this.recent_projects.remove(&path);
                    this.sync_recent_to_switcher();
                    cx.notify();
                    return;
                }
                if locate_index == Some(result.response) {
                    this.cmd_open_project(cx);
                }
            });
        });
        let _ = open_message_box_window(owner_bounds, dialog, on_response, cx);
    }

    /// Resolve the directory new projects should default to. Reads the
    /// user-configured default project directory from settings (falling back to
    /// the platform default), then best-effort creates it so the save dialog
    /// opens somewhere that exists. Never panics on a bad/missing path.
    pub(super) fn default_projects_dir(&self, cx: &Context<Self>) -> PathBuf {
        let dir = cx
            .try_global::<crate::settings::GlobalSettingsModel>()
            .map(|g| g.0.read(cx).current.general.resolved_default_project_dir())
            .unwrap_or_else(crate::project::io::default_projects_dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Authoritative project lifecycle state (Part G).
    pub fn project_state(&self) -> &crate::app_state::ProjectState {
        &self.project_state
    }

    /// Canonical project session for the current workspace.
    pub fn project_session(&self) -> &ProjectSession {
        &self.project_session
    }

    /// OS window title derived from the lifecycle state + dirty bit, e.g.
    /// `"Untitled Project — Unsaved"` / `"My Song — Saved"` (Part H).
    pub fn window_title(&self) -> String {
        self.project_state.window_title(
            self.project_session.display_name(),
            self.project_session.is_dirty,
        )
    }

    pub(super) fn reset_project(&mut self, cx: &mut Context<Self>) {
        // Replacing the live session invalidates any in-flight project-load
        // completion captured against the previous generation.
        self.advance_session_generation();
        self.project_session = ProjectSession::untitled();
        self.project_path = None;
        self.project_folder = None;
        self.file_browser.set_project_folder(None);
        self.project_switcher = ProjectSwitcherState::default();
        self.clip_clipboard.clear();
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.reset_input_state();
            timeline.state = TimelineState::default();
            cx.notify();
        });
        self.project_state = crate::app_state::ProjectState::UnsavedWorkspace;
    }

    // ── New project (no wizard) ────────────────────────────────────────────────

    /// Enter a fresh, empty, *unsaved* workspace. Replaces the old Project
    /// Wizard modal: there is no dialog, no folder is created, and nothing is
    /// written to disk until the user saves. The studio simply resets to a
    /// blank arrangement that is marked dirty/unsaved.
    pub fn new_empty_project(&mut self, cx: &mut Context<Self>) {
        self.reset_project(cx);
        self.project_session
            .bind_untitled("Untitled Project", false);
        self.sync_project_session_to_workspace(cx);
        self.mark_engine_media_dirty();
        self.schedule_audio_project_sync(cx, true, "new_empty_project");
        cx.notify();
    }

    /// Create a new unsaved workspace pre-populated from a `ProjectTemplate`.
    /// Like `new_empty_project`, this stays entirely in memory — the user saves
    /// when ready. Sample rate follows the current app defaults.
    pub fn new_project_from_template(&mut self, template: ProjectTemplate, cx: &mut Context<Self>) {
        self.reset_project(cx);

        let (ts_num, ts_den) = template.time_signature();
        self.initialize_timeline_for_new_workspace(
            Some(template),
            template.default_bpm(),
            ts_num,
            ts_den,
            cx,
        );

        self.project_session
            .bind_untitled(format!("Untitled {} Project", template.label()), true);
        self.sync_project_session_to_workspace(cx);
        self.mark_engine_media_dirty();
        self.schedule_audio_project_sync(cx, true, "new_template_project");
        cx.notify();
    }

    /// Create a named project from the Welcome screen. This is the first point
    /// where disk state is created: it makes the project folder tree, writes the
    /// `.fbproj`, updates recents, and leaves the workspace in `SavedProject`.
    pub fn create_saved_project_from_options(
        &mut self,
        options: ProjectCreateOptions,
        cx: &mut Context<Self>,
    ) {
        project_lifecycle_log!(
            "create requested name={} dir={} template={}",
            options.name,
            options.base_dir.display(),
            options.template.label()
        );
        let safe_name = crate::project::io::sanitize_project_name(&options.name);
        let folder = match create_project_folder(&options.base_dir, &safe_name) {
            Ok(folder) => folder,
            Err(e) => {
                eprintln!("[Project] folder create failed: {e}");
                self.project_state = crate::app_state::ProjectState::Error(e.to_string());
                self.project_switcher.current_project.subtitle = format!("Create failed: {e}");
                cx.notify();
                return;
            }
        };
        project_lifecycle_log!("folder created: {}", folder.display());
        let final_name = folder
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| safe_name.clone());
        let path = folder.join(format!(
            "{}.{}",
            final_name,
            crate::project::io::PROJECT_FILE_EXT
        ));
        project_lifecycle_log!("file target: {}", path.display());

        self.clip_clipboard.clear();
        let template = if options.template == ProjectTemplate::Empty {
            None
        } else {
            Some(options.template)
        };
        self.initialize_timeline_for_new_workspace(
            template,
            options.bpm,
            options.time_signature_num,
            options.time_signature_den,
            cx,
        );

        let now = now_secs();
        self.project_session.bind_saved(
            ProjectSession::fresh_id(),
            final_name.clone(),
            Some(folder.clone()),
            path.clone(),
            now,
            now,
        );
        self.project_session.is_dirty = true;
        project_lifecycle_log!(
            "binding current session: name={} path={}",
            final_name,
            path.display()
        );
        self.sync_project_session_to_workspace(cx);

        project_lifecycle_log!("save requested: mode=save path={}", path.display());
        if self.do_save_project(&path, cx) {
            project_lifecycle_log!("save complete: {}", path.display());
            if let Err(e) = verify_project_file(&path) {
                project_lifecycle_log!("verify after create failed: {}", e.technical_detail());
                self.show_project_open_failed_dialog(
                    "Create Project Failed",
                    "The project folder was created, but the project file could not be verified.",
                    Some(format!("Details: {}", e.technical_detail())),
                    Some(path.clone()),
                    ProjectOpenOptions::default(),
                    cx,
                );
                return;
            }
            self.mark_engine_media_dirty();
            self.schedule_audio_project_sync(cx, true, "project_created");
        } else {
            project_lifecycle_log!(
                "save failed after create — session remains bound to {}",
                path.display()
            );
        }
    }

    /// Run a guarded lifecycle action *after* the dirty-project guard has been
    /// satisfied (not dirty, Don't Save, or a successful Save).
    pub(super) fn run_lifecycle_action(&mut self, action: LifecycleAction, cx: &mut Context<Self>) {
        match action {
            LifecycleAction::NewProject => self.new_empty_project(cx),
            LifecycleAction::OpenProject => self.cmd_open_project(cx),
        }
    }

    /// Save, then run a pending close/quit action if save succeeds.
    pub(super) fn save_close_then(&mut self, cx: &mut Context<Self>) {
        self.save_then(SaveThenAction::PendingClose, cx);
    }

    /// Save, then run a pending New/Open lifecycle action if save succeeds.
    pub(super) fn save_lifecycle_then(&mut self, action: LifecycleAction, cx: &mut Context<Self>) {
        self.save_then(SaveThenAction::Lifecycle(action), cx);
    }

    /// Save, then switch to the requested project if save succeeds.
    pub(super) fn save_project_switch_then(
        &mut self,
        request: crate::layout::project_switch::ProjectSwitchRequest,
        cx: &mut Context<Self>,
    ) {
        self.save_then(SaveThenAction::ProjectSwitch(request), cx);
    }

    fn save_then(&mut self, after_save: SaveThenAction, cx: &mut Context<Self>) {
        if !self.project_session.needs_save_as() {
            if let Some(path) = self.project_session.project_file_path.clone() {
                self.save_project_in_background_then(path, Some(after_save), cx);
                return;
            }
        }

        #[cfg(feature = "native-dialogs")]
        {
            let default_dir = self.default_projects_dir(cx);
            let name = self.project_session.name.clone();
            let entity = cx.entity().clone();
            cx.spawn(async move |_this, cx| {
                if crate::shutdown::ShutdownState::global().is_shutting_down() {
                    return;
                }
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Save Project As")
                    .set_directory(&default_dir)
                    .set_file_name(&format!(
                        "{}.{}",
                        crate::project::io::sanitize_project_name(&name),
                        crate::project::io::PROJECT_FILE_EXT
                    ))
                    .add_filter(
                        "Futureboard Project",
                        crate::project::io::SUPPORTED_PROJECT_FILE_EXTS,
                    )
                    .save_file()
                    .await;
                if let Some(handle) = result {
                    let path = project_save_path_from_picker(handle.path().to_path_buf());
                    let _ = entity.update(cx, |this, cx| {
                        if crate::shutdown::ShutdownState::global().is_shutting_down() {
                            return;
                        }
                        this.save_project_in_background_then(path, Some(after_save), cx);
                    });
                } else {
                    let _ = entity.update(cx, |this, _cx| {
                        this.lifecycle_guard.pending_close_action = None;
                        this.lifecycle_guard.pending_lifecycle_action = None;
                        this.clear_project_switch_pending();
                    });
                }
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            self.lifecycle_guard.pending_close_action = None;
            self.lifecycle_guard.pending_lifecycle_action = None;
            self.clear_project_switch_pending();
            eprintln!("[Project] Save Project As unavailable: native file dialogs disabled");
        }
    }

    fn apply_save_then(&mut self, after_save: SaveThenAction, cx: &mut Context<Self>) {
        match after_save {
            SaveThenAction::PendingClose => self.perform_pending_close(cx),
            SaveThenAction::Lifecycle(action) => self.run_lifecycle_action(action, cx),
            SaveThenAction::ProjectSwitch(request) => {
                self.execute_confirmed_project_switch(request, cx)
            }
        }
    }

    // ── Close project (post-confirmation) ───────────────────────────────────────

    /// Unload the current project/session and return the app to the Welcome
    /// screen, keeping the application running. Runs only after the
    /// unsaved-changes guard is satisfied. This is *not* an app quit — the
    /// WCO / OS window close button handles quitting via [`Self::request_quit`].
    pub(super) fn do_close_project(&mut self, cx: &mut Context<Self>) {
        if crate::shutdown::ShutdownState::global().is_shutting_down() {
            return;
        }
        if crate::loading_session::is_project_lifecycle_busy() {
            eprintln!("[ProjectClose] ignored — project lifecycle already in progress");
            return;
        }
        let owner_bounds = self.studio_window_bounds(cx);
        let Some(studio) = self.window_hooks.self_window.take() else {
            eprintln!("[ProjectClose] ignored — studio window handle missing");
            return;
        };

        if let Some(request_shutdown) = self.window_hooks.on_request_session_shutdown.clone() {
            request_shutdown(
                crate::session_shutdown::SessionShutdownReason::ProjectClose,
                owner_bounds,
                studio,
                cx,
            );
            return;
        }

        let snapshot = self.capture_session_shutdown_snapshot_for_loading(
            crate::session_shutdown::SessionShutdownReason::ProjectClose,
            cx,
        );
        if let Err(error) = crate::session_shutdown::run_session_shutdown(snapshot, |_| {}) {
            eprintln!(
                "[ProjectClose] shutdown failed step={:?} error={}",
                error.step, error.message
            );
        }
        self.reset_project(cx);
        if !crate::shutdown::ShutdownState::global().is_shutting_down() {
            self.mark_engine_media_dirty();
            self.schedule_audio_project_sync(cx, true, "close_project");
        }
        if let Some(request_welcome) = self.window_hooks.on_request_welcome.clone() {
            request_welcome(cx);
        }
        if let Some(handle) = Some(studio) {
            cx.spawn(async move |_this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(0))
                    .await;
                if crate::shutdown::ShutdownState::global().is_shutting_down() {
                    return;
                }
                let _ = handle.update(cx, |_studio, window, cx| {
                    crate::window_position::persist_studio_window_from_window(window, cx);
                    window.remove_window();
                });
            })
            .detach();
        }
        if !crate::shutdown::ShutdownState::global().is_shutting_down() {
            cx.notify();
        }
    }

    // ── Save / load ───────────────────────────────────────────────────────────

    pub(super) fn mark_dirty(&mut self) {
        self.project_session.mark_dirty();
        self.project_switcher.current_project.is_dirty = true;
        self.project_switcher.current_project.subtitle = "Unsaved changes".to_string();
        self.mark_engine_project_dirty();
    }

    /// Mark the project session dirty for save/restore WITHOUT marking the audio
    /// engine dirty. Used by view-only state that is persisted in the project file
    /// but is not part of the engine graph snapshot — e.g. mixer-tree sidebar
    /// expand/collapse/pin/visibility/selection and the VSTi multi-out collapse
    /// flag. Routing these through [`Self::mark_dirty`] used to set
    /// `audio_bridge.project_dirty`, which forced the next poll to rebuild a full
    /// engine snapshot (serialize + dedup) on every tree interaction — pure waste
    /// because the snapshot never changes. See [`Self::mark_engine_project_dirty`].
    pub(crate) fn mark_dirty_view_only(&mut self) {
        self.project_session.mark_dirty();
        self.project_switcher.current_project.is_dirty = true;
        self.project_switcher.current_project.subtitle = "Unsaved changes".to_string();
    }

    pub(super) fn cmd_save_project(&mut self, cx: &mut Context<Self>) {
        if self.project_session.needs_save_as() {
            project_lifecycle_log!("save requested: mode=save_as path=<none>");
            self.cmd_save_project_as(cx);
        } else if let Some(path) = self.project_session.project_file_path.clone() {
            project_lifecycle_log!("save requested: mode=save path={}", path.display());
            self.save_project_in_background(path, cx);
        } else {
            project_lifecycle_log!("save requested: mode=save_as path=<none>");
            self.cmd_save_project_as(cx);
        }
    }

    pub(super) fn cmd_save_project_as(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "native-dialogs")]
        {
            let default_dir = self
                .project_session
                .project_file_path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .or_else(|| self.project_session.folder_path.clone())
                .unwrap_or_else(|| self.default_projects_dir(cx));
            let name = self.project_session.name.clone();
            let entity = cx.entity().clone();
            cx.spawn(async move |_this, cx| {
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Save Project As")
                    .set_directory(&default_dir)
                    .set_file_name(&format!(
                        "{}.{}",
                        crate::project::io::sanitize_project_name(&name),
                        crate::project::io::PROJECT_FILE_EXT
                    ))
                    .add_filter(
                        "Futureboard Project",
                        crate::project::io::SUPPORTED_PROJECT_FILE_EXTS,
                    )
                    .save_file()
                    .await;
                if let Some(handle) = result {
                    let path = project_save_path_from_picker(handle.path().to_path_buf());
                    let _ = entity.update(cx, |this, cx| {
                        project_lifecycle_log!(
                            "save requested: mode=save_as path={}",
                            path.display()
                        );
                        this.save_project_in_background(path, cx);
                    });
                }
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            let _ = cx;
            eprintln!("[Project] Save Project As unavailable: native file dialogs disabled");
        }
    }

    pub(super) fn cmd_save_project_copy(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "native-dialogs")]
        {
            let default_dir = self
                .project_session
                .project_file_path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .or_else(|| self.project_session.folder_path.clone())
                .unwrap_or_else(|| self.default_projects_dir(cx));
            let name = self.project_session.name.clone();
            let entity = cx.entity().clone();
            self.refresh_bridge_plugin_states(cx);
            let tl_state = self.timeline.read(cx).state.clone();
            let sample_rate = self.current_audio_sample_rate();
            cx.spawn(async move |_this, cx| {
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Save Copy")
                    .set_directory(&default_dir)
                    .set_file_name(&format!(
                        "{} Copy.{}",
                        crate::project::io::sanitize_project_name(&name),
                        crate::project::io::PROJECT_FILE_EXT
                    ))
                    .add_filter(
                        "Futureboard Project",
                        crate::project::io::SUPPORTED_PROJECT_FILE_EXTS,
                    )
                    .save_file()
                    .await;
                if let Some(handle) = result {
                    let path = handle.path().to_path_buf();
                    let mut project = FutureboardProject::from(&tl_state);
                    project.settings.sample_rate = sample_rate;
                    let _ = entity.update(cx, |_this, _cx| {
                        if let Err(e) = save_project(&mut project, &path) {
                            eprintln!("[Project] save copy failed: {e}");
                        }
                    });
                }
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            let _ = cx;
            eprintln!("[Project] Save Copy unavailable: native file dialogs disabled");
        }
    }

    /// Persist the project to `path`. Returns `true` on success so callers
    /// (notably the unsaved-changes guard) can decide whether to continue.
    pub(super) fn do_save_project(&mut self, path: &PathBuf, cx: &mut Context<Self>) -> bool {
        self.refresh_bridge_plugin_states(cx);
        let mut project = self.project_snapshot(cx);
        match save_project(&mut project, path) {
            Ok(()) => {
                self.finish_project_save(project, path.clone(), cx);
                true
            }
            Err(e) => {
                self.handle_project_save_error(e.to_string(), cx);
                false
            }
        }
    }

    fn save_project_in_background(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.save_project_in_background_then(path, None, cx);
    }

    fn save_project_in_background_then(
        &mut self,
        path: PathBuf,
        after_save: Option<SaveThenAction>,
        cx: &mut Context<Self>,
    ) {
        self.refresh_bridge_plugin_states(cx);
        let mut project = self.project_snapshot(cx);
        // Stamp the session this save belongs to. A plain background save sets no
        // lifecycle-busy flag, so a project switch/reset can land while the write
        // is in flight; without this guard the completion below would rebind
        // `project_session` (name/path/recent, mark saved) — stamping the old
        // project's identity onto the session that replaced it. See
        // `advance_session_generation`.
        let generation = self.session_generation();
        self.project_switcher.current_project.subtitle = "Saving...".to_string();
        self.start_background_task(
            "project-save",
            crate::components::BackgroundTaskKind::ProjectSave,
            "Save project",
            Some(path.to_string_lossy().to_string()),
            None,
            false,
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path_for_job = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { save_project(&mut project, &path_for_job).map(|_| project) })
                .await;
            let _ = this.update(cx, move |this, cx| {
                // The file is already written to disk regardless of the outcome
                // below. Only skip the *session-state* mutation when the session
                // was replaced mid-save.
                let superseded = this.session_generation() != generation;
                match result {
                    Ok(project) => {
                        if superseded {
                            this.complete_background_task(
                                "project-save",
                                Some("Project saved (session changed)".to_string()),
                            );
                            project_lifecycle_log!(
                                "save completed for superseded session — skipping rebind/after-save"
                            );
                            return;
                        }
                        this.finish_project_save(project, path, cx);
                        this.complete_background_task(
                            "project-save",
                            Some("Project saved".to_string()),
                        );
                        project_lifecycle_log!("save complete");
                        if let Some(after_save) = after_save {
                            this.apply_save_then(after_save, cx);
                        }
                    }
                    Err(e) => {
                        let error = e.to_string();
                        this.fail_background_task("project-save", error.clone());
                        if superseded {
                            project_lifecycle_log!(
                                "save failed for superseded session — not surfacing on new session"
                            );
                            return;
                        }
                        this.handle_project_save_error(error, cx);
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn project_snapshot(&self, cx: &mut Context<Self>) -> FutureboardProject {
        let tl_state = self.timeline.read(cx).state.clone();
        let mut project = FutureboardProject::from(&tl_state);
        project.id = self.project_session.id.clone();
        project.name = self.project_session.name.clone();
        project.created_at = self.project_session.created_at;
        project.modified_at = self.project_session.modified_at;
        project.settings.sample_rate = self.current_audio_sample_rate();
        project
    }

    fn finish_project_save(
        &mut self,
        project: FutureboardProject,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.sync_timeline_audio_paths_after_save(&project, &path, cx);
        let folder = path.parent().map(PathBuf::from);
        self.project_session.bind_saved(
            project.id,
            project.name.clone(),
            folder.clone(),
            path.clone(),
            project.created_at,
            project.modified_at,
        );
        project_lifecycle_log!(
            "current session updated: name={} path={}",
            self.project_session.name,
            path.display()
        );
        self.sync_project_session_to_workspace(cx);
        self.recent_projects
            .push(&project.name, path.clone(), now_secs());
        self.sync_recent_to_switcher();
        cx.notify();
    }

    fn handle_project_save_error(&mut self, error: String, cx: &mut Context<Self>) {
        eprintln!("[Project] save failed: {error}");
        self.project_switcher.current_project.subtitle = format!("Save failed: {error}");
        self.clear_project_switch_pending();
        cx.notify();
    }

    pub(super) fn maybe_autosave_project(&mut self, cx: &mut Context<Self>) {
        let autosave = self.settings.read(cx).current.general.autosave.clone();
        if !autosave.enabled || !self.project_session.is_dirty {
            return;
        }
        if self.autosave_in_flight {
            return;
        }
        let interval_minutes = autosave.interval_minutes.clamp(1, 240) as u64;
        if self.last_autosave_at.elapsed() < std::time::Duration::from_secs(interval_minutes * 60) {
            return;
        }

        let path = self.autosave_project_path();
        self.autosave_in_flight = true;
        self.last_autosave_at = std::time::Instant::now();
        self.refresh_bridge_plugin_states(cx);
        let mut project = self.project_snapshot(cx);
        self.start_background_task(
            "project-autosave",
            crate::components::BackgroundTaskKind::ProjectSave,
            "Autosave project",
            Some(path.to_string_lossy().to_string()),
            None,
            false,
        );
        cx.spawn(async move |this, cx| {
            let path_for_job = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { save_project(&mut project, &path_for_job) })
                .await;
            let _ = this.update(cx, move |this, cx| {
                this.autosave_in_flight = false;
                match result {
                    Ok(()) => {
                        this.complete_background_task(
                            "project-autosave",
                            Some("Autosave written".to_string()),
                        );
                        eprintln!("[Project] autosave written: {}", path.display());
                    }
                    Err(e) => {
                        let error = e.to_string();
                        this.fail_background_task("project-autosave", error.clone());
                        eprintln!("[Project] autosave failed: {error}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn session_autosave_flush_payload(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (Option<PathBuf>, Option<FutureboardProject>) {
        if !self.project_session.is_dirty {
            return (None, None);
        }
        // Project-switch shutdown bypasses the normal Save/Autosave entry
        // points, so capture live plugin state before building its recovery
        // snapshot too.
        self.refresh_bridge_plugin_states(cx);
        (
            Some(self.autosave_project_path()),
            Some(self.project_snapshot(cx)),
        )
    }

    fn autosave_project_path(&self) -> PathBuf {
        if let Some(path) = self.project_session.project_file_path.as_ref() {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("project");
            return path.with_file_name(format!("{stem}.autosave.fbproj"));
        }
        let dir = self.paths.app_data.join("Autosaves");
        dir.join(format!("{}.autosave.fbproj", self.project_session.id))
    }

    fn sync_timeline_audio_paths_after_save(
        &mut self,
        project: &FutureboardProject,
        path: &PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(project_root) = path.parent().map(PathBuf::from) else {
            return;
        };
        let updates: std::collections::HashMap<String, String> = project
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter_map(|clip| {
                let ClipSource::Audio {
                    source_path: Some(source_path),
                    ..
                } = &clip.source
                else {
                    return None;
                };
                let resolved = if source_path.is_absolute() {
                    source_path.clone()
                } else {
                    project_root.join(source_path)
                };
                Some((clip.id.clone(), resolved.to_string_lossy().into_owned()))
            })
            .collect();

        if updates.is_empty() {
            return;
        }

        let changed = self.timeline.update(cx, |timeline, cx| {
            let mut changed = false;
            for track in &mut timeline.state.tracks {
                for clip in &mut track.clips {
                    let Some(new_path) = updates.get(&clip.id) else {
                        continue;
                    };
                    let crate::components::timeline::timeline_state::ClipType::Audio {
                        source_path,
                        ..
                    } = &mut clip.clip_type
                    else {
                        continue;
                    };
                    // Only update the resolvable location. `file_id` (the asset
                    // key) must stay stable so the waveform cache binding — keyed
                    // on `file_id`, not the path — survives the path rewrite.
                    if source_path.as_deref() != Some(new_path.as_str()) {
                        *source_path = Some(new_path.clone());
                        changed = true;
                    }
                }
            }
            if changed {
                cx.notify();
            }
            changed
        });

        if changed {
            self.mark_engine_media_dirty();
            self.schedule_audio_project_sync(cx, true, "project_save_asset_paths");
        }
    }

    pub(super) fn cmd_open_project(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "native-dialogs")]
        {
            let default_dir = self
                .project_session
                .project_file_path
                .as_ref()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .or_else(|| self.project_session.folder_path.clone())
                .unwrap_or_else(|| self.default_projects_dir(cx));
            let entity = cx.entity().clone();
            cx.spawn(async move |_this, cx| {
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Open Project")
                    .set_directory(&default_dir)
                    .add_filter(
                        "Futureboard Project",
                        crate::project::io::SUPPORTED_PROJECT_FILE_EXTS,
                    )
                    // Projects exported by another DAW open through the
                    // importers in `crate::project::import`.
                    .add_filter(
                        "Cubase XML Track Archive",
                        crate::project::IMPORT_PROJECT_FILE_EXTS,
                    )
                    .pick_file()
                    .await;
                if let Some(handle) = result {
                    let path = handle.path().to_path_buf();
                    let _ = entity.update(cx, |this, cx| {
                        this.load_project_from_path(path, cx);
                    });
                }
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            let _ = cx;
            eprintln!("[Project] Open Project unavailable: native file dialogs disabled");
        }
    }

    // `load_project_from_path_with_options` lives in `super::session_load`.

    pub(super) fn cmd_open_recent_project(
        &mut self,
        owner_bounds: Option<gpui::Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let idx = self.project_switcher.selected_index;
        if idx == 0 {
            return;
        }
        let entry = self
            .project_switcher_visible_entries()
            .get(idx.saturating_sub(1))
            .cloned();
        if let Some(project) = entry {
            if let Some(path) = project.path {
                self.request_switch_project(
                    crate::layout::project_switch::ProjectSwitchRequest {
                        target_path: path,
                        target_name: Some(project.name),
                        source: crate::layout::project_switch::ProjectSwitchSource::RecentProject,
                    },
                    owner_bounds,
                    cx,
                );
            }
        }
    }

    pub(super) fn cmd_reveal_project_folder(&self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "windows")]
        if let Some(folder) = &self.project_folder {
            let _ = std::process::Command::new("explorer").arg(folder).spawn();
        }
        #[cfg(target_os = "macos")]
        if let Some(folder) = &self.project_folder {
            let _ = std::process::Command::new("open").arg(folder).spawn();
        }
        #[cfg(target_os = "linux")]
        if let Some(folder) = &self.project_folder {
            let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
        }
    }

    /// Refresh the recent-projects `missing` flags off the UI thread.
    ///
    /// Per-entry `Path::exists()` stats are synchronous and can stall for
    /// hundreds of ms on cloud-backed (OneDrive/Dropbox) paths, so we snapshot
    /// the paths, stat them on the background executor, then apply the results
    /// and re-sync the switcher on the foreground. Cheap no-op when empty.
    pub(super) fn spawn_refresh_recent_missing(&mut self, cx: &mut Context<Self>) {
        let paths = self.recent_projects.entry_paths();
        if paths.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let missing: Vec<bool> = cx
                .background_executor()
                .spawn(async move { paths.iter().map(|p| !p.exists()).collect() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.recent_projects.apply_missing(&missing);
                this.sync_recent_to_switcher();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn sync_recent_to_switcher(&mut self) {
        // Pure / non-blocking: builds the switcher list from the already-cached
        // `missing` flags. Freshness is refreshed separately and off-thread via
        // `spawn_refresh_recent_missing` so this never stalls the UI on FS I/O.
        self.project_switcher.recent_projects = self
            .recent_projects
            .entries()
            .iter()
            .map(|e| crate::components::project_switcher::ProjectSummary {
                name: e.name.clone(),
                path: Some(e.path.clone()),
                is_current: self.project_session.project_file_path.as_ref() == Some(&e.path),
                is_dirty: false,
                subtitle: if e.missing {
                    "Missing".to_string()
                } else {
                    String::new()
                },
            })
            .collect();
    }
}
