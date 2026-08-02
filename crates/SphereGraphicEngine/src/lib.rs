//! SphereGraphicEngine — reusable native graphics / text / window primitives.
//!
//! This crate owns the **platform graphics infrastructure** shared by
//! Futureboard's native shells:
//!
//! - DirectWrite text rendering (system fonts, optional custom collection, and
//!   fallback) rasterized through **GDI interop** — **Direct2D is never used**.
//! - DWM window chrome effects (immersive dark mode, rounded corners, themed
//!   border / caption color).
//! - A generic [`ChromeTheme`] / [`ChromeFontTheme`] for window chrome.
//! - [`SoftwarePainter`] GDI paint primitives (fills, frames, strokes).
//!
//! It is deliberately free of application / runtime concepts. It knows nothing
//! about plugins, VST, MIDI, editor sessions, or instance ids — callers own
//! that lifecycle and drive these primitives. Keeping it generic is what lets a
//! plugin editor shell, a settings window, or any future native surface reuse
//! the same text/window backend.

pub mod theme;

#[cfg(target_os = "windows")]
pub mod platform;

#[cfg(target_os = "windows")]
pub mod software;

/// Horizontal alignment for [`DWriteTextRenderer::draw_text`]. Defined at the
/// crate root so platform-neutral callers can name it without pulling in the
/// Windows backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    /// Left-aligned, vertically centered (titlebars).
    LeftMiddle,
    Center,
}

pub use theme::{ChromeFontTheme, ChromeTheme, Color};

#[cfg(target_os = "windows")]
pub use platform::windows::{
    CornerPreference, DWriteFontManager, DWriteTextRenderer, DwmApplyResult, DwmChromeOptions,
    DwmWindowEffects, FontConfig, FontDiagnostics,
};

#[cfg(target_os = "windows")]
pub use software::SoftwarePainter;
