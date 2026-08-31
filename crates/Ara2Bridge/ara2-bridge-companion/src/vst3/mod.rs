//! VST3 ARA companion shim and reciprocal adapters.

/// Audited opaque C ABI implemented by the pinned C++ shim.
pub mod ffi;
mod host;
mod plugin;

pub use host::{
    match_vst3_classes, Vst3AraMainClass, Vst3ClassId, Vst3ClassMatch, Vst3HostMainFactory,
    Vst3HostPlugin, Vst3ProcessorClass,
};
pub use plugin::{Vst3MainFactoryAdapter, Vst3PluginEntryAdapter};
