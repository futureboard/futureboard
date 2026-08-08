//! Centralized Preferences UI building blocks.
//!
//! Wraps [`settings_layout`] and [`box_list_view`] so Audio / MIDI / Performance /
//! Appearance pages share one DAW-native control vocabulary.

use std::sync::Arc;

use gpui::{div, px, App, Div, IntoElement, ParentElement, Styled, Window};

use crate::components::box_list_view::box_list_toggle;
use crate::components::combo_box::combo_box_trigger;
use crate::components::settings_layout::{
    settings_daw_row, settings_daw_row_with_description, settings_field_label,
    settings_section_card, settings_section_hint, settings_section_title, settings_status_badge,
    settings_value_readout, SETTINGS_LABEL_WIDTH, SETTINGS_ROW_GAP,
};
use crate::overlay::{form_combo_trigger_bounds, OverlayAnchor, COMBO_TRIGGER_HEIGHT};
use crate::theme::{typography, Colors};

pub use crate::components::box_list_view::{
    box_list_empty_state, box_list_group_label, box_list_icon_button, box_list_item,
    box_list_item_badge, box_list_item_content, box_list_item_leading_icon, box_list_item_subtitle,
    box_list_item_title, box_list_item_trailing, box_list_view, BoxListBadgeTone,
};

pub const RESTART_FOOTER_TEXT: &str = "* Restart Futureboard Studio to apply this change.";

/// Bordered settings group — alias for [`settings_section_card`].
pub fn settings_section(title: impl Into<String>) -> Div {
    settings_section_card().child(settings_section_title(title))
}

pub fn settings_section_hint_text(text: impl Into<String>) -> impl IntoElement {
    settings_section_hint(text)
}

/// Append ` *` when a setting requires restart to take effect.
pub fn settings_restart_label(label: impl Into<String>, restart_required: bool) -> String {
    let label = label.into();
    if restart_required {
        format!("{label} *")
    } else {
        label
    }
}

/// Shared footer for sections containing restart-required settings.
pub fn settings_restart_footer() -> impl IntoElement {
    div()
        .pt(px(8.0))
        .text_size(px(typography::UI_XS))
        .text_color(Colors::text_faint())
        .child(RESTART_FOOTER_TEXT)
}

/// Standard label + control row.
pub fn settings_row(label: impl Into<String>, control: impl IntoElement) -> impl IntoElement {
    settings_daw_row(label, control)
}

/// Row with optional description under the label column.
pub fn settings_row_with_description(
    label: impl Into<String>,
    description: Option<String>,
    control: impl IntoElement,
) -> impl IntoElement {
    settings_daw_row_with_description(label, description, control)
}

/// Row with restart-required marker on the label.
pub fn settings_row_restart(
    label: impl Into<String>,
    restart_required: bool,
    control: impl IntoElement,
) -> impl IntoElement {
    settings_daw_row(settings_restart_label(label, restart_required), control)
}

/// Read-only status/value cell.
pub fn settings_readout(text: impl Into<String>) -> impl IntoElement {
    settings_value_readout(text)
}

/// Compact success/warning badge.
pub fn settings_status(label: impl Into<String>, ok: bool) -> impl IntoElement {
    settings_status_badge(label, ok)
}

/// ON/OFF switch for boolean settings rows.
pub fn settings_toggle(
    id: impl Into<gpui::ElementId>,
    enabled: bool,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    box_list_toggle(id, enabled, on_toggle)
}

/// ComboBox trigger wired for Preferences form-column layout.
pub fn settings_combo_trigger(
    trigger_id: &'static str,
    selected: &str,
    open: bool,
    on_toggle: Arc<dyn Fn(Option<OverlayAnchor>, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let selected = selected.to_string();
    div().w_full().child(combo_box_trigger(
        trigger_id,
        selected,
        open,
        move |event, window, cx| {
            let layout = crate::overlay::settings_form_column(window);
            let bounds = form_combo_trigger_bounds(layout, event, COMBO_TRIGGER_HEIGHT);
            let anchor = if open {
                None
            } else {
                Some(OverlayAnchor { bounds })
            };
            on_toggle(anchor, window, cx);
        },
    ))
}

/// Device / path list wrapper — re-exported BoxListView group.
pub fn settings_box_list() -> Div {
    box_list_view()
}

pub fn settings_box_list_group(title: impl Into<String>) -> impl IntoElement {
    box_list_group_label(title)
}

/// Fixed-width label column (for custom rows).
pub fn settings_label(label: impl Into<String>) -> impl IntoElement {
    settings_field_label(label)
}

/// Control column shell.
pub fn settings_control_slot() -> Div {
    div().flex_1().min_w_0()
}

/// Two-column row shell when not using [`settings_row`].
pub fn settings_row_shell() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SETTINGS_ROW_GAP))
        .min_h(px(30.0))
}

pub fn settings_label_width() -> f32 {
    SETTINGS_LABEL_WIDTH
}
