use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use gpui::{font, Font, FontFallbacks, Rgba};
use serde::Deserialize;
use serde_json::Value;

/// Native UI font selected independently on each supported desktop platform.
#[cfg(target_os = "macos")]
pub const SYSTEM_UI_FONT_FAMILY: &str = ".AppleSystemUIFont";
#[cfg(target_os = "windows")]
pub const SYSTEM_UI_FONT_FAMILY: &str = "Segoe UI";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const SYSTEM_UI_FONT_FAMILY: &str = "Noto Sans";

/// Compatibility alias used by existing UI call sites. It intentionally points
/// to the platform font rather than a bundled typeface.
pub const FONT_FAMILY: &str = SYSTEM_UI_FONT_FAMILY;

/// Real, selectable system family behind [`SYSTEM_UI_FONT_FAMILY`]. macOS keeps
/// the SF UI font under a hidden dot-name that only resolves as a fallback
/// descriptor, so the stack also names a family that can be selected directly.
#[cfg(target_os = "macos")]
const SYSTEM_UI_FONT_FAMILY_ALIAS: Option<&str> = Some("Helvetica Neue");
#[cfg(not(target_os = "macos"))]
const SYSTEM_UI_FONT_FAMILY_ALIAS: Option<&str> = None;

/// Primary Thai system family. Keeping Thai base glyphs and marks in one family
/// avoids broken composition while still respecting native platform typography.
#[cfg(target_os = "macos")]
pub const SYSTEM_THAI_UI_FONT_FAMILY: &str = "Thonburi";
#[cfg(target_os = "windows")]
pub const SYSTEM_THAI_UI_FONT_FAMILY: &str = "Leelawadee UI";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const SYSTEM_THAI_UI_FONT_FAMILY: &str = "Noto Sans Thai";

#[cfg(target_os = "macos")]
const PLATFORM_THAI_UI_FONT_FAMILIES: &[&str] = &[SYSTEM_THAI_UI_FONT_FAMILY, "Sukhumvit Set"];
#[cfg(target_os = "windows")]
const PLATFORM_THAI_UI_FONT_FAMILIES: &[&str] = &[SYSTEM_THAI_UI_FONT_FAMILY, "Tahoma"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const PLATFORM_THAI_UI_FONT_FAMILIES: &[&str] = &[SYSTEM_THAI_UI_FONT_FAMILY, "Noto Serif Thai"];

#[cfg(target_os = "macos")]
const PLATFORM_UI_FALLBACK_FAMILIES: &[&str] = &["Helvetica Neue", "Arial"];
#[cfg(target_os = "windows")]
const PLATFORM_UI_FALLBACK_FAMILIES: &[&str] = &["Segoe UI Variable", "Arial"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const PLATFORM_UI_FALLBACK_FAMILIES: &[&str] = &["Ubuntu", "DejaVu Sans", "Liberation Sans"];

/// Alias kept for callsites that want an explicit display font name.
pub const DISPLAY_FONT_FAMILY: &str = FONT_FAMILY;

/// Readability adjustment for the native application. This scales text only;
/// controls and window geometry retain their compact DAW layout.
pub const UI_TEXT_SCALE: f32 = 1.08;

/// Bundled cross-surface theme template. Installed to AppData on first run and
/// used as the fallback when no user theme exists or a user theme omits tokens.
pub const DEFAULT_THEME_JSON: &str = include_str!("../../../packages/shared/themes/Default.json");
pub const TEMPLATE_THEME_JSON: &str = include_str!("../../../packages/shared/themes/Template.json");

/// Build a fallback list, dropping the primary family and any repeat. A duplicate
/// costs a redundant descriptor in the platform cascade list and never adds
/// coverage.
fn fallback_list(primary: &str, candidates: impl IntoIterator<Item = &'static str>) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for candidate in candidates {
        if candidate == primary || list.iter().any(|kept| kept == candidate) {
            continue;
        }
        list.push(candidate.to_string());
    }
    list
}

/// Central UI font fallback stack made exclusively from platform fonts.
///
/// A Thai-capable family is named ahead of the generic tiers so Thai base glyphs
/// and marks are shaped by one font instead of being split per glyph.
pub fn ui_font_fallback_stack() -> Vec<String> {
    let mut candidates = vec![FONT_FAMILY];
    candidates.extend(PLATFORM_THAI_UI_FONT_FAMILIES);
    candidates.extend(SYSTEM_UI_FONT_FAMILY_ALIAS);
    candidates.extend(PLATFORM_UI_FALLBACK_FAMILIES);
    fallback_list("", candidates)
}

pub fn ui_font() -> Font {
    let mut font = font(FONT_FAMILY);
    font.fallbacks = Some(FontFallbacks::from_fonts(ui_font_fallback_stack()));
    font
}

pub fn ui_font_for_language(language_code: &str) -> Font {
    let normalized = language_code.trim().replace('_', "-").to_ascii_lowercase();
    if normalized == "th" || normalized.starts_with("th-") {
        let family = SYSTEM_THAI_UI_FONT_FAMILY;
        let mut candidates = PLATFORM_THAI_UI_FONT_FAMILIES.to_vec();
        candidates.push(FONT_FAMILY);
        candidates.extend(SYSTEM_UI_FONT_FAMILY_ALIAS);
        candidates.extend(PLATFORM_UI_FALLBACK_FAMILIES);

        let mut font = font(family);
        font.fallbacks = Some(FontFallbacks::from_fonts(fallback_list(family, candidates)));
        return font;
    }
    ui_font()
}

/// Compact DAW typography tokens (logical px — GPUI/DWrite scale for DPI).
pub mod typography {
    /// Small metadata labels (dB scale, channel index).
    pub const UI_XS: f32 = 11.0;
    /// Default UI body / toolbar / track header label.
    pub const UI_SM: f32 = 12.0;
    /// Section headers, dialog titles, emphasized labels.
    pub const UI_MD: f32 = 13.0;
    /// Semibold section / panel titles.
    pub const UI_TITLE: f32 = 13.0;
    /// Native plugin editor wrapper titlebar (Pro-C 3, etc.).
    pub const PLUGIN_TITLE: f32 = 12.0;
    /// Default line-height ratio for single-line chrome text.
    pub const LINE_HEIGHT: f32 = 1.3;
}

/// Recommended text sizes. Kept here so individual components don't drift.
pub mod text {
    use super::typography::*;

    /// Caps-style sublabels — INSERTS / SENDS / TRACK.
    pub const CAPS: f32 = UI_XS;
    /// Small meta (CH 01, dB scale).
    pub const META: f32 = UI_XS;
    /// Standard UI label (track name, button label).
    pub const UI: f32 = UI_SM;
    /// Inspector / title text.
    pub const TITLE: f32 = UI_MD;
}

pub mod menu {
    pub const PANEL_MIN_WIDTH: f32 = 210.0;
    pub const PANEL_MAX_WIDTH: f32 = 340.0;
    pub const PANEL_PAD: f32 = 3.0;
    pub const ROW_HEIGHT: f32 = 20.0;
    pub const ROW_PAD_X: f32 = 8.0;
    pub const CHECK_SLOT_W: f32 = 18.0;
    pub const ICON_SIZE: f32 = 11.0;
    pub const CHEVRON_SIZE: f32 = 11.0;
    pub const LABEL_TEXT_SIZE: f32 = crate::theme::typography::UI_XS;
    pub const META_TEXT_SIZE: f32 = crate::theme::typography::UI_XS;
    pub const HEADER_TEXT_SIZE: f32 = crate::theme::typography::UI_XS;
    pub const HEADER_HEIGHT: f32 = 21.0;
    pub const SEPARATOR_MARGIN_Y: f32 = 2.0;
    pub const ITEM_GAP: f32 = 1.0;
}

#[derive(Debug, Clone)]
pub struct LoadedTheme {
    pub id: String,
    pub name: String,
    pub path: Option<PathBuf>,
    colors: HashMap<String, Rgba>,
    track_colors: Vec<Rgba>,
}

#[derive(Debug, Clone)]
pub struct ThemeLoadReport {
    pub active_id: String,
    pub active_name: String,
    pub active_path: Option<PathBuf>,
    pub discovered: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThemeManifest {
    id: Option<String>,
    name: Option<String>,
    tokens: Option<Value>,
    track_colors: Option<Vec<String>>,
}

static ACTIVE_THEME: OnceLock<RwLock<LoadedTheme>> = OnceLock::new();
static LAST_THEME_REPORT: OnceLock<RwLock<Option<ThemeLoadReport>>> = OnceLock::new();

fn active_theme_store() -> &'static RwLock<LoadedTheme> {
    ACTIVE_THEME.get_or_init(|| RwLock::new(load_default_theme()))
}

fn report_store() -> &'static RwLock<Option<ThemeLoadReport>> {
    LAST_THEME_REPORT.get_or_init(|| RwLock::new(None))
}

pub fn active_theme_summary() -> (String, String, Option<PathBuf>) {
    let theme = active_theme_store()
        .read()
        .unwrap_or_else(|e| e.into_inner());
    (theme.id.clone(), theme.name.clone(), theme.path.clone())
}

/// Themes available to the UI, including the bundled fallback. This is a
/// control-path query used by Preferences and the command palette, never by a
/// render hot path.
pub fn available_theme_summaries() -> Vec<(String, String)> {
    let paths = crate::paths::FutureboardPaths::resolve();
    let _ = fs::create_dir_all(&paths.themes);
    install_builtin_theme_templates(&paths.themes);

    let default = load_default_theme();
    let mut themes = vec![(default.id.clone(), default.name.clone())];
    let mut discovered = discover_theme_files(&paths.themes);
    discovered.sort();
    for path in discovered {
        if let Ok(theme) = load_theme_file(&path, &default) {
            if theme.id != "publisher.theme-id" && !themes.iter().any(|(id, _)| id == &theme.id) {
                themes.push((theme.id, theme.name));
            }
        }
    }
    themes
}

/// Activate one installed theme by its stable id. The active store is shared
/// by all token lookups, so the next GPUI render picks up the new colors.
pub fn activate_theme_by_id(id: &str) -> bool {
    let paths = crate::paths::FutureboardPaths::resolve();
    let _ = fs::create_dir_all(&paths.themes);
    install_builtin_theme_templates(&paths.themes);
    let default = load_default_theme();
    // Pre-selector builds persisted display labels rather than ids. Keep those
    // settings files valid by resolving the old dark presets to the bundled
    // fallback instead of leaving the process on an unrelated auto-selected
    // theme.
    let id = match id {
        "Fleet Dark" | "Ableton Dark" => default.id.as_str(),
        other => other,
    };
    let selected = if id == default.id {
        Some(default.clone())
    } else {
        let mut files = discover_theme_files(&paths.themes);
        files.sort();
        files.into_iter().find_map(|path| {
            load_theme_file(&path, &default)
                .ok()
                .filter(|theme| theme.id == id)
        })
    };
    let Some(theme) = selected else {
        return false;
    };
    *active_theme_store()
        .write()
        .unwrap_or_else(|e| e.into_inner()) = theme;
    true
}

pub fn last_theme_load_report() -> Option<ThemeLoadReport> {
    report_store()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Initialize the Native GPUI theme system from:
/// `%APPDATA%/Futureboard Studio/Extensions/Themes/**/theme.json` on Windows
/// and the equivalent config path on other platforms.
///
/// Selection order:
/// 1. `FUTUREBOARD_THEME_ID=<theme id>` when set.
/// 2. First discovered non-default, non-template user theme, sorted by path.
/// 3. Bundled `futureboard.default`.
pub fn initialize_theme_system() -> ThemeLoadReport {
    let paths = crate::paths::FutureboardPaths::resolve();
    let _ = fs::create_dir_all(&paths.themes);
    install_builtin_theme_templates(&paths.themes);

    let default = load_default_theme();
    let mut discovered = discover_theme_files(&paths.themes);
    discovered.sort();

    let requested_id = std::env::var("FUTUREBOARD_THEME_ID").ok();
    let mut loaded = Vec::new();
    let mut errors = Vec::new();

    for path in &discovered {
        match load_theme_file(path, &default) {
            Ok(theme) => loaded.push(theme),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }

    let chosen = requested_id
        .as_deref()
        .and_then(|id| loaded.iter().find(|theme| theme.id == id).cloned())
        .or_else(|| {
            loaded
                .iter()
                .find(|theme| theme.id != default.id && theme.id != "publisher.theme-id")
                .cloned()
        })
        .unwrap_or(default);

    let report = ThemeLoadReport {
        active_id: chosen.id.clone(),
        active_name: chosen.name.clone(),
        active_path: chosen.path.clone(),
        discovered: loaded.len(),
        errors,
    };

    *active_theme_store()
        .write()
        .unwrap_or_else(|e| e.into_inner()) = chosen;
    *report_store().write().unwrap_or_else(|e| e.into_inner()) = Some(report.clone());

    eprintln!(
        "[theme] active={} name={} discovered={} path={}",
        report.active_id,
        report.active_name,
        report.discovered,
        report
            .active_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<bundled>".to_string())
    );
    for error in &report.errors {
        eprintln!("[theme] failed to load {error}");
    }

    report
}

fn install_builtin_theme_templates(themes_dir: &Path) {
    let default_path = themes_dir.join("Default").join("theme.json");
    if !default_path.exists() {
        if let Some(parent) = default_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&default_path, DEFAULT_THEME_JSON);
    }

    // Keep the authoring template outside the `**/theme.json` discovery pattern
    // so it is available for users to copy but never auto-activates.
    let template_path = themes_dir.join("Template.json");
    if !template_path.exists() {
        let _ = fs::write(&template_path, TEMPLATE_THEME_JSON);
    }
}

fn discover_theme_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.eq_ignore_ascii_case("theme.json"))
                .unwrap_or(false)
            {
                files.push(path);
            }
        }
    }

    files
}

fn load_default_theme() -> LoadedTheme {
    load_theme_from_str(DEFAULT_THEME_JSON, None, None).unwrap_or_else(|error| {
        eprintln!("[theme] bundled Default.json is invalid: {error}");
        LoadedTheme {
            id: "futureboard.default".to_string(),
            name: "Futureboard Default".to_string(),
            path: None,
            colors: HashMap::new(),
            track_colors: DEFAULT_TRACK_COLOR_VALUES
                .iter()
                .map(|c| rgba_from_u32(*c))
                .collect(),
        }
    })
}

fn load_theme_file(path: &Path, base: &LoadedTheme) -> Result<LoadedTheme, String> {
    let json = fs::read_to_string(path).map_err(|e| e.to_string())?;
    load_theme_from_str(&json, Some(path.to_path_buf()), Some(base))
}

fn load_theme_from_str(
    json: &str,
    path: Option<PathBuf>,
    base: Option<&LoadedTheme>,
) -> Result<LoadedTheme, String> {
    let manifest: ThemeManifest = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut colors = base.map(|theme| theme.colors.clone()).unwrap_or_default();

    if let Some(tokens) = manifest.tokens.as_ref() {
        flatten_theme_tokens(tokens, "", &mut colors)?;
    }

    let track_colors = manifest
        .track_colors
        .as_ref()
        .and_then(|colors| {
            let parsed: Vec<Rgba> = colors
                .iter()
                .filter_map(|color| parse_theme_color(color).ok())
                .collect();
            if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            }
        })
        .or_else(|| base.map(|theme| theme.track_colors.clone()))
        .unwrap_or_else(|| {
            DEFAULT_TRACK_COLOR_VALUES
                .iter()
                .map(|c| rgba_from_u32(*c))
                .collect()
        });

    Ok(LoadedTheme {
        id: manifest
            .id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| "futureboard.unnamed".to_string()),
        name: manifest
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Unnamed Theme".to_string()),
        path,
        colors,
        track_colors,
    })
}

fn flatten_theme_tokens(
    value: &Value,
    prefix: &str,
    out: &mut HashMap<String, Rgba>,
) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let next = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_theme_tokens(value, &next, out)?;
            }
        }
        Value::String(color) => {
            let parsed = parse_theme_color(color)
                .map_err(|e| format!("invalid color token {prefix}={color:?}: {e}"))?;
            out.insert(prefix.to_string(), parsed);
        }
        _ => {}
    }
    Ok(())
}

fn parse_theme_color(input: &str) -> Result<Rgba, crate::color::ColorParseError> {
    crate::color::parse_hex_color(input)
}

fn rgba_from_u32(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 16) & 0xFF) as f32 / 255.0,
        g: ((value >> 8) & 0xFF) as f32 / 255.0,
        b: (value & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}

macro_rules! theme_color {
    ($name:ident, $key:literal, $fallback:literal) => {
        pub fn $name() -> Rgba {
            Self::resolve($key, $fallback)
        }
    };
}

pub struct Colors;

const DEFAULT_TRACK_COLOR_VALUES: [u32; 12] = [
    0x56C7C9, 0x7EDB9A, 0xF2C96D, 0xF27E77, 0x8FB7FF, 0x6EB7E8, 0xE89B61, 0xD982B6, 0xA8D36F,
    0x9CAFE8, 0xC49A6C, 0x71D6B5,
];

impl Colors {
    fn resolve(key: &str, fallback: &str) -> Rgba {
        let theme = active_theme_store()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        theme
            .colors
            .get(key)
            .copied()
            .unwrap_or_else(|| parse_theme_color(fallback).unwrap_or_else(|_| Rgba::default()))
    }

    // Backgrounds
    theme_color!(surface_base, "surface.base", "#1E1F22");
    theme_color!(surface_panel, "surface.panel", "#25262B");
    theme_color!(surface_panel_alt, "surface.panelAlt", "#1B1C20");
    theme_color!(surface_panel_raised, "surface.panelRaised", "#2B2D33");
    theme_color!(surface_canvas, "surface.canvas", "#15161A");
    theme_color!(surface_raised, "surface.raised", "#2B2D33");
    theme_color!(surface_input, "surface.input", "#181A1F");
    theme_color!(surface_window, "surface.window", "#15161A");
    theme_color!(surface_titlebar, "surface.titlebar", "#1B1C20");
    theme_color!(surface_sidebar, "surface.sidebar", "#1B1C20");
    theme_color!(surface_card, "surface.card", "#202126");
    theme_color!(surface_card_hover, "surface.cardHover", "#30323A");
    theme_color!(surface_card_selected, "surface.cardSelected", "#272536");
    theme_color!(surface_code, "surface.code", "#181A1F");
    theme_color!(surface_badge, "surface.badge", "#202126");
    theme_color!(surface_hover, "surface.hover", "#30323A");
    theme_color!(surface_active, "surface.active", "#2B2D33");
    theme_color!(surface_control_hover, "surface.controlHover", "#292B31");
    theme_color!(surface_overlay, "surface.overlay", "#00000085");

    // Borders
    theme_color!(border_subtle, "border.subtle", "#292D36");
    theme_color!(border_normal, "border.normal", "#343946");
    theme_color!(border_default, "border.default", "#FFFFFF1F");
    theme_color!(border_strong, "border.strong", "#4C505C");
    theme_color!(border_focus, "border.focus", "#4C8DFFC0");
    theme_color!(border_accent, "border.accent", "#4C8DFF80");
    theme_color!(divider, "border.divider", "#FFFFFF0F");

    // Text
    theme_color!(text_primary, "text.primary", "#DFE1E5");
    theme_color!(text_secondary, "text.secondary", "#C3C7D0");
    theme_color!(text_muted, "text.muted", "#8E96A3");
    theme_color!(text_faint, "text.faint", "#FFFFFF45");
    theme_color!(text_dim, "text.dim", "#FFFFFF66");
    theme_color!(text_disabled, "text.disabled", "#FFFFFF3B");
    theme_color!(text_inverse, "text.inverse", "#1E1F22");

    // Accent
    theme_color!(accent_primary, "accent.primary", "#4C8DFF");
    theme_color!(accent_primary_hover, "accent.primaryHover", "#6BA1FF");
    theme_color!(accent_hover, "accent.hover", "#6BA1FF");
    theme_color!(accent_active, "accent.active", "#4C8DFF28");
    theme_color!(accent_focus, "accent.focus", "#4C8DFFC0");
    theme_color!(accent_soft, "accent.soft", "#4C8DFF30");
    theme_color!(accent_muted, "accent.muted", "#4C8DFF20");
    theme_color!(accent_pressed, "accent.pressed", "#4C8DFF28");
    theme_color!(on_accent, "accent.onAccent", "#FFFFFF");

    // Status / Alert Accents
    theme_color!(status_error, "status.error", "#FF6B68");
    theme_color!(status_warning, "status.warning", "#E5C07B");
    theme_color!(semantic_warning, "semantic.warning", "#E5C07B");
    theme_color!(status_success, "status.success", "#6FCF97");
    theme_color!(accent_success, "accent.success", "#6FCF97");
    theme_color!(accent_warning, "accent.warning", "#E5C07B");
    theme_color!(accent_danger, "accent.danger", "#FF6B68");
    theme_color!(accent_purple, "accent.purple", "#AEB7C6");

    // Workspace tab / focused panel tokens.
    theme_color!(tab_text, "tab.text", "#C3C7D0");
    theme_color!(tab_text_muted, "tab.text_muted", "#8E96A3");
    theme_color!(tab_text_active, "tab.text_active", "#78A9FF");
    theme_color!(tab_indicator_active, "tab.indicator_active", "#78A9FF");
    theme_color!(tab_bg_active, "tab.backgroundActive", "#4C8DFF20");
    theme_color!(tab_bg_hover, "tab.backgroundHover", "#30323A");
    theme_color!(panel_border_focused, "panel.border_focused", "#4C8DFF80");
    theme_color!(panel_header_active, "panel.header_active", "#4C8DFF");

    // DAW-specific
    theme_color!(meter_bg, "meter.background", "#FFFFFF0D");
    theme_color!(meter_low, "meter.low", "#6FCF97");
    theme_color!(meter_mid, "meter.mid", "#E5C07B");
    theme_color!(meter_high, "meter.high", "#FF6B68");
    theme_color!(fader_rail, "fader.rail", "#FFFFFF0F");
    theme_color!(fader_thumb, "fader.thumb", "#DFE1E5");
    theme_color!(fader_tick, "fader.tick", "#FFFFFF1F");
    theme_color!(fader_scale_text, "fader.scaleText", "#8E96A3");
    theme_color!(knob_bg, "knob.background", "#181A1F");
    theme_color!(knob_ring, "knob.ring", "#78A9FF");
    theme_color!(slot_bg, "slot.background", "#20232A");
    theme_color!(slot_border, "slot.border", "#FFFFFF1F");
    theme_color!(statusbar_bg, "statusbar.background", "#1B1C20");
    theme_color!(statusbar_text, "statusbar.text", "#8E96A3");
    theme_color!(mixer_bg, "mixer.background", "#15161A");
    theme_color!(master_strip_bg, "mixer.masterStripBackground", "#1B1C20");
    theme_color!(timeline_grid_major, "timeline.gridMajor", "#303642");
    theme_color!(timeline_grid_minor, "timeline.gridMinor", "#242832");
    theme_color!(timeline_grid_bar, "timeline.gridBar", "#3C4351");
    theme_color!(timeline_playhead, "timeline.playhead", "#FF6B68");
    theme_color!(timeline_background, "timeline.background", "#1E1F22");
    theme_color!(
        timeline_content_background,
        "timeline.contentBackground",
        "#1E1F22"
    );
    theme_color!(
        timeline_region_background,
        "timeline.regionBackground",
        "#FFFFFF06"
    );
    theme_color!(
        timeline_region_background_alt,
        "timeline.regionBackgroundAlt",
        "#FFFFFF04"
    );
    theme_color!(
        timeline_lane_background,
        "timeline.laneBackground",
        "#FFFFFF07"
    );
    theme_color!(
        timeline_lane_alt_background,
        "timeline.laneAltBackground",
        "#00000029"
    );
    theme_color!(
        timeline_selected_lane_background,
        "timeline.selectedLaneBackground",
        "#FFFFFF12"
    );
    theme_color!(
        timeline_empty_body_background,
        "timeline.emptyBodyBackground",
        "#00000024"
    );
    theme_color!(
        timeline_ruler_background,
        "timeline.rulerBackground",
        "#25262B"
    );
    theme_color!(timeline_ruler_tick, "timeline.rulerTick", "#FFFFFF1F");
    theme_color!(timeline_ruler_text, "timeline.rulerText", "#C3C7D0");
    theme_color!(timeline_selection, "timeline.selection", "#4C8DFF30");

    // Track colors (fallbacks)
    theme_color!(track_audio, "track.audio", "#48D4D0");
    theme_color!(track_midi, "track.midi", "#E5C07B");
    theme_color!(track_instrument, "track.instrument", "#78D88F");
    theme_color!(track_bus, "track.bus", "#78A9FF");
    theme_color!(track_return, "track.return", "#6FCF97");
    theme_color!(track_master, "track.master", "#DFE1E5");
    // Subdued overlays for track row states — graphite-leaning so the selected
    // track reads as elevated without flooding the header with accent hue.
    theme_color!(track_selected_overlay, "track.selectedOverlay", "#222532");
    theme_color!(track_muted_overlay, "track.mutedOverlay", "#17191F");

    // Surface selection states (used by rows/lanes that shouldn't get the full
    // accent treatment — sublanes, list selections).
    theme_color!(surface_selected, "surface.selected", "#272536");
    theme_color!(surface_selected_soft, "surface.selectedSoft", "#232230");
    theme_color!(surface_pressed, "surface.pressed", "#2A2E39");
    theme_color!(surface_muted, "surface.muted", "#191B21");

    // Extra named accents kept distinct from the purple primary.
    theme_color!(accent_cyan, "accent.cyan", "#48D4D0");
    theme_color!(accent_green, "accent.green", "#78D88F");

    // Automation sublane tokens — quiet graphite lanes with a purple curve so the
    // envelope is the only saturated element in the section.
    theme_color!(automation_curve, "automation.curve", "#78A9FF");
    theme_color!(automation_curve_hover, "automation.curveHover", "#A4C4FF");
    // Left header/label tint (opaque — sits over the header column, not the grid).
    theme_color!(automation_lane_bg, "automation.laneBg", "#181A21");
    theme_color!(
        automation_lane_bg_selected,
        "automation.laneBgSelected",
        "#1B1926"
    );
    theme_color!(
        automation_lane_header_bg,
        "automation.laneHeaderBg",
        "#1A1C23"
    );
    // Right-side lane body. TRANSLUCENT overlays (8-digit RGBA) so the timeline
    // grid drawn behind the rows stays visible — never an opaque dark block.
    // Selected ≈ rgba(124,92,255,0.05) over the timeline canvas.
    theme_color!(automation_canvas_bg, "automation.canvasBg", "#0E0F1417");
    theme_color!(
        automation_canvas_bg_selected,
        "automation.canvasBgSelected",
        "#4C8DFF0D"
    );
    // Faint value/center guides drawn behind the curve.
    theme_color!(
        automation_value_region_bg,
        "automation.valueRegionBg",
        "#4C8DFF08"
    );
    theme_color!(automation_center_line, "automation.centerLine", "#4C8DFF2E");
    theme_color!(automation_center_band, "automation.centerBand", "#4C8DFF06");
    theme_color!(automation_separator, "automation.separator", "#272B35");
    theme_color!(
        automation_separator_strong,
        "automation.separatorStrong",
        "#323746"
    );
    theme_color!(automation_rail, "automation.rail", "#4D4380");
    theme_color!(automation_rail_active, "automation.railActive", "#8A6CFF");
    theme_color!(automation_point, "automation.point", "#B9A8FF");

    // Compact button surface tokens shared by chrome controls.
    theme_color!(button_bg, "button.bg", "#20232A");
    theme_color!(button_bg_hover, "button.bgHover", "#292D38");
    theme_color!(button_bg_pressed, "button.bgPressed", "#303543");
    theme_color!(button_bg_active, "button.bgActive", "#3A2E70");
    theme_color!(button_border, "button.border", "#333846");
    theme_color!(button_border_hover, "button.borderHover", "#454B5C");
    theme_color!(button_text, "button.text", "#DDE2EC");
    theme_color!(button_text_muted, "button.textMuted", "#9AA3B2");

    // Surfaces
    theme_color!(bottom_panel_bg, "surface.bottomPanel", "#25262B");
    theme_color!(
        bottom_panel_header_bg,
        "surface.bottomPanelHeader",
        "#1B1C20"
    );
    theme_color!(mixer_strip_bg, "surface.mixerStrip", "#25262B");
    theme_color!(mixer_strip_bg_alt, "surface.mixerStripAlt", "#1B1C20");
    theme_color!(
        mixer_strip_selected_bg,
        "surface.mixerStripSelected",
        "#272536"
    );
    theme_color!(
        master_strip_header_bg,
        "surface.masterStripHeader",
        "#181A1F"
    );

    // Borders
    theme_color!(panel_border, "border.panel", "#FFFFFF14");
    theme_color!(strip_border, "border.strip", "#FFFFFF1F");
    theme_color!(strip_border_subtle, "border.stripSubtle", "#292D36");
    theme_color!(master_strip_border, "border.masterStrip", "#FFFFFF1F");

    // Slots
    theme_color!(slot_bg_hover, "slot.backgroundHover", "#292D38");
    theme_color!(slot_empty_text, "slot.emptyText", "#8E96A3");

    // Fader
    theme_color!(fader_groove, "fader.groove", "#15161A");
    theme_color!(fader_thumb_border, "fader.thumbBorder", "#FFFFFF40");

    // Meters
    theme_color!(meter_rail, "meter.rail", "#FFFFFF0A");
    theme_color!(meter_peak, "meter.peak", "#FFD700");

    // Status
    theme_color!(statusbar_text_muted, "statusbar.textMuted", "#FFFFFF66");
    theme_color!(statusbar_accent, "statusbar.accent", "#78A9FF");
    theme_color!(statusbar_warning, "statusbar.warning", "#E5C07B");

    /// Track-tinted audio clip body overlay. The arrangement grid is deliberately
    /// painted behind clips, so these alphas are part of the timeline layering
    /// contract rather than arbitrary component-local styling.
    pub fn timeline_audio_clip_fill(track_color: Rgba, selected: bool) -> Rgba {
        Self::with_alpha(track_color, if selected { 0.32 } else { 0.16 })
    }

    /// Audio clip outline, kept substantially stronger than the translucent body.
    pub fn timeline_audio_clip_border(track_color: Rgba, selected: bool) -> Rgba {
        Self::with_alpha(track_color, if selected { 0.95 } else { 0.62 })
    }

    /// Waveforms remain close to full strength even though their clip body is
    /// translucent, preserving transient readability against the visible grid.
    pub fn timeline_audio_clip_waveform(track_color: Rgba) -> Rgba {
        Self::with_alpha(track_color, 0.92)
    }

    // Helper to dynamically adjust alpha channel
    pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
        Rgba {
            r: color.r,
            g: color.g,
            b: color.b,
            a: alpha,
        }
    }

    pub const TRACK_COLORS: [u32; 12] = DEFAULT_TRACK_COLOR_VALUES;

    pub fn track_color_for_index(index: usize) -> Rgba {
        let theme = active_theme_store()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        theme
            .track_colors
            .get(index % theme.track_colors.len())
            .copied()
            .unwrap_or_else(|| rgba_from_u32(Self::TRACK_COLORS[index % Self::TRACK_COLORS.len()]))
    }
}

#[cfg(test)]
mod font_stack_tests {
    use super::*;

    /// Families the running OS does not ship are resolved to that platform's own
    /// default, so a stack that names another platform's system font silently has
    /// no system tier.
    const FOREIGN_FAMILIES: &[&str] = &[
        "Segoe UI",
        "Leelawadee UI",
        ".AppleSystemUIFont",
        "Helvetica Neue",
        "Thonburi",
        "Sukhumvit Set",
    ];

    fn platform_families() -> Vec<&'static str> {
        let mut families = vec![SYSTEM_UI_FONT_FAMILY];
        families.extend(SYSTEM_UI_FONT_FAMILY_ALIAS);
        families.extend(PLATFORM_THAI_UI_FONT_FAMILIES);
        families
    }

    #[test]
    fn ui_stack_leads_with_the_platform_system_family() {
        let stack = ui_font_fallback_stack();
        assert_eq!(stack.first().map(String::as_str), Some(FONT_FAMILY));
        assert_eq!(FONT_FAMILY, SYSTEM_UI_FONT_FAMILY);
        assert!(!stack.iter().any(|family| family == "Google Sans"));
        assert!(!stack.iter().any(|family| family.contains("Inter")));
    }

    #[test]
    fn ui_stack_names_this_platform_and_no_other() {
        let stack = ui_font_fallback_stack();
        for family in platform_families() {
            assert!(
                stack.iter().any(|entry| entry == family),
                "{family} missing from {stack:?}"
            );
        }
        for family in FOREIGN_FAMILIES {
            if platform_families().contains(family) {
                continue;
            }
            assert!(
                !stack.iter().any(|entry| entry == family),
                "{family} belongs to another platform but appears in {stack:?}"
            );
        }
    }

    #[test]
    fn ui_stack_covers_thai_without_repeating_a_family() {
        let stack = ui_font_fallback_stack();
        let mut seen = stack.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), stack.len(), "duplicate family in {stack:?}");

        for family in PLATFORM_THAI_UI_FONT_FAMILIES {
            assert!(
                stack.iter().any(|entry| entry == family),
                "Thai family {family} missing from {stack:?}"
            );
        }
    }

    #[test]
    fn thai_ui_font_is_thai_capable_and_never_falls_back_to_itself() {
        let font = ui_font_for_language("th-TH");
        let primary = font.family.to_string();
        assert!(
            PLATFORM_THAI_UI_FONT_FAMILIES.contains(&primary.as_str()),
            "{primary} cannot shape Thai"
        );

        let fallbacks = font.fallbacks.expect("Thai font needs a fallback list");
        let list = fallbacks.fallback_list();
        assert!(!list.contains(&primary));
        assert!(list.iter().any(|entry| *entry == SYSTEM_UI_FONT_FAMILY));
    }

    #[test]
    fn language_tag_matching_ignores_case_and_separator() {
        let thai = ui_font_for_language("TH_th").family.to_string();
        assert_eq!(thai, ui_font_for_language("th").family.to_string());
        assert_eq!(
            ui_font_for_language("en-US").family.to_string(),
            ui_font().family.to_string()
        );
    }
}
