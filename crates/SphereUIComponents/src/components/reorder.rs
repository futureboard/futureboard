//! Shared drag-reorder affordances.
//!
//! Two theme-tokened primitives used by any list that supports drag reorder
//! (FX/insert chains today; reusable for future sortable lists), so the reorder
//! UX stays consistent and no surface hand-rolls its own drag chrome:
//!
//! * [`drag_handle`] — a compact grip the user presses to start a drag. The
//!   caller attaches the GPUI `.id(..).on_drag(payload, ..)` (the drag payload
//!   is list-specific), so only the handle initiates a reorder; the rest of a
//!   row's controls (buttons, context menu) keep their own hit-testing.
//! * [`drop_over_highlight`] — the drop-position indicator: a 1px accent line
//!   drawn on the top edge of whichever row a compatible drag is hovering,
//!   applied through GPUI's `.drag_over::<T>(..)` style hook. No transient
//!   state required.
//!
//! GPUI's drag machinery applies its own small click-vs-drag movement threshold
//! internally, so a press that does not move still registers as a click on the
//! handle.

use gpui::{div, px, Div, ParentElement, StyleRefinement, Styled};

use crate::theme::Colors;

/// Compact vertical grip (two columns × three dots) used as a drag handle.
/// Subtle by default and sized for DAW density. Returns a plain [`Div`] so the
/// caller can chain `.id(..)` + `.on_drag(..)` (+ any hover treatment) on it.
/// Vector dots — no asset, no icon font, no emoji (respects the icon rules).
pub fn drag_handle() -> Div {
    let dot = || {
        div()
            .w(px(2.0))
            .h(px(2.0))
            .rounded(px(crate::theme::radius::PILL))
            .bg(Colors::text_faint())
    };
    let column = || {
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(dot())
            .child(dot())
            .child(dot())
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .w(px(12.0))
        .h(px(16.0))
        .gap(px(2.0))
        .child(column())
        .child(column())
}

/// Drop-position indicator styling for `.drag_over::<T>(drop_over_highlight)`:
/// a 1px accent line on the row's top edge marking "drop above this slot".
/// Shared so every reorderable list shows the same indicator.
pub fn drop_over_highlight(style: StyleRefinement) -> StyleRefinement {
    style
        .border_t(px(1.0))
        .border_color(Colors::accent_primary())
}
