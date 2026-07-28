//! Embedded editor UI for 67Clipper.
//!
//! The asset table is produced at build time by `build.rs` from `editorui/dist`
//! and lives in the library's read-only data segment. The CEF host serves it
//! through the `mikoplugin://clipper67/...` scheme; see [`builtin_ui_embed`]
//! for the lookup and URL rules.

use builtin_ui_embed::{EmbeddedPluginUi, EmbeddedUiAsset, EmbeddedUiAssetTable};

// Emits `static EMBEDDED_UI_ASSETS: &[::builtin_ui_embed::EmbeddedUiAsset]`,
// sorted by path so the table can be binary-searched.
include!(concat!(env!("OUT_DIR"), "/embedded_ui_assets.rs"));

/// The URL origin (`mikoplugin://<origin>/...`) this plugin's assets are served
/// under. Must match the `stem` in `SpherePluginHost`'s built-in catalog.
pub const UI_ORIGIN: &str = "clipper67";

/// Marker type carrying the embedded editor assets.
pub struct Clipper67Ui;

impl Clipper67Ui {
    /// The embedded asset table. Empty when `editorui/dist` was not built.
    pub fn table() -> EmbeddedUiAssetTable {
        EmbeddedUiAssetTable::new(EMBEDDED_UI_ASSETS)
    }

    /// Whether a UI was actually embedded at build time. The host uses this to
    /// report a clear error instead of opening a blank editor window.
    pub fn is_embedded() -> bool {
        !EMBEDDED_UI_ASSETS.is_empty()
    }
}

impl EmbeddedPluginUi for Clipper67Ui {
    fn get_ui_asset(path: &str) -> Option<EmbeddedUiAsset> {
        Self::table().get(path).copied()
    }

    fn resolve_ui_asset(path: &str) -> Option<EmbeddedUiAsset> {
        Self::table().resolve(path).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_are_total_and_never_panic() {
        for path in [
            "",
            "/",
            "/index.html",
            "/../secret",
            "/assets/%zz",
            "\\windows\\path",
            "/a/b/c/d/e",
        ] {
            let _ = Clipper67Ui::get_ui_asset(path);
            let _ = Clipper67Ui::resolve_ui_asset(path);
        }
    }

    #[test]
    fn traversal_is_rejected_even_when_a_table_is_present() {
        assert!(Clipper67Ui::get_ui_asset("/../../etc/passwd").is_none());
        assert!(Clipper67Ui::resolve_ui_asset("/../../etc/passwd").is_none());
    }

    #[test]
    fn a_built_dist_serves_index_html_and_round_trips_every_entry() {
        if !Clipper67Ui::is_embedded() {
            return;
        }
        let index = Clipper67Ui::get_ui_asset("/index.html")
            .expect("an embedded table always contains /index.html");
        assert!(!index.is_empty());
        assert!(index.mime_type.starts_with("text/html"));

        assert_eq!(
            Clipper67Ui::resolve_ui_asset("/").map(|a| a.path),
            Some("/index.html")
        );
        assert_eq!(
            Clipper67Ui::resolve_ui_asset("").map(|a| a.path),
            Some("/index.html")
        );

        let table = Clipper67Ui::table();
        for asset in EMBEDDED_UI_ASSETS {
            assert_eq!(
                table
                    .get(asset.path)
                    .map(|found: &EmbeddedUiAsset| found.path),
                Some(asset.path),
                "asset {} is not retrievable — table is not sorted by path",
                asset.path
            );
        }
    }
}
