//! Bakes the account-service endpoint that [`crate::auth`] reads with
//! `option_env!`.
//!
//! `option_env!` resolves where the code is *compiled*, so this has to live in
//! the crate that owns `auth.rs`. It used to be baked by the studio binary's
//! build script, which was correct while the account layer was `include!`d into
//! that binary; once the module moved here, the studio's `cargo:rustc-env`
//! stopped reaching it and every build would have reported "account service not
//! configured".
//!
//! Source precedence matches the studio's own bake: an explicit build
//! environment value wins (CI / distribution), otherwise the workspace `.env` is
//! read so a plain `cargo build` produces a working binary. No secret is baked —
//! the value is a public base URL, and the sign-in flow ships no client secret.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_AUTH_API_URL");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    // crates/SphereUIComponents → workspace root `.env` (absolute so the
    // working directory never matters).
    let dotenv_path = manifest_dir
        .join("../..")
        .join(".env")
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.join("../../.env"));
    println!("cargo:rerun-if-changed={}", dotenv_path.display());

    if let Some(url) = resolve_auth_api_url(&dotenv_path) {
        println!("cargo:rustc-env=FUTUREBOARD_AUTH_API_URL={url}");
    }
}

/// Absent rather than guessed: `auth::auth_configured()` reports false and the
/// UI disables sign-in, which is better than pointing a real sign-in at a
/// placeholder host.
fn resolve_auth_api_url(dotenv_path: &Path) -> Option<String> {
    std::env::var("FUTUREBOARD_AUTH_API_URL")
        .ok()
        .or_else(|| read_dotenv_value(dotenv_path, "AUTH_API_URL"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_dotenv_value(path: &Path, key: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    contents.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}
