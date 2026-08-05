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

fn release_version(release: &GithubRelease) -> Option<Version> {
    let tag = release.tag_name.trim().trim_start_matches('v');
    if tag != "nightly" {
        return Version::parse(tag).ok();
    }

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

fn platform_asset(release: &GithubRelease) -> Option<GithubAsset> {
    release
        .assets
        .iter()
        .find(|asset| {
            let name = asset.name.to_ascii_lowercase();
            #[cfg(target_os = "windows")]
            return name.ends_with(".exe") && name.contains("setup");
            #[cfg(target_os = "macos")]
            return name.ends_with(".dmg") && name.contains("macos");
            #[cfg(target_os = "linux")]
            return name.ends_with(".appimage") && name.contains("x86_64");
            #[allow(unreachable_code)]
            false
        })
        .cloned()
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

fn cache_root() -> PathBuf {
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

    #[test]
    fn nightly_version_comes_from_release_name() {
        let release = release("nightly", "Nightly v2026.8.3-nightly", true);
        assert_eq!(
            release_version(&release).unwrap(),
            Version::parse("2026.8.3-nightly").unwrap()
        );
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
