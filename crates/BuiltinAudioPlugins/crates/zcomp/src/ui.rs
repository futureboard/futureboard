//! Embedded editor UI for Z-Comp.
//!
//! Assets are produced at build time from `editor/dist` and served through
//! `mikoplugin://zcomp/...`.

use builtin_ui_embed::{EmbeddedPluginUi, EmbeddedUiAsset, EmbeddedUiAssetTable};

include!(concat!(env!("OUT_DIR"), "/embedded_ui_assets.rs"));

pub const UI_ORIGIN: &str = "zcomp";

pub struct ZcompUi;

impl ZcompUi {
    pub fn table() -> EmbeddedUiAssetTable {
        EmbeddedUiAssetTable::new(EMBEDDED_UI_ASSETS)
    }

    pub fn is_embedded() -> bool {
        !EMBEDDED_UI_ASSETS.is_empty()
    }
}

impl EmbeddedPluginUi for ZcompUi {
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
            let _ = ZcompUi::get_ui_asset(path);
            let _ = ZcompUi::resolve_ui_asset(path);
        }
    }

    #[test]
    fn traversal_is_rejected_even_when_a_table_is_present() {
        assert!(ZcompUi::get_ui_asset("/../../etc/passwd").is_none());
        assert!(ZcompUi::resolve_ui_asset("/../../etc/passwd").is_none());
    }

    #[test]
    fn a_built_dist_serves_index_html_and_round_trips_every_entry() {
        if !ZcompUi::is_embedded() {
            return;
        }
        let index = ZcompUi::get_ui_asset("/index.html")
            .expect("an embedded table always contains /index.html");
        assert!(!index.is_empty());
        assert!(index.mime_type.starts_with("text/html"));

        assert_eq!(
            ZcompUi::resolve_ui_asset("/").map(|a| a.path),
            Some("/index.html")
        );
        assert_eq!(
            ZcompUi::resolve_ui_asset("").map(|a| a.path),
            Some("/index.html")
        );

        let table = ZcompUi::table();
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
