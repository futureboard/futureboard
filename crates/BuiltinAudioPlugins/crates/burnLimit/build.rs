//! Builds and embeds BurnLimit's Svelte editor.
//!
//! The frontend is compiled into this Cargo invocation's `OUT_DIR`, not the
//! source tree's ignored `editorui/dist`. This makes a clean GitHub Actions
//! checkout produce the same embedded editor as a developer machine and avoids
//! races between target/profile builds sharing one `dist` directory.

use std::{
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-env-changed=BUN");
    println!("cargo:rerun-if-changed=editorui/index.html");
    println!("cargo:rerun-if-changed=editorui/package.json");
    println!("cargo:rerun-if-changed=editorui/svelte.config.js");
    println!("cargo:rerun-if-changed=editorui/tsconfig.json");
    println!("cargo:rerun-if-changed=editorui/vite.config.ts");
    println!("cargo:rerun-if-changed=editorui/src");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .ancestors()
        .nth(4)
        .expect("burnLimit must remain inside crates/BuiltinAudioPlugins/crates");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("bun.lock").display()
    );

    let editor_dir = manifest_dir.join("editorui");
    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is always set for build scripts"));
    let editor_dist = out_dir.join("burnlimit-editor-dist");
    let bun = env::var_os("BUN").unwrap_or_else(default_bun_executable);

    run(
        &bun,
        &editor_dir,
        [OsStr::new("install"), OsStr::new("--frozen-lockfile")],
        "install BurnLimit editor dependencies",
    );
    run(
        &bun,
        &editor_dir,
        [
            OsStr::new("run"),
            OsStr::new("build"),
            OsStr::new("--"),
            OsStr::new("--outDir"),
            editor_dist.as_os_str(),
            OsStr::new("--emptyOutDir"),
        ],
        "build the BurnLimit editor",
    );

    let options = builtin_ui_embed::generate::GenerateOptions::from_out_dir(editor_dist, out_dir);
    let report = builtin_ui_embed::generate::generate(&options)
        .unwrap_or_else(|error| panic!("burnlimit: failed to embed editor UI: {error}"));
    assert!(
        report.dist_present && report.asset_count > 0,
        "burnlimit: frontend build completed without producing embeddable assets"
    );
    println!(
        "cargo:rustc-env=BURNLIMIT_UI_ASSET_COUNT={}",
        report.asset_count
    );
}

fn default_bun_executable() -> OsString {
    if cfg!(windows) {
        OsString::from("bun.exe")
    } else {
        OsString::from("bun")
    }
}

fn run<I, S>(program: &OsStr, cwd: &Path, args: I, action: &str)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "burnlimit: could not {action} with `{}`: {error}. \
                 Install Bun or set BUN to its executable path",
                program.to_string_lossy()
            )
        });
    assert!(
        status.success(),
        "burnlimit: failed to {action} (command exited with {status})"
    );
}
