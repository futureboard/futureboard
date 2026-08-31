//! Companion API adapters connecting ARA to CLAP, VST3, and Audio Unit hosts.
//!
//! # Role and boundaries
//!
//! This crate maps stable ARA factory identity and one-shot controller binding into supplied public
//! companion APIs. It does not implement DSP, format entry points, state serialization, GUI, bundle
//! registration, or signing. [`CompanionProcessorBinding`] has **No direct C counterpart**; it
//! supports the role and lifecycle rules shared by `ARACLAP.h`, `ARAVST3.h`, and `ARAAudioUnit.h`.
//!
//! # Lifecycle and threading
//!
//! Factory backing lives through format entry teardown. Binding precedes activation, state load,
//! processing-related extension use, and view creation. Processor and controller may die in either
//! documented order; shared tombstoned state survives until both sides release it. Model operations
//! use the model thread, while DSP and GUI threading remain format-owned.
//!
//! # Features and platforms
//!
//! `clap` uses checked-in CLAP 1.1.9 declarations. `vst3` requires `ARA_VST3_SDK_DIR` and a C++17
//! compiler. `audio-unit-v2` is Apple-only and requires `ARA_AUDIO_UNIT_SDK_DIR` plus platform Core
//! Audio headers. Builds never download SDKs.
//!
//! # Compatibility and licensing
//!
//! The crate targets Rust 1.82 and ARA 2.3. It is MIT OR Apache-2.0. CLAP is MIT; VST3 and Audio
//! Unit SDK licenses are independent and must be accepted by the integrator.
//!
//! # Example
//!
//! ```
//! use ara2_bridge_companion::CompanionRoles;
//!
//! let roles = CompanionRoles::PLAYBACK_RENDERER | CompanionRoles::EDITOR_RENDERER;
//! assert!(roles.contains(CompanionRoles::PLAYBACK_RENDERER));
//! ```
//!
//! See the workspace companion specification and the upstream
//! [ARA API](https://github.com/Celemony/ARA_API).

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::missing_safety_doc)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod binding;
mod lifecycle;

#[cfg(any(feature = "clap", feature = "vst3", feature = "audio-unit-v2"))]
pub(crate) use binding::record_controller_destroy_snapshot;
pub use binding::{
    controller_destroy_handler_count, notify_document_controller_destroyed,
    register_controller_destroy_handler, take_controller_destroy_snapshots,
    CompanionControllerBinding, CompanionFactory, CompanionLifetimeProbe,
    CompanionProcessorBinding, CompanionRoles, ControllerDestroyRegistration,
    ControllerDestroySnapshot,
};
pub use lifecycle::LifecycleEvent;

/// CLAP 1.1.9 ARA companion declarations and adapters.
#[cfg(feature = "clap")]
pub mod clap;

/// VST3 ARA companion shim and reciprocal adapters.
#[cfg(feature = "vst3")]
pub mod vst3;

/// Apple Audio Unit v2 ARA property shim and reciprocal adapters.
#[cfg(feature = "audio-unit-v2")]
pub mod audio_unit;
