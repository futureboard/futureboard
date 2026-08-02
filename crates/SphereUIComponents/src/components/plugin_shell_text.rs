//! Thin adapter: native plugin-editor-shell text rendering delegates to
//! [`sphere_graphic_engine`]'s DirectWrite backend.
//!
//! This module owns only shell font *policy* — which families/sizes the shell
//! chrome wants — plus the thread-local renderer cache and the shell-tagged
//! diagnostics log. The DirectWrite implementation (font loading, custom
//! collection, fallback, GDI-interop glyph rasterization) lives in the engine.
//! **Direct2D is never used.**

/// Shell font metrics — re-exported from the engine's generic chrome font
/// theme so existing call sites keep their type name.
pub use sphere_graphic_engine::ChromeFontTheme as PluginShellFontTheme;
/// Horizontal alignment, re-exported from the engine.
pub use sphere_graphic_engine::TextAlign;

/// Centralized shell font theme — sourced from [`crate::theme`], not magic
/// strings scattered through the chrome.
pub fn shell_font_theme() -> PluginShellFontTheme {
    PluginShellFontTheme {
        family_primary: crate::theme::FONT_FAMILY,
        family_fallback: crate::theme::SYSTEM_THAI_UI_FONT_FAMILY,
        title_size: crate::theme::typography::PLUGIN_TITLE,
        body_size: crate::theme::typography::UI_SM,
        weight_title: 500,
        weight_body: 400,
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::cell::RefCell;
    use std::sync::Once;

    use super::shell_font_theme;
    use sphere_graphic_engine::{DWriteTextRenderer, FontConfig, TextAlign};
    use windows::Win32::Foundation::{COLORREF, RECT};
    use windows::Win32::Graphics::Gdi::HDC;

    thread_local! {
        // Outer Option: not yet initialized. Inner Option: initialization ran
        // but DirectWrite was unavailable (stay on the GDI fallback path).
        static RENDERER: RefCell<Option<Option<DWriteTextRenderer>>> = const { RefCell::new(None) };
    }

    static INIT_LOG: Once = Once::new();

    /// Lazily build the engine renderer from the system-font policy, logging
    /// diagnostics once under the shell's tag.
    fn with_renderer<R>(f: impl FnOnce(Option<&DWriteTextRenderer>) -> R) -> R {
        RENDERER.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                let theme = shell_font_theme();
                let config = FontConfig {
                    primary_family: theme.family_primary.to_string(),
                    fallback_family: theme.family_fallback.to_string(),
                    default_weight: theme.weight_title,
                };
                let renderer = DWriteTextRenderer::new(&[], config);
                if let Some(renderer) = renderer.as_ref() {
                    log_diagnostics(renderer);
                }
                *slot = Some(renderer);
            }
            f(slot.as_ref().and_then(|inner| inner.as_ref()))
        })
    }

    fn log_diagnostics(renderer: &DWriteTextRenderer) {
        INIT_LOG.call_once(|| {
            let theme = shell_font_theme();
            eprintln!("[Fonts] default_ui_font={}", theme.family_primary);
            eprintln!("[Fonts] fallback_ui_font={}", theme.family_fallback);
            let d = renderer.diagnostics();
            eprintln!("[plugin-shell-font] source=system");
            eprintln!("[plugin-shell-font] primary={}", d.primary_family);
            eprintln!("[plugin-shell-font] fallback={}", d.fallback_family);
            eprintln!("[plugin-shell-font] dwrite_factory_version=5");
            eprintln!(
                "[plugin-shell-font] custom_collection_created={}",
                d.custom_collection_ok
            );
            eprintln!(
                "[plugin-shell-font] font_bytes_loaded count={}",
                d.font_bytes_loaded
            );
            eprintln!("[plugin-shell-font] family_resolved={}", d.resolved_primary);
            eprintln!("[plugin-shell-font] loaded=true");
        });
    }

    /// Render `text` into `rect` on `hdc` with the engine's DirectWrite backend.
    /// Returns `false` on any failure so the caller can fall back to GDI
    /// `DrawTextW`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        hdc: HDC,
        rect: RECT,
        text: &str,
        family: &str,
        weight: u32,
        em_px: f32,
        bg: COLORREF,
        fg: COLORREF,
        align: TextAlign,
        dpi_scale: f32,
    ) -> bool {
        draw_text_with_line_height(
            hdc,
            rect,
            text,
            family,
            weight,
            em_px,
            bg,
            fg,
            align,
            dpi_scale,
            em_px * 1.3,
        )
    }

    pub fn draw_text_with_line_height(
        hdc: HDC,
        rect: RECT,
        text: &str,
        family: &str,
        weight: u32,
        em_px: f32,
        bg: COLORREF,
        fg: COLORREF,
        align: TextAlign,
        dpi_scale: f32,
        line_height_px: f32,
    ) -> bool {
        with_renderer(|renderer| match renderer {
            Some(renderer) => renderer.draw_text_with_line_height(
                hdc,
                rect,
                text,
                family,
                weight,
                em_px,
                bg,
                fg,
                align,
                dpi_scale,
                line_height_px,
            ),
            None => false,
        })
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::TextAlign;

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        _hdc: isize,
        _rect: (),
        _text: &str,
        _family: &str,
        _weight: u32,
        _em_px: f32,
        _bg: (),
        _fg: (),
        _align: TextAlign,
        _dpi_scale: f32,
    ) -> bool {
        false
    }
}

#[cfg(target_os = "windows")]
pub use imp::{draw_text, draw_text_with_line_height};
