//! Visible vertical scrollbar thumb overlay for scrollable GPUI panels.

use gpui::{div, px, IntoElement, ScrollHandle, Styled};

use crate::theme::Colors;

/// Renders a compact vertical scrollbar thumb aligned to the right edge.
pub fn vertical_scrollbar_thumb(scroll: ScrollHandle) -> gpui::AnyElement {
    let viewport_h: f32 = scroll.bounds().size.height.into();
    let max_y: f32 = scroll.max_offset().y.into();
    let raw_y: f32 = scroll.offset().y.into();
    let offset_y: f32 = -raw_y;

    if viewport_h <= 0.0 || max_y <= 0.5 {
        return div().w(px(0.0)).h(px(0.0)).into_any_element();
    }

    let content_h = viewport_h + max_y;
    let min_thumb = 24.0_f32;
    let thumb_h = ((viewport_h / content_h) * viewport_h).max(min_thumb);
    let track_room = (viewport_h - thumb_h).max(0.0);
    let progress = (offset_y / max_y).clamp(0.0, 1.0);
    let thumb_top = progress * track_room;

    div()
        .absolute()
        .top(px(thumb_top))
        .right(px(2.0))
        .w(px(4.0))
        .h(px(thumb_h))
        .rounded(px(crate::theme::radius::PILL))
        .bg(Colors::with_alpha(Colors::text_primary(), 0.22))
        .into_any_element()
}
