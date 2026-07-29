//! Native "Export Arrangement" dialog.
//!
//! Compact, native-feeling Futureboard dialog (no web-style controls). It owns a
//! plain [`EngineProjectSnapshot`] + [`ExportProjectDefaults`] captured when the
//! window opens, edits an [`ExportSettings`], and — on Export — spawns a
//! background thread that runs the engine's `export_arrangement`. Progress flows
//! back through a shared `Mutex` polled by a GPUI timer loop; the worker thread
//! never touches GPUI and the UI never blocks. No entity is leased during the
//! render/encode work.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{
    div, px, App, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, WindowHandle,
};

use sphere_encoder::AudioFileFormat;
use DirectAudio::plugin_bridge::PluginBridgeSinkMap;
use DirectAudio::types::EngineProjectSnapshot;
use DirectAudio::{
    export_arrangement_with_bridges, export_tracks_single_pass_with_bridges,
    ArrangementExportSummary, ExportCancelToken, ExportProgress, ExportStage, TrackExportTarget,
};

use crate::components::form::select::{select, SelectOption};
use crate::components::progress_dialog::{progress_bar, ProgressBarValue};
use crate::components::text_input::{
    bind_mouse_selection, text_field_with_callbacks_and_ime,
};
use crate::components::title_bar::external_window_titlebar_compact;
use crate::components::title_bar::TITLEBAR_HEIGHT;
use crate::components::{TextInputAction, TextInputState};
use crate::theme::{self, Colors};
use gpui::AppContext;

use super::export_settings::{
    ExportChannelMode, ExportMode, ExportNormalizeChoice, ExportProjectDefaults, ExportRangeChoice,
    ExportSampleRateChoice, ExportSettings, ExportTailChoice,
};

pub const EXPORT_WINDOW_WIDTH: f32 = 560.0;
const EXPORT_WINDOW_HEIGHT: f32 = 585.0;
const BODY_PAD: f32 = 14.0;
const ROW_GAP: f32 = 9.0;
const LABEL_W: f32 = 96.0;
const CONTROL_H: f32 = 28.0;
const BUTTON_H: f32 = 28.0;

/// Lifecycle of the export job, surfaced in the window body.
pub enum ExportJobState {
    Editing,
    Running(ExportProgress),
    Complete(Vec<ArrangementExportSummary>),
    Failed(String),
    Cancelled,
}

#[derive(Default)]
struct ExportShared {
    progress: Option<ExportProgress>,
    done: Option<Result<Vec<ArrangementExportSummary>, String>>,
}

struct ExportJob {
    shared: Arc<Mutex<ExportShared>>,
    cancel: ExportCancelToken,
}

/// Detaches the live realtime plugin-bridge sinks for the duration of an export
/// and guarantees they are re-installed when the guard leaves scope — success,
/// error, or worker panic (Drop runs during unwind). Losing the restore would
/// leave every bridged insert silent in realtime playback after the export.
struct BridgeSinkHandoff<'a> {
    engine: Option<&'a DirectAudio::AudioEngine>,
    sinks: &'a PluginBridgeSinkMap,
}

impl<'a> BridgeSinkHandoff<'a> {
    fn detach(
        engine: Option<&'a DirectAudio::AudioEngine>,
        sinks: &'a PluginBridgeSinkMap,
    ) -> Self {
        if let Some(engine) = engine {
            if !sinks.is_empty() {
                for id in sinks.keys() {
                    let _ = engine.set_plugin_bridge_sink(id.clone(), None);
                }
                // Deterministic handoff: wait for the audio callback to ack that
                // the removals were applied before the offline worker starts
                // driving the shared bridge. Ack timeout (no open stream, paused
                // device, stalled callback) falls back to the old fixed grace
                // sleep rather than racing the callback.
                if !engine.wait_for_command_barrier(std::time::Duration::from_millis(500)) {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        }
        Self { engine, sinks }
    }
}

impl Drop for BridgeSinkHandoff<'_> {
    fn drop(&mut self) {
        if let Some(engine) = self.engine {
            for (id, sink) in self.sinks {
                let _ = engine.set_plugin_bridge_sink(id.clone(), Some(sink.clone()));
            }
        }
    }
}

/// Which dropdown is currently open (only one at a time).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectField {
    Mode,
    Format,
    FormatOption,
    Range,
    SampleRate,
    Channels,
    Normalize,
    Tail,
}

pub struct ExportArrangementWindow {
    project_name: String,
    snapshot: EngineProjectSnapshot,
    bridge_sinks: PluginBridgeSinkMap,
    audio_engine: Option<DirectAudio::AudioEngine>,
    defaults: ExportProjectDefaults,
    settings: ExportSettings,
    /// Editable export stem (mixdown file name / stems folder prefix).
    name_input: TextInputState,
    state: ExportJobState,
    open_select: Option<SelectField>,
    job: Option<ExportJob>,
    focus_handle: FocusHandle,
}

impl ExportArrangementWindow {
    pub fn new(
        project_name: String,
        snapshot: EngineProjectSnapshot,
        bridge_sinks: PluginBridgeSinkMap,
        audio_engine: Option<DirectAudio::AudioEngine>,
        defaults: ExportProjectDefaults,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut settings = ExportSettings::default();
        // Default output: <project>.wav in the temp dir as a safe fallback; the
        // opener can override with a project Exports folder.
        let file = ExportSettings::default_file_name(&project_name, settings.format);
        settings.output_path = Some(std::env::temp_dir().join(file));
        let stem = export_stem_from_input(&project_name);
        let mut name_input = TextInputState::new("export-file-name", cx.focus_handle())
            .with_placeholder("Export name");
        name_input.set_value(&stem);
        Self {
            project_name,
            snapshot,
            bridge_sinks,
            audio_engine,
            defaults,
            settings,
            name_input,
            state: ExportJobState::Editing,
            open_select: None,
            job: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Override the default output path (e.g. project Exports folder).
    pub fn set_default_output(&mut self, path: PathBuf) {
        self.settings.output_path = Some(path);
        self.sync_name_from_path();
    }

    fn export_name(&self) -> String {
        export_stem_from_input(&self.name_input.value)
    }

    /// Keep `output_path` in the same folder, with the name field as the stem.
    fn sync_output_from_name(&mut self) {
        let stem = self.export_name();
        let parent = self
            .settings
            .output_path
            .as_ref()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        let file = format!("{stem}.{}", self.settings.format.extension());
        self.settings.output_path = Some(parent.join(file));
    }

    fn sync_name_from_path(&mut self) {
        if let Some(stem) = self
            .settings
            .output_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .filter(|s| !s.trim().is_empty())
        {
            self.name_input.set_value(stem);
        }
    }

    fn subtitle(&self) -> String {
        let range = match self.settings.range {
            ExportRangeChoice::EntireArrangement => "Entire arrangement",
            ExportRangeChoice::TimeSelection { .. } => "Time selection",
            ExportRangeChoice::LoopRange { .. } => "Loop range",
            ExportRangeChoice::Custom { .. } => "Custom range",
        };
        let mode = self.settings.mode.label();
        if self.project_name.trim().is_empty() {
            format!("{mode} — {range}")
        } else {
            format!("{} — {mode} — {range}", self.project_name)
        }
    }

    fn close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        // Closing always cancels a running job so no orphaned worker keeps
        // writing after the window is gone.
        if let Some(job) = &self.job {
            job.cancel.cancel();
        }
        window.remove_window();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, ExportJobState::Editing | ExportJobState::Failed(_))
            && self.name_input.is_focused(window)
        {
            let action = self.name_input.handle_key_ime(event, Some(cx));
            self.sync_output_from_name();
            match action {
                TextInputAction::Submit => {
                    self.start_export(cx);
                }
                TextInputAction::Cancel => {
                    self.close(window, cx);
                }
                TextInputAction::Consumed | TextInputAction::Pass => {
                    cx.notify();
                }
            }
            return;
        }

        match event.keystroke.key.as_str() {
            "escape" => {
                if self.open_select.is_some() {
                    self.open_select = None;
                    cx.notify();
                } else if matches!(self.state, ExportJobState::Running(_)) {
                    self.request_cancel(cx);
                } else {
                    self.close(window, cx);
                }
            }
            "enter" if matches!(self.state, ExportJobState::Editing) => {
                self.start_export(cx);
            }
            _ => {}
        }
    }

    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &self.job {
            job.cancel.cancel();
        }
        cx.notify();
    }

    fn browse_output(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "native-dialogs")]
        {
            let entity = cx.entity().clone();
            let format = self.settings.format;
            let mode = self.settings.mode;
            let export_name = self.export_name();
            let start = self
                .settings
                .output_path
                .clone()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(std::env::temp_dir);
            let file = format!("{export_name}.{}", format.extension());
            cx.spawn(async move |_this, cx| {
                let dialog = rfd::AsyncFileDialog::new()
                    .set_title(if mode == ExportMode::Mixdown {
                        "Export Mixdown"
                    } else {
                        "Choose Export Folder"
                    })
                    .set_directory(&start);
                let result = if mode == ExportMode::Mixdown {
                    dialog
                        .set_file_name(&file)
                        .add_filter(format.as_str().to_uppercase(), &[format.extension()])
                        .save_file()
                        .await
                } else {
                    dialog.pick_folder().await
                };
                if let Some(handle) = result {
                    let path = if mode == ExportMode::Mixdown {
                        handle.path().to_path_buf()
                    } else {
                        handle.path().join(format!("{export_name}.{}", format.extension()))
                    };
                    let _ = entity.update(cx, |this, cx| {
                        this.settings.output_path = Some(path);
                        this.sync_name_from_path();
                        cx.notify();
                    });
                }
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            self.state =
                ExportJobState::Failed("Native file dialogs are unavailable in this build.".into());
            cx.notify();
        }
    }

    fn start_export(&mut self, cx: &mut Context<Self>) {
        self.open_select = None;
        self.sync_output_from_name();
        let request = match self.settings.to_request(&self.snapshot, &self.defaults) {
            Ok(request) => request,
            Err(err) => {
                self.state = ExportJobState::Failed(err.user_message());
                cx.notify();
                return;
            }
        };

        let shared = Arc::new(Mutex::new(ExportShared::default()));
        let cancel = ExportCancelToken::new();
        self.job = Some(ExportJob {
            shared: shared.clone(),
            cancel: cancel.clone(),
        });
        self.state = ExportJobState::Running(ExportProgress::stage_only(
            ExportStage::Preparing,
            request.render.content_frames(),
        ));

        // Worker thread: plain data only, never touches GPUI.
        let batch_targets = build_batch_targets(
            &request,
            self.settings.mode,
            &self.defaults,
            &self.export_name(),
        );
        if self.settings.mode != ExportMode::Mixdown && batch_targets.is_empty() {
            self.state = ExportJobState::Failed("No source tracks are available to export.".into());
            self.job = None;
            cx.notify();
            return;
        }
        let worker_shared = shared.clone();
        let worker_cancel = cancel.clone();
        let snapshot = self.snapshot.clone();
        let bridge_sinks = self.bridge_sinks.clone();
        let audio_engine = self.audio_engine.clone();
        std::thread::Builder::new()
            .name("fb-arrangement-export".to_string())
            .spawn(move || {
                let progress_shared = worker_shared.clone();
                let mut on_progress = move |progress| {
                    if let Ok(mut guard) = progress_shared.lock() {
                        guard.progress = Some(progress);
                    }
                };
                // Scope the sink handoff so the live sinks are re-installed
                // (guard Drop) before the terminal result is published below —
                // and on any panic path, since Drop runs during unwind.
                let result = {
                    let _bridge_handoff =
                        BridgeSinkHandoff::detach(audio_engine.as_ref(), &bridge_sinks);
                    if batch_targets.is_empty() {
                        export_arrangement_with_bridges(
                            &snapshot,
                            &request,
                            &worker_cancel,
                            Some(&bridge_sinks),
                            &mut on_progress,
                        )
                        .map(|summary| vec![summary])
                    } else {
                        export_tracks_single_pass_with_bridges(
                            &snapshot,
                            &batch_targets,
                            &worker_cancel,
                            Some(&bridge_sinks),
                            &mut on_progress,
                        )
                    }
                    .map_err(|error| error.to_string())
                };
                if let Ok(mut guard) = worker_shared.lock() {
                    guard.done = Some(result);
                }
            })
            .ok();

        // Poll loop: copy shared progress into UI state until terminal.
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            loop {
                if crate::shutdown::ShutdownState::global().is_shutting_down() {
                    break;
                }
                executor.timer(std::time::Duration::from_millis(50)).await;
                let keep_going = this
                    .update(cx, |this, cx| this.poll_job(cx))
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();

        cx.notify();
    }

    /// Apply the latest shared progress/result. Returns `false` once terminal.
    fn poll_job(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(job) = &self.job else {
            return false;
        };
        let (progress, done) = {
            let Ok(mut guard) = job.shared.lock() else {
                return true;
            };
            (guard.progress.take(), guard.done.take())
        };
        if let Some(done) = done {
            self.state = match done {
                Ok(summary) => ExportJobState::Complete(summary),
                Err(message) if job.cancel.is_cancelled() && message.contains("cancel") => {
                    ExportJobState::Cancelled
                }
                Err(message) => ExportJobState::Failed(message),
            };
            self.job = None;
            cx.notify();
            return false;
        }
        if let Some(progress) = progress {
            self.state = ExportJobState::Running(progress);
            cx.notify();
        }
        true
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

impl Render for ExportArrangementWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the destination path stem aligned with the name field (IME edits
        // notify without going through handle_key).
        if matches!(
            self.state,
            ExportJobState::Editing | ExportJobState::Failed(_)
        ) {
            self.sync_output_from_name();
        }
        let target = cx.entity().clone();
        let title = "Export Arrangement".to_string();

        let body = match &self.state {
            ExportJobState::Editing | ExportJobState::Failed(_) => {
                self.render_editing(window, target.clone())
            }
            ExportJobState::Running(progress) => {
                self.render_progress(progress.clone(), target.clone())
            }
            ExportJobState::Complete(summary) => {
                self.render_complete(summary.clone(), target.clone())
            }
            ExportJobState::Cancelled => self.render_terminal_message(
                "Export cancelled.",
                Colors::text_secondary(),
                target.clone(),
            ),
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(theme::ui_font())
            .bg(Colors::surface_base())
            .overflow_hidden()
            .rounded_md()
            .border(px(1.0))
            .border_color(Colors::border_subtle())
            .shadow(vec![gpui::BoxShadow {
                color: Colors::surface_overlay().into(),
                offset: gpui::point(px(0.0), px(6.0)),
                blur_radius: px(20.0),
                spread_radius: px(0.0),
                inset: false,
            }])
            .capture_key_down({
                let target = target.clone();
                move |event, window, cx| {
                    let _ = target.update(cx, |this, cx| this.handle_key(event, window, cx));
                }
            })
            .child(div().w(px(0.0)).h(px(0.0)).track_focus(&self.focus_handle))
            .child(external_window_titlebar_compact(
                title,
                "export-window-close",
                {
                    let target = target.clone();
                    move |window, cx| {
                        let _ = target.update(cx, |this, cx| this.close(window, cx));
                    }
                },
            ))
            .child(
                div()
                    .px(px(BODY_PAD))
                    .pt(px(6.0))
                    .text_size(px(11.0))
                    .text_color(Colors::text_muted())
                    .truncate()
                    .child(self.subtitle()),
            )
            .child(body)
    }
}

impl ExportArrangementWindow {
    fn render_editing(
        &self,
        window: &Window,
        target: gpui::Entity<Self>,
    ) -> gpui::AnyElement {
        let invalid = self.settings.validate(&self.defaults).err();

        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .px(px(BODY_PAD))
            .py(px(BODY_PAD))
            .gap(px(ROW_GAP))
            .child(section_label("DESTINATION"))
            .child(self.name_row(window, target.clone()))
            .child(self.output_row(target.clone()))
            .child(self.mode_row(target.clone()))
            .child(section_label("FORMAT"))
            .child(self.format_row(target.clone()))
            .child(self.format_option_row(target.clone()))
            .child(self.sample_rate_row(target.clone()))
            .child(self.channels_row(target.clone()))
            .child(section_label("RENDER"))
            .child(self.range_row(target.clone()))
            .child(self.normalize_row(target.clone()))
            .child(self.tail_row(target.clone()))
            .child(div().flex_1());

        if let ExportJobState::Failed(message) = &self.state {
            col = col.child(error_banner(message.clone()));
        } else if let Some(err) = &invalid {
            col = col.child(hint_banner(err.user_message()));
        }

        col = col.child(self.footer(invalid.is_none(), target));
        col.into_any_element()
    }

    fn labeled<E: IntoElement>(&self, label: &str, control: E) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("export-row-{label}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .w(px(LABEL_W))
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(Colors::text_secondary())
                    .child(label.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(control.into_any_element()),
            )
    }

    fn name_row(&self, window: &Window, target: gpui::Entity<Self>) -> impl IntoElement {
        let focused = self.name_input.is_focused(window);
        let callbacks = bind_mouse_selection(target.clone(), |this| &mut this.name_input);
        let control =
            text_field_with_callbacks_and_ime(&self.name_input, focused, callbacks, target);
        self.labeled(
            if self.settings.mode == ExportMode::Mixdown {
                "File name"
            } else {
                "Name"
            },
            control,
        )
    }

    fn output_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let path_label = self
            .settings
            .normalized_output_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "No file selected".to_string());
        let control = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h(px(CONTROL_H))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .rounded_md()
                    .border(px(1.0))
                    .border_color(Colors::border_subtle())
                    .bg(Colors::surface_input())
                    .text_size(px(11.0))
                    .text_color(Colors::text_primary())
                    .truncate()
                    .child(path_label),
            )
            .child(secondary_button("export-browse", "Browse…", {
                let target = target.clone();
                move |_window, cx| {
                    let _ = target.update(cx, |this, cx| this.browse_output(cx));
                }
            }));
        self.labeled(
            if self.settings.mode == ExportMode::Mixdown {
                "Output file"
            } else {
                "Output base"
            },
            control,
        )
    }

    fn mode_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let selected = match self.settings.mode {
            ExportMode::Mixdown => "mixdown",
            ExportMode::Stems => "stems",
            ExportMode::Multitrack => "multitrack",
        };
        let options = vec![
            SelectOption::new("mixdown", "Mixdown — master output"),
            SelectOption::new("stems", "Stems — all mixer channels"),
            SelectOption::new("multitrack", "Multitrack — direct tracks"),
        ];
        let control = self.dropdown(
            SelectField::Mode,
            "export-mode",
            selected.to_string(),
            options,
            target,
        );
        self.labeled("Export", control)
    }

    fn dropdown(
        &self,
        field: SelectField,
        id: &'static str,
        selected: String,
        options: Vec<SelectOption>,
        target: gpui::Entity<Self>,
    ) -> impl IntoElement {
        let open = self.open_select == Some(field);
        let toggle_target = target.clone();
        let change_target = target.clone();
        select(
            id,
            Some(selected.as_str()),
            selected.clone(),
            options,
            open,
            false,
            Arc::new(move |_, _window, cx| {
                let _ = toggle_target.update(cx, |this, cx| {
                    this.open_select = if this.open_select == Some(field) {
                        None
                    } else {
                        Some(field)
                    };
                    cx.notify();
                });
            }),
            Arc::new(move |value, _window, cx| {
                let value = value.clone();
                let _ = change_target.update(cx, |this, cx| {
                    this.apply_select(field, &value);
                    this.open_select = None;
                    cx.notify();
                });
            }),
        )
    }

    fn format_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let options = vec![
            SelectOption::new("wav", "WAV"),
            SelectOption::new("flac", "FLAC"),
            SelectOption::new("mp3", "MP3").disabled(!self.defaults.mp3_available),
        ];
        let control = self.dropdown(
            SelectField::Format,
            "export-format",
            self.settings.format.as_str().to_string(),
            options,
            target,
        );
        self.labeled("Format", control)
    }

    fn format_option_row(&self, target: gpui::Entity<Self>) -> gpui::Stateful<gpui::Div> {
        let (label, selected, options) = match self.settings.format {
            AudioFileFormat::Wav => (
                "Bit depth",
                match self.settings.wav_sample_format {
                    sphere_encoder::AudioSampleFormat::F32 => "f32",
                    sphere_encoder::AudioSampleFormat::I24 => "i24",
                    _ => "i16",
                }
                .to_string(),
                vec![
                    SelectOption::new("f32", "Float 32"),
                    SelectOption::new("i24", "PCM 24"),
                    SelectOption::new("i16", "PCM 16"),
                ],
            ),
            AudioFileFormat::Flac => (
                "Bit depth",
                format!("{}", self.settings.flac_bit_depth),
                vec![
                    SelectOption::new("16", "16-bit"),
                    SelectOption::new("24", "24-bit"),
                ],
            ),
            AudioFileFormat::Mp3 => (
                "Bitrate",
                format!("{}", self.settings.mp3_bitrate_kbps),
                vec![
                    SelectOption::new("128", "128 kbps"),
                    SelectOption::new("192", "192 kbps"),
                    SelectOption::new("256", "256 kbps"),
                    SelectOption::new("320", "320 kbps"),
                ],
            ),
            AudioFileFormat::Rauf => ("Bit depth", "f32".to_string(), vec![]),
        };
        let control = self.dropdown(
            SelectField::FormatOption,
            "export-format-option",
            selected,
            options,
            target,
        );
        self.labeled(label, control)
    }

    fn range_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let has_sel = self.defaults.time_selection.is_some();
        let has_loop = self.defaults.loop_range.is_some();
        let selected = match self.settings.range {
            ExportRangeChoice::EntireArrangement => "entire",
            ExportRangeChoice::TimeSelection { .. } => "selection",
            ExportRangeChoice::LoopRange { .. } => "loop",
            ExportRangeChoice::Custom { .. } => "custom",
        };
        let options = vec![
            SelectOption::new("entire", "Entire arrangement"),
            SelectOption::new("selection", "Time selection").disabled(!has_sel),
            SelectOption::new("loop", "Loop range").disabled(!has_loop),
            SelectOption::new("custom", "Custom range"),
        ];
        let control = self.dropdown(
            SelectField::Range,
            "export-range",
            selected.to_string(),
            options,
            target,
        );
        self.labeled("Range", control)
    }

    fn sample_rate_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let selected = match self.settings.sample_rate {
            ExportSampleRateChoice::Project => "project",
            ExportSampleRateChoice::Hz44100 => "44100",
            ExportSampleRateChoice::Hz48000 => "48000",
            ExportSampleRateChoice::Hz88200 => "88200",
            ExportSampleRateChoice::Hz96000 => "96000",
        };
        let options = vec![
            SelectOption::new("project", "Project"),
            SelectOption::new("44100", "44100 Hz"),
            SelectOption::new("48000", "48000 Hz"),
            SelectOption::new("88200", "88200 Hz"),
            SelectOption::new("96000", "96000 Hz"),
        ];
        let control = self.dropdown(
            SelectField::SampleRate,
            "export-rate",
            selected.to_string(),
            options,
            target,
        );
        self.labeled("Sample rate", control)
    }

    fn channels_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let selected = match self.settings.channels {
            ExportChannelMode::Stereo => "stereo",
            ExportChannelMode::Mono => "mono",
        };
        let options = vec![
            SelectOption::new("stereo", "Stereo"),
            SelectOption::new("mono", "Mono"),
        ];
        let control = self.dropdown(
            SelectField::Channels,
            "export-channels",
            selected.to_string(),
            options,
            target,
        );
        self.labeled("Channels", control)
    }

    fn normalize_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let selected = match self.settings.normalize {
            ExportNormalizeChoice::Off => "off",
            ExportNormalizeChoice::PeakDb(_) => "peak",
        };
        let options = vec![
            SelectOption::new("off", "Off"),
            SelectOption::new("peak", "Peak −1.0 dB")
                .disabled(self.settings.mode != ExportMode::Mixdown),
        ];
        let control = self.dropdown(
            SelectField::Normalize,
            "export-normalize",
            selected.to_string(),
            options,
            target,
        );
        self.labeled("Normalize", control)
    }

    fn tail_row(&self, target: gpui::Entity<Self>) -> impl IntoElement {
        let selected = match self.settings.tail {
            ExportTailChoice::None => "none",
            ExportTailChoice::FixedSeconds(_) => "fixed",
            ExportTailChoice::UntilSilence { .. } => "silence",
        };
        let options = vec![
            SelectOption::new("none", "None"),
            SelectOption::new("fixed", "Fixed 5 s"),
            SelectOption::new("silence", "Until silence"),
        ];
        let control = self.dropdown(
            SelectField::Tail,
            "export-tail",
            selected.to_string(),
            options,
            target,
        );
        self.labeled("Tail", control)
    }

    fn apply_select(&mut self, field: SelectField, value: &str) {
        match field {
            SelectField::Mode => {
                self.settings.mode = match value {
                    "stems" => ExportMode::Stems,
                    "multitrack" => ExportMode::Multitrack,
                    _ => ExportMode::Mixdown,
                };
                if self.settings.mode != ExportMode::Mixdown {
                    self.settings.normalize = ExportNormalizeChoice::Off;
                }
            }
            SelectField::Format => {
                self.settings.format = match value {
                    "flac" => AudioFileFormat::Flac,
                    "mp3" => AudioFileFormat::Mp3,
                    _ => AudioFileFormat::Wav,
                };
                self.sync_output_from_name();
            }
            SelectField::FormatOption => match self.settings.format {
                AudioFileFormat::Wav => {
                    self.settings.wav_sample_format = match value {
                        "f32" => sphere_encoder::AudioSampleFormat::F32,
                        "i16" => sphere_encoder::AudioSampleFormat::I16,
                        _ => sphere_encoder::AudioSampleFormat::I24,
                    };
                }
                AudioFileFormat::Flac => {
                    self.settings.flac_bit_depth = if value == "16" { 16 } else { 24 };
                }
                AudioFileFormat::Mp3 => {
                    self.settings.mp3_bitrate_kbps = value.parse().unwrap_or(256);
                }
                AudioFileFormat::Rauf => {}
            },
            SelectField::Range => {
                self.settings.range = match value {
                    "selection" => self
                        .defaults
                        .time_selection
                        .map(|(s, e)| ExportRangeChoice::TimeSelection {
                            start_beat: s,
                            end_beat: e,
                        })
                        .unwrap_or(ExportRangeChoice::EntireArrangement),
                    "loop" => self
                        .defaults
                        .loop_range
                        .map(|(s, e)| ExportRangeChoice::LoopRange {
                            start_beat: s,
                            end_beat: e,
                        })
                        .unwrap_or(ExportRangeChoice::EntireArrangement),
                    "custom" => ExportRangeChoice::Custom {
                        start_beat: 0.0,
                        end_beat: self.defaults.content_end_beat.max(1.0),
                    },
                    _ => ExportRangeChoice::EntireArrangement,
                };
            }
            SelectField::SampleRate => {
                self.settings.sample_rate = match value {
                    "44100" => ExportSampleRateChoice::Hz44100,
                    "48000" => ExportSampleRateChoice::Hz48000,
                    "88200" => ExportSampleRateChoice::Hz88200,
                    "96000" => ExportSampleRateChoice::Hz96000,
                    _ => ExportSampleRateChoice::Project,
                };
            }
            SelectField::Channels => {
                self.settings.channels = if value == "mono" {
                    ExportChannelMode::Mono
                } else {
                    ExportChannelMode::Stereo
                };
            }
            SelectField::Normalize => {
                self.settings.normalize = if value == "peak" {
                    ExportNormalizeChoice::PeakDb(-1.0)
                } else {
                    ExportNormalizeChoice::Off
                };
            }
            SelectField::Tail => {
                self.settings.tail = match value {
                    "fixed" => ExportTailChoice::FixedSeconds(5.0),
                    "silence" => ExportTailChoice::UntilSilence {
                        max_seconds: 10.0,
                        threshold_db: -60.0,
                    },
                    _ => ExportTailChoice::None,
                };
            }
        }
    }

    fn footer(&self, can_export: bool, target: gpui::Entity<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .pt(px(8.0))
            .border_t(px(1.0))
            .border_color(Colors::border_subtle())
            .child(secondary_button("export-cancel", "Cancel", {
                let target = target.clone();
                move |window, cx| {
                    let _ = target.update(cx, |this, cx| this.close(window, cx));
                }
            }))
            .child(primary_button("export-start", "Export", can_export, {
                let target = target.clone();
                move |_window, cx| {
                    let _ = target.update(cx, |this, cx| this.start_export(cx));
                }
            }))
    }

    fn render_progress(
        &self,
        progress: ExportProgress,
        target: gpui::Entity<Self>,
    ) -> gpui::AnyElement {
        let percent = format!("{:.0}%", progress.percent);
        let detail = format!(
            "{} of {} frames",
            progress.rendered_frames, progress.total_frames
        );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .px(px(BODY_PAD))
            .py(px(BODY_PAD))
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Colors::text_primary())
                            .child(progress.stage.as_str().to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Colors::accent_primary())
                            .child(percent),
                    ),
            )
            .child(progress_bar(ProgressBarValue::value(
                progress.percent / 100.0,
            )))
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(Colors::text_muted())
                    .child(detail),
            )
            .child(self.output_path_caption())
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(secondary_button("export-cancel-run", "Cancel", {
                        let target = target.clone();
                        move |_window, cx| {
                            let _ = target.update(cx, |this, cx| this.request_cancel(cx));
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_complete(
        &self,
        summaries: Vec<ArrangementExportSummary>,
        target: gpui::Entity<Self>,
    ) -> gpui::AnyElement {
        let Some(summary) = summaries.first() else {
            return self.render_terminal_message(
                "Export completed without output files.",
                Colors::text_secondary(),
                target,
            );
        };
        let info = format!(
            "{} • {:.2} s • {} ch • {} Hz",
            if summaries.len() == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", summaries.len())
            },
            summary.duration_seconds,
            summary.channels,
            summary.sample_rate
        );
        let path = summary.output_path.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .px(px(BODY_PAD))
            .py(px(BODY_PAD))
            .gap(px(10.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(Colors::accent_primary())
                    .child("Export complete"),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(Colors::text_primary())
                    .truncate()
                    .child(summary.output_path.display().to_string()),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(Colors::text_muted())
                    .child(info),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(secondary_button("export-reveal", "Open Folder", {
                        move |_window, _cx| {
                            if let Some(dir) = path.parent() {
                                let _ = open_in_file_manager(dir);
                            }
                        }
                    }))
                    .child(primary_button("export-close", "Close", true, {
                        let target = target.clone();
                        move |window, cx| {
                            let _ = target.update(cx, |this, cx| this.close(window, cx));
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_terminal_message(
        &self,
        message: &str,
        color: gpui::Rgba,
        target: gpui::Entity<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .px(px(BODY_PAD))
            .py(px(BODY_PAD))
            .gap(px(10.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(color)
                    .child(message.to_string()),
            )
            .child(div().flex_1())
            .child(div().flex().flex_row().justify_end().child(primary_button(
                "export-close-term",
                "Close",
                true,
                {
                    let target = target.clone();
                    move |window, cx| {
                        let _ = target.update(cx, |this, cx| this.close(window, cx));
                    }
                },
            )))
            .into_any_element()
    }

    fn output_path_caption(&self) -> impl IntoElement {
        let path = self
            .settings
            .normalized_output_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        div()
            .text_size(px(10.0))
            .text_color(Colors::text_faint())
            .truncate()
            .child(path)
    }
}

fn section_label(label: &'static str) -> impl IntoElement {
    div()
        .mt(px(2.0))
        .pb(px(2.0))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_faint())
        .child(label)
}

pub(super) fn build_batch_targets(
    request: &DirectAudio::ArrangementExportRequest,
    mode: ExportMode,
    defaults: &ExportProjectDefaults,
    project_name: &str,
) -> Vec<TrackExportTarget> {
    if mode == ExportMode::Mixdown {
        return Vec::new();
    }
    let base_dir = request
        .output_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let suffix = if mode == ExportMode::Stems {
        "Stems"
    } else {
        "Multitrack"
    };
    let output_dir = base_dir.join(format!("{} {suffix}", sanitize_file_stem(project_name)));

    defaults
        .track_targets
        .iter()
        .filter(|target| mode != ExportMode::Multitrack || target.include_in_multitrack)
        .enumerate()
        .map(|(index, target)| {
            let mut item_request = request.clone();
            item_request.output_path = output_dir.join(format!(
                "{:02} {}.{}",
                index + 1,
                sanitize_file_stem(&target.name),
                request.format.extension()
            ));
            TrackExportTarget {
                track_id: target.id.clone(),
                request: item_request,
            }
        })
        .collect()
}

fn sanitize_file_stem(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|character| {
            if matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) {
                '_'
            } else {
                character
            }
        })
        .collect();
    if sanitized.is_empty() {
        "Track".to_string()
    } else {
        sanitized
    }
}

/// Stem for the mixdown file / stems folder prefix. Empty input falls back to
/// "Export" (not "Track" — that default is for unnamed mixer channels).
fn export_stem_from_input(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "Export".to_string();
    }
    sanitize_file_stem(trimmed)
}

crate::impl_single_input_window_ime!(ExportArrangementWindow, name_input);

// ── Small shared button helpers (compact, theme-token only) ──────────────────

fn secondary_button(
    id: &'static str,
    label: &str,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(BUTTON_H))
        .px(px(12.0))
        .rounded(px(5.0))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .text_size(px(12.0))
        .text_color(Colors::text_secondary())
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_control_hover()))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(label.to_string())
}

fn primary_button(
    id: &'static str,
    label: &str,
    enabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let mut button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(BUTTON_H))
        .px(px(14.0))
        .min_w(px(86.0))
        .rounded(px(5.0))
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.to_string());
    if enabled {
        button = button
            .bg(Colors::accent_primary())
            .text_color(gpui::white())
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| s.opacity(0.9))
            .on_click(move |_, window, cx| on_click(window, cx));
    } else {
        button = button
            .bg(Colors::surface_input())
            .text_color(Colors::text_faint())
            .cursor(gpui::CursorStyle::OperationNotAllowed);
    }
    button
}

fn error_banner(message: String) -> impl IntoElement {
    banner(message, Colors::status_error())
}

fn hint_banner(message: String) -> impl IntoElement {
    banner(message, Colors::text_muted())
}

fn banner(message: String, color: gpui::Rgba) -> impl IntoElement {
    div()
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(5.0))
        .bg(Colors::surface_panel_alt())
        .text_size(px(10.0))
        .text_color(color)
        .child(message)
}

fn open_in_file_manager(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(dir).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(dir).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

// ── Opener ───────────────────────────────────────────────────────────────────

/// Open the external Export Arrangement window centered over `owner_bounds`.
pub fn open_export_arrangement_window(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    project_name: String,
    snapshot: EngineProjectSnapshot,
    bridge_sinks: PluginBridgeSinkMap,
    audio_engine: Option<DirectAudio::AudioEngine>,
    defaults: ExportProjectDefaults,
    default_output: Option<PathBuf>,
    cx: &mut App,
) -> Result<WindowHandle<ExportArrangementWindow>, String> {
    use crate::window_position::{apply_owner_display, centered_window_bounds};
    use gpui::{size, WindowBackgroundAppearance, WindowBounds, WindowKind};

    let height = TITLEBAR_HEIGHT + EXPORT_WINDOW_HEIGHT;
    let window_bounds =
        centered_window_bounds(owner_bounds, size(px(EXPORT_WINDOW_WIDTH), px(height)), cx);

    let mut window_options = crate::platform_chrome::external_dialog_window_options_partial();
    window_options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    window_options.kind = WindowKind::Dialog;
    window_options.is_resizable = false;
    window_options.is_minimizable = false;
    window_options.window_background = WindowBackgroundAppearance::Transparent;
    apply_owner_display(&mut window_options, owner_bounds, cx);

    cx.open_window(window_options, move |_window, cx| {
        cx.new(|cx| {
            let mut win = ExportArrangementWindow::new(
                project_name,
                snapshot,
                bridge_sinks,
                audio_engine,
                defaults,
                cx,
            );
            if let Some(path) = default_output {
                win.set_default_output(path);
            }
            win
        })
    })
    .map_err(|e| e.to_string())
}
