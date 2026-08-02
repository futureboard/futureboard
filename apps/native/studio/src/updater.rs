//! GitHub Releases based application update discovery and staging.
//!
//! Network and filesystem work in this module is blocking by design. Callers
//! must run it on GPUI's background executor, never on the UI or audio thread.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sphere_ui_components::settings::UpdateChannel;

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

impl AvailableUpdate {
    pub fn asset_name(&self) -> &str {
        &self.asset.name
    }
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

pub fn download_update(update: &AvailableUpdate, cache_root: &Path) -> Result<PathBuf, String> {
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

pub fn launch_update(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = Command::new(path);
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("failed to inspect AppImage: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("failed to make AppImage executable: {error}"))?;
        if let Some(current) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
            if current.is_file() {
                let staged = current.with_extension("AppImage.update");
                fs::copy(path, &staged)
                    .map_err(|error| format!("failed to stage AppImage update: {error}"))?;
                let mut staged_permissions = fs::metadata(&staged)
                    .map_err(|error| format!("failed to inspect staged AppImage: {error}"))?
                    .permissions();
                staged_permissions.set_mode(0o755);
                fs::set_permissions(&staged, staged_permissions)
                    .map_err(|error| format!("failed to prepare staged AppImage: {error}"))?;
                fs::rename(&staged, &current)
                    .map_err(|error| format!("failed to install AppImage update: {error}"))?;
                return Ok(());
            }
        }
        Command::new(path)
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("failed to open update installer: {error}"))
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
