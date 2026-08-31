//! CLAP ARA declarations and reciprocal adapters.

pub mod sys;

mod host;
mod plugin;

pub use host::{ClapAraHostFactory, ClapAraHostPlugin, DiscoveredClapFactory};
pub use plugin::{clap_ara_get_extension, ClapAraEntry, ClapAraPluginAdapter};
