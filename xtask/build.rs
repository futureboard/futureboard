//! Stage the Professional Edition build toolchain when its private source is
//! present.
//!
//! The Steinberg ASIO SDK provisioning lives in the Git-ignored
//! `crates/ExclusiveEdition` checkout, so the public workspace carries no ASIO
//! build tooling. A plain Community checkout simply does not have the file, and
//! `src/toolchain.rs` compiles its stub instead — `cargo xtask` keeps working
//! either way, and the Professional path reports what is missing rather than
//! failing to build.
//!
//! Detection is by file presence, not a Cargo feature, so `cargo xtask …` is
//! spelled identically for both editions.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(professional_toolchain)");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    // xtask -> workspace root -> the private checkout.
    let source_dir = manifest_dir.join("../crates/ExclusiveEdition/xtask");
    let source_path = source_dir.join("toolchain.rs");
    println!("cargo:rerun-if-changed={}", source_path.display());
    // Watch the directory as well as the file. Cargo compares mtimes, and a
    // file that *appears* can carry an older mtime than the last build — a
    // `git checkout` of the private tree, or a move — which the file watch
    // alone would miss, leaving a stale stub compiled in. The directory's mtime
    // does change when an entry is added or removed.
    println!("cargo:rerun-if-changed={}", source_dir.display());

    let Ok(source) = std::fs::read_to_string(&source_path) else {
        return;
    };

    let output_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("futureboard-professional");
    std::fs::create_dir_all(&output_dir)
        .expect("failed to create the Professional toolchain output dir");

    stage(&source, &output_dir.join("toolchain.rs"));
    println!("cargo::rustc-cfg=professional_toolchain");
}

/// Copy the private source, demoting its crate-level `//!` comments to ordinary
/// `//` ones: `include!` cannot accept inner doc comments inside the module
/// that includes it.
fn stage(source: &str, destination: &Path) {
    let staged = source
        .lines()
        .map(|line| {
            line.strip_prefix("//!")
                .map_or_else(|| line.to_owned(), |comment| format!("//{comment}"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(destination, staged)
        .unwrap_or_else(|error| panic!("failed to stage {}: {error}", destination.display()));
}
