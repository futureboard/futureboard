//! Pregenerated raw FFI bindings for ARA 2.3.
//!
//! # Role and boundaries
//!
//! This crate mirrors the C ABI published by the
//! [Celemony ARA API](https://github.com/Celemony/ARA_API). Its generated items have direct ARA C
//! counterparts with the same upstream symbol names. It provides no host or plug-in behavior;
//! higher-level crates own validation, lifetimes, threading, and callback policy. Downstream builds
//! do not run bindgen and do not require Clang or an SDK checkout.
//!
//! # Lifecycle, threading, ownership, and failure
//!
//! ARA records are packed and versioned by their leading `structSize` field. Never create a
//! reference to a potentially unaligned field. Use [`access`] and consult [`compatibility`] before
//! reading a generation-dependent field. Raw pointers, nullable callbacks, lifetimes, thread rules,
//! realtime status, and failure fallbacks remain the caller's responsibility.
//!
//! # Features and platforms
//!
//! There are no Cargo features. Generated layouts are selected for x86_64, AArch64, or i686;
//! unsupported architectures fail at compile time. Published packages contain all generated input.
//!
//! # Compatibility and licensing
//!
//! The constants below identify ARA API 2.3.0 and the SDK commit used for generation. Generated
//! derivatives retain Celemony's Apache-2.0 provenance; the Rust crate is MIT OR Apache-2.0.
//!
//! # Example
//!
//! ```
//! assert_eq!(ara2_bridge_sys::ARA_SOURCE_TAG, "releases/2.3.0");
//! assert_eq!(
//!     ara2_bridge_sys::compatibility::DOCUMENT_CONTROLLER_CALLBACKS.len(),
//!     54
//! );
//! ```
#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::missing_safety_doc)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(target_arch = "arm")]
compile_error!("ARA 2.3 ARM32 bindings are not generated or ABI-proven");

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(target_arch = "x86", target_pointer_width = "32")
)))]
compile_error!(
    "unsupported target architecture: ara2-bridge-sys supports x86_64, AArch64, and i686"
);

/// Canonical upstream repository used to generate these bindings.
pub const ARA_SOURCE_REPOSITORY: &str = "https://github.com/Celemony/ARA_API";

/// Upstream release tag represented by these bindings.
pub const ARA_SOURCE_TAG: &str = "releases/2.3.0";

/// Normative ARA API Git commit represented by these bindings.
pub const ARA_API_COMMIT: &str = "65ec5c43b943a48cb5446f448a0492db6af8534b";

/// ARA SDK superproject Git commit that pins the API input.
pub const ARA_SDK_COMMIT: &str = "a2b1aac1d1d5c4eed387db85a9c0cdb7d460254c";

mod generated;

pub use generated::*;
