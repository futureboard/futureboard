//! MixerPanel — the bottom-panel mixer view.
//!
//! Layout structure:
//!
//! ```text
//! ┌─ mixer_sub_header ───────────────────────────────────────────────────┐
//! ├──────────────────────────────────────────────────────────────────────┤
//! │                                                                      │
//! │  channel_scroll_area (flex_1, overflow-x scroll)        │  master    │
//! │  ┌───────┐┌───────┐┌───────┐ ...                        │  block     │
//! │  │ strip ││ strip ││ strip │                            │ (fixed)    │
//! │  └───────┘└───────┘└───────┘                            │            │
//! │                                                          │            │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! * Channel strips are a horizontal flex row inside the scroll area; they
//!   never share width with the master block.
//! * The master block is pinned to the right edge and has its own bordered
//!   gutter so the empty middle (when track count is small) reads as
//!   intentional, not as floating dead space.
//! * Strip internals are a vertical stack with explicit shared insert/send
//!   viewport heights; only the lower pan/fader area grows to fill remaining
//!   height.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, svg, AppContext, ClickEvent, DragMoveEvent, Entity, InteractiveElement, IntoElement,
    MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled,
};
use std::collections::HashSet;

use crate::assets;
use crate::i18n::I18n;
use crate::components::fader::{db_scale_column, db_value_pill, fader_with_drag_callbacks};
use crate::components::knob::knob_bipolar;
use crate::components::mixer_render::{MixerRenderSnapshot, MixerRenderViewport, MixerStripGeom};
use crate::components::mixer_surface::{mixer_gpu_primitives_active, render_mixer_primitives};
use crate::components::mixer_tree_sidebar_view::MixerTreeSidebar;
use crate::components::panel::FxSlotDrag;
use crate::components::reorder::{drag_handle, drop_over_highlight};
use crate::components::sidebar::BrowserDragItem;
use crate::components::timeline::timeline_state::{
    is_vsti_output_child_track_id, volume, vsti_output_bus_flat_range,
    vsti_output_bus_strip_indices, vsti_output_child_channels_for_bus_layout,
    vsti_output_child_insert_id, InsertLoadStatus, InsertSlotState, MasterBusState, SendSlotState,
    TrackOutputRouting, TrackState, TrackType, MASTER_TRACK_ID,
};
use crate::components::timeline::vu_meter::meter_surface;
use crate::theme::Colors;

mod callbacks;
mod drag;
mod split;
pub use callbacks::*;
use drag::SendSlotDrag;
pub use split::*;

/// True when a mixer strip should paint as selected (multi-select aware).
#[inline]
fn mixer_strip_is_selected(track_id: &str, primary: Option<&str>, selected_ids: &[String]) -> bool {
    if !selected_ids.is_empty() {
        selected_ids.iter().any(|id| id == track_id)
    } else {
        primary == Some(track_id)
    }
}

/// Maximum insert slots per track. Once reached, the trailing empty "+ Add
/// Insert" slot and the INSERTS header "+" are hidden/disabled.
const MAX_INSERT_SLOTS: usize = 8;

/// Optional clickable "+" affordance for a [`section_header`]. `None` renders
/// the inert decorative plus (used by SENDS and the master strip); `Some`
/// renders an interactive, hit-tested plus that runs `on_click`.
struct HeaderPlus {
    id: gpui::SharedString,
    on_click: std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App)>,
}

// ─── Mixer sub-header ("Mixer  N ch") ────────────────────────────────────────

pub fn mixer_sub_header(track_count: usize, i18n: I18n) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(30.0))
        .px(px(10.0))
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .child(
            svg()
                .path(assets::ICON_SLIDERS_HORIZONTAL_PATH)
                .w(px(14.0))
                .h(px(14.0))
                .text_color(Colors::text_muted()),
        )
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_primary())
                .child(i18n.tr("mixer.title")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .px(px(5.0))
                .py(px(1.0))
                .rounded_md()
                .bg(Colors::button_bg())
                .border(px(1.0))
                .border_color(Colors::border_default())
                .text_size(px(9.0))
                .text_color(Colors::text_secondary())
                .child(format!("{} ch", track_count)),
        )
}

// ─── Section header ──────────────────────────────────────────────────────────

fn section_header(
    label: impl Into<String>,
    _accent: gpui::Rgba,
    plus: Option<HeaderPlus>,
) -> impl IntoElement {
    let label = label.into();
    let section_accent = Colors::accent_primary();
    let soft_accent = Colors::with_alpha(section_accent, 0.55); // Approved: dynamic accent decoration alpha

    // The trailing "+" — interactive when `plus` is Some, otherwise an inert
    // decorative glyph. Interactive variant carries its own id + occlude so the
    // strip's track-select mouse handler can't swallow the click.
    let plus_el: gpui::AnyElement = match plus {
        Some(HeaderPlus { id, on_click }) => div()
            .id(id)
            .w(px(12.0))
            .h(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| s.bg(Colors::surface_control_hover()))
            .child(
                svg()
                    .path(assets::ICON_PLUS_PATH)
                    .w(px(9.0))
                    .h(px(9.0))
                    .text_color(Colors::text_muted()),
            )
            .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
                on_click(w, cx);
            })
            .occlude()
            .into_any_element(),
        None => div()
            .w(px(12.0))
            .h(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .child(
                svg()
                    .path(assets::ICON_PLUS_PATH)
                    .w(px(9.0))
                    .h(px(9.0))
                    .text_color(Colors::text_faint()),
            )
            .into_any_element(),
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .flex_none()
        .gap(px(3.0))
        .h(px(SEC_SECTION_HEADER_H))
        .px(px(5.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(div().w(px(2.0)).h(px(8.0)).rounded_full().bg(soft_accent))
                .child(
                    div()
                        .text_size(px(7.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_secondary())
                        .child(label),
                ),
        )
        .child(plus_el)
}

fn empty_slot(i18n: I18n) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .mx(px(4.0))
        .py(px(2.0))
        .rounded_sm()
        .text_size(px(8.0))
        .text_color(Colors::text_muted())
        .child(i18n.tr("mixer.insert.empty"))
}

fn mixer_track_type_label(track_type: TrackType, i18n: I18n) -> String {
    match track_type {
        TrackType::Audio => i18n.tr("mixer.track-type.audio"),
        TrackType::Midi => i18n.tr("mixer.track-type.midi"),
        TrackType::Instrument => i18n.tr("mixer.track-type.instrument"),
        TrackType::Bus => i18n.tr("mixer.track-type.bus"),
        TrackType::Return => i18n.tr("mixer.track-type.return"),
        TrackType::Group => "GRP".to_string(),
        TrackType::Master => i18n.tr("mixer.track-type.master"),
    }
}

// ─── M/S/R/I buttons ────────────────────────────────────────────────────────

fn msri_button(
    id: gpui::ElementId,
    label: &'static str,
    active: bool,
    active_bg: gpui::Rgba,
    active_fg: gpui::Rgba,
    on_click: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let mut btn = div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(16.0))
        .flex_1()
        .rounded_sm()
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::BOLD)
        .id(id)
        .cursor(gpui::CursorStyle::PointingHand)
        .on_mouse_down(gpui::MouseButton::Left, on_click)
        .child(label);

    if active {
        btn = btn.bg(active_bg).text_color(active_fg);
    } else {
        btn = btn
            .bg(Colors::button_bg())
            .border(px(1.0))
            .border_color(Colors::button_border())
            .text_color(Colors::button_text_muted())
            .hover(|s| s.bg(Colors::button_bg_hover()));
    }
    btn
}

fn button_row(track: &TrackState, callbacks: &MixerCallbacks, id_num: usize) -> impl IntoElement {
    let track_id = track.id.clone();

    let on_mute = {
        let id = track_id.clone();
        let cb = callbacks.on_toggle_mute.clone();
        move |_: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| cb(&id, w, cx)
    };
    let on_solo = {
        let id = track_id.clone();
        let cb = callbacks.on_toggle_solo.clone();
        move |_: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| cb(&id, w, cx)
    };
    let on_arm = {
        let id = track_id.clone();
        let cb = callbacks.on_toggle_arm.clone();
        move |_: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| cb(&id, w, cx)
    };
    let on_input = {
        let id = track_id.clone();
        let cb = callbacks.on_toggle_input.clone();
        move |_: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| cb(&id, w, cx)
    };

    div()
        .flex()
        .flex_row()
        .gap(px(2.0))
        .px(px(4.0))
        .h(px(SEC_BUTTONS_H))
        .items_center()
        .child(msri_button(
            ("mix-m-btn", id_num).into(),
            "M",
            track.muted,
            Colors::accent_warning(),
            Colors::text_inverse(),
            on_mute,
        ))
        .child(msri_button(
            ("mix-s-btn", id_num).into(),
            "S",
            track.solo,
            Colors::accent_success(),
            Colors::text_inverse(),
            on_solo,
        ))
        .child(msri_button(
            ("mix-r-btn", id_num).into(),
            "R",
            track.armed,
            Colors::accent_danger(),
            Colors::text_inverse(),
            on_arm,
        ))
        .child(msri_button(
            ("mix-i-btn", id_num).into(),
            "I",
            track.input_monitor.is_active(track.armed),
            Colors::accent_primary(),
            Colors::text_inverse(),
            on_input,
        ))
}

// ─── Meter ──────────────────────────────────────────────────────────────────

// ─── Strip sections ─────────────────────────────────────────────────────────

fn vsti_output_group_key(track_id: &str, insert_id: &str) -> String {
    format!("{track_id}:{insert_id}")
}

fn strip_header(
    track: &TrackState,
    index: usize,
    vsti_output_group: Option<(&str, bool, usize, &MixerCallbacks)>,
    i18n: I18n,
) -> impl IntoElement {
    let type_label = mixer_track_type_label(track.track_type, i18n);
    let channel_label = i18n.tr_vars(
        "mixer.channel",
        &[("nn", format!("{:02}", index + 1))],
    );

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .h(px(SEC_HEADER_H))
        .px(px(5.0))
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .child(div().w(px(2.0)).h(px(20.0)).rounded_full().bg(track.color))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex_1()
                        .truncate()
                        .text_size(px(10.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(track.name.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_size(px(7.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(Colors::text_secondary())
                                .child(type_label),
                        )
                        .child(
                            div()
                                .text_size(px(7.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(Colors::text_secondary())
                                .child(channel_label),
                        ),
                ),
        )
        .children(
            vsti_output_group.map(|(group_key, expanded, count, callbacks)| {
                let group_key = group_key.to_string();
                let toggle = callbacks.on_toggle_vsti_output_group.clone();
                div()
                    .id(gpui::SharedString::from(format!(
                        "vsti-output-group-toggle-{group_key}"
                    )))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(18.0))
                    .h(px(20.0))
                    .rounded_sm()
                    .border(px(1.0))
                    .border_color(Colors::border_default())
                    .bg(if count > 0 {
                        Colors::accent_muted()
                    } else {
                        Colors::button_bg()
                    })
                    .cursor(gpui::CursorStyle::PointingHand)
                    .hover(|s| s.bg(Colors::surface_control_hover()))
                    .child(
                        svg()
                            .path(if expanded {
                                assets::ICON_CHEVRON_DOWN_PATH
                            } else {
                                assets::ICON_CHEVRON_RIGHT_PATH
                            })
                            .w(px(11.0))
                            .h(px(11.0))
                            .text_color(Colors::text_primary()),
                    )
                    .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
                        toggle(&group_key, w, cx);
                    })
                    .occlude()
            }),
        )
}

/// Output bus indices that get their own mixer sub-strip for this instrument
/// insert, derived from the plugin's REAL per-bus output layout (never by
/// blindly pairing channels two-by-two). One strip per real output bus:
/// a mono bus is one strip, a stereo bus is one strip, a multi-channel bus is
/// one strip. Mirrors `vsti_output_bus_strip_indices` / `ensure_vsti_output_child_tracks`
/// so the visible sub-strips line up 1:1 with the model child tracks and the
/// engine routes.
fn vsti_output_bus_strips(slot: &InsertSlotState) -> Vec<u8> {
    vsti_output_bus_strip_indices(&slot.output_bus_channel_counts)
}

/// Human-readable label for a VSTi output bus strip, reflecting the real bus
/// layout: `"Out 1 (Mono / Ch 1)"`, `"Out 2 (Mono / Ch 2)"`,
/// `"Out 3 (Stereo / Ch 3/4)"`, or `"Out N (Multi / Ch X-Y)"` for buses with
/// more than two channels. Falls back to a plain channel-pair label when the
/// host has not reported a layout yet.
pub fn vsti_output_bus_label(bus_counts: &[u8], bus_index: u8) -> String {
    let n = (bus_index as u16).saturating_add(1);
    // A single multichannel bus is split into flat stereo pairs; describe each
    // pair from its mapped flat channels rather than the (single) bus range.
    if bus_counts.len() == 1 && bus_counts[0] > 2 {
        if let Some((l, r)) = vsti_output_child_channels_for_bus_layout(bus_counts, bus_index) {
            return if l == r {
                format!("Out {n} (Mono / Ch {l})")
            } else {
                format!("Out {n} (Stereo / Ch {l}/{r})")
            };
        }
    }
    if let Some((start, count)) = vsti_output_bus_flat_range(bus_counts, bus_index as usize) {
        return match count {
            0 | 1 => format!("Out {n} (Mono / Ch {start})"),
            2 => format!("Out {n} (Stereo / Ch {start}/{})", start.saturating_add(1)),
            c => {
                let end = (start as u16).saturating_add(c as u16).saturating_sub(1);
                format!("Out {n} (Multi / Ch {start}-{end})")
            }
        };
    }
    // Unknown layout (host hasn't reported): legacy consecutive pair label.
    if let Some((l, r)) = vsti_output_child_channels_for_bus_layout(bus_counts, bus_index) {
        return format!("Out {n} (Ch {l}/{r})");
    }
    format!("Out {n}")
}

fn log_vsti_child_meter_subscribe_once(track: &TrackState) {
    if !crate::forensic_trace::forensic_trace_enabled() || !is_vsti_output_child_track_id(&track.id)
    {
        return;
    }
    static LOGGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let logged = LOGGED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let Ok(mut logged) = logged.lock() else {
        return;
    };
    if !logged.insert(track.id.clone()) {
        return;
    }
    let bus_index = track
        .id
        .rsplit_once(":bus:")
        .and_then(|(_, bus)| bus.parse::<u8>().ok())
        .unwrap_or(0);
    eprintln!(
        "[METER SUBSCRIBE]\nstrip_view_id={}\nsubscription_key={}\nmixer_channel_id={}\nbus_index={}\ninitial_peak_l={:.6}\ninitial_peak_r={:.6}",
        track.id, track.id, track.id, bus_index, track.meter_level_l, track.meter_level_r
    );
}

fn log_vsti_child_strip_state(track: &TrackState) {
    if !crate::forensic_trace::forensic_trace_enabled() || !is_vsti_output_child_track_id(&track.id)
    {
        return;
    }
    let bus_index = track
        .id
        .rsplit_once(":bus:")
        .and_then(|(_, bus)| bus.parse::<u8>().ok())
        .unwrap_or(0);
    let plugin_instance_id = track
        .id
        .strip_prefix("vsti-out:")
        .and_then(|rest| rest.split_once(":bus:").map(|(plugin, _)| plugin))
        .unwrap_or("");
    eprintln!(
        "[STRIP STATE]\nstrip_view_id={}\nplugin_instance_id={}\nbus_index={}\nmixer_channel_id={}\nvu_peak_l={:.6}\nvu_peak_r={:.6}\nmute_state={}\nsolo_state={}",
        track.id,
        plugin_instance_id,
        bus_index,
        track.id,
        track.meter_level_l,
        track.meter_level_r,
        track.muted,
        track.solo
    );
}

fn insert_chip(
    track_id: &str,
    insert_index: usize,
    slot: &InsertSlotState,
    callbacks: &MixerCallbacks,
) -> impl IntoElement {
    let track_id_owned = track_id.to_string();
    let slot_id = slot.id.clone();
    let display = slot.display_name.clone();
    let display_for_log = display.clone();
    let bypassed = slot.bypassed;
    let on_open = callbacks.on_open_insert_editor.clone();
    let on_bypass = callbacks.on_toggle_insert_bypass.clone();
    let on_remove = callbacks.on_remove_insert.clone();

    let (bg, text) = match &slot.load_status {
        InsertLoadStatus::Ready if !bypassed => (Colors::accent_muted(), Colors::text_primary()),
        InsertLoadStatus::Ready => (Colors::button_bg(), Colors::text_muted()),
        InsertLoadStatus::Loading => (Colors::button_bg(), Colors::text_muted()),
        InsertLoadStatus::Missing(_) | InsertLoadStatus::Failed(_) => (
            Colors::with_alpha(Colors::status_error(), 0.16),
            Colors::status_error(),
        ),
        InsertLoadStatus::Disabled => (Colors::button_bg(), Colors::text_muted()),
        InsertLoadStatus::Empty => (Colors::button_bg(), Colors::slot_empty_text()),
    };

    let id_owned = slot_id.clone();
    let bypass_pair = (track_id_owned.clone(), slot_id.clone());
    let remove_pair = (track_id_owned.clone(), slot_id.clone());

    // Drag payload carries the stable plugin_instance_id, so reorder identity
    // follows the instance rather than the visual index. The grip and the chip
    // body both start the same drag; child controls occlude their own clicks.
    let drag_payload = FxSlotDrag {
        track_id: track_id_owned.clone(),
        insert_id: slot_id.clone(),
        display_name: slot.display_name.clone(),
    };
    let chip_drag_payload = drag_payload.clone();
    let handle = drag_handle()
        .id(gpui::SharedString::from(format!(
            "mixer-fx-drag-{track_id}-{slot_id}"
        )))
        .occlude()
        .on_drag(drag_payload, |drag, _offset, _window, cx| {
            cx.new(|_| drag.clone())
        });

    // Drop target: dropping a compatible drag onto this chip moves it into the
    // gap *above* this slot (`insertion_index == insert_index`, the slot's full
    // insert-chain index). `can_drop` restricts drops to the same track and
    // `drag_over` paints the shared accent drop-position line.
    let drop_track = track_id_owned.clone();
    let can_drop_track = track_id_owned.clone();
    let reorder = callbacks.on_reorder_insert.clone();
    let drop_plugin_preset = callbacks.on_drop_plugin_preset.clone();
    let drop_gap = insert_index;
    let preset_track = track_id_owned.clone();
    let preset_slot = insert_index;

    let open_target = (track_id_owned, insert_index, slot_id);

    div()
        .id(gpui::SharedString::from(format!(
            "insert-chip-{}",
            id_owned
        )))
        .can_drop(move |dragged, _window, _cx| {
            if dragged
                .downcast_ref::<FxSlotDrag>()
                .is_some_and(|d| d.track_id == can_drop_track)
            {
                return true;
            }
            dragged
                .downcast_ref::<BrowserDragItem>()
                .is_some_and(|item| is_plugin_preset_path(&item.path))
        })
        .drag_over::<FxSlotDrag>(|style, _drag, _window, _cx| drop_over_highlight(style))
        .drag_over::<BrowserDragItem>(|style, _drag, _window, _cx| drop_over_highlight(style))
        .on_drop::<FxSlotDrag>(move |drag, window, cx| {
            if drag.track_id == drop_track {
                reorder(
                    &(drop_track.clone(), drag.insert_id.clone(), drop_gap),
                    window,
                    cx,
                );
            }
        })
        .on_drop::<BrowserDragItem>(move |item, window, cx| {
            if is_plugin_preset_path(&item.path) {
                drop_plugin_preset(
                    &(item.path.clone(), preset_track.clone(), preset_slot),
                    window,
                    cx,
                );
            }
        })
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .gap(px(3.0))
        .mx(px(2.0))
        .px(px(4.0))
        .h(px(18.0))
        .rounded_sm()
        .bg(bg)
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(text)
        .cursor(gpui::CursorStyle::PointingHand)
        .on_drag(chip_drag_payload, |drag, _offset, _window, cx| {
            cx.new(|_| drag.clone())
        })
        .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
            eprintln!(
                "[mixer] insert row clicked track_id={} insert_index={} plugin={} plugin_instance_id={}",
                open_target.0, open_target.1, display_for_log, open_target.2
            );
            on_open(&open_target, w, cx);
        })
        .occlude()
        // Grip drag handle (leftmost) mirrors the whole-chip drag affordance.
        .child(handle)
        .child(div().flex_1().min_w(px(0.0)).truncate().child(display))
        // Bypass dot — small interactive square.
        .child(
            div()
                .id(gpui::SharedString::from(format!(
                    "insert-bypass-{}",
                    bypass_pair.1
                )))
                .w(px(8.0))
                .h(px(8.0))
                .rounded_sm()
                .bg(if bypassed {
                    Colors::text_faint()
                } else {
                    Colors::status_success()
                })
                .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
                    on_bypass(&bypass_pair, w, cx);
                })
                .occlude(),
        )
        // Remove ×.
        .child(
            div()
                .id(gpui::SharedString::from(format!(
                    "insert-remove-{}",
                    remove_pair.1
                )))
                .text_size(px(10.0))
                .text_color(Colors::text_muted())
                .px(px(2.0))
                .cursor(gpui::CursorStyle::PointingHand)
                .child("×")
                .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
                    on_remove(&remove_pair, w, cx);
                })
                .occlude(),
        )
}

/// Trailing drop zone rendered below the last insert chip so a dragged slot can
/// land at the very end of the chain (`gap == inserts.len()`); the per-chip drop
/// targets only cover the gaps *above* each existing slot. Same-track guarded and
/// shows the shared accent drop-position line while a compatible drag hovers.
fn insert_drop_end(track_id: &str, gap: usize, callbacks: &MixerCallbacks) -> impl IntoElement {
    let track_id_owned = track_id.to_string();
    let can_drop_track = track_id_owned.clone();
    let reorder = callbacks.on_reorder_insert.clone();
    div()
        .id(gpui::SharedString::from(format!(
            "mixer-fx-drop-end-{track_id_owned}"
        )))
        .flex_none()
        .h(px(6.0))
        .mx(px(2.0))
        .can_drop(move |dragged, _window, _cx| {
            dragged
                .downcast_ref::<FxSlotDrag>()
                .is_some_and(|d| d.track_id == can_drop_track)
        })
        .drag_over::<FxSlotDrag>(|style, _drag, _window, _cx| drop_over_highlight(style))
        .on_drop::<FxSlotDrag>(move |drag, window, cx| {
            if drag.track_id == track_id_owned {
                reorder(
                    &(track_id_owned.clone(), drag.insert_id.clone(), gap),
                    window,
                    cx,
                );
            }
        })
}

/// Trailing empty insert slot. Clicking it opens the plugin picker for the
/// next available slot (`next_slot`) on this track. `next_slot` is used for
/// debug logging only — the picker appends to the track's insert chain.
fn add_insert_button(
    track_id: &str,
    next_slot: usize,
    callbacks: &MixerCallbacks,
) -> impl IntoElement {
    let track_id_owned = track_id.to_string();
    let on_add = callbacks.on_add_insert.clone();
    let drop_plugin_preset = callbacks.on_drop_plugin_preset.clone();
    let drop_track = track_id_owned.clone();
    div()
        .id(gpui::SharedString::from(format!(
            "insert-add-{}",
            track_id_owned
        )))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(3.0))
        .mx(px(2.0))
        .px(px(4.0))
        .h(px(18.0))
        .rounded_sm()
        .border(px(1.0))
        .border_dashed()
        .border_color(Colors::border_default())
        .bg(Colors::button_bg())
        .text_size(px(9.0))
        .text_color(Colors::text_muted())
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| {
            s.bg(Colors::button_bg_hover())
                .border_color(Colors::accent_primary())
                .text_color(Colors::text_secondary())
        })
        .can_drop(|dragged, _window, _cx| {
            dragged
                .downcast_ref::<BrowserDragItem>()
                .is_some_and(|item| is_plugin_preset_path(&item.path))
        })
        .drag_over::<BrowserDragItem>(|style, _drag, _window, _cx| drop_over_highlight(style))
        .on_drop::<BrowserDragItem>(move |item, window, cx| {
            if is_plugin_preset_path(&item.path) {
                drop_plugin_preset(
                    &(item.path.clone(), drop_track.clone(), next_slot),
                    window,
                    cx,
                );
            }
        })
        .child(
            svg()
                .path(assets::ICON_PLUS_PATH)
                .w(px(8.0))
                .h(px(8.0))
                .text_color(Colors::text_muted()),
        )
        .child("Insert")
        .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
            eprintln!(
                "[mixer] empty insert slot + clicked track={track_id_owned} slot={next_slot}"
            );
            on_add(&track_id_owned, w, cx);
        })
        .occlude()
}

fn is_plugin_preset_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pst"))
}

fn inserts_section(
    track: &TrackState,
    _index: usize,
    callbacks: &MixerCallbacks,
    height_px: f32,
    i18n: I18n,
) -> impl IntoElement {
    let effect_start = if track.track_type == TrackType::Instrument {
        1
    } else {
        0
    };
    let used = track.inserts.len();
    let at_max = used >= MAX_INSERT_SLOTS;

    let mut chips = div().flex().flex_col().flex_none().gap(px(2.0)).px(px(2.0));
    let effects = track.effect_inserts();
    for (offset, slot) in effects.iter().enumerate() {
        let insert_index = effect_start + offset;
        chips = chips.child(insert_chip(&track.id, insert_index, slot, callbacks));
    }
    // Drop-at-end zone below the last chip (gap == full insert-chain length, so
    // the instrument slot at index 0 is counted). Only meaningful once a slot
    // exists to drag.
    if !effects.is_empty() {
        chips = chips.child(insert_drop_end(&track.id, track.inserts.len(), callbacks));
    }
    // Requirement: always render one trailing empty slot after the last insert,
    // until MAX_INSERT_SLOTS is reached.
    if !at_max {
        chips = chips.child(add_insert_button(&track.id, used, callbacks));
    }

    // Header "+" adds to the next available slot for *this* track; hidden
    // (inert) once the rack is full.
    let header_plus = if at_max {
        None
    } else {
        let track_id = track.id.clone();
        let on_add = callbacks.on_add_insert.clone();
        Some(HeaderPlus {
            id: gpui::SharedString::from(format!("insert-header-add-{}", track.id)),
            on_click: std::sync::Arc::new(move |w, cx| {
                eprintln!("[mixer] INSERTS header + clicked track={track_id} slot={used}");
                on_add(&track_id, w, cx);
            }),
        })
    };

    div()
        .flex()
        .flex_col()
        .flex_none()
        .h(px(height_px))
        .overflow_hidden()
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .child(section_header(i18n.tr("mixer.section.inserts"), track.color, header_plus))
        .child(
            div()
                .id(gpui::SharedString::from(format!(
                    "insert-slot-scroll-{}",
                    track.id
                )))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(chips),
        )
}

fn master_inserts_section(
    accent: gpui::Rgba,
    master: &MasterBusState,
    callbacks: &MixerCallbacks,
    height_px: f32,
    i18n: I18n,
) -> impl IntoElement {
    let used = master.inserts.len();
    let at_max = used >= MAX_INSERT_SLOTS;

    let mut chips = div().flex().flex_col().flex_none().gap(px(2.0)).px(px(2.0));
    for (insert_index, slot) in master.inserts.iter().enumerate() {
        chips = chips.child(insert_chip(MASTER_TRACK_ID, insert_index, slot, callbacks));
    }
    if !master.inserts.is_empty() {
        chips = chips.child(insert_drop_end(
            MASTER_TRACK_ID,
            master.inserts.len(),
            callbacks,
        ));
    }
    if !at_max {
        chips = chips.child(add_insert_button(MASTER_TRACK_ID, used, callbacks));
    }

    let header_plus = if at_max {
        None
    } else {
        let on_add = callbacks.on_add_insert.clone();
        Some(HeaderPlus {
            id: gpui::SharedString::from("insert-header-add-master"),
            on_click: std::sync::Arc::new(move |w, cx| {
                eprintln!("[mixer] INSERTS header + clicked track=master slot={used}");
                on_add(&MASTER_TRACK_ID.to_string(), w, cx);
            }),
        })
    };

    div()
        .flex()
        .flex_col()
        .flex_none()
        .h(px(height_px))
        .overflow_hidden()
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .child(section_header(i18n.tr("mixer.section.inserts"), accent, header_plus))
        .child(
            div()
                .id("insert-slot-scroll-master")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(chips),
        )
}

fn send_chip(
    track_id: &str,
    send_index: usize,
    send: &SendSlotState,
    target_name: &str,
    callbacks: &MixerCallbacks,
) -> impl IntoElement {
    let remove_pair = (track_id.to_string(), send.id.clone());
    let on_remove = callbacks.on_remove_send.clone();
    let (bg, text) = if send.enabled {
        (Colors::accent_muted(), Colors::text_primary())
    } else {
        (Colors::button_bg(), Colors::text_muted())
    };
    let drag_payload = SendSlotDrag {
        track_id: track_id.to_string(),
        send_id: send.id.clone(),
        target_name: target_name.to_string(),
    };
    let chip_drag_payload = drag_payload.clone();
    let handle = drag_handle()
        .id(gpui::SharedString::from(format!(
            "mixer-send-drag-{}-{}",
            track_id, send.id
        )))
        .occlude()
        .on_drag(drag_payload, |drag, _offset, _window, cx| {
            cx.new(|_| drag.clone())
        });
    let can_drop_track = track_id.to_string();
    let drop_track = track_id.to_string();
    let reorder = callbacks.on_reorder_send.clone();
    let gain_pair = (track_id.to_string(), send.id.clone());
    let gain_change = callbacks.on_send_gain_change.clone();
    let gain_reset_pair = gain_pair.clone();
    let gain_reset = callbacks.on_send_gain_change.clone();
    let gain_norm = ((send.gain_db.clamp(-60.0, 6.0) + 60.0) / 66.0).clamp(0.0, 1.0);
    let gain_label = if send.gain_db <= -59.95 {
        "-∞".to_string()
    } else {
        format!("{:+.1} dB", send.gain_db)
    };
    div()
        .id(gpui::SharedString::from(format!("send-chip-{}", send.id)))
        .can_drop(move |dragged, _window, _cx| {
            dragged
                .downcast_ref::<SendSlotDrag>()
                .is_some_and(|d| d.track_id == can_drop_track)
        })
        .drag_over::<SendSlotDrag>(|style, _drag, _window, _cx| drop_over_highlight(style))
        .on_drop::<SendSlotDrag>(move |drag, window, cx| {
            if drag.track_id == drop_track {
                reorder(
                    &(drop_track.clone(), drag.send_id.clone(), send_index),
                    window,
                    cx,
                );
            }
        })
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .mx(px(2.0))
        .px(px(3.0))
        .h(px(26.0))
        .rounded_sm()
        .bg(bg)
        .text_size(px(8.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(text)
        .cursor(gpui::CursorStyle::PointingHand)
        .on_drag(chip_drag_payload, |drag, _offset, _window, cx| {
            cx.new(|_| drag.clone())
        })
        .child(handle)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(0.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(div().truncate().child(format!("→ {target_name}")))
                        .child(
                            div()
                                .ml(px(3.0))
                                .text_size(px(7.0))
                                .text_color(Colors::text_secondary())
                                .child(gain_label),
                        ),
                )
                .child(crate::components::slider::compact_slider_with_reset(
                    format!("mixer-send-gain-{}-{}", track_id, send.id),
                    gain_norm,
                    Colors::accent_primary(),
                    move |value, window, cx| {
                        let gain_db = (*value * 66.0 - 60.0).clamp(-60.0, 6.0);
                        gain_change(
                            &(gain_pair.0.clone(), gain_pair.1.clone(), gain_db),
                            window,
                            cx,
                        );
                    },
                    Some(move |window: &mut gpui::Window, cx: &mut gpui::App| {
                        gain_reset(
                            &(gain_reset_pair.0.clone(), gain_reset_pair.1.clone(), 0.0),
                            window,
                            cx,
                        );
                    }),
                )),
        )
        .child(
            div()
                .id(gpui::SharedString::from(format!("send-remove-{}", send.id)))
                .text_size(px(10.0))
                .text_color(Colors::text_muted())
                .px(px(2.0))
                .child("×")
                .on_mouse_down(gpui::MouseButton::Left, move |_e, w, cx| {
                    on_remove(&remove_pair, w, cx);
                })
                .occlude(),
        )
}

fn send_drop_end(track_id: &str, gap: usize, callbacks: &MixerCallbacks) -> impl IntoElement {
    let track_id_owned = track_id.to_string();
    let can_drop_track = track_id_owned.clone();
    let reorder = callbacks.on_reorder_send.clone();
    div()
        .id(gpui::SharedString::from(format!(
            "mixer-send-drop-end-{track_id_owned}"
        )))
        .flex_none()
        .h(px(6.0))
        .mx(px(2.0))
        .can_drop(move |dragged, _window, _cx| {
            dragged
                .downcast_ref::<SendSlotDrag>()
                .is_some_and(|d| d.track_id == can_drop_track)
        })
        .drag_over::<SendSlotDrag>(|style, _drag, _window, _cx| drop_over_highlight(style))
        .on_drop::<SendSlotDrag>(move |drag, window, cx| {
            if drag.track_id == track_id_owned {
                reorder(
                    &(track_id_owned.clone(), drag.send_id.clone(), gap),
                    window,
                    cx,
                );
            }
        })
}

fn add_send_button(track_id: &str, callbacks: &MixerCallbacks) -> impl IntoElement {
    let track_id_owned = track_id.to_string();
    let on_add = callbacks.on_add_send.clone();
    div()
        .id(gpui::SharedString::from(format!(
            "send-add-{}",
            track_id_owned
        )))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(3.0))
        .mx(px(2.0))
        .px(px(4.0))
        .h(px(18.0))
        .rounded_sm()
        .border(px(1.0))
        .border_dashed()
        .border_color(Colors::border_default())
        .bg(Colors::button_bg())
        .text_size(px(9.0))
        .text_color(Colors::text_muted())
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| {
            s.bg(Colors::button_bg_hover())
                .border_color(Colors::accent_primary())
                .text_color(Colors::text_secondary())
        })
        .child(
            svg()
                .path(assets::ICON_PLUS_PATH)
                .w(px(8.0))
                .h(px(8.0))
                .text_color(Colors::text_muted()),
        )
        .child("Send")
        .on_mouse_down(gpui::MouseButton::Left, move |event, w, cx| {
            let x: f32 = event.position.x.into();
            let y: f32 = event.position.y.into();
            on_add(&(track_id_owned.clone(), x, y), w, cx);
        })
        .occlude()
}

fn sends_section(
    track: &TrackState,
    all_tracks: &[TrackState],
    callbacks: &MixerCallbacks,
    height_px: f32,
    i18n: I18n,
) -> impl IntoElement {
    // Bus/return strips carry an aux-send rack so chained send/return paths
    // (bus → return, return → bus) are available from the mixer.
    let mut chips = div().flex().flex_col().flex_none().gap(px(2.0)).px(px(2.0));
    for (send_index, send) in track.sends.iter().enumerate() {
        // Resolve the live target name (handles renames) with the stored
        // label as a fallback.
        let target_name = all_tracks
            .iter()
            .find(|t| t.id == send.target_track_id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| send.target_name.clone());
        chips = chips.child(send_chip(
            &track.id,
            send_index,
            send,
            &target_name,
            callbacks,
        ));
    }
    if !track.sends.is_empty() {
        chips = chips.child(send_drop_end(&track.id, track.sends.len(), callbacks));
    }
    chips = chips.child(add_send_button(&track.id, callbacks));

    div()
        .flex()
        .flex_col()
        .flex_none()
        .h(px(height_px))
        .overflow_hidden()
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .child(section_header(i18n.tr("mixer.section.sends"), track.color, None))
        .child(
            div()
                .id(gpui::SharedString::from(format!(
                    "send-slot-scroll-{}",
                    track.id
                )))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(chips),
        )
}

fn pan_section(
    track: &TrackState,
    callbacks: &MixerCallbacks,
    _is_selected: bool,
) -> impl IntoElement {
    let track_id = track.id.clone();
    let pan_cb = callbacks.on_pan_change.clone();
    let on_pan_change = move |new_pan: &f32, w: &mut gpui::Window, cx: &mut gpui::App| {
        pan_cb(&(track_id.clone(), *new_pan), w, cx);
    };

    // Match web MixerPanel pan row: knob only, then a tight L/R legend (no caption
    // under the disk — center is shown by the bipolar tick + arc).
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.0))
        .h(px(SEC_PAN_H))
        .py(px(5.0))
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .child(knob_bipolar(
            format!("mix-pan-{}", track.id),
            track.pan,
            -1.0,
            1.0,
            Colors::accent_primary(),
            None,
            0.0,
            on_pan_change,
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .px(px(8.0))
                .child(
                    div()
                        .text_size(px(7.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(Colors::text_secondary())
                        .child("L"),
                )
                .child(
                    div()
                        .text_size(px(7.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(Colors::text_secondary())
                        .child("R"),
                ),
        )
}

fn fader_area(
    track: &TrackState,
    callbacks: &MixerCallbacks,
    is_selected: bool,
) -> impl IntoElement {
    let display_vol = track.display_volume();
    let db_str = volume::format_db(display_vol);
    let has_volume_automation = track.has_active_volume_automation();
    let automation_reading = has_volume_automation && track.volume_automation_read;
    let track_id = track.id.clone();
    let start_cb = callbacks.on_volume_drag_start.clone();
    let start_id = track_id.clone();
    let on_vol_start = move |new_norm: &f32, w: &mut gpui::Window, cx: &mut gpui::App| {
        start_cb(&(start_id.clone(), *new_norm), w, cx);
    };
    let preview_cb = callbacks.on_volume_drag_preview.clone();
    let preview_id = track_id.clone();
    let on_vol_preview = move |new_norm: &f32, w: &mut gpui::Window, cx: &mut gpui::App| {
        preview_cb(&(preview_id.clone(), *new_norm), w, cx);
    };
    let commit_cb = callbacks.on_volume_drag_commit.clone();
    let commit_id = track_id.clone();
    let on_vol_commit = move |w: &mut gpui::Window, cx: &mut gpui::App| {
        commit_cb(&commit_id, w, cx);
    };
    let reset_cb = callbacks.on_volume_change.clone();
    let reset_id = track_id;
    let on_vol_reset = move |w: &mut gpui::Window, cx: &mut gpui::App| {
        reset_cb(&(reset_id.clone(), volume::db_to_norm(0.0)), w, cx);
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(SEC_FADER_MIN_H))
        .items_center()
        .w_full()
        .px(px(4.0))
        .pt(px(5.0))
        .pb(px(6.0))
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(px(3.0))
                .child(db_value_pill(db_str, is_selected || automation_reading))
                .when(has_volume_automation, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(14.0))
                            .min_w(px(16.0))
                            .px(px(3.0))
                            .rounded_sm()
                            .bg(if automation_reading {
                                Colors::accent_muted()
                            } else {
                                Colors::button_bg()
                            })
                            .border(px(1.0))
                            .border_color(if automation_reading {
                                Colors::accent_primary()
                            } else {
                                Colors::border_default()
                            })
                            .text_size(px(8.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if automation_reading {
                                Colors::text_primary()
                            } else {
                                Colors::text_muted()
                            })
                            .child("A"),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(2.0))
                .flex_1()
                .min_h_0()
                .w_full()
                .justify_center()
                .child(db_scale_column())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .h_full()
                        .items_center()
                        .justify_center()
                        .child(fader_with_drag_callbacks(
                            format!("mix-fader-{}", track.id),
                            display_vol,
                            Colors::accent_primary(),
                            Some(on_vol_start),
                            Some(on_vol_preview),
                            Some(on_vol_commit),
                            Some(on_vol_reset),
                        )),
                )
                .child(meter_surface(
                    track.meter_level_l,
                    track.meter_level_r,
                    track.meter_peak_hold_l,
                    track.meter_peak_hold_r,
                    track.meter_clip,
                )),
        )
}

fn strip_footer(name: &str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(SEC_FOOTER_H))
        .px(px(4.0))
        .border_t(px(1.0))
        .border_color(Colors::border_default())
        .bg(Colors::surface_panel_alt())
        .child(
            div()
                .w_full()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_secondary())
                .child(name.to_string()),
        )
}

/// Human-readable output-routing label for a strip: `Main`, the resolved
/// Bus/Return name, or the routing's own fallback label.
fn output_routing_label(track: &TrackState, all_tracks: &[TrackState]) -> String {
    match &track.routing.output {
        TrackOutputRouting::Main => "Main".to_string(),
        TrackOutputRouting::Bus { bus_id } => all_tracks
            .iter()
            .find(|t| &t.id == bus_id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| bus_id.clone()),
        other => other.label(),
    }
}

/// Vertical drop applied to the output picker's anchor so the menu opens
/// just under the OUT pill rather than on top of it. Half the pill height
/// (16px) plus a small gap clears the pill for a centred click.
const OUTPUT_BUTTON_MENU_DROP: f32 = 12.0;

/// Output-routing dropdown button. Shows the current target and opens the
/// output picker (Main / Bus / Return) on click, wired through
/// `on_open_output_picker` to the real `set_track_output_routing`.
fn output_button(
    track_id: &str,
    label: String,
    id_num: usize,
    on_open: std::sync::Arc<
        dyn Fn(&(String, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static,
    >,
) -> impl IntoElement {
    let track_id = track_id.to_string();
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(22.0))
        .px(px(4.0))
        .child(
            div()
                .id(("mix-output-btn", id_num))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(px(4.0))
                .h(px(16.0))
                .px(px(6.0))
                .max_w_full()
                .min_w(px(0.0))
                .rounded_sm()
                .bg(Colors::button_bg())
                .border(px(1.0))
                .border_color(Colors::border_default())
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|s| {
                    s.bg(Colors::surface_control_hover())
                        .border_color(Colors::border_strong())
                })
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(8.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_muted())
                        .child(label),
                )
                .child(
                    svg()
                        .path(assets::ICON_CHEVRON_DOWN_PATH)
                        .w(px(9.0))
                        .h(px(9.0))
                        .flex_shrink_0()
                        .text_color(Colors::text_faint()),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    move |event: &gpui::MouseDownEvent, w, cx| {
                        let x: f32 = event.position.x.into();
                        // Drop the menu just below the button instead of at the
                        // click point, so it doesn't cover the button/footer.
                        // The click lands inside the 16px pill; offsetting by ~half
                        // its height clears the pill's bottom edge. The overlay
                        // positioner still flips upward if it can't fit below.
                        let y: f32 = f32::from(event.position.y) + OUTPUT_BUTTON_MENU_DROP;
                        on_open(&(track_id.clone(), x, y), w, cx);
                        cx.stop_propagation();
                    },
                )
                .occlude(),
        )
}

// ─── Vertical split handle ───────────────────────────────────────────────────

/// Compact horizontal splitter between mixer vertical sections. 6px hitbox
/// with a short centered grip line; hover/active use theme tokens (no web-style
/// chunky handle). `id_num` namespaces the GPUI element id per strip so
/// drag/click state never bleeds between strips. The drag uses the same
/// `on_drag` + ancestor `on_drag_move` capture pattern as the bottom panel.
fn vertical_split_handle(
    id_num: usize,
    target: MixerSplitTarget,
    split: &MixerSplit,
) -> impl IntoElement {
    let on_down = split.on_action.clone();
    let on_dbl = split.on_action.clone();
    let is_resizing = split.active_target == Some(target);

    let grip = if is_resizing {
        Colors::accent_primary()
    } else {
        Colors::border_subtle()
    };

    let mut handle = div()
        .id((
            match target {
                MixerSplitTarget::InsertSend => "mix-split-insert-send",
                MixerSplitTarget::SendFader => "mix-split-send-fader",
            },
            id_num,
        ))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(SEC_SPLITTER_H))
        .border_b(px(1.0))
        .border_color(Colors::border_default())
        .cursor(gpui::CursorStyle::ResizeUpDown)
        .child(div().w(px(20.0)).h(px(1.0)).rounded_full().bg(grip))
        .on_mouse_down(gpui::MouseButton::Left, move |e: &MouseDownEvent, w, cx| {
            let y: f32 = e.position.y.into();
            on_down(MixerSplitAction::ResizeStart(target, y), w, cx);
        })
        .on_drag(MixerSplitDrag, |_drag, _offset, _window, cx| {
            cx.new(|_| MixerSplitDrag)
        })
        .on_click(move |e: &ClickEvent, w, cx| {
            if e.click_count() >= 2 {
                on_dbl(MixerSplitAction::Reset(target), w, cx);
            }
        })
        .occlude();

    if is_resizing {
        handle = handle.bg(Colors::accent_soft());
    } else {
        handle = handle.hover(|s| s.bg(Colors::surface_control_hover()));
    }
    handle
}

// ─── Channel strip ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn channel_strip(
    track: &TrackState,
    all_tracks: &[TrackState],
    index: usize,
    is_selected: bool,
    callbacks: &MixerCallbacks,
    split: &MixerSplit,
    strip_available_px: f32,
    vsti_group_expanded: Option<bool>,
    // When true the GPU primitive layer owns the strip background, top accent
    // bar, and right separator — the strip container omits them so the batched
    // canvas behind shows through. Inner sections keep their native styling.
    gpu_decor: bool,
    i18n: I18n,
) -> impl IntoElement {
    log_vsti_child_meter_subscribe_once(track);
    log_vsti_child_strip_state(track);
    let id_num = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        track.id.hash(&mut hasher);
        hasher.finish() as usize
    };

    let strip_bg = if is_selected {
        Colors::surface_card_selected()
    } else if index % 2 == 0 {
        Colors::surface_panel()
    } else {
        Colors::surface_panel_alt()
    };
    let border_col = if is_selected {
        Colors::accent_primary()
    } else {
        Colors::border_default()
    };

    let select_id = track.id.clone();
    let select_cb = callbacks.on_select_track.clone();
    let on_select_strip =
        move |event: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| {
            if event.button == gpui::MouseButton::Left {
                let additive = event.modifiers.control || event.modifiers.platform;
                let range = event.modifiers.shift;
                select_cb(&select_id, additive, range, w, cx);
            }
        };
    let context_id = track.id.clone();
    let on_context = callbacks.on_context_menu.clone();
    let (insert_h, send_h) = clamp_mixer_section_heights_for_strip(
        split.insert_px,
        split.send_px,
        strip_available_px.max(STRIP_MIN_HEIGHT),
    );
    let vsti_group = track
        .instrument_insert()
        .filter(|slot| !slot.is_empty())
        .map(|slot| {
            let group_key = vsti_output_group_key(&track.id, &slot.id);
            let expanded = vsti_group_expanded.unwrap_or(true);
            let count = vsti_output_bus_strips(slot).len();
            (group_key, expanded, count)
        });

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(STRIP_WIDTH))
        .min_h(px(STRIP_MIN_HEIGHT))
        .h_full()
        .overflow_hidden()
        .when(!gpu_decor, |s| {
            s.bg(strip_bg)
                .border_r(px(1.0))
                .border_color(border_col)
                .hover(|h| h.bg(Colors::surface_hover()))
        })
        .id(("mix-strip", id_num))
        // Select during capture so occluding child controls (header, inserts,
        // pan and fader) cannot delay or swallow channel selection. The child
        // still receives the same event and performs its own action normally.
        .capture_any_mouse_down(on_select_strip)
        .when_some(on_context, |this, cb| {
            this.on_mouse_down(
                gpui::MouseButton::Right,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    let x: f32 = event.position.x.into();
                    let y: f32 = event.position.y.into();
                    cb(&(context_id.clone(), x, y), window, cx);
                },
            )
        })
        // Top accent line (GPU layer paints it when gpu_decor is on)
        .when(!gpu_decor, |s| {
            s.child(div().w_full().h(px(2.0)).bg(track.color))
        })
        .child(strip_header(
            track,
            index,
            vsti_group.as_ref().map(|(group_key, expanded, count)| {
                (group_key.as_str(), *expanded, *count, callbacks)
            }),
            i18n,
        ))
        .child(inserts_section(track, index, callbacks, insert_h, i18n))
        .child(vertical_split_handle(
            id_num,
            MixerSplitTarget::InsertSend,
            split,
        ))
        .child(sends_section(track, all_tracks, callbacks, send_h, i18n))
        .child(vertical_split_handle(
            id_num,
            MixerSplitTarget::SendFader,
            split,
        ))
        // ── Lower Control — pan / fader / meter / M·S·R·I. Takes the remaining
        // height; the fader area is the flex_1 child so it absorbs growth and
        // shrinks first when space is tight (pan + buttons stay fixed).
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(LOWER_CONTROL_MIN_H))
                .overflow_hidden()
                .w_full()
                .child(pan_section(track, callbacks, is_selected))
                .child(fader_area(track, callbacks, is_selected))
                .child(button_row(track, callbacks, id_num)),
        )
        .child(output_button(
            &track.id,
            output_routing_label(track, all_tracks),
            id_num,
            callbacks.on_open_output_picker.clone(),
        ))
        .child(strip_footer(&track.name))
}

#[allow(clippy::too_many_arguments)]
fn vsti_output_sub_strip(
    parent_track: &TrackState,
    child_track: &TrackState,
    all_tracks: &[TrackState],
    track_index: usize,
    insert_id: &str,
    bus_index: u8,
    bus_counts: &[u8],
    selected_track_id: Option<&str>,
    focus_highlight: bool,
    vsti_output_meters: &std::collections::HashMap<String, VstiOutputMeterState>,
    callbacks: &MixerCallbacks,
    split: &MixerSplit,
    strip_available_px: f32,
    gpu_decor: bool,
    i18n: I18n,
) -> impl IntoElement {
    let id_num = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        child_track.id.hash(&mut hasher);
        hasher.finish() as usize
    };
    let bus_label = vsti_output_bus_label(bus_counts, bus_index);
    // Render the backing per-bus child track so its mute/solo/fader/pan and VU
    // are the real, engine-routed per-output-bus state — never the parent's.
    let mut sub_track = child_track.clone();
    sub_track.name = bus_label.clone();
    sub_track.inserts.clear();
    sub_track.sends.clear();
    // Per-output-bus VU: the child track already carries engine pass-2 L/R
    // peaks. Fall back to the dedicated per-channel meter map for this bus's
    // flat channels when the child track meter has not been populated yet.
    if sub_track.meter_level_l <= 0.0 && sub_track.meter_level_r <= 0.0 {
        if let Some((channel_l, channel_r)) =
            vsti_output_child_channels_for_bus_layout(bus_counts, bus_index)
        {
            let meter_l = vsti_output_meters.get(&vsti_output_meter_key(
                &parent_track.id,
                insert_id,
                channel_l,
            ));
            let meter_r = vsti_output_meters.get(&vsti_output_meter_key(
                &parent_track.id,
                insert_id,
                channel_r,
            ));
            if let Some(meter) = meter_l {
                sub_track.meter_level_l = meter.level;
                sub_track.meter_peak_hold_l = meter.peak_hold;
                sub_track.meter_clip |= meter.clip;
            }
            if let Some(meter) = meter_r {
                sub_track.meter_level_r = meter.level;
                sub_track.meter_peak_hold_r = meter.peak_hold;
                sub_track.meter_clip |= meter.clip;
            }
        }
    }
    let is_selected = focus_highlight || selected_track_id == Some(child_track.id.as_str());
    let select_id = child_track.id.clone();
    let select_cb = callbacks.on_select_track.clone();
    let on_select_strip =
        move |event: &gpui::MouseDownEvent, w: &mut gpui::Window, cx: &mut gpui::App| {
            if event.button == gpui::MouseButton::Left {
                let additive = event.modifiers.control || event.modifiers.platform;
                let range = event.modifiers.shift;
                select_cb(&select_id, additive, range, w, cx);
            }
        };
    let context_id = child_track.id.clone();
    let on_context = callbacks.on_context_menu.clone();
    let (insert_h, send_h) = clamp_mixer_section_heights_for_strip(
        split.insert_px,
        split.send_px,
        strip_available_px.max(STRIP_MIN_HEIGHT),
    );

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(STRIP_WIDTH))
        .min_h(px(STRIP_MIN_HEIGHT))
        .h_full()
        .overflow_hidden()
        .when(!gpu_decor, |s| {
            // Multi-output sub-strips must read as grouped children of their
            // instrument parent, not independent channels: a sunken/nested
            // background plus left+right separators tinted in the parent track
            // colour bracket the group, distinct from the neutral separators
            // between top-level strips.
            s.bg(if is_selected {
                Colors::surface_card_selected()
            } else {
                Colors::surface_canvas()
            })
            .border_l(px(1.0))
            .border_r(px(1.0))
            .border_color(if is_selected {
                Colors::accent_primary()
            } else {
                Colors::with_alpha(parent_track.color, 0.45)
            })
            .hover(|h| h.bg(Colors::surface_hover()))
        })
        .id(("mix-vsti-sub-strip", id_num))
        .capture_any_mouse_down(on_select_strip)
        .when_some(on_context, |this, cb| {
            this.on_mouse_down(
                gpui::MouseButton::Right,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    let x: f32 = event.position.x.into();
                    let y: f32 = event.position.y.into();
                    cb(&(context_id.clone(), x, y), window, cx);
                },
            )
        })
        .when(!gpu_decor, |s| {
            s.child(div().w_full().h(px(2.0)).bg(parent_track.color))
        })
        // Real callbacks: mute / solo / volume / pan all target the child track
        // id (via button_row / pan_section / fader_area below), so S/M and the
        // fader operate per output bus.
        .child(strip_header(&sub_track, track_index, None, i18n))
        // Real per-bus insert rack: the backing child track is a genuine Bus
        // model track, so its FX chain is added/bypassed/reordered by child
        // track id and processed by the engine's pass-2 routing chain for
        // this output bus only (Add Insert opens the Effects picker).
        .child(inserts_section(
            child_track,
            track_index,
            callbacks,
            insert_h,
            i18n,
        ))
        .child(vertical_split_handle(
            id_num,
            MixerSplitTarget::InsertSend,
            split,
        ))
        .child(sends_section(
            child_track,
            all_tracks,
            callbacks,
            send_h,
            i18n,
        ))
        .child(vertical_split_handle(
            id_num,
            MixerSplitTarget::SendFader,
            split,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(LOWER_CONTROL_MIN_H))
                .overflow_hidden()
                .w_full()
                .child(pan_section(&sub_track, callbacks, is_selected))
                .child(fader_area(&sub_track, callbacks, is_selected))
                .child(button_row(&sub_track, callbacks, id_num)),
        )
        .child(strip_footer(&bus_label))
}

// ─── Master block ───────────────────────────────────────────────────────────

pub(crate) fn master_strip(
    accent: gpui::Rgba,
    master: &MasterBusState,
    on_master_vol_change: std::sync::Arc<dyn Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static>,
    callbacks: &MixerCallbacks,
    split: &MixerSplit,
    strip_available_px: f32,
    i18n: I18n,
) -> impl IntoElement {
    let db_str = volume::format_db(master.volume);
    let on_start_cb = callbacks.on_master_volume_drag_start.clone();
    let on_master_start = move |v: &f32, w: &mut gpui::Window, cx: &mut gpui::App| {
        on_start_cb(v, w, cx);
    };
    let on_preview_cb = callbacks.on_master_volume_drag_preview.clone();
    let on_master_preview = move |v: &f32, w: &mut gpui::Window, cx: &mut gpui::App| {
        on_preview_cb(v, w, cx);
    };
    let on_commit_cb = callbacks.on_master_volume_drag_commit.clone();
    let on_master_commit = move |w: &mut gpui::Window, cx: &mut gpui::App| {
        on_commit_cb(w, cx);
    };
    let on_reset_cb = on_master_vol_change;
    let on_master_reset = move |w: &mut gpui::Window, cx: &mut gpui::App| {
        on_reset_cb(&volume::db_to_norm(0.0), w, cx);
    };
    let (insert_h, _send_h) = clamp_mixer_section_heights_for_strip(
        split.insert_px,
        split.send_px,
        strip_available_px.max(STRIP_MIN_HEIGHT),
    );

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(STRIP_WIDTH))
        .min_h(px(STRIP_MIN_HEIGHT))
        .h_full()
        .overflow_hidden()
        .bg(Colors::surface_panel_alt())
        .border_l(px(1.0))
        .border_color(Colors::border_default())
        .child(div().w_full().h(px(2.0)).bg(accent))
        // Header
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .h(px(SEC_HEADER_H))
                .px(px(5.0))
                .border_b(px(1.0))
                .border_color(Colors::border_default())
                .child(div().w(px(2.0)).h(px(20.0)).rounded_full().bg(accent))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Colors::text_primary())
                                .child(i18n.tr("mixer.master.label")),
                        )
                        .child(
                            div()
                                .text_size(px(7.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(Colors::text_secondary())
                                .child(i18n.tr("mixer.master.bus-label")),
                        ),
                ),
        )
        .child(master_inserts_section(
            accent,
            master,
            callbacks,
            insert_h,
            i18n,
        ))
        // ── Lower Control — STEREO/OUT row, fader cluster, OUT button.
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(LOWER_CONTROL_MIN_H))
                .overflow_hidden()
                .w_full()
                // Master skips pan; show the level pill in this row instead so
                // the overall vertical rhythm matches a normal strip.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(4.0))
                        .h(px(SEC_PAN_H))
                        .border_b(px(1.0))
                        .border_color(Colors::border_default())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .min_w(px(46.0))
                                .px(px(6.0))
                                .h(px(14.0))
                                .rounded_sm()
                                .bg(Colors::button_bg())
                                .border(px(1.0))
                                .border_color(Colors::accent_primary())
                                .text_size(px(9.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Colors::text_secondary())
                                .child(i18n.tr("mixer.stereo")),
                        )
                        .child(
                            div()
                                .text_size(px(7.5))
                                .text_color(Colors::text_secondary())
                                .child(i18n.tr("mixer.output.1-2")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(SEC_FADER_MIN_H))
                        .items_center()
                        .w_full()
                        .px(px(4.0))
                        .pt(px(5.0))
                        .pb(px(6.0))
                        .gap(px(5.0))
                        .child(db_value_pill(db_str.clone(), true))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(2.0))
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .justify_center()
                                .child(db_scale_column())
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .h_full()
                                        .items_center()
                                        .justify_center()
                                        .child(fader_with_drag_callbacks(
                                            "mix-fader-master",
                                            master.volume,
                                            accent,
                                            Some(on_master_start),
                                            Some(on_master_preview),
                                            Some(on_master_commit),
                                            Some(on_master_reset),
                                        )),
                                )
                                .child(meter_surface(
                                    master.meter_level_l,
                                    master.meter_level_r,
                                    master.meter_peak_hold_l,
                                    master.meter_peak_hold_r,
                                    master.meter_clip,
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(SEC_BUTTONS_H))
                        .px(px(4.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .h(px(16.0))
                                .px(px(6.0))
                                .rounded_sm()
                                .bg(Colors::button_bg())
                                .border(px(1.0))
                                .border_color(Colors::border_default())
                                .text_size(px(8.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Colors::text_muted())
                                .child(i18n.tr("mixer.output.1-2")),
                        ),
                ),
        )
        .child(strip_footer(&i18n.tr("mixer.master.label")))
}

// ─── Public: Mixer Panel ─────────────────────────────────────────────────────

/// Strip columns above/below the visible viewport that are kept rendered to
/// prevent pop-in during horizontal mixer scrolling.
const MIXER_OVERSCAN: usize = 1;

pub(crate) fn mixer_visible_item_range(
    strip_count: usize,
    scroll_x: f32,
    viewport_width: f32,
) -> std::ops::Range<usize> {
    let max_scroll_x = (strip_count as f32 * STRIP_WIDTH - viewport_width.max(0.0)).max(0.0);
    let scroll_x = scroll_x.clamp(0.0, max_scroll_x);
    let first_visible = (scroll_x / STRIP_WIDTH).floor() as usize;
    let visible_start = first_visible.saturating_sub(MIXER_OVERSCAN);
    let last_visible = ((scroll_x + viewport_width.max(0.0)) / STRIP_WIDTH).ceil() as usize;
    let visible_end = (last_visible + MIXER_OVERSCAN).min(strip_count);
    visible_start.min(visible_end)..visible_end
}

#[derive(Clone, Debug, Default)]
pub struct VstiOutputMeterState {
    pub level: f32,
    pub peak_hold: f32,
    pub clip: bool,
}

pub fn vsti_output_meter_key(track_id: &str, insert_id: &str, channel: u8) -> String {
    format!("{track_id}:{insert_id}:{channel}")
}

#[derive(Clone, Copy)]
pub(crate) enum MixerRenderItem {
    Track {
        track_index: usize,
    },
    /// One mixer sub-strip per REAL plugin output bus. `child_index` points at
    /// the backing `vsti-out:{insert}:bus:{bus_index}` model track that owns the
    /// per-bus mute/solo/fader/meter state; `bus_index` is the plugin output bus.
    VstiOutput {
        parent_index: usize,
        insert_index: usize,
        bus_index: u8,
        child_index: usize,
    },
}

pub fn mixer_render_item_count(
    tracks: &[TrackState],
    collapsed_vsti_output_groups: &HashSet<String>,
    hidden_channels: &HashSet<String>,
) -> usize {
    collect_mixer_render_items(tracks, collapsed_vsti_output_groups, hidden_channels).len()
}

fn channel_id_for_render_item(tracks: &[TrackState], item: &MixerRenderItem) -> String {
    match item {
        MixerRenderItem::Track { track_index } => tracks[*track_index].id.clone(),
        MixerRenderItem::VstiOutput { child_index, .. } => tracks[*child_index].id.clone(),
    }
}

pub fn mixer_strip_index_for_channel(
    tracks: &[TrackState],
    collapsed_vsti_output_groups: &HashSet<String>,
    hidden_channels: &HashSet<String>,
    channel_id: &str,
) -> Option<usize> {
    let items = collect_mixer_render_items(tracks, collapsed_vsti_output_groups, hidden_channels);
    items
        .iter()
        .position(|item| channel_id_for_render_item(tracks, item) == channel_id)
}

pub fn mixer_scroll_x_for_strip_index(
    strip_index: usize,
    viewport_width: f32,
    strip_count: usize,
) -> f32 {
    let total_content_w = strip_count as f32 * STRIP_WIDTH;
    let max_scroll_x = (total_content_w - viewport_width).max(0.0);
    let strip_x = strip_index as f32 * STRIP_WIDTH;
    let target = strip_x - (viewport_width - STRIP_WIDTH) * 0.5;
    target.clamp(0.0, max_scroll_x.max(0.0))
}

pub(crate) fn collect_mixer_render_items(
    tracks: &[TrackState],
    collapsed_vsti_output_groups: &HashSet<String>,
    hidden_channels: &HashSet<String>,
) -> Vec<MixerRenderItem> {
    // Index VSTi output children once. The old parent-by-parent full track scan
    // was O(n²), which became visible as multi-second mixer stalls at 1k tracks.
    let mut children_by_insert: std::collections::HashMap<&str, Vec<(u8, usize)>> =
        std::collections::HashMap::new();
    for (child_index, child) in tracks.iter().enumerate() {
        let Some(insert_id) = vsti_output_child_insert_id(&child.id) else {
            continue;
        };
        let Some(bus_index) = child
            .id
            .rsplit_once(":bus:")
            .and_then(|(_, bus)| bus.parse::<u8>().ok())
        else {
            continue;
        };
        children_by_insert
            .entry(insert_id)
            .or_default()
            .push((bus_index, child_index));
    }
    for children in children_by_insert.values_mut() {
        children.sort_unstable_by_key(|(bus_index, _)| *bus_index);
    }

    let mut items = Vec::with_capacity(tracks.len());
    for (track_index, track) in tracks.iter().enumerate() {
        // VSTi multi-out child tracks are model/engine route nodes. The visible
        // mixer sub-strips are injected from the parent insert below, so these
        // backing tracks should never render as ordinary BUS channel strips.
        if vsti_output_child_insert_id(&track.id).is_some() {
            continue;
        }
        if hidden_channels.contains(&track.id) {
            continue;
        }
        items.push(MixerRenderItem::Track { track_index });

        let Some(slot) = track.instrument_insert().filter(|slot| !slot.is_empty()) else {
            continue;
        };
        let group_key = vsti_output_group_key(&track.id, &slot.id);
        if collapsed_vsti_output_groups.contains(&group_key) {
            continue;
        }
        // One sub-strip per backing child track (real output bus). Iterating the
        // model child tracks — rather than re-deriving channel pairs here — keeps
        // the visible strips, the engine routes, and per-bus mute/solo/meters in
        // perfect 1:1 lockstep with `ensure_vsti_output_child_tracks`.
        for &(bus_index, child_index) in children_by_insert
            .get(slot.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let child_id = &tracks[child_index].id;
            if hidden_channels.contains(child_id) {
                continue;
            }
            items.push(MixerRenderItem::VstiOutput {
                parent_index: track_index,
                insert_index: 0,
                bus_index,
                child_index,
            });
        }
    }
    items
}

/// Cached center background for the empty mixer bay. Grid geometry is rebuilt
/// only when width/height changes — never during paint from routing/session data.
#[derive(Clone, Copy, Debug, Default)]
struct MixerCenterGridCache {
    width_q: i64,
    height_q: i64,
    rebuild_count: u64,
}

thread_local! {
    static MIXER_CENTER_GRID: std::cell::RefCell<MixerCenterGridCache> =
        const { std::cell::RefCell::new(MixerCenterGridCache {
            width_q: 0,
            height_q: 0,
            rebuild_count: 0,
        }) };
}

/// Lightweight empty mixer center: solid background + optional batched vertical
/// grid lines. No strip layout nodes, no per-frame Vec allocation.
pub fn mixer_center_lightweight(width: f32, height: f32) -> impl IntoElement {
    use gpui::canvas;

    let width = width.max(0.0);
    let height = height.max(STRIP_MIN_HEIGHT);
    let width_q = (width * 4.0).round() as i64;
    let height_q = (height * 4.0).round() as i64;

    MIXER_CENTER_GRID.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.width_q != width_q || cache.height_q != height_q {
            cache.width_q = width_q;
            cache.height_q = height_q;
            cache.rebuild_count = cache.rebuild_count.saturating_add(1);
            crate::perf::count("mixer_grid_rebuild_count", cache.rebuild_count);
        }
    });

    div()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .relative()
        .bg(Colors::surface_window())
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    let _ = bounds;
                },
                move |bounds, (), window, _cx| {
                    let _scope = crate::perf::PerfScope::enter("MixerCenter");
                    crate::perf::count("mixer_center_paint_count", 1);
                    paint_mixer_center_grid(bounds, width, window);
                },
            )
            .absolute()
            .inset_0(),
        )
}

fn paint_mixer_center_grid(
    bounds: gpui::Bounds<gpui::Pixels>,
    width: f32,
    window: &mut gpui::Window,
) {
    use gpui::{fill, point, px, size, Bounds};

    let origin_x = f32::from(bounds.origin.x);
    let origin_y = f32::from(bounds.origin.y);
    let h = f32::from(bounds.size.height).max(0.0);
    if h <= 0.0 || width <= 0.0 {
        return;
    }
    let stripe_count = (width / STRIP_WIDTH).ceil().clamp(0.0, 64.0) as usize;
    let color = Colors::border_subtle();
    for i in 0..stripe_count {
        let x = i as f32 * STRIP_WIDTH;
        if x > width {
            break;
        }
        let rect = Bounds::new(point(px(origin_x + x), px(origin_y)), size(px(1.0), px(h)));
        window.paint_quad(fill(rect, color));
    }
}

fn mixer_empty_bay(spare_w: f32, height: f32) -> impl IntoElement {
    mixer_center_lightweight(spare_w, height)
}

/// Build the draw-only mixer primitive snapshot for the GPU layer. Mirrors the
/// virtualized strip order in [`mixer_strip_scroller`] (strip *render position* ×
/// `STRIP_WIDTH`), and reproduces each strip's background / accent / separator so
/// the batched canvas paints exactly what the legacy `div` strips would. Reads
/// only cloned UI state — never the audio engine, project, or routing.
pub(crate) fn build_mixer_render_snapshot(
    tracks: &[TrackState],
    collapsed_vsti_output_groups: &HashSet<String>,
    hidden_mixer_channels: &HashSet<String>,
    selected_track_id: Option<&str>,
    selected_track_ids: &[String],
    scroll_x: f32,
    channel_area_width: f32,
    strip_height: f32,
) -> MixerRenderSnapshot {
    let _s = crate::perf::PerfScope::enter("MixerSnapshotBuild");
    let render_items =
        collect_mixer_render_items(tracks, collapsed_vsti_output_groups, hidden_mixer_channels);
    let mut strips = Vec::with_capacity(render_items.len());
    for (pos, item) in render_items.iter().enumerate() {
        let x = pos as f32 * STRIP_WIDTH;
        let geom = match *item {
            MixerRenderItem::Track { track_index } => {
                let track = &tracks[track_index];
                let selected =
                    mixer_strip_is_selected(&track.id, selected_track_id, selected_track_ids);
                let bg = if selected {
                    Colors::surface_card_selected()
                } else if track_index % 2 == 0 {
                    Colors::surface_panel()
                } else {
                    Colors::surface_panel_alt()
                };
                let separator = if selected {
                    Colors::accent_primary()
                } else {
                    Colors::border_default()
                };
                MixerStripGeom {
                    x,
                    width: STRIP_WIDTH,
                    height: strip_height,
                    bg,
                    accent: track.color,
                    separator,
                    selected,
                    is_master: false,
                    meter_l: track.meter_level_l,
                    meter_r: track.meter_level_r,
                    hovered: false,
                }
            }
            MixerRenderItem::VstiOutput {
                parent_index,
                child_index,
                ..
            } => {
                let parent = &tracks[parent_index];
                let child = &tracks[child_index];
                let selected =
                    mixer_strip_is_selected(&child.id, selected_track_id, selected_track_ids);
                MixerStripGeom {
                    x,
                    width: STRIP_WIDTH,
                    height: strip_height,
                    bg: if selected {
                        Colors::surface_card_selected()
                    } else {
                        Colors::surface_panel_alt()
                    },
                    accent: parent.color,
                    separator: if selected {
                        Colors::accent_primary()
                    } else {
                        Colors::border_default()
                    },
                    selected,
                    is_master: false,
                    meter_l: child.meter_level_l,
                    meter_r: child.meter_level_r,
                    hovered: false,
                }
            }
        };
        strips.push(geom);
    }
    let viewport = MixerRenderViewport {
        channel_area_width,
        height: strip_height,
        scroll_x,
        master_x: None,
    };
    // accent bar = 2px (matches the strip top accent line); separator = 1px.
    MixerRenderSnapshot::new(viewport, strips, None, 2.0, 1.0)
}

pub fn mixer_panel(
    tracks: &[TrackState],
    master: &MasterBusState,
    selected_track_id: Option<&str>,
    selected_track_ids: &[String],
    callbacks: MixerCallbacks,
    collapsed_vsti_output_groups: &HashSet<String>,
    hidden_mixer_channels: &HashSet<String>,
    vsti_output_meters: &std::collections::HashMap<String, VstiOutputMeterState>,
    scroll_x: f32,
    viewport_width: f32,
    viewport_height: f32,
    on_scroll: std::sync::Arc<dyn Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static>,
    split: MixerSplit,
    tree_sidebar: Option<Entity<MixerTreeSidebar>>,
    tree_sidebar_enabled: bool,
    i18n: I18n,
) -> impl IntoElement {
    let _shell = crate::perf::PerfScope::enter("MixerShell");
    crate::perf::count("mixer_shell_layout_count", 1);

    let track_count = tracks.len();
    let accent = Colors::accent_primary();
    let on_master = callbacks.on_master_volume_change.clone();
    let strip_available_px = (viewport_height - 30.0).max(STRIP_MIN_HEIGHT);

    // Optional GPU primitive layer: when active, the channel strips drop their
    // background / accent bar / separator and a single batched `canvas` paints
    // them behind the strip row. The master stays a native pinned strip (its
    // exact pinned x is not known until layout). Opt-in / reversible.
    let gpu_active = mixer_gpu_primitives_active();

    let strip_count =
        mixer_render_item_count(tracks, collapsed_vsti_output_groups, hidden_mixer_channels);

    // Empty mixer fast path — shell + tree (external) + cheap center + isolated master.
    if strip_count == 0 {
        crate::perf::count("mixer_shell_layout_count", 1);
        let channel_row = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .child(mixer_center_lightweight(viewport_width, strip_available_px))
            .child(div().w(px(1.0)).h_full().bg(Colors::border_default()))
            .child(mixer_master_strip_pinned(
                accent,
                master,
                on_master,
                &callbacks,
                &split,
                strip_available_px,
                i18n,
            ));

        let content_row = if tree_sidebar_enabled {
            let mut row = div().flex().flex_row().flex_1().min_h_0();
            if let Some(sidebar) = tree_sidebar {
                row = row.child(sidebar);
            }
            row.child(channel_row)
        } else {
            channel_row
        };

        let split_for_move = split.clone();
        let split_for_end = split.clone();
        return div()
            .flex()
            .flex_col()
            .size_full()
            .bg(Colors::surface_window())
            .on_drag_move::<MixerSplitDrag>(move |event: &DragMoveEvent<MixerSplitDrag>, w, cx| {
                let y: f32 = event.event.position.y.into();
                (split_for_move.on_action)(MixerSplitAction::ResizeMove(y), w, cx);
            })
            .on_mouse_up(gpui::MouseButton::Left, move |_e, w, cx| {
                (split_for_end.on_action)(MixerSplitAction::ResizeEnd, w, cx);
            })
            .child(mixer_sub_header(track_count, i18n))
            .child(content_row);
    }

    let strip_row = mixer_strip_scroller(
        tracks,
        selected_track_id,
        selected_track_ids,
        callbacks.clone(),
        collapsed_vsti_output_groups,
        hidden_mixer_channels,
        vsti_output_meters,
        scroll_x,
        viewport_width,
        strip_available_px,
        &split,
        on_scroll,
        gpu_active,
        i18n,
    );

    let master_block = mixer_master_strip_pinned(
        accent,
        master,
        on_master,
        &callbacks,
        &split,
        strip_available_px,
        i18n,
    );

    let mut channel_row = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .child(strip_row)
        .child(div().w(px(1.0)).h_full().bg(Colors::border_default()))
        .child(master_block);

    if gpu_active {
        let snapshot = build_mixer_render_snapshot(
            tracks,
            collapsed_vsti_output_groups,
            hidden_mixer_channels,
            selected_track_id,
            selected_track_ids,
            scroll_x,
            viewport_width,
            strip_available_px,
        );
        let primitives = render_mixer_primitives(&snapshot);
        // Wrap so the batched canvas paints behind the strip/gutter/master row.
        channel_row = div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(primitives)
            .child(channel_row.size_full());
    }

    let content_row = if tree_sidebar_enabled {
        let mut row = div().flex().flex_row().flex_1().min_h_0();
        if let Some(sidebar) = tree_sidebar {
            row = row.child(sidebar);
        }
        row.child(channel_row)
    } else {
        channel_row
    };

    let split_for_move = split.clone();
    let split_for_end = split.clone();

    div()
        .flex()
        .flex_col()
        .size_full()
        .bg(Colors::surface_window())
        .on_drag_move::<MixerSplitDrag>(move |event: &DragMoveEvent<MixerSplitDrag>, w, cx| {
            let y: f32 = event.event.position.y.into();
            (split_for_move.on_action)(MixerSplitAction::ResizeMove(y), w, cx);
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_e, w, cx| {
            (split_for_end.on_action)(MixerSplitAction::ResizeEnd, w, cx);
        })
        .child(mixer_sub_header(track_count, i18n))
        .child(content_row)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn mixer_strip_scroller(
    tracks: &[TrackState],
    selected_track_id: Option<&str>,
    selected_track_ids: &[String],
    callbacks: MixerCallbacks,
    collapsed_vsti_output_groups: &HashSet<String>,
    hidden_mixer_channels: &HashSet<String>,
    vsti_output_meters: &std::collections::HashMap<String, VstiOutputMeterState>,
    scroll_x: f32,
    viewport_width: f32,
    strip_available_px: f32,
    split: &MixerSplit,
    on_scroll: std::sync::Arc<dyn Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static>,
    gpu_decor: bool,
    i18n: I18n,
) -> impl IntoElement {
    let _scope = crate::perf::PerfScope::enter("MixerStripScroller");
    crate::perf::count("mixer_strip_layout_count", 1);
    crate::perf::count("mixer_strip_paint_count", 1);

    let render_items =
        collect_mixer_render_items(tracks, collapsed_vsti_output_groups, hidden_mixer_channels);
    let strip_count = render_items.len();
    crate::perf::count("mixer_strips", tracks.len() as u64);

    let total_content_w = strip_count as f32 * STRIP_WIDTH;
    let max_scroll_x = (total_content_w - viewport_width).max(0.0);
    let scroll_x = scroll_x.clamp(0.0, max_scroll_x.max(0.0));
    let spare_channel_w = (viewport_width - total_content_w).max(0.0);

    let visible_range = mixer_visible_item_range(strip_count, scroll_x, viewport_width);
    let visible_start = visible_range.start;
    let visible_end = visible_range.end;

    let left_spacer_w = visible_start as f32 * STRIP_WIDTH;
    let right_spacer_w = strip_count.saturating_sub(visible_end) as f32 * STRIP_WIDTH;

    crate::perf::count(
        "visible_mixer_strips",
        visible_end.saturating_sub(visible_start) as u64,
    );

    let visible_strips: Vec<gpui::AnyElement> = render_items[visible_start..visible_end]
        .iter()
        .map(|item| match *item {
            MixerRenderItem::Track { track_index } => {
                let track = &tracks[track_index];
                let is_sel =
                    mixer_strip_is_selected(&track.id, selected_track_id, selected_track_ids);
                let vsti_group_expanded = track
                    .instrument_insert()
                    .filter(|slot| !slot.is_empty())
                    .map(|slot| {
                        !collapsed_vsti_output_groups
                            .contains(&vsti_output_group_key(&track.id, &slot.id))
                    });
                channel_strip(
                    track,
                    tracks,
                    track_index,
                    is_sel,
                    &callbacks,
                    split,
                    strip_available_px,
                    vsti_group_expanded,
                    gpu_decor,
                    i18n,
                )
                .into_any_element()
            }
            MixerRenderItem::VstiOutput {
                parent_index,
                insert_index,
                bus_index,
                child_index,
            } => {
                let parent = &tracks[parent_index];
                let bus_counts = parent.inserts[insert_index]
                    .output_bus_channel_counts
                    .clone();
                let child_selected = mixer_strip_is_selected(
                    &tracks[child_index].id,
                    selected_track_id,
                    selected_track_ids,
                );
                vsti_output_sub_strip(
                    parent,
                    &tracks[child_index],
                    tracks,
                    parent_index,
                    &parent.inserts[insert_index].id,
                    bus_index,
                    &bus_counts,
                    selected_track_id,
                    child_selected,
                    vsti_output_meters,
                    &callbacks,
                    split,
                    strip_available_px,
                    gpu_decor,
                    i18n,
                )
                .into_any_element()
            }
        })
        .collect();

    let on_scroll_wheel = {
        let on_scroll = on_scroll.clone();
        move |event: &gpui::ScrollWheelEvent, window: &mut gpui::Window, cx: &mut gpui::App| {
            let (dx, dy) = match &event.delta {
                gpui::ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                gpui::ScrollDelta::Lines(l) => (l.x * STRIP_WIDTH, l.y * STRIP_WIDTH * 0.5),
            };
            let delta = if event.modifiers.shift {
                if dx.abs() > f32::EPSILON {
                    dx
                } else {
                    dy
                }
            } else if dx.abs() >= dy.abs() {
                dx
            } else {
                dy
            };
            if delta.abs() <= f32::EPSILON || max_scroll_x <= 0.0 {
                return;
            }
            let new_x = (scroll_x + delta).clamp(0.0, max_scroll_x.max(0.0));
            on_scroll(new_x, window, cx);
            window.prevent_default();
            cx.stop_propagation();
        }
    };

    div()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .relative()
        .overflow_hidden()
        .on_scroll_wheel(on_scroll_wheel)
        .when(spare_channel_w > 0.0, |d| {
            // Keep the empty bay confined to the unused right-hand region.
            // A normal flex child expands across the entire scroller and, in
            // GPU-decoration mode, paints over every strip when the window is
            // wide enough to show all channels.
            d.child(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .bottom_0()
                    .w(px(spare_channel_w))
                    .overflow_hidden()
                    .child(mixer_empty_bay(spare_channel_w, strip_available_px)),
            )
        })
        .child(
            div()
                .absolute()
                .left(px(-scroll_x))
                .top_0()
                .bottom_0()
                .flex()
                .flex_row()
                .h_full()
                .min_h(px(STRIP_MIN_HEIGHT))
                .when(left_spacer_w > 0.0, |d| {
                    d.child(
                        div()
                            .w(px(left_spacer_w))
                            .h_full()
                            .flex_none()
                            .bg(Colors::surface_window()),
                    )
                })
                .children(visible_strips)
                .when(right_spacer_w > 0.0, |d| {
                    d.child(
                        div()
                            .w(px(right_spacer_w))
                            .h_full()
                            .flex_none()
                            .bg(Colors::surface_window()),
                    )
                }),
        )
}

fn mixer_master_strip_pinned(
    accent: gpui::Rgba,
    master: &MasterBusState,
    on_master: std::sync::Arc<dyn Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static>,
    callbacks: &MixerCallbacks,
    split: &MixerSplit,
    strip_available_px: f32,
    i18n: I18n,
) -> impl IntoElement {
    let _scope = crate::perf::PerfScope::enter("MixerMasterStrip");
    master_strip(
        accent,
        master,
        on_master,
        callbacks,
        split,
        strip_available_px,
        i18n,
    )
}

#[cfg(test)]
mod vsti_output_label_tests {
    use super::vsti_output_bus_label;

    #[test]
    fn mixed_mono_and_stereo_buses_get_real_layout_labels() {
        // bus0 mono (ch1), bus1 mono (ch2), bus2 stereo (ch3/4).
        let counts = [1u8, 1, 2];
        assert_eq!(vsti_output_bus_label(&counts, 0), "Out 1 (Mono / Ch 1)");
        assert_eq!(vsti_output_bus_label(&counts, 1), "Out 2 (Mono / Ch 2)");
        assert_eq!(vsti_output_bus_label(&counts, 2), "Out 3 (Stereo / Ch 3/4)");
    }

    #[test]
    fn multichannel_bus_label_shows_full_channel_range() {
        // bus0 stereo (ch1/2), bus1 quad (ch3-6).
        let counts = [2u8, 4];
        assert_eq!(vsti_output_bus_label(&counts, 1), "Out 2 (Multi / Ch 3-6)");
    }

    #[test]
    fn single_multichannel_bus_labels_each_flat_pair() {
        assert_eq!(vsti_output_bus_label(&[8u8], 0), "Out 1 (Stereo / Ch 1/2)");
        assert_eq!(vsti_output_bus_label(&[8u8], 1), "Out 2 (Stereo / Ch 3/4)");
    }
}

#[cfg(test)]
mod collapse_filter_tests {
    use super::{collect_mixer_render_items, vsti_output_group_key, MixerRenderItem};
    use crate::components::timeline::timeline_state::{
        CreateTrackOptions, InputMonitorMode, InsertPluginFormat, TimelineState, TrackType,
    };
    use std::collections::HashSet;

    fn drum_scenario(output_bus_layout: &[u32]) -> (TimelineState, String, String) {
        let mut state = TimelineState::default();
        let track_id = state.create_track(CreateTrackOptions {
            name: "Drums".to_string(),
            track_type: TrackType::Instrument,
            color: crate::theme::Colors::track_color_for_index(0),
            volume: 1.0,
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        let slot = state.ensure_insert_slot_at(&track_id, 0).expect("slot");
        state.set_insert_plugin(
            &track_id,
            &slot,
            "drums".to_string(),
            Some(std::path::PathBuf::from("C:/p/drums.vst3")),
            InsertPluginFormat::Vst3,
            None,
            "drums".to_string(),
        );
        state.set_insert_output_bus_layout(&track_id, &slot, output_bus_layout);
        let output_channels = output_bus_layout.iter().copied().sum::<u32>().max(2);
        state.auto_enable_detected_insert_outputs(&track_id, &slot, output_channels);
        (state, track_id, slot)
    }

    #[test]
    fn expanded_includes_children_collapsed_hides_only_children() {
        let (state, track_id, slot) = drum_scenario(&[2, 2, 2]);

        // Expanded (empty collapsed set): parent + one sub-strip per REAL output
        // bus (bus 0/1/2), never split per channel into 4 strips.
        let expanded = collect_mixer_render_items(&state.tracks, &HashSet::new(), &HashSet::new());
        assert_eq!(expanded.len(), 4);
        assert_eq!(
            expanded
                .iter()
                .filter(|item| matches!(item, MixerRenderItem::VstiOutput { .. }))
                .count(),
            3
        );

        // Collapsed: child strips filtered out, only the parent strip remains.
        let mut collapsed = HashSet::new();
        collapsed.insert(vsti_output_group_key(&track_id, &slot));
        let collapsed_items =
            collect_mixer_render_items(&state.tracks, &collapsed, &HashSet::new());
        assert_eq!(
            collapsed_items.len(),
            1,
            "collapse hides only the child strips, never the parent"
        );

        // The model still holds every track — collapse changed the VIEW only.
        assert_eq!(state.tracks.len(), 4);
    }

    #[test]
    fn mixed_mono_and_stereo_buses_show_one_strip_per_bus() {
        // Acceptance: outputs 1: mono, 2: mono, 3/4: stereo → THREE output
        // strips (one per bus), never two blindly-paired stereo strips.
        let (state, _track_id, _slot) = drum_scenario(&[1, 1, 2]);
        let expanded = collect_mixer_render_items(&state.tracks, &HashSet::new(), &HashSet::new());
        assert_eq!(
            expanded
                .iter()
                .filter(|item| matches!(item, MixerRenderItem::VstiOutput { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn expanded_single_multichannel_bus_includes_flat_pair_children() {
        let (state, track_id, slot) = drum_scenario(&[8]);

        // Expanded: one parent strip + one sub-strip per flat stereo pair of the
        // single 8-channel bus (Ch 1/2, 3/4, 5/6, 7/8).
        let expanded = collect_mixer_render_items(&state.tracks, &HashSet::new(), &HashSet::new());
        assert_eq!(expanded.len(), 5);
        assert_eq!(
            expanded
                .iter()
                .filter(|item| matches!(item, MixerRenderItem::VstiOutput { .. }))
                .count(),
            4
        );

        // Collapsed: only the view list hides the children; the state still keeps
        // every child track for routing and meters.
        let mut collapsed = HashSet::new();
        collapsed.insert(vsti_output_group_key(&track_id, &slot));
        let collapsed_items =
            collect_mixer_render_items(&state.tracks, &collapsed, &HashSet::new());
        assert_eq!(collapsed_items.len(), 1);
        assert_eq!(state.tracks.len(), 5);
    }
}

#[cfg(test)]
mod mixer_virtualization_tests {
    use super::mixer_visible_item_range;

    #[test]
    fn thousand_strip_detail_window_stays_bounded() {
        let first = mixer_visible_item_range(1_000, 0.0, 880.0);
        let middle = mixer_visible_item_range(1_000, 44_000.0, 880.0);
        let end = mixer_visible_item_range(1_000, f32::MAX, 880.0);
        assert!(first.len() <= 12);
        assert!(middle.len() <= 12);
        assert!(end.len() <= 12);
        assert_eq!(end.end, 1_000);
    }
}
