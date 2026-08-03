//! Static composite fallback chains for the native UI font.
//!
//! This is the **only** place UI font family names are written down. Each list
//! is a coverage-ordered cascade handed to the platform text system as a
//! `gpui::FontFallbacks`; the native shaper (DirectWrite / CoreText / rustybuzz)
//! then resolves each grapheme cluster against the chain. We never pick a font
//! by language and never split strings ourselves.
//!
//! Icon fonts (e.g. `Segoe Fluent Icons`) are deliberately **absent** from these
//! chains — icon glyphs live in Private Use Area ranges and must stay on the
//! explicit icon-font widgets, or they would hijack ordinary text.

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// Modern optical-size UI family (Windows 11). The bare "Segoe UI Variable"
/// family does not exist; Windows exposes Text/Display/Small optical variants.
#[cfg(target_os = "windows")]
pub const WINDOWS_VARIABLE_TEXT: &str = "Segoe UI Variable Text";

/// Large-optical-size variant used for the display role.
#[cfg(target_os = "windows")]
pub const WINDOWS_VARIABLE_DISPLAY: &str = "Segoe UI Variable Display";

/// Static UI family shipped on every supported Windows; the anchor when the
/// variable optical family is not installed (older Windows 10).
#[cfg(target_os = "windows")]
pub const WINDOWS_LEGACY_UI: &str = "Segoe UI";

/// Composite fallback chain appended after the anchor. `Segoe UI` leads so that,
/// when the anchor is the variable face, Latin text the variable face already
/// covers is never affected, and only true gaps cascade onward.
///
/// Deliberately excludes `Segoe Fluent Icons` (PUA icon glyphs stay explicit).
#[cfg(target_os = "windows")]
pub const WINDOWS_UI_FALLBACKS: &[&str] = &[
    "Segoe UI",              // static UI backstop for the variable anchor
    "Leelawadee UI",         // Thai
    "Yu Gothic UI",          // Japanese
    "Microsoft YaHei UI",    // Simplified Chinese
    "Microsoft JhengHei UI", // Traditional Chinese
    "Malgun Gothic",         // Korean
    "Segoe UI Emoji",        // color emoji — kept last
];

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

/// Deterministic cascade for the macOS system UI font. The anchor itself is the
/// live system font resolved by GPUI/CoreText from the `.SystemUIFont`
/// descriptor (never hardcoded to Helvetica). Apple Color Emoji is kept last so
/// GPUI's emoji color path stays reachable.
#[cfg(target_os = "macos")]
pub const MACOS_UI_FALLBACKS: &[&str] = &[
    "Thonburi",            // Thai
    "PingFang SC",         // Simplified Chinese
    "PingFang TC",         // Traditional Chinese
    "Hiragino Sans",       // Japanese
    "Apple SD Gothic Neo", // Korean
    "Apple Color Emoji",   // color emoji — kept last
];

// ---------------------------------------------------------------------------
// Linux / other — unchanged behavior (Fontconfig/font-kit owns the cascade)
// ---------------------------------------------------------------------------

/// Linux anchor. Preserved verbatim from the previous `theme` implementation so
/// Phase 1 does not change Linux behavior.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const LINUX_UI_ANCHOR: &str = "Noto Sans";

/// Linux fallback chain. Preserved verbatim (including the redundant leading
/// self-entry the previous stack produced) to guarantee zero behavior change on
/// Linux; Fontconfig resolves the rest.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const LINUX_UI_FALLBACKS: &[&str] = &[
    "Noto Sans",
    "Noto Sans Thai",
    "Noto Serif Thai",
    "Ubuntu",
    "DejaVu Sans",
    "Liberation Sans",
];
