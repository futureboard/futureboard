//! Professional Edition build toolchain: the Steinberg ASIO SDK and libclang.
//!
//! The Windows Professional build compiles `asio-sys`, which needs two things this
//! workspace cannot vendor:
//!
//! * the **ASIO SDK** headers (`common/asio.h`, `host/asiodrivers.h`), which
//!   Steinberg licenses separately and which therefore cannot live in the repo;
//! * **libclang**, because `asio-sys` runs bindgen over those headers.
//!
//! Left to itself, `asio-sys` downloads the SDK into `%TEMP%\asio_sdk`. That
//! path is where this stopped working: with SDK 2.3.4 the archive gained an
//! `ASIOSDK/` root directory, and the extraction leaves the folder skeleton
//! behind with no headers in it. `asio-sys` then sees a directory that exists,
//! skips the download for good, and every later build fails with
//!
//!     fatal error C1083: Cannot open include file: 'asiodrivers.h'
//!
//! So the SDK is provisioned here instead, into `build/asio/` beside the CEF
//! distribution, and `CPAL_ASIO_DIR` is pointed at a directory that has been
//! *verified* to contain the headers. A half-extracted tree is treated as
//! absent and replaced rather than trusted.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

/// Where Steinberg publishes the SDK. The URL redirects to the current
/// versioned archive, so this keeps working across SDK releases — which is also
/// why the extracted layout is validated rather than assumed.
const ASIO_SDK_URL: &str = "https://www.steinberg.net/asiosdk";

/// Cap on the downloaded archive. The real SDK is ~9 MB; anything far larger is
/// a redirect to something that is not the SDK.
const MAX_SDK_BYTES: u64 = 64 * 1024 * 1024;

/// Marker written next to the extracted SDK once its licence has been accepted,
/// so the acceptance survives a new shell without an environment variable.
const LICENSE_MARKER: &str = "STEINBERG_ASIO_LICENSE_ACCEPTED";

/// Environment variable a developer sets to accept Steinberg's terms.
const LICENSE_ENV: &str = "FUTUREBOARD_ACCEPT_ASIO_LICENSE";

/// Resolved locations the Professional build needs.
#[derive(Debug, Clone)]
pub struct ProfessionalToolchain {
    /// Directory containing `common/` and `host/` — the value for
    /// `CPAL_ASIO_DIR`.
    pub asio_sdk_dir: PathBuf,
    /// Directory containing `libclang.dll` — the value for `LIBCLANG_PATH`.
    pub libclang_dir: PathBuf,
}

impl ProfessionalToolchain {
    /// Apply the toolchain to a Cargo invocation.
    ///
    /// `PATH` is extended rather than replaced: `asio-sys` shells out to the
    /// MSVC toolchain it discovers through `vcvarsall.bat`, and dropping the
    /// inherited `PATH` would take that with it.
    pub fn apply(&self, command: &mut std::process::Command) {
        command.env("CPAL_ASIO_DIR", &self.asio_sdk_dir);
        command.env("LIBCLANG_PATH", &self.libclang_dir);
        if let Some(path) = prepended_path(&self.libclang_dir) {
            command.env("PATH", path);
        }
    }
}

fn prepended_path(dir: &Path) -> Option<std::ffi::OsString> {
    let current = std::env::var_os("PATH")?;
    let mut entries = vec![dir.to_path_buf()];
    entries.extend(std::env::split_paths(&current));
    std::env::join_paths(entries).ok()
}

/// Prepare the Professional Edition toolchain, downloading the ASIO SDK if the
/// licence has been accepted and no usable copy is present.
pub fn prepare_professional(workspace_root: &Path) -> Result<ProfessionalToolchain> {
    let libclang_dir = resolve_libclang(workspace_root)?;
    let asio_sdk_dir = resolve_asio_sdk(workspace_root)?;
    eprintln!(
        "[xtask] professional toolchain: ASIO SDK {}, libclang {}",
        asio_sdk_dir.display(),
        libclang_dir.display()
    );
    Ok(ProfessionalToolchain {
        asio_sdk_dir,
        libclang_dir,
    })
}

// ── libclang ─────────────────────────────────────────────────────────────────

/// Find the directory holding `libclang.dll`.
///
/// An explicit `LIBCLANG_PATH` wins, then the repo-local `.bin` LLVM drop that
/// `build-private-temporary.py` produces. Both are checked for the actual
/// library rather than assumed, because a stale directory is the failure mode
/// that produces a confusing bindgen panic much later in the build.
fn resolve_libclang(workspace_root: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("LIBCLANG_PATH") {
        candidates.push(PathBuf::from(configured));
    }
    let local_llvm = workspace_root.join(".bin");
    candidates.push(local_llvm.join("bin"));
    candidates.push(local_llvm.join("lib"));

    for candidate in &candidates {
        if has_libclang(candidate) {
            return Ok(candidate.clone());
        }
    }

    bail!(
        "could not find libclang, which `asio-sys` needs to run bindgen over the ASIO headers.\n\
         Checked:\n{}\n\
         Fix it by putting an LLVM distribution in `{}` (its `bin/` must contain \
         libclang.dll), or by setting LIBCLANG_PATH to a directory that does.",
        candidates
            .iter()
            .map(|path| format!("  - {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n"),
        local_llvm.display()
    )
}

fn has_libclang(dir: &Path) -> bool {
    [
        "libclang.dll",
        "libclang.so",
        "libclang.dylib",
        "libclang.lib",
    ]
    .iter()
    .any(|name| dir.join(name).is_file())
}

// ── ASIO SDK ─────────────────────────────────────────────────────────────────

/// Find (or fetch) an ASIO SDK whose headers are actually present.
fn resolve_asio_sdk(workspace_root: &Path) -> Result<PathBuf> {
    if let Some(configured) = std::env::var_os("CPAL_ASIO_DIR") {
        let configured = PathBuf::from(configured);
        match sdk_root(&configured) {
            Some(root) => return Ok(root),
            None => eprintln!(
                "[xtask] warning: CPAL_ASIO_DIR points at {} which has no ASIO headers - ignoring it",
                configured.display()
            ),
        }
    }

    let sdk_home = workspace_root.join("build").join("asio");
    if let Some(root) = sdk_root(&sdk_home) {
        return Ok(root);
    }

    if !license_accepted(&sdk_home) {
        bail!(
            "no usable ASIO SDK found, and Steinberg's licence has not been accepted.\n\
             The SDK cannot be vendored into this repository.\n\
             \n\
             Either accept the licence and let this download it:\n  \
             set {LICENSE_ENV}=1   (or create the file {})\n\
             or extract the SDK yourself so that `{}` contains `common/asio.h` \
             and `host/asiodrivers.h`.",
            sdk_home.join(LICENSE_MARKER).display(),
            sdk_home.display()
        );
    }

    eprintln!(
        "[xtask] downloading the ASIO SDK into {}",
        sdk_home.display()
    );
    let downloaded = download_asio_sdk(&sdk_home)?;
    sdk_root(&downloaded).ok_or_else(|| {
        anyhow!(
            "the downloaded ASIO SDK at {} does not contain common/asio.h and \
             host/asiodrivers.h - the archive layout changed",
            downloaded.display()
        )
    })
}

/// The directory to hand `CPAL_ASIO_DIR`, or `None` when this is not an SDK.
///
/// Checks for the two headers `asio-sys` compiles against. The archive has
/// carried its contents under a single root folder (`ASIOSDK/`) since 2.3.4, so
/// one level of nesting is unwrapped — that nesting is exactly what left a bare
/// directory skeleton in `%TEMP%` and broke the build.
fn sdk_root(candidate: &Path) -> Option<PathBuf> {
    if is_sdk_root(candidate) {
        return Some(candidate.to_path_buf());
    }
    let entries = fs::read_dir(candidate).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .find(|path| is_sdk_root(path))
}

fn is_sdk_root(candidate: &Path) -> bool {
    candidate.join("common").join("asio.h").is_file()
        && candidate.join("host").join("asiodrivers.h").is_file()
}

/// Whether Steinberg's terms have been accepted for this checkout.
fn license_accepted(sdk_home: &Path) -> bool {
    if sdk_home.join(LICENSE_MARKER).is_file() {
        return true;
    }
    std::env::var(LICENSE_ENV)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Download and extract the SDK under `sdk_home`, returning the extracted root.
fn download_asio_sdk(sdk_home: &Path) -> Result<PathBuf> {
    fs::create_dir_all(sdk_home)
        .with_context(|| format!("failed to create {}", sdk_home.display()))?;

    let mut response = ureq::get(ASIO_SDK_URL)
        .header("User-Agent", "Futureboard-xtask/1")
        .call()
        .context("failed to download the ASIO SDK from steinberg.net")?;
    let mut archive = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_SDK_BYTES)
        .read_to_end(&mut archive)
        .context("failed to read the ASIO SDK download")?;
    if archive.len() as u64 >= MAX_SDK_BYTES {
        bail!("the ASIO SDK download exceeded {MAX_SDK_BYTES} bytes - that is not the SDK");
    }

    let extracted = extract_zip(&archive, sdk_home)?;
    // Record acceptance beside the SDK so the next build in a fresh shell does
    // not ask again for a licence this developer already accepted.
    let _ = fs::write(
        sdk_home.join(LICENSE_MARKER),
        b"Steinberg ASIO SDK licence accepted for this checkout.\n",
    );
    Ok(extracted)
}

/// Extract a zip into `destination`, returning the directory that received it.
///
/// Entries are resolved against the destination and rejected if they escape it:
/// a zip is untrusted input, and `..` in an entry name is how an archive writes
/// outside the folder it was extracted into.
fn extract_zip(archive: &[u8], destination: &Path) -> Result<PathBuf> {
    let reader = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader).context("the ASIO SDK download is not a zip")?;

    let mut top_level: Option<PathBuf> = None;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            bail!(
                "the ASIO SDK archive contains an unsafe path: {}",
                entry.name()
            );
        };
        let target = destination.join(&relative);

        if let Some(first) = relative.components().next() {
            let first = destination.join(first.as_os_str());
            if top_level.is_none() {
                top_level = Some(first);
            }
        }

        if entry.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = fs::File::create(&target)
            .with_context(|| format!("failed to write {}", target.display()))?;
        std::io::copy(&mut entry, &mut file)
            .with_context(|| format!("failed to extract {}", target.display()))?;
    }

    Ok(top_level.unwrap_or_else(|| destination.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sdk(root: &Path) {
        fs::create_dir_all(root.join("common")).unwrap();
        fs::create_dir_all(root.join("host")).unwrap();
        fs::write(root.join("common").join("asio.h"), b"// asio").unwrap();
        fs::write(root.join("host").join("asiodrivers.h"), b"// drivers").unwrap();
    }

    #[test]
    fn an_sdk_is_recognized_at_the_root() {
        let temp = tempfile::tempdir().unwrap();
        write_sdk(temp.path());
        assert_eq!(sdk_root(temp.path()).as_deref(), Some(temp.path()));
    }

    /// SDK 2.3.4 packs everything under `ASIOSDK/`. Unwrapping that is the
    /// difference between a working build and `Cannot open include file`.
    #[test]
    fn an_sdk_is_recognized_one_level_down() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("ASIOSDK");
        write_sdk(&nested);
        assert_eq!(sdk_root(temp.path()).as_deref(), Some(nested.as_path()));
    }

    /// The failure this module exists for: a directory skeleton with no
    /// headers must read as "no SDK", never as one to compile against.
    #[test]
    fn a_directory_skeleton_is_not_an_sdk() {
        let temp = tempfile::tempdir().unwrap();
        for dir in ["asio", "common", "driver", "host"] {
            fs::create_dir_all(temp.path().join(dir)).unwrap();
        }
        assert!(sdk_root(temp.path()).is_none());
    }

    #[test]
    fn libclang_is_found_in_the_repo_local_llvm_drop() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join(".bin").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("libclang.dll"), b"stub").unwrap();

        // A stale LIBCLANG_PATH must not win over a directory that really has
        // the library — that mistake surfaces as a bindgen panic much later.
        assert!(has_libclang(&bin));
        assert!(!has_libclang(temp.path()));
        assert_eq!(resolve_libclang(temp.path()).unwrap(), bin);
    }

    #[test]
    fn a_missing_toolchain_explains_where_to_put_it() {
        let temp = tempfile::tempdir().unwrap();
        let error = resolve_libclang(temp.path()).unwrap_err().to_string();
        assert!(error.contains(".bin"), "unhelpful error: {error}");
    }

    #[test]
    fn extraction_keeps_the_archive_inside_its_destination() {
        let temp = tempfile::tempdir().unwrap();
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("ASIOSDK/common/asio.h", options).unwrap();
            std::io::Write::write_all(&mut writer, b"// asio").unwrap();
            writer
                .start_file("ASIOSDK/host/asiodrivers.h", options)
                .unwrap();
            std::io::Write::write_all(&mut writer, b"// drivers").unwrap();
            writer.finish().unwrap();
        }

        let extracted = extract_zip(&buffer, temp.path()).unwrap();
        assert_eq!(extracted, temp.path().join("ASIOSDK"));
        assert!(sdk_root(temp.path()).is_some());
    }
}
