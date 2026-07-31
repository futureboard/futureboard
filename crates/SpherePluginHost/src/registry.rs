//! Platform-neutral plugin registry types and VST3/CLAP scan for native GPUI.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::plugin_db::PluginScanStatus;
use crate::preset::{clear_all_presets, ensure_preset_folders, register_plugin};
use crate::scan::cache::{
    load_au_cache_state, record_au_scan_failure, record_au_scan_success, save_au_cache_state,
    should_auto_scan_au,
};
use crate::scan::isolation::{
    plugin_info_from_descriptor, run_isolated_bundle_scan, run_isolated_format_scan,
    IsolatedScanRequest,
};
use crate::scan::types::PluginScanFormat;
use crate::scanner::discover_plugin_bundles;
use crate::types::PluginInfo;

/// Plug-in container format (aligned with Electron `AudioPluginRegistryEntry.format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginFormat {
    Vst3,
    Clap,
    Au,
    Lv2,
    Unknown,
}

impl PluginFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vst3 => "VST3",
            Self::Clap => "CLAP",
            Self::Au => "AU",
            Self::Lv2 => "LV2",
            Self::Unknown => "Unknown",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "VST3" => Self::Vst3,
            "CLAP" => Self::Clap,
            "AU" => Self::Au,
            "LV2" => Self::Lv2,
            _ => Self::Unknown,
        }
    }

    /// Whether a plug-in of this format is a file the app can stat.
    ///
    /// Audio Units are registered with the system by component id
    /// (`au:<type>:<subtype>:<manufacturer>`) and expose no path a host can
    /// open, so the scanner stores that identifier in the `path` field. Every
    /// check that reads a missing file as a missing plug-in has to ask this
    /// first, or every AU reports itself as broken.
    pub fn has_module_file(self) -> bool {
        !matches!(self, Self::Au)
    }
}

/// Effect vs instrument (heuristic classification; matches Electron).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Effect,
    Instrument,
}

/// Row status in the plug-in manager list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    /// `.pst` on disk (Electron "Available").
    PresetReady,
    MissingPreset,
}

/// Scanner / registry host readiness (maps `AudioPluginHostStatus`).
#[derive(Debug, Clone)]
pub struct NativeHostStatus {
    pub available: bool,
    pub backend: String,
    pub message: String,
    pub db_path: PathBuf,
    pub preset_root: PathBuf,
    pub default_scan_paths: Vec<PathBuf>,
}

/// One plug-in in the cached registry (maps `AudioPluginRegistryEntry`).
#[derive(Debug, Clone)]
pub struct RegistryPlugin {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub format: PluginFormat,
    pub category: String,
    pub raw_category: Option<String>,
    pub sub_categories: Option<String>,
    pub kind: PluginKind,
    pub path: PathBuf,
    pub class_id: Option<String>,
    pub version: Option<String>,
    pub sdk_metadata_loaded: bool,
    pub preset_path: PathBuf,
    pub scanned_at_ms: i64,
    pub status: PluginStatus,
    pub scan_status: PluginScanStatus,
    pub error_message: Option<String>,
}

impl RegistryPlugin {
    pub fn display_category(&self) -> String {
        display_category(
            self.format,
            &self.category,
            self.raw_category.as_deref(),
            self.sub_categories.as_deref(),
        )
    }

    /// Whether this row is a Futureboard built-in (stock) plug-in.
    pub fn is_builtin(&self) -> bool {
        crate::builtin::is_builtin_id(&self.id)
    }

    /// Insert onto a track is supported for wired formats (or any built-in) once
    /// the preset is ready.
    pub fn supports_insert(&self) -> bool {
        (self.is_builtin()
            || matches!(
                self.format,
                PluginFormat::Vst3 | PluginFormat::Clap | PluginFormat::Au
            ))
            && self.status == PluginStatus::PresetReady
    }

    /// Editor window: built-ins use the embedded CEF (`mikoplugin://`) editor;
    /// external plug-ins use the native host-owned window (VST3 only today,
    /// matching the Electron lifecycle).
    pub fn supports_editor(&self) -> bool {
        if self.is_builtin() {
            return crate::builtin::builtin_has_editor(&self.id);
        }
        self.format == PluginFormat::Vst3 && self.supports_insert()
    }
}

/// Normalize category label (Electron `normalizeCategoryLabel` + UI fallback).
pub fn display_category(
    format: PluginFormat,
    category: &str,
    raw_category: Option<&str>,
    sub_categories: Option<&str>,
) -> String {
    let tags: Vec<&str> = sub_categories
        .unwrap_or("")
        .split('|')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();

    let has = |needle: &str| tags.iter().any(|t| t.eq_ignore_ascii_case(needle));

    if format == PluginFormat::Vst3 {
        if has("Instrument") {
            return "Instrument".to_string();
        }
        if has("EQ") {
            return "EQ".to_string();
        }
        if has("Dynamics") {
            return "Dynamics".to_string();
        }
        if has("Reverb") {
            return "Reverb".to_string();
        }
        if has("Delay") {
            return "Delay".to_string();
        }
        if category.eq_ignore_ascii_case("audio module class") {
            return tags
                .iter()
                .find(|t| !t.eq_ignore_ascii_case("fx"))
                .map(|s| (*s).to_string())
                .unwrap_or_else(|| "Effect".to_string());
        }
        if !tags.is_empty() {
            return tags.join("|");
        }
        return category.to_string();
    }

    if format == PluginFormat::Clap {
        let specific: Vec<&str> = tags
            .iter()
            .copied()
            .filter(|t| {
                !matches!(
                    t.to_ascii_lowercase().as_str(),
                    "audio-effect" | "audio effect" | "plugin" | "utility"
                )
            })
            .collect();
        let display_tags: Vec<&str> = if specific.is_empty() {
            tags.clone()
        } else {
            specific
        };
        if display_tags
            .iter()
            .any(|t| t.eq_ignore_ascii_case("instrument"))
        {
            return "Instrument".to_string();
        }
        if display_tags
            .iter()
            .any(|t| t.to_ascii_lowercase().contains("effect"))
        {
            return "Effect".to_string();
        }
        if category.eq_ignore_ascii_case("audio effect") {
            return "Effect".to_string();
        }
        return display_tags
            .first()
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| category.to_string());
    }

    if let Some(sub) = sub_categories.filter(|s| !s.trim().is_empty()) {
        return sub.trim().to_string();
    }
    if !category.is_empty() {
        return category.to_string();
    }
    raw_category
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "Uncategorized".to_string())
}

pub fn classify_kind(category: &str, name: &str, sub_categories: Option<&str>) -> PluginKind {
    let haystack =
        format!("{} {} {}", category, sub_categories.unwrap_or(""), name).to_ascii_lowercase();
    if haystack.contains("instrument")
        || haystack.contains("synth")
        || haystack.contains("synthesizer")
        || haystack.contains("sampler")
        || haystack.contains("rompler")
        || haystack.contains("drum")
        || haystack.contains("piano")
        || haystack.contains("organ")
        || haystack.contains("bass")
        || haystack.contains("generator")
    {
        PluginKind::Instrument
    } else {
        PluginKind::Effect
    }
}

/// OS-default plug-in scan folders (matches Electron `defaultScanPaths`).
pub fn default_scan_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            let pf = PathBuf::from(pf);
            paths.push(pf.join("Common Files").join("VST3"));
            paths.push(pf.join("Common Files").join("CLAP"));
            paths.push(pf.join("VSTPlugins"));
            paths.push(pf.join("Steinberg").join("VSTPlugins"));
        }
        if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
            let pf86 = PathBuf::from(pf86);
            paths.push(pf86.join("Common Files").join("VST3"));
            paths.push(pf86.join("Common Files").join("CLAP"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            paths.push(local.join("Programs").join("Common").join("VST3"));
            paths.push(local.join("Programs").join("Common").join("CLAP"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join("Library/Audio/Plug-Ins/VST3"));
            paths.push(home.join("Library/Audio/Plug-Ins/CLAP"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
        paths.push(PathBuf::from("/usr/lib/clap"));
        paths.push(PathBuf::from("/usr/local/lib/clap"));
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".vst3"));
            paths.push(home.join(".clap"));
        }
    }
    paths
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct PluginScanFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct RegistryScanResult {
    pub plugins: Vec<RegistryPlugin>,
    pub scanned_paths: Vec<PathBuf>,
    pub failed: Vec<PluginScanFailure>,
    pub generated_presets: u32,
    pub au_scan_error: Option<String>,
    pub au_scan_crashed: bool,
    pub au_auto_scan_disabled: bool,
    pub au_scan_available: bool,
}

/// Scan job options for the native plug-in manager.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub paths: Option<Vec<PathBuf>>,
    /// When true, delete all `.pst` files under the preset root before scanning.
    pub delete_presets_first: bool,
    /// When true, scan AudioUnit plug-ins (macOS only). Ignored when safe mode is active.
    pub include_au: bool,
    /// When set, only scan the listed formats.
    pub formats_only: Option<Vec<PluginFormat>>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            paths: None,
            delete_presets_first: false,
            include_au: true,
            formats_only: None,
        }
    }
}

/// Incremental scan progress (bundle discovery → metadata → registration).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ScanProgress {
    Started {
        bundle_total: usize,
    },
    ScanningBundle {
        current: usize,
        total: usize,
        path: PathBuf,
    },
    Registering {
        current: usize,
        total: usize,
        name: String,
        plugin: RegistryPlugin,
        generated_presets: u32,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
    FormatFinished {
        format: PluginFormat,
        success_count: usize,
        failed_count: usize,
        crashed_count: usize,
        error: Option<String>,
    },
}

pub fn default_preset_root() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Futureboard Studio")
        .join("Audio Plug-ins")
}

fn safe_file_name(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*\x00-\x1f".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

fn preset_path_for_plugin(
    preset_root: &Path,
    format: PluginFormat,
    kind: PluginKind,
    name: &str,
) -> PathBuf {
    let fmt_dir = match format {
        PluginFormat::Clap => "CLAP",
        PluginFormat::Au => "AU",
        _ => "VST3",
    };
    let kind_dir = match kind {
        PluginKind::Instrument => "Instruments",
        PluginKind::Effect => "Effects",
    };
    preset_root
        .join(fmt_dir)
        .join(kind_dir)
        .join(format!("{}.pst", safe_file_name(name)))
}

fn resolve_unique_preset_path(
    plugin: RegistryPlugin,
    occupied: &mut HashSet<String>,
) -> RegistryPlugin {
    let mut candidate = plugin.preset_path.to_string_lossy().to_string();
    let mut index = 2;
    while occupied.contains(&candidate.to_lowercase()) {
        let parsed = plugin
            .preset_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let stem = plugin
            .preset_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Plug-in");
        let ext = plugin
            .preset_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("pst");
        candidate = parsed
            .join(format!("{stem} ({index}).{ext}"))
            .to_string_lossy()
            .to_string();
        index += 1;
    }
    occupied.insert(candidate.to_lowercase());
    RegistryPlugin {
        preset_path: PathBuf::from(candidate),
        ..plugin
    }
}

fn registry_display_key(plugin: &RegistryPlugin) -> String {
    [
        plugin.vendor.as_str(),
        plugin.name.as_str(),
        plugin.format.label(),
        plugin.category.as_str(),
        match plugin.kind {
            PluginKind::Instrument => "instrument",
            PluginKind::Effect => "effect",
        },
    ]
    .join("|")
    .to_lowercase()
}

/// Opt-in incremental (per-bundle) rescan. Default OFF, so the scanner's
/// behaviour is byte-identical to a full rescan unless the user sets
/// `FUTUREBOARD_PLUGIN_SCAN_INCREMENTAL=1`. Gated because skipping the isolated
/// bundle scan reuses cached rows, and that path is best validated against a
/// real plug-in library before it becomes the default.
fn incremental_scan_enabled() -> bool {
    std::env::var("FUTUREBOARD_PLUGIN_SCAN_INCREMENTAL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether every cached entry belonging to a bundle is still valid, so the
/// bundle's isolated scan can be skipped and the cached rows reused. Reuse is
/// allowed ONLY when the bundle has at least one cached entry and every entry is
/// both usable (a prior failure/crash must be retried, never cached) and
/// signature-fresh (its stored mtime+size still matches the file on disk). Pure
/// over `(is_usable, is_fresh)` pairs so the eligibility rule is unit-testable
/// without a live scan.
fn bundle_is_reusable(per_entry: impl IntoIterator<Item = (bool, bool)>) -> bool {
    let mut saw_entry = false;
    for (usable, fresh) in per_entry {
        saw_entry = true;
        if !usable || !fresh {
            return false;
        }
    }
    saw_entry
}

/// Build a registry row from a native scan result (`PluginInfo`).
pub fn registry_plugin_from_scan(info: &PluginInfo, scanned_at_ms: i64) -> RegistryPlugin {
    let format = PluginFormat::from_str_lossy(&info.format);
    let raw = info.category.clone();
    let sub = info.sub_categories.clone();
    let category = display_category(format, &raw, Some(&raw), sub.as_deref());
    let kind = classify_kind(&raw, &info.name, sub.as_deref());
    let preset_root = default_preset_root();
    let preset_path = preset_path_for_plugin(&preset_root, format, kind, &info.name);
    let path = PathBuf::from(&info.path);
    // A format with no module file (AU) has nothing to stat, so readiness rests
    // on the preset alone.
    let module_present = !format.has_module_file() || path.exists();
    let status = if module_present && preset_path.exists() {
        PluginStatus::PresetReady
    } else {
        PluginStatus::MissingPreset
    };
    RegistryPlugin {
        id: info.id.clone(),
        name: info.name.clone(),
        vendor: info.vendor.clone(),
        format,
        category,
        raw_category: Some(raw),
        sub_categories: sub,
        kind,
        path,
        class_id: info.class_id.clone(),
        version: info.version.clone(),
        sdk_metadata_loaded: info.sdk_metadata_loaded,
        preset_path,
        scanned_at_ms,
        status,
        scan_status: if info.sdk_metadata_loaded {
            PluginScanStatus::Success
        } else {
            PluginScanStatus::Failed
        },
        error_message: None,
    }
}

/// Host readiness for the plug-in manager UI.
pub fn native_host_status() -> NativeHostStatus {
    let preset_root = default_preset_root();
    // Native GPUI build does not link the N-API surface; treat host as available
    // if we can compute scan paths + preset root. Electron uses the N-API entrypoints.
    let (available, backend, message) = (
        true,
        "native".to_string(),
        if cfg!(target_os = "macos") {
            "Native plugin scanner ready (VST3, CLAP, AudioUnit)."
        } else {
            "Native plugin scanner ready (VST3, CLAP). AudioUnit unavailable on this platform."
        }
        .to_string(),
    );
    NativeHostStatus {
        available,
        backend,
        message,
        db_path: dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Futureboard Studio")
            .join("audio-plugin-registry.sqlite"),
        preset_root,
        default_scan_paths: default_scan_paths(),
    }
}

/// Registry service: scan VST3 + CLAP via [`scan_audio_plugin_paths`].
pub struct PluginRegistry;

/// Outcome of [`PluginRegistry::load_catalog`].
#[derive(Debug)]
pub enum CatalogLoad {
    /// SQLite cache loaded successfully (may still be empty).
    Loaded {
        catalog: crate::plugin_db::PluginCatalog,
        sqlite_ms: u128,
    },
    /// `index.dat` does not exist on disk yet.
    MissingDatabase { path: PathBuf },
    /// SQLite open/read failed — caller renders an error panel and offers
    /// rebuild/retry.
    Error { path: PathBuf, message: String },
}

impl PluginRegistry {
    pub fn host_status() -> NativeHostStatus {
        native_host_status()
    }

    /// Load the SQLite-backed plug-in catalog. Never scans, never touches
    /// plug-in binaries, never opens the VST3/CLAP SDK. Safe to call on a
    /// background executor. Distinct error states are returned so the picker
    /// can render `MissingDatabase` vs `Error(text)` vs `Loaded { empty }`.
    pub fn load_catalog() -> CatalogLoad {
        use crate::plugin_db::{
            database_exists, database_path, open_database_readonly, read_all, PluginCatalog,
        };
        let path = database_path();
        if !database_exists() {
            return CatalogLoad::MissingDatabase { path };
        }
        let started = std::time::Instant::now();
        let conn = match open_database_readonly() {
            Ok(c) => c,
            Err(e) => return CatalogLoad::Error { path, message: e },
        };
        let plugins = match read_all(&conn) {
            Ok(v) => v,
            Err(e) => {
                return CatalogLoad::Error {
                    path,
                    message: e.to_string(),
                }
            }
        };
        CatalogLoad::Loaded {
            catalog: PluginCatalog {
                plugins,
                loaded_at: std::time::Instant::now(),
                source_path: path,
            },
            sqlite_ms: started.elapsed().as_millis(),
        }
    }

    /// Compatibility shim used by callers that still consume the legacy
    /// `RegistryPlugin` shape — projects the SQLite catalog (or, if missing,
    /// the `.pst` files) into a flat `Vec`.
    pub fn load_cached() -> (Vec<RegistryPlugin>, i64) {
        use crate::plugin_db::{database_exists, last_scan_ms, open_database_readonly, read_all};
        if database_exists() {
            if let Ok(conn) = open_database_readonly() {
                if let Ok(rows) = read_all(&conn) {
                    let last = last_scan_ms(&conn).unwrap_or(0);
                    let plugins: Vec<RegistryPlugin> =
                        rows.iter().map(|e| e.to_registry_plugin()).collect();
                    return (plugins, last);
                }
            }
        }
        // Fallback to legacy `.pst` cache (kept for users upgrading from the
        // pre-SQLite build; remove once everyone has rescanned).
        let mut plugins = crate::preset::load_cached_plugins();
        plugins.sort_by(|a, b| {
            let kind = match (a.kind, b.kind) {
                (PluginKind::Instrument, PluginKind::Effect) => std::cmp::Ordering::Less,
                (PluginKind::Effect, PluginKind::Instrument) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            };
            kind.then_with(|| a.vendor.cmp(&b.vendor))
                .then_with(|| a.name.cmp(&b.name))
        });
        let last = plugins.iter().map(|p| p.scanned_at_ms).max().unwrap_or(0);
        (plugins, last)
    }

    /// Upsert the given plugins into the SQLite catalog inside one
    /// transaction. Used by the scanner once a scan completes.
    pub fn write_catalog(plugins: &[RegistryPlugin]) -> Result<(), String> {
        use crate::plugin_db::{open_database, upsert_plugins, PluginCatalogEntry};
        let mut conn = open_database()?;
        let entries: Vec<PluginCatalogEntry> =
            plugins.iter().map(PluginCatalogEntry::from).collect();
        upsert_plugins(&mut conn, &entries).map_err(|e| e.to_string())
    }

    /// Delete every plug-in row from the SQLite cache and remove all `.pst`
    /// files. Returns total entries dropped (sum of both sources).
    pub fn clear_cache() -> Result<u32, String> {
        use crate::plugin_db::{clear_with_run_record, database_exists, open_database};
        let mut removed_db = 0u32;
        if database_exists() {
            let mut conn = open_database()?;
            removed_db = clear_with_run_record(&mut conn).map_err(|e| e.to_string())?;
        }
        let removed_pst = crate::preset::clear_plugin_cache().unwrap_or(0);
        Ok(removed_db + removed_pst)
    }

    /// Count of rows whose backing binary cannot be opened anymore.
    pub fn cached_failed_count(plugins: &[RegistryPlugin]) -> u32 {
        plugins
            .iter()
            .filter(|p| p.status == PluginStatus::MissingPreset || !p.path.exists())
            .count() as u32
    }

    /// Scan default OS paths, or the provided folders (VST3 + CLAP).
    pub fn scan(requested_paths: Option<Vec<PathBuf>>) -> RegistryScanResult {
        Self::scan_with_progress(
            ScanOptions {
                paths: requested_paths,
                delete_presets_first: false,
                include_au: true,
                formats_only: None,
            },
            |_| {},
        )
    }

    /// Discover bundles, read metadata, validate, and write `.pst` presets with progress callbacks.
    pub fn scan_with_progress(
        options: ScanOptions,
        mut on_progress: impl FnMut(ScanProgress) + Send,
    ) -> RegistryScanResult {
        let scan_vst3_clap = options.formats_only.as_ref().is_none_or(|formats| {
            formats
                .iter()
                .any(|format| matches!(format, PluginFormat::Vst3 | PluginFormat::Clap))
        });
        let scan_au_requested = options.include_au
            && options
                .formats_only
                .as_ref()
                .is_none_or(|formats| formats.contains(&PluginFormat::Au));

        let mut au_cache_state = load_au_cache_state();
        let au_scan_available = cfg!(target_os = "macos");
        let scan_au =
            scan_au_requested && au_scan_available && should_auto_scan_au(&au_cache_state);

        let cached_plugins: Vec<RegistryPlugin> = if options.delete_presets_first {
            Vec::new()
        } else {
            Self::load_cached().0
        };
        let cached_vst3_clap_plugins: Vec<RegistryPlugin> = cached_plugins
            .iter()
            .filter(|plugin| plugin.format != PluginFormat::Au)
            .cloned()
            .collect();
        let cached_au_plugins: Vec<RegistryPlugin> = cached_plugins
            .iter()
            .filter(|plugin| plugin.format == PluginFormat::Au)
            .cloned()
            .collect();

        let mut scanned_paths = Vec::new();
        let mut failed = Vec::new();
        let mut plugins = Vec::new();
        let mut generated_presets = 0u32;
        let mut au_scan_error = None;
        let mut au_scan_crashed = false;

        if scan_vst3_clap {
            let requested: Vec<PathBuf> = options
                .paths
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(default_scan_paths);

            for path in requested {
                if path.exists() {
                    scanned_paths.push(path);
                } else {
                    failed.push(PluginScanFailure {
                        path: path.clone(),
                        error: "Path does not exist".to_string(),
                    });
                    on_progress(ScanProgress::Failed {
                        path,
                        error: "Path does not exist".to_string(),
                    });
                }
            }

            if options.delete_presets_first {
                if let Err(error) = clear_all_presets() {
                    failed.push(PluginScanFailure {
                        path: PathBuf::from("(presets)"),
                        error: format!("Failed to clear presets: {error}"),
                    });
                }
            }

            let _ = ensure_preset_folders();

            let bundles = discover_plugin_bundles(&scanned_paths);
            let bundle_total = bundles.len();
            on_progress(ScanProgress::Started { bundle_total });

            let scanned_at = now_ms();
            let mut pending = Vec::new();
            let mut seen = HashSet::new();
            let mut occupied_presets = HashSet::new();

            // Opt-in incremental scan: a map of each cached row's stored file
            // signature, keyed by plug-in path. Only loaded when the feature is
            // enabled, so the default full-scan path is untouched.
            let incremental = incremental_scan_enabled();
            let signature_by_path: HashMap<PathBuf, (Option<String>, Option<i64>)> = if incremental
            {
                use crate::plugin_db::{database_exists, open_database_readonly, read_all};
                if database_exists() {
                    open_database_readonly()
                        .ok()
                        .and_then(|conn| read_all(&conn).ok())
                        .map(|rows| {
                            rows.into_iter()
                                .map(|e| (e.path, (e.file_modified_at, e.file_size)))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    HashMap::new()
                }
            } else {
                HashMap::new()
            };

            for (index, bundle) in bundles.iter().enumerate() {
                on_progress(ScanProgress::ScanningBundle {
                    current: index + 1,
                    total: bundle_total.max(1),
                    path: bundle.clone(),
                });

                // Incremental reuse: if every cached row under this bundle is
                // still usable and signature-fresh, reuse the cached rows and
                // skip the (expensive, subprocess) isolated scan. Reused rows
                // still flow through the same dedup / preset / register path
                // below, so only the scanner launch is avoided.
                if incremental {
                    let reuse: Vec<RegistryPlugin> = cached_vst3_clap_plugins
                        .iter()
                        .filter(|p| p.path.starts_with(bundle))
                        .cloned()
                        .collect();
                    let reusable = bundle_is_reusable(reuse.iter().map(|p| {
                        let (cached_m, cached_s) = signature_by_path
                            .get(&p.path)
                            .cloned()
                            .unwrap_or((None, None));
                        let (current_m, current_s) = crate::plugin_db::file_signature(&p.path);
                        (
                            p.scan_status.is_usable(),
                            crate::plugin_db::signature_is_fresh(
                                cached_m.as_deref(),
                                cached_s,
                                current_m.as_deref(),
                                current_s,
                            ),
                        )
                    }));
                    if reusable {
                        for plugin in reuse {
                            let key = registry_display_key(&plugin);
                            if !seen.insert(key) {
                                continue;
                            }
                            let plugin = resolve_unique_preset_path(plugin, &mut occupied_presets);
                            pending.push(plugin);
                        }
                        continue;
                    }
                }

                match run_isolated_bundle_scan(bundle) {
                    Ok(infos) => {
                        for info in infos {
                            let mut plugin = registry_plugin_from_scan(&info, scanned_at);
                            let key = registry_display_key(&plugin);
                            if !seen.insert(key) {
                                continue;
                            }
                            plugin = resolve_unique_preset_path(plugin, &mut occupied_presets);
                            pending.push(plugin);
                        }
                    }
                    Err(error) => {
                        failed.push(PluginScanFailure {
                            path: bundle.clone(),
                            error: error.clone(),
                        });
                        on_progress(ScanProgress::Failed {
                            path: bundle.clone(),
                            error,
                        });
                    }
                }
            }

            let register_total = pending.len();
            for (index, mut plugin) in pending.into_iter().enumerate() {
                let current = index + 1;
                match register_plugin(&mut plugin) {
                    Ok(()) => {
                        generated_presets += 1;
                    }
                    Err(error) => {
                        plugin.scan_status = PluginScanStatus::Failed;
                        plugin.error_message = Some(error.clone());
                        failed.push(PluginScanFailure {
                            path: plugin.path.clone(),
                            error: error.clone(),
                        });
                        on_progress(ScanProgress::Failed {
                            path: plugin.path.clone(),
                            error,
                        });
                    }
                }

                plugins.push(plugin.clone());
                on_progress(ScanProgress::Registering {
                    current,
                    total: register_total.max(1),
                    name: plugin.name.clone(),
                    plugin,
                    generated_presets,
                });
            }
        } else {
            plugins.extend(cached_vst3_clap_plugins);
            if options.delete_presets_first {
                if let Err(error) = clear_all_presets() {
                    failed.push(PluginScanFailure {
                        path: PathBuf::from("(presets)"),
                        error: format!("Failed to clear presets: {error}"),
                    });
                }
            }
        }

        if scan_au {
            let au_outcome = run_isolated_format_scan(IsolatedScanRequest {
                format: PluginScanFormat::AudioUnit,
                paths: Vec::new(),
                validate_plugins: false,
            });
            let payload = au_outcome.payload;
            au_scan_crashed = payload.process_crashed;
            if payload.process_crashed {
                au_scan_error = payload
                    .error
                    .clone()
                    .or_else(|| Some("AudioUnit scan process crashed".into()));
                record_au_scan_failure(
                    &mut au_cache_state,
                    au_scan_error.clone().unwrap_or_default(),
                    true,
                );
                plugins.extend(cached_au_plugins);
            } else if let Some(error) = payload.error {
                au_scan_error = Some(error.clone());
                record_au_scan_failure(&mut au_cache_state, error, false);
                plugins.extend(cached_au_plugins);
            } else {
                let scanned_at = now_ms();
                for descriptor in payload.plugins {
                    let info = plugin_info_from_descriptor(&descriptor);
                    let mut plugin = registry_plugin_from_scan(&info, scanned_at);
                    plugin.scan_status = match descriptor.scan_status {
                        crate::scan::types::PluginScanStatus::Success => PluginScanStatus::Success,
                        crate::scan::types::PluginScanStatus::Crashed => PluginScanStatus::Crashed,
                        crate::scan::types::PluginScanStatus::Skipped => PluginScanStatus::Skipped,
                        _ => PluginScanStatus::Failed,
                    };
                    plugin.error_message = descriptor.error_message.clone();
                    plugins.push(plugin);
                }
                record_au_scan_success(&mut au_cache_state, scanned_at);
            }

            on_progress(ScanProgress::FormatFinished {
                format: PluginFormat::Au,
                success_count: plugins
                    .iter()
                    .filter(|plugin| plugin.format == PluginFormat::Au)
                    .count(),
                failed_count: payload.failures.len(),
                crashed_count: payload.crashed_plugins.len(),
                error: au_scan_error.clone(),
            });
        } else if scan_au_requested && au_cache_state.auto_scan_disabled {
            au_scan_error = Some(
                "AudioUnit auto-scan disabled after repeated crashes. Use Retry AudioUnit Scan."
                    .into(),
            );
            plugins.extend(cached_au_plugins);
        } else if !au_scan_available && scan_au_requested {
            au_scan_error = Some("AudioUnit scanning is unavailable on this platform.".into());
        }

        let _ = save_au_cache_state(&au_cache_state);

        plugins.sort_by(|a, b| {
            let kind = match (a.kind, b.kind) {
                (PluginKind::Instrument, PluginKind::Effect) => std::cmp::Ordering::Less,
                (PluginKind::Effect, PluginKind::Instrument) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            };
            kind.then_with(|| a.vendor.cmp(&b.vendor))
                .then_with(|| a.name.cmp(&b.name))
        });

        if scan_vst3_clap || scan_au {
            if let Err(err) = Self::write_catalog(&plugins) {
                failed.push(PluginScanFailure {
                    path: crate::plugin_db::database_path(),
                    error: format!("sqlite write: {err}"),
                });
            } else if std::env::var_os("FUTUREBOARD_PLUGIN_DB_DEBUG").is_some() {
                eprintln!(
                    "[plugin-db] wrote {} rows to {}",
                    plugins.len(),
                    crate::plugin_db::database_path().display()
                );
            }
        }

        RegistryScanResult {
            plugins,
            scanned_paths,
            failed,
            generated_presets,
            au_scan_error,
            au_scan_crashed,
            au_auto_scan_disabled: au_cache_state.auto_scan_disabled,
            au_scan_available,
        }
    }

    /// Scan AudioUnit plug-ins only. Safe to call when VST3/CLAP results should be preserved.
    pub fn scan_au_only() -> RegistryScanResult {
        let mut au_cache_state = load_au_cache_state();
        au_cache_state.auto_scan_disabled = false;
        let _ = save_au_cache_state(&au_cache_state);
        Self::scan_with_progress(
            ScanOptions {
                paths: None,
                delete_presets_first: false,
                include_au: true,
                formats_only: Some(vec![PluginFormat::Au]),
            },
            |_| {},
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vst3_instrument_category_normalization() {
        let cat = display_category(
            PluginFormat::Vst3,
            "Audio Module Class",
            Some("Audio Module Class"),
            Some("Instrument|Synth"),
        );
        assert_eq!(cat, "Instrument");
    }

    #[test]
    fn bundle_reuse_requires_all_entries_usable_and_fresh() {
        // All usable + fresh -> reuse.
        assert!(bundle_is_reusable([(true, true), (true, true)]));
    }

    #[test]
    fn bundle_reuse_rejected_when_any_entry_stale() {
        // One stale (changed binary) entry forces a full rescan of the bundle.
        assert!(!bundle_is_reusable([(true, true), (true, false)]));
    }

    #[test]
    fn bundle_reuse_rejected_when_any_entry_unusable() {
        // A prior failure/crash must be retried, never cached.
        assert!(!bundle_is_reusable([(true, true), (false, true)]));
    }

    #[test]
    fn bundle_with_no_cached_entries_is_not_reusable() {
        // A newly-appeared bundle (no cached rows) must be scanned.
        assert!(!bundle_is_reusable(std::iter::empty()));
    }
}
