//! Visual diagnostic screen for the composite UI font.
//!
//! Renders a fixed multi-script sample across weights, sizes, and a few widget
//! shapes so multilingual rendering can be eyeballed for tofu, clipped Thai
//! marks, broken Arabic joining, split emoji ZWJ sequences, wrong CJK fallback,
//! baseline drift, and accidental icon-font substitution.
//!
//! This is a developer surface. Open it with [`open_font_diagnostics_window`].
//! It exercises the exact same [`crate::fonts::ui_font`] path as real UI text —
//! nothing here shapes or splits text by hand.

use gpui::{
    div, px, App, AppContext, Bounds, Context, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, WindowBounds,
    WindowHandle, WindowKind,
};

use crate::fonts::{self, FontManager};
use crate::theme::Colors;
use crate::window_position::centered_window_bounds;

/// Canonical multilingual sample. Every glyph here must render from the single
/// composite descriptor via native fallback — Latin, Thai (with combining
/// marks), CJK (SC/TC/JP), Hangul, Arabic (RTL + joining), Devanagari, and a
/// color emoji.
pub const MIXED_SCRIPT_SAMPLE: &[&str] = &[
    "Futureboard Studio — สวัสดี 世界 你好 안녕하세요 مرحبا हिन्दी 😀",
    "กำลังบันทึกเสียง Track 01 — 48 kHz / 24-bit",
    "ภาษาไทย English 日本語 简体中文 繁體中文 한국어",
];

/// Weights exercised by the matrix.
const WEIGHTS: &[(FontWeight, &str)] = &[
    (FontWeight::NORMAL, "Regular 400"),
    (FontWeight::MEDIUM, "Medium 500"),
    (FontWeight::SEMIBOLD, "Semibold 600"),
    (FontWeight::BOLD, "Bold 700"),
];

/// Logical pixel sizes exercised by the matrix (covers DAW chrome range).
const SIZES: &[f32] = &[10.0, 11.0, 12.0, 13.0, 16.0, 24.0];

pub const FONT_DIAGNOSTICS_WIDTH: f32 = 820.0;
pub const FONT_DIAGNOSTICS_HEIGHT: f32 = 720.0;

pub struct FontDiagnostics;

impl FontDiagnostics {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        FontDiagnostics
    }
}

impl Render for FontDiagnostics {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let manager = FontManager::global();

        div()
            .id("font-diagnostics")
            .size_full()
            .overflow_y_scroll()
            .bg(Colors::surface_window())
            .text_color(Colors::text_primary())
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(header(manager))
            .child(section_title("Realistic UI block — 13px / Regular"))
            .child(sample_block(FontWeight::NORMAL, 13.0))
            .child(section_title("Weight × size matrix (sample line 1)"))
            .child(weight_size_matrix())
            .child(section_title("Widget shapes"))
            .child(widget_samples())
            .child(section_title("Truncation (single line, ellipsis)"))
            .child(truncated_row())
            .child(section_title("Multiline wrap"))
            .child(multiline_block())
    }
}

fn header(manager: &FontManager) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .pb(px(12.0))
        .border_b_1()
        .border_color(Colors::border_normal())
        .child(
            div()
                .font(fonts::display_font(FontWeight::SEMIBOLD))
                .text_size(px(20.0))
                .child("Font Diagnostics"),
        )
        .child(
            div()
                .font(fonts::ui_font(FontWeight::NORMAL))
                .text_size(px(12.0))
                .text_color(Colors::text_muted())
                .child(SharedString::from(format!(
                    "platform={}  anchor={}",
                    std::env::consts::OS,
                    manager.ui_anchor()
                ))),
        )
}

fn section_title(label: &'static str) -> impl IntoElement {
    div()
        .font(fonts::ui_font(FontWeight::SEMIBOLD))
        .text_size(px(12.0))
        .text_color(Colors::text_secondary())
        .child(label)
}

/// All three sample lines at one weight/size — the realistic mixed-run case.
fn sample_block(weight: FontWeight, size: f32) -> impl IntoElement {
    let mut col = div().flex().flex_col().gap(px(2.0));
    for line in MIXED_SCRIPT_SAMPLE {
        col = col.child(
            div()
                .font(fonts::ui_font(weight))
                .text_size(px(size))
                .child(SharedString::from(*line)),
        );
    }
    col
}

fn weight_size_matrix() -> impl IntoElement {
    let mut col = div().flex().flex_col().gap(px(10.0));
    for &size in SIZES {
        let mut group = div().flex().flex_col().gap(px(2.0)).child(
            div()
                .font(fonts::ui_font(FontWeight::MEDIUM))
                .text_size(px(10.0))
                .text_color(Colors::text_muted())
                .child(SharedString::from(format!("{size:.0}px"))),
        );
        for (weight, _label) in WEIGHTS {
            group = group.child(
                div()
                    .font(fonts::ui_font(*weight))
                    .text_size(px(size))
                    .child(SharedString::from(MIXED_SCRIPT_SAMPLE[0])),
            );
        }
        col = col.child(group);
    }
    col
}

fn widget_samples() -> impl IntoElement {
    let label = "สวัสดี 世界 😀 Track";
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(10.0))
        .items_center()
        // Button-like
        .child(
            div()
                .px(px(12.0))
                .py(px(6.0))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::accent_primary())
                .text_color(Colors::on_accent())
                .font(fonts::ui_font(FontWeight::MEDIUM))
                .text_size(px(12.0))
                .child(SharedString::from(label)),
        )
        // Input-like
        .child(
            div()
                .px(px(10.0))
                .py(px(6.0))
                .min_w(px(180.0))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::surface_input())
                .border_1()
                .border_color(Colors::border_normal())
                .font(fonts::ui_font(FontWeight::NORMAL))
                .text_size(px(12.0))
                .child(SharedString::from(label)),
        )
        // Tab-like
        .child(
            div()
                .px(px(12.0))
                .py(px(6.0))
                .border_b_2()
                .border_color(Colors::accent_primary())
                .font(fonts::ui_font(FontWeight::SEMIBOLD))
                .text_size(px(12.0))
                .child(SharedString::from(label)),
        )
}

fn truncated_row() -> impl IntoElement {
    div()
        .max_w(px(240.0))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .font(fonts::ui_font(FontWeight::NORMAL))
        .text_size(px(13.0))
        .child(SharedString::from(MIXED_SCRIPT_SAMPLE[0]))
}

fn multiline_block() -> impl IntoElement {
    let joined = MIXED_SCRIPT_SAMPLE.join("  ·  ");
    div()
        .max_w(px(360.0))
        .font(fonts::ui_font(FontWeight::NORMAL))
        .text_size(px(13.0))
        .child(SharedString::from(joined))
}

/// Open the font diagnostics window.
pub fn open_font_diagnostics_window(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    cx: &mut App,
) -> Result<WindowHandle<FontDiagnostics>, String> {
    let window_bounds = centered_window_bounds(
        owner_bounds,
        gpui::size(px(FONT_DIAGNOSTICS_WIDTH), px(FONT_DIAGNOSTICS_HEIGHT)),
        cx,
    );
    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    options.kind = WindowKind::Normal;

    cx.open_window(options, move |_window, cx| cx.new(FontDiagnostics::new))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_covers_the_required_scripts() {
        let all: String = MIXED_SCRIPT_SAMPLE.concat();
        // Thai, CJK, Hangul, Arabic, Devanagari, emoji all present in one set.
        assert!(
            all.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c)),
            "Thai"
        );
        assert!(
            all.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
            "CJK"
        );
        assert!(
            all.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)),
            "Hangul"
        );
        assert!(
            all.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)),
            "Arabic"
        );
        assert!(
            all.chars().any(|c| ('\u{0900}'..='\u{097F}').contains(&c)),
            "Devanagari"
        );
        assert!(all.chars().any(|c| c == '😀'), "emoji");
    }
}
