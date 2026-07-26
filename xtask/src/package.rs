//! Package orchestration: build → collect → stage → validate → publish.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cargo_metadata::MetadataCommand;

use crate::cargo_build::{self, APP_PACKAGE};
use crate::cef;
use crate::metadata::{BuildInfo, MetadataInputs};
use crate::platform::{Edition, platform_folder};
use crate::plugins;
use crate::staging::{self, StagingPlan};
use crate::validation::{self, ValidationInputs};

/// Which Built-in Plugins to build and stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSelection {
    /// Do not build any plugins (default).
    None,
    /// Build every discovered Built-in Plugin.
    All,
    /// Build only the named plugin crates (e.g. `rodharerist`, `equz8`).
    Only(Vec<String>),
}

impl PluginSelection {
    /// Resolve the CLI inputs: `--plugin <spec>` (`all` / `none` / comma list),
    /// or the legacy `--plugins` bool (= `all`). `--plugin` wins when both given.
    pub fn parse(plugin: Option<&str>, plugins_flag: bool) -> Self {
        if let Some(spec) = plugin {
            return match spec.trim().to_ascii_lowercase().as_str() {
                "all" | "*" => Self::All,
                "" | "none" => Self::None,
                _ => {
                    let names: Vec<String> = spec
                        .split(',')
                        .map(|name| name.trim().to_string())
                        .filter(|name| !name.is_empty())
                        .collect();
                    if names.is_empty() {
                        Self::None
                    } else {
                        Self::Only(names)
                    }
                }
            };
        }
        if plugins_flag { Self::All } else { Self::None }
    }

    fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Everything the `package` subcommand needs.
pub struct PackageOptions {
    pub profile: String,
    pub target: Option<String>,
    pub edition: Edition,
    /// Root of the distributable tree (default `out`).
    pub out_root: PathBuf,
    /// Also stage debug symbols into `symbols/`.
    pub symbols: bool,
    /// Which Built-in Plugin dynamic libraries to build and stage.
    pub plugins: PluginSelection,
    /// Stage the shared CEF runtime flat beside the binary when available.
    pub stage_cef: bool,
}

/// Run the full package pipeline and return the published directory.
pub fn run(options: &PackageOptions) -> Result<PathBuf> {
    // 1. Build and discover the real executable paths (app + runtime sidecars).
    let build = cargo_build::build(&options.profile, options.target.as_deref(), options.edition)?;
    let executable = &build.app_executable;
    eprintln!("[xtask] built executable: {}", executable.display());

    // Resolve the effective target triple (explicit flag or detected host) for
    // platform naming and metadata.
    let target_triple = match &options.target {
        Some(target) => target.clone(),
        None => host_target().context("could not determine host target triple")?,
    };
    let platform = platform_folder(&target_triple);

    // 2. Prepare staging (clean any stale directory first).
    let plan = StagingPlan::new(
        &options.out_root,
        &options.profile,
        options.edition,
        &platform,
    );
    plan.prepare()?;

    // 3. Copy required runtime files into staging — the app binary, the sidecar
    //    executables it spawns from its own directory, and any runtime libraries
    //    the build placed beside the binary.
    let binary_name = staging::stage_executable(&plan.staging_dir, executable)?;

    let mut sidecar_names = Vec::with_capacity(build.sidecar_executables.len());
    for sidecar in &build.sidecar_executables {
        let name = staging::stage_executable(&plan.staging_dir, sidecar)?;
        eprintln!("[xtask] staged sidecar executable: {name}");
        sidecar_names.push(name);
    }

    let siblings = staging::stage_runtime_siblings(&plan.staging_dir, executable)?;
    for lib in &siblings {
        eprintln!("[xtask] staged runtime library: {lib}");
    }

    // 4. Create expected directories.
    staging::create_layout_dirs(&plan.staging_dir)?;

    // 4a. Stage the shared CEF runtime flat beside the binary (never a CEF/
    //     subdir). A normal package is not publishable without it; developers
    //     who intentionally need a CEF-free tree must opt out with --no-cef.
    let mut cef_staged = false;
    if options.stage_cef {
        match cef::locate_cef_dist(&workspace_root(), &target_triple) {
            Some(dist) => {
                let report = cef::stage_cef(&plan.staging_dir, &dist, &target_triple)
                    .context("failed to stage the CEF runtime")?;
                eprintln!(
                    "[xtask] staged CEF runtime flat: {} files + {} locales",
                    report.runtime_files.len(),
                    report.locale_count
                );
                cef_staged = true;
            }
            None => {
                let checked = cef::cef_dist_candidates(&workspace_root(), &target_triple)
                    .into_iter()
                    .map(|path| format!("  - {}", path.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "CEF distribution not found or invalid for {target_triple}.\n\
                     Checked:\n{checked}\n\
                     Install it with `cargo run -p SphereWebView --example install_cef \
                     --features installer`, or pass --no-cef for an intentional \
                     CEF-free developer package"
                )
            }
        }
    }

    // 4b. Optionally build and stage Built-in Plugin dynamic libraries.
    if options.plugins.is_enabled() {
        let discovered = plugin_packages()?;
        // Resolve the requested selection to actual discovered packages.
        let selected: Vec<&DiscoveredPlugin> = match &options.plugins {
            PluginSelection::None => Vec::new(),
            PluginSelection::All => discovered.iter().collect(),
            PluginSelection::Only(names) => {
                for requested in names {
                    if !discovered.iter().any(|p| &p.name == requested) {
                        eprintln!(
                            "[xtask] warning: requested plugin `{requested}` is not a \
                             Built-in Plugin cdylib crate — skipping"
                        );
                    }
                }
                discovered
                    .iter()
                    .filter(|p| names.iter().any(|n| n == &p.name))
                    .collect()
            }
        };

        // An editor-bearing plugin is not release-ready when its static bundle
        // is absent: compiling it would silently embed an empty asset table.
        for plugin in &selected {
            if plugins::has_editor_ui(&plugin.crate_dir)
                && !plugins::editor_ui_built(&plugin.crate_dir)
            {
                bail!(
                    "plugin `{}` has an editor bundle but no built dist/index.html; \
                     run its editor build before packaging",
                    plugin.name,
                );
            }
        }
        let packages: Vec<String> = selected.iter().map(|p| p.name.clone()).collect();
        let built = plugins::build_plugins(
            &packages,
            &options.profile,
            options.target.as_deref(),
            options.edition,
        )?;
        let staged = plugins::stage_plugins(&plan.staging_dir, &built, &target_triple)?;
        if staged.is_empty() {
            eprintln!("[xtask] plugin build requested but no plugin cdylibs were produced");
        }
        for name in &staged {
            eprintln!("[xtask] staged plugin: Plugins/{name}");
        }
        // Completeness check only when building the full set (a subset omits the
        // rest on purpose).
        if options.plugins == PluginSelection::All {
            let missing = plugins::missing_builtin_plugins(&staged, &target_triple);
            if !missing.is_empty() {
                bail!(
                    "expected built-in plugin(s) were not staged: {}",
                    missing.join(", "),
                );
            }
        }
    }

    // 5. Optional symbols.
    if options.symbols {
        let staged = staging::stage_symbols(&plan.staging_dir, executable)?;
        if staged.is_empty() {
            eprintln!("[xtask] --symbols requested but no .pdb was found beside the binary");
        }
        for path in &staged {
            eprintln!("[xtask] staged symbols: {path}");
        }
    }

    // 6. Write build metadata.
    let version = package_version()?;
    let info = BuildInfo::collect(&MetadataInputs {
        binary: &binary_name,
        sidecars: &sidecar_names,
        edition: options.edition,
        profile: &options.profile,
        target: &target_triple,
        platform: &platform,
        version: &version,
    });
    staging::write_build_info(&plan.staging_dir, &info.to_json()?)?;

    // 7. Validate the staged application before publishing.
    validation::validate_staging(&ValidationInputs {
        staging_dir: &plan.staging_dir,
        binary_name: &binary_name,
        sidecars: &sidecar_names,
        symbols_enabled: options.symbols,
        triple: &target_triple,
        cef_staged,
    })
    .context("staged package failed validation; previous output left untouched")?;

    // 8. Atomically publish, then tidy the (now empty) staging root.
    staging::publish(&plan.staging_dir, &plan.final_dir)?;
    staging::cleanup_staging_root_if_empty(&plan.staging_dir);
    eprintln!("[xtask] published package: {}", plan.final_dir.display());

    Ok(plan.final_dir)
}

/// The Futureboard workspace root (xtask lives at `<root>/xtask`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live under the workspace root")
        .to_path_buf()
}

/// A discovered Built-in Plugin package (name + crate directory).
struct DiscoveredPlugin {
    name: String,
    crate_dir: PathBuf,
}

/// Workspace member packages that build a Built-in Plugin dynamic library
/// (`cdylib`/`dylib`) and live under `crates/BuiltinAudioPlugins/crates`.
fn plugin_packages() -> Result<Vec<DiscoveredPlugin>> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to query cargo metadata for plugin discovery")?;
    // Normalize to forward slashes so the directory match is OS-independent.
    let marker = plugins::PLUGIN_CRATES_DIR; // "crates/BuiltinAudioPlugins/crates"
    let mut packages = Vec::new();
    for package in &metadata.packages {
        let manifest = package.manifest_path.as_std_path();
        let in_plugin_dir = manifest
            .to_string_lossy()
            .replace('\\', "/")
            .contains(marker);
        let produces_dylib = package.targets.iter().any(|target| {
            target
                .kind
                .iter()
                .any(|kind| matches!(kind.as_str(), "cdylib" | "dylib"))
        });
        if in_plugin_dir && produces_dylib {
            let crate_dir = manifest
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            packages.push(DiscoveredPlugin {
                name: package.name.to_string(),
                crate_dir,
            });
        }
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

/// Read the `futureboard_native` package version from workspace metadata,
/// reusing the existing application versioning source of truth.
fn package_version() -> Result<String> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to query cargo metadata for the application version")?;
    metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == APP_PACKAGE)
        .map(|package| package.version.to_string())
        .ok_or_else(|| anyhow!("package `{APP_PACKAGE}` not found in workspace metadata"))
}

/// The host target triple, parsed from `rustc -vV`'s `host:` line.
fn host_target() -> Result<String> {
    let output =
        std::process::Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()))
            .arg("-vV")
            .output()
            .context("failed to run rustc -vV")?;
    let text = String::from_utf8(output.stdout).context("rustc -vV output was not UTF-8")?;
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_string())
        .ok_or_else(|| anyhow!("rustc -vV did not report a host triple"))
}

#[cfg(test)]
mod tests {
    use super::PluginSelection;

    #[test]
    fn plugin_selection_all_and_none() {
        assert_eq!(
            PluginSelection::parse(Some("all"), false),
            PluginSelection::All
        );
        assert_eq!(
            PluginSelection::parse(Some("ALL"), false),
            PluginSelection::All
        );
        assert_eq!(
            PluginSelection::parse(Some("*"), false),
            PluginSelection::All
        );
        assert_eq!(
            PluginSelection::parse(Some("none"), false),
            PluginSelection::None
        );
        assert_eq!(
            PluginSelection::parse(Some(""), true),
            PluginSelection::None
        );
    }

    #[test]
    fn plugin_selection_list_is_trimmed() {
        assert_eq!(
            PluginSelection::parse(Some("rodharerist, equz8 ,,fa76"), false),
            PluginSelection::Only(vec![
                "rodharerist".to_string(),
                "equz8".to_string(),
                "fa76".to_string(),
            ])
        );
    }

    #[test]
    fn legacy_plugins_flag_means_all_and_default_is_none() {
        assert_eq!(PluginSelection::parse(None, true), PluginSelection::All);
        assert_eq!(PluginSelection::parse(None, false), PluginSelection::None);
        // Explicit --plugin wins over the legacy bool.
        assert_eq!(
            PluginSelection::parse(Some("none"), true),
            PluginSelection::None
        );
    }
}
