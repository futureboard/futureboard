//! Embeds the compiled Svelte editor (`editorui/dist`) into the library.
//!
//! The generator enumerates `dist/` every build, so Vite's content-hashed
//! filenames are never hard-coded. A missing or unbuilt `dist/` is not an
//! error: an empty table is emitted and the crate still compiles, which keeps
//! `cargo test -p transient` working for anyone who has not run
//! `bun run build` in `editorui/`.

use std::path::PathBuf;

fn main() {
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is always set for build scripts"));
    let dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("editorui/dist");

    let options = builtin_ui_embed::generate::GenerateOptions::from_out_dir(dist, out_dir);
    match builtin_ui_embed::generate::generate(&options) {
        Ok(report) if report.dist_present => {
            println!(
                "cargo:rustc-env=TRANSIENT_UI_ASSET_COUNT={}",
                report.asset_count
            );
        }
        Ok(_) => {
            println!(
                "cargo:warning=transient: editorui/dist not built; the plugin editor \
                 will have no UI assets (run `bun run build` in editorui/)"
            );
        }
        Err(error) => panic!("transient: failed to embed editor UI: {error}"),
    }
}
