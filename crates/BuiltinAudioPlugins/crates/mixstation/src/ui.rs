//! Embedded editor assets served from `mikoplugin://mixstation/...`.

use builtin_ui_embed::{EmbeddedPluginUi, EmbeddedUiAsset, EmbeddedUiAssetTable};

include!(concat!(env!("OUT_DIR"), "/embedded_ui_assets.rs"));

pub const UI_ORIGIN: &str = "mixstation";

pub struct MixStationUi;

impl MixStationUi {
    pub fn table() -> EmbeddedUiAssetTable {
        EmbeddedUiAssetTable::new(EMBEDDED_UI_ASSETS)
    }

    pub fn is_embedded() -> bool {
        !EMBEDDED_UI_ASSETS.is_empty()
    }
}

impl EmbeddedPluginUi for MixStationUi {
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
    fn traversal_is_rejected() {
        assert!(MixStationUi::get_ui_asset("/../../secret").is_none());
        assert!(MixStationUi::resolve_ui_asset("/../../secret").is_none());
    }

    #[test]
    fn embedded_entries_are_retrievable() {
        if !MixStationUi::is_embedded() {
            return;
        }
        assert!(MixStationUi::resolve_ui_asset("/").is_some());
        let table = MixStationUi::table();
        for asset in EMBEDDED_UI_ASSETS {
            assert!(table.get(asset.path).is_some());
        }
    }
}
