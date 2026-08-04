use crate::assets;
use crate::i18n::I18n;
use crate::theme::Colors;
use gpui::{
    div, px, svg, App, AppContext, Empty, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BottomTab {
    Mixer,
    Editor,
    EffectEditor,
}

/// Persistent state for the resizable bottom panel.
/// `resize_start_*` are transient — recorded on mouse-down so the on_drag_move
/// handler can recompute height as a pure function of the current mouse Y.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BottomPanelState {
    pub height_px: f32,
    pub min_height_px: f32,
    pub max_height_px: f32,
    pub is_resizing: bool,
    pub resize_start_y: f32,
    pub resize_start_height: f32,
}

impl Default for BottomPanelState {
    fn default() -> Self {
        Self {
            height_px: 280.0,
            min_height_px: 180.0,
            max_height_px: 720.0,
            is_resizing: false,
            resize_start_y: 0.0,
            resize_start_height: 0.0,
        }
    }
}

/// Zero-sized marker used as the drag payload for the bottom panel resize handle.
#[derive(Clone, Copy, Debug, Default)]
pub struct BottomPanelResizeDrag;

impl Render for BottomPanelResizeDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

// ─── Sub-components for Editor ───────────────────────────────────────────────

/// Fallback for the docked Editor tab when no real Piano Roll element is
/// supplied. Previously this drew mock MIDI notes; that violated the
/// "no mock timeline rendering" rule, so it is now an empty placeholder.
/// The normal layout always passes the real `PianoRoll` and never hits this.
fn editor_panel() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size_full()
        .bg(Colors::surface_base())
        .text_size(px(11.0))
        .text_color(Colors::text_muted())
        .child("Select a clip to edit")
}

// ─── Sub-components for Effect Editor ────────────────────────────────────────

fn effect_editor_panel() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size_full()
        .bg(Colors::surface_base())
        .text_size(px(11.0))
        .text_color(Colors::text_muted())
        .child("Effect Editor is provided by the live insert-chain view")
}

// ─── Main Bottom Panel ───────────────────────────────────────────────────────

fn tab_button(
    id: &'static str,
    label: impl Into<String>,
    icon_path: &'static str,
    tab: BottomTab,
    active_tab: BottomTab,
    on_click: std::sync::Arc<impl Fn(&BottomTab, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let active = tab == active_tab;
    let on_click_clone = on_click.clone();
    let text_color = if active {
        Colors::tab_text_active()
    } else {
        Colors::tab_text_muted()
    };

    let mut btn = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(10.0))
        .rounded_md()
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        // Apply text color at the button level too so the label (a plain
        // string child, not an svg) inherits the right color. Previously
        // only the icon got `.text_color`, so inactive labels rendered with
        // the default (black) text color.
        .text_color(text_color)
        .id(id)
        .on_click(move |_, window, cx| {
            on_click_clone(&tab, window, cx);
        })
        .child(
            svg()
                .path(icon_path)
                .w(px(14.0))
                .h(px(14.0))
                .text_color(text_color),
        )
        .child(label.into());

    if active {
        btn = btn
            .bg(Colors::tab_bg_active())
            // Accent indicator at the bottom
            .child(
                div()
                    .absolute()
                    .bottom(px(0.0))
                    .left(px(6.0))
                    .right(px(6.0))
                    .h(px(2.0))
                    .bg(Colors::tab_indicator_active()),
            );
    } else {
        btn = btn.hover(|style| {
            style
                .bg(Colors::tab_bg_hover())
                .text_color(Colors::tab_text())
        });
    }

    btn
}

pub fn bottom_panel(
    active_tab: BottomTab,
    panel_state: BottomPanelState,
    mixer_tab: gpui::AnyElement,
    editor_content: Option<gpui::AnyElement>,
    on_tab_click: impl Fn(&BottomTab, &mut Window, &mut App) + 'static,
    on_resize_start: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
    on_resize_move: impl Fn(&gpui::DragMoveEvent<BottomPanelResizeDrag>, &mut Window, &mut App)
        + 'static,
    on_resize_end: impl Fn(&gpui::MouseUpEvent, &mut Window, &mut App) + 'static,
    i18n: I18n,
) -> impl IntoElement {
    let on_tab_click = std::sync::Arc::new(on_tab_click);
    let mut editor_content = editor_content;
    div()
        .flex()
        .flex_col()
        .h(px(panel_state.height_px))
        .w_full()
        .border_t(px(1.0))
        .border_color(Colors::panel_border())
        .bg(Colors::bottom_panel_bg())
        .relative()
        // While dragging, listen for move events on the whole panel.
        .on_drag_move::<BottomPanelResizeDrag>(on_resize_move)
        .on_mouse_up(gpui::MouseButton::Left, on_resize_end)
        // Resize handle — 5px strip pinned to the top edge.
        .child(
            div()
                .absolute()
                .top(px(-2.0))
                .left_0()
                .right_0()
                .h(px(5.0))
                .id("bottom-panel-resize-handle")
                .cursor(gpui::CursorStyle::ResizeUpDown)
                .hover(|s| s.bg(Colors::accent_soft()))
                .on_mouse_down(gpui::MouseButton::Left, on_resize_start)
                .on_drag(BottomPanelResizeDrag, |_drag, _offset, _window, cx| {
                    cx.new(|_| BottomPanelResizeDrag)
                }),
        )
        // Tab Header
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(28.0))
                .px(px(8.0))
                .border_b(px(1.0))
                .border_color(Colors::panel_border())
                .bg(Colors::bottom_panel_header_bg())
                .child(tab_button(
                    "bottom-tab-mixer",
                    i18n.tr("bottom-panel.tab.mixer"),
                    assets::ICON_SLIDERS_HORIZONTAL_PATH,
                    BottomTab::Mixer,
                    active_tab,
                    on_tab_click.clone(),
                ))
                .child(tab_button(
                    "bottom-tab-editor",
                    i18n.tr("bottom-panel.tab.editor"),
                    assets::ICON_PENCIL_PATH,
                    BottomTab::Editor,
                    active_tab,
                    on_tab_click.clone(),
                ))
                .child(tab_button(
                    "bottom-tab-effect-editor",
                    i18n.tr("bottom-panel.tab.effect-editor"),
                    assets::ICON_SPARKLES_PATH,
                    BottomTab::EffectEditor,
                    active_tab,
                    on_tab_click,
                )),
        )
        // Tab Content — must declare itself as a flex container so the
        // active panel can `size_full` into the remaining space below the
        // tab header. Without `flex().flex_col()` the panel collapsed to
        // its content height and left a gap at the bottom.
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(match active_tab {
                    BottomTab::Mixer => mixer_tab,
                    BottomTab::Editor => editor_content
                        .take()
                        .unwrap_or_else(|| editor_panel().into_any_element()),
                    BottomTab::EffectEditor => effect_editor_panel().into_any_element(),
                }),
        )
}
