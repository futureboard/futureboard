//! Minimal composite `FontManager`.
//!
//! Phase 1 responsibilities only:
//! - resolve the platform system-UI anchor family,
//! - build and cache `gpui::Font` descriptors per (role, weight),
//! - attach the platform composite fallback chain (see [`super::chains`]).
//!
//! There is intentionally **no** font database, coverage index, or per-glyph
//! resolution here. Shaping and per-cluster fallback stay entirely inside the
//! native text system, driven by the `FontFallbacks` list we attach.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use gpui::{font, Font, FontFallbacks, FontWeight, SharedString};

use super::chains;

/// Composite font roles exposed to the UI. `Display` selects a large-optical
/// anchor where the platform provides one; otherwise it mirrors `Ui`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CompositeRole {
    Ui,
    Display,
}

/// Process-wide composite font policy. Cheap to clone-out descriptors from.
pub struct FontManager {
    ui_anchor: SharedString,
    display_anchor: SharedString,
    ui_fallbacks: Vec<String>,
    display_fallbacks: Vec<String>,
    /// Windows only: whether the modern `Segoe UI Variable Text` family was
    /// found in the system font collection. Recorded for diagnostics.
    #[cfg(target_os = "windows")]
    segoe_variable_found: bool,
    /// `(role, weight bits) -> descriptor`. GPUI keys its own font/metric caches
    /// off the `Font` value, so returning the same value keeps those warm too.
    descriptors: RwLock<HashMap<(CompositeRole, u32), Font>>,
}

impl FontManager {
    /// Global instance. Discovery runs once, lazily, on first access.
    pub fn global() -> &'static FontManager {
        static MANAGER: OnceLock<FontManager> = OnceLock::new();
        MANAGER.get_or_init(FontManager::register_system_fonts)
    }

    /// Resolve anchors and assemble composite chains for the current platform.
    pub fn register_system_fonts() -> FontManager {
        #[cfg(target_os = "windows")]
        let manager = {
            let segoe_variable_found = windows_family_installed(chains::WINDOWS_VARIABLE_TEXT);
            let ui_anchor: SharedString = if segoe_variable_found {
                chains::WINDOWS_VARIABLE_TEXT.into()
            } else {
                chains::WINDOWS_LEGACY_UI.into()
            };
            // Display uses the large-optical variant only if both it and the
            // text variant are present; otherwise it mirrors the UI anchor.
            let display_anchor: SharedString = if segoe_variable_found
                && windows_family_installed(chains::WINDOWS_VARIABLE_DISPLAY)
            {
                chains::WINDOWS_VARIABLE_DISPLAY.into()
            } else {
                ui_anchor.clone()
            };
            let fallbacks = dedup_chain(&ui_anchor, chains::WINDOWS_UI_FALLBACKS);
            FontManager {
                ui_anchor,
                display_anchor: display_anchor.clone(),
                display_fallbacks: dedup_chain(&display_anchor, chains::WINDOWS_UI_FALLBACKS),
                ui_fallbacks: fallbacks,
                segoe_variable_found,
                descriptors: RwLock::new(HashMap::new()),
            }
        };

        #[cfg(target_os = "macos")]
        let manager = {
            // GPUI/CoreText resolves `.SystemUIFont` to the real system family
            // (SF Pro). We never hardcode Helvetica or assume a selectable name.
            let ui_anchor: SharedString = ".SystemUIFont".into();
            FontManager {
                ui_fallbacks: dedup_chain(&ui_anchor, chains::MACOS_UI_FALLBACKS),
                display_fallbacks: dedup_chain(&ui_anchor, chains::MACOS_UI_FALLBACKS),
                display_anchor: ui_anchor.clone(),
                ui_anchor,
                descriptors: RwLock::new(HashMap::new()),
            }
        };

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let manager = {
            // Unchanged Linux behavior: Fontconfig-first, previous stack verbatim.
            let ui_anchor: SharedString = chains::LINUX_UI_ANCHOR.into();
            let fallbacks: Vec<String> = chains::LINUX_UI_FALLBACKS
                .iter()
                .map(|s| s.to_string())
                .collect();
            FontManager {
                display_anchor: ui_anchor.clone(),
                ui_anchor,
                display_fallbacks: fallbacks.clone(),
                ui_fallbacks: fallbacks,
                descriptors: RwLock::new(HashMap::new()),
            }
        };

        #[cfg(debug_assertions)]
        manager.log_diagnostics();
        manager
    }

    /// Composite UI font descriptor at the given weight.
    pub fn ui_font(&self, weight: FontWeight) -> Font {
        self.descriptor(CompositeRole::Ui, weight)
    }

    /// Composite display font descriptor (large text) at the given weight.
    pub fn display_font(&self, weight: FontWeight) -> Font {
        self.descriptor(CompositeRole::Display, weight)
    }

    /// Live system-UI anchor family for the current platform. For diagnostics
    /// and callers that need the descriptor string (`.SystemUIFont` on macOS).
    pub fn ui_anchor(&self) -> &SharedString {
        &self.ui_anchor
    }

    fn descriptor(&self, role: CompositeRole, weight: FontWeight) -> Font {
        let key = (role, weight.0.to_bits());
        if let Some(existing) = self.descriptors.read().unwrap().get(&key) {
            return existing.clone();
        }

        let (anchor, fallbacks) = match role {
            CompositeRole::Ui => (&self.ui_anchor, &self.ui_fallbacks),
            CompositeRole::Display => (&self.display_anchor, &self.display_fallbacks),
        };

        let mut descriptor = font(anchor.clone());
        descriptor.weight = weight;
        descriptor.fallbacks = Some(FontFallbacks::from_fonts(fallbacks.clone()));

        self.descriptors
            .write()
            .unwrap()
            .insert(key, descriptor.clone());
        descriptor
    }

    #[cfg(debug_assertions)]
    fn log_diagnostics(&self) {
        eprintln!("[fonts] platform={}", std::env::consts::OS);
        eprintln!("[fonts] ui_anchor={}", self.ui_anchor);
        eprintln!("[fonts] display_anchor={}", self.display_anchor);
        eprintln!("[fonts] ui_fallbacks={:?}", self.ui_fallbacks);
        #[cfg(target_os = "windows")]
        eprintln!(
            "[fonts] segoe_ui_variable_text_found={}",
            self.segoe_variable_found
        );
    }
}

/// Drop the anchor and any repeat from a static chain, returning owned strings.
/// A duplicate only adds a redundant descriptor to the native cascade.
fn dedup_chain(anchor: &str, chain: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(chain.len());
    for &family in chain {
        if family == anchor || out.iter().any(|kept| kept == family) {
            continue;
        }
        out.push(family.to_string());
    }
    out
}

/// Whether a font family is present in the DirectWrite system font collection.
///
/// Used only at startup to decide the Windows anchor; not on any render path.
#[cfg(target_os = "windows")]
fn windows_family_installed(family: &str) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory5, IDWriteFontCollection1, DWRITE_FACTORY_TYPE_SHARED,
    };

    unsafe {
        let Ok(factory) = DWriteCreateFactory::<IDWriteFactory5>(DWRITE_FACTORY_TYPE_SHARED) else {
            return false;
        };
        let mut collection: Option<IDWriteFontCollection1> = None;
        if factory
            .GetSystemFontCollection(false, &mut collection, true)
            .is_err()
        {
            return false;
        }
        let Some(collection) = collection else {
            return false;
        };
        let mut index: u32 = 0;
        let mut exists = windows::core::BOOL(0);
        if collection
            .FindFamilyName(&HSTRING::from(family), &mut index, &mut exists)
            .is_err()
        {
            return false;
        }
        exists.as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_chain_drops_anchor_and_repeats() {
        let chain = dedup_chain("Segoe UI", &["Segoe UI", "Leelawadee UI", "Leelawadee UI"]);
        assert_eq!(chain, vec!["Leelawadee UI".to_string()]);
    }

    #[test]
    fn ui_font_carries_the_composite_fallback_chain() {
        let manager = FontManager::register_system_fonts();
        let descriptor = manager.ui_font(FontWeight::NORMAL);
        let fallbacks = descriptor
            .fallbacks
            .expect("composite UI font must attach a fallback chain");
        assert!(
            !fallbacks.fallback_list().is_empty(),
            "fallback chain must be non-empty"
        );
        // The anchor must never appear in its own fallback list.
        assert!(
            !fallbacks
                .fallback_list()
                .iter()
                .any(|family| *family == descriptor.family),
            "anchor must not repeat inside its fallback chain"
        );
    }

    #[test]
    fn weight_is_threaded_into_the_descriptor() {
        let manager = FontManager::register_system_fonts();
        assert_eq!(manager.ui_font(FontWeight::BOLD).weight, FontWeight::BOLD);
        assert_eq!(
            manager.ui_font(FontWeight::SEMIBOLD).weight,
            FontWeight::SEMIBOLD
        );
    }

    #[test]
    fn descriptors_are_cached_by_role_and_weight() {
        let manager = FontManager::register_system_fonts();
        let first = manager.ui_font(FontWeight::MEDIUM);
        let second = manager.ui_font(FontWeight::MEDIUM);
        assert_eq!(first, second);
        assert_eq!(manager.descriptors.read().unwrap().len(), 1);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn linux_behavior_is_unchanged() {
        let manager = FontManager::register_system_fonts();
        let descriptor = manager.ui_font(FontWeight::NORMAL);
        assert_eq!(descriptor.family.as_ref(), chains::LINUX_UI_ANCHOR);
    }
}
