//! Composite native UI font system (Phase 1).
//!
//! One composite system-UI font descriptor for all normal UI text, resolved and
//! shaped entirely by the platform's native text system. No language detection,
//! no per-character font selection, no custom shaping — the manager only decides
//! the anchor family and attaches a coverage-ordered `FontFallbacks` chain.
//!
//! Entry points:
//! - [`FontManager::global`] — process-wide instance.
//! - [`ui_font`] / [`display_font`] — cached `gpui::Font` descriptors.

mod chains;
mod diagnostics;
mod manager;

pub use diagnostics::{open_font_diagnostics_window, FontDiagnostics, MIXED_SCRIPT_SAMPLE};
pub use manager::{CompositeRole, FontManager};

use gpui::{Font, FontWeight};

/// Composite UI font at the given weight. Preferred entry point for UI text.
pub fn ui_font(weight: FontWeight) -> Font {
    FontManager::global().ui_font(weight)
}

/// Composite display font (large text) at the given weight.
pub fn display_font(weight: FontWeight) -> Font {
    FontManager::global().display_font(weight)
}
