//! Plugin picker sidebar filter rail.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, InteractiveElement, IntoElement, ParentElement, ScrollHandle,
    StatefulInteractiveElement, Styled, Window,
};

use crate::components::plugin_picker::filter::FilterCounts;
use crate::components::plugin_picker::state::PickerFilter;
use crate::components::scroll_thumb::vertical_scrollbar_thumb;
use crate::theme::Colors;
use SpherePluginHost::PluginFormat;

pub const SIDEBAR_WIDTH: f32 = 184.0;

type FilterCb = Arc<dyn Fn(&PickerFilter, &mut Window, &mut App) + 'static>;

pub fn plugin_filter_sidebar(
    active: &PickerFilter,
    counts: &FilterCounts,
    vendors: &[String],
    categories: &[String],
    debug_mode: bool,
    au_available: bool,
    filter_cb: FilterCb,
    sidebar_scroll: &ScrollHandle,
) -> impl IntoElement {
    let mut col = div().flex().flex_col().w(px(SIDEBAR_WIDTH)).py(px(4.0));

    col = col.child(sidebar_section_label("Library"));
    col = col.child(sidebar_item(
        "pp-filter-all",
        "All",
        Some(counts.all),
        active == &PickerFilter::All,
        filter_cb.clone(),
        PickerFilter::All,
    ));
    col = col.child(sidebar_item(
        "pp-filter-fav",
        "Favorites",
        Some(counts.favorites),
        active == &PickerFilter::Favorites,
        filter_cb.clone(),
        PickerFilter::Favorites,
    ));
    col = col.child(sidebar_item(
        "pp-filter-recent",
        "Recently Used",
        Some(counts.recent),
        active == &PickerFilter::RecentlyUsed,
        filter_cb.clone(),
        PickerFilter::RecentlyUsed,
    ));

    col = col.child(sidebar_section_label("Kind"));
    col = col.child(sidebar_item(
        "pp-filter-inst",
        "Instruments",
        Some(counts.instruments),
        active == &PickerFilter::Instruments,
        filter_cb.clone(),
        PickerFilter::Instruments,
    ));
    col = col.child(sidebar_item(
        "pp-filter-fx",
        "Effects",
        Some(counts.effects),
        active == &PickerFilter::Effects,
        filter_cb.clone(),
        PickerFilter::Effects,
    ));

    col = col.child(sidebar_section_label("Format"));
    col = col.child(sidebar_item(
        "pp-filter-vst3",
        "VST3",
        Some(counts.vst3),
        active == &PickerFilter::Format(PluginFormat::Vst3),
        filter_cb.clone(),
        PickerFilter::Format(PluginFormat::Vst3),
    ));
    col = col.child(sidebar_item(
        "pp-filter-vst2",
        "VST2",
        Some(counts.vst2),
        active == &PickerFilter::Format(PluginFormat::Vst2),
        filter_cb.clone(),
        PickerFilter::Format(PluginFormat::Vst2),
    ));
    col = col.child(sidebar_item(
        "pp-filter-clap",
        "CLAP",
        Some(counts.clap),
        active == &PickerFilter::Format(PluginFormat::Clap),
        filter_cb.clone(),
        PickerFilter::Format(PluginFormat::Clap),
    ));
    col = col.child(sidebar_item(
        "pp-filter-builtin",
        "Built-in",
        Some(counts.builtin),
        active == &PickerFilter::Builtin,
        filter_cb.clone(),
        PickerFilter::Builtin,
    ));
    if au_available {
        col = col.child(sidebar_item(
            "pp-filter-au",
            if cfg!(target_os = "macos") {
                "AudioUnit"
            } else {
                "AudioUnit (Unavailable)"
            },
            Some(counts.au),
            active == &PickerFilter::Format(PluginFormat::Au),
            filter_cb.clone(),
            PickerFilter::Format(PluginFormat::Au),
        ));
    }

    if debug_mode && counts.failed > 0 {
        col = col.child(sidebar_section_label("Debug"));
        col = col.child(sidebar_item(
            "pp-filter-failed",
            "Failed / Missing",
            Some(counts.failed),
            active == &PickerFilter::Failed,
            filter_cb.clone(),
            PickerFilter::Failed,
        ));
    }

    if !vendors.is_empty() {
        col = col.child(sidebar_section_label("Vendors"));
        for (i, vendor) in vendors.iter().enumerate() {
            let active_vendor =
                matches!(active, PickerFilter::Vendor(v) if v.eq_ignore_ascii_case(vendor));
            col = col.child(sidebar_item(
                ("pp-filter-vendor", i),
                vendor.clone(),
                None,
                active_vendor,
                filter_cb.clone(),
                PickerFilter::Vendor(vendor.clone()),
            ));
        }
    }

    if !categories.is_empty() {
        col = col.child(sidebar_section_label("Categories"));
        for (i, category) in categories.iter().enumerate() {
            let active_category =
                matches!(active, PickerFilter::Category(c) if c.eq_ignore_ascii_case(category));
            col = col.child(sidebar_item(
                ("pp-filter-cat", i),
                category.clone(),
                None,
                active_category,
                filter_cb.clone(),
                PickerFilter::Category(category.clone()),
            ));
        }
    }

    let thumb_scroll = sidebar_scroll.clone();

    div()
        .flex()
        .flex_col()
        .w(px(SIDEBAR_WIDTH))
        .min_w(px(SIDEBAR_WIDTH))
        .h_full()
        .flex_shrink_0()
        .border_r(px(1.0))
        .border_color(Colors::divider())
        .bg(Colors::surface_panel_alt())
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .relative()
                .child(
                    div()
                        .id("plugin-picker-sidebar-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(sidebar_scroll)
                        .child(col),
                )
                .child(vertical_scrollbar_thumb(thumb_scroll)),
        )
}

fn sidebar_item(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    count: Option<usize>,
    active: bool,
    cb: FilterCb,
    value: PickerFilter,
) -> impl IntoElement {
    let label = label.into();
    div().px(px(4.0)).child(
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .px(px(8.0))
            .py(px(4.0))
            .rounded_md()
            .when(active, |el| el.bg(Colors::accent_muted()))
            .when(!active, |el| {
                el.hover(|s| s.bg(Colors::surface_control_hover()))
            })
            .cursor(gpui::CursorStyle::PointingHand)
            .on_click(move |_, window, cx| cb(&value, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(11.0))
                    .text_color(if active {
                        Colors::accent_primary()
                    } else {
                        Colors::text_dim()
                    })
                    .truncate()
                    .child(label),
            )
            .when_some(count, |el, n| {
                el.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(if active {
                            Colors::accent_primary()
                        } else {
                            Colors::text_faint()
                        })
                        .child(format!("{n}")),
                )
            }),
    )
}

fn sidebar_section_label(label: &'static str) -> impl IntoElement {
    div()
        .px(px(10.0))
        .pt(px(8.0))
        .pb(px(2.0))
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_faint())
        .child(label)
}
