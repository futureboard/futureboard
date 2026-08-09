//! Build the application layout in a temporary directory, then publish it
//! atomically into `out/`.
//!
//! Nothing here copies the Cargo target tree wholesale — only the executable,
//! known runtime sibling libraries, generated directories and metadata are
//! staged. Publishing swaps directories with a rename so a failed package never
//! leaves the final output half-written.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::platform::Edition;

/// Directory names created inside every staged application.
pub const BIN_DIR: &str = "bin";
pub const PLUGINS_DIR: &str = "Plugins";
pub const RESOURCES_DIR: &str = "Resources";
/// Where optional `--symbols` output lands (kept out of the runtime layout).
pub const SYMBOLS_DIR: &str = "symbols";
/// Metadata filename.
pub const BUILD_INFO_FILE: &str = "build-info.json";

/// Runtime shared libraries the app loads next to its binary. Staged only when
/// present beside the built executable (e.g. `onnxruntime.dll` fetched by the
/// studio build script for the MDX-NET stem backend). Absence is not an error —
/// the app falls back to its spectral stub.
const RUNTIME_SIBLING_LIBS: &[&str] = &[
    "onnxruntime.dll",
    "libonnxruntime.so",
    "libonnxruntime.dylib",
];

/// Resolved input/output paths for one package run.
pub struct StagingPlan {
    /// Temporary directory assembled before publishing.
    pub staging_dir: PathBuf,
    /// Final `out/...` directory the staging replaces on success.
    pub final_dir: PathBuf,
}

/// Compute the final `out/` directory for this build.
///
/// * `dev` profile → `out/dev/<platform>` (edition omitted, matching the spec's
///   development layout).
/// * any other profile → `out/<profile>/<edition>/<platform>` (so `release`
///   yields `out/release/community/windows-x64`).
pub fn final_output_dir(
    out_root: &Path,
    profile: &str,
    edition: Edition,
    platform: &str,
) -> PathBuf {
    if profile == "dev" {
        out_root.join("dev").join(platform)
    } else {
        out_root.join(profile).join(edition.as_str()).join(platform)
    }
}

/// Compute the temporary staging directory for this build. Always edition- and
/// profile-qualified so concurrent/adjacent packages never collide.
pub fn staging_dir(out_root: &Path, profile: &str, edition: Edition, platform: &str) -> PathBuf {
    out_root
        .join(".staging")
        .join(format!("{platform}-{}-{profile}", edition.as_str()))
}

impl StagingPlan {
    pub fn new(out_root: &Path, profile: &str, edition: Edition, platform: &str) -> Self {
        StagingPlan {
            staging_dir: staging_dir(out_root, profile, edition, platform),
            final_dir: final_output_dir(out_root, profile, edition, platform),
        }
    }

    /// Remove any leftover staging directory and create a fresh, empty one.
    pub fn prepare(&self) -> Result<()> {
        if self.staging_dir.exists() {
            fs::remove_dir_all(&self.staging_dir).with_context(|| {
                format!(
                    "failed to clean stale staging dir {}",
                    self.staging_dir.display()
                )
            })?;
        }
        fs::create_dir_all(&self.staging_dir).with_context(|| {
            format!(
                "failed to create staging dir {}",
                self.staging_dir.display()
            )
        })?;
        Ok(())
    }
}

/// Copy the executable into staging under its own file name (keeping the
/// platform-correct extension) and return the staged binary's file name.
pub fn stage_executable(staging_dir: &Path, executable: &Path) -> Result<String> {
    let file_name = executable_file_name(executable)?;
    copy_into(staging_dir, &file_name, executable)?;
    Ok(file_name)
}

/// Copy an executable into a child directory and return its staged relative path.
pub fn stage_executable_into(
    staging_dir: &Path,
    directory: &str,
    executable: &Path,
) -> Result<String> {
    let file_name = executable_file_name(executable)?;
    let relative = format!("{directory}/{file_name}");
    copy_into(staging_dir, &relative, executable)?;
    Ok(relative)
}

fn executable_file_name(executable: &Path) -> Result<String> {
    executable
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("executable path has no file name: {}", executable.display()))
        .map(str::to_string)
}

/// Copy any known runtime sibling libraries found next to the executable.
/// Returns the file names that were staged.
pub fn stage_runtime_siblings(staging_dir: &Path, executable: &Path) -> Result<Vec<String>> {
    let source_dir = executable
        .parent()
        .context("executable has no parent directory")?;
    let mut staged = Vec::new();
    for lib in RUNTIME_SIBLING_LIBS {
        let candidate = source_dir.join(lib);
        if candidate.is_file() {
            copy_into(staging_dir, lib, &candidate)?;
            staged.push((*lib).to_string());
        }
    }
    Ok(staged)
}

/// Create the application directories (`bin/`, `Plugins/`, `Resources/`).
pub fn create_layout_dirs(staging_dir: &Path) -> Result<()> {
    for dir in [BIN_DIR, PLUGINS_DIR, RESOURCES_DIR] {
        let path = safe_join(staging_dir, Path::new(dir))?;
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

/// Write `build-info.json` into staging.
pub fn write_build_info(staging_dir: &Path, json: &str) -> Result<()> {
    let path = safe_join(staging_dir, Path::new(BUILD_INFO_FILE))?;
    fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Copy the debug-symbols file (`.pdb`) beside the executable, if any, into a
/// dedicated `symbols/` directory. Only invoked when `--symbols` is passed.
pub fn stage_symbols(staging_dir: &Path, executable: &Path) -> Result<Vec<String>> {
    let source_dir = executable
        .parent()
        .context("executable has no parent directory")?;
    let stem = executable
        .file_stem()
        .and_then(|s| s.to_str())
        .context("executable has no file stem")?;
    let mut staged = Vec::new();
    let pdb = source_dir.join(format!("{stem}.pdb"));
    if pdb.is_file() {
        let rel = format!("{SYMBOLS_DIR}/{stem}.pdb");
        copy_into(staging_dir, &rel, &pdb)?;
        staged.push(rel);
    }
    Ok(staged)
}

/// Atomically replace `final_dir` with `staging_dir`.
///
/// Both live under `out/`, so a rename is same-filesystem and near-atomic. If
/// the swap fails after the old package was moved aside, the previous package is
/// restored so `out/` is never left half-updated.
pub fn publish(staging_dir: &Path, final_dir: &Path) -> Result<()> {
    if let Some(parent) = final_dir.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Move any existing package aside first (Windows cannot rename onto a
    // non-empty directory).
    let backup = if final_dir.exists() {
        let backup = backup_path(final_dir);
        fs::rename(final_dir, &backup).with_context(|| {
            format!(
                "failed to move existing package {} aside",
                final_dir.display()
            )
        })?;
        Some(backup)
    } else {
        None
    };

    match fs::rename(staging_dir, final_dir) {
        Ok(()) => {
            if let Some(backup) = backup {
                // Best-effort cleanup; a leftover backup never corrupts output.
                let _ = fs::remove_dir_all(&backup);
            }
            Ok(())
        }
        Err(error) => {
            if let Some(backup) = backup {
                let _ = fs::rename(&backup, final_dir);
            }
            Err(error).with_context(|| {
                format!(
                    "failed to publish staging into {} (previous package preserved)",
                    final_dir.display()
                )
            })
        }
    }
}

/// Remove the shared `.staging` parent directory if it is now empty. Best-effort
/// and strictly scoped to an *empty* directory, so it never deletes an unrelated
/// or in-progress staging tree under `out/`.
pub fn cleanup_staging_root_if_empty(staging_dir: &Path) {
    if let Some(parent) = staging_dir.parent() {
        if parent.file_name().and_then(|n| n.to_str()) == Some(".staging") {
            if let Ok(mut entries) = fs::read_dir(parent) {
                if entries.next().is_none() {
                    let _ = fs::remove_dir(parent);
                }
            }
        }
    }
}

/// Timestamped sibling directory used to hold the previous package during a swap.
fn backup_path(final_dir: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S%3f");
    let name = final_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("package");
    final_dir.with_file_name(format!(".{name}.old-{stamp}"))
}

/// Copy `source` to `root/<relative>`, guaranteeing the destination stays inside
/// `root` (no `..`, no absolute components) and creating parent dirs as needed.
pub fn copy_into(root: &Path, relative: &str, source: &Path) -> Result<PathBuf> {
    let dest = safe_join(root, Path::new(relative))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, &dest)
        .with_context(|| format!("failed to copy {} -> {}", source.display(), dest.display()))?;
    Ok(dest)
}

/// Join `relative` onto `root`, rejecting any component that could escape it.
/// This is the single choke point guarding against path traversal.
pub fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut result = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir => bail!("unsafe `..` in staged path `{}`", relative.display()),
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute component in staged path `{}`", relative.display())
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Edition;

    #[test]
    fn dev_output_path_omits_edition() {
        let out = Path::new("out");
        let path = final_output_dir(out, "dev", Edition::Community, "windows-x64");
        assert_eq!(path, Path::new("out/dev/windows-x64"));
        // Edition does not change the dev path.
        let path_ex = final_output_dir(out, "dev", Edition::Professional, "windows-x64");
        assert_eq!(path_ex, path);
    }

    #[test]
    fn release_output_path_splits_by_edition() {
        let out = Path::new("out");
        assert_eq!(
            final_output_dir(out, "release", Edition::Community, "windows-x64"),
            Path::new("out/release/community/windows-x64")
        );
        assert_eq!(
            final_output_dir(out, "release", Edition::Professional, "windows-x64"),
            Path::new("out/release/professional/windows-x64")
        );
    }

    #[test]
    fn staging_path_is_fully_qualified() {
        let out = Path::new("out");
        assert_eq!(
            staging_dir(out, "release", Edition::Professional, "windows-x64"),
            Path::new("out/.staging/windows-x64-professional-release")
        );
    }

    #[test]
    fn safe_join_accepts_nested_relative_paths() {
        let root = Path::new("stage");
        assert_eq!(
            safe_join(root, Path::new("Resources/logo.png")).unwrap(),
            Path::new("stage/Resources/logo.png")
        );
        assert_eq!(
            safe_join(root, Path::new("./build-info.json")).unwrap(),
            Path::new("stage/build-info.json")
        );
    }

    #[test]
    fn stage_executable_into_places_tool_under_bin() {
        let temp = tempfile::tempdir().unwrap();
        let source_dir = temp.path().join("artifacts");
        let staging_dir = temp.path().join("stage");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&staging_dir).unwrap();
        let source = source_dir.join("apak.exe");
        fs::write(&source, b"MZ").unwrap();

        let relative = stage_executable_into(&staging_dir, BIN_DIR, &source).unwrap();

        assert_eq!(relative, "bin/apak.exe");
        assert_eq!(fs::read(staging_dir.join(&relative)).unwrap(), b"MZ");
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let root = Path::new("stage");
        assert!(safe_join(root, Path::new("../escape")).is_err());
        assert!(safe_join(root, Path::new("a/../../escape")).is_err());
        assert!(safe_join(root, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn publish_replaces_previous_package() {
        let temp = tempfile::tempdir().unwrap();
        let out = temp.path();
        let final_dir = out.join("release/community/windows-x64");
        let staging = out.join(".staging/windows-x64-community-release");

        // Existing (stale) package.
        fs::create_dir_all(&final_dir).unwrap();
        fs::write(final_dir.join("old.txt"), "old").unwrap();

        // Fresh staging.
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("new.txt"), "new").unwrap();

        publish(&staging, &final_dir).unwrap();

        assert!(final_dir.join("new.txt").is_file());
        assert!(!final_dir.join("old.txt").exists());
        assert!(!staging.exists(), "staging is consumed by the rename");
        // No backups left behind.
        let leftovers: Vec<_> = fs::read_dir(final_dir.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".old-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn prepare_removes_stale_staging_contents() {
        let temp = tempfile::tempdir().unwrap();
        let plan = StagingPlan::new(temp.path(), "release", Edition::Community, "windows-x64");

        fs::create_dir_all(&plan.staging_dir).unwrap();
        fs::write(plan.staging_dir.join("junk.txt"), "junk").unwrap();

        plan.prepare().unwrap();

        assert!(plan.staging_dir.is_dir());
        assert!(!plan.staging_dir.join("junk.txt").exists());
    }
}
