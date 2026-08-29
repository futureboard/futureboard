use crate::theme::Colors;
use gpui::{div, px, svg, Div, InteractiveElement, ParentElement, Pixels, Rgba, Styled};

/// A reusable icon button component.
/// If `icon_path` is provided, it attempts to render the SVG asset.
/// Otherwise, it falls back to rendering the `fallback_text`.
pub fn icon_button(
    icon_path: Option<&'static str>,
    fallback_text: &'static str,
    width: Pixels,
    height: Pixels,
    icon_size: Pixels,
    color: Rgba,
) -> Div {
    let mut button = div()
        .w(width)
        .h(height)
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(crate::theme::radius::CONTROL))
        .text_color(color)
        .hover(|style| style.bg(Colors::surface_hover()));

    if let Some(path) = icon_path {
        button = button.child(svg().path(path).w(icon_size).h(icon_size).text_color(color));
    } else {
        button = button.child(fallback_text);
    }

    button
}
