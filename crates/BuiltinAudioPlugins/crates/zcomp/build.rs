//! Embeds the compiled React editor (`editor/dist`) into the library.
//!
//! A missing `dist/` is not fatal: an empty table is emitted so `cargo test`
//! and DSP-only builds keep working without a prior `bun run build`.

use std::path::PathBuf;

fn main() {
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is always set for build scripts"));
    let dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("editor/dist");

    let options = builtin_ui_embed::generate::GenerateOptions::from_out_dir(dist, out_dir);
    match builtin_ui_embed::generate::generate(&options) {
        Ok(report) if report.dist_present => {
            println!(
                "cargo:rustc-env=ZCOMP_UI_ASSET_COUNT={}",
                report.asset_count
            );
        }
        Ok(_) => {
            println!(
                "cargo:warning=zcomp: editor/dist not built; the plugin editor \
                 will have no UI assets (run `bun run build` in editor/)"
            );
        }
        Err(error) => panic!("zcomp: failed to embed editor UI: {error}"),
    }
}
