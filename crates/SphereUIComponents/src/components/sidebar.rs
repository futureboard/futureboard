//! Browser sidebar — left dock of the studio shell.
//!
//! A dense, grouped DAW asset browser. The navigation model (in
//! `file_browser.rs`) is organized into subtle **Collections / Library /
//! Places** groups; this module is pure presentation:
//!
//! * a compact top utility toolbar (collapse-all / rescan / item count),
//! * an integrated search field,
//! * a virtualized tree that renders three row kinds — group headers,
//!   filesystem folder/file rows, and honest empty-state info rows,
//! * a footer showing the current selection.
//!
//! Real folders are read lazily from disk through `file_browser.rs`; icons are
//! resolved from the model's semantic [`BrowserIcon`], never guessed from text.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    canvas, div, fill, point, px, size, svg, uniform_list, App, AppContext, Bounds, Empty,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Role, ScrollHandle,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, Window,
};

use crate::assets;
use crate::components::file_browser::{
    BrowserIcon, BrowserNodeKind, BrowserVisibleNode, FileBrowserState,
};
use crate::components::icon_button::icon_button;
use crate::components::text_input::{
    text_field_with_callbacks, TextInputCallbacks, TextInputState,
};
use crate::components::timeline::waveform_cache;
use crate::i18n::I18n;
use crate::theme::Colors;
use DirectAudio::AUDITION_PREVIEW_SECONDS;

pub const SIDEBAR_WIDTH: f32 = 272.0;
/// Compact utility toolbar above the search field.
const TOOLBAR_HEIGHT: f32 = 28.0;
/// Compact row height — dense without losing readability. Every row kind uses
/// this exact height so `uniform_list` virtualization/scroll math stays correct.
const TREE_ROW_HEIGHT: f32 = 22.0;
/// Per-depth indent. Smaller than a generic file explorer so deep trees
/// still fit the 272 px sidebar.
const TREE_INDENT: f32 = 12.0;
/// Width of the left padding before the disclosure arrow at depth 0.
const TREE_LEFT_PAD: f32 = 6.0;
/// Width of the disclosure arrow column. Keeps icons aligned across rows
/// whether or not the row is expandable.
const DISCLOSURE_W: f32 = 12.0;

pub type ActivateFileCb = Arc<dyn Fn(&PathBuf, &mut Window, &mut App) + 'static>;
pub type SelectEntryCb = Arc<dyn Fn(&PathBuf, &mut Window, &mut App) + 'static>;
pub type ToggleNodeCb = Arc<dyn Fn(&(String, Option<PathBuf>), &mut Window, &mut App) + 'static>;
pub type BrowserContextCb =
    Arc<dyn Fn(&(Option<PathBuf>, f32, f32), &mut Window, &mut App) + 'static>;
/// Toolbar action with no payload (Collapse All / Rescan).
pub type BrowserActionCb = Arc<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(Clone, Debug)]
pub struct BrowserDragItem {
    pub path: PathBuf,
    pub label: String,
}

pub struct BrowserDragPreview {
    label: String,
}

impl Render for BrowserDragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(5.0))
            .rounded_md()
            .border(px(1.0))
            .border_color(Colors::border_subtle())
            .bg(Colors::surface_raised())
            .shadow_lg()
            .child(
                svg()
                    .path(assets::ICON_FILE_PATH)
                    .w(px(12.0))
                    .h(px(12.0))
                    .text_color(Colors::status_success()),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(Colors::text_primary())
                    .child(self.label.clone()),
            )
    }
}

#[allow(clippy::too_many_arguments)]
pub fn sidebar(
    state: &FileBrowserState,
    scroll: UniformListScrollHandle,
    search_input: &TextInputState,
    search_focused: bool,
    active: bool,
    search_callbacks: TextInputCallbacks,
    on_toggle: ToggleNodeCb,
    on_select: SelectEntryCb,
    on_activate_file: ActivateFileCb,
    on_context_menu: BrowserContextCb,
    on_collapse_all: BrowserActionCb,
    on_rescan: BrowserActionCb,
    i18n: I18n,
) -> impl IntoElement {
    // ── Top utility toolbar ─────────────────────────────────────────
    let collapse_cb = on_collapse_all.clone();
    let rescan_cb = on_rescan.clone();
    let toolbar = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .h(px(TOOLBAR_HEIGHT))
        .px(px(8.0))
        .border_b(px(1.0))
        .border_color(if active {
            Colors::panel_border_focused()
        } else {
            Colors::border_subtle()
        })
        .bg(Colors::surface_panel_alt())
        .child(
            div()
                .text_color(if active {
                    Colors::panel_header_active()
                } else {
                    Colors::tab_text()
                })
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(i18n.tr("browser.panel.title")),
        )
        .child(div().flex_1())
        .child(
            div()
                .px(px(4.0))
                .text_size(px(9.0))
                .text_color(Colors::text_faint())
                .child(format!("{}", state.visible_node_count())),
        )
        .child(
            icon_button(
                Some(assets::ICON_MINUS_PATH),
                "–",
                px(20.0),
                px(20.0),
                px(12.0),
                Colors::text_muted(),
            )
            .id("browser-collapse-all")
            .role(Role::Button)
            .aria_label("Collapse all browser folders")
            .focusable()
            .tab_stop(true)
            .focus_visible(|style| style.bg(Colors::surface_control_hover()))
            .cursor(gpui::CursorStyle::PointingHand)
            .on_click(move |_e, w, cx| collapse_cb(w, cx)),
        )
        .child(
            icon_button(
                Some(assets::ICON_REPEAT_PATH),
                "↻",
                px(20.0),
                px(20.0),
                px(12.0),
                Colors::text_muted(),
            )
            .id("browser-rescan")
            .role(Role::Button)
            .aria_label("Rescan browser files")
            .focusable()
            .tab_stop(true)
            .focus_visible(|style| style.bg(Colors::surface_control_hover()))
            .cursor(gpui::CursorStyle::PointingHand)
            .on_click(move |_e, w, cx| rescan_cb(w, cx)),
        );

    // ── Search field ────────────────────────────────────────────────
    let search_container = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(5.0))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_panel_alt())
        .child(
            svg()
                .path(assets::ICON_SEARCH_PATH)
                .w(px(11.0))
                .h(px(11.0))
                .text_color(Colors::text_muted()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(text_field_with_callbacks(
                    search_input,
                    search_focused,
                    search_callbacks,
                )),
        );

    // ── Row virtualization ──────────────────────────────────────────
    let nodes = Arc::new(state.visible_nodes());
    let count = nodes.len();
    crate::perf::count("browser_rows", count as u64);
    let scroll_for_thumb = scroll.0.borrow().base_handle.clone();
    let on_toggle_l = on_toggle.clone();
    let on_select_l = on_select.clone();
    let on_activate_l = on_activate_file.clone();
    let on_context_l = on_context_menu.clone();
    let nodes_for_list = nodes.clone();

    let listing_scroll = uniform_list("browser-tree", count, move |range, _window, _cx| {
        let nodes = nodes_for_list.clone();
        let on_toggle = on_toggle_l.clone();
        let on_select = on_select_l.clone();
        let on_activate = on_activate_l.clone();
        let on_context = on_context_l.clone();
        range
            .map(|i| {
                let node = &nodes[i];
                match node.kind {
                    BrowserNodeKind::GroupHeader => {
                        group_header_row(i, node, on_toggle.clone()).into_any_element()
                    }
                    BrowserNodeKind::Info => info_row(node).into_any_element(),
                    _ => tree_row(
                        i,
                        node,
                        on_toggle.clone(),
                        on_select.clone(),
                        on_activate.clone(),
                        on_context.clone(),
                        i18n,
                    )
                    .into_any_element(),
                }
            })
            .collect::<Vec<_>>()
    })
    .track_scroll(&scroll)
    .size_full()
    .px(px(2.0))
    .py(px(3.0));

    let thumb = scrollbar_thumb(scroll_for_thumb);

    let listing = div()
        .flex_1()
        .min_h_0()
        .relative()
        .child(listing_scroll)
        .child(thumb);

    // ── Mini waveform preview pane (shown for the selected audio file) ──
    let preview_pane = state
        .selected_audio_path()
        .map(|path| browser_waveform_pane(path, state.preview_position_for(path)));

    // ── Footer: current selection (lightweight info row) ─────────────
    let selected_label = state
        .selected
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| i18n.tr("browser.selection.none"));
    let footer = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .h(px(24.0))
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_input())
        .child(
            div()
                .text_size(px(8.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_muted())
                .child(i18n.tr("browser.selection.label")),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .truncate()
                .text_size(px(10.0))
                .text_color(Colors::text_muted())
                .child(truncate_path(&selected_label, 42)),
        );

    div()
        .flex()
        .flex_col()
        .w(px(SIDEBAR_WIDTH))
        .h_full()
        .bg(Colors::surface_panel())
        .border_r(px(1.0))
        .border_color(if active {
            Colors::panel_border_focused()
        } else {
            Colors::border_subtle()
        })
        .child(toolbar)
        .child(search_container)
        .child(listing)
        .children(preview_pane)
        .child(footer)
}

/// Mini waveform details for the selected audio file. Peaks come from the shared
/// waveform cache (decoded off-thread on select); while decoding it shows an
/// honest pending baseline. Selecting a file queues its audible audition.
///
/// `playhead_seconds` is the engine's real preview position for this exact file
/// (see `FileBrowserState::preview_position_for`), so the line below only ever
/// moves while audio is actually playing.
fn browser_waveform_pane(
    path: &std::path::Path,
    playhead_seconds: Option<f32>,
) -> impl IntoElement {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio")
        .to_string();
    let key = path.to_string_lossy().to_string();
    let preview = waveform_cache::get_preview_arc(&key);
    let meta = preview.as_ref().map(|p| {
        let secs = p.duration_seconds;
        let mins = (secs as u64) / 60;
        let rem = secs - (mins * 60) as f64;
        let sr = p.sample_rate as f32 / 1000.0;
        format!("{mins}:{rem:04.1} · {sr:.1}k")
    });

    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(20.0))
        .child(
            svg()
                .path(assets::ICON_FILE_PATH)
                .w(px(12.0))
                .h(px(12.0))
                .text_color(Colors::accent_primary()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .truncate()
                .text_size(px(10.5))
                .text_color(Colors::text_secondary())
                .child(name),
        )
        .children(meta.map(|m| {
            div()
                .text_size(px(9.0))
                .text_color(Colors::text_faint())
                .child(m)
        }));

    let waveform: gpui::AnyElement = match preview {
        Some(preview) => mini_waveform_canvas(preview, playhead_seconds).into_any_element(),
        None => div()
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .text_size(px(9.0))
            .text_color(Colors::text_faint())
            .child("Decoding waveform…")
            .into_any_element(),
    };

    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(8.0))
        .py(px(5.0))
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_input())
        .child(header)
        .child(
            div()
                .relative()
                .h(px(40.0))
                .w_full()
                .rounded_sm()
                .overflow_hidden()
                .bg(Colors::surface_base())
                .child(waveform),
        )
}

/// Draw a peak-rendered waveform filling the canvas. Columns are computed from
/// the real paint bounds (DPI-correct) using one min/max bar per pixel column —
/// canvas + `paint_quad`, never DOM spam (DESIGN.md waveform rules).
///
/// Two overlays share this canvas so they cannot disagree about the
/// seconds → x transform: the preview-limit shade (the part of a long file the
/// audition will not reach) and the playhead.
fn mini_waveform_canvas(
    preview: std::sync::Arc<waveform_cache::WaveformPreview>,
    playhead_seconds: Option<f32>,
) -> impl IntoElement {
    let mut color = Colors::accent_primary();
    color.a = 0.72;
    let mut past_limit_shade = Colors::surface_base();
    past_limit_shade.a = 0.55;
    let mut limit_marker = Colors::text_faint();
    limit_marker.a = 0.55;
    let playhead_color = Colors::timeline_playhead();
    let mut played_shade = Colors::accent_primary();
    played_shade.a = 0.10;
    let element = canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let w: f32 = f32::from(bounds.size.width).max(1.0);
            let h: f32 = f32::from(bounds.size.height).max(1.0);
            let center = h / 2.0;
            let cols = (w.floor() as usize).max(1);
            // Whole-file seconds → x. Both overlays below use only this.
            let duration = preview.duration_seconds.max(f64::MIN_POSITIVE);
            let x_at = |seconds: f64| ((seconds / duration) as f32).clamp(0.0, 1.0) * w;
            let samples_per_pixel = (preview.total_frames.max(1) as f32 / w).max(1.0);
            let Some(lod) = waveform_cache::pick_lod(&preview, samples_per_pixel) else {
                return;
            };
            let total = lod.peaks.len().max(1);
            for col in 0..cols {
                let frac0 = col as f32 / cols as f32;
                let frac1 = (col + 1) as f32 / cols as f32;
                let p0 = (frac0 * total as f32).floor() as usize;
                let p1 = (frac1 * total as f32).ceil() as usize;
                let end = p1.min(total).max(p0 + 1);
                let mut mn = 0.0f32;
                let mut mx = 0.0f32;
                for pk in &lod.peaks[p0..end] {
                    if pk.min < mn {
                        mn = pk.min;
                    }
                    if pk.max > mx {
                        mx = pk.max;
                    }
                }
                let top = center - mx.min(1.0) * center;
                let bottom = center - mn.max(-1.0) * center;
                let bar_h = (bottom - top).max(1.0);
                let r = Bounds::new(
                    bounds.origin + point(px(col as f32), px(top)),
                    size(px(1.0), px(bar_h)),
                );
                window.paint_quad(fill(r, color));
            }

            // Preview window: a selection only auditions the file's first
            // `AUDITION_PREVIEW_SECONDS`. Shade the rest so the waveform stays
            // honest about what will actually be heard.
            let limit_x = x_at(AUDITION_PREVIEW_SECONDS);
            if limit_x < w - 1.0 {
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(limit_x), px(0.0)),
                        size(px(w - limit_x), px(h)),
                    ),
                    past_limit_shade,
                ));
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(limit_x), px(0.0)),
                        size(px(1.0), px(h)),
                    ),
                    limit_marker,
                ));
            }

            // Playhead — engine position, drawn only while a preview is audible.
            if let Some(seconds) = playhead_seconds {
                let x = x_at(seconds as f64);
                if x > 0.0 {
                    window.paint_quad(fill(
                        Bounds::new(bounds.origin, size(px(x), px(h))),
                        played_shade,
                    ));
                }
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(x.min(w - 1.0)), px(0.0)),
                        size(px(1.0), px(h)),
                    ),
                    playhead_color,
                ));
            }
        },
    )
    .absolute()
    .inset_0();

    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(element)
}

/// Subtle, collapsible section header (COLLECTIONS / LIBRARY / PLACES).
fn group_header_row(
    index: usize,
    node: &BrowserVisibleNode,
    on_toggle: ToggleNodeCb,
) -> impl IntoElement {
    let id = node.id.clone();
    let expanded = node.expanded;
    let chevron = if expanded {
        assets::ICON_CHEVRON_DOWN_PATH
    } else {
        assets::ICON_CHEVRON_RIGHT_PATH
    };

    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .h(px(TREE_ROW_HEIGHT))
        .w_full()
        .pl(px(TREE_LEFT_PAD))
        .pr(px(6.0))
        .id(("browser-group", index))
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_hover()))
        // Subtle separator above each group (except the first). Absolutely
        // positioned so it never changes the row height uniform_list assumes.
        .child(if index == 0 {
            Empty.into_any_element()
        } else {
            div()
                .absolute()
                .top(px(0.0))
                .left(px(6.0))
                .right(px(6.0))
                .h(px(1.0))
                .bg(Colors::divider())
                .into_any_element()
        })
        .child(
            svg()
                .path(chevron)
                .w(px(9.0))
                .h(px(9.0))
                .text_color(Colors::text_muted()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .truncate()
                .text_size(px(9.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_muted())
                .child(node.label.to_uppercase()),
        )
        .on_click(move |_e, w, cx| {
            on_toggle(&(id.clone(), None), w, cx);
        })
}

/// Non-interactive empty-state / hint row (honest "no provider yet" state).
fn info_row(node: &BrowserVisibleNode) -> impl IntoElement {
    let depth = node.depth as f32;
    let is_error = node.error.is_some();
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(TREE_ROW_HEIGHT))
        .w_full()
        .pl(px(TREE_LEFT_PAD + depth * TREE_INDENT + DISCLOSURE_W))
        .pr(px(6.0))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .truncate()
                .text_size(px(9.5))
                .text_color(if is_error {
                    Colors::status_warning()
                } else {
                    Colors::text_faint()
                })
                .child(node.label.clone()),
        )
}

fn tree_row(
    index: usize,
    node: &BrowserVisibleNode,
    on_toggle: ToggleNodeCb,
    on_select: SelectEntryCb,
    on_activate_file: ActivateFileCb,
    on_context_menu: BrowserContextCb,
    i18n: I18n,
) -> impl IntoElement {
    let id = node.id.clone();
    let path = node.path.clone();
    let path_for_select = node.path.clone();
    let path_for_activate = node.path.clone();
    let path_for_toggle = node.path.clone();
    let path_for_context = node.path.clone();
    let label = node.label.clone();
    let expandable = node.expandable;
    let expanded = node.expanded;
    let selected = node.selected;
    let is_folder = node.kind == BrowserNodeKind::Folder;
    let is_file = node.kind == BrowserNodeKind::File;
    let is_audio = node.is_audio();
    let is_plugin_preset = node.is_plugin_preset();
    let is_root_item = node.depth == 1;
    let depth = node.depth as f32;

    let row_height = TREE_ROW_HEIGHT;

    let bg = if selected {
        Colors::surface_selected()
    } else {
        gpui::transparent_black().into()
    };

    let text_color = if selected {
        Colors::text_primary()
    } else if is_folder {
        Colors::text_secondary()
    } else if is_audio || node.is_midi() || is_plugin_preset {
        Colors::text_muted()
    } else {
        Colors::text_faint()
    };

    let icon_path = browser_icon_path(node.icon, expanded);
    let icon_color = browser_icon_color(selected);

    // Depth indent guides (start at depth 2 — depth-1 items sit under headers).
    let mut indent_guides = Vec::new();
    if node.depth > 1 {
        for level in 1..node.depth {
            let x = TREE_LEFT_PAD + (level as f32) * TREE_INDENT + DISCLOSURE_W * 0.5;
            indent_guides.push(
                div()
                    .absolute()
                    .top(px(0.0))
                    .bottom(px(0.0))
                    .left(px(x))
                    .w(px(1.0))
                    .bg(Colors::divider()),
            );
        }
    }

    let disclosure = div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(DISCLOSURE_W))
        .h_full()
        .child(disclosure_icon(expandable, expanded));

    let label_weight = if is_root_item {
        gpui::FontWeight::SEMIBOLD
    } else if is_folder {
        gpui::FontWeight::MEDIUM
    } else {
        gpui::FontWeight::NORMAL
    };

    let mut row = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .h(px(row_height))
        .w_full()
        .gap(px(3.0))
        .pl(px(TREE_LEFT_PAD + depth * TREE_INDENT))
        .pr(px(6.0))
        .bg(bg)
        .id(("browser-tree-row", index))
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(move |s| {
            s.bg(if selected {
                Colors::surface_pressed()
            } else {
                Colors::surface_hover()
            })
        })
        .children(indent_guides)
        .child(if selected {
            div()
                .absolute()
                .left(px(0.0))
                .top(px(3.0))
                .bottom(px(3.0))
                .w(px(2.0))
                .bg(Colors::accent_primary())
                .into_any_element()
        } else {
            Empty.into_any_element()
        })
        .child(disclosure)
        .child(
            svg()
                .path(icon_path)
                .w(px(12.0))
                .h(px(12.0))
                .text_color(icon_color),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .truncate()
                .text_size(px(11.0))
                .font_weight(label_weight)
                .text_color(text_color)
                .child(label.clone()),
        )
        .children(node.error.as_ref().map(|_| {
            div()
                .text_size(px(9.0))
                .text_color(Colors::status_error())
                .child(i18n.tr("browser.file.unavailable"))
        }));

    // Clicks and toggles
    let toggle_for_click = on_toggle.clone();
    let toggle_id = id.clone();
    let toggle_path = path_for_toggle.clone();
    let select_path = path_for_select.clone();
    row = row.on_click(move |event, w, cx| {
        if let Some(p) = select_path.as_ref() {
            on_select(p, w, cx);
        }
        if expandable {
            toggle_for_click(&(toggle_id.clone(), toggle_path.clone()), w, cx);
        } else if is_file && event.click_count() >= 2 {
            if let Some(p) = path_for_activate.as_ref() {
                on_activate_file(p, w, cx);
            }
        }
    });

    row = row.on_mouse_down(
        gpui::MouseButton::Right,
        move |event: &gpui::MouseDownEvent, window, cx| {
            let x: f32 = event.position.x.into();
            let y: f32 = event.position.y.into();
            on_context_menu(&(path_for_context.clone(), x, y), window, cx);
        },
    );

    let draggable_plugin_preset = path
        .as_ref()
        .and_then(|path| path.extension())
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pst"));
    if is_audio || draggable_plugin_preset {
        let drag_label = label.clone();
        if let Some(path) = path {
            row = row.on_drag(
                BrowserDragItem {
                    path,
                    label: drag_label,
                },
                |drag, _offset, _window, cx| {
                    cx.new(|_| BrowserDragPreview {
                        label: drag.label.clone(),
                    })
                },
            );
        }
    }

    row
}

/// Resolve a semantic [`BrowserIcon`] to a registered SVG glyph.
fn browser_icon_path(icon: BrowserIcon, expanded: bool) -> &'static str {
    match icon {
        BrowserIcon::Favorites => assets::ICON_STAR_PATH,
        BrowserIcon::Recent => assets::ICON_CLOCK_PATH,
        BrowserIcon::Samples => assets::ICON_SHARE_PATH,
        BrowserIcon::Instruments => assets::ICON_CPU_PATH,
        BrowserIcon::Plugins | BrowserIcon::PresetFile => assets::ICON_PLUG_PATH,
        BrowserIcon::AudioFiles | BrowserIcon::Music | BrowserIcon::AudioFile => {
            assets::ICON_MUSIC_PATH
        }
        BrowserIcon::Projects | BrowserIcon::ProjectFile => assets::ICON_SAVE_PATH,
        BrowserIcon::UserLibrary | BrowserIcon::Downloads | BrowserIcon::Desktop => {
            assets::ICON_FOLDER_PATH
        }
        BrowserIcon::Drive => assets::ICON_FOLDER_PATH,
        BrowserIcon::Folder => {
            if expanded {
                assets::ICON_FOLDER_OPEN_PATH
            } else {
                assets::ICON_FOLDER_PATH
            }
        }
        BrowserIcon::FolderOpen => assets::ICON_FOLDER_OPEN_PATH,
        BrowserIcon::Templates
        | BrowserIcon::Documents
        | BrowserIcon::Videos
        | BrowserIcon::MidiFile
        | BrowserIcon::GenericFile
        | BrowserIcon::None => assets::ICON_FILE_PATH,
    }
}

/// Monochrome icon tint. Browser glyphs carry their meaning in the shape, so
/// they share one neutral ramp instead of a per-content-type hue; selection is
/// already signalled by the row background and the brighter label.
fn browser_icon_color(selected: bool) -> gpui::Rgba {
    if selected {
        Colors::text_primary()
    } else {
        Colors::text_muted()
    }
}

fn scrollbar_thumb(scroll: ScrollHandle) -> impl IntoElement {
    let viewport_h: f32 = scroll.bounds().size.height.into();
    let max_y: f32 = scroll.max_offset().y.into();
    let raw_y: f32 = scroll.offset().y.into();
    let offset_y: f32 = -raw_y;

    if viewport_h <= 0.0 || max_y <= 0.5 {
        return div().w(px(0.0)).h(px(0.0)).into_any_element();
    }

    let content_h = viewport_h + max_y;
    let min_thumb = 24.0_f32;
    let thumb_h = ((viewport_h / content_h) * viewport_h).max(min_thumb);
    let track_room = (viewport_h - thumb_h).max(0.0);
    let progress = (offset_y / max_y).clamp(0.0, 1.0);
    let thumb_top = progress * track_room;

    div()
        .absolute()
        .top(px(thumb_top))
        .right(px(2.0))
        .w(px(4.0))
        .h(px(thumb_h))
        .rounded_full()
        .bg(Colors::with_alpha(Colors::text_primary(), 0.2))
        .into_any_element()
}

fn disclosure_icon(expandable: bool, expanded: bool) -> impl IntoElement {
    if expandable {
        let icon_path = if expanded {
            assets::ICON_CHEVRON_DOWN_PATH
        } else {
            assets::ICON_CHEVRON_RIGHT_PATH
        };
        svg()
            .path(icon_path)
            .w(px(9.0))
            .h(px(9.0))
            .text_color(Colors::text_muted())
            .into_any_element()
    } else {
        div().w(px(9.0)).h(px(9.0)).into_any_element()
    }
}

fn truncate_path(s: &str, max: usize) -> String {
    let max = max.max(4);
    let char_len = s.chars().count();
    if char_len <= max {
        return s.to_string();
    }

    // Take the last `max - 3` chars (leave room for "...") without slicing in the middle
    // of a UTF-8 codepoint.
    let keep = max.saturating_sub(3);
    let start = char_len.saturating_sub(keep);
    let tail: String = s.chars().skip(start).collect();
    format!("...{tail}")
}
