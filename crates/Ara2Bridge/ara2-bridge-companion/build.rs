//! Builds the C++ companion shims for the enabled ARA companion APIs.
//!
//! # Difference from upstream
//!
//! Upstream resolves both SDKs from environment variables and then verifies each
//! checkout against a locked commit, tree, and clean status with `git`. That
//! exists because upstream ships as a published crate and cannot know what a
//! consumer points it at.
//!
//! Here the SDKs are submodules of this repository — `external/ARA_SDK` and
//! `external/vst3sdk` — pinned by the parent repo, which is the same guarantee
//! by a stronger mechanism. Re-verifying them with `git` would also fail
//! outright, since this vendored tree has no `.git` of its own. Environment
//! overrides are still honoured for out-of-tree SDK experiments.

use std::env;
use std::path::PathBuf;

/// Repository root, three levels up from `crates/Ara2Bridge/ara2-bridge-companion`.
fn repository_root() -> PathBuf {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    manifest
        .ancestors()
        .nth(3)
        .expect("vendored crate sits three levels below the repository root")
        .to_path_buf()
}

/// Resolves an SDK root from `variable`, falling back to the pinned submodule.
fn sdk_root(variable: &str, submodule: &str, marker: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed={variable}");
    let path = env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join(submodule));
    if !path.join(marker).is_file() {
        panic!(
            "{variable} / {submodule} does not look like an SDK checkout: {} is missing.\n\
             Run `git submodule update --init --recursive {submodule}`.",
            path.join(marker).display()
        );
    }
    path
}

fn build_vst3() {
    if env::var_os("CARGO_FEATURE_VST3").is_none() {
        return;
    }
    let vst3 = sdk_root(
        "ARA_VST3_SDK_DIR",
        "external/vst3sdk",
        "pluginterfaces/base/funknown.h",
    );
    let ara = sdk_root("ARA_SDK_DIR", "external/ARA_SDK", "ARA_API/ARAInterface.h");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("native/vst3/ara_vst3_shim.cpp")
        .include("native/vst3")
        .include(&vst3)
        .include(ara.join("ARA_API"))
        .warnings(true)
        .flag_if_supported("-fvisibility=hidden");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag_if_supported("/EHsc");
    }
    build.compile("ara2_vst3_shim");

    println!("cargo:rerun-if-changed=native/vst3/ara_vst3_shim.hpp");
    println!("cargo:rerun-if-changed=native/vst3/ara_vst3_shim.cpp");
    println!(
        "cargo:rerun-if-changed={}",
        ara.join("ARA_API/ARAVST3.h").display()
    );
}

fn build_audio_unit() {
    if env::var_os("CARGO_FEATURE_AUDIO_UNIT_V2").is_none() {
        return;
    }
    if env::var("CARGO_CFG_TARGET_VENDOR").as_deref() != Ok("apple") {
        panic!("audio-unit-v2 is supported only on Apple targets");
    }
    let audio_unit = sdk_root(
        "ARA_AUDIO_UNIT_SDK_DIR",
        "external/AudioUnitSDK",
        "Source/AudioUnitSDK.h",
    );
    let ara = sdk_root("ARA_SDK_DIR", "external/ARA_SDK", "ARA_API/ARAInterface.h");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("native/audio_unit/ara_au_shim.mm")
        .include("native/audio_unit")
        .include(audio_unit.join("Source"))
        .include(ara.join("ARA_API"))
        .warnings(true)
        .compile("ara2_audio_unit_shim");

    println!("cargo:rustc-link-lib=framework=AudioToolbox");
    println!("cargo:rerun-if-changed=native/audio_unit/ara_au_shim.h");
    println!("cargo:rerun-if-changed=native/audio_unit/ara_au_shim.mm");
    println!(
        "cargo:rerun-if-changed={}",
        ara.join("ARA_API/ARAAudioUnit.h").display()
    );
}

fn main() {
    build_vst3();
    build_audio_unit();
}
