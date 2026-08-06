//! Audio Connections window — the native shell around
//! [`AudioConnectionsPanelState`].
//!
//! The window owns only transient view state. Rows are read from the project
//! registry every render and every edit is emitted as a [`ConnectionEdit`] that
//! the layout applies through the registry's structured mutation API, so the
//! project stays the single source of truth and all project mutation lives in
//! one place.

use gpui::prelude::FluentBuilder;
use gpui::AppContext as _;
use gpui::{
    div, px, Context, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window,
};

use crate::audio_connections::{
    AudioConnectionDirection, AudioConnectionId, AudioConnectionRegistry, AudioConnectionStatus,
    ChannelLayout,
};
use crate::components::audio_connections_panel::{
    AudioConnectionsPanelState, ConnectionRow, ConnectionsTab, PendingConfirmation,
};
use crate::theme::Colors;
use crate::window_position::{apply_owner_display, centered_window_bounds};

const WINDOW_WIDTH: f32 = 900.0;
const WINDOW_HEIGHT: f32 = 460.0;
const WINDOW_MIN_WIDTH: f32 = 720.0;
const WINDOW_MIN_HEIGHT: f32 = 320.0;

/// Applies one registry edit against the project and republishes routing when
/// the mutation asks for it.
pub type ConnectionEditCb =
    std::sync::Arc<dyn Fn(&ConnectionEdit, &mut gpui::Window, &mut gpui::App) + 'static>;

/// One user action from the panel. Deliberately data rather than closures, so
/// the layout owns every project mutation in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionEdit {
    Add {
        direction: AudioConnectionDirection,
        layout: ChannelLayout,
    },
    SetEnabled {
        id: AudioConnectionId,
        enabled: bool,
    },
    Duplicate {
        id: AudioConnectionId,
    },
    /// Already confirmed by the panel when references exist.
    Remove {
        id: AudioConnectionId,
    },
    ResetDefaults {
        direction: AudioConnectionDirection,
    },
}

/// Everything the window needs from the project, refreshed on each sync.
#[derive(Clone, Default)]
pub struct AudioConnectionsSnapshot {
    pub registry: AudioConnectionRegistry,
    pub device_summary: String,
    /// `false` shows the no-project empty state and disables mutation.
    pub has_project: bool,
    /// Track ids referencing each connection, keyed by connection id. Used to
    /// describe the consequences of a removal before it happens.
    pub references: std::collections::HashMap<String, Vec<String>>,
}

pub struct AudioConnectionsWindow {
    panel: AudioConnectionsPanelState,
    snapshot: AudioConnectionsSnapshot,
    on_edit: ConnectionEditCb,
    on_close: std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static>,
}

impl AudioConnectionsWindow {
    pub fn new(
        snapshot: AudioConnectionsSnapshot,
        on_edit: ConnectionEditCb,
        on_close: std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static>,
    ) -> Self {
        Self {
            panel: AudioConnectionsPanelState::new(),
            snapshot,
            on_edit,
            on_close,
        }
    }

    /// Push fresh project data. Selections that no longer resolve are pruned so
    /// a removed row cannot stay highlighted.
    pub fn sync(&mut self, snapshot: AudioConnectionsSnapshot, cx: &mut Context<Self>) {
        self.snapshot = snapshot;
        self.panel.prune_selection(&self.snapshot.registry);
        cx.notify();
    }

    /// Called when the project changes so no connection id survives across
    /// projects.
    pub fn on_project_changed(
        &mut self,
        snapshot: AudioConnectionsSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.panel.on_project_changed();
        self.snapshot = snapshot;
        cx.notify();
    }

    pub fn panel_state(&self) -> &AudioConnectionsPanelState {
        &self.panel
    }

    fn references_for(&self, id: &AudioConnectionId) -> Vec<String> {
        self.snapshot
            .references
            .get(id.as_str())
            .cloned()
            .unwrap_or_default()
    }
}

fn status_color(status: AudioConnectionStatus) -> gpui::Rgba {
    match status {
        AudioConnectionStatus::Active => Colors::accent_success(),
        AudioConnectionStatus::Disabled => Colors::text_muted(),
        AudioConnectionStatus::Conflict => Colors::status_error(),
        _ => Colors::accent_warning(),
    }
}

const COL_ENABLED: f32 = 34.0;
const COL_NAME: f32 = 168.0;
const COL_CONFIG: f32 = 88.0;
const COL_DEVICE: f32 = 160.0;
const COL_PORT: f32 = 112.0;
const COL_STATUS: f32 = 112.0;
const ROW_HEIGHT: f32 = 24.0;

fn header_cell(label: &str, width: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_none()
        .px(px(6.0))
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_secondary())
        .truncate()
        .child(label.to_string())
}

fn cell(text: String, width: f32, muted: bool) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_none()
        .px(px(6.0))
        .text_size(px(10.0))
        .truncate()
        .text_color(if muted {
            Colors::text_muted()
        } else {
            Colors::text_primary()
        })
        .child(text)
}

/// Compact toolbar button. Takes a mouse-down listener so both plain closures
/// and `cx.listener(..)` results can be passed.
fn toolbar_button(
    id: &'static str,
    label: String,
    enabled: bool,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(20.0))
        .px(px(8.0))
        .rounded_sm()
        .bg(Colors::button_bg())
        .border(px(1.0))
        .border_color(Colors::button_border())
        .text_size(px(9.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if enabled {
            Colors::text_primary()
        } else {
            Colors::text_muted()
        })
        .when(enabled, |button| {
            button
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::button_bg_hover()))
                .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                    cx.stop_propagation();
                    on_click(event, window, cx);
                })
        })
        .child(label)
}

impl AudioConnectionsWindow {
    fn render_row(
        &self,
        row: ConnectionRow,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = row.id.clone();
        let toggle_id = row.id.clone();
        let next_enabled = !row.enabled;
        let status_label = row.status.label().to_string();

        div()
            .id(("audio-connection-row", index))
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .border_b(px(1.0))
            .border_color(Colors::border_subtle())
            .when(row.selected, |r| r.bg(Colors::surface_selected_soft()))
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _event: &gpui::MouseDownEvent, _w, cx| {
                    this.panel.select_only(id.clone());
                    cx.notify();
                }),
            )
            // Enabled toggle. Disabling keeps every mapping — the bus simply
            // resolves to silence until it is re-enabled.
            .child(
                div()
                    .w(px(COL_ENABLED))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id(("audio-connection-enabled", index))
                            .w(px(12.0))
                            .h(px(12.0))
                            .rounded_sm()
                            .border(px(1.0))
                            .border_color(if row.enabled {
                                Colors::accent_primary()
                            } else {
                                Colors::border_default()
                            })
                            .bg(if row.enabled {
                                Colors::accent_primary()
                            } else {
                                Colors::button_bg()
                            })
                            .cursor(gpui::CursorStyle::PointingHand)
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _event: &gpui::MouseDownEvent, w, cx| {
                                    cx.stop_propagation();
                                    (this.on_edit)(
                                        &ConnectionEdit::SetEnabled {
                                            id: toggle_id.clone(),
                                            enabled: next_enabled,
                                        },
                                        w,
                                        cx,
                                    );
                                }),
                            ),
                    ),
            )
            .child(cell(row.name, COL_NAME, false))
            .child(cell(
                match row.layout {
                    ChannelLayout::Mono => "Mono".to_string(),
                    ChannelLayout::Stereo => "Stereo".to_string(),
                    ChannelLayout::Custom { channels } => format!("{channels} ch"),
                },
                COL_CONFIG,
                false,
            ))
            .child(cell(row.device_label, COL_DEVICE, false))
            .child(cell(row.left_port, COL_PORT, false))
            .child(cell(
                row.right_port.clone().unwrap_or_default(),
                COL_PORT,
                row.right_port.is_none(),
            ))
            .child(
                div()
                    .w(px(COL_STATUS))
                    .flex_none()
                    .px(px(6.0))
                    .text_size(px(9.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .truncate()
                    .text_color(status_color(row.status))
                    .child(status_label),
            )
    }
}

impl Render for AudioConnectionsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_project = self.snapshot.has_project;
        let rows = self.panel.rows(&self.snapshot.registry);
        let (count, attention) = self.panel.summary(&self.snapshot.registry);
        let tab = self.panel.tab;
        let direction = tab.direction();
        let selected = self.panel.single_selection().cloned();

        // ── Header ──────────────────────────────────────────────────────────
        let on_close = self.on_close.clone();
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(34.0))
            .px(px(10.0))
            .flex_none()
            .border_b(px(1.0))
            .border_color(Colors::border_default())
            .bg(Colors::surface_panel_alt())
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(Colors::text_primary())
                    .child("Audio Connections"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(9.5))
                    .truncate()
                    .text_color(Colors::text_secondary())
                    .child(self.snapshot.device_summary.clone()),
            )
            .child(toolbar_button(
                "audio-connections-close",
                "Close".to_string(),
                true,
                move |_event, window, cx| on_close(window, cx),
            ));

        // ── Tabs ────────────────────────────────────────────────────────────
        let mut tabs = div()
            .flex()
            .flex_row()
            .h(px(24.0))
            .flex_none()
            .px(px(6.0))
            .gap(px(2.0))
            .border_b(px(1.0))
            .border_color(Colors::border_default());
        for (this_tab, element_id) in [
            (ConnectionsTab::Inputs, "audio-connections-tab-in"),
            (ConnectionsTab::Outputs, "audio-connections-tab-out"),
        ] {
            let active = tab == this_tab;
            tabs = tabs.child(
                div()
                    .id(element_id)
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(22.0))
                    .px(px(12.0))
                    .cursor(gpui::CursorStyle::PointingHand)
                    .border_b(px(2.0))
                    .border_color(if active {
                        Colors::accent_primary()
                    } else {
                        Colors::border_subtle()
                    })
                    .text_size(px(10.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if active {
                        Colors::text_primary()
                    } else {
                        Colors::text_secondary()
                    })
                    .child(this_tab.label())
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _event: &gpui::MouseDownEvent, _w, cx| {
                            this.panel.set_tab(this_tab);
                            cx.notify();
                        }),
                    ),
            );
        }

        // ── Toolbar ─────────────────────────────────────────────────────────
        let add_mono = {
            let on_edit = self.on_edit.clone();
            move |_e: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| {
                on_edit(
                    &ConnectionEdit::Add {
                        direction,
                        layout: ChannelLayout::Mono,
                    },
                    w,
                    cx,
                );
            }
        };
        let add_stereo = {
            let on_edit = self.on_edit.clone();
            move |_e: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| {
                on_edit(
                    &ConnectionEdit::Add {
                        direction,
                        layout: ChannelLayout::Stereo,
                    },
                    w,
                    cx,
                );
            }
        };

        let duplicate_button = selected.clone().map(|id| {
            let on_edit = self.on_edit.clone();
            toolbar_button(
                "audio-connections-duplicate",
                "Duplicate".to_string(),
                has_project,
                move |_e, w, cx| on_edit(&ConnectionEdit::Duplicate { id: id.clone() }, w, cx),
            )
        });

        let remove_button = selected.clone().map(|id| {
            let name = self
                .snapshot
                .registry
                .name_of(&id)
                .unwrap_or_default()
                .to_string();
            let affected = self.references_for(&id);
            toolbar_button(
                "audio-connections-remove",
                "Remove".to_string(),
                has_project,
                cx.listener(move |this, _event: &gpui::MouseDownEvent, w, cx| {
                    let confirmation = PendingConfirmation::Remove {
                        connection_id: id.clone(),
                        connection_name: name.clone(),
                        affected_tracks: affected.clone(),
                    };
                    // Referenced buses ask first; an unused one goes straight
                    // through, matching the lightweight-confirmation
                    // convention elsewhere in Studio.
                    if confirmation.is_destructive() {
                        this.panel.pending = Some(confirmation);
                        cx.notify();
                    } else {
                        (this.on_edit)(&ConnectionEdit::Remove { id: id.clone() }, w, cx);
                    }
                }),
            )
        });

        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .h(px(30.0))
            .px(px(8.0))
            .flex_none()
            .border_b(px(1.0))
            .border_color(Colors::border_default())
            .child(toolbar_button(
                "audio-connections-add-mono",
                "Add Mono".to_string(),
                has_project,
                add_mono,
            ))
            .child(toolbar_button(
                "audio-connections-add-stereo",
                "Add Stereo".to_string(),
                has_project,
                add_stereo,
            ))
            .children(duplicate_button)
            .children(remove_button)
            .child(div().flex_1())
            .child(toolbar_button(
                "audio-connections-reset",
                "Reset Defaults".to_string(),
                has_project,
                cx.listener(move |this, _event: &gpui::MouseDownEvent, _w, cx| {
                    let affected = this
                        .snapshot
                        .references
                        .values()
                        .flatten()
                        .cloned()
                        .collect();
                    this.panel.pending = Some(PendingConfirmation::ResetDefaults {
                        direction,
                        affected_tracks: affected,
                    });
                    cx.notify();
                }),
            ));

        // ── Table ───────────────────────────────────────────────────────────
        let header_row = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(22.0))
            .flex_none()
            .bg(Colors::surface_panel_alt())
            .border_b(px(1.0))
            .border_color(Colors::border_default())
            .child(header_cell("On", COL_ENABLED))
            .child(header_cell("Bus Name", COL_NAME))
            .child(header_cell("Configuration", COL_CONFIG))
            .child(header_cell("Audio Device", COL_DEVICE))
            .child(header_cell("Left / Mono", COL_PORT))
            .child(header_cell("Right", COL_PORT))
            .child(header_cell("Status", COL_STATUS));

        let body = if !has_project {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_size(px(10.5))
                .text_color(Colors::text_muted())
                .child("No project is open. Audio Connections are stored per project.")
                .into_any_element()
        } else if rows.is_empty() {
            div()
                .flex()
                .flex_1()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.0))
                .text_size(px(10.5))
                .text_color(Colors::text_muted())
                .child(format!("No {} buses yet.", tab.label().to_lowercase()))
                .child("Use Add Mono / Add Stereo, or Reset Defaults.")
                .into_any_element()
        } else {
            let mut list = div()
                .id("audio-connections-rows")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll();
            for (index, row) in rows.into_iter().enumerate() {
                list = list.child(self.render_row(row, index, cx));
            }
            list.into_any_element()
        };

        // ── Footer ──────────────────────────────────────────────────────────
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .h(px(22.0))
            .px(px(10.0))
            .flex_none()
            .border_t(px(1.0))
            .border_color(Colors::border_default())
            .bg(Colors::surface_panel_alt())
            .text_size(px(9.5))
            .text_color(Colors::text_secondary())
            .child(format!("{count} connection(s)"))
            .when(attention > 0, |bar| {
                bar.child(
                    div()
                        .text_color(Colors::accent_warning())
                        .child(format!("{attention} need attention")),
                )
            })
            .children(self.panel.warnings.first().map(|warning| {
                div()
                    .flex_1()
                    .truncate()
                    .text_color(Colors::accent_warning())
                    .child(warning.clone())
            }));

        // ── Confirmation ────────────────────────────────────────────────────
        let confirmation = self.panel.pending.clone().map(|pending| {
            let (title, detail, edit) = match &pending {
                PendingConfirmation::Remove {
                    connection_id,
                    connection_name,
                    affected_tracks,
                } => (
                    format!("Remove \"{connection_name}\"?"),
                    format!(
                        "{} track(s) use this connection and will be set to No Input.",
                        affected_tracks.len()
                    ),
                    ConnectionEdit::Remove {
                        id: connection_id.clone(),
                    },
                ),
                PendingConfirmation::ResetDefaults {
                    direction,
                    affected_tracks,
                } => (
                    format!("Reset {} to defaults?", tab.label()),
                    format!(
                        "Existing buses are replaced. {} track reference(s) may be unassigned.",
                        affected_tracks.len()
                    ),
                    ConnectionEdit::ResetDefaults {
                        direction: *direction,
                    },
                ),
            };
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(Colors::with_alpha(Colors::surface_base(), 0.72))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .w(px(340.0))
                        .p(px(12.0))
                        .rounded_sm()
                        .bg(Colors::surface_raised())
                        .border(px(1.0))
                        .border_color(Colors::border_default())
                        .child(
                            div()
                                .text_size(px(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Colors::text_primary())
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(Colors::text_secondary())
                                .child(detail),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_end()
                                .gap(px(6.0))
                                .child(toolbar_button(
                                    "audio-connections-confirm-cancel",
                                    "Cancel".to_string(),
                                    true,
                                    cx.listener(|this, _event: &gpui::MouseDownEvent, _w, cx| {
                                        this.panel.pending = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(toolbar_button(
                                    "audio-connections-confirm-ok",
                                    "Confirm".to_string(),
                                    true,
                                    cx.listener(
                                        move |this, _event: &gpui::MouseDownEvent, w, cx| {
                                            this.panel.pending = None;
                                            (this.on_edit)(&edit, w, cx);
                                            cx.notify();
                                        },
                                    ),
                                )),
                        ),
                )
        });

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(Colors::surface_window())
            .child(header)
            .child(tabs)
            .child(toolbar)
            .child(header_row)
            .child(body)
            .child(footer)
            .children(confirmation)
    }
}

/// Open the Audio Connections window, mirroring the shared external-dialog
/// window conventions used by the other Studio utility windows.
pub fn open_audio_connections_window(
    owner_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    snapshot: AudioConnectionsSnapshot,
    on_edit: ConnectionEditCb,
    on_close: std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static>,
    cx: &mut gpui::App,
) -> Result<gpui::WindowHandle<AudioConnectionsWindow>, String> {
    let window_bounds = centered_window_bounds(
        owner_bounds,
        gpui::size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        cx,
    );
    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(gpui::WindowBounds::Windowed(window_bounds));
    options.kind = gpui::WindowKind::Dialog;
    options.is_resizable = true;
    options.is_minimizable = true;
    options.window_background = gpui::WindowBackgroundAppearance::Opaque;
    options.window_min_size = Some(gpui::size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT)));
    apply_owner_display(&mut options, owner_bounds, cx);

    cx.open_window(options, move |_window, cx| {
        cx.new(|_cx| AudioConnectionsWindow::new(snapshot, on_edit, on_close))
    })
    .map_err(|error| error.to_string())
}
