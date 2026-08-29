//! Detached mixer window for multi-monitor / float layout.
//!
//! Renders from a cloned [`MixerSnapshot`] — never reads [`StudioLayout`] during
//! `Render` (avoids GPUI entity re-entrancy when the main studio is updating).

use std::{collections::HashSet, sync::Arc};

use gpui::{
    div, px, size, App, AppContext, Bounds, Context, Entity, FocusHandle, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Point, Render, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle,
};

use crate::components::mixer_panel::{
    clamp_mixer_section_height_px, mixer_panel, MixerCallbacks, MixerSplit, MixerSplitAction,
    MixerSplitTarget, VstiOutputMeterState,
};
use crate::components::mixer_tree_sidebar::MIXER_TREE_COLLAPSED_RAIL_WIDTH;
use crate::components::mixer_tree_sidebar_view::MixerTreeSidebar;
use crate::components::timeline::timeline_state::{MasterBusState, MonitorBusState, TrackState};
use crate::components::title_bar::{external_window_titlebar, TITLEBAR_HEIGHT};
use crate::i18n::I18n;
use crate::theme::Colors;

pub const MIXER_WINDOW_WIDTH: f32 = 1180.0;
pub const MIXER_WINDOW_HEIGHT: f32 = 420.0;
pub const MIXER_WINDOW_MIN_WIDTH: f32 = 760.0;
pub const MIXER_WINDOW_MIN_HEIGHT: f32 = 320.0;
const MIXER_MENU_BAR_HEIGHT: f32 = 24.0;
const MIXER_MENU_FONT_SIZE: f32 = 11.0;
const MIXER_MENU_IDS: [(&str, &str); 5] = [
    ("file", "File"),
    ("edit", "Edit"),
    ("scene", "Scene"),
    ("tools", "Tools"),
    ("help", "Help"),
];

fn mixer_menu_bar(i18n: I18n) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .h(px(MIXER_MENU_BAR_HEIGHT))
        .px(px(4.0))
        .gap(px(1.0))
        .bg(Colors::surface_titlebar())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .children(
            MIXER_MENU_IDS
                .into_iter()
                .enumerate()
                .map(|(index, (menu_id, fallback))| {
                    div()
                        .id(("mixer-menu-label", index))
                        .h_full()
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .rounded(px(crate::theme::radius::CONTROL))
                        .text_size(px(MIXER_MENU_FONT_SIZE))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(Colors::text_secondary())
                        .hover(|style| {
                            style
                                .bg(Colors::surface_control_hover())
                                .text_color(Colors::text_primary())
                        })
                        .cursor(gpui::CursorStyle::PointingHand)
                        .child(i18n.tr_menu(menu_id, fallback))
                }),
        )
}

/// View-model for the external mixer — cloned from the studio timeline.
#[derive(Clone)]
pub struct MixerSnapshot {
    pub tracks: Vec<TrackState>,
    pub master: MasterBusState,
    /// Input-monitoring bus for the pinned Monitor strip.
    pub monitor: MonitorBusState,
    pub selected_track_id: Option<String>,
    /// Multi-select set (Ctrl/Cmd additive, Shift range). Highlight uses this.
    pub selected_track_ids: Vec<String>,
    pub mixer_scroll_x: f32,
    /// Shared insert viewport height (clamped by the owner).
    pub mixer_insert_section_px: f32,
    /// Shared send viewport height (clamped by the owner).
    pub mixer_send_section_px: f32,
    /// Active splitter target while dragging (drives active-handle highlight).
    pub mixer_split_active_target: Option<MixerSplitTarget>,
    /// Collapsed VSTi output strip groups keyed by `track_id:insert_id`.
    pub collapsed_vsti_output_groups: HashSet<String>,
    pub hidden_mixer_channels: HashSet<String>,
    pub vsti_output_meters: std::collections::HashMap<String, VstiOutputMeterState>,
    pub tree_sidebar_enabled: bool,
    pub tree_sidebar_collapsed: bool,
    pub tree_sidebar_width_px: f32,
}

pub struct MixerWindow {
    snapshot: MixerSnapshot,
    tree_sidebar: Entity<MixerTreeSidebar>,
    callbacks: MixerCallbacks,
    on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
    on_mixer_scroll: Arc<dyn Fn(f32, &mut Window, &mut App) + Send + Sync>,
    on_mixer_split: Arc<dyn Fn(MixerSplitAction, &mut Window, &mut App) + Send + Sync>,
    dispatch_key: Arc<dyn Fn(&KeyDownEvent, &mut App) -> bool + Send + Sync>,
    focus_handle: FocusHandle,
}

impl MixerWindow {
    pub fn new(
        snapshot: MixerSnapshot,
        tree_sidebar: Entity<MixerTreeSidebar>,
        callbacks: MixerCallbacks,
        on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
        on_mixer_scroll: Arc<dyn Fn(f32, &mut Window, &mut App) + Send + Sync>,
        on_mixer_split: Arc<dyn Fn(MixerSplitAction, &mut Window, &mut App) + Send + Sync>,
        dispatch_key: Arc<dyn Fn(&KeyDownEvent, &mut App) -> bool + Send + Sync>,
        cx: &mut Context<Self>,
    ) -> Self {
        external_mixer_debug(&format!(
            "external mixer window created tracks={}",
            snapshot.tracks.len()
        ));
        Self {
            snapshot,
            tree_sidebar,
            callbacks,
            on_close,
            on_mixer_scroll,
            on_mixer_split,
            dispatch_key,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_snapshot(&mut self, snapshot: MixerSnapshot) {
        external_mixer_debug(&format!(
            "external mixer snapshot updated tracks={}",
            snapshot.tracks.len()
        ));
        self.snapshot = snapshot;
    }
}

impl Render for MixerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Reclaim the shortcut anchor unless the tree filter is actively typing.
        // Orphaned focus (filter closed / strip click that doesn't take focus)
        // otherwise leaves capture_key_down early-returning forever on Linux.
        let filter_owns_keyboard = self.tree_sidebar.read(cx).is_filter_focused(window);
        if !self.focus_handle.is_focused(window) && !filter_owns_keyboard {
            self.focus_handle.focus(window, cx);
        }
        let viewport_width: f32 = window.bounds().size.width.into();
        let viewport_height: f32 = window.bounds().size.height.into();
        let MixerSnapshot {
            tracks,
            master,
            monitor,
            selected_track_id,
            selected_track_ids,
            mixer_scroll_x,
            mixer_insert_section_px,
            mixer_send_section_px,
            mixer_split_active_target,
            collapsed_vsti_output_groups,
            hidden_mixer_channels,
            vsti_output_meters,
            tree_sidebar_enabled,
            tree_sidebar_collapsed,
            tree_sidebar_width_px,
        } = self.snapshot.clone();
        let mixer_callbacks = self.callbacks.clone();
        let on_mixer_scroll = self.on_mixer_scroll.clone();
        let on_mixer_split = self.on_mixer_split.clone();
        let on_close = self.on_close.clone();
        let dispatch_key = self.dispatch_key.clone();
        let shortcut_focus = self.focus_handle.clone();
        let focus_on_pointer = self.focus_handle.clone();
        let tree_width = if tree_sidebar_enabled {
            if tree_sidebar_collapsed {
                MIXER_TREE_COLLAPSED_RAIL_WIDTH
            } else {
                crate::components::mixer_tree_sidebar::clamp_mixer_tree_sidebar_width(
                    tree_sidebar_width_px,
                )
            }
        } else {
            0.0
        };
        let mixer_viewport_width = (viewport_width - tree_width - 90.0).max(100.0);
        let mixer_viewport_height =
            (viewport_height - TITLEBAR_HEIGHT - MIXER_MENU_BAR_HEIGHT).max(0.0);
        let mixer_split = MixerSplit {
            insert_px: clamp_mixer_section_height_px(mixer_insert_section_px),
            send_px: clamp_mixer_section_height_px(mixer_send_section_px),
            active_target: mixer_split_active_target,
            on_action: on_mixer_split,
        };
        let i18n = I18n::from_app(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(crate::theme::ui_font())
            .bg(Colors::surface_window())
            .overflow_hidden()
            .capture_any_mouse_down(move |_event, window, cx| {
                focus_on_pointer.focus(window, cx);
            })
            .capture_key_down(move |event, _window, cx| {
                let window = _window;
                if window
                    .focused(cx)
                    .is_some_and(|focused| focused != shortcut_focus)
                {
                    return;
                }
                if (dispatch_key)(event, cx) {
                    window.prevent_default();
                    cx.stop_propagation();
                }
            })
            .child(div().w(px(0.0)).h(px(0.0)).track_focus(&self.focus_handle))
            .child(external_window_titlebar(
                i18n.tr("mixer.title"),
                "mixer-window-close",
                move |window, cx| {
                    on_close(window, cx);
                    window.remove_window();
                },
            ))
            .child(mixer_menu_bar(i18n))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .bg(Colors::bottom_panel_bg())
                    .child(mixer_panel(
                        &tracks,
                        &master,
                        &monitor,
                        selected_track_id.as_deref(),
                        &selected_track_ids,
                        mixer_callbacks,
                        &collapsed_vsti_output_groups,
                        &hidden_mixer_channels,
                        &vsti_output_meters,
                        mixer_scroll_x,
                        mixer_viewport_width,
                        mixer_viewport_height,
                        on_mixer_scroll,
                        mixer_split,
                        Some(self.tree_sidebar.clone()),
                        tree_sidebar_enabled,
                        i18n,
                    )),
            )
    }
}

pub fn open_mixer_window(
    owner_bounds: Bounds<gpui::Pixels>,
    snapshot: MixerSnapshot,
    tree_sidebar: Entity<MixerTreeSidebar>,
    callbacks: MixerCallbacks,
    on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
    on_mixer_scroll: Arc<dyn Fn(f32, &mut Window, &mut App) + Send + Sync>,
    on_mixer_split: Arc<dyn Fn(MixerSplitAction, &mut Window, &mut App) + Send + Sync>,
    dispatch_key: Arc<dyn Fn(&KeyDownEvent, &mut App) -> bool + Send + Sync>,
    cx: &mut App,
) -> Result<WindowHandle<MixerWindow>, String> {
    external_mixer_debug(&format!(
        "opening external mixer window tracks={}",
        snapshot.tracks.len()
    ));

    let parent_x: f32 = owner_bounds.origin.x.into();
    let parent_y: f32 = owner_bounds.origin.y.into();
    let parent_w: f32 = owner_bounds.size.width.into();
    let parent_h: f32 = owner_bounds.size.height.into();
    let origin = Point {
        x: px((parent_x + parent_w - MIXER_WINDOW_WIDTH).max(24.0)),
        y: px((parent_y + parent_h - MIXER_WINDOW_HEIGHT).max(24.0)),
    };

    let mut options = crate::platform_chrome::external_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(Bounds {
        origin,
        size: size(px(MIXER_WINDOW_WIDTH), px(MIXER_WINDOW_HEIGHT)),
    }));
    options.is_resizable = true;
    options.is_minimizable = true;
    options.window_background = WindowBackgroundAppearance::Opaque;
    options.window_min_size = Some(size(
        px(MIXER_WINDOW_MIN_WIDTH),
        px(MIXER_WINDOW_MIN_HEIGHT),
    ));
    crate::window_position::apply_owner_display(&mut options, Some(owner_bounds), cx);

    cx.open_window(options, move |_window, cx| {
        cx.new(|cx| {
            MixerWindow::new(
                snapshot,
                tree_sidebar,
                callbacks,
                on_close,
                on_mixer_scroll,
                on_mixer_split,
                dispatch_key,
                cx,
            )
        })
    })
    .map_err(|e| e.to_string())
}

/// Cached check for the external-mixer debug flag. Read once and memoized so the
/// hot fader/pan/knob drag path can skip building a `format!` message string on
/// every mouse-move when the flag is off (the spec bans string allocation in the
/// fader drag hot path).
pub(crate) fn external_mixer_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("FUTUREBOARD_EXTERNAL_MIXER_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

pub(crate) fn external_mixer_debug(message: &str) {
    if external_mixer_debug_enabled() {
        eprintln!("[external-mixer] {message}");
    }
}
