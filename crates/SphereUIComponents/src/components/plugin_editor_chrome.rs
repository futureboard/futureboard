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

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, size, svg, App, AppContext, Bounds, Context, ElementId, InteractiveElement,
    IntoElement, ParentElement, Pixels, Point, Render, StatefulInteractiveElement, Styled,
    Subscription, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};

use crate::assets;
use crate::theme::{self, Colors};

/// One plug-in open in a channel's editor window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEditorTab {
    /// Insert slot id — how the studio and the host both address it.
    pub insert_id: String,
    /// Plug-in name, as the tab shows it.
    pub display_name: String,
    /// 1-based slot position on the channel.
    pub insert_number: usize,
}

/// What the chrome shows, as of the studio's last refresh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginEditorChrome {
    /// Plug-in display name, as the titlebar shows it.
    pub plugin_name: String,
    /// Channel this window belongs to, for the title.
    pub track_name: String,
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

/// Renders the open preset list, to be placed under the chrome row.
///
/// Drawn by the window rather than inside the row so it can overlap the
/// plug-in's region: the row is 26 pixels tall and a preset list is not.
pub fn render_preset_menu(
    chrome: &PluginEditorChrome,
    emit: impl Fn(PluginEditorAction, &mut App) + Clone + 'static,
) -> gpui::AnyElement {
    let mut list = div()
        .flex()
        .flex_col()
        .size_full()
        .py(px(PRESET_MENU_PAD))
        .rounded(px(4.0))
        .bg(Colors::surface_panel_raised())
        .border_1()
        .border_color(Colors::border_subtle())
        .overflow_hidden()
        .occlude();

    if chrome.presets.is_empty() {
        return list
            .child(
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(px(10.0))
                    .font(theme::ui_font())
                    .text_color(Colors::text_faint())
                    .child("No presets saved for this plug-in"),
            )
            .into_any_element();
    }

    for (index, name) in chrome.presets.iter().enumerate() {
        let selected = chrome.preset_index == Some(index);
        let pick = emit.clone();
        list = list.child(
            div()
                .id(ElementId::Name(format!("plugin-preset-{index}").into()))
                .flex()
                .items_center()
                .h(px(PRESET_MENU_ROW_H))
                .px(px(10.0))
                .text_size(px(10.0))
                .font(theme::ui_font())
                .text_color(if selected {
                    Colors::text_primary()
                } else {
                    Colors::text_secondary()
                })
                .when(selected, |style| style.bg(Colors::surface_control_hover()))
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|style| style.bg(Colors::surface_control_hover()))
                .on_click(move |_, _window, cx| pick(PluginEditorAction::SelectPreset(index), cx))
                .child(name.clone()),
        );
    }
    list.into_any_element()
}

/// Width of the preset list, and the row and padding it is built from. The
/// window has to be sized before it is opened, so the list's geometry cannot
/// live only inside its own layout.
const PRESET_MENU_W: f32 = 200.0;
const PRESET_MENU_ROW_H: f32 = 22.0;
const PRESET_MENU_PAD: f32 = 4.0;
const PRESET_MENU_MAX_H: f32 = 320.0;

/// Size the preset list window needs for `count` presets.
pub fn preset_menu_size(count: usize) -> gpui::Size<Pixels> {
    // An empty list still shows one row saying so, which is the whole reason it
    // opens at all when nothing is saved yet.
    let rows = count.max(1) as f32;
    let height = (PRESET_MENU_PAD * 2.0 + rows * PRESET_MENU_ROW_H).min(PRESET_MENU_MAX_H);
    size(px(PRESET_MENU_W), px(height))
}

/// The preset list, in a window of its own.
///
/// It cannot be drawn inside the editor window, and that is not a layering bug
/// to be fixed with z-order. Below the header the client area *is* a native
/// child window the plug-in draws into, and a native child composites above
/// everything its host paints — which is why the chrome row is described up
/// there as "the one strip that is never covered by its view". A list dropped
/// from that row lands squarely in the covered region, so it was being built
/// and drawn every frame and never once seen.
///
/// A borderless `PopUp` is what a menu over a native child has to be. It also
/// dismisses the way a menu should: losing activation closes it, so clicking
/// anywhere else — including into the plug-in's own view — puts it away.
pub struct PresetMenuWindow {
    chrome: PluginEditorChrome,
    on_action: Rc<dyn Fn(PluginEditorAction, &mut App)>,
    /// Whether this window has ever held activation.
    ///
    /// Dismiss-on-blur must not fire before the list has been shown. The window
    /// it opens over hosts a plug-in's native child, and a plug-in that takes
    /// keyboard focus back on its own would otherwise deactivate the list on the
    /// frame it appeared — closing it before anyone saw it, which is the exact
    /// symptom this window exists to fix.
    seen_active: bool,
    /// Dropped with the window; while it lives, losing focus closes the list.
    _activation: Option<Subscription>,
}

impl PresetMenuWindow {
    fn new(
        chrome: PluginEditorChrome,
        on_action: Rc<dyn Fn(PluginEditorAction, &mut App)>,
    ) -> Self {
        Self {
            chrome,
            on_action,
            seen_active: false,
            _activation: None,
        }
    }
}

impl Render for PresetMenuWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let emit = self.on_action.clone();
        div()
            .size_full()
            .child(render_preset_menu(&self.chrome, move |action, cx| {
                emit(action, cx)
            }))
    }
}

/// Opens the preset list at `origin`, in screen coordinates.
///
/// `on_action` is handed every choice the list makes, including the
/// [`PluginEditorAction::TogglePresetMenu`] it sends when it dismisses itself,
/// so the editor window has one place to learn the list is gone.
pub fn open_preset_menu(
    origin: Point<Pixels>,
    chrome: PluginEditorChrome,
    on_action: impl Fn(PluginEditorAction, &mut App) + 'static,
    cx: &mut App,
) -> Option<WindowHandle<PresetMenuWindow>> {
    let bounds = Bounds {
        origin,
        size: preset_menu_size(chrome.presets.len()),
    };
    let options = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: None,
        ..Default::default()
    };
    let on_action: Rc<dyn Fn(PluginEditorAction, &mut App)> = Rc::new(on_action);
    cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let mut menu = PresetMenuWindow::new(chrome, on_action);
            // A menu that outlives the click that dismissed it is a menu the
            // user has to hunt down. The first activation callback fires for
            // this window becoming active, so only a *loss* closes it.
            menu._activation = Some(cx.observe_window_activation(
                window,
                |menu: &mut PresetMenuWindow, window: &mut Window, cx: &mut Context<_>| {
                    if window.is_window_active() {
                        menu.seen_active = true;
                        return;
                    }
                    // Never shown yet: a plug-in that grabbed focus back is not
                    // the user dismissing anything. Staying open is the right
                    // failure here — the list is at worst sticky, rather than
                    // gone before it was seen.
                    if !menu.seen_active {
                        return;
                    }
                    let on_action = menu.on_action.clone();
                    on_action(PluginEditorAction::TogglePresetMenu(false), cx);
                    window.remove_window();
                },
            ));
            menu
        })
    })
    .ok()
}

/// Height of the tab strip. Sized to a browser tab, which is what it is.
pub const TAB_STRIP_H: f32 = 30.0;

/// Renders the tab strip: one tab per plug-in open on this channel.
///
/// Browser-shaped on purpose — a channel's inserts are a chain the user moves
/// along, and a row of named tabs with their own close buttons is the gesture
/// everyone already has for that.
pub fn render_tab_strip(
    tabs: &[PluginEditorTab],
    active: &str,
    emit: impl Fn(PluginEditorAction, &mut App) + Clone + 'static,
) -> gpui::AnyElement {
    let mut strip = div()
        .flex()
        .flex_row()
        .items_end()
        .h(px(TAB_STRIP_H))
        .px(px(4.0))
        .gap(px(2.0))
        .bg(Colors::surface_panel_alt())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .overflow_hidden();

    for tab in tabs {
        let selected = tab.insert_id == active;
        let select = emit.clone();
        let close = emit.clone();
        let select_id = tab.insert_id.clone();
        let close_id = tab.insert_id.clone();
        strip = strip.child(
            div()
                .id(ElementId::Name(
                    format!("plugin-tab-{}", tab.insert_id).into(),
                ))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(TAB_STRIP_H - 4.0))
                .pl(px(10.0))
                .pr(px(4.0))
                .max_w(px(220.0))
                .rounded_t(px(5.0))
                .bg(if selected {
                    Colors::surface_panel()
                } else {
                    Colors::surface_panel_alt()
                })
                .when(selected, |style| {
                    style
                        .border_t(px(1.0))
                        .border_l(px(1.0))
                        .border_r(px(1.0))
                        .border_color(Colors::border_subtle())
                })
                .cursor(gpui::CursorStyle::PointingHand)
                .occlude()
                .hover(|style| style.bg(Colors::surface_control_hover()))
                .on_click(move |_, _window, cx| {
                    select(PluginEditorAction::SelectTab(select_id.clone()), cx)
                })
                // The slot number leads: on a channel with two of the same
                // plug-in, the name alone does not say which one this is.
                .child(
                    div()
                        .text_size(px(10.0))
                        .font(theme::ui_font())
                        .text_color(Colors::text_faint())
                        .child(tab.insert_number.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_size(px(11.0))
                        .font(theme::ui_font())
                        .text_color(if selected {
                            Colors::text_primary()
                        } else {
                            Colors::text_secondary()
                        })
                        .child(tab.display_name.clone()),
                )
                .child(
                    div()
                        .id(ElementId::Name(
                            format!("plugin-tab-close-{}", tab.insert_id).into(),
                        ))
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(16.0))
                        .h(px(16.0))
                        .rounded(px(3.0))
                        .hover(|style| style.bg(Colors::surface_control_hover()))
                        .occlude()
                        .on_click(move |_, _window, cx| {
                            close(PluginEditorAction::CloseTab(close_id.clone()), cx)
                        })
                        .child(
                            svg()
                                .path(assets::ICON_CLOSE_SMALL_PATH)
                                .w(px(10.0))
                                .h(px(10.0))
                                .text_color(Colors::text_faint()),
                        ),
                ),
        );
    }

    strip.into_any_element()
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
    /// Load the preset at this index.
    SelectPreset(usize),
    /// Open or close the preset list.
    TogglePresetMenu(bool),
    /// Bring another of this channel's open plug-ins to the front.
    SelectTab(String),
    /// Close one plug-in's tab. Closing the last one closes the window.
    CloseTab(String),
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

/// An icon-only button, for controls whose glyph says it better than a word.
fn chrome_icon_button(
    id: impl Into<ElementId>,
    icon: &'static str,
    enabled: bool,
    active: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let tint = if !enabled {
        Colors::text_faint()
    } else if active {
        Colors::text_primary()
    } else {
        Colors::text_secondary()
    };
    let glyph = svg().path(icon).w(px(13.0)).h(px(13.0)).text_color(tint);
    let base = div()
        .id(id.into())
        .flex()
        .items_center()
        .justify_center()
        .w(px(22.0))
        .h(px(18.0))
        .rounded(px(3.0))
        .child(glyph);
    if !enabled {
        return base.into_any_element();
    }
    base.cursor(gpui::CursorStyle::PointingHand)
        .occlude()
        .when(active, |style| style.bg(Colors::accent_primary()))
        .when(!active, |style| {
            style.hover(|style| style.bg(Colors::surface_control_hover()))
        })
        .on_click(move |_, window, cx| on_click(window, cx))
        .into_any_element()
}

/// An icon with a value beside it, for the readouts.
fn chrome_readout(icon: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .child(
            svg()
                .path(icon)
                .w(px(11.0))
                .h(px(11.0))
                .text_color(Colors::text_faint()),
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
    menu_open: bool,
    emit: impl Fn(PluginEditorAction, &mut App) + Clone + 'static,
) -> gpui::AnyElement {
    let active = chrome.active;
    let has_presets = !chrome.presets.is_empty();

    let emit_active = emit.clone();
    let emit_prev = emit.clone();
    let emit_next = emit.clone();
    let emit_menu = emit.clone();
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
        .child(chrome_icon_button(
            "plugin-editor-active",
            assets::ICON_POWER_PATH,
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
                .child(chrome_icon_button(
                    "plugin-editor-preset-prev",
                    assets::ICON_CHEVRON_LEFT_PATH,
                    has_presets,
                    false,
                    move |_window, cx| emit_prev(PluginEditorAction::StepPreset(-1), cx),
                ))
                // The name is the menu trigger. A chain of presets is stepped
                // through with the chevrons; picking one by name needs a list.
                .child(
                    div()
                        .id("plugin-editor-preset-menu")
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .h(px(18.0))
                        .px(px(8.0))
                        .min_w(px(120.0))
                        .rounded(px(3.0))
                        .bg(Colors::surface_base())
                        .text_size(px(10.0))
                        .font(theme::ui_font())
                        .text_color(Colors::text_secondary())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .occlude()
                        .hover(|style| style.bg(Colors::surface_control_hover()))
                        .on_click(move |_, _window, cx| {
                            emit_menu(PluginEditorAction::TogglePresetMenu(!menu_open), cx)
                        })
                        .child(div().flex_1().child(chrome.preset_label()))
                        .child(
                            svg()
                                .path(assets::ICON_CHEVRON_DOWN_PATH)
                                .w(px(10.0))
                                .h(px(10.0))
                                .text_color(Colors::text_faint()),
                        ),
                )
                .child(chrome_icon_button(
                    "plugin-editor-preset-next",
                    assets::ICON_CHEVRON_RIGHT_PATH,
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
        .child(chrome_readout(assets::ICON_CPU_PATH, chrome.cpu_label()))
        .child(chrome_readout(
            assets::ICON_TIMER_PATH,
            chrome.latency_label(),
        ))
        .into_any_element()
}
