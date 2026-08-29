use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    div, px, svg, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window,
};

use crate::assets;
use crate::theme::Colors;

pub fn combobox_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var_os("FUTUREBOARD_COMBOBOX_DEBUG").is_some()
            || std::env::var_os("FUTUREBOARD_OVERLAY_DEBUG").is_some()
    })
}

fn combobox_debug(message: &str) {
    if combobox_debug_enabled() {
        eprintln!("[combobox] {message}");
    }
}

/// Remove duplicate labels while preserving first-seen order.
pub fn dedupe_preserve_order(options: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(options.len());
    for option in options {
        if seen.insert(option.clone()) {
            out.push(option.clone());
        } else if combobox_debug_enabled() {
            combobox_debug(&format!("duplicate option detected: {option}"));
        }
    }
    out
}

#[derive(Clone, Copy)]
pub struct ComboBoxOption<T: Copy + PartialEq + 'static> {
    pub label: &'static str,
    pub value: T,
}

pub fn combo_box_trigger(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    open: bool,
    on_mouse_down: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(30.0))
        .w_full()
        .min_w(px(0.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(if open {
            Colors::border_focus()
        } else {
            Colors::border_subtle()
        })
        .bg(if open {
            Colors::surface_card()
        } else {
            Colors::surface_input()
        })
        .px(px(9.0))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| {
            s.bg(Colors::surface_control_hover())
                .border_color(Colors::border_strong())
        })
        .on_mouse_down(gpui::MouseButton::Left, on_mouse_down)
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .truncate()
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_primary())
                .child(label.into()),
        )
        .child(
            svg()
                .path(assets::ICON_CHEVRON_DOWN_PATH)
                .w(px(11.0))
                .h(px(11.0))
                .flex_shrink_0()
                .text_color(Colors::text_faint()),
        )
}

pub fn combo_box_menu<T: Copy + PartialEq + 'static>(
    id: impl Into<gpui::ElementId>,
    position: crate::overlay::OverlayPosition,
    selected: T,
    options: &'static [ComboBoxOption<T>],
    on_select: Arc<dyn Fn(T, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let left: f32 = position.x.into();
    let top: f32 = position.y.into();
    let width: f32 = position.width.map(|w| w.into()).unwrap_or(120.0);
    let max_h: f32 = position.max_height.map(|h| h.into()).unwrap_or(200.0);
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .max_h(px(max_h))
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_card())
        .shadow(vec![gpui::BoxShadow {
            color: Colors::surface_overlay().into(),
            offset: gpui::point(px(0.0), px(10.0)),
            blur_radius: px(28.0),
            spread_radius: px(0.0),
            inset: false,
        }])
        .p(px(4.0))
        .id(id)
        .overflow_y_scroll()
        .occlude()
        .children(options.iter().enumerate().map(|(index, option)| {
            let active = option.value == selected;
            let value = option.value;
            let on_select = on_select.clone();
            div()
                .id(("combo-box-option", index))
                .h(px(25.0))
                .w_full()
                .rounded(px(crate::theme::radius::CONTROL))
                .px(px(8.0))
                .flex()
                .items_center()
                .justify_between()
                .bg(if active {
                    Colors::accent_muted()
                } else {
                    gpui::transparent_black().into()
                })
                .text_size(px(10.5))
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if active {
                    Colors::text_primary()
                } else {
                    Colors::text_secondary()
                })
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::surface_control_hover()))
                .on_click(move |_, window, cx| on_select(value, window, cx))
                .child(option.label)
                .children(active.then(|| {
                    svg()
                        .path(assets::ICON_CHECK_PATH)
                        .w(px(11.0))
                        .h(px(11.0))
                        .text_color(Colors::accent_primary())
                }))
        }))
}

pub fn combo_box_string_menu(
    id: impl Into<gpui::ElementId>,
    position: crate::overlay::OverlayPosition,
    selected: &str,
    options: &[String],
    on_select: Arc<dyn Fn(String, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let deduped = dedupe_preserve_order(options);
    combobox_debug(&format!(
        "open options={} unique={} selected={selected}",
        options.len(),
        deduped.len()
    ));
    let left: f32 = position.x.into();
    let top: f32 = position.y.into();
    let width: f32 = position.width.map(|w| w.into()).unwrap_or(120.0);
    let max_h: f32 = position.max_height.map(|h| h.into()).unwrap_or(148.0);
    #[cfg(target_os = "windows")]
    let platform = "windows";
    #[cfg(not(target_os = "windows"))]
    let platform = "other";
    combobox_debug(&format!(
        "menu_bounds platform={platform} x={left:.0} y={top:.0} w={width:.0} max_h={max_h:.0}"
    ));
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .max_h(px(max_h))
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_card())
        .shadow(vec![gpui::BoxShadow {
            color: Colors::surface_overlay().into(),
            offset: gpui::point(px(0.0), px(6.0)),
            blur_radius: px(18.0),
            spread_radius: px(0.0),
            inset: false,
        }])
        .p(px(4.0))
        .id(id)
        .overflow_y_scroll()
        .occlude()
        .children(deduped.iter().enumerate().map(|(index, option)| {
            let active = option == selected;
            let value = option.clone();
            let on_select = on_select.clone();
            div()
                .id(("combo-box-string-option", index))
                .min_h(px(25.0))
                .w_full()
                .rounded(px(crate::theme::radius::CONTROL))
                .px(px(8.0))
                .py(px(4.0))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .bg(if active {
                    Colors::accent_muted()
                } else {
                    gpui::transparent_black().into()
                })
                .text_size(px(10.5))
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if active {
                    Colors::text_primary()
                } else {
                    Colors::text_secondary()
                })
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::surface_control_hover()))
                .on_click(move |_, window, cx| on_select(value.clone(), window, cx))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex_1()
                        .truncate()
                        .child(option.clone()),
                )
                .children(active.then(|| {
                    svg()
                        .path(assets::ICON_CHECK_PATH)
                        .w(px(11.0))
                        .h(px(11.0))
                        .flex_shrink_0()
                        .text_color(Colors::accent_primary())
                }))
        }))
}
