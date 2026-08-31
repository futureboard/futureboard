//! Audio Unit v2 ARA property shim and reciprocal adapters.

/// Audited opaque C ABI implemented by the Apple-only Objective-C++ shim.
pub mod ffi;
mod host;
mod plugin;

pub use host::AudioUnitHostInstance;
pub use plugin::AudioUnitPluginAdapter;
