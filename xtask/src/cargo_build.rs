//! Drive Cargo and discover the real executable paths.
//!
//! We never assume `target/<profile>/FutureboardNative.exe`. Cargo is run with
//! `--message-format=json-render-diagnostics`; the emitted `compiler-artifact`
//! messages tell us exactly where each binary landed, which keeps working across
//! custom target triples, profiles and per-edition target directories.
//!
//! The application ships more than one executable: at runtime
//! `FutureboardNative.exe` spawns two sidecar processes it resolves *next to
//! itself* — the out-of-process plugin/editor host (`FutureboardPluginHostX64`)
//! and the isolated plugin scanner (`FutureboardPluginScanner`). Both are
//! `[[bin]]` targets of the `sphere-plugin-host` package. The distributable also
//! carries the APAK installer and CLI tools under `bin/`, so all required
//! executables are built in one invocation and discovered from Cargo messages.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use cargo_metadata::{Artifact, Message};

use crate::platform::Edition;
use crate::toolchain;

/// The Futureboard workspace root (xtask lives at `<root>/xtask`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live under the workspace root")
        .to_path_buf()
}

/// Whether the build targets Windows, where ASIO exists at all.
fn target_is_windows(target: Option<&str>) -> bool {
    target
        .map(|triple| triple.contains("windows"))
        .unwrap_or(cfg!(target_os = "windows"))
}

/// The application package and its primary binary.
pub const APP_PACKAGE: &str = "futureboard_native";
pub const APP_BINARY: &str = "FutureboardNative";

/// Package that owns the runtime sidecar executables.
const SIDECAR_PACKAGE: &str = "sphere-plugin-host";
const CEF_HELPER_PACKAGE: &str = "futureboard_cef_helper";
const APAK_PACKAGE: &str = "apakinstaller";
pub const CEF_HELPER_BINARY: &str = "futureboard_cef_helper";

/// Sidecar binaries `FutureboardNative` spawns from its own directory. These are
/// separate `[[bin]]` targets, so building the app package alone does not
/// produce them — they must be requested explicitly.
pub const SIDECAR_BINARIES: &[&str] = &["FutureboardPluginHostX64", "FutureboardPluginScanner"];

/// APAK tools shipped under the staged application's `bin/` directory.
pub const APAK_BINARIES: &[&str] = &["apakinstaller", "apak", "makeapak"];

/// Feature flags that unlock the sidecar `[[bin]]` targets (their
/// `required-features`).
const SIDECAR_FEATURES: &[&str] = &[
    "sphere-plugin-host/plugin-host-bin",
    "sphere-plugin-host/plugin-scanner-bin",
];

/// Result of a successful build: every executable Cargo produced that the
/// package needs.
#[derive(Debug, Clone)]
pub struct BuildOutput {
    /// Absolute path to the primary application binary.
    pub app_executable: PathBuf,
    /// Absolute paths to the runtime sidecar executables, in the order of
    /// [`SIDECAR_BINARIES`].
    pub sidecar_executables: Vec<PathBuf>,
    /// Dedicated CEF subprocess entry point, built only for macOS packages.
    pub cef_helper_executable: Option<PathBuf>,
    /// APAK GUI/CLI executables, in the order of [`APAK_BINARIES`].
    pub apak_executables: Vec<PathBuf>,
}

/// Build the application and its sidecars for the requested profile / target /
/// edition, returning the actual executable paths parsed from Cargo's output.
pub fn build(
    profile: &str,
    target: Option<&str>,
    edition: Edition,
    cef_path: Option<&Path>,
) -> Result<BuildOutput> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let target_is_macos = target
        .map(|triple| triple.ends_with("apple-darwin"))
        .unwrap_or(cfg!(target_os = "macos"));

    let mut command = Command::new(&cargo);
    command
        .arg("build")
        .arg("--message-format=json-render-diagnostics")
        .args(["--package", APP_PACKAGE])
        .args(["--package", SIDECAR_PACKAGE])
        .args(["--package", APAK_PACKAGE])
        .args(["--bin", APP_BINARY])
        .args(["--profile", profile])
        .args(["--target-dir", edition.target_dir()]);

    for bin in SIDECAR_BINARIES {
        command.args(["--bin", bin]);
    }
    for bin in APAK_BINARIES {
        command.args(["--bin", bin]);
    }
    if target_is_macos {
        command
            .args(["--package", CEF_HELPER_PACKAGE])
            .args(["--bin", CEF_HELPER_BINARY]);
    }
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    if let Some(cef_path) = cef_path {
        // Always pass an absolute, target-matched distribution. cef-dll-sys
        // treats relative CEF_PATH values as version roots and may append its
        // own version/platform components.
        command.env("CEF_PATH", cef_path);
    }

    // The Professional build compiles `asio-sys`, which needs the Steinberg SDK
    // and libclang. Resolving them here — rather than letting `asio-sys` fetch
    // the SDK into %TEMP% — is what stops a half-extracted download from being
    // compiled against forever after.
    if edition == Edition::Professional && target_is_windows(target) {
        toolchain::prepare_professional(&workspace_root())?.apply(&mut command);
    }

    // Merge the edition features with the sidecar bin features into one
    // `--features`, so a single build graph unifies shared-dependency features
    // (no rebuild thrash between the app and its sidecars).
    let features = merged_features(edition);
    if !features.is_empty() {
        command.args(["--features", &features]);
    }

    eprintln!(
        "[xtask] building {APP_BINARY} + sidecars + APAK tools (edition={edition}, profile={profile}, target={})",
        target.unwrap_or("<host>")
    );

    // Artifacts arrive as JSON on stdout; let rendered diagnostics/progress
    // stream to the inherited stderr so the developer sees a normal build.
    command.stdout(Stdio::piped()).stderr(Stdio::inherit());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn `{cargo} build`"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("cargo produced no stdout stream"))?;

    let mut executables: BTreeMap<String, PathBuf> = BTreeMap::new();
    for message in Message::parse_stream(BufReader::new(stdout)) {
        let message = message.context("failed to parse a cargo JSON message")?;
        if let Message::CompilerArtifact(artifact) = message {
            if let Some((name, path)) = wanted_executable(&artifact) {
                executables.insert(name, path);
            }
        }
    }

    let status = child.wait().context("failed to wait on cargo build")?;
    if !status.success() {
        bail!("cargo build failed with {status}");
    }

    let app_executable = executables.remove(APP_BINARY).ok_or_else(|| {
        anyhow!(
            "cargo build succeeded but emitted no executable artifact for `{APP_BINARY}`; \
             is the `{APP_PACKAGE}` package still producing a `[[bin]]` named `{APP_BINARY}`?"
        )
    })?;

    let mut sidecar_executables = Vec::with_capacity(SIDECAR_BINARIES.len());
    for bin in SIDECAR_BINARIES {
        let path = executables.remove(*bin).ok_or_else(|| {
            anyhow!(
                "cargo build succeeded but emitted no executable artifact for sidecar `{bin}`; \
                 the app spawns it at runtime and it must ship in the package"
            )
        })?;
        sidecar_executables.push(path);
    }
    let cef_helper_executable = if target_is_macos {
        Some(executables.remove(CEF_HELPER_BINARY).ok_or_else(|| {
            anyhow!(
                "cargo build succeeded but emitted no executable artifact for macOS CEF helper \
                 `{CEF_HELPER_BINARY}`"
            )
        })?)
    } else {
        None
    };
    let mut apak_executables = Vec::with_capacity(APAK_BINARIES.len());
    for bin in APAK_BINARIES {
        let path = executables.remove(*bin).ok_or_else(|| {
            anyhow!(
                "cargo build succeeded but emitted no executable artifact for APAK tool `{bin}`"
            )
        })?;
        apak_executables.push(path);
    }

    Ok(BuildOutput {
        app_executable,
        sidecar_executables,
        cef_helper_executable,
        apak_executables,
    })
}

/// Comma-joined `--features` value combining edition features (if any) with the
/// sidecar bin features.
fn merged_features(edition: Edition) -> String {
    let mut features: Vec<&str> = Vec::new();
    if let Some(edition_features) = edition.cargo_features() {
        features.push(edition_features);
    }
    features.extend_from_slice(SIDECAR_FEATURES);
    features.join(",")
}

/// Return `(binary_name, executable_path)` if this artifact is one of the
/// executables we asked Cargo to build.
fn wanted_executable(artifact: &Artifact) -> Option<(String, PathBuf)> {
    let name = artifact.target.name.as_str();
    let is_wanted = (name == APP_BINARY
        || name == CEF_HELPER_BINARY
        || SIDECAR_BINARIES.contains(&name)
        || APAK_BINARIES.contains(&name))
        && artifact
            .target
            .kind
            .iter()
            .any(|kind| kind.as_str() == "bin");
    if !is_wanted {
        return None;
    }
    artifact
        .executable
        .as_ref()
        .map(|path| (name.to_string(), PathBuf::from(path.as_std_path())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn community_features_are_sidecar_only() {
        assert_eq!(
            merged_features(Edition::Community),
            "sphere-plugin-host/plugin-host-bin,sphere-plugin-host/plugin-scanner-bin"
        );
    }

    #[test]
    fn professional_features_prepend_edition_flags() {
        assert_eq!(
            merged_features(Edition::Professional),
            "futureboard_native/professional,sphere_directaudioengine/asio,\
sphere-plugin-host/plugin-host-bin,sphere-plugin-host/plugin-scanner-bin"
        );
    }
}
