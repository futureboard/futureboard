use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, svg, App, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, Toggled, Window,
};

use crate::assets;
use crate::theme::Colors;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbButtonKind {
    Default,
    Primary,
}

pub fn fb_section_label(label: &'static str) -> impl IntoElement {
    div()
        .h(px(14.0))
        .flex()
        .items_center()
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_faint())
        .child(label)
}

pub fn fb_field_label(label: impl Into<String>) -> impl IntoElement {
    let label = label.into();
    div()
        .w(px(86.0))
        .flex_shrink_0()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(Colors::text_muted())
        .child(label)
}

pub fn fb_form_row(label: impl Into<String>, child: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .min_h(px(32.0))
        .child(fb_field_label(label))
        .child(div().flex_1().min_w_0().child(child))
}

pub fn fb_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    kind: FbButtonKind,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let primary = kind == FbButtonKind::Primary;
    let label: String = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_disabled(!enabled)
        .when(enabled, |this| {
            this.focusable()
                .tab_stop(true)
                .focus_visible(|style| style.border_color(Colors::border_focus()))
        })
        .flex()
        .items_center()
        .justify_center()
        .h(px(30.0))
        .min_w(px(if primary { 112.0 } else { 76.0 }))
        .px(px(12.0))
        .rounded_md()
        .border(px(1.0))
        .border_color(if primary {
            Colors::border_accent()
        } else {
            Colors::border_subtle()
        })
        .bg(if primary && enabled {
            Colors::accent_primary()
        } else if primary {
            Colors::accent_muted()
        } else {
            Colors::surface_input()
        })
        .opacity(if enabled { 1.0 } else { 0.45 })
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if primary && enabled {
            Colors::on_accent()
        } else {
            Colors::text_secondary()
        })
        .when(enabled, |this| {
            this.cursor(gpui::CursorStyle::PointingHand)
                .hover(move |s| {
                    if primary {
                        s.bg(Colors::accent_primary())
                    } else {
                        s.bg(Colors::surface_control_hover())
                    }
                })
                .on_click(on_click)
        })
        .child(label)
}

pub fn fb_stepper_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let accessible_label = match label {
        "+" => "Increase",
        "-" | "−" => "Decrease",
        label => label,
    };
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(accessible_label)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.border_color(Colors::border_focus()))
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(28.0))
        .rounded_md()
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_input())
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_secondary())
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| {
            s.bg(Colors::surface_control_hover())
                .border_color(Colors::border_strong())
        })
        .on_click(on_click)
        .child(label)
}

/// Compact section header used inside the Inspector. Slightly stronger than
/// [`fb_section_label`] — a row with an uppercase title and a hairline rule —
/// so the long Inspector section list is easy to scan.
pub fn fb_section_header(label: impl Into<String>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(18.0))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(9.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(Colors::text_faint())
                .child(label.into()),
        )
        .child(div().flex_1().h(px(1.0)).bg(Colors::border_subtle()))
}

/// Small color chip. Set `interactive` callers should wrap this in their own
/// clickable container; this is the visual swatch only.
pub fn fb_color_swatch(color: gpui::Rgba, size: f32) -> impl IntoElement {
    div()
        .w(px(size))
        .h(px(size))
        .rounded_sm()
        .border(px(1.0))
        .border_color(Colors::with_alpha(Colors::text_primary(), 0.18))
        .bg(color)
}

/// Compact DAW-style checkbox row. `checked` drives the box fill; clicking the
/// whole row fires `on_toggle`.
pub fn fb_checkbox(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    checked: bool,
    enabled: bool,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: String = label.into();
    let mut row = div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(label.clone())
        .aria_disabled(!enabled)
        .aria_toggled(if checked {
            Toggled::True
        } else {
            Toggled::False
        })
        .when(enabled, |this| {
            this.focusable()
                .tab_stop(true)
                .focus_visible(|style| style.bg(Colors::surface_hover()))
        })
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.0))
        .min_h(px(22.0))
        .child(
            div()
                .w(px(13.0))
                .h(px(13.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .border(px(1.0))
                .border_color(if checked {
                    Colors::accent_primary()
                } else {
                    Colors::border_subtle()
                })
                .bg(if checked {
                    Colors::accent_primary()
                } else {
                    Colors::surface_input()
                })
                .when(checked, |b| {
                    b.child(
                        svg()
                            .path(assets::ICON_CHECK_PATH)
                            .w(px(9.0))
                            .h(px(9.0))
                            .text_color(Colors::on_accent()),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(Colors::text_secondary())
                .child(label),
        );
    if enabled {
        row = row
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| s.bg(Colors::surface_hover()))
            .on_click(on_toggle);
    } else {
        row = row.opacity(0.45);
    }
    row
}

pub fn fb_segmented_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: String = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_toggled(if active {
            Toggled::True
        } else {
            Toggled::False
        })
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.border_color(Colors::border_focus()))
        .flex()
        .items_center()
        .justify_center()
        .flex_1()
        .h(px(28.0))
        .min_w(px(44.0))
        .px(px(8.0))
        .rounded_md()
        .border(px(1.0))
        .border_color(if active {
            Colors::border_accent()
        } else {
            Colors::border_subtle()
        })
        .bg(if active {
            Colors::accent_muted()
        } else {
            Colors::surface_input()
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
        .on_click(on_click)
        .child(label)
}
