//! Futureboard `.pst` preset files (FBPST format, aligned with Electron `PluginHostNative`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plugin_db::PluginScanStatus;
use crate::registry::{default_preset_root, RegistryPlugin};

const PRESET_MAGIC: &[u8; 5] = b"FBPST";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PresetMetadata<'a> {
    preset_format: &'static str,
    version: u32,
    created_at: i64,
    plugin_metadata: PresetPluginMetadata<'a>,
    plugin_state: PresetPluginState,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PresetPluginMetadata<'a> {
    id: &'a str,
    name: &'a str,
    vendor: &'a str,
    format: &'a str,
    category: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_category: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sub_categories: Option<&'a str>,
    kind: &'a str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    class_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'a str>,
    sdk_metadata_loaded: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PresetPluginState {
    encoding: &'static str,
    byte_length: u32,
    source: &'static str,
}

/// Ensure `Documents/Futureboard Studio/Audio Plug-ins/{VST3,CLAP}/{Instruments,Effects}` exist.
pub fn ensure_preset_folders() -> Result<(), String> {
    for folder in preset_subfolders() {
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn preset_subfolders() -> Vec<PathBuf> {
    let root = default_preset_root();
    [
        "VST3/Instruments",
        "VST3/Effects",
        "CLAP/Instruments",
        "CLAP/Effects",
    ]
    .into_iter()
    .map(|rel| root.join(rel))
    .collect()
}

/// Delete every `.pst` under the preset root. Returns number of files removed.
pub fn clear_all_presets() -> Result<u32, String> {
    let root = default_preset_root();
    if !root.exists() {
        return Ok(0);
    }
    clear_pst_files_recursive(&root)
}

fn clear_pst_files_recursive(dir: &Path) -> Result<u32, String> {
    let mut deleted = 0u32;
    let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            deleted = deleted.saturating_add(clear_pst_files_recursive(&path)?);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pst"))
        {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Validate that a plug-in binary exists before registration. Formats with no
/// module file (AU, addressed by component id) have nothing to check here.
pub fn validate_plugin_for_registration(plugin: &RegistryPlugin) -> Result<(), String> {
    if plugin.format.has_module_file() && !plugin.path.exists() {
        return Err(format!(
            "Plug-in binary is missing: {}",
            plugin.path.display()
        ));
    }
    Ok(())
}

/// Write `.pst` for this registry row (does not change [`RegistryPlugin::status`]).
pub fn write_preset(plugin: &RegistryPlugin) -> Result<(), String> {
    validate_plugin_for_registration(plugin)?;
    ensure_preset_folders()?;

    if let Some(parent) = plugin.preset_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let bytes = build_preset_binary(plugin);
    let tmp = plugin.preset_path.with_extension("pst.tmp");
    {
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, &plugin.preset_path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Validate and write `.pst`; marks the row as [`crate::registry::PluginStatus::PresetReady`].
pub fn register_plugin(plugin: &mut RegistryPlugin) -> Result<(), String> {
    write_preset(plugin)?;
    plugin.status = crate::registry::PluginStatus::PresetReady;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetMetadataOwned {
    #[serde(default)]
    created_at: i64,
    plugin_metadata: PresetPluginMetadataOwned,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetPluginMetadataOwned {
    id: String,
    name: String,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    raw_category: Option<String>,
    #[serde(default)]
    sub_categories: Option<String>,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    class_id: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    sdk_metadata_loaded: bool,
}

/// Read a single `.pst` and reconstruct its [`RegistryPlugin`] row. No plugin
/// binary is touched — the binary `status` reflects whether the path on disk
/// still exists.
pub fn read_preset_file(preset_path: &Path) -> Result<RegistryPlugin, String> {
    use crate::registry::{display_category, PluginFormat, PluginKind, PluginStatus};

    let bytes = fs::read(preset_path).map_err(|e| e.to_string())?;
    if bytes.len() < 24 || &bytes[..5] != PRESET_MAGIC {
        return Err("Not an FBPST preset".to_string());
    }
    let meta_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let meta_start = 24usize;
    let meta_end = meta_start
        .checked_add(meta_len)
        .ok_or_else(|| "Preset metadata length overflow".to_string())?;
    if bytes.len() < meta_end {
        return Err("Preset metadata truncated".to_string());
    }
    let parsed: PresetMetadataOwned =
        serde_json::from_slice(&bytes[meta_start..meta_end]).map_err(|e| e.to_string())?;

    let pm = parsed.plugin_metadata;
    let format = PluginFormat::from_str_lossy(&pm.format);
    let kind = match pm.kind.to_ascii_lowercase().as_str() {
        "instrument" => PluginKind::Instrument,
        _ => PluginKind::Effect,
    };
    let category = if pm.category.is_empty() {
        display_category(
            format,
            &pm.category,
            pm.raw_category.as_deref(),
            pm.sub_categories.as_deref(),
        )
    } else {
        pm.category.clone()
    };
    let binary_path = PathBuf::from(&pm.path);
    let status = if binary_path.exists() {
        PluginStatus::PresetReady
    } else {
        PluginStatus::MissingPreset
    };
    let scan_status = if binary_path.exists() {
        PluginScanStatus::Success
    } else {
        PluginScanStatus::MetadataOnly
    };

    Ok(RegistryPlugin {
        id: pm.id,
        name: pm.name,
        vendor: pm.vendor,
        format,
        category,
        raw_category: pm.raw_category,
        sub_categories: pm.sub_categories,
        kind,
        path: binary_path,
        class_id: pm.class_id,
        version: pm.version,
        sdk_metadata_loaded: pm.sdk_metadata_loaded,
        preset_path: preset_path.to_path_buf(),
        scanned_at_ms: parsed.created_at,
        status,
        scan_status,
        error_message: None,
    })
}

/// Walk the preset root and load every cached `.pst` row. This does **not**
/// touch any plug-in binary, scan default OS folders, or invoke the VST3/CLAP
/// SDK; it is safe to call on the UI thread when the cache is small.
pub fn load_cached_plugins() -> Vec<RegistryPlugin> {
    let mut out = Vec::new();
    let root = default_preset_root();
    if !root.exists() {
        return out;
    }
    collect_pst_files(&root, &mut |path| {
        if let Ok(plugin) = read_preset_file(path) {
            out.push(plugin);
        }
    });
    out
}

fn collect_pst_files(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pst_files(&path, visit);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pst"))
        {
            visit(&path);
        }
    }
}

/// Delete the entire preset cache directory tree. Used by Plugin Manager
/// "Clear Plugin Cache". Returns the number of `.pst` files removed.
pub fn clear_plugin_cache() -> Result<u32, String> {
    clear_all_presets()
}

fn build_preset_binary(plugin: &RegistryPlugin) -> Vec<u8> {
    let kind = match plugin.kind {
        crate::registry::PluginKind::Instrument => "instrument",
        crate::registry::PluginKind::Effect => "effect",
    };
    let metadata = PresetMetadata {
        preset_format: "Mochi preset: Futureboard",
        version: 1,
        created_at: plugin.scanned_at_ms,
        plugin_metadata: PresetPluginMetadata {
            id: &plugin.id,
            name: &plugin.name,
            vendor: &plugin.vendor,
            format: plugin.format.label(),
            category: &plugin.category,
            raw_category: plugin.raw_category.as_deref(),
            sub_categories: plugin.sub_categories.as_deref(),
            kind,
            path: plugin.path.display().to_string(),
            class_id: plugin.class_id.as_deref(),
            version: plugin.version.as_deref(),
            sdk_metadata_loaded: plugin.sdk_metadata_loaded,
        },
        plugin_state: PresetPluginState {
            encoding: "binary",
            byte_length: 0,
            source: "pending-native-instantiation",
        },
    };

    let meta = serde_json::to_vec(&metadata).unwrap_or_default();
    let mut header = [0u8; 24];
    header[..5].copy_from_slice(PRESET_MAGIC);
    header[6..8].copy_from_slice(&1u16.to_le_bytes());
    header[8..12].copy_from_slice(&(meta.len() as u32).to_le_bytes());
    header[12..16].copy_from_slice(&0u32.to_le_bytes());

    let mut out = Vec::with_capacity(24 + meta.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&meta);
    out
}
