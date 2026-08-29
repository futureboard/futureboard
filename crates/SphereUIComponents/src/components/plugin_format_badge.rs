//! Shared plug-in format badge — VST3/CLAP brand icons, text fallback for other formats.

use gpui::{div, px, svg, AnyElement, IntoElement, ParentElement, Styled};

use crate::assets;
use crate::theme::Colors;
use SpherePluginHost::{PluginFormat, RegistryPlugin};

const FORMAT_ICON_SIZE: f32 = 22.0;

pub fn plugin_format_badge(format: PluginFormat) -> AnyElement {
    match format {
        PluginFormat::Vst3 => format_icon_badge(assets::ICON_PLUGIN_VST3_PATH),
        PluginFormat::Clap => format_icon_badge(assets::ICON_PLUGIN_CLAP_PATH),
        _ => text_format_badge(format),
    }
}

/// Format badge that labels Futureboard stock plug-ins as "Built-in" instead of
/// the `Unknown` format they store internally (no dedicated enum variant).
pub fn plugin_format_badge_for(plugin: &RegistryPlugin) -> AnyElement {
    if plugin.is_builtin() {
        return labeled_format_badge(
            "Built-in",
            Colors::accent_primary(),
            gpui::rgba(0x61AFEF18),
            Colors::accent_primary(),
        );
    }
    plugin_format_badge(plugin.format)
}

fn format_icon_badge(path: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(FORMAT_ICON_SIZE))
        .h(px(FORMAT_ICON_SIZE))
        .child(
            svg()
                .path(path)
                .w(px(FORMAT_ICON_SIZE))
                .h(px(FORMAT_ICON_SIZE))
                .text_color(Colors::text_primary()),
        )
        .into_any_element()
}

fn text_format_badge(format: PluginFormat) -> AnyElement {
    let (fg, bg, border) = match format {
        // A hosted format with no brand icon: read as a plain identity label,
        // not as the warning tone reserved for formats we cannot load.
        PluginFormat::Au | PluginFormat::Vst2 => (
            Colors::text_secondary(),
            Colors::surface_input(),
            Colors::border_default(),
        ),
        _ => (
            Colors::text_faint(),
            Colors::surface_input(),
            Colors::border_subtle(),
        ),
    };
    labeled_format_badge(format.label(), fg, bg, border)
}

fn labeled_format_badge(
    label: &'static str,
    fg: gpui::Rgba,
    bg: gpui::Rgba,
    border: gpui::Rgba,
) -> AnyElement {
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(border)
        .bg(bg)
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(fg)
        .child(label)
        .into_any_element()
}
