//! The plug-in editor window's own chrome: what the window contributes above
//! the plug-in's surface.
//!
//! A plug-in editor window is mostly not ours — below the titlebar the whole
//! client area is a native child window the plug-in draws into, and nothing the
//! host paints can appear there. The titlebar strip is therefore the only place
//! the *host's* controls for that plug-in can live, so this is where the insert
//! it belongs to, whether it is active, its presets, and what it costs are
//! shown.
//!
//! # Ownership
//!
//! The window renders this and nothing more. Every value is pushed in by
//! [`crate::layout::StudioLayout`], which owns the insert, the engine and the
//! preset files; every control queues an action the studio drains and applies.
//! That keeps the window a view over real state instead of a second place where
//! a plug-in's active flag or preset list is decided.

use gpui::{
    div, px, App, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window,
};

use crate::theme::{self, Colors};

/// What the chrome shows, as of the studio's last refresh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginEditorChrome {
    /// Plug-in display name, as the titlebar shows it.
    pub plugin_name: String,
    /// 1-based insert slot this plug-in occupies on its track.
    pub insert_number: usize,
    /// Whether the insert is processing. `false` covers both a disabled and a
    /// bypassed slot — from the editor's side they are the same statement.
    pub active: bool,
    /// Latency the plug-in reports, and the rate it is measured against.
    pub latency_samples: u32,
    pub sample_rate: u32,
    /// Share of one audio block this insert's processing took, 0..=1.
    ///
    /// `None` while nothing has been measured — no stream open, or the insert
    /// has not been processed yet. It is never guessed: an unmeasured plug-in
    /// shows a dash, not a zero.
    pub cpu_load: Option<f32>,
    /// User presets available for this plug-in, in menu order.
    pub presets: Vec<String>,
    /// Which of `presets` is loaded, when the studio knows.
    pub preset_index: Option<usize>,
}

impl PluginEditorChrome {
    /// The window title: `{PluginName} - Insert {n}`.
    pub fn window_title(&self) -> String {
        let name = if self.plugin_name.is_empty() {
            "Plugin"
        } else {
            self.plugin_name.as_str()
        };
        if self.insert_number == 0 {
            return name.to_string();
        }
        format!("{name} - Insert {}", self.insert_number)
    }

    /// Current preset name, or a placeholder when none is loaded.
    fn preset_label(&self) -> String {
        self.preset_index
            .and_then(|index| self.presets.get(index))
            .cloned()
            .unwrap_or_else(|| {
                if self.presets.is_empty() {
                    "No presets".to_string()
                } else {
                    "Unsaved".to_string()
                }
            })
    }

    /// `3.2 ms` / `0 smp` — whichever states the latency most plainly.
    fn latency_label(&self) -> String {
        if self.latency_samples == 0 {
            return "0 ms".to_string();
        }
        if self.sample_rate == 0 {
            return format!("{} smp", self.latency_samples);
        }
        let ms = self.latency_samples as f64 * 1000.0 / self.sample_rate as f64;
        format!("{ms:.1} ms")
    }

    fn cpu_label(&self) -> String {
        match self.cpu_load {
            // Rounded to whole percent: a per-block share jitters, and a
            // one-decimal readout that never settles reads as noise.
            Some(load) => format!("{:.0}%", (load * 100.0).clamp(0.0, 999.0)),
            None => "—".to_string(),
        }
    }
}

/// A control the chrome asks the studio to carry out.
///
/// Queued rather than applied: the window has no access to the insert, the
/// engine, or the preset store, and routing every change through the studio
/// keeps one owner for each of them.
#[derive(Clone, Debug, PartialEq)]
pub enum PluginEditorAction {
    /// Turn the insert's processing on or off.
    SetActive(bool),
    /// Move `delta` places through the preset list, wrapping.
    StepPreset(i32),
    /// Store the plug-in's current state as a new preset.
    SavePreset,
}

/// Small square/pill button used across the chrome.
fn chrome_button(
    id: impl Into<ElementId>,
    label: impl Into<String>,
    enabled: bool,
    active: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let mut button = div()
        .id(id.into())
        .flex()
        .items_center()
        .justify_center()
        .h(px(18.0))
        .px(px(7.0))
        .rounded(px(3.0))
        .text_size(px(10.0))
        .font(theme::ui_font())
        .child(label.into());
    if !enabled {
        return button
            .text_color(Colors::text_faint())
            .bg(Colors::surface_base())
            .into_any_element();
    }
    button = button
        .cursor(gpui::CursorStyle::PointingHand)
        .occlude()
        .on_click(move |_, window, cx| on_click(window, cx));
    if active {
        button
            .text_color(Colors::text_primary())
            .bg(Colors::accent_primary())
            .into_any_element()
    } else {
        button
            .text_color(Colors::text_secondary())
            .bg(Colors::surface_base())
            .hover(|style| style.bg(Colors::surface_control_hover()))
            .into_any_element()
    }
}

/// A label/value pair, for the readouts.
fn chrome_readout(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(9.0))
                .font(theme::ui_font())
                .text_color(Colors::text_faint())
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(10.0))
                .font(theme::ui_font())
                .text_color(Colors::text_secondary())
                .child(value),
        )
}

/// Height of the chrome row. Mirrors `plugin_editor_window::CHROME_H`.
const CHROME_ROW_H: f32 = 26.0;

/// Builds the chrome row that sits under the titlebar, above the plug-in.
///
/// `emit` queues an action on the window.
pub fn render_chrome_tools(
    chrome: &PluginEditorChrome,
    emit: impl Fn(PluginEditorAction, &mut App) + Clone + 'static,
) -> gpui::AnyElement {
    let active = chrome.active;
    let has_presets = !chrome.presets.is_empty();

    let emit_active = emit.clone();
    let emit_prev = emit.clone();
    let emit_next = emit.clone();
    let emit_save = emit;

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .h(px(CHROME_ROW_H))
        .px(px(8.0))
        .bg(Colors::surface_panel())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        // Active: the plug-in's own on/off, in the one strip that is never
        // covered by its view.
        .child(chrome_button(
            "plugin-editor-active",
            if active { "ACTIVE" } else { "BYPASSED" },
            true,
            active,
            move |_window, cx| emit_active(PluginEditorAction::SetActive(!active), cx),
        ))
        // Presets: step and save. Nothing here invents a preset — the list is
        // whatever the studio found on disk for this plug-in.
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(chrome_button(
                    "plugin-editor-preset-prev",
                    "‹",
                    has_presets,
                    false,
                    move |_window, cx| emit_prev(PluginEditorAction::StepPreset(-1), cx),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(18.0))
                        .px(px(8.0))
                        .min_w(px(96.0))
                        .rounded(px(3.0))
                        .bg(Colors::surface_base())
                        .text_size(px(10.0))
                        .font(theme::ui_font())
                        .text_color(Colors::text_secondary())
                        .child(chrome.preset_label()),
                )
                .child(chrome_button(
                    "plugin-editor-preset-next",
                    "›",
                    has_presets,
                    false,
                    move |_window, cx| emit_next(PluginEditorAction::StepPreset(1), cx),
                ))
                .child(chrome_button(
                    "plugin-editor-preset-save",
                    "Save",
                    true,
                    false,
                    move |_window, cx| emit_save(PluginEditorAction::SavePreset, cx),
                )),
        )
        // Readouts sit at the far end: they are watched, not operated, so they
        // stay clear of the controls the pointer goes for.
        .child(div().flex_1())
        .child(chrome_readout("CPU", chrome.cpu_label()))
        .child(chrome_readout("LAT", chrome.latency_label()))
        .into_any_element()
}
