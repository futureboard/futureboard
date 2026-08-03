use gpui::{
    div, px, svg, App, Div, InteractiveElement, IntoElement, ParentElement, Rgba, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowControlArea,
};

use crate::assets;
use crate::platform_chrome::{PlatformChromePolicy, TITLEBAR_HEIGHT_PX};
use crate::theme::{self, Colors};

pub const TITLEBAR_HEIGHT: f32 = TITLEBAR_HEIGHT_PX;
pub const STATUSBAR_HEIGHT: f32 = 22.0;
pub const CHROME_ICON_BUTTON_SIZE: f32 = 26.0;
/// Windows caption controls are deliberately glyph-centric: no visible button
/// padding or boxed treatment. Other platforms retain their safer hit area.
#[cfg(target_os = "windows")]
pub const WINDOW_CONTROL_WIDTH: f32 = 40.0;
#[cfg(not(target_os = "windows"))]
pub const WINDOW_CONTROL_WIDTH: f32 = 34.0;
pub const CHROME_PAD_X: f32 = 6.0;
pub const CHROME_TEXT_SIZE: f32 = crate::theme::typography::UI_XS;
pub const CHROME_TITLE_SIZE: f32 = crate::theme::typography::UI_XS;
/// Windows 10 caption glyph family.
pub const WINDOWS_MDL2_ICON_FONT: &str = "Segoe MDL2 Assets";
/// Windows 11 caption glyph family.
pub const WINDOWS_FLUENT_ICON_FONT: &str = "Segoe Fluent Icons";

#[cfg(target_os = "windows")]
const WINDOWS_11_MIN_BUILD: u32 = 22_000;

#[cfg(target_os = "windows")]
fn windows_control_icon_font() -> &'static str {
    static FONT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FONT.get_or_init(|| windows_control_icon_font_for_build(windows_build_number()))
}

#[cfg(target_os = "windows")]
fn windows_build_number() -> Option<u32> {
    let mut version: windows::Win32::System::SystemInformation::OSVERSIONINFOW =
        unsafe { std::mem::zeroed() };
    version.dwOSVersionInfoSize = std::mem::size_of_val(&version) as u32;
    let status = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut version) };
    status.is_ok().then_some(version.dwBuildNumber)
}

#[cfg(target_os = "windows")]
fn windows_control_icon_font_for_build(build: Option<u32>) -> &'static str {
    if build.is_some_and(|build| build >= WINDOWS_11_MIN_BUILD) {
        WINDOWS_FLUENT_ICON_FONT
    } else {
        WINDOWS_MDL2_ICON_FONT
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_icon_font_tests {
    use super::*;

    #[test]
    fn selects_caption_font_for_windows_generation() {
        assert!(windows_build_number().is_some());
        assert_eq!(
            windows_control_icon_font_for_build(Some(19_045)),
            WINDOWS_MDL2_ICON_FONT
        );
        assert_eq!(
            windows_control_icon_font_for_build(Some(WINDOWS_11_MIN_BUILD)),
            WINDOWS_FLUENT_ICON_FONT
        );
        assert_eq!(
            windows_control_icon_font_for_build(None),
            WINDOWS_MDL2_ICON_FONT
        );
    }
}

/// Caption glyph for the current platform. Windows uses the icon font shipped
/// with that OS generation; other platforms retain the shared SVG icon set.
pub fn window_control_icon(
    area: WindowControlArea,
    icon_path: &'static str,
    fallback_text: impl Into<SharedString>,
) -> Div {
    #[cfg(target_os = "windows")]
    {
        let _ = fallback_text;
        let glyph = match area {
            WindowControlArea::Min => "\u{E921}",
            WindowControlArea::Max if icon_path == crate::assets::ICON_RESTORE_PATH => "\u{E923}",
            WindowControlArea::Max => "\u{E922}",
            WindowControlArea::Close => "\u{E8BB}",
            WindowControlArea::Drag => "",
        };
        div()
            .flex()
            .items_center()
            .justify_center()
            .font(gpui::font(windows_control_icon_font()))
            .text_size(px(10.0))
            .text_color(Colors::text_muted())
            .child(glyph)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = area;
        chrome_button(Some(icon_path), fallback_text, false, Colors::text_muted())
    }
}

pub fn section_separator() -> impl gpui::IntoElement {
    div()
        .w(px(1.0))
        .h(px(18.0))
        .mx(px(3.0))
        .bg(Colors::panel_border())
}

pub fn chrome_button(
    icon_path: Option<&'static str>,
    fallback_text: impl Into<SharedString>,
    _active: bool,
    color: Rgba,
) -> Div {
    // Transport and panel state is communicated by the semantic icon color
    // alone. Keeping the chrome flat makes the compact top bar read like a
    // desktop DAW rather than a row of boxed web controls.
    let mut button = div()
        .w(px(CHROME_ICON_BUTTON_SIZE))
        .h(px(CHROME_ICON_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .text_color(color);

    if let Some(path) = icon_path {
        button = button.child(svg().path(path).w(px(13.0)).h(px(13.0)).text_color(color));
    } else {
        button = button.child(fallback_text.into());
    }

    button
}

pub fn window_control_button(
    area: WindowControlArea,
    icon_path: &'static str,
    fallback_text: impl Into<SharedString>,
) -> Div {
    let button = window_control_icon(area, icon_path, fallback_text)
        .w(px(WINDOW_CONTROL_WIDTH))
        .h(px(TITLEBAR_HEIGHT))
        .rounded_none()
        .hover(move |style| {
            style.bg(if area == WindowControlArea::Close {
                Colors::accent_danger()
            } else {
                Colors::surface_control_hover()
            })
        })
        .window_control_area(area)
        .occlude();

    #[cfg(target_os = "linux")]
    let button = button.on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
        cx.stop_propagation();
        match area {
            WindowControlArea::Min => window.minimize_window(),
            WindowControlArea::Max => window.zoom_window(),
            WindowControlArea::Close => window.remove_window(),
            WindowControlArea::Drag => {}
        }
    });

    button
}

pub fn draggable_spacer() -> Div {
    div()
        .flex_1()
        .h_full()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _cx| {
            window.start_window_move();
        })
}

/// Compact title bar for external floating dialogs (Project Wizard, Preferences).
/// Drag uses an explicit flex spacer (same pattern as the main studio chrome) so
/// interactive children do not swallow window-move hit testing.
pub fn external_window_titlebar(
    title: impl Into<String>,
    close_id: impl Into<gpui::ElementId>,
    on_close: impl Fn(&mut Window, &mut App) + 'static + Clone,
) -> impl IntoElement {
    external_window_titlebar_with_icon(None, title, close_id, on_close)
}

/// Optional leading icon (e.g. Add Tracks dialog).
pub fn external_window_titlebar_with_icon(
    icon_path: Option<&'static str>,
    title: impl Into<String>,
    close_id: impl Into<gpui::ElementId>,
    on_close: impl Fn(&mut Window, &mut App) + 'static + Clone,
) -> impl IntoElement {
    let policy = PlatformChromePolicy::external_dialog();
    let on_close = on_close.clone();
    let title_text = crate::platform_chrome::branded_window_title(&title.into());

    let mut title_row = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .h_full()
        .flex_shrink_0();
    if let Some(path) = icon_path.filter(|_| policy.show_titlebar_icon()) {
        title_row = title_row.child(
            svg()
                .path(path)
                .w(px(13.0))
                .h(px(13.0))
                .text_color(Colors::accent_primary()),
        );
    }
    title_row = title_row.child(
        div()
            .text_size(px(CHROME_TITLE_SIZE))
            .font(theme::ui_font())
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(Colors::text_primary())
            .child(title_text),
    );

    let mut bar = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(policy.titlebar_height_px))
        .pl(policy.external_titlebar_left_padding())
        .pr(px(if policy.show_window_controls {
            0.0
        } else {
            CHROME_PAD_X
        }))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_titlebar())
        .child(title_row)
        .child(draggable_spacer());

    if policy.show_window_controls {
        bar = bar
            .child(external_window_control_button(
                WindowControlArea::Min,
                "external-window-minimize",
                assets::ICON_MINIMIZE_PATH,
                move |window, _cx| window.minimize_window(),
            ))
            .child(external_window_control_button(
                WindowControlArea::Max,
                "external-window-maximize",
                assets::ICON_MAXIMIZE_PATH,
                move |window, _cx| window.zoom_window(),
            ))
            .child(external_window_control_button(
                WindowControlArea::Close,
                close_id,
                assets::ICON_X_PATH,
                move |window, cx| on_close(window, cx),
            ));
    } else if policy.needs_drawn_close_fallback() {
        bar = bar.child(
            div()
                .id(close_id)
                .flex()
                .items_center()
                .justify_center()
                .w(px(WINDOW_CONTROL_WIDTH))
                .h(px(TITLEBAR_HEIGHT))
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::surface_control_hover()))
                .occlude()
                .on_click(move |_, window, cx| on_close(window, cx))
                .child(
                    svg()
                        .path(assets::ICON_X_PATH)
                        .w(px(12.0))
                        .h(px(12.0))
                        .text_color(Colors::text_faint()),
                ),
        );
    }

    bar
}

/// Close-only compact title bar for native message boxes (no min/max controls).
pub fn external_window_titlebar_compact(
    title: impl Into<String>,
    close_id: impl Into<gpui::ElementId>,
    on_close: impl Fn(&mut Window, &mut App) + 'static + Clone,
) -> impl IntoElement {
    let policy = PlatformChromePolicy::external_dialog();
    let on_close = on_close.clone();
    let title_text = crate::platform_chrome::branded_window_title(&title.into());

    let close_button = policy.needs_drawn_close_fallback().then(|| {
        div()
            .id(close_id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(WINDOW_CONTROL_WIDTH))
            .h(px(TITLEBAR_HEIGHT))
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| s.bg(Colors::surface_control_hover()))
            .occlude()
            .on_click(move |_, window, cx| on_close(window, cx))
            .child(
                window_control_icon(WindowControlArea::Close, assets::ICON_X_PATH, "X")
                    .text_color(Colors::text_faint()),
            )
    });

    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(policy.titlebar_height_px))
        .pl(policy.external_titlebar_left_padding())
        .pr(px(CHROME_PAD_X))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_titlebar())
        .child(
            div()
                .flex()
                .items_center()
                .h_full()
                .flex_shrink_0()
                .text_size(px(CHROME_TITLE_SIZE))
                .font(theme::ui_font())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_primary())
                .child(title_text),
        )
        .child(draggable_spacer())
        .children(close_button)
}

pub type WindowChromeCloseCb = std::sync::Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>;

/// Title bar for a window opened without platform caption controls, whose close
/// affordance exists only while closing is actually allowed. Used by the session
/// transaction window: during a handoff nothing may close it, and once it holds a
/// terminal error the same bar exposes the real dismiss action.
pub fn chromeless_window_titlebar(
    title: impl Into<String>,
    close_id: impl Into<gpui::ElementId>,
    on_close: Option<WindowChromeCloseCb>,
) -> impl IntoElement {
    let policy = PlatformChromePolicy::chromeless_dialog();
    let title_text = crate::platform_chrome::branded_window_title(&title.into());

    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(policy.titlebar_height_px))
        .pl(policy.external_titlebar_left_padding())
        .pr(px(CHROME_PAD_X))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_titlebar())
        .child(
            div()
                .flex()
                .items_center()
                .h_full()
                .flex_shrink_0()
                .text_size(px(CHROME_TITLE_SIZE))
                .font(theme::ui_font())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_primary())
                .child(title_text),
        )
        .child(draggable_spacer())
        .children(on_close.map(|on_close| {
            div()
                .id(close_id)
                .flex()
                .items_center()
                .justify_center()
                .w(px(WINDOW_CONTROL_WIDTH))
                .h(px(TITLEBAR_HEIGHT))
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::accent_danger()))
                .occlude()
                .on_click(move |_, window, cx| on_close(window, cx))
                .child(
                    window_control_icon(WindowControlArea::Close, assets::ICON_X_PATH, "X")
                        .text_color(Colors::text_faint()),
                )
        }))
}

fn external_window_control_button(
    area: WindowControlArea,
    id: impl Into<gpui::ElementId>,
    icon_path: &'static str,
    on_click: impl Fn(&mut Window, &mut App) + 'static + Clone,
) -> impl IntoElement {
    div()
        .window_control_area(area)
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(WINDOW_CONTROL_WIDTH))
        .h(px(TITLEBAR_HEIGHT))
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(move |style| {
            style.bg(if area == WindowControlArea::Close {
                Colors::accent_danger()
            } else {
                Colors::surface_control_hover()
            })
        })
        .active(|style| style.bg(Colors::button_bg_pressed()))
        .occlude()
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(window_control_icon(area, icon_path, "").text_color(Colors::text_faint()))
}

pub fn status_item(text: impl Into<String>, strong: bool) -> impl gpui::IntoElement {
    div()
        .h(px(18.0))
        .flex()
        .items_center()
        .px(px(6.0))
        .rounded_sm()
        .text_size(px(crate::theme::typography::UI_XS))
        .font(theme::ui_font())
        .font_weight(if strong {
            gpui::FontWeight::MEDIUM
        } else {
            gpui::FontWeight::NORMAL
        })
        .text_color(if strong {
            Colors::statusbar_text()
        } else {
            Colors::statusbar_text_muted()
        })
        .child(text.into())
}
