use std::sync::Arc;

use gpui::{
    div, px, svg, App, AppContext, DragMoveEvent, Empty, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
    WindowControlArea,
};

use crate::assets;
use crate::components::menu_bar;
use crate::components::text_input::{
    text_field_with_callbacks, TextInputCallbacks, TextInputState,
};
use crate::components::title_bar::{
    chrome_button, draggable_spacer, section_separator, CHROME_TITLE_SIZE, WINDOW_CONTROL_WIDTH,
};
use crate::platform_chrome::PlatformChromePolicy;
use crate::theme::Colors;

/// Click handler for top-level menu buttons. Receives `(menu_id, anchor_x)`
/// — anchor_x is the click X position which the dropdown overlay uses to
/// align itself under the clicked label.
pub type MenuOpenCb = menu_bar::MenuOpenCb;
pub type ChromeActionCb = Arc<dyn Fn(&(), &mut Window, &mut App) + 'static>;
pub type ProjectOpenCb = Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>;
pub type BpmChangeCb = Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>;
pub type BpmDragCb = Arc<dyn Fn(&BpmDragSample, &mut Window, &mut App) + 'static>;
/// Opens the compact tempo menu. Payload is the (x, y) screen position used to
/// anchor the popover beneath the BPM display.
pub type BpmMenuCb = Arc<dyn Fn(&(f32, f32), &mut Window, &mut App) + 'static>;

pub const BPM_MIN: f32 = 20.0;
pub const BPM_MAX: f32 = 999.0;

static BPM_DRAG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_bpm_drag_id() -> u64 {
    BPM_DRAG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One drag move sample. The handler on the owning entity accumulates
/// `cur_y - prev_y` deltas across samples — never the absolute distance
/// from the drag origin — so the cursor hitting the top/bottom of the
/// window doesn't cap the BPM range (FL Studio–style behavior).
#[derive(Clone, Copy, Debug)]
pub struct BpmDragSample {
    pub drag_id: u64,
    pub start_bpm: f32,
    pub cur_y: f32,
    pub shift: bool,
    pub control: bool,
    pub platform: bool,
    pub alt: bool,
}

/// Drag state for the transport BPM display. Carries the unique `drag_id`
/// so the receiver can tell a new drag from a continuation of the active
/// one, plus the captured `start_bpm`.
#[derive(Clone, Debug)]
pub struct BpmDrag {
    pub drag_id: u64,
    pub start_bpm: f32,
}

impl Render for BpmDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Clone)]
pub struct ProjectChromeState {
    pub name: String,
    pub is_dirty: bool,
    pub on_open_project_menu: ProjectOpenCb,
}

#[derive(Clone)]
pub struct PanelChromeState {
    pub browser_visible: bool,
    pub inspector_visible: bool,
    pub mixer_visible: bool,
    pub on_toggle_browser: ChromeActionCb,
    pub on_toggle_mixer: ChromeActionCb,
    pub on_toggle_inspector: ChromeActionCb,
}

#[derive(Clone)]
pub struct TransportChromeState {
    pub playing: bool,
    pub recording: bool,
    pub count_in_bars: u32,
    pub loop_enabled: bool,
    pub metronome_enabled: bool,
    pub follow_playhead: bool,
    /// True when auto-scroll is in continuous (smooth) mode rather than paged.
    /// Drives the FOLLOW button accent and is toggled via right-click.
    pub auto_scroll_continuous: bool,
    pub position_label: String,
    pub bpm: f32,
    pub bpm_label: String,
    /// True when tempo automation is active — drives the small "AUTO" badge.
    pub bpm_has_automation: bool,
    /// Inline BPM editor state (open flag, field contents, focus).
    pub bpm_editing: bool,
    pub bpm_input: TextInputState,
    pub bpm_input_callbacks: TextInputCallbacks,
    pub bpm_edit_focused: bool,
    pub time_signature_label: String,
    pub ts_has_markers: bool,
    pub ts_editing: bool,
    pub ts_num_input: TextInputState,
    pub ts_num_input_callbacks: TextInputCallbacks,
    pub ts_den_input: TextInputState,
    pub ts_den_input_callbacks: TextInputCallbacks,
    pub ts_edit_focus_num: bool,
    pub on_ts_menu: BpmMenuCb,
    pub on_ts_edit_start: ChromeActionCb,
    pub on_return_to_start: ChromeActionCb,
    pub on_play_toggle: ChromeActionCb,
    pub on_stop: ChromeActionCb,
    pub on_record: ChromeActionCb,
    pub on_count_in_cycle: ChromeActionCb,
    pub on_loop_toggle: ChromeActionCb,
    pub on_metronome_toggle: ChromeActionCb,
    pub on_follow_toggle: ChromeActionCb,
    /// Right-click on FOLLOW: switch auto-scroll between paged and continuous.
    pub on_follow_mode_toggle: ChromeActionCb,
    pub on_set_bpm: BpmChangeCb,
    pub on_bpm_drag: BpmDragCb,
    pub on_bpm_menu: BpmMenuCb,
    /// Opens the inline numeric BPM editor (double-click / "Edit BPM…").
    pub on_bpm_edit_start: ChromeActionCb,
    /// Taps in the current session (0 = idle). Used for brief tap-button feedback only.
    pub tap_tempo_session_taps: u8,
    /// Left-click registers a tap; right-click opens the tap tempo menu.
    pub on_tap_tempo: ChromeActionCb,
    pub on_tap_tempo_menu: BpmMenuCb,
}

fn tap_tempo_chip(
    session_taps: u8,
    on_tap: ChromeActionCb,
    on_menu: BpmMenuCb,
) -> gpui::AnyElement {
    let active = session_taps > 0;
    let bg = if active {
        Colors::with_alpha(Colors::accent_primary(), 0.2)
    } else {
        Colors::surface_input()
    };
    let text_color = if active {
        Colors::accent_primary()
    } else {
        Colors::text_secondary()
    };

    div()
        .id("transport-tap-tempo")
        .h(px(19.0))
        .min_w(px(26.0))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(2.0))
        .px(px(4.0))
        .rounded_md()
        .bg(bg)
        .text_color(text_color)
        .text_size(px(8.0))
        .font_weight(gpui::FontWeight::BOLD)
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_control_hover()))
        .child("TAP")
        .children((1..=session_taps.min(4)).map(|_| {
            div()
                .w(px(3.0))
                .h(px(3.0))
                .rounded_full()
                .bg(Colors::with_alpha(Colors::accent_primary(), 0.85))
        }))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_tap(&(), window, cx);
        })
        .on_mouse_down(
            MouseButton::Right,
            move |event: &gpui::MouseDownEvent, window, cx| {
                let pos = event.position;
                on_menu(&(pos.x.into(), pos.y.into()), window, cx);
            },
        )
        .into_any_element()
}

fn transport_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_TRANSPORT_DEBUG").is_some())
}

fn bpm_display(
    state_bpm: f32,
    label: String,
    on_bpm_drag: BpmDragCb,
    on_bpm_menu: BpmMenuCb,
    on_bpm_edit_start: ChromeActionCb,
    editing: bool,
    bpm_input: &TextInputState,
    bpm_input_callbacks: TextInputCallbacks,
    edit_focused: bool,
) -> gpui::AnyElement {
    // Inline numeric editor: replaces the draggable box while open. Keys are
    // routed to the input by the layout's key handler; Enter commits, Escape
    // cancels.
    if editing {
        return div()
            .w(px(48.0))
            .child(text_field_with_callbacks(
                bpm_input,
                edit_focused,
                bpm_input_callbacks,
            ))
            .into_any_element();
    }

    let on_bpm_drag_move = on_bpm_drag.clone();
    let on_bpm_menu_down = on_bpm_menu.clone();
    div()
        .id("transport-bpm")
        .w(px(36.0))
        .h(px(19.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(Colors::surface_input())
        .text_color(Colors::text_primary())
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor(gpui::CursorStyle::ResizeUpDown)
        .hover(|s| s.bg(Colors::surface_control_hover()))
        .child(label)
        .occlude()
        // Double-click opens inline numeric edit; left-drag scrubs the value;
        // right-click opens the tempo menu.
        .on_click(move |event: &gpui::ClickEvent, window, cx| {
            if event.click_count() >= 2 {
                on_bpm_edit_start(&(), window, cx);
            }
        })
        .on_mouse_down(
            gpui::MouseButton::Right,
            move |event: &gpui::MouseDownEvent, window, cx| {
                let pos = event.position;
                on_bpm_menu_down(&(pos.x.into(), pos.y.into()), window, cx);
            },
        )
        .on_drag(
            BpmDrag {
                drag_id: 0,
                start_bpm: state_bpm,
            },
            move |drag, _offset, _window, cx| {
                let id = next_bpm_drag_id();
                let started = BpmDrag {
                    drag_id: id,
                    start_bpm: drag.start_bpm,
                };
                cx.new(|_| started)
            },
        )
        .on_drag_move::<BpmDrag>(move |event: &DragMoveEvent<BpmDrag>, window, cx| {
            let drag = event.drag(cx);
            let mods = event.event.modifiers;
            let sample = BpmDragSample {
                drag_id: drag.drag_id,
                start_bpm: drag.start_bpm,
                cur_y: event.event.position.y.into(),
                shift: mods.shift,
                control: mods.control,
                platform: mods.platform,
                alt: mods.alt,
            };
            on_bpm_drag_move(&sample, window, cx);
        })
        .into_any_element()
}

/// Per-(logical)-pixel BPM sensitivity for a given modifier combination.
/// DAW-style feel: normal ≈ 1 BPM / 10 px, Shift = fine, Ctrl/Alt = coarse.
/// Because the BPM drag now warps the OS cursor (Windows), the per-pixel feel
/// is screen-height independent — the cursor never reaches the screen edge.
pub fn bpm_drag_sensitivity(shift: bool, coarse: bool) -> f32 {
    if shift {
        0.02
    } else if coarse {
        0.5
    } else {
        0.1
    }
}

/// Minimum per-event delta (in px) accepted by the BPM drag handler.
/// Below this, the event is treated as cursor jitter and ignored.
pub const BPM_DRAG_DEADZONE_PX: f32 = 0.5;

pub fn bpm_debug_enabled() -> bool {
    transport_debug_enabled()
}

fn menu_area(
    open_menu_id: Option<&str>,
    on_open_menu: MenuOpenCb,
    viewport_width: f32,
) -> impl IntoElement {
    menu_bar::menu_bar(open_menu_id, on_open_menu, viewport_width)
}

fn project_title(state: ProjectChromeState, anchor_x: f32) -> impl IntoElement {
    let on_open = state.on_open_project_menu.clone();
    let status = if state.is_dirty { "Unsaved" } else { "Saved" };
    let status_color = if state.is_dirty {
        Colors::status_warning()
    } else {
        Colors::status_success()
    };
    let title = crate::platform_chrome::branded_window_title(&state.name);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded_md()
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_control_hover()))
        .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
            on_open(&anchor_x, window, cx);
        })
        .occlude()
        .child(
            svg()
                .path(assets::ICON_FILE_PATH)
                .w(px(12.0))
                .h(px(12.0))
                .text_color(Colors::text_muted()),
        )
        .child(
            div()
                .min_w(px(0.0))
                .text_color(Colors::text_secondary())
                .text_size(px(CHROME_TITLE_SIZE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .truncate()
                .child(title),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(5.0))
                .py(px(2.0))
                .rounded_sm()
                .bg(Colors::surface_input())
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .child(div().w(px(4.0)).h(px(4.0)).rounded_full().bg(status_color))
                .child(
                    div()
                        .text_color(Colors::text_muted())
                        .text_size(px(8.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(status.to_uppercase()),
                ),
        )
}

// ── Right section — transport + panel toggles + utility ───────────────────────

fn transport_controls(state: TransportChromeState) -> impl IntoElement {
    let play_color = if state.playing {
        Colors::accent_primary()
    } else {
        Colors::text_muted()
    };
    let record_color = if state.recording {
        Colors::status_error()
    } else {
        Colors::text_faint()
    };
    let loop_color = if state.loop_enabled {
        Colors::accent_primary()
    } else {
        Colors::text_muted()
    };
    let metronome_color = if state.metronome_enabled {
        Colors::accent_primary()
    } else {
        Colors::text_muted()
    };
    // Continuous mode reads as a distinct accent so the right-click toggle is
    // visible at a glance; paged follow keeps the standard accent.
    let follow_color = if state.follow_playhead {
        if state.auto_scroll_continuous {
            Colors::status_success()
        } else {
            Colors::accent_primary()
        }
    } else {
        Colors::text_muted()
    };
    let on_return = state.on_return_to_start.clone();
    let on_play = state.on_play_toggle.clone();
    let on_stop = state.on_stop.clone();
    let on_record = state.on_record.clone();
    let on_count_in_cycle = state.on_count_in_cycle.clone();
    let on_loop = state.on_loop_toggle.clone();
    let on_metronome = state.on_metronome_toggle.clone();
    let on_follow = state.on_follow_toggle.clone();
    let on_follow_mode = state.on_follow_mode_toggle.clone();
    let on_bpm_drag = state.on_bpm_drag.clone();
    let on_bpm_menu = state.on_bpm_menu.clone();
    let on_bpm_edit_start = state.on_bpm_edit_start.clone();
    let bpm_value = state.bpm;
    let bpm_label = state.bpm_label.clone();
    let bpm_has_automation = state.bpm_has_automation;
    let bpm_editing = state.bpm_editing;
    let bpm_input = state.bpm_input.clone();
    let bpm_input_callbacks = state.bpm_input_callbacks.clone();
    let bpm_edit_focused = state.bpm_edit_focused;
    let tap_tempo_session_taps = state.tap_tempo_session_taps;
    let on_tap_tempo = state.on_tap_tempo.clone();
    let on_tap_tempo_menu = state.on_tap_tempo_menu.clone();
    let ts_has_markers = state.ts_has_markers;
    let on_ts_menu = state.on_ts_menu.clone();
    let on_ts_edit_start = state.on_ts_edit_start.clone();
    let ts_editing = state.ts_editing;
    let ts_num_input = state.ts_num_input.clone();
    let ts_num_input_callbacks = state.ts_num_input_callbacks.clone();
    let ts_den_input = state.ts_den_input.clone();
    let ts_den_input_callbacks = state.ts_den_input_callbacks.clone();
    let ts_edit_focus_num = state.ts_edit_focus_num;

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(1.0))
        // Skip back
        .child(
            chrome_button(
                Some(assets::ICON_SKIP_BACK_PATH),
                "<<",
                false,
                Colors::text_muted(),
            )
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                on_return(&(), window, cx);
            })
            .occlude(),
        )
        // Play
        .child(
            chrome_button(Some(assets::ICON_PLAY_PATH), ">", state.playing, play_color)
                .cursor(gpui::CursorStyle::PointingHand)
                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    on_play(&(), window, cx);
                })
                .occlude(),
        )
        // Stop
        .child(
            chrome_button(
                Some(assets::ICON_SQUARE_PATH),
                "[]",
                false,
                Colors::text_muted(),
            )
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                on_stop(&(), window, cx);
            })
            .occlude(),
        )
        // Record
        .child(
            chrome_button(
                Some(assets::ICON_CIRCLE_PATH),
                "REC",
                state.recording,
                record_color,
            )
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                on_record(&(), window, cx);
            })
            .occlude(),
        )
        // Record count-in. This edits the persisted recording setting and the
        // recording path consumes the same value before capture starts.
        .child(
            div()
                .h(px(20.0))
                .min_w(px(34.0))
                .px(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(Colors::surface_input())
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .text_size(px(8.0))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(if state.count_in_bars > 0 {
                    Colors::accent_primary()
                } else {
                    Colors::text_muted()
                })
                .cursor(gpui::CursorStyle::PointingHand)
                .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                    on_count_in_cycle(&(), window, cx);
                })
                .child(format!("COUNT {}", state.count_in_bars))
                .occlude(),
        )
        // Loop
        .child(
            chrome_button(
                Some(assets::ICON_REPEAT2_PATH),
                "LOOP",
                state.loop_enabled,
                loop_color,
            )
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                on_loop(&(), window, cx);
            })
            .occlude(),
        )
        // Metronome
        .child(
            chrome_button(
                Some(assets::ICON_METRONOME_PATH),
                "MET",
                state.metronome_enabled,
                metronome_color,
            )
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                on_metronome(&(), window, cx);
            })
            .occlude(),
        )
        // Follow playhead / Auto-scroll. Magnet icon reads as "snap to
        // playhead" — same metaphor most DAWs use for this button.
        // Left-click toggles follow on/off; right-click switches the auto-scroll
        // mode between paged (jump) and continuous (smooth) follow.
        .child(
            chrome_button(
                Some(assets::TIMELINE_SCROLL_PATH),
                "FOLLOW",
                state.follow_playhead,
                follow_color,
            )
            .cursor(gpui::CursorStyle::PointingHand)
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                on_follow(&(), window, cx);
            })
            .on_mouse_down(gpui::MouseButton::Right, move |_, window, cx| {
                on_follow_mode(&(), window, cx);
            })
            .occlude(),
        )
        .child(section_separator())
        // Position display
        .child(
            div()
                .w(px(78.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(Colors::surface_base())
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .text_color(Colors::text_primary())
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(state.position_label),
        )
        .child(section_separator())
        // BPM
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(4.0))
                .child(
                    div()
                        .text_color(Colors::text_muted())
                        .text_size(px(9.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("BPM"),
                )
                .child(bpm_display(
                    bpm_value,
                    bpm_label,
                    on_bpm_drag,
                    on_bpm_menu,
                    on_bpm_edit_start,
                    bpm_editing,
                    &bpm_input,
                    bpm_input_callbacks,
                    bpm_edit_focused,
                ))
                // AUTO badge — shown only when tempo automation is active so the
                // single-tempo case stays clean.
                .children(if bpm_has_automation {
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(13.0))
                            .px(px(3.0))
                            .rounded(px(3.0))
                            .bg(Colors::with_alpha(Colors::accent_primary(), 0.18))
                            .border(px(1.0))
                            .border_color(Colors::with_alpha(Colors::accent_primary(), 0.45))
                            .text_color(Colors::accent_primary())
                            .text_size(px(8.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("AUTO"),
                    )
                } else {
                    None
                })
                .child(tap_tempo_chip(
                    tap_tempo_session_taps,
                    on_tap_tempo,
                    on_tap_tempo_menu,
                )),
        )
        // Time signature
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .px(px(4.0))
                .cursor(gpui::CursorStyle::PointingHand)
                .on_mouse_down(gpui::MouseButton::Right, move |event, window, cx| {
                    let x: f32 = event.position.x.into();
                    let y: f32 = event.position.y.into();
                    on_ts_menu(&(x, y), window, cx);
                })
                .children(if ts_has_markers {
                    Some(
                        div()
                            .px(px(3.0))
                            .py(px(1.0))
                            .rounded(px(3.0))
                            .bg(Colors::with_alpha(Colors::accent_primary(), 0.16))
                            .text_size(px(8.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Colors::accent_primary())
                            .child("AUTO")
                            .into_any_element(),
                    )
                } else {
                    None
                })
                .children(if ts_editing {
                    vec![
                        div()
                            .w(px(22.0))
                            .child(text_field_with_callbacks(
                                &ts_num_input,
                                ts_edit_focus_num,
                                ts_num_input_callbacks,
                            ))
                            .into_any_element(),
                        div()
                            .text_color(Colors::text_muted())
                            .text_size(px(10.0))
                            .child("/")
                            .into_any_element(),
                        div()
                            .w(px(22.0))
                            .child(text_field_with_callbacks(
                                &ts_den_input,
                                !ts_edit_focus_num,
                                ts_den_input_callbacks,
                            ))
                            .into_any_element(),
                    ]
                } else {
                    let on_ts_edit = on_ts_edit_start.clone();
                    vec![div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(2.0))
                        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                            if event.click_count >= 2 {
                                on_ts_edit(&(), window, cx);
                            }
                        })
                        .child(
                            div()
                                .w(px(18.0))
                                .h(px(19.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(Colors::surface_input())
                                .text_color(Colors::text_primary())
                                .text_size(px(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(
                                    state
                                        .time_signature_label
                                        .split_once('/')
                                        .map(|(num, _)| num.to_string())
                                        .unwrap_or_else(|| "4".to_string()),
                                ),
                        )
                        .child(
                            div()
                                .text_color(Colors::text_muted())
                                .text_size(px(10.0))
                                .child("/"),
                        )
                        .child(
                            div()
                                .w(px(18.0))
                                .h(px(19.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(Colors::surface_input())
                                .text_color(Colors::text_primary())
                                .text_size(px(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(
                                    state
                                        .time_signature_label
                                        .split_once('/')
                                        .map(|(_, den)| den.to_string())
                                        .unwrap_or_else(|| "4".to_string()),
                                ),
                        )
                        .into_any_element()]
                }),
        )
}

fn panel_toggle_button(
    icon_path: &'static str,
    fallback: &'static str,
    active: bool,
    on_click: ChromeActionCb,
) -> impl IntoElement {
    let color = if active {
        Colors::accent_primary()
    } else {
        Colors::text_muted()
    };
    let on_click = on_click.clone();
    chrome_button(Some(icon_path), fallback, active, color)
        .cursor(gpui::CursorStyle::PointingHand)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_click(&(), window, cx);
        })
        .occlude()
}

fn panel_toggles(state: PanelChromeState) -> impl IntoElement {
    let on_browser = state.on_toggle_browser.clone();
    let on_mixer = state.on_toggle_mixer.clone();
    let on_inspector = state.on_toggle_inspector.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .child(panel_toggle_button(
            assets::ICON_FOLDER_OPEN_PATH,
            "BROWSER",
            state.browser_visible,
            on_browser,
        ))
        .child(panel_toggle_button(
            assets::ICON_PANEL_BOTTOM_PATH,
            "MIXER",
            state.mixer_visible,
            on_mixer,
        ))
        .child(panel_toggle_button(
            assets::ICON_PANEL_RIGHT_PATH,
            "INSPECT",
            state.inspector_visible,
            on_inspector,
        ))
}

fn utility_buttons() -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .px(px(2.0))
        // Import audio
        .child(chrome_button(
            Some(assets::ICON_FOLDER_PATH),
            "IMPORT",
            false,
            Colors::text_muted(),
        ))
        // Save
        .child(chrome_button(
            Some(assets::ICON_SAVE_PATH),
            "SAVE",
            false,
            Colors::text_muted(),
        ))
        // Share
        .child(chrome_button(
            Some(assets::ICON_SHARE_PATH),
            "SHARE",
            false,
            Colors::text_muted(),
        ))
}

fn report_bug_button() -> impl IntoElement {
    let amber_bg = Colors::with_alpha(Colors::status_warning(), 0.07);
    let amber_text = Colors::with_alpha(Colors::status_warning(), 0.70);
    let amber_border = Colors::with_alpha(Colors::status_warning(), 0.22);

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded_md()
        .bg(amber_bg)
        .border_1()
        .border_color(amber_border)
        .hover(|s| {
            s.bg(Colors::with_alpha(Colors::status_warning(), 0.14))
                .border_color(Colors::with_alpha(Colors::status_warning(), 0.40))
        })
        .child(
            svg()
                .path(assets::ICON_BUG_PATH)
                .w(px(11.0))
                .h(px(11.0))
                .text_color(amber_text),
        )
        .child(
            div()
                .text_color(amber_text)
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Report bug"),
        )
        .occlude()
}

// ── Account chip ──────────────────────────────────────────────────────────────

/// Compact app-chrome account control. Present only on builds that install an account
/// provider (Exclusive Edition); Community builds render nothing. Signed out it
/// is a "Sign in" chip; signed in it shows only the compact avatar and opens
/// the account menu. Reads the account snapshot fresh each render, so sign-in /
/// sign-out reflect here without extra wiring.
pub(crate) fn account_chip() -> Option<impl IntoElement> {
    let snapshot = crate::account::current_account()?;
    let signed_in = snapshot.signed_in;
    let action = if signed_in {
        crate::account::AccountAction::OpenMenu
    } else {
        crate::account::AccountAction::SignIn
    };
    let text_color = if signed_in {
        Colors::text_secondary()
    } else {
        Colors::text_muted()
    };

    let mut chip = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(6.0))
        .mr(px(4.0))
        .rounded_md()
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_control_hover()))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            crate::account::dispatch_account_action(action, window, cx);
        });

    chip = if signed_in {
        chip.child(account_avatar(&snapshot))
    } else {
        chip.child(
            svg()
                .path(assets::ICON_USER_PATH)
                .w(px(13.0))
                .h(px(13.0))
                .text_color(text_color),
        )
    };

    Some(if signed_in {
        chip.occlude()
    } else {
        chip.child(
            div()
                .text_size(px(11.0))
                .text_color(text_color)
                .child("Sign in"),
        )
        .occlude()
    })
}

/// Compact circular avatar badge showing the user's first initial. A remote
/// profile image is intentionally deferred (needs async download/decode); the
/// URL is already carried in the session for that follow-up.
fn account_avatar(snapshot: &crate::account::AccountSnapshot) -> impl IntoElement {
    let initial = snapshot
        .username
        .as_deref()
        .or(snapshot.email.as_deref())
        .and_then(|value| value.trim().chars().next())
        .map(|character| character.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());

    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(18.0))
        .h(px(18.0))
        .rounded_full()
        .bg(Colors::accent_muted())
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::accent_primary())
        .child(initial)
}

fn window_controls(window: &gpui::Window, on_close: Option<ChromeActionCb>) -> impl IntoElement {
    let is_maximized = window.is_maximized();
    let (max_path, max_fallback) = if is_maximized {
        (assets::ICON_RESTORE_PATH, "RESTORE")
    } else {
        (assets::ICON_MAXIMIZE_PATH, "MAX")
    };

    let control_button = |area: WindowControlArea,
                          icon_path: &'static str,
                          fallback_text: &'static str,
                          on_linux: Option<ChromeActionCb>| {
        let button =
            crate::components::title_bar::window_control_icon(area, icon_path, fallback_text)
                .w(px(WINDOW_CONTROL_WIDTH))
                .h(px(crate::components::title_bar::TITLEBAR_HEIGHT))
                .rounded_none()
                .window_control_area(area)
                .occlude();

        #[cfg(target_os = "linux")]
        let button = {
            let action = on_linux.clone();
            button.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    WindowControlArea::Close => {
                        if let Some(action) = action.as_ref() {
                            action(&(), window, cx);
                        } else {
                            window.remove_window();
                        }
                    }
                    WindowControlArea::Drag => {}
                }
            })
        };

        #[cfg(not(target_os = "linux"))]
        {
            let _ = on_linux;
        }

        button
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .child(control_button(
            WindowControlArea::Min,
            assets::ICON_MINIMIZE_PATH,
            "-",
            None,
        ))
        .child(control_button(
            WindowControlArea::Max,
            max_path,
            max_fallback,
            None,
        ))
        .child(control_button(
            WindowControlArea::Close,
            assets::ICON_X_PATH,
            "X",
            on_close,
        ))
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn app_chrome(
    window: &gpui::Window,
    open_menu_id: Option<&str>,
    on_open_menu: MenuOpenCb,
    project: ProjectChromeState,
    transport: TransportChromeState,
    panels: PanelChromeState,
    on_window_close: Option<ChromeActionCb>,
) -> impl IntoElement {
    let policy = PlatformChromePolicy::current();
    let viewport_width: f32 = window.bounds().size.width.into();
    let chrome_left: f32 = policy.traffic_light_left_padding().into();
    let menu_width = if policy.show_in_window_menubar {
        menu_bar::menu_bar_chrome_width(viewport_width) + 7.0
    } else {
        0.0
    };
    let project_anchor_x = chrome_left + menu_width;

    let mut chrome = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(policy.titlebar_height_px))
        .w_full()
        .bg(Colors::surface_titlebar())
        .border_b_1()
        .border_color(Colors::border_subtle())
        .pl(policy.traffic_light_left_padding())
        // Windows: NCHITTEST callback returns `HTCAPTION` for hitboxes
        // tagged Drag, letting DefWindowProc start the system move.
        .window_control_area(WindowControlArea::Drag)
        // Linux (Wayland / X11) and macOS: `start_window_move` is the
        // implemented drag API there; the WindowControlArea path is a
        // no-op on those platforms. Safe to attach here because every
        // interactive child below (menu buttons, transport buttons,
        // window controls, report-bug) calls `.occlude()`. Occlude is
        // `HitboxBehavior::BlockMouse`, which breaks the `hit_test`
        // iteration at that child — the chrome's id is then NOT in
        // `mouse_hit_test.ids`, so this on_mouse_down does NOT fire
        // for clicks on those buttons.
        .on_mouse_down(MouseButton::Left, |_, window, _cx| {
            window.start_window_move();
        });

    if policy.show_in_window_menubar {
        chrome = chrome
            .child(menu_area(open_menu_id, on_open_menu, viewport_width))
            .child(section_separator());
    }

    chrome = chrome
        .child(project_title(project, project_anchor_x))
        .child(draggable_spacer())
        .child(transport_controls(transport))
        .child(section_separator())
        .child(panel_toggles(panels))
        .child(section_separator());

    // Account chip sits between the panel toggles and the window controls. Only
    // an Exclusive build with an installed account provider renders it.
    if let Some(chip) = account_chip() {
        chrome = chrome.child(chip).child(section_separator());
    }

    if policy.show_window_controls {
        chrome = chrome.child(window_controls(window, on_window_close));
    }

    chrome
}
