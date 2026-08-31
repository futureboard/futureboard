//! Validated version-aware access to foreign ARA plug-in factories and controllers.

mod controller;
mod dispatch;
mod factory;
mod generated_dispatch;

pub use controller::DocumentController;
pub use factory::{FactoryMetadata, LoadedFactory};
pub use generated_dispatch::{DispatchMethod, DISPATCH_METHODS};

/// Returns the generated host-to-plug-in dispatch coverage manifest.
pub fn dispatch_manifest() -> &'static [DispatchMethod] {
    DISPATCH_METHODS
}
