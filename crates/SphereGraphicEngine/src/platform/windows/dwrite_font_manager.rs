//! DirectWrite font manager: optional in-memory font loading, custom
//! collection, and system font fallback.
//!
//! Mirrors GPUI's Windows font path: embedded font bytes via
//! `IDWriteInMemoryFontFileLoader`, a custom font collection, and
//! `IDWriteFontFallback` for Thai / extended Unicode. The manager owns the
//! DirectWrite factory and the GDI-interop handles the text renderer needs; it
//! is **not** plugin- or app-aware — fonts and families come in via
//! [`FontConfig`] and a slice of font blobs.

use windows::core::{BOOL, HSTRING, PCWSTR};
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory5, IDWriteFontCollection1, IDWriteFontFallback,
    IDWriteFontSetBuilder1, IDWriteGdiInterop, IDWriteInMemoryFontFileLoader,
    IDWriteRenderingParams, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_UNICODE_RANGE,
};
use windows::Win32::System::SystemServices::LOCALE_NAME_MAX_LENGTH;

/// Family / weight policy for the font manager. Generic — no app or plugin
/// knowledge.
#[derive(Debug, Clone)]
pub struct FontConfig {
    /// Preferred UI family name (e.g. "Segoe UI").
    pub primary_family: String,
    /// Fallback family for scripts the primary lacks (e.g. "Leelawadee UI").
    pub fallback_family: String,
    /// Default weight used when resolving the primary family.
    pub default_weight: u32,
}

/// Read-only facts about what the manager loaded, for caller-side diagnostics.
#[derive(Debug, Clone)]
pub struct FontDiagnostics {
    pub primary_family: String,
    pub fallback_family: String,
    pub custom_collection_ok: bool,
    pub font_bytes_loaded: u32,
    pub resolved_primary: String,
}

/// Owns the DirectWrite factory, embedded-font collection, system collection,
/// fallback chain, and GDI-interop handles.
pub struct DWriteFontManager {
    pub(crate) factory: IDWriteFactory5,
    in_memory_loader: IDWriteInMemoryFontFileLoader,
    pub(crate) custom_collection: IDWriteFontCollection1,
    pub(crate) system_collection: IDWriteFontCollection1,
    pub(crate) font_fallback: IDWriteFontFallback,
    pub(crate) gdi: IDWriteGdiInterop,
    pub(crate) rendering_params: IDWriteRenderingParams,
    pub(crate) locale: HSTRING,
    pub(crate) resolved_primary: String,
    diagnostics: FontDiagnostics,
}

impl Drop for DWriteFontManager {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .factory
                .UnregisterFontFileLoader(&self.in_memory_loader);
        }
    }
}

impl DWriteFontManager {
    /// Build a manager from in-memory font blobs and a [`FontConfig`].
    ///
    /// Returns `None` if DirectWrite initialization fails. `font_blobs` are
    /// added to a custom collection in order; empty blobs are skipped.
    pub fn new(font_blobs: &[&[u8]], config: &FontConfig) -> Option<Self> {
        unsafe { make_font_manager(font_blobs, config) }
    }

    /// What the manager loaded (families, byte counts, resolved primary).
    pub fn diagnostics(&self) -> &FontDiagnostics {
        &self.diagnostics
    }

    pub(crate) fn resolved_primary(&self) -> &str {
        &self.resolved_primary
    }
}

unsafe fn load_embedded_fonts(
    factory: &IDWriteFactory5,
    loader: &IDWriteInMemoryFontFileLoader,
    builder: &IDWriteFontSetBuilder1,
    blobs: &[&[u8]],
) -> u32 {
    let mut loaded = 0u32;
    for bytes in blobs {
        if bytes.is_empty() {
            continue;
        }
        match loader
            .CreateInMemoryFontFileReference(
                factory,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                None,
            )
            .and_then(|file| builder.AddFontFile(&file))
        {
            Ok(()) => {
                loaded += 1;
                eprintln!("[Fonts] loaded embedded font bytes={}", bytes.len());
            }
            Err(err) => {
                eprintln!(
                    "[Fonts] failed embedded font bytes={} error={err}",
                    bytes.len()
                );
            }
        }
    }
    loaded
}

unsafe fn build_font_fallback(
    factory: &IDWriteFactory5,
    custom_collection: &IDWriteFontCollection1,
    system_collection: &IDWriteFontCollection1,
    fallback_family: &str,
) -> Option<IDWriteFontFallback> {
    let builder = factory.CreateFontFallbackBuilder().ok()?;
    let fallback_name = HSTRING::from(fallback_family);
    for collection in [custom_collection, system_collection] {
        let font_set = collection.GetFontSet().ok()?;
        let fonts = font_set
            .GetMatchingFonts(
                &fallback_name,
                DWRITE_FONT_WEIGHT(400),
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )
            .ok()?;
        if fonts.GetFontCount() == 0 {
            continue;
        }
        let face_ref = fonts.GetFontFaceReference(0).ok()?;
        let face = face_ref.CreateFontFace().ok()?;
        let mut count = 0u32;
        face.GetUnicodeRanges(None, &mut count).ok()?;
        if count == 0 {
            continue;
        }
        let mut ranges = vec![DWRITE_UNICODE_RANGE::default(); count as usize];
        face.GetUnicodeRanges(Some(&mut ranges), &mut count).ok()?;
        let _ = builder.AddMapping(&ranges, &[fallback_name.as_ptr()], None, None, None, 1.0);
        break;
    }
    if let Ok(system) = factory.GetSystemFontFallback() {
        let _ = builder.AddMappings(&system);
    }
    builder.CreateFontFallback().ok()
}

/// Resolve a family name within a collection to the locale-specific family
/// name DirectWrite expects (or `None` if the family isn't present).
pub(crate) unsafe fn resolve_family_in_collection(
    collection: &IDWriteFontCollection1,
    family: &str,
    weight: u32,
    locale: &HSTRING,
) -> Option<String> {
    let family_h = HSTRING::from(family);
    let font_set = collection.GetFontSet().ok()?;
    let fonts = font_set
        .GetMatchingFonts(
            &family_h,
            DWRITE_FONT_WEIGHT(weight as i32),
            DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
        )
        .ok()?;
    if fonts.GetFontCount() == 0 {
        return None;
    }
    let face_ref = fonts.GetFontFaceReference(0).ok()?;
    let face = face_ref.CreateFontFace().ok()?;
    let names = face.GetFamilyNames().ok()?;
    let mut index = 0u32;
    let mut exists = BOOL(0);
    names.FindLocaleName(locale, &mut index, &mut exists).ok()?;
    if !exists.as_bool() {
        names
            .FindLocaleName(PCWSTR::null(), &mut index, &mut exists)
            .ok()?;
    }
    if !exists.as_bool() {
        return Some(family.to_string());
    }
    let len = names.GetStringLength(index).ok()? as usize;
    let mut buf = vec![0u16; len + 1];
    names.GetString(index, &mut buf).ok()?;
    Some(String::from_utf16_lossy(&buf[..len]))
}

unsafe fn make_font_manager(
    font_blobs: &[&[u8]],
    config: &FontConfig,
) -> Option<DWriteFontManager> {
    let factory: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
    let in_memory_loader = factory.CreateInMemoryFontFileLoader().ok()?;
    factory.RegisterFontFileLoader(&in_memory_loader).ok()?;
    let builder = factory.CreateFontSetBuilder().ok()?;

    let font_bytes_loaded = load_embedded_fonts(&factory, &in_memory_loader, &builder, font_blobs);

    let custom_font_set = builder.CreateFontSet().ok()?;
    let custom_collection = factory
        .CreateFontCollectionFromFontSet(&custom_font_set)
        .ok()?;
    let custom_collection_ok = custom_collection.GetFontFamilyCount() > 0;

    let mut system_collection = None;
    factory
        .GetSystemFontCollection(false, &mut system_collection, true)
        .ok()?;
    let system_collection = system_collection?;

    let mut locale = [0u16; LOCALE_NAME_MAX_LENGTH as usize];
    GetUserDefaultLocaleName(&mut locale);
    let locale = HSTRING::from_wide(&locale);

    let font_fallback = build_font_fallback(
        &factory,
        &custom_collection,
        &system_collection,
        &config.fallback_family,
    )
    .or_else(|| factory.GetSystemFontFallback().ok())?;

    let resolved_primary = resolve_family_in_collection(
        &custom_collection,
        &config.primary_family,
        config.default_weight,
        &locale,
    )
    .or_else(|| {
        resolve_family_in_collection(
            &system_collection,
            &config.primary_family,
            config.default_weight,
            &locale,
        )
    })
    .unwrap_or_else(|| "Segoe UI".to_string());

    let gdi = factory.GetGdiInterop().ok()?;
    let rendering_params = factory.CreateRenderingParams().ok()?;

    let diagnostics = FontDiagnostics {
        primary_family: config.primary_family.clone(),
        fallback_family: config.fallback_family.clone(),
        custom_collection_ok,
        font_bytes_loaded,
        resolved_primary: resolved_primary.clone(),
    };

    Some(DWriteFontManager {
        factory,
        in_memory_loader,
        custom_collection,
        system_collection,
        font_fallback,
        gdi,
        rendering_params,
        locale,
        resolved_primary,
        diagnostics,
    })
}
