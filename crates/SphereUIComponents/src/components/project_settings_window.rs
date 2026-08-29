//! Project Settings window.
//!
//! Everything here belongs to *the open project*, not to the application:
//! tempo, meter, and the sample rate the project is worked at. Application
//! preferences (themes, keymaps, plugin folders, audio devices, the defaults
//! used when creating a *new* project) stay in the Settings window — the two
//! were previously reached through the same surface, which made "Project
//! Settings" open a dialog whose contents were mostly not about the project.
//!
//! The window renders a snapshot pushed by `StudioLayout` and sends edits back
//! through callbacks, so `TimelineState` remains the owner of the project values
//! shown. Nothing here mutates project state directly and
//! nothing here caches an edit: a value changes on screen because the studio
//! accepted it and pushed a new snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, size, App, AppContext, Bounds, Context, DragMoveEvent, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind,
};

use crate::components::app_chrome::{next_bpm_drag_id, BpmDrag, BpmDragSample};
use crate::components::controls::{fb_button, FbButtonKind};
use crate::components::form::{select, select_dismiss_backdrop, SelectOption};
use crate::components::title_bar::external_window_titlebar;
use crate::theme::{self, Colors};
use crate::window_position::{apply_owner_display, centered_window_bounds};

pub const PROJECT_SETTINGS_WINDOW_WIDTH: f32 = 460.0;
pub const PROJECT_SETTINGS_WINDOW_HEIGHT: f32 = 470.0;

/// Sample rates the project can be worked at. Same list the audio settings
/// offer, because this control routes through the same engine-restart flow.
const SAMPLE_RATES: [u32; 5] = [44_100, 48_000, 88_200, 96_000, 192_000];

/// Time signatures offered for the project's base meter.
const TIME_SIGNATURES: [(u32, u32); 8] = [
    (4, 4),
    (3, 4),
    (2, 4),
    (6, 8),
    (5, 4),
    (7, 8),
    (9, 8),
    (12, 8),
];

/// The live project state this window shows. Built by `StudioLayout` from
/// `TimelineState` plus the active engine-rate diagnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSettingsSnapshot {
    pub name: String,
    /// Project file on disk; `None` for an unsaved project.
    pub path: Option<PathBuf>,
    pub is_dirty: bool,
    /// Base project tempo (the tempo at beat 0).
    pub bpm: f32,
    /// Base time signature (the meter at beat 0).
    pub time_signature: (u32, u32),
    /// `true` when the project has tempo automation beyond the base tempo, so
    /// the base value is not the tempo everywhere.
    pub has_tempo_markers: bool,
    /// `true` when the project has meter changes beyond the base signature.
    pub has_time_signature_markers: bool,
    pub sample_rate: u32,
    /// Rate the audio engine is actually running at, when a stream is open.
    pub engine_sample_rate: Option<u32>,
    pub track_count: usize,
}

impl Default for ProjectSettingsSnapshot {
    fn default() -> Self {
        Self {
            name: "Untitled Project".to_string(),
            path: None,
            is_dirty: false,
            bpm: 120.0,
            time_signature: (4, 4),
            has_tempo_markers: false,
            has_time_signature_markers: false,
            sample_rate: 48_000,
            engine_sample_rate: None,
            track_count: 0,
        }
    }
}

/// Edits the window sends back to the studio. Each one is applied through the
/// studio's existing command path (tempo → transport, meter → the time
/// signature map, sample rate → the project-owned engine-reopen flow), so this
/// window adds no second way to change them.
#[derive(Clone)]
pub struct ProjectSettingsCallbacks {
    pub on_bpm_drag: Arc<dyn Fn(BpmDragSample, &mut App) + Send + Sync>,
    /// Pointer release after a BPM scrub — closes the gesture as one undo entry.
    pub on_bpm_drag_end: Arc<dyn Fn(&mut App) + Send + Sync>,
    pub on_set_time_signature: Arc<dyn Fn(u32, u32, &mut App) + Send + Sync>,
    pub on_set_sample_rate: Arc<dyn Fn(u32, &mut App) + Send + Sync>,
    pub on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
}

/// Which dropdown is open. Only one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenMenu {
    TimeSignature,
    SampleRate,
}

pub struct ProjectSettingsWindow {
    focus_handle: FocusHandle,
    snapshot: ProjectSettingsSnapshot,
    callbacks: ProjectSettingsCallbacks,
    open_menu: Option<OpenMenu>,
}

impl ProjectSettingsWindow {
    fn new(
        snapshot: ProjectSettingsSnapshot,
        callbacks: ProjectSettingsCallbacks,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            snapshot,
            callbacks,
            open_menu: None,
        }
    }

    /// Adopt a snapshot pushed by the studio. Returns `true` when anything
    /// changed, so the caller only notifies on a real update.
    pub fn set_snapshot(&mut self, snapshot: ProjectSettingsSnapshot) -> bool {
        if self.snapshot == snapshot {
            return false;
        }
        self.snapshot = snapshot;
        true
    }

    fn toggle_menu(&mut self, menu: OpenMenu, cx: &mut Context<Self>) {
        self.open_menu = if self.open_menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }
}

impl Render for ProjectSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot.clone();
        let on_close = self.callbacks.on_close.clone();

        let mut root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::surface_base())
            .text_color(Colors::text_primary())
            .font(theme::ui_font())
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.as_str() != "escape" {
                    return;
                }
                // Escape cancels the transient dropdown before the window.
                if this.open_menu.take().is_some() {
                    cx.notify();
                } else {
                    window.remove_window();
                }
            }))
            .child(external_window_titlebar(
                "Project Settings",
                "project-settings-close",
                move |window, cx| on_close(window, cx),
            ))
            .child(
                div()
                    .id("project-settings-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px(px(18.0))
                    .py(px(12.0))
                    .child(project_identity(&snapshot))
                    .child(self.tempo_section(&snapshot, cx))
                    .child(self.audio_section(&snapshot, cx)),
            )
            .child(footer(self.callbacks.on_close.clone()));

        if self.open_menu.is_some() {
            let target = cx.entity().clone();
            root = root.child(select_dismiss_backdrop(Arc::new(
                move |_: &(), _window, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.open_menu = None;
                        cx.notify();
                    });
                },
            )));
        }
        root
    }
}

impl ProjectSettingsWindow {
    fn tempo_section(
        &self,
        snapshot: &ProjectSettingsSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ts_toggle = cx.entity().clone();
        let ts_change = cx.entity().clone();
        let selected_ts = format!(
            "{}/{}",
            snapshot.time_signature.0, snapshot.time_signature.1
        );

        settings_section("TEMPO & METER")
            .child(settings_row(
                "Tempo",
                if snapshot.has_tempo_markers {
                    "Base tempo · Tempo map active"
                } else {
                    "Base tempo"
                },
                self.bpm_scrub_field(snapshot.bpm),
            ))
            .child(settings_row(
                "Time signature",
                if snapshot.has_time_signature_markers {
                    "Base meter · Meter changes active"
                } else {
                    "Base meter"
                },
                div()
                    .w(px(150.0))
                    .child(select(
                        "project-settings-time-signature",
                        Some(selected_ts.as_str()),
                        "-",
                        TIME_SIGNATURES
                            .iter()
                            .map(|(num, den)| {
                                let label = format!("{num}/{den}");
                                SelectOption::new(label.clone(), label)
                            })
                            .collect(),
                        self.open_menu == Some(OpenMenu::TimeSignature),
                        false,
                        Arc::new(move |_: &(), _w, cx| {
                            let _ = ts_toggle.update(cx, |this, cx| {
                                this.toggle_menu(OpenMenu::TimeSignature, cx)
                            });
                        }),
                        Arc::new(move |value: &String, _w, cx| {
                            let Some((num, den)) = value.split_once('/') else {
                                return;
                            };
                            let (Ok(num), Ok(den)) = (num.parse::<u32>(), den.parse::<u32>())
                            else {
                                return;
                            };
                            let apply = ts_change.update(cx, |this, cx| {
                                this.open_menu = None;
                                cx.notify();
                                this.callbacks.on_set_time_signature.clone()
                            });
                            apply(num, den, cx);
                        }),
                    ))
                    .into_any_element(),
            ))
    }

    fn bpm_scrub_field(&self, bpm: f32) -> gpui::AnyElement {
        let on_bpm_drag = self.callbacks.on_bpm_drag.clone();
        let on_bpm_drag_end_up = self.callbacks.on_bpm_drag_end.clone();
        let on_bpm_drag_end_out = self.callbacks.on_bpm_drag_end.clone();

        div()
            .id("project-settings-bpm")
            .w(px(100.0))
            .h(px(28.0))
            .flex()
            .flex_row()
            .items_center()
            .px(px(8.0))
            .rounded(px(crate::theme::radius::CONTROL))
            .border(px(1.0))
            .border_color(Colors::border_subtle())
            .bg(Colors::surface_input())
            .cursor(gpui::CursorStyle::ResizeUpDown)
            .hover(|style| style.bg(Colors::surface_control_hover()))
            .child(
                div()
                    .mr(px(6.0))
                    .text_size(px(10.0))
                    .text_color(Colors::text_faint())
                    .child("↕"),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(Colors::text_primary())
                    .child(format!("{bpm:.2}")),
            )
            .child(
                div()
                    .ml(px(5.0))
                    .text_size(px(8.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(Colors::text_faint())
                    .child("BPM"),
            )
            .occlude()
            .on_drag(
                BpmDrag {
                    drag_id: 0,
                    start_bpm: bpm,
                },
                move |drag, _offset, _window, cx| {
                    cx.new(|_| BpmDrag {
                        drag_id: next_bpm_drag_id(),
                        start_bpm: drag.start_bpm,
                    })
                },
            )
            .on_drag_move::<BpmDrag>(move |event: &DragMoveEvent<BpmDrag>, _window, cx| {
                let drag = event.drag(cx);
                let modifiers = event.event.modifiers;
                on_bpm_drag(
                    BpmDragSample {
                        drag_id: drag.drag_id,
                        start_bpm: drag.start_bpm,
                        cur_y: event.event.position.y.into(),
                        shift: modifiers.shift,
                        control: modifiers.control,
                        platform: modifiers.platform,
                        alt: modifiers.alt,
                    },
                    cx,
                );
            })
            // Release closes the scrub as one undo entry. Wired on and off the
            // element for the same reason as the transport BPM box; a release
            // with no scrub in flight is a no-op.
            .on_mouse_up(
                gpui::MouseButton::Left,
                move |_: &gpui::MouseUpEvent, _window, cx| {
                    on_bpm_drag_end_up(cx);
                },
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                move |_: &gpui::MouseUpEvent, _window, cx| {
                    on_bpm_drag_end_out(cx);
                },
            )
            .into_any_element()
    }

    fn audio_section(
        &self,
        snapshot: &ProjectSettingsSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sr_toggle = cx.entity().clone();
        let sr_change = cx.entity().clone();
        let selected_rate = snapshot.sample_rate.to_string();
        // Only report a mismatch when a stream is actually open; with no engine
        // there is no "running at" value to disagree with.
        let engine_mismatch = snapshot
            .engine_sample_rate
            .is_some_and(|rate| rate != snapshot.sample_rate);

        settings_section("AUDIO")
            .child(settings_row(
                "Project rate",
                "Requested sample rate",
                div()
                    .w(px(150.0))
                    .child(select(
                        "project-settings-sample-rate",
                        Some(selected_rate.as_str()),
                        "-",
                        SAMPLE_RATES
                            .iter()
                            .map(|rate| {
                                SelectOption::new(rate.to_string(), format_sample_rate(*rate))
                            })
                            .collect(),
                        self.open_menu == Some(OpenMenu::SampleRate),
                        false,
                        Arc::new(move |_: &(), _w, cx| {
                            let _ = sr_toggle
                                .update(cx, |this, cx| this.toggle_menu(OpenMenu::SampleRate, cx));
                        }),
                        Arc::new(move |value: &String, _w, cx| {
                            let Ok(rate) = value.parse::<u32>() else {
                                return;
                            };
                            let apply = sr_change.update(cx, |this, cx| {
                                this.open_menu = None;
                                cx.notify();
                                this.callbacks.on_set_sample_rate.clone()
                            });
                            apply(rate, cx);
                        }),
                    ))
                    .into_any_element(),
            ))
            .child(settings_row(
                "Active rate",
                "Audio engine runtime",
                readonly_value(
                    snapshot
                        .engine_sample_rate
                        .map(format_sample_rate)
                        .unwrap_or_else(|| "Engine stopped".to_string()),
                ),
            ))
            .when(engine_mismatch, |section| {
                section.child(status_note("Restart audio to apply the project rate."))
            })
    }
}

fn project_identity(snapshot: &ProjectSettingsSnapshot) -> impl IntoElement {
    let location = snapshot
        .path
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "Not saved".to_string());
    let status = if snapshot.is_dirty {
        "Unsaved changes"
    } else if snapshot.path.is_none() {
        "Not saved"
    } else {
        "Saved"
    };
    let status_color = if snapshot.is_dirty {
        Colors::status_warning()
    } else {
        Colors::text_muted()
    };
    let tracks = match snapshot.track_count {
        1 => "1 track".to_string(),
        count => format!("{count} tracks"),
    };

    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap(px(5.0))
        .pb(px(13.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(snapshot.name.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(9.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(status_color)
                        .child(status),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(9.5))
                        .text_color(Colors::text_faint())
                        .child(location),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(9.5))
                        .text_color(Colors::text_faint())
                        .child(tracks),
                ),
        )
}

fn settings_section(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .child(
            div()
                .h(px(29.0))
                .flex()
                .items_center()
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(Colors::text_faint())
                .child(title),
        )
}

fn settings_row(
    label: &'static str,
    detail: &'static str,
    control: gpui::AnyElement,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.0))
        .min_h(px(47.0))
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .child(
            div()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(Colors::text_secondary())
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(9.0))
                        .text_color(Colors::text_faint())
                        .child(detail),
                ),
        )
        .child(control)
}

fn readonly_value(value: String) -> gpui::AnyElement {
    div()
        .w(px(150.0))
        .text_size(px(10.5))
        .text_color(Colors::text_secondary())
        .child(value)
        .into_any_element()
}

fn status_note(text: &'static str) -> impl IntoElement {
    div()
        .min_h(px(29.0))
        .flex()
        .items_center()
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .text_size(px(9.5))
        .text_color(Colors::status_warning())
        .child(text)
}

fn format_sample_rate(rate: u32) -> String {
    format!("{},{:03} Hz", rate / 1_000, rate % 1_000)
}

fn footer(on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .flex_none()
        .h(px(46.0))
        .px(px(16.0))
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .child(fb_button(
            "project-settings-done",
            "Done",
            FbButtonKind::Primary,
            true,
            move |_, window, cx| on_close(window, cx),
        ))
}

pub fn open_project_settings_window(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    snapshot: ProjectSettingsSnapshot,
    callbacks: ProjectSettingsCallbacks,
    cx: &mut App,
) -> Result<WindowHandle<ProjectSettingsWindow>, String> {
    let window_bounds = centered_window_bounds(
        owner_bounds,
        size(
            px(PROJECT_SETTINGS_WINDOW_WIDTH),
            px(PROJECT_SETTINGS_WINDOW_HEIGHT),
        ),
        cx,
    );
    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    options.kind = WindowKind::Dialog;
    options.is_resizable = false;
    options.is_minimizable = false;
    options.window_background = WindowBackgroundAppearance::Opaque;
    apply_owner_display(&mut options, owner_bounds, cx);

    cx.open_window(options, move |_window, cx| {
        cx.new(|cx| ProjectSettingsWindow::new(snapshot, callbacks, cx))
    })
    .map_err(|error| error.to_string())
}
