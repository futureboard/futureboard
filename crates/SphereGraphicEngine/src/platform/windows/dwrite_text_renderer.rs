//! DirectWrite text renderer rasterized through **GDI interop** — never
//! Direct2D.
//!
//! Glyphs are drawn into an `IDWriteBitmapRenderTarget` (a GDI DIB section) via
//! a custom `IDWriteTextRenderer`, then composited onto the destination `HDC`
//! with `BitBlt`. This keeps the path swap-chain-free so foreign child HWNDs
//! (e.g. a parented plugin view) keep painting underneath the chrome.
//!
//! Owns a [`DWriteFontManager`]; both are `!Send` COM objects, so callers
//! typically hold the renderer in thread-local storage on their UI thread.

use core::ffi::c_void;

use windows::core::{implement, BOOL, HSTRING};
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteBitmapRenderTarget, IDWritePixelSnapping_Impl, IDWriteRenderingParams,
    IDWriteTextFormat1, IDWriteTextLayout, IDWriteTextRenderer, IDWriteTextRenderer_Impl,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_MATRIX, DWRITE_MEASURING_MODE,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateSolidBrush, DeleteObject, FillRect, HDC, SRCCOPY,
};
use windows_core::Interface;

use super::dwrite_font_manager::{
    resolve_family_in_collection, DWriteFontManager, FontDiagnostics,
};
use super::FontConfig;
use crate::TextAlign;

/// A DirectWrite text renderer over an owned [`DWriteFontManager`].
pub struct DWriteTextRenderer {
    manager: DWriteFontManager,
}

impl DWriteTextRenderer {
    /// Build a renderer from font blobs and a [`FontConfig`]. Returns `None` if
    /// DirectWrite initialization fails.
    pub fn new(font_blobs: &[&[u8]], config: FontConfig) -> Option<Self> {
        Some(Self {
            manager: DWriteFontManager::new(font_blobs, &config)?,
        })
    }

    /// Build a renderer over an already-constructed font manager.
    pub fn from_manager(manager: DWriteFontManager) -> Self {
        Self { manager }
    }

    /// Font diagnostics from the underlying manager.
    pub fn diagnostics(&self) -> &FontDiagnostics {
        self.manager.diagnostics()
    }

    /// Render `text` into `rect` on `hdc`. Returns `false` on any failure so the
    /// caller can fall back to GDI `DrawTextW`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &self,
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
        self.draw_text_with_line_height(
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
        &self,
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
        let w = (rect.right - rect.left).max(0);
        let h = (rect.bottom - rect.top).max(0);
        if w == 0 || h == 0 || text.is_empty() {
            return false;
        }

        let dpi_scale = normalized_dpi_scale(dpi_scale);
        let layout_w = w as f32 / dpi_scale;
        let layout_h = h as f32 / dpi_scale;
        let layout = unsafe {
            create_text_layout(
                &self.manager,
                text,
                family,
                weight,
                em_px,
                layout_w,
                layout_h,
                align,
                line_height_px,
            )
        };
        let Some(layout) = layout else {
            return false;
        };

        let mut metrics = DWRITE_TEXT_METRICS::default();
        if unsafe { layout.GetMetrics(&mut metrics) }.is_err() {
            return false;
        }

        unsafe {
            draw_layout(
                &self.manager,
                hdc,
                rect,
                w,
                h,
                &layout,
                bg,
                fg,
                align,
                dpi_scale,
            )
        }
    }
}

fn normalized_dpi_scale(dpi_scale: f32) -> f32 {
    if dpi_scale.is_finite() && dpi_scale > 0.0 {
        dpi_scale
    } else {
        1.0
    }
}

struct DrawContext {
    brt: IDWriteBitmapRenderTarget,
    params: IDWriteRenderingParams,
    fg: COLORREF,
    pixels_per_dip: f32,
}

#[implement(IDWriteTextRenderer)]
struct GlyphRunRenderer;

#[allow(non_snake_case)]
impl IDWritePixelSnapping_Impl for GlyphRunRenderer_Impl {
    fn IsPixelSnappingDisabled(
        &self,
        _clientdrawingcontext: *const c_void,
    ) -> windows::core::Result<BOOL> {
        Ok(BOOL(0))
    }

    fn GetCurrentTransform(
        &self,
        _clientdrawingcontext: *const c_void,
        transform: *mut DWRITE_MATRIX,
    ) -> windows::core::Result<()> {
        unsafe {
            *transform = DWRITE_MATRIX {
                m11: 1.0,
                m12: 0.0,
                m21: 0.0,
                m22: 1.0,
                dx: 0.0,
                dy: 0.0,
            };
        }
        Ok(())
    }

    fn GetPixelsPerDip(&self, clientdrawingcontext: *const c_void) -> windows::core::Result<f32> {
        let ctx = unsafe { &*(clientdrawingcontext.cast::<DrawContext>()) };
        Ok(ctx.pixels_per_dip)
    }
}

#[allow(non_snake_case)]
impl IDWriteTextRenderer_Impl for GlyphRunRenderer_Impl {
    fn DrawGlyphRun(
        &self,
        clientdrawingcontext: *const c_void,
        baselineoriginx: f32,
        baselineoriginy: f32,
        measuringmode: DWRITE_MEASURING_MODE,
        glyphrun: *const windows::Win32::Graphics::DirectWrite::DWRITE_GLYPH_RUN,
        _glyphrundescription: *const windows::Win32::Graphics::DirectWrite::DWRITE_GLYPH_RUN_DESCRIPTION,
        _clientdrawingeffect: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        let ctx = unsafe { &*(clientdrawingcontext.cast::<DrawContext>()) };
        let glyphrun = unsafe { &*glyphrun };
        unsafe {
            ctx.brt.DrawGlyphRun(
                baselineoriginx,
                baselineoriginy,
                measuringmode,
                glyphrun,
                &ctx.params,
                ctx.fg,
                None,
            )?;
        }
        Ok(())
    }

    fn DrawUnderline(
        &self,
        _clientdrawingcontext: *const c_void,
        _baselineoriginx: f32,
        _baselineoriginy: f32,
        _underline: *const windows::Win32::Graphics::DirectWrite::DWRITE_UNDERLINE,
        _clientdrawingeffect: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn DrawStrikethrough(
        &self,
        _clientdrawingcontext: *const c_void,
        _baselineoriginx: f32,
        _baselineoriginy: f32,
        _strikethrough: *const windows::Win32::Graphics::DirectWrite::DWRITE_STRIKETHROUGH,
        _clientdrawingeffect: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn DrawInlineObject(
        &self,
        _clientdrawingcontext: *const c_void,
        _originx: f32,
        _originy: f32,
        _inlineobject: windows::core::Ref<
            windows::Win32::Graphics::DirectWrite::IDWriteInlineObject,
        >,
        _issideways: BOOL,
        _isrighttoleft: BOOL,
        _clientdrawingeffect: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Choose the collection (custom embedded, else system) that contains `family`,
/// returning the resolved family name and whether the system fallback was used.
unsafe fn pick_collection(
    manager: &DWriteFontManager,
    family: &str,
    weight: u32,
) -> (
    windows::Win32::Graphics::DirectWrite::IDWriteFontCollection1,
    String,
    bool,
) {
    if let Some(name) =
        resolve_family_in_collection(&manager.custom_collection, family, weight, &manager.locale)
    {
        return (manager.custom_collection.clone(), name, false);
    }
    if let Some(name) =
        resolve_family_in_collection(&manager.system_collection, family, weight, &manager.locale)
    {
        return (manager.system_collection.clone(), name, true);
    }
    (
        manager.system_collection.clone(),
        manager.resolved_primary().to_string(),
        true,
    )
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_text_layout(
    manager: &DWriteFontManager,
    text: &str,
    family: &str,
    weight: u32,
    em_px: f32,
    max_w: f32,
    max_h: f32,
    align: TextAlign,
    line_height_px: f32,
) -> Option<IDWriteTextLayout> {
    let (collection, resolved_name, _) = pick_collection(manager, family, weight);
    let family_h = HSTRING::from(resolved_name.as_str());
    let format: IDWriteTextFormat1 = manager
        .factory
        .CreateTextFormat(
            &family_h,
            &collection,
            DWRITE_FONT_WEIGHT(weight as i32),
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            em_px,
            &manager.locale,
        )
        .ok()?
        .cast()
        .ok()?;
    let _ = format.SetFontFallback(Some(&manager.font_fallback));
    let (text_align, para_align) = match align {
        TextAlign::Left => (
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
        ),
        TextAlign::LeftMiddle => (
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        ),
        TextAlign::Center => (
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        ),
    };
    let _ = format.SetTextAlignment(text_align);
    let _ = format.SetParagraphAlignment(para_align);
    let line_height = line_height_px.max(em_px);
    let _ = format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line_height, em_px);

    let wide: Vec<u16> = text.encode_utf16().collect();
    manager
        .factory
        .CreateTextLayout(&wide, &format, max_w, max_h)
        .ok()
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_layout(
    manager: &DWriteFontManager,
    dst: HDC,
    rect: RECT,
    w: i32,
    h: i32,
    layout: &IDWriteTextLayout,
    bg: COLORREF,
    fg: COLORREF,
    align: TextAlign,
    dpi_scale: f32,
) -> bool {
    let dpi_scale = normalized_dpi_scale(dpi_scale);
    let Ok(brt) = manager
        .gdi
        .CreateBitmapRenderTarget(None, w as u32, h as u32)
    else {
        return false;
    };
    let _ = brt.SetPixelsPerDip(dpi_scale);
    let mem = brt.GetMemoryDC();
    let brush = CreateSolidBrush(bg);
    let full = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    FillRect(mem, &full, brush);
    let _ = DeleteObject(brush.into());

    let mut metrics = DWRITE_TEXT_METRICS::default();
    if layout.GetMetrics(&mut metrics).is_err() {
        return false;
    }
    let origin_x = match metrics.left {
        x if x.is_finite() => x.max(0.0),
        _ => 0.0,
    };
    // Paragraph CENTER alignment already vertically centers within the layout
    // box — do not add a second offset or glyphs render outside the bitmap.
    let layout_h = h as f32 / dpi_scale;
    let origin_y = match align {
        TextAlign::LeftMiddle | TextAlign::Center => 0.0,
        TextAlign::Left => ((layout_h - metrics.height) / 2.0).max(0.0),
    };

    let ctx = DrawContext {
        brt,
        params: manager.rendering_params.clone(),
        fg,
        pixels_per_dip: dpi_scale,
    };
    let renderer: IDWriteTextRenderer = GlyphRunRenderer.into();
    if layout
        .Draw(
            Some((&raw const ctx).cast::<c_void>()),
            &renderer,
            origin_x,
            origin_y,
        )
        .is_err()
    {
        return false;
    }

    BitBlt(dst, rect.left, rect.top, w, h, Some(mem), 0, 0, SRCCOPY).is_ok()
}
