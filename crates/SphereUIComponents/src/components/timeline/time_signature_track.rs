use crate::components::timeline::global_lane_header::{
    global_lane_header, global_lane_resize_handle, GlobalLaneHeaderActions, GlobalLaneResizeArmCb,
    GlobalLaneResizeResetCb,
};
use crate::components::timeline::marker_flag::{marker_flag_layer, MarkerFlag};
use crate::components::timeline::timeline_grid::timeline_grid;
use crate::components::timeline::timeline_state::{GlobalLaneKind, TimelineState};
use crate::theme::Colors;
use gpui::prelude::FluentBuilder;
use gpui::{div, px, InteractiveElement, IntoElement, ParentElement, Styled};

pub type TimeSignatureTrackDownCallback = std::sync::Arc<
    dyn Fn(&(f64, Option<String>, bool, u32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

pub type TimeSignatureTrackContextCallback = std::sync::Arc<
    dyn Fn(&(f64, Option<String>, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

pub type GlobalLaneVoidCallback =
    std::sync::Arc<dyn Fn(&(), &mut gpui::Window, &mut gpui::App) + 'static>;
pub type GlobalLaneMenuCallback =
    std::sync::Arc<dyn Fn(&(f32, f32), &mut gpui::Window, &mut gpui::App) + 'static>;

/// Global Time Signature lane — compact marker blocks over the project map.
pub fn time_signature_track_lane(
    state: &TimelineState,
    lane_height: f32,
    on_down: Option<TimeSignatureTrackDownCallback>,
    on_context: Option<TimeSignatureTrackContextCallback>,
    on_add: Option<GlobalLaneVoidCallback>,
    on_header_menu: Option<GlobalLaneMenuCallback>,
    on_hide: Option<GlobalLaneVoidCallback>,
    on_toggle_collapsed: Option<GlobalLaneVoidCallback>,
    on_resize_arm: Option<GlobalLaneResizeArmCb>,
    on_resize_reset: Option<GlobalLaneResizeResetCb>,
) -> impl IntoElement {
    let lane_w = state.viewport.viewport_width.max(1.0);
    let points = state.time_signature_map.points.clone();
    let selected = state.selected_time_signature_point_id.clone();

    // Anchored flags, not centred pills: a signature change takes effect *at*
    // its beat, and the old centred pill put half the chip in the bar before it.
    let flags: Vec<MarkerFlag> = points
        .iter()
        .filter_map(|p| {
            let x = state.beats_to_x(p.beat as f32);
            if x < -64.0 || x > lane_w + 64.0 {
                return None;
            }
            Some(MarkerFlag {
                x,
                label: p.label(),
                selected: selected.as_deref() == Some(p.id.as_str()),
            })
        })
        .collect();
    // `time_signature_at_beat` already resolves an implicit 4/4 for an empty
    // map; surface that rather than leaving the lane blank.
    let mut flags = flags;
    if points.is_empty() {
        let implicit = state.time_signature_map.time_signature_at_beat(0.0);
        flags.push(MarkerFlag {
            x: state.beats_to_x(0.0),
            label: implicit.label(),
            selected: false,
        });
    }
    let (flag_layer, flag_labels) = marker_flag_layer(flags, lane_w, lane_height);

    let subtitle = state.time_signature_lane_header_subtitle();

    let interaction = on_down.map(|cb| {
        let state_hit = state.clone();
        div()
            .absolute()
            .inset_0()
            .id("time-signature-track-hit")
            .on_mouse_down(
                gpui::MouseButton::Left,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let wx: f32 = event.position.x.into();
                    let lane_x = state_hit.lane_x_from_window_x(wx);
                    let beat = state_hit.x_to_beat(lane_x).max(0.0);
                    let snapped = state_hit.snap_beats(beat as f32) as f64;
                    let ppb = state_hit.viewport.pixels_per_beat.max(1.0) as f64;
                    let beat_tol = 12.0 / ppb;
                    let point_id = state_hit.time_signature_point_at(snapped, beat_tol);
                    cb(
                        &(snapped, point_id, false, event.click_count as u32),
                        window,
                        cx,
                    );
                },
            )
            .when_some(on_context, |layer, ctx_cb| {
                let state_ctx = state.clone();
                layer.on_mouse_down(
                    gpui::MouseButton::Right,
                    move |event: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        let wx: f32 = event.position.x.into();
                        let sx: f32 = event.position.x.into();
                        let sy: f32 = event.position.y.into();
                        let lane_x = state_ctx.lane_x_from_window_x(wx);
                        let beat = state_ctx.x_to_beat(lane_x).max(0.0);
                        let ppb = state_ctx.viewport.pixels_per_beat.max(1.0) as f64;
                        let point_id = state_ctx.time_signature_point_at(beat, 12.0 / ppb);
                        ctx_cb(&(beat, point_id, sx, sy), window, cx);
                    },
                )
            })
    });

    let header = global_lane_header(
        "time-signature",
        "Time Signature",
        subtitle,
        state.time_signature_track_collapsed,
        "Hide Time Signature Track",
        GlobalLaneHeaderActions {
            on_add,
            on_menu: on_header_menu,
            on_hide,
            on_toggle_collapsed,
        },
    );

    let resize_handle = on_resize_arm
        .zip(on_resize_reset)
        .map(|(arm, reset)| global_lane_resize_handle(GlobalLaneKind::TimeSignature, arm, reset));

    div()
        .flex()
        .flex_row()
        .relative()
        .h(px(lane_height))
        .w_full()
        .bg(Colors::surface_panel_alt())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .child(header)
        .child(
            div()
                .flex_1()
                .h_full()
                .relative()
                .overflow_hidden()
                .bg(Colors::timeline_content_background())
                .child(timeline_grid(state, lane_w, lane_height))
                .child(flag_layer)
                .children(flag_labels)
                .children(interaction)
                // Debug: outline time_signature_lane_content_rect (FUTUREBOARD_UI_DEBUG_CLIPS=1).
                .children(crate::perf::debug_clip_outline()),
        )
        .children(resize_handle)
}
