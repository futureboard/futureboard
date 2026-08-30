//! GDI text system — the fallback path when DirectWrite is unavailable or
//! misbehaving.
//!
//! # Why this exists
//!
//! [`DirectWriteTextSystem`](crate::DirectWriteTextSystem) is the right default:
//! it shapes with HarfBuzz-class quality, rasterizes with ClearType, and knows
//! about colour fonts. But it is also the single largest source of "the app
//! opens to a blank window" reports on Windows — a broken font cache service, a
//! third-party font manager hooking `DWrite.dll`, a locked-down enterprise
//! image, or a GPU driver that loses the D3D device during startup will all take
//! DirectWrite down and, with it, every glyph in the app.
//!
//! GDI has none of that surface. It is older, it renders through the same
//! `gdi32.dll` that has shipped since NT, and it needs no device. This backend
//! trades quality for the ability to draw *something* legible.
//!
//! # What it gives up
//!
//! - **Grayscale antialiasing only.** GDI's ClearType path renders into a DC,
//!   not into an alpha texture we can hand the atlas, so this reports
//!   [`TextRenderingMode::Grayscale`] and never claims subpixel coverage.
//! - **Whole-pixel glyph positions.** `GetGlyphOutlineW` has no subpixel phase,
//!   so every `subpixel_variant` rasterizes identically. Text is a shade less
//!   even at small sizes, and horizontal scrolling shimmers slightly.
//! - **No colour emoji.** Emoji come out as monochrome outlines.
//! - **Uniscribe shaping, not DirectWrite shaping.** Complex scripts (Thai,
//!   Arabic, Devanagari) still shape and position correctly — Uniscribe is the
//!   engine Windows used for a decade — but ligature and kerning coverage in
//!   modern OpenType fonts is not identical.
//! - **No automatic font fallback across runs.** A run is shaped in the font it
//!   was assigned; a codepoint the font lacks renders as `.notdef` rather than
//!   being borrowed from another family. The caller's own font-run splitting is
//!   what keeps mixed-script text working.
//!
//! # Threading
//!
//! Everything funnels through one memory DC, and `SelectObject` mutates it, so
//! the state is behind a `Mutex` rather than the `RwLock` the DirectWrite
//! backend can afford. Shaped lines are cached a layer above this in gpui, so
//! the contention that matters is glyph rasterization during atlas warm-up.

use std::ffi::c_void;

use anyhow::{Context as _, Result};
use collections::HashMap;
use parking_lot::Mutex;
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::*;
use windows::Win32::Graphics::Gdi::*;
use windows::core::PCWSTR;

use gpui::*;

/// Probe size used only to read `otmEMSquare` out of a font. Any size works;
/// this one is large enough that the rounded metrics it returns are still
/// usable if the real design-unit pass somehow fails.
const EM_PROBE_PX: i32 = 256;

/// Ceiling on the design-unit HFONT height. Fonts report an em square of 1000
/// (PostScript) or 2048 (TrueType); anything wildly larger is a corrupt face
/// and would ask GDI for an absurd bitmap.
const MAX_EM_SQUARE: u32 = 16_384;

/// Largest glyph bitmap this backend will ask GDI for, in pixels per side.
/// `GetGlyphOutlineW` allocates the whole bitmap up front, so a bad font size
/// must not be able to request hundreds of megabytes.
const MAX_GLYPH_PX: i32 = 1024;

/// `GGO_GRAY8_BITMAP` returns coverage in 65 levels (0..=64), not 0..=255.
const GRAY8_LEVELS: u32 = 64;

/// `GetGlyphOutlineW` and friends report failure as `GDI_ERROR`, which the
/// crate declares as `i32` while the functions return `u32`.
const GDI_CALL_FAILED: u32 = u32::MAX;

/// Ask for a bitmap of the glyph *by glyph index*, not by character.
fn gray8_by_index() -> GET_GLYPH_OUTLINE_FORMAT {
    GET_GLYPH_OUTLINE_FORMAT(GGO_GRAY8_BITMAP.0 | GGO_GLYPH_INDEX.0)
}

fn metrics_by_index() -> GET_GLYPH_OUTLINE_FORMAT {
    GET_GLYPH_OUTLINE_FORMAT(GGO_METRICS.0 | GGO_GLYPH_INDEX.0)
}

/// GDI handle wrappers. GDI objects are process-wide and safe to hand between
/// threads as long as only one thread has a given DC selected at a time, which
/// the `Mutex` around the state guarantees. The raw `windows` newtypes are not
/// `Send`, so the marker goes here rather than on the whole state.
struct OwnedDc(HDC);

unsafe impl Send for OwnedDc {}

impl Drop for OwnedDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct OwnedFont(HFONT);

unsafe impl Send for OwnedFont {}

impl Drop for OwnedFont {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

/// A font added from memory by [`GdiTextSystem::add_fonts`]. Held so the
/// resource is released with the text system rather than leaking for the life
/// of the process.
struct MemoryFont(HANDLE);

unsafe impl Send for MemoryFont {}

impl Drop for MemoryFont {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveFontMemResourceEx(self.0);
        }
    }
}

/// Uniscribe's per-font shaping cache. Opaque to us; it must be freed with
/// `ScriptFreeCache` and must not be shared between fonts.
struct ScriptCache(*mut c_void);

unsafe impl Send for ScriptCache {}

impl Drop for ScriptCache {
    fn drop(&mut self) {
        unsafe {
            let _ = ScriptFreeCache(&raw mut self.0);
        }
    }
}

struct GdiFont {
    /// The descriptor this entry was selected for, kept so a second request for
    /// the same font resolves to the same `FontId`.
    descriptor: Font,
    /// HFONT sized so one GDI logical unit equals one font design unit. Every
    /// metric this backend reports in design units is measured through it.
    design_font: OwnedFont,
    units_per_em: u32,
    metrics: FontMetrics,
    /// Rasterizing and shaping both need a pixel-sized face; keyed by the
    /// rounded device-pixel em size.
    sized_fonts: HashMap<u32, OwnedFont>,
    /// One Uniscribe cache per pixel size, for the same reason.
    script_caches: HashMap<u32, ScriptCache>,
}

struct GdiTextState {
    dc: OwnedDc,
    fonts: Vec<GdiFont>,
    font_to_font_id: HashMap<Font, FontId>,
    family_names: Vec<String>,
    memory_fonts: Vec<MemoryFont>,
}

pub(crate) struct GdiTextSystem {
    state: Mutex<GdiTextState>,
}

impl GdiTextSystem {
    pub(crate) fn new() -> Result<Self> {
        let dc = unsafe { CreateCompatibleDC(None) };
        anyhow::ensure!(!dc.is_invalid(), "CreateCompatibleDC failed for GDI text");
        unsafe {
            // Glyph metrics must not be rounded to a text-alignment grid, and
            // the mapping mode has to stay 1:1 with device pixels or every
            // `GetGlyphOutlineW` result is silently scaled.
            SetMapMode(dc, MM_TEXT);
            SetGraphicsMode(dc, GM_ADVANCED);
        }
        let dc = OwnedDc(dc);
        let family_names = enumerate_font_families(dc.0);

        Ok(Self {
            state: Mutex::new(GdiTextState {
                dc,
                fonts: Vec::new(),
                font_to_font_id: HashMap::default(),
                family_names,
                memory_fonts: Vec::new(),
            }),
        })
    }
}

impl PlatformTextSystem for GdiTextSystem {
    fn add_fonts(&self, fonts: Vec<std::borrow::Cow<'static, [u8]>>) -> Result<()> {
        let mut state = self.state.lock();
        for bytes in fonts {
            // GDI reports neither the family name nor a handle-to-name mapping
            // for a memory font, so the names are recovered by diffing the
            // process font list around the install. Enumeration is only done on
            // font load, not per frame.
            let before = enumerate_font_families(state.dc.0);
            let installed = 0u32;
            let handle = unsafe {
                AddFontMemResourceEx(
                    bytes.as_ptr() as *const c_void,
                    bytes.len() as u32,
                    None,
                    &raw const installed,
                )
            };
            if handle.is_invalid() || installed == 0 {
                log::warn!(
                    "GDI text: AddFontMemResourceEx rejected a {} byte font",
                    bytes.len()
                );
                continue;
            }
            state.memory_fonts.push(MemoryFont(handle));

            let after = enumerate_font_families(state.dc.0);
            for name in after {
                if !before.iter().any(|existing| existing == &name)
                    && !state.family_names.iter().any(|existing| existing == &name)
                {
                    state.family_names.push(name);
                }
            }
        }
        state.family_names.sort();
        state.family_names.dedup();
        Ok(())
    }

    fn all_font_names(&self) -> Vec<String> {
        self.state.lock().family_names.clone()
    }

    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        let mut state = self.state.lock();
        if let Some(font_id) = state.font_to_font_id.get(descriptor) {
            return Ok(*font_id);
        }
        state.select_and_cache_font(descriptor)
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.state
            .lock()
            .fonts
            .get(font_id.0)
            .map(|font| font.metrics)
            .unwrap_or_else(fallback_font_metrics)
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        let mut state = self.state.lock();
        let metrics = state.design_glyph_metrics(font_id, glyph_id)?;
        // GDI reports the glyph origin with y growing *up* from the baseline;
        // gpui's bounds grow down, matching the DirectWrite backend.
        Ok(Bounds {
            origin: point(
                metrics.gmptGlyphOrigin.x as f32,
                (metrics.gmptGlyphOrigin.y - metrics.gmBlackBoxY as i32) as f32,
            ),
            size: size(metrics.gmBlackBoxX as f32, metrics.gmBlackBoxY as f32),
        })
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        let mut state = self.state.lock();
        let metrics = state.design_glyph_metrics(font_id, glyph_id)?;
        Ok(size(metrics.gmCellIncX as f32, 0.0))
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        let mut state = self.state.lock();
        state.glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let mut state = self.state.lock();
        let metrics = state.raster_glyph_metrics(params)?;
        if metrics.gmBlackBoxX == 0 || metrics.gmBlackBoxY == 0 {
            return Ok(Bounds {
                origin: point(0.into(), 0.into()),
                size: size(0.into(), 0.into()),
            });
        }
        Ok(Bounds {
            origin: point(
                DevicePixels(metrics.gmptGlyphOrigin.x),
                DevicePixels(-metrics.gmptGlyphOrigin.y),
            ),
            size: size(
                DevicePixels(metrics.gmBlackBoxX as i32),
                DevicePixels(metrics.gmBlackBoxY as i32),
            ),
        })
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        anyhow::ensure!(
            raster_bounds.size.width.0 > 0 && raster_bounds.size.height.0 > 0,
            "glyph bounds are empty"
        );
        let mut state = self.state.lock();
        state.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let mut state = self.state.lock();
        state.layout_line(text, font_size, runs)
    }

    fn recommended_rendering_mode(
        &self,
        _font_id: FontId,
        _font_size: Pixels,
    ) -> TextRenderingMode {
        // GDI's ClearType output only exists inside a DC. Claiming subpixel here
        // would make the renderer read three channels out of a one-channel mask.
        TextRenderingMode::Grayscale
    }
}

impl GdiTextState {
    fn select_and_cache_font(&mut self, descriptor: &Font) -> Result<FontId> {
        let design_font = create_design_font(self.dc.0, descriptor)?;
        let (units_per_em, metrics) =
            read_font_metrics(self.dc.0, design_font.0).context("reading GDI outline metrics")?;

        let font_id = FontId(self.fonts.len());
        self.fonts.push(GdiFont {
            descriptor: descriptor.clone(),
            design_font,
            units_per_em,
            metrics,
            sized_fonts: HashMap::default(),
            script_caches: HashMap::default(),
        });
        self.font_to_font_id.insert(descriptor.clone(), font_id);
        Ok(font_id)
    }

    /// Select the design-unit face and return glyph metrics in font units.
    fn design_glyph_metrics(&mut self, font_id: FontId, glyph_id: GlyphId) -> Result<GLYPHMETRICS> {
        let font = self
            .fonts
            .get(font_id.0)
            .with_context(|| format!("unknown GDI font id {}", font_id.0))?;
        let hfont = font.design_font.0;
        glyph_metrics(self.dc.0, hfont, glyph_id.0 as u32)
    }

    /// Device-pixel em size for a render request, clamped so a runaway scale
    /// factor cannot ask GDI for an enormous bitmap.
    fn raster_em_px(params: &RenderGlyphParams) -> u32 {
        let px = f32::from(params.font_size) * params.scale_factor;
        (px.round().max(1.0) as u32).min(MAX_GLYPH_PX as u32)
    }

    fn raster_glyph_metrics(&mut self, params: &RenderGlyphParams) -> Result<GLYPHMETRICS> {
        let em_px = Self::raster_em_px(params);
        let hfont = self.sized_font(params.font_id, em_px)?;
        glyph_metrics(self.dc.0, hfont, params.glyph_id.0)
    }

    /// Get (creating if needed) the HFONT for `font_id` at `em_px` device pixels.
    fn sized_font(&mut self, font_id: FontId, em_px: u32) -> Result<HFONT> {
        let dc = self.dc.0;
        let font = self
            .fonts
            .get_mut(font_id.0)
            .with_context(|| format!("unknown GDI font id {}", font_id.0))?;
        let _ = dc;
        if let Some(existing) = font.sized_fonts.get(&em_px) {
            return Ok(existing.0);
        }
        let created = create_hfont(&font.descriptor, -(em_px as i32))?;
        let handle = created.0;
        font.sized_fonts.insert(em_px, created);
        Ok(handle)
    }

    fn glyph_for_char(&mut self, font_id: FontId, ch: char) -> Option<GlyphId> {
        let font = self.fonts.get(font_id.0)?;
        let hfont = font.design_font.0;
        let mut utf16 = [0u16; 3];
        let encoded = ch.encode_utf16(&mut utf16[..2]).len();
        // `GetGlyphIndicesW` takes a code-unit count, so a surrogate pair is two
        // units that resolve to one glyph — take the first index it fills in.
        let mut indices = [0u16; 2];
        let filled = unsafe {
            let _guard = SelectedFont::new(self.dc.0, hfont);
            GetGlyphIndicesW(
                self.dc.0,
                PCWSTR(utf16.as_ptr()),
                encoded as i32,
                indices.as_mut_ptr(),
                GGI_MARK_NONEXISTING_GLYPHS,
            )
        };
        if filled == GDI_CALL_FAILED || indices[0] == 0xFFFF || indices[0] == 0 {
            return None;
        }
        Some(GlyphId(indices[0] as u32))
    }

    fn rasterize_glyph(
        &mut self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let em_px = Self::raster_em_px(params);
        let hfont = self.sized_font(params.font_id, em_px)?;
        let dc = self.dc.0;

        let width = raster_bounds.size.width.0 as usize;
        let height = raster_bounds.size.height.0 as usize;
        anyhow::ensure!(
            raster_bounds.size.width.0 <= MAX_GLYPH_PX
                && raster_bounds.size.height.0 <= MAX_GLYPH_PX,
            "glyph bitmap {}x{} exceeds the GDI fallback's limit",
            width,
            height
        );

        let mut metrics = GLYPHMETRICS::default();
        let matrix = identity_mat2();
        let (raw, gdi_width, gdi_height) = unsafe {
            let _guard = SelectedFont::new(dc, hfont);
            let needed = GetGlyphOutlineW(
                dc,
                params.glyph_id.0,
                gray8_by_index(),
                &raw mut metrics,
                0,
                None,
                &matrix,
            );
            anyhow::ensure!(
                needed != GDI_CALL_FAILED,
                "GetGlyphOutlineW size query failed"
            );
            if needed == 0 {
                // A blank glyph (space) legitimately has no bitmap.
                (Vec::new(), 0usize, 0usize)
            } else {
                let mut buffer = vec![0u8; needed as usize];
                let written = GetGlyphOutlineW(
                    dc,
                    params.glyph_id.0,
                    gray8_by_index(),
                    &raw mut metrics,
                    needed,
                    Some(buffer.as_mut_ptr() as *mut c_void),
                    &matrix,
                );
                anyhow::ensure!(
                    written != GDI_CALL_FAILED,
                    "GetGlyphOutlineW rasterization failed"
                );
                (
                    buffer,
                    metrics.gmBlackBoxX as usize,
                    metrics.gmBlackBoxY as usize,
                )
            }
        };

        // `GGO_GRAY8_BITMAP` pads every row to a 4-byte boundary and reports
        // coverage in 65 levels. Unpad, rescale to 0..=255, and place the result
        // inside the bounds the caller already reserved — they can differ by a
        // pixel when the metrics were measured in a separate call.
        let mut alpha = vec![0u8; width * height];
        if !raw.is_empty() && gdi_width > 0 && gdi_height > 0 {
            let stride = (gdi_width + 3) & !3;
            let copy_w = gdi_width.min(width);
            let copy_h = gdi_height.min(height);
            for y in 0..copy_h {
                let src_row = y * stride;
                let dst_row = y * width;
                for x in 0..copy_w {
                    let coverage = raw.get(src_row + x).copied().unwrap_or(0) as u32;
                    let scaled =
                        (coverage.min(GRAY8_LEVELS) * 255 + GRAY8_LEVELS / 2) / GRAY8_LEVELS;
                    alpha[dst_row + x] = scaled as u8;
                }
            }
        }

        if params.is_emoji {
            // No colour glyph support here; hand back the monochrome coverage in
            // the RGBA shape the caller expects for emoji so it still draws.
            let rgba = alpha
                .into_iter()
                .flat_map(|coverage| [0, 0, 0, coverage])
                .collect::<Vec<_>>();
            return Ok((raster_bounds.size, rgba));
        }

        Ok((raster_bounds.size, alpha))
    }

    fn layout_line(&mut self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let em_px = (f32::from(font_size).round().max(1.0) as u32).min(MAX_GLYPH_PX as u32);
        let mut layout = LineLayout {
            font_size,
            len: text.len(),
            ..Default::default()
        };

        // Ascent/descent come from the first run's font so an empty line still
        // has the height the caller sized its row for.
        if let Some(first) = runs.first()
            && let Some(font) = self.fonts.get(first.font_id.0)
        {
            let scale = f32::from(font_size) / font.units_per_em.max(1) as f32;
            layout.ascent = px(font.metrics.ascent * scale);
            layout.descent = px(-font.metrics.descent * scale);
        }

        let mut x = 0.0f32;
        let mut byte_offset = 0usize;
        for run in runs {
            let end = (byte_offset + run.len).min(text.len());
            let slice = text.get(byte_offset..end).unwrap_or("");
            byte_offset = end;
            if slice.is_empty() {
                continue;
            }
            let glyphs = self.shape_run(slice, run.font_id, em_px, font_size, &mut x);
            if !glyphs.is_empty() {
                layout.runs.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs,
                });
            }
        }

        layout.width = px(x);
        layout
    }

    /// Shape one run and append its glyphs, advancing `x` past the run.
    ///
    /// Uniscribe is tried first because it is the only GDI-era path that
    /// positions marks and reorders complex scripts. A failure there (an
    /// unsupported script on an old system, a font Uniscribe refuses) falls
    /// back to raw glyph indices and advances, which is still correct for
    /// scripts that need no reordering.
    fn shape_run(
        &mut self,
        text: &str,
        font_id: FontId,
        em_px: u32,
        font_size: Pixels,
        x: &mut f32,
    ) -> Vec<ShapedGlyph> {
        let Ok(hfont) = self.sized_font(font_id, em_px) else {
            return Vec::new();
        };
        // GDI works in whole pixels; carry the requested size's fractional part
        // so a 12.5 px line is not silently laid out at 13 px.
        let scale = if em_px > 0 {
            f32::from(font_size) / em_px as f32
        } else {
            1.0
        };

        let utf16: Vec<u16> = text.encode_utf16().collect();
        // Map each UTF-16 code-unit index back to a byte index in `text`, so a
        // shaped glyph can report the byte offset gpui indexes selections with.
        let mut utf16_to_byte = Vec::with_capacity(utf16.len() + 1);
        for (byte_ix, ch) in text.char_indices() {
            for _ in 0..ch.len_utf16() {
                utf16_to_byte.push(byte_ix);
            }
        }
        utf16_to_byte.push(text.len());

        match self.shape_run_uniscribe(&utf16, &utf16_to_byte, font_id, em_px, hfont, scale, x) {
            Ok(glyphs) => glyphs,
            Err(error) => {
                log::debug!("GDI text: Uniscribe shaping failed ({error:#}); using plain advances");
                self.shape_run_simple(&utf16, &utf16_to_byte, hfont, scale, x)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shape_run_uniscribe(
        &mut self,
        utf16: &[u16],
        utf16_to_byte: &[usize],
        font_id: FontId,
        em_px: u32,
        hfont: HFONT,
        scale: f32,
        x: &mut f32,
    ) -> Result<Vec<ShapedGlyph>> {
        let dc = self.dc.0;
        let script_cache = self.script_cache_ptr(font_id, em_px)?;

        // Uniscribe writes one item per script/direction change plus a
        // terminator, so the buffer is sized for the worst case of one item per
        // code unit.
        let mut items = vec![SCRIPT_ITEM::default(); utf16.len() + 2];
        let mut item_count = 0i32;
        unsafe {
            ScriptItemize(utf16, None, None, &mut items, &raw mut item_count)
                .context("ScriptItemize")?;
        }
        anyhow::ensure!(item_count > 0, "ScriptItemize produced no items");

        let mut out = Vec::with_capacity(utf16.len());
        let _guard = unsafe { SelectedFont::new(dc, hfont) };

        for item_ix in 0..item_count as usize {
            let start = items[item_ix].iCharPos as usize;
            let end = items[item_ix + 1].iCharPos as usize;
            if end <= start || end > utf16.len() {
                continue;
            }
            let chars = &utf16[start..end];
            // Uniscribe's documented headroom for a shaped item.
            let max_glyphs = chars.len() * 3 / 2 + 16;
            let mut glyphs = vec![0u16; max_glyphs];
            let mut log_clusters = vec![0u16; chars.len()];
            let mut visual_attrs = vec![SCRIPT_VISATTR::default(); max_glyphs];
            let mut glyph_count = 0i32;
            let mut analysis = items[item_ix].a;

            unsafe {
                ScriptShape(
                    dc,
                    script_cache,
                    PCWSTR(chars.as_ptr()),
                    chars.len() as i32,
                    max_glyphs as i32,
                    &raw mut analysis,
                    glyphs.as_mut_ptr(),
                    log_clusters.as_mut_ptr(),
                    visual_attrs.as_mut_ptr(),
                    &raw mut glyph_count,
                )
                .context("ScriptShape")?;
            }
            if glyph_count <= 0 {
                continue;
            }
            let glyph_count = glyph_count as usize;

            let mut advances = vec![0i32; glyph_count];
            let mut offsets = vec![GOFFSET::default(); glyph_count];
            let mut abc = ABC::default();
            unsafe {
                ScriptPlace(
                    dc,
                    script_cache,
                    glyphs.as_ptr(),
                    glyph_count as i32,
                    visual_attrs.as_ptr(),
                    &raw mut analysis,
                    advances.as_mut_ptr(),
                    Some(offsets.as_mut_ptr()),
                    &raw mut abc,
                )
                .context("ScriptPlace")?;
            }

            // `logClusters[char] = glyph` maps forward; invert it so each glyph
            // reports the first source character it came from. Without this a
            // click into shaped Thai lands on the wrong grapheme.
            let mut glyph_to_char = vec![usize::MAX; glyph_count];
            for (char_ix, cluster) in log_clusters.iter().enumerate() {
                let glyph_ix = *cluster as usize;
                if glyph_ix < glyph_count {
                    glyph_to_char[glyph_ix] = glyph_to_char[glyph_ix].min(char_ix);
                }
            }
            let mut last_char = 0usize;
            for glyph_ix in 0..glyph_count {
                if glyph_to_char[glyph_ix] == usize::MAX {
                    glyph_to_char[glyph_ix] = last_char;
                } else {
                    last_char = glyph_to_char[glyph_ix];
                }
            }

            for glyph_ix in 0..glyph_count {
                let source_char = start + glyph_to_char[glyph_ix];
                let byte_index = utf16_to_byte
                    .get(source_char)
                    .copied()
                    .unwrap_or_else(|| utf16_to_byte.last().copied().unwrap_or(0));
                out.push(ShapedGlyph {
                    id: GlyphId(glyphs[glyph_ix] as u32),
                    position: point(px(*x + offsets[glyph_ix].du as f32 * scale), px(0.0)),
                    index: byte_index,
                    is_emoji: false,
                });
                *x += advances[glyph_ix] as f32 * scale;
            }
        }

        Ok(out)
    }

    /// Plain index-and-advance shaping: correct for scripts that need no
    /// reordering, and the safety net when Uniscribe declines.
    fn shape_run_simple(
        &mut self,
        utf16: &[u16],
        utf16_to_byte: &[usize],
        hfont: HFONT,
        scale: f32,
        x: &mut f32,
    ) -> Vec<ShapedGlyph> {
        let dc = self.dc.0;
        let mut indices = vec![0u16; utf16.len()];
        let filled = unsafe {
            let _guard = SelectedFont::new(dc, hfont);
            GetGlyphIndicesW(
                dc,
                PCWSTR(utf16.as_ptr()),
                utf16.len() as i32,
                indices.as_mut_ptr(),
                GGI_MARK_NONEXISTING_GLYPHS,
            )
        };
        if filled == GDI_CALL_FAILED {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(indices.len());
        for (unit_ix, glyph) in indices.iter().enumerate() {
            if *glyph == 0xFFFF {
                continue;
            }
            let mut advance = 0i32;
            let ok = unsafe {
                let _guard = SelectedFont::new(dc, hfont);
                GetCharWidthI(dc, 0, 1, Some(glyph), &raw mut advance).as_bool()
            };
            if !ok {
                advance = 0;
            }
            let byte_index = utf16_to_byte.get(unit_ix).copied().unwrap_or(0);
            out.push(ShapedGlyph {
                id: GlyphId(*glyph as u32),
                position: point(px(*x), px(0.0)),
                index: byte_index,
                is_emoji: false,
            });
            *x += advance as f32 * scale;
        }
        out
    }

    /// Uniscribe caches shaping tables per (font, size). The pointer stays valid
    /// as long as the entry lives in `script_caches`, which outlives the call.
    fn script_cache_ptr(&mut self, font_id: FontId, em_px: u32) -> Result<*mut *mut c_void> {
        let font = self
            .fonts
            .get_mut(font_id.0)
            .with_context(|| format!("unknown GDI font id {}", font_id.0))?;
        let entry = font
            .script_caches
            .entry(em_px)
            .or_insert_with(|| ScriptCache(std::ptr::null_mut()));
        Ok(&raw mut entry.0)
    }
}

/// RAII `SelectObject` guard — restores the DC's previous font on drop so a
/// bail-out mid-measurement cannot leave the shared DC in a foreign state.
struct SelectedFont {
    dc: HDC,
    previous: HGDIOBJ,
}

impl SelectedFont {
    unsafe fn new(dc: HDC, font: HFONT) -> Self {
        let previous = unsafe { SelectObject(dc, font.into()) };
        Self { dc, previous }
    }
}

impl Drop for SelectedFont {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
        }
    }
}

fn identity_mat2() -> MAT2 {
    MAT2 {
        eM11: FIXED { fract: 0, value: 1 },
        eM12: FIXED { fract: 0, value: 0 },
        eM21: FIXED { fract: 0, value: 0 },
        eM22: FIXED { fract: 0, value: 1 },
    }
}

fn glyph_metrics(dc: HDC, hfont: HFONT, glyph: u32) -> Result<GLYPHMETRICS> {
    let mut metrics = GLYPHMETRICS::default();
    let matrix = identity_mat2();
    let result = unsafe {
        let _guard = SelectedFont::new(dc, hfont);
        GetGlyphOutlineW(
            dc,
            glyph,
            metrics_by_index(),
            &raw mut metrics,
            0,
            None,
            &matrix,
        )
    };
    anyhow::ensure!(
        result != GDI_CALL_FAILED,
        "GetGlyphOutlineW(GGO_METRICS) failed"
    );
    Ok(metrics)
}

/// Build the LOGFONT for a descriptor at `height` (negative = character height,
/// which is what makes one logical unit equal one design unit at `-em`).
fn create_hfont(descriptor: &Font, height: i32) -> Result<OwnedFont> {
    let mut log_font = LOGFONTW {
        lfHeight: height,
        lfWeight: descriptor.weight.0.round() as i32,
        lfItalic: u8::from(descriptor.style != FontStyle::Normal),
        lfCharSet: DEFAULT_CHARSET,
        lfOutPrecision: OUT_TT_PRECIS,
        lfClipPrecision: CLIP_DEFAULT_PRECIS,
        // Antialiased rather than ClearType: the mask this backend hands the
        // atlas has one channel, so asking for subpixel output would only
        // produce colour fringes the renderer cannot interpret.
        lfQuality: ANTIALIASED_QUALITY,
        lfPitchAndFamily: (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u8,
        ..Default::default()
    };
    let family: Vec<u16> = descriptor.family.encode_utf16().collect();
    let copy_len = family.len().min(log_font.lfFaceName.len() - 1);
    log_font.lfFaceName[..copy_len].copy_from_slice(&family[..copy_len]);

    let handle = unsafe { CreateFontIndirectW(&log_font) };
    anyhow::ensure!(
        !handle.is_invalid(),
        "CreateFontIndirectW failed for {:?}",
        descriptor.family
    );
    Ok(OwnedFont(handle))
}

/// Create the design-unit face: probe the em square, then rebuild at `-em` so
/// every metric GDI returns is already in font units.
fn create_design_font(dc: HDC, descriptor: &Font) -> Result<OwnedFont> {
    let probe = create_hfont(descriptor, -EM_PROBE_PX)?;
    let em_square = {
        let _guard = unsafe { SelectedFont::new(dc, probe.0) };
        read_outline_metrics(dc)
            .map(|otm| otm.em_square)
            .unwrap_or(0)
    };
    drop(probe);

    let em_square = if em_square == 0 || em_square > MAX_EM_SQUARE {
        // A bitmap-only face has no outline metrics. 2048 keeps the reported
        // units consistent even though the glyphs will not scale smoothly.
        2048
    } else {
        em_square
    };
    create_hfont(descriptor, -(em_square as i32))
}

/// The numeric half of `OUTLINETEXTMETRICW`.
///
/// The Win32 struct also holds four `PSTR` name fields that point *into* the
/// caller's scratch buffer. Returning the struct itself would hand back four
/// pointers into freed memory, so the numbers are copied out and the buffer
/// dies here.
#[derive(Clone, Copy)]
struct OutlineMetrics {
    em_square: u32,
    ascent: i32,
    descent: i32,
    line_gap: u32,
    cap_height: u32,
    x_height: u32,
    font_box: RECT,
    underscore_size: i32,
    underscore_position: i32,
}

fn read_outline_metrics(dc: HDC) -> Option<OutlineMetrics> {
    unsafe {
        let needed = GetOutlineTextMetricsW(dc, 0, None);
        if needed == 0 {
            return None;
        }
        let mut buffer = vec![0u8; needed as usize];
        let otm_ptr = buffer.as_mut_ptr() as *mut OUTLINETEXTMETRICW;
        if GetOutlineTextMetricsW(dc, needed, Some(otm_ptr)) == 0 {
            return None;
        }
        // The buffer is a `Vec<u8>`, so it carries no alignment guarantee for
        // the struct GDI just wrote into it.
        let otm = std::ptr::read_unaligned(otm_ptr);
        Some(OutlineMetrics {
            em_square: otm.otmEMSquare,
            ascent: otm.otmAscent,
            descent: otm.otmDescent,
            line_gap: otm.otmLineGap,
            cap_height: otm.otmsCapEmHeight,
            x_height: otm.otmsXHeight,
            font_box: otm.otmrcFontBox,
            underscore_size: otm.otmsUnderscoreSize,
            underscore_position: otm.otmsUnderscorePosition,
        })
    }
}

/// Read design-unit metrics through a `-em`-sized face.
fn read_font_metrics(dc: HDC, hfont: HFONT) -> Result<(u32, FontMetrics)> {
    let _guard = unsafe { SelectedFont::new(dc, hfont) };
    let otm = read_outline_metrics(dc).context("GetOutlineTextMetricsW returned nothing")?;
    let units_per_em = if otm.em_square == 0 {
        2048
    } else {
        otm.em_square
    };

    // gpui's convention matches DirectWrite: ascent positive above the baseline,
    // descent negative below it. GDI already signs `otmDescent` that way.
    let metrics = FontMetrics {
        units_per_em,
        ascent: otm.ascent as f32,
        descent: otm.descent as f32,
        line_gap: otm.line_gap as f32,
        underline_position: otm.underscore_position as f32,
        underline_thickness: otm.underscore_size as f32,
        cap_height: otm.cap_height as f32,
        x_height: otm.x_height as f32,
        bounding_box: Bounds {
            origin: point(otm.font_box.left as f32, otm.font_box.bottom as f32),
            size: size(
                (otm.font_box.right - otm.font_box.left) as f32,
                (otm.font_box.top - otm.font_box.bottom) as f32,
            ),
        },
    };
    Ok((units_per_em, metrics))
}

fn fallback_font_metrics() -> FontMetrics {
    FontMetrics {
        units_per_em: 2048,
        ascent: 1638.0,
        descent: -410.0,
        line_gap: 0.0,
        underline_position: -205.0,
        underline_thickness: 102.0,
        cap_height: 1462.0,
        x_height: 1062.0,
        bounding_box: Bounds {
            origin: point(-1024.0, -410.0),
            size: size(3072.0, 2662.0),
        },
    }
}

unsafe extern "system" fn enum_families_proc(
    log_font: *const LOGFONTW,
    _metrics: *const TEXTMETRICW,
    _font_type: u32,
    lparam: LPARAM,
) -> i32 {
    unsafe {
        let names = &mut *(lparam.0 as *mut Vec<String>);
        let face = &(*log_font).lfFaceName;
        let len = face.iter().position(|c| *c == 0).unwrap_or(face.len());
        // `@Family` entries are the vertical-writing duplicates Windows adds for
        // CJK; they are not selectable families in this app's sense.
        let name = String::from_utf16_lossy(&face[..len]);
        if !name.starts_with('@') && !name.is_empty() {
            names.push(name);
        }
        1
    }
}

fn enumerate_font_families(dc: HDC) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut log_font = LOGFONTW {
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    unsafe {
        EnumFontFamiliesExW(
            dc,
            &raw mut log_font,
            Some(enum_families_proc),
            LPARAM(&raw mut names as isize),
            0,
        );
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` above re-exports gpui's own `test` attribute, which a glob
    // would otherwise hand to `#[test]` here and expand forever. An explicit
    // import outranks the glob and puts the real one back.
    use core::prelude::v1::test;

    /// `PlatformTextSystem` is `Send + Sync`; the GDI handles inside are only
    /// safe to share because every path goes through the mutex.
    #[test]
    fn the_text_system_is_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GdiTextSystem>();
    }

    /// Creating the system must not need a window, a device, or DirectWrite —
    /// that is the whole reason it exists.
    #[test]
    fn it_builds_without_a_window() {
        let system = GdiTextSystem::new().expect("GDI text system");
        assert!(
            !system.all_font_names().is_empty(),
            "every Windows install has at least one font family"
        );
    }

    /// The whole pipeline on a font that ships with Windows: select, measure,
    /// map a character, lay out a line, and rasterize.
    #[test]
    fn it_measures_and_rasterizes_a_system_font() {
        let system = GdiTextSystem::new().expect("GDI text system");
        let font = font("Segoe UI");
        let font_id = system.font_id(&font).expect("Segoe UI");

        let metrics = system.font_metrics(font_id);
        assert!(metrics.units_per_em > 0);
        assert!(
            metrics.ascent > 0.0 && metrics.descent < 0.0,
            "ascent is above the baseline and descent below it, got {} / {}",
            metrics.ascent,
            metrics.descent
        );

        let glyph = system.glyph_for_char(font_id, 'A').expect("glyph for 'A'");
        let advance = system.advance(font_id, glyph).expect("advance");
        assert!(advance.width > 0.0, "'A' has width");

        let params = RenderGlyphParams {
            font_id,
            glyph_id: glyph,
            font_size: px(16.0),
            subpixel_variant: Default::default(),
            scale_factor: 1.0,
            is_emoji: false,
            subpixel_rendering: false,
            dilation: 0,
        };
        let bounds = system.glyph_raster_bounds(&params).expect("raster bounds");
        assert!(bounds.size.width.0 > 0 && bounds.size.height.0 > 0);

        let (mask_size, mask) = system.rasterize_glyph(&params, bounds).expect("rasterize");
        assert_eq!(
            mask.len(),
            (mask_size.width.0 * mask_size.height.0) as usize,
            "a monochrome glyph is one byte per pixel"
        );
        assert!(
            mask.iter().any(|coverage| *coverage > 0),
            "'A' cannot rasterize to an empty mask"
        );
    }

    #[test]
    fn it_lays_out_a_line_left_to_right() {
        let system = GdiTextSystem::new().expect("GDI text system");
        let font_id = system.font_id(&font("Segoe UI")).expect("Segoe UI");
        let text = "Futureboard";
        let layout = system.layout_line(
            text,
            px(14.0),
            &[FontRun {
                len: text.len(),
                font_id,
            }],
        );

        assert!(layout.width > px(0.0), "a laid-out line has width");
        let glyphs: Vec<_> = layout.runs.iter().flat_map(|run| &run.glyphs).collect();
        assert_eq!(glyphs.len(), text.len(), "one glyph per ASCII character");
        for pair in glyphs.windows(2) {
            assert!(
                pair[1].position.x >= pair[0].position.x,
                "glyph positions advance left to right"
            );
        }
        assert!(glyphs.last().unwrap().index < text.len());
    }

    /// Grayscale only: claiming subpixel would make the renderer read three
    /// channels out of the one-channel mask this backend produces.
    #[test]
    fn it_never_claims_subpixel_coverage() {
        let system = GdiTextSystem::new().expect("GDI text system");
        let font_id = system.font_id(&font("Segoe UI")).expect("Segoe UI");
        assert_eq!(
            system.recommended_rendering_mode(font_id, px(13.0)),
            TextRenderingMode::Grayscale
        );
    }

    /// The reason this backend shapes through Uniscribe instead of walking
    /// characters and summing advances.
    ///
    /// Thai tone marks are zero-advance glyphs stacked over the preceding
    /// consonant. A naive `GetCharWidthI` loop gives each one a full advance
    /// and the marks march sideways across the line, which is exactly the
    /// failure this test would catch.
    #[test]
    fn it_stacks_thai_tone_marks_instead_of_advancing_past_them() {
        let system = GdiTextSystem::new().expect("GDI text system");
        let font_id = system.font_id(&font("Segoe UI")).expect("Segoe UI");
        // A stripped Windows image may have no Thai coverage; there is nothing
        // to assert about shaping a font that cannot draw the script.
        if system.glyph_for_char(font_id, 'ก').is_none() {
            return;
        }

        let width_of = |text: &str| {
            system
                .layout_line(
                    text,
                    px(16.0),
                    &[FontRun {
                        len: text.len(),
                        font_id,
                    }],
                )
                .width
        };

        let base = width_of("ก");
        // ก + MAI THO (U+0E49): one base glyph plus one mark above it.
        let with_mark = width_of("ก้");
        assert!(base > px(0.0), "the base consonant has width");
        assert_eq!(
            with_mark, base,
            "a tone mark must not add advance — got {with_mark:?} vs {base:?}"
        );
    }

    /// A second request for the same descriptor must not allocate a second
    /// HFONT and a second Uniscribe cache.
    #[test]
    fn font_ids_are_stable_per_descriptor() {
        let system = GdiTextSystem::new().expect("GDI text system");
        let first = system.font_id(&font("Segoe UI")).unwrap();
        let second = system.font_id(&font("Segoe UI")).unwrap();
        assert_eq!(first, second);
    }
}
