//! Audio Plug-in Manager — GPUI-rendered native dialog (VST3/CLAP scan, Electron layout parity).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, size, svg, App, AppContext, Bounds, Context, Entity, FocusHandle, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle, StatefulInteractiveElement,
    Styled, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
};
use SpherePluginHost::load_au_cache_state;
use SpherePluginHost::preset::register_plugin;
use SpherePluginHost::registry::{
    NativeHostStatus, PluginFormat, PluginKind, PluginRegistry, PluginStatus, RegistryPlugin,
    RegistryScanResult, ScanOptions, ScanProgress,
};

use crate::assets;
use crate::components::controls::{fb_button, FbButtonKind};
use crate::components::plugin_format_badge::plugin_format_badge;
use crate::components::progress_dialog::{
    open_progress_dialog_window, open_standalone_progress_dialog_window, ProgressBarValue,
    ProgressDialogCancelCb, ProgressDialogOptions,
};
use crate::components::scroll_thumb::vertical_scrollbar_thumb;
use crate::components::text_input::{
    bind_mouse_selection, text_field_with_callbacks_and_ime, TextInputAction, TextInputCallbacks,
    TextInputState,
};
use crate::components::title_bar::external_window_titlebar;
use crate::theme::{self, Colors};

pub const PLUGIN_MANAGER_WINDOW_WIDTH: f32 = 980.0;
pub const PLUGIN_MANAGER_WINDOW_HEIGHT: f32 = 640.0;
pub const PLUGIN_MANAGER_WINDOW_MIN_WIDTH: f32 = 860.0;
pub const PLUGIN_MANAGER_WINDOW_MIN_HEIGHT: f32 = 520.0;

type VoidCb = Arc<dyn Fn(&(), &mut Window, &mut App) + 'static>;
type StrCb = Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>;

const SIDEBAR_WIDTH: f32 = 196.0;
const DETAILS_WIDTH: f32 = 248.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Vendor,
    Category,
    Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarFilter {
    All,
    Instrument,
    Effect,
    Format(PluginFormat),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FilterCounts {
    pub all: usize,
    pub instruments: usize,
    pub effects: usize,
    pub vst3: usize,
    pub clap: usize,
    pub au: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginScanMode {
    /// Scan folders and register missing `.pst` files (overwrites on conflict).
    Rescan,
    /// Delete all `.pst` presets, clear the list, then scan and register everything.
    RescanAll,
    /// Scan AudioUnit plug-ins only (macOS).
    RescanAu,
}

#[derive(Debug, Clone)]
pub struct PluginManagerDialogState {
    pub plugins: Vec<RegistryPlugin>,
    pub scan_paths: Vec<PathBuf>,
    pub status_text: String,
    pub scanning: bool,
    pub failed_count: u32,
    pub generated_presets: u32,
    pub scan_progress_current: usize,
    pub scan_progress_total: usize,
    pub scan_progress_label: String,
    pub sidebar_filter: SidebarFilter,
    pub sort_key: SortKey,
    pub sort_dir: SortDir,
    pub selected_id: Option<String>,
    pub host: NativeHostStatus,
    /// `created_at_ms` of the most recent `.pst` in the cache. `0` = no cache.
    pub last_scan_at_ms: i64,
    /// True once the cached index has been loaded (or determined to be empty).
    pub cache_loaded: bool,
    pub au_scan_available: bool,
    pub au_scan_error: Option<String>,
    pub au_auto_scan_disabled: bool,
}

impl PluginManagerDialogState {
    pub fn new_empty() -> Self {
        let host = PluginRegistry::host_status();
        let au_cache = load_au_cache_state();
        let status_text = if host.available {
            "Loading cached plug-in index…".to_string()
        } else {
            host.message.clone()
        };
        Self {
            scan_paths: host.default_scan_paths.clone(),
            status_text,
            scanning: false,
            failed_count: 0,
            generated_presets: 0,
            scan_progress_current: 0,
            scan_progress_total: 0,
            scan_progress_label: String::new(),
            plugins: Vec::new(),
            sidebar_filter: SidebarFilter::All,
            sort_key: SortKey::Name,
            sort_dir: SortDir::Asc,
            selected_id: None,
            host,
            last_scan_at_ms: 0,
            cache_loaded: false,
            au_scan_available: cfg!(target_os = "macos"),
            au_scan_error: au_cache.last_error.clone(),
            au_auto_scan_disabled: au_cache.auto_scan_disabled,
        }
    }

    /// Apply a cached `.pst` load to the dialog. Does not touch any plug-in
    /// binary or trigger an SDK scan.
    pub fn apply_cache_load(&mut self, plugins: Vec<RegistryPlugin>, last_scan_at_ms: i64) {
        self.failed_count = PluginRegistry::cached_failed_count(&plugins);
        self.last_scan_at_ms = last_scan_at_ms;
        let count = plugins.len();
        self.plugins = plugins;
        self.cache_loaded = true;
        self.scanning = false;
        self.status_text = if count == 0 {
            "No plugin index found. Click Scan Now to scan plugins.".to_string()
        } else {
            format!("{count} plug-in(s) cached.")
        };
    }

    pub fn apply_scan_result(&mut self, result: RegistryScanResult) {
        self.host = PluginRegistry::host_status();
        self.plugins = result.plugins;
        self.scan_paths = result.scanned_paths;
        self.failed_count = result.failed.len() as u32;
        self.generated_presets = result.generated_presets;
        self.au_scan_available = result.au_scan_available;
        self.au_scan_error = result.au_scan_error.clone();
        self.au_auto_scan_disabled = result.au_auto_scan_disabled;
        self.scanning = false;
        self.cache_loaded = true;
        self.last_scan_at_ms = self
            .plugins
            .iter()
            .map(|p| p.scanned_at_ms)
            .max()
            .unwrap_or(0);
        self.scan_progress_current = 0;
        self.scan_progress_total = 0;
        self.scan_progress_label.clear();

        let count = self.plugins.len();
        self.status_text = if let Some(au_error) = &result.au_scan_error {
            if count > 0 {
                format!("AudioUnit scan failed. VST3/CLAP results are still available. {au_error}")
            } else if self.failed_count > 0 {
                format!(
                    "Scan finished with {} path error(s). {au_error}",
                    self.failed_count
                )
            } else {
                format!("AudioUnit scan failed. {au_error}")
            }
        } else if count == 0 && self.failed_count > 0 {
            format!("Scan finished with {} path error(s).", self.failed_count)
        } else if count == 0 {
            "No plug-ins found in scan locations.".to_string()
        } else if self.failed_count > 0 {
            format!(
                "Found {} plug-in(s); {} path error(s).",
                count, self.failed_count
            )
        } else if self.generated_presets > 0 {
            format!(
                "Registered {} preset(s). {} plug-in(s) cached.",
                self.generated_presets, count
            )
        } else {
            format!("Found {} plug-in(s).", count)
        };

        if let Some(id) = &self.selected_id {
            if !self.plugins.iter().any(|p| &p.id == id) {
                self.selected_id = None;
            }
        }
    }

    pub fn begin_scan(&mut self, mode: PluginScanMode) {
        self.scanning = true;
        self.scan_progress_current = 0;
        self.scan_progress_total = 0;
        self.scan_progress_label.clear();
        self.failed_count = 0;
        if mode == PluginScanMode::RescanAll {
            self.plugins.clear();
            self.generated_presets = 0;
        }
        self.au_scan_error = None;
        self.status_text = match mode {
            PluginScanMode::Rescan => {
                "Scanning and registering VST3, CLAP, and AudioUnit plug-ins…".to_string()
            }
            PluginScanMode::RescanAll => {
                "Deleting presets and rescanning all plug-ins…".to_string()
            }
            PluginScanMode::RescanAu => "Scanning AudioUnit plug-ins…".to_string(),
        };
    }

    pub fn apply_scan_progress(&mut self, progress: &ScanProgress) {
        match progress {
            ScanProgress::Started { bundle_total } => {
                self.scan_progress_total = *bundle_total;
                self.scan_progress_current = 0;
                self.scan_progress_label = "Discovering plug-in bundles…".to_string();
            }
            ScanProgress::ScanningBundle {
                current,
                total,
                path,
            } => {
                self.scan_progress_current = *current;
                self.scan_progress_total = *total;
                self.scan_progress_label = format!(
                    "Reading metadata: {}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("bundle")
                );
            }
            ScanProgress::Registering {
                current,
                total,
                name,
                plugin,
                generated_presets,
            } => {
                self.scan_progress_current = *current;
                self.scan_progress_total = *total;
                self.scan_progress_label = name.clone();
                self.generated_presets = *generated_presets;
                if let Some(existing) = self.plugins.iter_mut().find(|p| p.id == plugin.id) {
                    *existing = plugin.clone();
                } else {
                    self.plugins.push(plugin.clone());
                }
            }
            ScanProgress::Failed { .. } => {}
            ScanProgress::FormatFinished {
                format,
                success_count,
                failed_count,
                crashed_count,
                error,
            } => {
                if *format == PluginFormat::Au {
                    self.au_scan_error = error.clone();
                    if *crashed_count > 0 {
                        self.status_text = format!(
                            "AudioUnit scan process crashed. VST3/CLAP results are still available."
                        );
                    } else if let Some(message) = error {
                        self.status_text = format!(
                            "AudioUnit scan failed ({success_count} ok, {failed_count} failed): {message}"
                        );
                    }
                }
            }
        }
    }

    pub fn scan_progress_fraction(&self) -> f32 {
        if self.scan_progress_total == 0 {
            return 0.0;
        }
        (self.scan_progress_current as f32 / self.scan_progress_total as f32).clamp(0.0, 1.0)
    }

    pub fn counts(&self) -> FilterCounts {
        FilterCounts {
            all: self.plugins.len(),
            instruments: self
                .plugins
                .iter()
                .filter(|p| p.kind == PluginKind::Instrument)
                .count(),
            effects: self
                .plugins
                .iter()
                .filter(|p| p.kind == PluginKind::Effect)
                .count(),
            vst3: self
                .plugins
                .iter()
                .filter(|p| p.format == PluginFormat::Vst3)
                .count(),
            clap: self
                .plugins
                .iter()
                .filter(|p| p.format == PluginFormat::Clap)
                .count(),
            au: self
                .plugins
                .iter()
                .filter(|p| p.format == PluginFormat::Au)
                .count(),
        }
    }

    pub fn selected_plugin(&self) -> Option<&RegistryPlugin> {
        let id = self.selected_id.as_ref()?;
        self.plugins.iter().find(|p| &p.id == id)
    }

    pub fn visible_plugins<'a>(&'a self, query: &str) -> Vec<&'a RegistryPlugin> {
        let mut result: Vec<&RegistryPlugin> = self.plugins.iter().collect();

        result.retain(|p| match &self.sidebar_filter {
            SidebarFilter::All => true,
            SidebarFilter::Instrument => p.kind == PluginKind::Instrument,
            SidebarFilter::Effect => p.kind == PluginKind::Effect,
            SidebarFilter::Format(fmt) => p.format == *fmt,
        });

        let q = query.trim().to_ascii_lowercase();
        if !q.is_empty() {
            result.retain(|p| {
                let hay = format!(
                    "{} {} {} {} {}",
                    p.name,
                    p.vendor,
                    p.display_category(),
                    p.raw_category.as_deref().unwrap_or(""),
                    p.path.display()
                )
                .to_ascii_lowercase();
                hay.contains(&q)
            });
        }

        result.sort_by(|a, b| {
            let cmp = match self.sort_key {
                SortKey::Name => a.name.cmp(&b.name),
                SortKey::Vendor => a.vendor.cmp(&b.vendor),
                SortKey::Category => a.display_category().cmp(&b.display_category()),
                SortKey::Format => a.format.label().cmp(b.format.label()),
            };
            match self.sort_dir {
                SortDir::Asc => cmp,
                SortDir::Desc => cmp.reverse(),
            }
        });

        result
    }
}

fn reveal_path_for_plugin(plugin: &RegistryPlugin) -> &Path {
    if plugin.preset_path.exists() {
        &plugin.preset_path
    } else {
        &plugin.path
    }
}

#[derive(Clone)]
pub struct PluginManagerCallbacks {
    pub on_close: VoidCb,
    pub on_rescan: VoidCb,
    pub on_select_id: StrCb,
    pub on_sidebar_filter: Arc<dyn Fn(&SidebarFilter, &mut Window, &mut App) + 'static>,
    pub on_sort: Arc<dyn Fn(&SortKey, &mut Window, &mut App) + 'static>,
    pub on_insert: StrCb,
    pub on_open_editor: StrCb,
    pub on_reveal_preset: StrCb,
    pub on_register_plugin: StrCb,
    pub on_rescan_all: VoidCb,
    pub on_rescan_au: VoidCb,
    pub on_clear_cache: VoidCb,
    pub on_open_db_folder: VoidCb,
}

fn icon(path: &'static str, size: f32, color: gpui::Rgba) -> impl IntoElement {
    svg().path(path).text_color(color).size(px(size))
}

fn scan_progress_bar(state: &PluginManagerDialogState) -> impl IntoElement {
    let fraction = state.scan_progress_fraction();
    let pct = (fraction * 100.0).round() as u32;
    let label = if state.scan_progress_total > 0 {
        format!(
            "Scanning {} of {} — {}",
            state.scan_progress_current.min(state.scan_progress_total),
            state.scan_progress_total,
            state.scan_progress_label
        )
    } else {
        state.scan_progress_label.clone()
    };

    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(8.0))
        .border_b(px(1.0))
        .border_color(Colors::divider())
        .bg(Colors::surface_input())
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(Colors::text_secondary())
                        .truncate()
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::accent_primary())
                        .child(format!("{pct}%")),
                ),
        )
        .child(
            div()
                .h(px(4.0))
                .rounded_sm()
                .bg(Colors::surface_panel_alt())
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(fraction.max(0.02)))
                        .bg(Colors::accent_primary()),
                ),
        )
}

fn status_badge(label: &'static str, ready: bool) -> impl IntoElement {
    let (fg, bg) = if ready {
        (Colors::text_primary(), Colors::surface_input())
    } else {
        (Colors::text_faint(), Colors::surface_panel_alt())
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .min_w(px(72.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded_sm()
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(bg)
        .text_size(px(10.0))
        .font_weight(if ready {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .text_color(fg)
        .child(label)
}

fn format_relative_time(ms: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(ms);
    let delta = (now_ms - ms).max(0);
    let secs = delta / 1000;
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins} min ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

fn rgba_warning_soft() -> gpui::Rgba {
    gpui::rgba(0xE5C07B18)
}

fn sidebar_section(label: &'static str, children: Vec<impl IntoElement>) -> impl IntoElement {
    div()
        .mb(px(4.0))
        .child(
            div()
                .px(px(12.0))
                .pt(px(8.0))
                .pb(px(2.0))
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_faint())
                .child(label),
        )
        .child(div().px(px(4.0)).children(children))
}

fn sidebar_item(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    count: usize,
    active: bool,
    disabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .w_full()
        .px(px(8.0))
        .py(px(5.0))
        .rounded_md()
        .when(active, |el| el.bg(Colors::accent_muted()))
        .when(!disabled, |el| {
            el.cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| s.bg(Colors::surface_control_hover()))
                .on_click(on_click)
        })
        .when(disabled, |el| el.opacity(0.35))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.0))
                .text_color(if active {
                    Colors::accent_primary()
                } else {
                    Colors::text_dim()
                })
                .child(label),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(if active {
                    Colors::accent_primary()
                } else {
                    Colors::text_faint()
                })
                .child(format!("{count}")),
        )
}

fn col_header(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    key: SortKey,
    state: &PluginManagerDialogState,
    on_sort: Arc<dyn Fn(&SortKey, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let active = state.sort_key == key;
    let on_sort = on_sort.clone();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .cursor(gpui::CursorStyle::PointingHand)
        .on_click(move |_, window, cx| on_sort(&key, window, cx))
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if active {
            Colors::accent_primary()
        } else {
            Colors::text_faint()
        })
        .child(label)
        .child(div().text_size(px(9.0)).child(if active {
            match state.sort_dir {
                SortDir::Asc => "▲",
                SortDir::Desc => "▼",
            }
        } else {
            "⇅"
        }))
}

fn details_panel(plugin: &RegistryPlugin, callbacks: &PluginManagerCallbacks) -> impl IntoElement {
    let insert_enabled = plugin.supports_insert();
    let editor_enabled = plugin.supports_editor();
    let insert_cb = callbacks.on_insert.clone();
    let editor_cb = callbacks.on_open_editor.clone();
    let reveal_cb = callbacks.on_reveal_preset.clone();
    let register_cb = callbacks.on_register_plugin.clone();
    let id_insert = plugin.id.clone();
    let id_editor = plugin.id.clone();
    let id_reveal = plugin.id.clone();
    let id_register = plugin.id.clone();
    let can_register = plugin.status == PluginStatus::MissingPreset && plugin.path.exists();

    div()
        .flex()
        .flex_col()
        .w(px(DETAILS_WIDTH))
        .min_w(px(DETAILS_WIDTH))
        .border_l(px(1.0))
        .border_color(Colors::divider())
        .bg(Colors::surface_panel_alt())
        .child(
            div()
                .px(px(12.0))
                .py(px(10.0))
                .border_b(px(1.0))
                .border_color(Colors::divider())
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_primary())
                .child("Plug-in Details"),
        )
        .child(
            div()
                .id("plugin-manager-details-scroll")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .px(px(12.0))
                .py(px(10.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(detail_row("Name", &plugin.name))
                .child(detail_row("Vendor", &plugin.vendor))
                .child(detail_row("Category", &plugin.display_category()))
                .when_some(plugin.raw_category.as_ref(), |this, raw| {
                    this.child(detail_row("SDK Category", raw))
                })
                .child(detail_row("Format", plugin.format.label()))
                .child(detail_row(
                    "Kind",
                    match plugin.kind {
                        PluginKind::Instrument => "Instrument",
                        PluginKind::Effect => "Effect",
                    },
                ))
                .child(detail_row("Path", &plugin.path.display().to_string()))
                .when_some(plugin.class_id.as_ref(), |this, cid| {
                    this.child(detail_row("Class ID", cid))
                })
                .when_some(plugin.version.as_ref(), |this, ver| {
                    this.child(detail_row("Version", ver))
                })
                .child(detail_row(
                    "Preset",
                    &plugin.preset_path.display().to_string(),
                ))
                .child(detail_row(
                    "Status",
                    match plugin.status {
                        PluginStatus::PresetReady => "Available",
                        PluginStatus::MissingPreset => "Missing preset",
                    },
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .px(px(12.0))
                .py(px(10.0))
                .border_t(px(1.0))
                .border_color(Colors::divider())
                .child(fb_button(
                    "plugin-mgr-insert",
                    "Insert on Selected Track",
                    FbButtonKind::Primary,
                    insert_enabled,
                    move |_, window, cx| insert_cb(&id_insert, window, cx),
                ))
                .child(fb_button(
                    "plugin-mgr-editor",
                    "Open Plug-in Editor",
                    FbButtonKind::Default,
                    editor_enabled,
                    move |_, window, cx| editor_cb(&id_editor, window, cx),
                ))
                .child(fb_button(
                    "plugin-mgr-register",
                    "Validate & Register",
                    FbButtonKind::Primary,
                    can_register,
                    move |_, window, cx| register_cb(&id_register, window, cx),
                ))
                .child(fb_button(
                    "plugin-mgr-reveal",
                    if plugin.status == PluginStatus::PresetReady {
                        "Reveal Preset in Explorer"
                    } else {
                        "Reveal Plug-in in Explorer"
                    },
                    FbButtonKind::Default,
                    plugin.path.exists() || plugin.preset_path.exists(),
                    move |_, window, cx| reveal_cb(&id_reveal, window, cx),
                ))
                .when(!editor_enabled, |this| {
                    this.child(
                        div()
                            .text_size(px(9.5))
                            .text_color(Colors::text_faint())
                            .child("Editor: VST3 plug-ins with an available preset only."),
                    )
                }),
        )
}

fn detail_row(label: &'static str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_faint())
                .child(label),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(Colors::text_secondary())
                .child(value.to_string()),
        )
}

fn reveal_in_os(path: &Path) {
    reveal_preset_in_os(path);
}

fn reveal_preset_in_os(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,\"{}\"", path.display()))
                .spawn();
        } else {
            let _ = std::process::Command::new("explorer")
                .arg(format!("\"{}\"", path.display()))
                .spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(if path.is_file() { "-R" } else { "" })
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = if path.is_file() {
            std::process::Command::new("xdg-open")
                .arg(path.parent().unwrap_or(path))
                .spawn()
        } else {
            std::process::Command::new("xdg-open").arg(path).spawn()
        };
    }
}

/// Main plug-in manager body (toolbar, sidebar, list, optional details, status bar).
pub fn plugin_manager_panel(
    state: &PluginManagerDialogState,
    search_input: &TextInputState,
    search_focused: bool,
    search_callbacks: TextInputCallbacks,
    search_ime_target: Entity<PluginManagerWindow>,
    callbacks: PluginManagerCallbacks,
    sidebar_scroll: &ScrollHandle,
    list_scroll: &ScrollHandle,
) -> impl IntoElement {
    let rescan = callbacks.on_rescan.clone();
    let rescan_all = callbacks.on_rescan_all.clone();
    let rescan_au = callbacks.on_rescan_au.clone();
    let clear_cache = callbacks.on_clear_cache.clone();
    let open_db_folder = callbacks.on_open_db_folder.clone();
    let counts = state.counts();
    let visible = state.visible_plugins(&search_input.value);
    let visible_len = visible.len();
    let selected = state.selected_plugin();
    let filter_cb = callbacks.on_sidebar_filter.clone();
    let sort_cb = callbacks.on_sort.clone();

    let sidebar_all = filter_cb.clone();
    let sidebar_inst = filter_cb.clone();
    let sidebar_fx = filter_cb.clone();
    let sidebar_vst3 = filter_cb.clone();
    let sidebar_clap = filter_cb.clone();
    let sidebar_au = filter_cb.clone();

    let sidebar_thumb_scroll = sidebar_scroll.clone();
    let list_thumb_scroll = list_scroll.clone();

    let mut list_rows: Vec<gpui::AnyElement> = Vec::new();
    if visible.is_empty() {
        list_rows.push(
            div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(120.0))
                .text_size(px(11.0))
                .text_color(Colors::text_faint())
                .child(if state.scanning {
                    "Scanning… plug-ins will appear here one by one."
                } else if state.plugins.is_empty() && state.cache_loaded {
                    "No plugin index found. Click Scan Now to scan plugins."
                } else if state.plugins.is_empty() {
                    "Loading cached plug-in index…"
                } else {
                    "No plug-ins match the current filter."
                })
                .into_any_element(),
        );
    } else {
        let select_cb = callbacks.on_select_id.clone();
        for (row_index, plugin) in visible.into_iter().enumerate() {
            let pid = plugin.id.clone();
            let selected_row = state.selected_id.as_deref() == Some(pid.as_str());
            let kind_icon = match plugin.kind {
                PluginKind::Instrument => assets::ICON_MUSIC_PATH,
                PluginKind::Effect => assets::ICON_SLIDERS_HORIZONTAL_PATH,
            };
            let kind_color = match plugin.kind {
                PluginKind::Instrument => Colors::accent_primary(),
                PluginKind::Effect => Colors::status_success(),
            };
            let reveal = callbacks.on_reveal_preset.clone();
            let reveal_id = plugin.id.clone();
            let status_ready = plugin.status == PluginStatus::PresetReady;

            list_rows.push(
                div()
                    .id(("plugin-row", row_index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .min_h(px(40.0))
                    .py(px(4.0))
                    .px(px(12.0))
                    .gap(px(8.0))
                    .border_b(px(1.0))
                    .border_color(Colors::divider())
                    .when(selected_row, |el| el.bg(Colors::accent_muted()))
                    .when(!selected_row, |el| {
                        el.hover(|s| s.bg(Colors::surface_control_hover()))
                    })
                    .cursor(gpui::CursorStyle::PointingHand)
                    .on_click({
                        let select_cb = select_cb.clone();
                        let pid = pid.clone();
                        move |_, window, cx| select_cb(&pid, window, cx)
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .min_w_0()
                            .flex_1()
                            .child(icon(kind_icon, 12.0, kind_color))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(Colors::text_primary())
                                    .truncate()
                                    .child(plugin.name.clone()),
                            ),
                    )
                    .child(
                        div()
                            .w(px(110.0))
                            .text_size(px(11.0))
                            .text_color(Colors::text_dim())
                            .truncate()
                            .child(plugin.vendor.clone()),
                    )
                    .child(
                        div()
                            .w(px(100.0))
                            .text_size(px(11.0))
                            .text_color(Colors::text_dim())
                            .truncate()
                            .child(plugin.display_category()),
                    )
                    .child(
                        div()
                            .w(px(72.0))
                            .flex()
                            .items_center()
                            .child(plugin_format_badge(plugin.format)),
                    )
                    .child(
                        div()
                            .id(("plugin-status", row_index))
                            .w(px(88.0))
                            .flex()
                            .items_center()
                            .cursor(gpui::CursorStyle::PointingHand)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                reveal(&reveal_id, window, cx);
                            })
                            .child(status_badge(
                                if status_ready { "Available" } else { "Missing" },
                                status_ready,
                            )),
                    )
                    .into_any_element(),
            );
        }
    }

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .bg(Colors::surface_canvas())
        .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .h(px(40.0))
                        .px(px(12.0))
                        .border_b(px(1.0))
                        .border_color(Colors::divider())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(text_field_with_callbacks_and_ime(
                                    search_input,
                                    search_focused,
                                    search_callbacks,
                                    search_ime_target,
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(Colors::text_faint())
                                .child(format!(
                                    "{visible_len} plug-in{}",
                                    if visible_len == 1 { "" } else { "s" }
                                )),
                        )
                        .child(fb_button(
                            "plugin-manager-scan-now",
                            if state.scanning {
                                "Scanning…"
                            } else {
                                "Scan Now"
                            },
                            FbButtonKind::Primary,
                            !state.scanning,
                            move |_, window, cx| rescan(&(), window, cx),
                        ))
                        .child(fb_button(
                            "plugin-manager-full-rescan",
                            "Full Rescan",
                            FbButtonKind::Default,
                            !state.scanning,
                            move |_, window, cx| rescan_all(&(), window, cx),
                        ))
                        .when(state.au_scan_available, |row| {
                            row.child(fb_button(
                                "plugin-manager-retry-au",
                                if state.au_auto_scan_disabled {
                                    "Retry AudioUnit Scan"
                                } else {
                                    "Scan AudioUnit"
                                },
                                FbButtonKind::Default,
                                !state.scanning,
                                move |_, window, cx| rescan_au(&(), window, cx),
                            ))
                        })
                        .child(fb_button(
                            "plugin-manager-clear-cache",
                            "Clear Database",
                            FbButtonKind::Default,
                            !state.scanning && !state.plugins.is_empty(),
                            move |_, window, cx| clear_cache(&(), window, cx),
                        ))
                        .child(fb_button(
                            "plugin-manager-open-db-folder",
                            "Open DB Folder",
                            FbButtonKind::Default,
                            !state.scanning,
                            move |_, window, cx| open_db_folder(&(), window, cx),
                        )),
                )
                .when(state.scanning, |panel| panel.child(scan_progress_bar(state)))
                .when(state.au_auto_scan_disabled && state.au_scan_available, |panel| {
                    panel.child(
                        div()
                            .px(px(12.0))
                            .py(px(6.0))
                            .border_b(px(1.0))
                            .border_color(Colors::divider())
                            .bg(rgba_warning_soft())
                            .text_size(px(10.5))
                            .text_color(Colors::status_warning())
                            .child(
                                "AudioUnit auto-scan disabled after repeated crashes. Use Retry AudioUnit Scan.",
                            ),
                    )
                })
                .when(
                    state.au_scan_error.is_some() && !state.scanning && state.au_scan_available,
                    |panel| {
                        let message = state.au_scan_error.clone().unwrap_or_default();
                        panel.child(
                            div()
                                .px(px(12.0))
                                .py(px(6.0))
                                .border_b(px(1.0))
                                .border_color(Colors::divider())
                                .bg(Colors::surface_input())
                                .text_size(px(10.5))
                                .text_color(Colors::status_warning())
                                .child(message),
                        )
                    },
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_h(px(0.0))
                                .w(px(SIDEBAR_WIDTH))
                                .border_r(px(1.0))
                                .border_color(Colors::divider())
                                .bg(Colors::surface_panel_alt())
                                .child(
                                    div()
                                        .flex_1()
                                        .min_h(px(0.0))
                                        .relative()
                                        .child(
                                            div()
                                                .id("plugin-manager-sidebar-scroll")
                                                .size_full()
                                                .overflow_y_scroll()
                                                .track_scroll(sidebar_scroll)
                                                .child(
                                                    div()
                                                        .py(px(4.0))
                                                        .child(sidebar_section(
                                                    "Library",
                                                    vec![
                                                        sidebar_item(
                                                            "pm-filter-all",
                                                            "All Plug-ins",
                                                            counts.all,
                                                            state.sidebar_filter == SidebarFilter::All,
                                                            false,
                                                            move |_, w, cx| {
                                                                sidebar_all(
                                                                    &SidebarFilter::All,
                                                                    w,
                                                                    cx,
                                                                )
                                                            },
                                                        )
                                                        .into_any_element(),
                                                    ],
                                                ))
                                                .child(sidebar_section(
                                                    "Kind",
                                                    vec![
                                                        sidebar_item(
                                                            "pm-filter-inst",
                                                            "Instruments",
                                                            counts.instruments,
                                                            state.sidebar_filter
                                                                == SidebarFilter::Instrument,
                                                            false,
                                                            move |_, w, cx| {
                                                                sidebar_inst(
                                                                    &SidebarFilter::Instrument,
                                                                    w,
                                                                    cx,
                                                                )
                                                            },
                                                        )
                                                        .into_any_element(),
                                                        sidebar_item(
                                                            "pm-filter-fx",
                                                            "Effects",
                                                            counts.effects,
                                                            state.sidebar_filter == SidebarFilter::Effect,
                                                            false,
                                                            move |_, w, cx| {
                                                                sidebar_fx(
                                                                    &SidebarFilter::Effect,
                                                                    w,
                                                                    cx,
                                                                )
                                                            },
                                                        )
                                                        .into_any_element(),
                                                    ],
                                                ))
                                                .child(sidebar_section(
                                                    "Format",
                                                    vec![
                                                        sidebar_item(
                                                            "pm-filter-vst3",
                                                            "VST3",
                                                            counts.vst3,
                                                            state.sidebar_filter
                                                                == SidebarFilter::Format(PluginFormat::Vst3),
                                                            false,
                                                            move |_, w, cx| {
                                                                sidebar_vst3(
                                                                    &SidebarFilter::Format(PluginFormat::Vst3),
                                                                    w,
                                                                    cx,
                                                                )
                                                            },
                                                        )
                                                        .into_any_element(),
                                                        sidebar_item(
                                                            "pm-filter-clap",
                                                            "CLAP",
                                                            counts.clap,
                                                            state.sidebar_filter
                                                                == SidebarFilter::Format(PluginFormat::Clap),
                                                            false,
                                                            move |_, w, cx| {
                                                                sidebar_clap(
                                                                    &SidebarFilter::Format(PluginFormat::Clap),
                                                                    w,
                                                                    cx,
                                                                )
                                                            },
                                                        )
                                                        .into_any_element(),
                                                        sidebar_item(
                                                            "pm-filter-au",
                                                            if state.au_scan_available {
                                                                "AU"
                                                            } else {
                                                                "AU (Unavailable)"
                                                            },
                                                            counts.au,
                                                            state.sidebar_filter
                                                                == SidebarFilter::Format(PluginFormat::Au),
                                                            !state.au_scan_available,
                                                            move |_, w, cx| {
                                                                sidebar_au(
                                                                    &SidebarFilter::Format(PluginFormat::Au),
                                                                    w,
                                                                    cx,
                                                                )
                                                            },
                                                        )
                                                        .into_any_element(),
                                                    ],
                                                ))
                                                .child(
                                                    div()
                                                        .border_t(px(1.0))
                                                        .border_color(Colors::divider())
                                                        .child(
                                                            sidebar_section(
                                                                "Scan Locations",
                                                                if state.scan_paths.is_empty() {
                                                                    vec![div()
                                                                        .px(px(10.0))
                                                                        .py(px(4.0))
                                                                        .text_size(px(10.0))
                                                                        .text_color(Colors::text_faint())
                                                                        .child("No scan paths detected")
                                                                        .into_any_element()]
                                                                } else {
                                                                    state
                                                                        .scan_paths
                                                                        .iter()
                                                                        .enumerate()
                                                                        .map(|(i, path)| {
                                                                            div()
                                                                                .flex()
                                                                                .flex_row()
                                                                                .items_center()
                                                                                .gap(px(6.0))
                                                                                .px(px(10.0))
                                                                                .py(px(4.0))
                                                                                .id(("scan-path", i))
                                                                                .child(icon(
                                                                                    assets::ICON_FOLDER_PATH,
                                                                                    11.0,
                                                                                    Colors::text_faint(),
                                                                                ))
                                                                                .child(
                                                                                    div()
                                                                                        .text_size(px(10.0))
                                                                                        .text_color(Colors::text_faint())
                                                                                        .truncate()
                                                                                        .child(path.display().to_string()),
                                                                                )
                                                                                .into_any_element()
                                                                        })
                                                                        .collect()
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            div()
                                                                .px(px(8.0))
                                                                .pb(px(8.0))
                                                                .child(
                                                                    fb_button(
                                                                        "pm-add-location",
                                                                        "+ Add Location",
                                                                        FbButtonKind::Default,
                                                                        false,
                                                                        |_, _, _| {},
                                                                    ),
                                                                ),
                                                        ),
                                                ),
                                            ),
                                        )
                                        .child(vertical_scrollbar_thumb(sidebar_thumb_scroll)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(px(32.0))
                                        .px(px(12.0))
                                        .border_b(px(1.0))
                                        .border_color(Colors::divider())
                                        .bg(Colors::surface_input())
                                        .child(
                                            div().flex_1().child(col_header(
                                                "pm-sort-name",
                                                "Name",
                                                SortKey::Name,
                                                state,
                                                sort_cb.clone(),
                                            )),
                                        )
                                        .child(
                                            div().w(px(110.0)).child(col_header(
                                                "pm-sort-vendor",
                                                "Vendor",
                                                SortKey::Vendor,
                                                state,
                                                sort_cb.clone(),
                                            )),
                                        )
                                        .child(
                                            div().w(px(100.0)).child(col_header(
                                                "pm-sort-cat",
                                                "Category",
                                                SortKey::Category,
                                                state,
                                                sort_cb.clone(),
                                            )),
                                        )
                                        .child(
                                            div().w(px(72.0)).child(col_header(
                                                "pm-sort-fmt",
                                                "Format",
                                                SortKey::Format,
                                                state,
                                                sort_cb,
                                            )),
                                        )
                                        .child(
                                            div()
                                                .w(px(88.0))
                                                .text_size(px(10.0))
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(Colors::text_faint())
                                                .child("Status"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_h(px(0.0))
                                        .relative()
                                        .child(
                                            div()
                                                .id("plugin-manager-list-scroll")
                                                .size_full()
                                                .overflow_y_scroll()
                                                .track_scroll(list_scroll)
                                                .children(list_rows),
                                        )
                                        .child(vertical_scrollbar_thumb(list_thumb_scroll)),
                                ),
                        )
                        .when_some(selected, |panel, plugin| {
                            panel.child(details_panel(plugin, &callbacks))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(32.0))
                        .px(px(12.0))
                        .border_t(px(1.0))
                        .border_color(Colors::divider())
                        .bg(Colors::surface_input())
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_size(px(10.0))
                                .text_color(Colors::text_faint())
                                .child(state.status_text.clone())
                                .when(state.failed_count > 0, |el| {
                                    el.child(
                                        div()
                                            .text_color(Colors::status_warning())
                                            .child(format!("• {} failed", state.failed_count)),
                                    )
                                })
                                .when(state.generated_presets > 0, |el| {
                                    el.child(
                                        div()
                                            .text_color(Colors::accent_primary())
                                            .child(format!(
                                                "• {} generated",
                                                state.generated_presets
                                            )),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(10.0))
                                .text_size(px(10.0))
                                .text_color(Colors::text_faint())
                                .child(
                                    div()
                                        .truncate()
                                        .max_w(px(360.0))
                                        .child(SpherePluginHost::database_path()
                                            .display()
                                            .to_string()),
                                )
                                .when(state.last_scan_at_ms > 0, |el| {
                                    el.child(div().child(format!(
                                        "Last scan: {}",
                                        format_relative_time(state.last_scan_at_ms)
                                    )))
                                })
                                .child(div().child(format!("{} cached", state.plugins.len())))
                                .when(state.failed_count > 0, |el| {
                                    el.child(
                                        div()
                                            .text_color(Colors::status_warning())
                                            .child(format!("{} missing", state.failed_count)),
                                    )
                                }),
                        ),
                )
}

pub struct PluginManagerWindow {
    pub state: PluginManagerDialogState,
    search_input: TextInputState,
    focus_handle: FocusHandle,
    initial_cache_loaded: bool,
    sidebar_scroll: ScrollHandle,
    list_scroll: ScrollHandle,
}

impl PluginManagerWindow {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            state: PluginManagerDialogState::new_empty(),
            search_input: TextInputState::new("plugin-manager-search", cx.focus_handle())
                .with_placeholder("Search plug-ins..."),
            focus_handle: cx.focus_handle(),
            initial_cache_loaded: false,
            sidebar_scroll: ScrollHandle::new(),
            list_scroll: ScrollHandle::new(),
        }
    }

    /// Read the `.pst` cache on a background thread and apply it to the
    /// dialog. No plug-in binary is touched and no SDK scan is performed.
    fn arm_cache_load(cx: &mut Context<Self>) {
        let debug = std::env::var_os("FUTUREBOARD_PLUGIN_MANAGER_DEBUG").is_some();
        let started = std::time::Instant::now();
        cx.spawn(async move |this, cx| {
            let (plugins, last_ms) = cx
                .background_executor()
                .spawn(async { PluginRegistry::load_cached() })
                .await;
            let count = plugins.len();
            let _ = this.update(cx, |win, cx| {
                win.state.apply_cache_load(plugins, last_ms);
                cx.notify();
            });
            if debug {
                eprintln!(
                    "[plugin-manager] cache_loaded plugins={count} load_ms={}",
                    started.elapsed().as_millis()
                );
            }
        })
        .detach();
    }

    /// Discover, validate, and register plug-ins on a worker thread; stream progress to the UI.
    fn arm_background_scan(cx: &mut Context<Self>, mode: PluginScanMode) {
        let options = ScanOptions {
            paths: None,
            delete_presets_first: mode == PluginScanMode::RescanAll,
            include_au: mode != PluginScanMode::RescanAu || cfg!(target_os = "macos"),
            formats_only: if mode == PluginScanMode::RescanAu {
                Some(vec![PluginFormat::Au])
            } else {
                None
            },
        };

        cx.spawn(async move |this, cx| {
            let (tx, rx) = std::sync::mpsc::channel::<ScanProgress>();
            let scan_options = options;
            let handle = std::thread::spawn(move || {
                PluginRegistry::scan_with_progress(scan_options, |progress| {
                    let _ = tx.send(progress);
                })
            });

            loop {
                while let Ok(progress) = rx.try_recv() {
                    let _ = this.update(cx, |win, cx| {
                        win.state.apply_scan_progress(&progress);
                        cx.notify();
                    });
                }
                if handle.is_finished() {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(32))
                    .await;
            }

            while let Ok(progress) = rx.try_recv() {
                let _ = this.update(cx, |win, cx| {
                    win.state.apply_scan_progress(&progress);
                    cx.notify();
                });
            }

            match handle.join() {
                Ok(result) => {
                    let _ = this.update(cx, |win, cx| {
                        win.state.apply_scan_result(result);
                        cx.notify();
                    });
                }
                Err(_) => {
                    let _ = this.update(cx, |win, cx| {
                        win.state.scanning = false;
                        win.state.status_text = "Scan thread panicked.".to_string();
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_input.is_focused(window) {
            let action = self.search_input.handle_key_ime(event, Some(cx));
            if matches!(action, TextInputAction::Cancel) {
                window.remove_window();
            }
            cx.notify();
            return;
        }
        if event.keystroke.key.as_str() == "escape" {
            window.remove_window();
        }
    }
}

// Route platform IME (CJK/Thai composition + candidate-window positioning) to
// the search field. Coexists with `handle_key_with_clipboard` (handle_key);
// GPUI suppresses key dispatch for keystrokes the IME consumes.
crate::impl_single_input_window_ime!(PluginManagerWindow, search_input);

impl Render for PluginManagerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.initial_cache_loaded {
            self.initial_cache_loaded = true;
            // Load cached `.pst` index only. Never auto-scan VST3/CLAP binaries
            // — the user must press Scan Now / Full Rescan explicitly.
            Self::arm_cache_load(cx);
        }

        let target = cx.entity().clone();
        let search_focused = self.search_input.is_focused(window);

        let callbacks = PluginManagerCallbacks {
            on_close: Arc::new(|_: &(), window: &mut Window, _cx: &mut App| {
                window.remove_window();
            }),
            on_rescan: Arc::new({
                let target = target.clone();
                move |_: &(), _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        if this.state.scanning {
                            return;
                        }
                        this.state.begin_scan(PluginScanMode::Rescan);
                        cx.notify();
                        PluginManagerWindow::arm_background_scan(cx, PluginScanMode::Rescan);
                    });
                }
            }),
            on_rescan_all: Arc::new({
                let target = target.clone();
                move |_: &(), _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        if this.state.scanning {
                            return;
                        }
                        this.state.begin_scan(PluginScanMode::RescanAll);
                        cx.notify();
                        PluginManagerWindow::arm_background_scan(cx, PluginScanMode::RescanAll);
                    });
                }
            }),
            on_rescan_au: Arc::new({
                let target = target.clone();
                move |_: &(), _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        if this.state.scanning || !this.state.au_scan_available {
                            return;
                        }
                        this.state.begin_scan(PluginScanMode::RescanAu);
                        cx.notify();
                        PluginManagerWindow::arm_background_scan(cx, PluginScanMode::RescanAu);
                    });
                }
            }),
            on_select_id: Arc::new({
                let target = target.clone();
                move |id: &String, _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        let toggle_off = this.state.selected_id.as_deref() == Some(id.as_str());
                        this.state.selected_id = if toggle_off { None } else { Some(id.clone()) };
                        cx.notify();
                    });
                }
            }),
            on_sidebar_filter: Arc::new({
                let target = target.clone();
                move |filter: &SidebarFilter, _w, cx| {
                    let filter = filter.clone();
                    let _ = target.update(cx, |this, cx| {
                        this.state.sidebar_filter = filter;
                        cx.notify();
                    });
                }
            }),
            on_sort: Arc::new({
                let target = target.clone();
                move |key: &SortKey, _w, cx| {
                    let key = *key;
                    let _ = target.update(cx, |this, cx| {
                        if this.state.sort_key == key {
                            this.state.sort_dir = match this.state.sort_dir {
                                SortDir::Asc => SortDir::Desc,
                                SortDir::Desc => SortDir::Asc,
                            };
                        } else {
                            this.state.sort_key = key;
                            this.state.sort_dir = SortDir::Asc;
                        }
                        cx.notify();
                    });
                }
            }),
            on_insert: Arc::new({
                let target = target.clone();
                move |_id: &String, _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.state.status_text = "Insert on track: not connected yet.".to_string();
                        cx.notify();
                    });
                }
            }),
            on_open_editor: Arc::new({
                let target = target.clone();
                move |_id: &String, _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.state.status_text = "Plug-in editor: not connected yet.".to_string();
                        cx.notify();
                    });
                }
            }),
            on_reveal_preset: Arc::new({
                let target = target.clone();
                move |id: &String, _w, cx| {
                    let _ = target.update(cx, |this, _cx| {
                        if let Some(plugin) = this.state.plugins.iter().find(|p| p.id == *id) {
                            reveal_preset_in_os(reveal_path_for_plugin(plugin));
                        }
                    });
                }
            }),
            on_open_db_folder: Arc::new({
                move |_: &(), _w, _cx| {
                    let dir = SpherePluginHost::database_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    reveal_in_os(&dir);
                }
            }),
            on_clear_cache: Arc::new({
                let target = target.clone();
                move |_: &(), _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        if this.state.scanning {
                            return;
                        }
                        match PluginRegistry::clear_cache() {
                            Ok(removed) => {
                                this.state.plugins.clear();
                                this.state.selected_id = None;
                                this.state.failed_count = 0;
                                this.state.last_scan_at_ms = 0;
                                this.state.cache_loaded = true;
                                this.state.status_text = format!(
                                    "Cleared {removed} cached preset(s). Click Scan Now to rebuild."
                                );
                            }
                            Err(error) => {
                                this.state.status_text = format!("Clear cache failed: {error}");
                            }
                        }
                        cx.notify();
                    });
                }
            }),
            on_register_plugin: Arc::new({
                let target = target.clone();
                move |id: &String, _w, cx| {
                    let _ = target.update(cx, |this, cx| {
                        let Some(plugin) = this.state.plugins.iter_mut().find(|p| p.id == *id)
                        else {
                            return;
                        };
                        let name = plugin.name.clone();
                        match register_plugin(plugin) {
                            Ok(()) => {
                                this.state.generated_presets =
                                    this.state
                                        .plugins
                                        .iter()
                                        .filter(|p| p.status == PluginStatus::PresetReady)
                                        .count() as u32;
                                this.state.status_text = format!("Registered preset for {name}.");
                            }
                            Err(error) => {
                                this.state.status_text = format!("Register failed: {error}");
                            }
                        }
                        cx.notify();
                    });
                }
            }),
        };

        let sw_target = target.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .relative()
            .font(theme::ui_font())
            .bg(Colors::surface_window())
            .overflow_hidden()
            .capture_key_down({
                let target = sw_target.clone();
                move |event, window, cx| {
                    let _ = target.update(cx, |this, cx| this.handle_key(event, window, cx));
                }
            })
            .child(div().w(px(0.0)).h(px(0.0)).track_focus(&self.focus_handle))
            .child(external_window_titlebar(
                "Audio Plug-in Manager",
                "plugin-manager-window-close",
                {
                    let target = sw_target.clone();
                    move |window, cx| {
                        let _ = target.update(cx, |_, cx| cx.notify());
                        window.remove_window();
                    }
                },
            ))
            .child(plugin_manager_panel(
                &self.state,
                &self.search_input,
                search_focused,
                bind_mouse_selection(cx.entity().clone(), |this| &mut this.search_input),
                target.clone(),
                callbacks,
                &self.sidebar_scroll,
                &self.list_scroll,
            ))
    }
}

pub fn open_plugin_manager_window(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    cx: &mut App,
) -> Result<WindowHandle<PluginManagerWindow>, String> {
    let window_bounds = crate::window_position::centered_window_bounds(
        owner_bounds,
        size(
            px(PLUGIN_MANAGER_WINDOW_WIDTH),
            px(PLUGIN_MANAGER_WINDOW_HEIGHT),
        ),
        cx,
    );

    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    options.kind = WindowKind::Dialog;
    options.is_resizable = true;
    options.is_minimizable = false;
    options.window_background = WindowBackgroundAppearance::Transparent;
    options.window_min_size = Some(size(
        px(PLUGIN_MANAGER_WINDOW_MIN_WIDTH),
        px(PLUGIN_MANAGER_WINDOW_MIN_HEIGHT),
    ));
    crate::window_position::apply_owner_display(&mut options, owner_bounds, cx);

    cx.open_window(options, |_window, cx| cx.new(PluginManagerWindow::new))
        .map_err(|error| error.to_string())
}

/// Scan the default plug-in locations in a compact, standalone progress dialog.
/// Closing the dialog only hides progress; the registry scan continues safely
/// on its worker thread and persists its result for the next plug-in picker.
pub fn open_plugin_scan_progress_dialog(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    cx: &mut App,
) -> Result<(), String> {
    open_plugin_scan_progress_dialog_impl(owner_bounds, None, cx)
}

/// First-launch scan surface. It stays in the startup window handoff and opens
/// the caller's next surface exactly once when the scan finishes or the user
/// closes the progress window. The scan itself remains safe to finish in the
/// background after a manual close.
pub fn open_startup_plugin_scan_progress_dialog(
    on_complete: Arc<dyn Fn(&mut App) + Send + Sync>,
    cx: &mut App,
) -> Result<(), String> {
    open_plugin_scan_progress_dialog_impl(None, Some(on_complete), cx)
}

fn open_plugin_scan_progress_dialog_impl(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    on_startup_complete: Option<Arc<dyn Fn(&mut App) + Send + Sync>>,
    cx: &mut App,
) -> Result<(), String> {
    let startup_flow = on_startup_complete.is_some();
    let options = ProgressDialogOptions::default()
        .title("Plug-in Scan")
        .heading("Scanning installed plug-ins")
        .detail(plugin_scan_discovery_text())
        .progress(ProgressBarValue::Indeterminate)
        .footer(if startup_flow {
            "Welcome will open when scanning finishes."
        } else {
            "You can hide this window; scanning will continue."
        })
        .hide_percent();
    let options = if startup_flow {
        options
    } else {
        options.cancel_label("Hide")
    };

    let startup_finished = Arc::new(AtomicBool::new(false));
    let guarded_complete = on_startup_complete.map(|on_complete| {
        let finished = startup_finished.clone();
        Arc::new(move |cx: &mut App| {
            if !finished.swap(true, Ordering::AcqRel) {
                on_complete(cx);
            }
        }) as Arc<dyn Fn(&mut App) + Send + Sync>
    });
    let on_cancel: Option<ProgressDialogCancelCb> = guarded_complete.as_ref().map(|complete| {
        let complete = complete.clone();
        Arc::new(move |_window: &mut Window, cx: &mut App| complete(cx)) as ProgressDialogCancelCb
    });
    let dialog = if startup_flow {
        open_standalone_progress_dialog_window(options, on_cancel, cx)?
    } else {
        open_progress_dialog_window(owner_bounds, options, on_cancel, cx)?
    };

    cx.spawn(async move |cx| {
        let (tx, rx) = std::sync::mpsc::channel::<ScanProgress>();
        let handle = std::thread::spawn(move || {
            PluginRegistry::scan_with_progress(ScanOptions::default(), |progress| {
                let _ = tx.send(progress);
            })
        });

        loop {
            while let Ok(progress) = rx.try_recv() {
                update_scan_progress_dialog(&dialog, progress, startup_flow, cx);
            }
            if handle.is_finished() {
                break;
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(32))
                .await;
        }

        while let Ok(progress) = rx.try_recv() {
            update_scan_progress_dialog(&dialog, progress, startup_flow, cx);
        }

        let completed = match handle.join() {
            Ok(result) => {
                let plugin_count = result.plugins.len();
                let failed_count = result.failed.len();
                let detail = if let Some(au_error) = result.au_scan_error.as_deref() {
                    format!(
                        "{plugin_count} plug-in(s) are ready. AudioUnit scan issue: {au_error}"
                    )
                } else if failed_count == 0 {
                    format!("{plugin_count} plug-in(s) are ready to use.")
                } else {
                    format!(
                        "{plugin_count} plug-in(s) are ready; {failed_count} item(s) could not be scanned."
                    )
                };
                let footer = if result.generated_presets > 0 {
                    format!("Registered {} preset(s).", result.generated_presets)
                } else {
                    "The plug-in index is up to date.".to_string()
                };
                ProgressDialogOptions::default()
                    .title("Plug-in Scan")
                    .heading("Scan complete")
                    .detail(detail)
                    .progress(ProgressBarValue::value(1.0))
                    .footer(footer)
                    .cancel_label("Close")
            }
            Err(_) => ProgressDialogOptions::default()
                .title("Plug-in Scan")
                .heading("Scan failed")
                .detail("The plug-in scanner stopped unexpectedly. You can try again from Plug-in Manager.")
                .progress(ProgressBarValue::Indeterminate)
                .footer("No running scan remains.")
                .cancel_label("Close")
                .hide_percent(),
        };
        if let Some(complete) = guarded_complete {
            // Open Welcome before retiring the progress surface so GPUI never
            // observes a zero-window gap under LastWindowClosed behavior.
            let _ = cx.update(|app| complete(app));
            let _ = dialog.update(cx, |_view, window, _cx| window.remove_window());
        } else {
            let _ = dialog.update(cx, |view, _window, cx| {
                view.set_options(completed, cx);
            });
        }
    })
    .detach();

    Ok(())
}

fn update_scan_progress_dialog(
    dialog: &WindowHandle<crate::components::progress_dialog::ProgressDialogWindow>,
    progress: ScanProgress,
    startup_flow: bool,
    cx: &mut gpui::AsyncApp,
) {
    let mut options = match progress {
        ScanProgress::Started { bundle_total } => ProgressDialogOptions::default()
            .title("Plug-in Scan")
            .heading("Scanning installed plug-ins")
            .detail(format!(
                "Found {bundle_total} plug-in bundle(s) to inspect."
            ))
            .progress(if bundle_total == 0 {
                ProgressBarValue::Indeterminate
            } else {
                ProgressBarValue::value(0.0)
            })
            .footer("You can hide this window; scanning will continue.")
            .cancel_label("Hide"),
        ScanProgress::ScanningBundle {
            current,
            total,
            path,
        } => ProgressDialogOptions::default()
            .title("Plug-in Scan")
            .heading("Reading plug-in metadata")
            .detail(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Plug-in bundle"),
            )
            .progress(scan_fraction(current, total))
            .footer(format!("{} of {} bundle(s)", current.min(total), total))
            .cancel_label("Hide"),
        ScanProgress::Registering {
            current,
            total,
            name,
            generated_presets,
            ..
        } => ProgressDialogOptions::default()
            .title("Plug-in Scan")
            .heading("Registering plug-ins")
            .detail(name)
            .progress(scan_fraction(current, total))
            .footer(format!(
                "{} of {} plug-in(s) · {} preset(s)",
                current.min(total),
                total,
                generated_presets
            ))
            .cancel_label("Hide"),
        ScanProgress::Failed { path, error } => ProgressDialogOptions::default()
            .title("Plug-in Scan")
            .heading("Scanning installed plug-ins")
            .detail(format!(
                "Skipped {}: {error}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("plug-in")
            ))
            .progress(ProgressBarValue::Indeterminate)
            .footer("The scan is continuing with the remaining plug-ins.")
            .cancel_label("Hide")
            .hide_percent(),
        ScanProgress::FormatFinished {
            format,
            success_count,
            failed_count,
            crashed_count,
            error,
        } => {
            let format_name = if format == PluginFormat::Au {
                "AudioUnit"
            } else {
                format.label()
            };
            let detail = error.unwrap_or_else(|| format!("{format_name} scan finished."));
            ProgressDialogOptions::default()
                .title("Plug-in Scan")
                .heading(format!("{format_name} scan complete"))
                .detail(detail)
                .progress(ProgressBarValue::Indeterminate)
                .footer(format!(
                    "{success_count} ready · {failed_count} failed · {crashed_count} crashed"
                ))
                .cancel_label("Hide")
                .hide_percent()
        }
    };

    if startup_flow {
        options.footer = Some("Welcome will open when scanning finishes.".to_string());
        options.cancel_label = None;
    }

    let _ = dialog.update(cx, |view, _window, cx| {
        view.set_options(options, cx);
    });
}

fn scan_fraction(current: usize, total: usize) -> ProgressBarValue {
    if total == 0 {
        ProgressBarValue::Indeterminate
    } else {
        ProgressBarValue::value(current as f32 / total as f32)
    }
}

fn plugin_scan_discovery_text() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Discovering VST3, CLAP, and AudioUnit plug-ins…"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Discovering VST3 and CLAP plug-ins…"
    }
}
