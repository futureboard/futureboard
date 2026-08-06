//! Install the pinned CEF distribution(s) into `build/cef/<version>/<platform>`.
//!
//! ```text
//! install_cef [--force] [--target <triple>]...
//! ```
//!
//! With no `--target`, the host distribution is installed. macOS universal
//! packaging needs both Apple distributions on the same machine, so the flag is
//! repeatable and also accepts `universal-macos` as a shorthand for the pair.

use sphere_webview::CefTarget;

const MACOS_UNIVERSAL_ALIASES: &[&str] = &["universal-macos", "macos-universal", "universal"];

fn main() {
    let targets = match parse_targets(std::env::args().skip(1)) {
        Ok(targets) => targets,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let force = std::env::args().skip(1).any(|arg| arg == "--force");
    let targets = match targets {
        Some(targets) => targets,
        None => match CefTarget::current() {
            Ok(target) => vec![target],
            Err(error) => {
                eprintln!("CEF installation failed: {error}");
                std::process::exit(1);
            }
        },
    };

    for target in targets {
        match sphere_webview::install_cef_target(target, force) {
            Ok(path) => println!("CEF installed at {}", path.display()),
            Err(error) => {
                eprintln!(
                    "CEF installation failed for {}: {error}",
                    target.target_triple()
                );
                std::process::exit(1);
            }
        }
    }
}

/// `Ok(None)` means no `--target` was given (install the host distribution).
fn parse_targets(args: impl Iterator<Item = String>) -> Result<Option<Vec<CefTarget>>, String> {
    let mut targets: Vec<CefTarget> = Vec::new();
    let mut expecting_target = false;
    let mut requested = false;
    for arg in args {
        if expecting_target {
            expecting_target = false;
            requested = true;
            for target in resolve_target(&arg)? {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
            continue;
        }
        match arg.as_str() {
            "--force" => {}
            "--target" => expecting_target = true,
            other if other.starts_with("--target=") => {
                requested = true;
                for target in resolve_target(&other["--target=".len()..])? {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    if expecting_target {
        return Err("--target requires a value".to_string());
    }
    Ok(requested.then_some(targets))
}

fn resolve_target(value: &str) -> Result<Vec<CefTarget>, String> {
    if MACOS_UNIVERSAL_ALIASES.contains(&value) {
        return Ok(vec![CefTarget::MacOsX86_64, CefTarget::MacOsAarch64]);
    }
    CefTarget::from_target_triple(value)
        .map(|target| vec![target])
        .map_err(|error| error.to_string())
}
