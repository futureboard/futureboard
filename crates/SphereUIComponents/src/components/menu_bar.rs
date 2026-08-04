use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, svg, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window,
};

use crate::assets;
use crate::i18n::I18n;
use crate::menu::MenuManifest;
use crate::overlay::{compute_overlay_position, OverlayAnchor, OverlayPlacement, OverlaySize};
use crate::platform_chrome::PlatformChromePolicy;
use crate::theme::{menu as menu_style, Colors};

use super::title_bar::{CHROME_TEXT_SIZE, TITLEBAR_HEIGHT};

pub type MenuOpenCb = Arc<dyn Fn(&(String, f32), &mut Window, &mut App) + 'static>;
pub type MenuCloseCb = Arc<dyn Fn(&(), &mut Window, &mut App) + 'static>;

/// `open_menu_id` value while the compact hamburger picker is shown.
pub const MENU_PICKER_ID: &str = "__menu_picker__";

const PICKER_PANEL_WIDTH: f32 = 168.0;
const PICKER_ROW_HEIGHT: f32 = menu_style::ROW_HEIGHT;
const MENU_BAR_PAD_X: f32 = 2.0;
const MENU_LABEL_PAD_X: f32 = 7.0;
const MENU_LABEL_GAP: f32 = 1.0;
const MENU_LABEL_CHAR_W: f32 = 6.2;
const COMPACT_MENU_BUTTON_SIZE: f32 = 16.0;

pub fn menu_bar_chrome_width(viewport_width: f32, i18n: I18n) -> f32 {
    if PlatformChromePolicy::menubar_compact(viewport_width) {
        MENU_BAR_PAD_X * 2.0 + COMPACT_MENU_BUTTON_SIZE
    } else {
        let manifest = MenuManifest::load();
        let labels_width = manifest
            .menus
            .iter()
            .map(|menu| menu_label_width(&i18n.tr_menu(&menu.id, &menu.label)))
            .sum::<f32>();
        let gaps = manifest.menus.len().saturating_sub(1) as f32 * MENU_LABEL_GAP;
        MENU_BAR_PAD_X * 2.0 + labels_width + gaps
    }
}

pub fn menu_bar(
    open_menu_id: Option<&str>,
    on_open_menu: MenuOpenCb,
    viewport_width: f32,
    i18n: I18n,
) -> impl IntoElement {
    if PlatformChromePolicy::menubar_compact(viewport_width) {
        menu_bar_compact(open_menu_id, on_open_menu).into_any_element()
    } else {
        menu_bar_full(open_menu_id, on_open_menu, i18n).into_any_element()
    }
}

fn menu_bar_full(
    open_menu_id: Option<&str>,
    on_open_menu: MenuOpenCb,
    i18n: I18n,
) -> impl IntoElement {
    let manifest = MenuManifest::load();
    let open_id_owned = open_menu_id.map(|s| s.to_string());
    let chrome_left: f32 = PlatformChromePolicy::current()
        .traffic_light_left_padding()
        .into();
    let mut next_label_left = chrome_left + MENU_BAR_PAD_X;

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(MENU_LABEL_GAP))
        .h(px(TITLEBAR_HEIGHT))
        .px(px(MENU_BAR_PAD_X))
        .children(manifest.menus.iter().enumerate().map(|(i, menu)| {
            let is_open = open_id_owned.as_deref() == Some(menu.id.as_str());
            let menu_id = menu.id.clone();
            let hover_menu_id = menu.id.clone();
            let label = i18n.tr_menu(&menu.id, &menu.label);
            let anchor_x = next_label_left;
            next_label_left += menu_label_width(&label) + MENU_LABEL_GAP;
            let cb = on_open_menu.clone();
            let hover_cb = on_open_menu.clone();
            let can_hover_switch = open_id_owned.is_some() && !is_open;

            menu_label_button(
                ("top-menu", i),
                label,
                is_open,
                can_hover_switch,
                move |hovered, window, cx| {
                    if *hovered {
                        hover_cb(&(hover_menu_id.clone(), anchor_x), window, cx);
                    }
                },
                move |_event, window, cx| {
                    cb(&(menu_id.clone(), anchor_x), window, cx);
                },
            )
        }))
}

fn menu_bar_compact(open_menu_id: Option<&str>, on_open_menu: MenuOpenCb) -> impl IntoElement {
    let is_open = open_menu_id == Some(MENU_PICKER_ID);
    let cb = on_open_menu.clone();
    let chrome_left: f32 = PlatformChromePolicy::current()
        .traffic_light_left_padding()
        .into();
    let anchor_x = chrome_left + MENU_BAR_PAD_X;

    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(TITLEBAR_HEIGHT))
        .px(px(MENU_BAR_PAD_X))
        .child(
            div()
                .id("top-menu-hamburger")
                .w(px(COMPACT_MENU_BUTTON_SIZE))
                .h(px(COMPACT_MENU_BUTTON_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(if is_open {
                    Colors::surface_control_hover()
                } else {
                    gpui::transparent_black().into()
                })
                .hover(|s| s.bg(Colors::surface_control_hover()))
                .cursor(gpui::CursorStyle::PointingHand)
                .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                    cb(&(MENU_PICKER_ID.to_string(), anchor_x), window, cx);
                })
                .occlude()
                .child(
                    svg()
                        .path(assets::ICON_MENU_PATH)
                        .w(px(14.0))
                        .h(px(14.0))
                        .text_color(if is_open {
                            Colors::text_primary()
                        } else {
                            Colors::text_muted()
                        }),
                ),
        )
}

/// Compact-mode overlay: pick a top-level menu (File, Edit, …) then open its panel.
pub fn menu_picker_dropdown(
    anchor: OverlayAnchor,
    viewport_width: f32,
    viewport_height: f32,
    on_open_menu: MenuOpenCb,
    on_close: MenuCloseCb,
    i18n: I18n,
) -> impl IntoElement {
    let manifest = MenuManifest::load();
    let row_count = manifest.menus.len();
    let panel_height = menu_style::PANEL_PAD * 2.0
        + row_count as f32 * PICKER_ROW_HEIGHT
        + (row_count.saturating_sub(1)) as f32 * menu_style::ITEM_GAP;

    let window_bounds = gpui::bounds(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(px(viewport_width), px(viewport_height)),
    );
    let pos = compute_overlay_position(
        anchor.bounds,
        OverlaySize {
            width: PICKER_PANEL_WIDTH,
            height: panel_height.max(80.0),
        },
        window_bounds,
        OverlayPlacement::BottomStart,
        4.0,
    );
    let panel_left: f32 = pos.x.into();
    let panel_top: f32 = pos.y.into();

    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(div().absolute().top_0().left_0().size_full().on_mouse_down(
            gpui::MouseButton::Left,
            {
                let on_close = on_close.clone();
                move |_, window, cx| on_close(&(), window, cx)
            },
        ))
        .child(
            div()
                .absolute()
                .left(px(panel_left))
                .top(px(panel_top))
                .w(px(PICKER_PANEL_WIDTH))
                .flex()
                .flex_col()
                .p(px(menu_style::PANEL_PAD))
                .gap(px(menu_style::ITEM_GAP))
                .rounded_md()
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .bg(Colors::surface_raised())
                .shadow_lg()
                .children(manifest.menus.iter().enumerate().map(|(i, menu)| {
                    let menu_id = menu.id.clone();
                    let label = i18n.tr_menu(&menu.id, &menu.label);
                    let cb = on_open_menu.clone();
                    div()
                        .id(("menu-picker-row", i))
                        .h(px(PICKER_ROW_HEIGHT))
                        .px(px(menu_style::ROW_PAD_X))
                        .flex()
                        .items_center()
                        .rounded_md()
                        .text_size(px(menu_style::LABEL_TEXT_SIZE))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(Colors::text_primary())
                        .hover(|s| s.bg(Colors::surface_control_hover()))
                        .cursor(gpui::CursorStyle::PointingHand)
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                            cb(&(menu_id.clone(), panel_left), window, cx);
                        })
                        .occlude()
                        .child(label)
                })),
        )
}

fn menu_label_width(label: &str) -> f32 {
    MENU_LABEL_PAD_X * 2.0 + label.chars().count() as f32 * MENU_LABEL_CHAR_W
}

pub fn menu_label_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    active: bool,
    enable_hover_switch: bool,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
    on_mouse_down: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(24.0))
        .px(px(MENU_LABEL_PAD_X))
        .flex()
        .items_center()
        .rounded_md()
        .text_size(px(CHROME_TEXT_SIZE))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if active {
            Colors::text_primary()
        } else {
            Colors::text_muted()
        })
        .bg(if active {
            Colors::surface_control_hover()
        } else {
            gpui::transparent_black().into()
        })
        .hover(|s| {
            s.bg(Colors::surface_control_hover())
                .text_color(Colors::text_primary())
        })
        .cursor(gpui::CursorStyle::PointingHand)
        .when(enable_hover_switch, |this| this.on_hover(on_hover))
        .on_mouse_down(gpui::MouseButton::Left, on_mouse_down)
        .occlude()
        .child(label.into())
}
