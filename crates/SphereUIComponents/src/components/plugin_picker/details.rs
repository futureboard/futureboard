//! Plugin details panel for the picker.

use gpui::prelude::FluentBuilder;
use gpui::{div, px, IntoElement, ParentElement, Styled};

use crate::components::plugin_picker::category::normalized_category_label;
use crate::components::plugin_picker::insert::{
    validate_insert, InsertValidation, PluginInsertTarget,
};
use crate::components::plugin_picker::list_view::{format_badge_for, scan_status_label};
use crate::theme::Colors;
use SpherePluginHost::RegistryPlugin;

pub const DETAILS_WIDTH: f32 = 228.0;

pub fn plugin_details_panel(
    plugin: &RegistryPlugin,
    insert_target: &PluginInsertTarget,
) -> impl IntoElement {
    let validation = validate_insert(plugin, insert_target);
    let scan = scan_status_label(plugin).unwrap_or("Ready");
    let kind = plugin.kind.label();

    div()
        .flex()
        .flex_col()
        .w(px(DETAILS_WIDTH))
        .min_w(px(DETAILS_WIDTH))
        .flex_shrink_0()
        .border_l(px(1.0))
        .border_color(Colors::divider())
        .bg(Colors::surface_panel_alt())
        .child(
            div()
                .px(px(12.0))
                .py(px(10.0))
                .border_b(px(1.0))
                .border_color(Colors::divider())
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_faint())
                .child("Plug-in Details"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(10.0))
                .child(detail_row("Name", &plugin.name))
                .child(detail_row("Vendor", &plugin.vendor))
                .child(detail_row(
                    "Format",
                    if plugin.is_builtin() {
                        "Built-in"
                    } else {
                        plugin.format.label()
                    },
                ))
                .child(detail_row("Category", &normalized_category_label(plugin)))
                .child(detail_row("Kind", kind))
                .child(detail_row("Path", &plugin.path.display().to_string()))
                .when_some(plugin.version.as_deref(), |panel, version| {
                    panel.child(detail_row("Version", version))
                })
                .when_some(plugin.class_id.as_deref(), |panel, class_id| {
                    panel.child(detail_row("Class ID", class_id))
                })
                .child(detail_row("Scan Status", scan))
                .when_some(plugin.error_message.as_deref(), |panel, error| {
                    panel.child(detail_row("Error", error))
                })
                .child(div().pt(px(4.0)).child(format_badge_for(plugin)))
                .when(validation != InsertValidation::Ok, |panel| {
                    panel.child(
                        div()
                            .mt(px(6.0))
                            .px(px(8.0))
                            .py(px(6.0))
                            .rounded_md()
                            .bg(gpui::rgba(0xE5C07B18))
                            .text_size(px(10.0))
                            .text_color(Colors::status_warning())
                            .child(
                                validation
                                    .message()
                                    .unwrap_or("Cannot insert this plug-in here."),
                            ),
                    )
                }),
        )
}

fn detail_row(label: &'static str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_faint())
                .child(label),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(Colors::text_secondary())
                .child(value.to_string()),
        )
}
