//! Embeds Windows icon, application manifest, and version resources from `apps/shared/`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../../packages/shared/app/windows/app.rc");
    println!("cargo:rerun-if-changed=../../../packages/shared/app/windows/app.manifest");
    println!("cargo:rerun-if-changed=../../../packages/shared/app/icons/icon.ico");
    println!("cargo:rerun-if-changed=../../../.discordrpcsecret");

    stage_exclusive_sources();
    download_onnxruntime();

    // Optional override for the Discord application id. Futureboard's own is
    // compiled into `sphere_discord_rpc` (`DEFAULT_APPLICATION_ID`), so this
    // only matters for a fork or a build that targets a different Discord
    // application. An explicit build environment value wins over the file.
    if std::env::var_os("FUTUREBOARD_DISCORD_CLIENT_ID").is_none() {
        if let Ok(application_id) = std::fs::read_to_string("../../../.discordrpcsecret") {
            let application_id = application_id.trim();
            if !application_id.is_empty()
                && application_id
                    .chars()
                    .all(|character| character.is_ascii_digit())
            {
                println!("cargo:rustc-env=FUTUREBOARD_DISCORD_CLIENT_ID={application_id}");
            }
        }
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // `libcef` (and the staged ONNX Runtime) ship beside the binary in the
    // packaged tree, so the loader must search the executable's own directory.
    // Windows already searches it; ELF/Mach-O need an explicit run path.
    match target_os.as_str() {
        "linux" => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        "macos" => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path"),
        _ => {}
    }
    if target_os == "windows" {
        embed_resource::compile(
            "../../../packages/shared/app/windows/app.rc",
            embed_resource::NONE,
        )
        .manifest_required()
        .unwrap();
    }
}

/// Default ONNX Runtime release fetched for the real MDX-NET stem backend.
/// Override with `FUTUREBOARD_ORT_VERSION`.
const ORT_DEFAULT_VERSION: &str = "1.27.1";

/// Download the ONNX Runtime shared library from the microsoft/onnxruntime
/// GitHub release and place it next to the built binary, so the Stem Extractor
/// can load it at runtime (`{appdir}/onnxruntime.dll` · `libonnxruntime.so` ·
/// `libonnxruntime.dylib`).
///
/// Only runs with `--features stem-onnx`. Failures are non-fatal: the app still
/// builds and simply falls back to the spectral stub at runtime. Set
/// `FUTUREBOARD_ORT_SKIP_DOWNLOAD=1` to skip (e.g. offline / air-gapped builds).
fn download_onnxruntime() {
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_ORT_VERSION");
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_ORT_SKIP_DOWNLOAD");

    if std::env::var_os("CARGO_FEATURE_STEM_ONNX").is_none() {
        return;
    }
    if std::env::var_os("FUTUREBOARD_ORT_SKIP_DOWNLOAD").is_some() {
        println!("cargo:warning=ONNX Runtime download skipped (FUTUREBOARD_ORT_SKIP_DOWNLOAD set)");
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let cuda = std::env::var_os("CARGO_FEATURE_STEM_CUDA").is_some();
    let directml = std::env::var_os("CARGO_FEATURE_STEM_DIRECTML").is_some();

    let version = std::env::var("FUTUREBOARD_ORT_VERSION")
        .unwrap_or_else(|_| ORT_DEFAULT_VERSION.to_string());

    // Where the binary ends up: OUT_DIR is target/<profile>/build/<pkg>/out.
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or(out_dir.clone());

    let (lib_name, dest_name) = match target_os.as_str() {
        "windows" => ("onnxruntime.dll", "onnxruntime.dll"),
        "macos" => ("libonnxruntime", "libonnxruntime.dylib"),
        "linux" => ("libonnxruntime.so", "libonnxruntime.so"),
        other => {
            println!("cargo:warning=ONNX Runtime auto-download unsupported for OS `{other}`");
            return;
        }
    };
    let dest = profile_dir.join(dest_name);
    if dest.is_file() {
        return; // Cached from a previous build.
    }

    let (platform, ext) = match (target_os.as_str(), target_arch.as_str()) {
        ("windows", "x86_64") => ("win-x64", "zip"),
        ("windows", "aarch64") => ("win-arm64", "zip"),
        ("linux", "x86_64") => ("linux-x64", "tgz"),
        ("linux", "aarch64") => ("linux-aarch64", "tgz"),
        // universal2 covers both Apple Silicon and Intel.
        ("macos", _) => ("osx-universal2", "tgz"),
        _ => {
            println!(
                "cargo:warning=ONNX Runtime auto-download unsupported for {target_os}/{target_arch}"
            );
            return;
        }
    };
    // Preferred assets in priority order. DirectML/GPU packages only exist for
    // x64 desktop; a CPU package is always the final fallback. GitHub currently
    // ships DirectML only via NuGet, so the DirectML candidate typically 404s
    // and we fall back to CPU (the DirectML EP then degrades to CPU at runtime).
    let mut assets: Vec<String> = Vec::new();
    if directml && platform == "win-x64" {
        assets.push(format!("onnxruntime-{platform}-directml-{version}.{ext}"));
    }
    if cuda && matches!(platform, "win-x64" | "linux-x64") {
        assets.push(format!("onnxruntime-{platform}-gpu-{version}.{ext}"));
    }
    assets.push(format!("onnxruntime-{platform}-{version}.{ext}"));

    for asset in &assets {
        let url = format!(
            "https://github.com/microsoft/onnxruntime/releases/download/v{version}/{asset}"
        );
        println!("cargo:warning=Downloading ONNX Runtime {version} ({asset})...");
        let bytes = match http_get(&url) {
            Ok(bytes) => bytes,
            Err(err) => {
                println!("cargo:warning=  {asset} unavailable ({err})");
                continue;
            }
        };
        let extracted = if ext == "zip" {
            extract_lib_from_zip(&bytes, lib_name)
        } else {
            extract_lib_from_tgz(&bytes, lib_name)
        };
        match extracted {
            Some(lib) => match std::fs::write(&dest, lib) {
                Ok(()) => {
                    println!("cargo:warning=ONNX Runtime staged at {}", dest.display());
                    return;
                }
                Err(err) => println!("cargo:warning=Could not write {}: {err}", dest.display()),
            },
            None => println!("cargo:warning=  {lib_name} not found inside {asset}"),
        }
    }

    println!(
        "cargo:warning=ONNX Runtime not staged; Stem Extractor will use the spectral stub. \
         Place {dest_name} beside the app or set ORT_DYLIB_PATH to enable real MDX-NET."
    );
}

/// GET `url` into memory, following redirects (GitHub → object storage).
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("request error: {e}"))?;
    let mut buf = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("read error: {e}"))?;
    Ok(buf)
}

/// Extract the regular file whose name matches `lib_name` from a zip archive.
fn extract_lib_from_zip(bytes: &[u8], lib_name: &str) -> Option<Vec<u8>> {
    use std::io::{Cursor, Read};
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).ok()?;
        if !file.is_file() {
            continue;
        }
        let name = file.name().rsplit('/').next().unwrap_or("").to_string();
        if lib_matches(&name, lib_name) {
            let mut out = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut out).ok()?;
            return Some(out);
        }
    }
    None
}

/// Extract the regular file whose name matches `lib_name` from a gzip+tar
/// archive.
fn extract_lib_from_tgz(bytes: &[u8], lib_name: &str) -> Option<Vec<u8>> {
    use std::io::{Cursor, Read};
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        if lib_matches(&name, lib_name) {
            let mut out = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut out).ok()?;
            return Some(out);
        }
    }
    None
}

/// Whether an archive entry file name is the ONNX Runtime library. Handles
/// versioned unix sonames (`libonnxruntime.so.1.27.1`, `libonnxruntime.1.27.1.dylib`).
fn lib_matches(entry_name: &str, lib_name: &str) -> bool {
    if entry_name == lib_name {
        return true;
    }
    match lib_name {
        "libonnxruntime.so" => entry_name.starts_with("libonnxruntime.so"),
        "libonnxruntime" => {
            entry_name.starts_with("libonnxruntime") && entry_name.ends_with(".dylib")
        }
        _ => false,
    }
}

/// Stage private implementation files only when Cargo is compiling the
/// Exclusive Edition. `include!` cannot accept their crate-level `//!` comments
/// inside the application's bridge module, so those comments become ordinary
/// comments in the generated copies.
fn stage_exclusive_sources() {
    // The license signing key and API endpoint are baked in via `option_env!`.
    // Rebuild when either changes so a build never keeps stale license config
    // and never silently ships without it.
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_LICENSE_PUBLIC_KEY");
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_LICENSE_API_URL");
    // Supabase auth config is baked from the repo `.env` (or the build
    // environment). Only the public URL and the *publishable* anon key are ever
    // read here — never the service secret key.
    println!("cargo:rerun-if-changed=../../../.env");
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_SUPABASE_URL");
    println!("cargo:rerun-if-env-changed=FUTUREBOARD_SUPABASE_ANON_KEY");
    // The EULA is embedded (include_str!) from the staged copies below; rebuild
    // when the source text changes.
    println!("cargo:rerun-if-changed=../../../crates/ExclusiveEdition/assets/EULA.EN.txt");
    println!("cargo:rerun-if-changed=../../../crates/ExclusiveEdition/assets/EULA.TH.txt");

    if std::env::var_os("CARGO_FEATURE_EXCLUSIVE").is_none() {
        return;
    }

    bake_supabase_config();

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let source_dir = manifest_dir.join("../../../crates/ExclusiveEdition/src");
    let assets_dir = manifest_dir.join("../../../crates/ExclusiveEdition/assets");
    let output_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("futureboard-exclusive");

    std::fs::create_dir_all(&output_dir).expect("failed to create Exclusive Edition output dir");

    stage_exclusive_source(&source_dir, &output_dir, "license.rs");
    stage_exclusive_source(&source_dir, &output_dir, "license_activation_dialog.rs");
    stage_exclusive_source(&source_dir, &output_dir, "auth.rs");
    stage_exclusive_source(&source_dir, &output_dir, "auth_dialog.rs");
    stage_exclusive_source(&source_dir, &output_dir, "eula.rs");
    stage_exclusive_source(&source_dir, &output_dir, "eula_dialog.rs");

    // The EULA text is embedded into the binary. Copy it beside the staged
    // source so `include_str!(concat!(env!("OUT_DIR"), ...))` finds it.
    copy_exclusive_asset(&assets_dir, &output_dir, "EULA.EN.txt");
    copy_exclusive_asset(&assets_dir, &output_dir, "EULA.TH.txt");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        stage_exclusive_source(&source_dir, &output_dir, "asio.rs");
    }
}

/// Copy an Exclusive Edition asset verbatim into the staging directory so the
/// staged source can embed it with `include_str!`.
fn copy_exclusive_asset(assets_dir: &Path, output_dir: &Path, file_name: &str) {
    let source_path = assets_dir.join(file_name);
    println!("cargo:rerun-if-changed={}", source_path.display());
    std::fs::copy(&source_path, output_dir.join(file_name)).unwrap_or_else(|error| {
        panic!(
            "Exclusive Edition asset is required for --features exclusive: {}: {error}",
            source_path.display()
        )
    });
}

/// Bake the Supabase auth endpoint and publishable key for `option_env!` in the
/// staged `auth.rs`. Source precedence: an explicit build-environment value
/// wins (CI/distribution), otherwise the repo `.env` is read.
///
/// SECURITY: the `.env` also contains `SUPABASE_SECRET_KEY` (a service-role
/// secret). It is deliberately never read or emitted here — a service secret in
/// a shipped desktop binary would be a full account-takeover credential. Only
/// the public URL and the publishable anon key belong in the client.
fn bake_supabase_config() {
    let dotenv = read_dotenv("../../../.env");
    let resolve = |build_key: &str, dotenv_key: &str| -> Option<String> {
        std::env::var(build_key)
            .ok()
            .or_else(|| {
                dotenv
                    .iter()
                    .find(|(k, _)| k == dotenv_key)
                    .map(|(_, v)| v.clone())
            })
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    if let Some(url) = resolve("FUTUREBOARD_SUPABASE_URL", "SUPABASE_URL") {
        println!("cargo:rustc-env=FUTUREBOARD_SUPABASE_URL={url}");
    }
    if let Some(anon) = resolve("FUTUREBOARD_SUPABASE_ANON_KEY", "SUPABASE_PUBLISHABLE_KEY") {
        println!("cargo:rustc-env=FUTUREBOARD_SUPABASE_ANON_KEY={anon}");
    }
}

/// Minimal `KEY=VALUE` parser for the repo `.env`. Ignores blanks and `#`
/// comments; does not expand or unquote — the values used here are plain tokens.
fn read_dotenv(relative: &str) -> Vec<(String, String)> {
    let Ok(contents) = std::fs::read_to_string(relative) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn stage_exclusive_source(source_dir: &Path, output_dir: &Path, file_name: &str) {
    let source_path = source_dir.join(file_name);
    println!("cargo:rerun-if-changed={}", source_path.display());

    let source = std::fs::read_to_string(&source_path).unwrap_or_else(|error| {
        panic!(
            "Exclusive Edition source is required for --features exclusive: {}: {error}",
            source_path.display()
        )
    });
    let staged = source
        .lines()
        .map(|line| {
            line.strip_prefix("//!")
                .map_or_else(|| line.to_owned(), |comment| format!("//{comment}"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(output_dir.join(file_name), staged)
        .unwrap_or_else(|error| panic!("failed to stage {file_name}: {error}"));
}
