//! Embeds APAK verification configuration and Windows resources.

use std::path::{Path, PathBuf};

fn main() {
    bake_apak_verifying_key();

    println!("cargo:rerun-if-changed=windows/apak_tools.rc");
    println!("cargo:rerun-if-changed=windows/apakinstaller.manifest");
    println!("cargo:rerun-if-changed=windows/apak.manifest");
    println!("cargo:rerun-if-changed=windows/makeapak.manifest");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        embed_resource::compile("windows/apak_tools.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}

fn bake_apak_verifying_key() {
    const DEVELOPMENT_SIGNING_KEY: &str =
        "0000000000000000000000000000000000000000000000000000000000000001";

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let verifying_key_path = manifest_dir.join("../../../apak.public.key");
    println!("cargo:rerun-if-env-changed=APAK_VERIFYING_KEY");
    println!("cargo:rerun-if-changed={}", verifying_key_path.display());

    let configured_value = std::env::var("APAK_VERIFYING_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| read_key_file(&verifying_key_path));
    let verifying_value = match configured_value {
        Some(value) => value,
        None if std::env::var("PROFILE").as_deref() == Ok("release") => panic!(
            "release APAK build requires APAK_VERIFYING_KEY from the build environment or {}",
            verifying_key_path.display()
        ),
        None => {
            println!(
                "cargo:warning=APAK_VERIFYING_KEY is not configured; using a development-only trust anchor"
            );
            apak::verifying_key_value_from_signing_key_value(DEVELOPMENT_SIGNING_KEY)
                .expect("development APAK key is valid")
        }
    };
    apak::parse_verifying_key(&verifying_value)
        .unwrap_or_else(|error| panic!("invalid APAK verification key: {error}"));

    println!("cargo:rustc-env=APAK_VERIFYING_KEY={verifying_value}");
}

fn read_key_file(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let value = line
                .strip_prefix("APAK_VERIFYING_KEY=")
                .or_else(|| line.strip_prefix("APAK_SIGNING_KEY="))
                .unwrap_or(line)
                .trim();
            trim_quotes(value).to_string()
        })
}

fn trim_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}
