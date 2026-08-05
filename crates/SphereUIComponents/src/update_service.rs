//! Application update provider registry.
//!
//! The UI crate owns the update dialog, but the release/asset transport lives
//! in the `futureboard_native` binary (`apps/native/studio/src/updater.rs`).
//! Rather than invert the crate dependency, the binary registers a provider at
//! boot and the dialog drives it through this plain-data interface.
//!
//! Every provider method blocks (network, filesystem, process spawn). Callers
//! must run them on GPUI's background executor, never on the UI or audio
//! thread.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::settings::UpdateChannel;

/// A release the running build can move to.
///
/// `payload` carries the provider's own release/asset record so the dialog can
/// hand the exact candidate back to [`UpdateProvider::download`] without the UI
/// crate knowing the GitHub schema.
#[derive(Clone)]
pub struct UpdateCandidate {
    pub version: String,
    pub channel: UpdateChannel,
    pub asset_name: String,
    pub asset_size: u64,
    pub release_url: Option<String>,
    pub payload: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for UpdateCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateCandidate")
            .field("version", &self.version)
            .field("channel", &self.channel)
            .field("asset_name", &self.asset_name)
            .field("asset_size", &self.asset_size)
            .finish_non_exhaustive()
    }
}

/// What the caller must do after [`UpdateProvider::install`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The installer/swapper is running and needs this process to exit before
    /// it can replace the application files. The caller quits the app.
    QuitRequired,
    /// The update was handed to the platform (installer UI, Finder, package
    /// manager) and the user drives it from there. The app keeps running.
    Handoff,
}

/// `(received_bytes, total_bytes)`; `total` is `0` when the size is unknown.
pub type DownloadProgressFn<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

pub trait UpdateProvider: Send + Sync + 'static {
    /// Version string of the running build, in semver form.
    fn current_version(&self) -> String;

    /// Blocking release lookup. `Ok(None)` means "already up to date".
    fn check(&self, channel: UpdateChannel) -> Result<Option<UpdateCandidate>, String>;

    /// Blocking download to the update cache. Returns the staged file path.
    fn download(
        &self,
        candidate: &UpdateCandidate,
        progress: DownloadProgressFn<'_>,
    ) -> Result<PathBuf, String>;

    /// Blocking hand-off of the staged file to the platform installer.
    fn install(&self, staged: &Path) -> Result<InstallOutcome, String>;
}

type ProviderSlot = RwLock<Option<Arc<dyn UpdateProvider>>>;

fn provider_slot() -> &'static ProviderSlot {
    static SLOT: OnceLock<ProviderSlot> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install the provider that backs the update dialog. Called once at boot by
/// the native binary.
pub fn set_update_provider(provider: Arc<dyn UpdateProvider>) {
    if let Ok(mut slot) = provider_slot().write() {
        *slot = Some(provider);
    }
}

/// The registered provider, or `None` in builds without an update transport
/// (tests, tools, and any host that never called [`set_update_provider`]).
pub fn update_provider() -> Option<Arc<dyn UpdateProvider>> {
    provider_slot().read().ok().and_then(|slot| slot.clone())
}

/// `1.2 GB` / `340.5 MB` / `12.0 KB` — asset sizes for the dialog.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting_switches_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }
}
