//! Central color model + helpers for track / clip / lane color selection.
//!
//! This is the single source of truth for converting between `gpui::Rgba` and
//! stable hex strings, parsing user-entered hex, and the default DAW palette.
//! The project file format, the color picker popover, and every track-color
//! call site share these helpers so there is exactly one color implementation.
//!
//! Runtime / UI values stay as `gpui::Rgba`. Persisted values are stable hex
//! strings (`#RRGGBB`). "Auto" colors are not stored as a fixed color — they
//! resolve to a generated palette color at runtime via [`ProjectColor`].

use std::fmt;

use gpui::Rgba;

/// Default DAW track palette as stable hex strings.
///
/// Kept in lock-step with [`crate::theme::Colors::TRACK_COLORS`] so that
/// auto-color assignment and the picker's quick presets stay consistent and
/// existing projects keep loading with the same swatches.
/// Bundled default track palette, as authoring-friendly hex.
///
/// This mirrors `trackColors` in `packages/shared/themes/Default.json` and the
/// `DEFAULT_TRACK_COLOR_VALUES` fallback in [`crate::theme`]. It exists only so
/// the strings are readable here; `default_palette_matches_theme` below is the
/// guard that stops the two copies drifting. Runtime code should ask the theme
/// via [`auto_color_for_index`], which follows whatever theme is active.
pub const DEFAULT_TRACK_COLORS: &[&str] = &[
    "#4FC9D8", "#4FD39A", "#8FD165", "#E3C15E", "#EC9A5C", "#F0776F", "#E87BAF", "#A98BF5",
    "#7FA8FF", "#5FBEE8", "#C0A177", "#4FD2BC",
];

/// Maximum number of recent custom colors kept in user preferences.
pub const MAX_RECENT_COLORS: usize = 12;

/// Error returned when a hex string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorParseError {
    /// Input was empty (after trimming and dropping a leading `#`).
    Empty,
    /// Input contained a non-hex character.
    InvalidDigit(char),
    /// Input length was not 3, 6, or 8 hex digits.
    InvalidLength(usize),
}

impl fmt::Display for ColorParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "enter a hex color"),
            Self::InvalidDigit(c) => write!(f, "invalid character '{c}'"),
            Self::InvalidLength(n) => write!(f, "expected 3, 6, or 8 digits, got {n}"),
        }
    }
}

impl std::error::Error for ColorParseError {}

/// Clamp every channel into `0.0..=1.0`.
pub fn normalize_color(color: Rgba) -> Rgba {
    Rgba {
        r: color.r.clamp(0.0, 1.0),
        g: color.g.clamp(0.0, 1.0),
        b: color.b.clamp(0.0, 1.0),
        a: color.a.clamp(0.0, 1.0),
    }
}

fn channel_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Convert an opaque color to HSV. Returns `(hue, saturation, value)` each in
/// `0.0..=1.0` (hue wraps). Alpha is ignored; achromatic colors return hue `0`.
pub fn rgba_to_hsv(color: Rgba) -> (f32, f32, f32) {
    let r = color.r.clamp(0.0, 1.0);
    let g = color.g.clamp(0.0, 1.0);
    let b = color.b.clamp(0.0, 1.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let value = max;
    let saturation = if max <= 0.0 { 0.0 } else { delta / max };
    let mut h6 = if delta <= 0.0 {
        0.0
    } else if (max - r).abs() < f32::EPSILON {
        (g - b) / delta
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    if h6 < 0.0 {
        h6 += 6.0;
    }
    (h6 / 6.0, saturation, value)
}

/// Convert HSV (each `0.0..=1.0`, hue wraps) to an opaque [`Rgba`].
pub fn hsv_to_rgba(hue: f32, saturation: f32, value: f32) -> Rgba {
    let h = hue.rem_euclid(1.0) * 6.0;
    let s = saturation.clamp(0.0, 1.0);
    let v = value.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Rgba {
        r: r + m,
        g: g + m,
        b: b + m,
        a: 1.0,
    }
}

/// Format a color as a stable `#RRGGBB` hex string. Alpha is dropped — the
/// persisted project format stores 6-digit hex and resolves alpha to opaque.
pub fn rgba_to_hex(color: Rgba) -> String {
    let c = normalize_color(color);
    format!(
        "#{:02X}{:02X}{:02X}",
        channel_to_u8(c.r),
        channel_to_u8(c.g),
        channel_to_u8(c.b)
    )
}

/// Format a color as `#RRGGBB` when opaque, or `#RRGGBBAA` when it has alpha.
/// Used by the picker's hex field where the user may have entered alpha.
pub fn rgba_to_hex_with_alpha(color: Rgba) -> String {
    let c = normalize_color(color);
    if c.a >= 0.999 {
        rgba_to_hex(c)
    } else {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            channel_to_u8(c.r),
            channel_to_u8(c.g),
            channel_to_u8(c.b),
            channel_to_u8(c.a)
        )
    }
}

fn u8_to_channel(v: u8) -> f32 {
    v as f32 / 255.0
}

/// Parse a hex color string. Accepts, case-insensitively, with or without a
/// leading `#`:
/// * `RGB`      → expands each nibble (e.g. `#0AF` → `#00AAFF`)
/// * `RRGGBB`
/// * `RRGGBBAA`
///
/// Returns a normalized [`Rgba`]; alpha defaults to `1.0` for 3/6-digit input.
pub fn parse_hex_color(input: &str) -> Result<Rgba, ColorParseError> {
    let trimmed = input.trim();
    let body = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if body.is_empty() {
        return Err(ColorParseError::Empty);
    }
    if let Some(bad) = body.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(ColorParseError::InvalidDigit(bad));
    }

    let nibble = |c: char| c.to_digit(16).unwrap_or(0) as u8;
    let pair = |hi: char, lo: char| (nibble(hi) << 4) | nibble(lo);

    let chars: Vec<char> = body.chars().collect();
    let (r, g, b, a) = match chars.len() {
        3 => {
            let r = nibble(chars[0]);
            let g = nibble(chars[1]);
            let b = nibble(chars[2]);
            ((r << 4) | r, (g << 4) | g, (b << 4) | b, 255)
        }
        6 => (
            pair(chars[0], chars[1]),
            pair(chars[2], chars[3]),
            pair(chars[4], chars[5]),
            255,
        ),
        8 => (
            pair(chars[0], chars[1]),
            pair(chars[2], chars[3]),
            pair(chars[4], chars[5]),
            pair(chars[6], chars[7]),
        ),
        n => return Err(ColorParseError::InvalidLength(n)),
    };

    Ok(Rgba {
        r: u8_to_channel(r),
        g: u8_to_channel(g),
        b: u8_to_channel(b),
        a: u8_to_channel(a),
    })
}

/// Resolve the auto-color for a track position (matches the project palette).
pub fn auto_color_for_index(index: usize) -> Rgba {
    crate::theme::Colors::track_color_for_index(index)
}

/// Stable, serialization-friendly representation of a chosen color.
///
/// `Auto` is not a fixed color: it resolves to a generated palette color at
/// runtime. `Custom` stores a stable hex string. This is what project data
/// should persist; the runtime/UI value remains a `gpui::Rgba`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectColor {
    /// Auto color — resolves to a palette color from the track index.
    Auto,
    /// A specific custom color stored as `#RRGGBB`.
    Custom { hex: String },
}

impl ProjectColor {
    /// Build a custom color from any `Rgba`.
    pub fn custom(color: Rgba) -> Self {
        Self::Custom {
            hex: rgba_to_hex(color),
        }
    }

    /// Build from a possibly-missing persisted hex string. `None` / empty ⇒
    /// `Auto`; an unparseable value falls back to `Auto` (never panics).
    pub fn from_stored(hex: Option<&str>) -> Self {
        match hex.map(str::trim) {
            None | Some("") => Self::Auto,
            Some(h) => match parse_hex_color(h) {
                Ok(_) => Self::Custom { hex: h.to_string() },
                Err(e) => {
                    color_picker_debug(&format!("stored color {h:?} invalid ({e}); using Auto"));
                    Self::Auto
                }
            },
        }
    }

    pub fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    /// Resolve to a concrete runtime color. `auto_index` is the track position
    /// used to pick a palette color when this is [`ProjectColor::Auto`].
    pub fn resolve(&self, auto_index: usize) -> Rgba {
        match self {
            Self::Auto => auto_color_for_index(auto_index),
            Self::Custom { hex } => {
                parse_hex_color(hex).unwrap_or_else(|_| auto_color_for_index(auto_index))
            }
        }
    }

    /// The hex string to persist, or `None` for `Auto`.
    pub fn stored_hex(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Custom { hex } => Some(hex),
        }
    }
}

/// Insert `hex` at the front of `recents`, de-duplicating (case-insensitively)
/// and capping the list at [`MAX_RECENT_COLORS`]. No-op for empty input.
pub fn push_recent_color(recents: &mut Vec<String>, hex: &str) {
    let normalized = match parse_hex_color(hex) {
        Ok(color) => rgba_to_hex(color),
        Err(_) => return,
    };
    recents.retain(|existing| !existing.eq_ignore_ascii_case(&normalized));
    recents.insert(0, normalized);
    recents.truncate(MAX_RECENT_COLORS);
}

fn recent_colors_path() -> std::path::PathBuf {
    crate::paths::FutureboardPaths::resolve()
        .app_data
        .join("recent_colors.json")
}

/// Load recent custom colors from user preferences. Returns an empty list when
/// the file is missing or unreadable (never panics).
pub fn load_recent_colors() -> Vec<String> {
    let path = recent_colors_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<String>>(&content) {
        Ok(list) => list
            .into_iter()
            .filter_map(|h| parse_hex_color(&h).ok().map(rgba_to_hex))
            .take(MAX_RECENT_COLORS)
            .collect(),
        Err(e) => {
            color_picker_debug(&format!("failed to parse recent_colors.json: {e}"));
            Vec::new()
        }
    }
}

/// Persist recent custom colors to user preferences (best-effort, off-thread).
pub fn save_recent_colors(recents: &[String]) {
    let path = recent_colors_path();
    let list = recents.to_vec();
    std::thread::spawn(move || {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&list) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    color_picker_debug(&format!("failed to write recent_colors.json: {e}"));
                }
            }
            Err(e) => color_picker_debug(&format!("failed to serialize recent colors: {e}")),
        }
    });
}

/// True when `FUTUREBOARD_COLOR_PICKER_DEBUG=1` (or `true`).
pub fn color_picker_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("FUTUREBOARD_COLOR_PICKER_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

pub fn color_picker_debug(message: &str) {
    if color_picker_debug_enabled() {
        eprintln!("[color-picker] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u8, g: u8, b: u8) -> Rgba {
        Rgba {
            r: u8_to_channel(r),
            g: u8_to_channel(g),
            b: u8_to_channel(b),
            a: 1.0,
        }
    }

    #[test]
    fn roundtrip_hex() {
        let c = rgba(0xFF, 0x00, 0xAA);
        assert_eq!(rgba_to_hex(c), "#FF00AA");
        assert_eq!(parse_hex_color("#FF00AA").unwrap(), c);
    }

    #[test]
    fn parse_accepts_all_forms() {
        let expected = rgba(0x00, 0xAA, 0xFF);
        assert_eq!(parse_hex_color("#0AF").unwrap(), expected);
        assert_eq!(parse_hex_color("0AF").unwrap(), expected);
        assert_eq!(parse_hex_color("#00AAFF").unwrap(), expected);
        assert_eq!(parse_hex_color("00aaff").unwrap(), expected);
        assert_eq!(parse_hex_color("  #00AAFF  ").unwrap(), expected);
    }

    #[test]
    fn parse_alpha_form() {
        let c = parse_hex_color("#FF00AA80").unwrap();
        assert_eq!(channel_to_u8(c.a), 0x80);
        assert_eq!(rgba_to_hex_with_alpha(c), "#FF00AA80");
    }

    #[test]
    fn parse_rejects_garbage_without_panic() {
        assert_eq!(parse_hex_color(""), Err(ColorParseError::Empty));
        assert_eq!(parse_hex_color("#"), Err(ColorParseError::Empty));
        assert_eq!(
            parse_hex_color("#GGGGGG"),
            Err(ColorParseError::InvalidDigit('G'))
        );
        assert_eq!(
            parse_hex_color("#FFFF"),
            Err(ColorParseError::InvalidLength(4))
        );
    }

    #[test]
    fn normalize_clamps() {
        let c = normalize_color(Rgba {
            r: 2.0,
            g: -1.0,
            b: 0.5,
            a: 9.0,
        });
        assert_eq!(c.r, 1.0);
        assert_eq!(c.g, 0.0);
        assert_eq!(c.b, 0.5);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn project_color_auto_resolves_to_palette() {
        let pc = ProjectColor::from_stored(None);
        assert!(pc.is_auto());
        assert_eq!(pc.resolve(3), auto_color_for_index(3));
        assert_eq!(pc.stored_hex(), None);
    }

    #[test]
    fn project_color_custom_roundtrips() {
        let pc = ProjectColor::from_stored(Some("#FF00AA"));
        assert_eq!(
            pc,
            ProjectColor::Custom {
                hex: "#FF00AA".into()
            }
        );
        assert_eq!(pc.resolve(0), rgba(0xFF, 0x00, 0xAA));
        assert_eq!(pc.stored_hex(), Some("#FF00AA"));
    }

    #[test]
    fn project_color_invalid_falls_back_to_auto() {
        let pc = ProjectColor::from_stored(Some("not-a-color"));
        assert!(pc.is_auto());
    }

    #[test]
    fn recent_dedupes_and_caps() {
        let mut recents = Vec::new();
        push_recent_color(&mut recents, "#FF00AA");
        push_recent_color(&mut recents, "#00FF00");
        push_recent_color(&mut recents, "#ff00aa"); // dup (case-insensitive)
        assert_eq!(recents, vec!["#FF00AA".to_string(), "#00FF00".to_string()]);

        for i in 0..MAX_RECENT_COLORS + 5 {
            push_recent_color(&mut recents, &format!("#0000{:02X}", i));
        }
        assert_eq!(recents.len(), MAX_RECENT_COLORS);
    }

    #[test]
    fn push_recent_ignores_invalid() {
        let mut recents = Vec::new();
        push_recent_color(&mut recents, "bogus");
        assert!(recents.is_empty());
    }

    #[test]
    fn default_palette_matches_theme() {
        assert_eq!(
            DEFAULT_TRACK_COLORS.len(),
            crate::theme::Colors::TRACK_COLORS.len()
        );
        for (i, hex) in DEFAULT_TRACK_COLORS.iter().enumerate() {
            assert_eq!(
                parse_hex_color(hex).unwrap(),
                crate::theme::Colors::track_color_for_index(i),
                "palette mismatch at {i}"
            );
        }
    }
}
