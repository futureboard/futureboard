//! GitHub Releases based application update discovery and staging.
//!
//! Network and filesystem work in this module is blocking by design. Callers
//! must run it on GPUI's background executor, never on the UI or audio thread.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sphere_ui_components::settings::UpdateChannel;
use sphere_ui_components::update_service::{
    DownloadProgressFn, InstallOutcome, UpdateCandidate, UpdateProvider,
};

const RELEASES_API: &str = "https://api.github.com/repos/futureboard/Futureboard/releases";
const USER_AGENT: &str = "Futureboard-Studio-Updater";

#[derive(Debug, Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub version: Version,
    pub channel: UpdateChannel,
    asset: GithubAsset,
}

fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, String> {
    let response = ureq::get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("GitHub API request failed: {error}"))?;
    serde_json::from_reader(response.into_body().into_reader())
        .map_err(|error| format!("GitHub API returned invalid JSON: {error}"))
}

/// Installers are named `Futureboard.Studio-<version>-<platform>.<ext>`, with
/// `<version>` written straight from `version.json` by the release workflow —
/// which also verifies these exact shapes before it uploads. That makes the
/// filename a machine-written copy of the version, which a release title is not.
const ASSET_NAME_PREFIX: &str = "futureboard.studio-";

/// The platform tails that follow the version, lowercased for matching.
///
/// Spelling them out is what makes the split reliable: semver's prerelease
/// grammar accepts hyphens inside an identifier, so `2026.8.9-beta1-windows`
/// parses happily as a version with the prerelease `beta1-windows`. The tail
/// cannot be inferred by scanning — it has to be known.
const ASSET_PLATFORM_SUFFIXES: &[&str] = &[
    "-windows-x86_64-setup.exe",
    "-macos-universal.dmg",
    "-macos-arm64.dmg",
    "-macos-x86_64.dmg",
    "-x86_64.appimage",
    // Releases predating architecture-tagged DMG names.
    "-macos.dmg",
];

fn version_from_asset_name(name: &str) -> Option<Version> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix(ASSET_NAME_PREFIX)?;
    let version = ASSET_PLATFORM_SUFFIXES
        .iter()
        .find_map(|suffix| rest.strip_suffix(suffix))?;
    Version::parse(version).ok()
}

/// The version a release offers, or `None` if it cannot be established — such a
/// release is skipped entirely.
///
/// A version tag answers this outright. A *moving* tag cannot: `nightly` is
/// republished in place every day, and 2026.8.9-beta1 went out on `beta`, where
/// `Version::parse("beta")` fails and the whole release became invisible to the
/// updater.
///
/// The fallback reads the asset this machine would actually install, so the
/// version offered is the version that arrives — and a release carrying a
/// leftover asset from an older build cannot advertise that older version.
///
/// Asset names are preferred over the release name because a title is prose: for
/// this very release it reads "Futureboard Studio 2026.8.9 Beta 1", whose only
/// parseable token is `2026.8.9` — which *outranks* the `2026.8.9-beta1` inside
/// the installer, and would offer every user a permanent update to the build
/// they are already running.
fn release_version(release: &GithubRelease) -> Option<Version> {
    let tag = release.tag_name.trim().trim_start_matches('v');
    if let Ok(version) = Version::parse(tag) {
        return Some(version);
    }

    platform_asset(release)
        .and_then(|asset| version_from_asset_name(&asset.name))
        .or_else(|| version_from_release_name(release))
}

/// Last resort for a moving tag whose assets predate the naming convention.
fn version_from_release_name(release: &GithubRelease) -> Option<Version> {
    release
        .name
        .as_deref()?
        .split_whitespace()
        .find_map(|word| {
            let candidate = word
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '.' && character != '-'
                })
                .trim_start_matches('v');
            Version::parse(candidate).ok()
        })
}

fn release_matches_channel(release: &GithubRelease, channel: UpdateChannel) -> bool {
    if release.draft {
        return false;
    }
    let Some(version) = release_version(release) else {
        return false;
    };
    match channel {
        UpdateChannel::Nightly => release.tag_name == "nightly",
        // Beta users also receive stable releases. Trust the semantic version
        // here because older repository releases were occasionally published
        // with an incorrect GitHub `prerelease` flag.
        UpdateChannel::Beta => release.tag_name != "nightly",
        UpdateChannel::Stable => {
            release.tag_name != "nightly" && !release.prerelease && version.pre.is_empty()
        }
    }
}

/// How well an asset name fits this build, lower being better. `None` means the
/// asset is not installable here at all.
///
/// Only macOS has more than one candidate: releases carry a universal DMG plus
/// a per-architecture one. A plain `find` would hand whichever came first —
/// possibly the Intel image to an Apple Silicon machine — so the match is
/// ranked instead of taken greedily.
fn asset_rank(name: &str) -> Option<u8> {
    #[cfg(target_os = "windows")]
    {
        return (name.ends_with(".exe") && name.contains("setup")).then_some(0);
    }
    #[cfg(target_os = "linux")]
    {
        return (name.ends_with(".appimage") && name.contains("x86_64")).then_some(0);
    }
    #[cfg(target_os = "macos")]
    {
        if !name.ends_with(".dmg") || !name.contains("macos") {
            return None;
        }
        #[cfg(target_arch = "aarch64")]
        let (native, foreign) = ("macos-arm64", "macos-x86_64");
        #[cfg(target_arch = "x86_64")]
        let (native, foreign) = ("macos-x86_64", "macos-arm64");
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        let (native, foreign) = ("macos-universal", "\0");

        if name.contains(native) {
            return Some(0);
        }
        if name.contains("macos-universal") {
            return Some(1);
        }
        if name.contains(foreign) {
            // Running the other architecture's image would mean installing a
            // Rosetta-only (or unrunnable) build over a native one.
            return None;
        }
        // Releases published before the DMG name carried an architecture.
        return Some(2);
    }
    #[allow(unreachable_code)]
    {
        let _ = name;
        None
    }
}

fn platform_asset(release: &GithubRelease) -> Option<GithubAsset> {
    release
        .assets
        .iter()
        .filter_map(|asset| asset_rank(&asset.name.to_ascii_lowercase()).map(|rank| (rank, asset)))
        // Ties keep the first asset, matching the previous `find` behaviour.
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, asset)| asset.clone())
}

fn choose_release(
    releases: Vec<GithubRelease>,
    channel: UpdateChannel,
    current: &Version,
) -> Option<AvailableUpdate> {
    releases
        .into_iter()
        .filter(|release| release_matches_channel(release, channel))
        .filter_map(|release| {
            let version = release_version(&release)?;
            let asset = platform_asset(&release)?;
            let available = match channel {
                // A user explicitly opting into nightly may move from the same
                // stable base to its prerelease build.
                UpdateChannel::Nightly => version != *current,
                UpdateChannel::Beta | UpdateChannel::Stable => version > *current,
            };
            available.then(|| AvailableUpdate {
                version,
                channel,
                asset,
            })
        })
        .max_by(|left, right| left.version.cmp(&right.version))
}

pub fn check_for_update(
    channel: UpdateChannel,
    current_version: &str,
) -> Result<Option<AvailableUpdate>, String> {
    let current = Version::parse(current_version)
        .map_err(|error| format!("invalid installed version {current_version:?}: {error}"))?;
    let url = match channel {
        UpdateChannel::Nightly => format!("{RELEASES_API}/tags/nightly"),
        UpdateChannel::Beta | UpdateChannel::Stable => format!("{RELEASES_API}?per_page=30"),
    };
    let releases = if channel == UpdateChannel::Nightly {
        vec![get_json::<GithubRelease>(&url)?]
    } else {
        get_json::<Vec<GithubRelease>>(&url)?
    };
    Ok(choose_release(releases, channel, &current))
}

pub fn download_update(
    update: &AvailableUpdate,
    cache_root: &Path,
    progress: &dyn Fn(u64, u64),
) -> Result<PathBuf, String> {
    let file_name = Path::new(&update.asset.name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "release asset has an invalid filename".to_string())?;
    let update_dir = cache_root.join("Updates").join(update.version.to_string());
    fs::create_dir_all(&update_dir)
        .map_err(|error| format!("failed to create update cache: {error}"))?;
    let destination = update_dir.join(file_name);
    let partial = destination.with_extension("part");

    let response = ureq::get(&update.asset.browser_download_url)
        .header("Accept", "application/octet-stream")
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("update download failed: {error}"))?;
    let mut reader = response.into_body().into_reader();
    let mut writer = BufWriter::new(
        File::create(&partial).map_err(|error| format!("failed to create update file: {error}"))?,
    );
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                drop(writer);
                let _ = fs::remove_file(&partial);
                return Err(format!("failed to download update: {error}"));
            }
        };
        if let Err(error) = writer.write_all(&buffer[..count]) {
            drop(writer);
            let _ = fs::remove_file(&partial);
            return Err(format!("failed to save update: {error}"));
        }
        hasher.update(&buffer[..count]);
        downloaded = downloaded.saturating_add(count as u64);
        progress(downloaded, update.asset.size);
    }
    if let Err(error) = writer.flush() {
        drop(writer);
        let _ = fs::remove_file(&partial);
        return Err(format!("failed to flush update: {error}"));
    }
    drop(writer);
    if downloaded != update.asset.size {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "download size mismatch: expected {} bytes, received {downloaded}",
            update.asset.size
        ));
    }
    let actual_digest = format!("sha256:{:x}", hasher.finalize());
    if update
        .asset
        .digest
        .as_deref()
        .is_none_or(|expected| !expected.eq_ignore_ascii_case(&actual_digest))
    {
        let _ = fs::remove_file(&partial);
        return Err("GitHub did not provide a matching SHA-256 digest for the update".to_string());
    }
    if destination.exists() {
        fs::remove_file(&destination)
            .map_err(|error| format!("failed to replace cached update: {error}"))?;
    }
    fs::rename(&partial, &destination)
        .map_err(|error| format!("failed to finalize update: {error}"))?;
    Ok(destination)
}

/// Hand the staged download to the platform installer.
///
/// Every platform path either replaces this installation in place — which needs
/// the process to exit first — or hands the file to the platform and lets the
/// user drive it.
pub fn install_update(staged: &Path, cache_root: &Path) -> Result<InstallOutcome, String> {
    #[cfg(target_os = "windows")]
    return install_windows(staged, cache_root);
    #[cfg(target_os = "macos")]
    return install_macos(staged, cache_root);
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    return install_linux(staged, cache_root);
}

/// Windows: the release asset is the Inno Setup installer. Run it with Inno's
/// command-line switches so the update applies to the *current* installation
/// (same directory, same scope) instead of opening a fresh wizard.
///
/// Switch reference (Inno Setup 6):
///   `/SILENT`             progress window only, no wizard pages
///   `/SP-`                skip the "This will install..." prompt
///   `/SUPPRESSMSGBOXES`   accept the default answer for setup message boxes
///   `/NORESTART`          never reboot the machine on our behalf
///   `/CLOSEAPPLICATIONS`  let Restart Manager close remaining app processes
///   `/DIR`, `/ALLUSERS` / `/CURRENTUSER`  keep the existing install location
///
/// `installer.iss` declares `PrivilegesRequired=lowest` with
/// `PrivilegesRequiredOverridesAllowed=dialog commandline`, so `/ALLUSERS`
/// is the supported way to update a machine-wide install (Windows shows the
/// elevation prompt).
#[cfg(target_os = "windows")]
fn install_windows(staged: &Path, cache_root: &Path) -> Result<InstallOutcome, String> {
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let log_path = cache_root.join("Updates").join("install.log");

    let mut command = Command::new(staged);
    command
        .arg("/SILENT")
        .arg("/SP-")
        .arg("/SUPPRESSMSGBOXES")
        .arg("/NORESTART")
        .arg("/CLOSEAPPLICATIONS")
        .arg(format!("/LOG={}", log_path.display()));
    if let Some(dir) = install_dir.as_deref() {
        command.arg(format!("/DIR={}", dir.display()));
        command.arg(if is_machine_wide(dir) {
            "/ALLUSERS"
        } else {
            "/CURRENTUSER"
        });
    }
    command
        .spawn()
        .map_err(|error| format!("failed to start the installer: {error}"))?;
    Ok(InstallOutcome::QuitRequired)
}

/// True when the install directory sits under a Program Files root, which is
/// what an all-users Inno install produces.
#[cfg(target_os = "windows")]
fn is_machine_wide(install_dir: &Path) -> bool {
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|root| install_dir.starts_with(PathBuf::from(root)))
}

/// macOS: the asset is a DMG. Mount it, copy the bundle out, unmount, then let
/// a detached shell script swap the running bundle once this process exits.
#[cfg(target_os = "macos")]
fn install_macos(staged: &Path, cache_root: &Path) -> Result<InstallOutcome, String> {
    let Some(target) = current_macos_bundle() else {
        // Not running from a bundle (cargo run, CI). Reveal the DMG instead of
        // guessing an install location.
        Command::new("open")
            .arg(staged)
            .spawn()
            .map_err(|error| format!("failed to open the disk image: {error}"))?;
        return Ok(InstallOutcome::Handoff);
    };

    let work_dir = cache_root.join("Updates").join("macos");
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir)
        .map_err(|error| format!("failed to create the update workspace: {error}"))?;
    let mount_point = work_dir.join("mount");
    fs::create_dir_all(&mount_point)
        .map_err(|error| format!("failed to create the disk image mount point: {error}"))?;

    run_tool(
        "hdiutil",
        &[
            "attach".as_ref(),
            staged.as_os_str(),
            "-nobrowse".as_ref(),
            "-quiet".as_ref(),
            "-mountpoint".as_ref(),
            mount_point.as_os_str(),
        ],
    )?;

    let mounted_bundle = fs::read_dir(&mount_point)
        .map_err(|error| format!("failed to read the disk image: {error}"))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "app"));
    let mounted_bundle = match mounted_bundle {
        Some(bundle) => bundle,
        None => {
            let _ = run_tool(
                "hdiutil",
                &[
                    "detach".as_ref(),
                    mount_point.as_os_str(),
                    "-quiet".as_ref(),
                ],
            );
            return Err("the disk image does not contain an application bundle".to_string());
        }
    };

    let bundle_name = mounted_bundle
        .file_name()
        .ok_or_else(|| "the disk image bundle has no name".to_string())?;
    let staged_bundle = work_dir.join(bundle_name);
    let copy_result = run_tool(
        "ditto",
        &[mounted_bundle.as_os_str(), staged_bundle.as_os_str()],
    );
    let _ = run_tool(
        "hdiutil",
        &[
            "detach".as_ref(),
            mount_point.as_os_str(),
            "-quiet".as_ref(),
        ],
    );
    copy_result?;

    let script_path = work_dir.join("swap-bundle.sh");
    let script = format!(
        "#!/bin/sh\n\
         while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
         /bin/rm -rf \"{target}\"\n\
         /usr/bin/ditto \"{staged}\" \"{target}\" || exit 1\n\
         /usr/bin/xattr -dr com.apple.quarantine \"{target}\" 2>/dev/null\n\
         /bin/rm -rf \"{staged}\"\n\
         /usr/bin/open \"{target}\"\n",
        pid = std::process::id(),
        target = target.display(),
        staged = staged_bundle.display(),
    );
    fs::write(&script_path, script)
        .map_err(|error| format!("failed to write the update script: {error}"))?;
    spawn_detached_script(&script_path)?;
    Ok(InstallOutcome::QuitRequired)
}

/// `/Applications/Futureboard Studio.app` for
/// `/Applications/Futureboard Studio.app/Contents/MacOS/FutureboardNative`.
#[cfg(target_os = "macos")]
fn current_macos_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

/// Linux: the asset is an AppImage. When Studio is itself running from an
/// AppImage, replace that file and relaunch it after this process exits;
/// otherwise just run the downloaded image.
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn install_linux(staged: &Path, cache_root: &Path) -> Result<InstallOutcome, String> {
    make_executable(staged)?;

    let Some(current) = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    else {
        Command::new(staged)
            .spawn()
            .map_err(|error| format!("failed to start the AppImage: {error}"))?;
        return Ok(InstallOutcome::Handoff);
    };

    // Replace via a sibling temp file + rename so the swap is atomic and stays
    // on the same filesystem as the running image.
    let replacement = current.with_extension("AppImage.update");
    fs::copy(staged, &replacement)
        .map_err(|error| format!("failed to stage the AppImage update: {error}"))?;
    make_executable(&replacement)?;
    fs::rename(&replacement, &current).map_err(|error| {
        let _ = fs::remove_file(&replacement);
        format!("failed to install the AppImage update: {error}")
    })?;

    let work_dir = cache_root.join("Updates");
    fs::create_dir_all(&work_dir)
        .map_err(|error| format!("failed to create the update workspace: {error}"))?;
    let script_path = work_dir.join("relaunch-appimage.sh");
    let script = format!(
        "#!/bin/sh\n\
         while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
         exec \"{target}\"\n",
        pid = std::process::id(),
        target = current.display(),
    );
    fs::write(&script_path, script)
        .map_err(|error| format!("failed to write the relaunch script: {error}"))?;
    spawn_detached_script(&script_path)?;
    Ok(InstallOutcome::QuitRequired)
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("failed to inspect the AppImage: {error}"))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("failed to make the AppImage executable: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn run_tool(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(if detail.is_empty() {
        format!("{program} failed with status {}", output.status)
    } else {
        format!("{program} failed: {detail}")
    })
}

/// Run the swap/relaunch helper in its own session so it outlives this process.
#[cfg(not(target_os = "windows"))]
fn spawn_detached_script(script: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(script)
        .map_err(|error| format!("failed to inspect the update script: {error}"))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(script, permissions)
        .map_err(|error| format!("failed to prepare the update script: {error}"))?;

    // The child is reparented to init once this process exits, so no extra
    // detach step is needed; only the standard streams must be released.
    Command::new("/bin/sh")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("failed to start the update script: {error}"))
}

/// Update provider registered with `sphere_ui_components` at boot so the
/// Software Update dialog can drive this module.
pub struct GithubUpdateProvider;

impl UpdateProvider for GithubUpdateProvider {
    fn current_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn check(&self, channel: UpdateChannel) -> Result<Option<UpdateCandidate>, String> {
        Ok(check_for_update(channel, env!("CARGO_PKG_VERSION"))?.map(candidate_from))
    }

    fn download(
        &self,
        candidate: &UpdateCandidate,
        progress: DownloadProgressFn<'_>,
    ) -> Result<PathBuf, String> {
        let update = candidate
            .payload
            .downcast_ref::<AvailableUpdate>()
            .ok_or_else(|| "update candidate came from a different provider".to_string())?;
        download_update(update, &cache_root(), &|received, total| {
            progress(received, total)
        })
    }

    fn install(&self, staged: &Path) -> Result<InstallOutcome, String> {
        install_update(staged, &cache_root())
    }
}

/// Root of the update cache. Shared with the Professional transport, which
/// stages its own downloads in the same place before reusing
/// [`install_update`].
pub(crate) fn cache_root() -> PathBuf {
    sphere_ui_components::paths::FutureboardPaths::resolve().app_cache
}

pub fn candidate_from(update: AvailableUpdate) -> UpdateCandidate {
    UpdateCandidate {
        version: update.version.to_string(),
        channel: update.channel,
        asset_name: update.asset.name.clone(),
        asset_size: update.asset.size,
        release_url: None,
        payload: Arc::new(update),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, name: &str, prerelease: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_string(),
            name: Some(name.to_string()),
            draft: false,
            prerelease,
            assets: vec![GithubAsset {
                name: if cfg!(target_os = "windows") {
                    "FutureboardStudioSetup.exe"
                } else if cfg!(target_os = "macos") {
                    "Futureboard.Studio-2026.8.3-macos.dmg"
                } else {
                    "Futureboard.Studio-2026.8.3-x86_64.AppImage"
                }
                .to_string(),
                browser_download_url: "https://example.test/download".to_string(),
                size: 1,
                digest: Some("sha256:test".to_string()),
            }],
        }
    }

    /// A release now carries a universal DMG *and* both single-architecture
    /// ones. The native image must win, the universal image must be the
    /// fallback, and the other architecture's image must never be chosen.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prefers_native_then_universal_and_never_the_other_arch() {
        let dmg = |name: &str| GithubAsset {
            name: name.to_string(),
            browser_download_url: "https://example.test/download".to_string(),
            size: 1,
            digest: None,
        };
        let with = |assets: Vec<GithubAsset>| GithubRelease {
            tag_name: "2026.8.8".to_string(),
            name: Some("Release".to_string()),
            draft: false,
            prerelease: false,
            assets,
        };

        let native = if cfg!(target_arch = "aarch64") {
            "Futureboard.Studio-2026.8.8-macos-arm64.dmg"
        } else {
            "Futureboard.Studio-2026.8.8-macos-x86_64.dmg"
        };
        let foreign = if cfg!(target_arch = "aarch64") {
            "Futureboard.Studio-2026.8.8-macos-x86_64.dmg"
        } else {
            "Futureboard.Studio-2026.8.8-macos-arm64.dmg"
        };
        let universal = "Futureboard.Studio-2026.8.8-macos-universal.dmg";

        // Asset order must not decide the outcome.
        let all = with(vec![dmg(foreign), dmg(universal), dmg(native)]);
        assert_eq!(platform_asset(&all).unwrap().name, native);

        let no_native = with(vec![dmg(foreign), dmg(universal)]);
        assert_eq!(platform_asset(&no_native).unwrap().name, universal);

        // The wrong architecture alone is not an update.
        assert!(platform_asset(&with(vec![dmg(foreign)])).is_none());

        // Releases predating architecture-tagged names still resolve.
        let legacy = with(vec![dmg("Futureboard.Studio-2026.8.3-macos.dmg")]);
        assert_eq!(
            platform_asset(&legacy).unwrap().name,
            "Futureboard.Studio-2026.8.3-macos.dmg"
        );
    }

    /// An asset named the way the release workflow names them, for the platform
    /// this test binary is running on.
    fn versioned_release(tag: &str, name: &str, version: &str) -> GithubRelease {
        let asset = if cfg!(target_os = "windows") {
            format!("Futureboard.Studio-{version}-windows-x86_64-Setup.exe")
        } else if cfg!(target_os = "macos") {
            format!("Futureboard.Studio-{version}-macos-universal.dmg")
        } else {
            format!("Futureboard.Studio-{version}-x86_64.AppImage")
        };
        GithubRelease {
            tag_name: tag.to_string(),
            name: Some(name.to_string()),
            draft: false,
            prerelease: true,
            assets: vec![GithubAsset {
                name: asset,
                browser_download_url: "https://example.test/download".to_string(),
                size: 1,
                digest: Some("sha256:test".to_string()),
            }],
        }
    }

    /// A moving tag carries no version of its own, so it comes from the asset
    /// that would actually be installed.
    #[test]
    fn moving_tag_version_comes_from_the_asset_name() {
        let nightly = versioned_release("nightly", "Nightly v2026.8.3-nightly", "2026.8.3-nightly");
        assert_eq!(
            release_version(&nightly).unwrap(),
            Version::parse("2026.8.3-nightly").unwrap()
        );

        // 2026.8.9-beta1 shipped on `beta`. Before the asset fallback existed
        // this release resolved to `None` and was skipped by every channel.
        let beta = versioned_release(
            "beta",
            "Futureboard Studio 2026.8.9 Beta 1 — Hotfix",
            "2026.8.9-beta1",
        );
        assert_eq!(
            release_version(&beta).unwrap(),
            Version::parse("2026.8.9-beta1").unwrap()
        );
        assert!(release_matches_channel(&beta, UpdateChannel::Beta));
    }

    /// The release title is prose. "…2026.8.9 Beta 1" offers `2026.8.9`, which
    /// sorts *above* the `2026.8.9-beta1` in the installer, so trusting it would
    /// offer a user on the hotfix a permanent update to their own build.
    #[test]
    fn asset_name_outranks_a_prose_release_title() {
        let release = versioned_release(
            "beta",
            "Futureboard Studio 2026.8.9 Beta 1 — Hotfix",
            "2026.8.9-beta1",
        );
        let current = Version::parse("2026.8.9-beta1").unwrap();
        assert!(choose_release(vec![release], UpdateChannel::Beta, &current).is_none());
    }

    /// A version tag still wins outright, and a release whose version cannot be
    /// established anywhere is skipped rather than guessed at.
    #[test]
    fn version_tag_wins_and_an_unresolvable_release_is_skipped() {
        let tagged = versioned_release("v2026.8.9-beta1", "Anything at all", "2026.8.9-beta1");
        assert_eq!(
            release_version(&tagged).unwrap(),
            Version::parse("2026.8.9-beta1").unwrap()
        );

        let mut opaque = versioned_release("beta", "Futureboard Studio", "2026.8.9-beta1");
        opaque.assets[0].name = "installer.bin".to_string();
        assert!(release_version(&opaque).is_none());
    }

    #[test]
    fn stable_does_not_accept_mislabelled_prerelease_tag() {
        let candidate = release("2026.8.2-beta.1", "Beta", false);
        assert!(!release_matches_channel(&candidate, UpdateChannel::Stable));
        assert!(release_matches_channel(&candidate, UpdateChannel::Beta));
    }

    #[test]
    fn channel_selection_uses_newest_eligible_release() {
        let releases = vec![
            release("v2026.8.2", "Stable", false),
            release("v2026.8.3-beta.1", "Beta", true),
        ];
        let current = Version::parse("2026.8.1").unwrap();
        let stable = choose_release(releases.clone(), UpdateChannel::Stable, &current).unwrap();
        let beta = choose_release(releases, UpdateChannel::Beta, &current).unwrap();
        assert_eq!(stable.version, Version::parse("2026.8.2").unwrap());
        assert_eq!(beta.version, Version::parse("2026.8.3-beta.1").unwrap());
    }
}
