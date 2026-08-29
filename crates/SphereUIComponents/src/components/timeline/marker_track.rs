//! Global Marker lane — the arrangement's cue points, given a row of their own.
//!
//! Markers used to live only as 9 px chips crammed into the ruler, sharing every
//! pixel with the playhead scrub: there was nowhere to read a long name, no grab
//! target that was not also a seek, and no place to hang per-marker commands. A
//! marker is a *named point in the arrangement*, so it gets the same treatment
//! as the other conductor data — its own lane, its own header, its own hit area.
//!
//! Geometry is shared with the Tempo and Time Signature lanes: the same
//! `marker_flag_layer` shape (left edge on the beat, tapering right) so a
//! marker, a tempo change, and a meter change all read as "starts here".

use crate::components::timeline::global_lane_header::{
    global_lane_header, global_lane_resize_handle, GlobalLaneHeaderActions, GlobalLaneResizeArmCb,
    GlobalLaneResizeResetCb,
};
use crate::components::timeline::marker_flag::{marker_flag_layer, MarkerFlag};
use crate::components::timeline::timeline_grid::timeline_grid;
use crate::components::timeline::timeline_state::{
    GlobalLaneKind, TimelineMarkerDrag, TimelineState,
};
use crate::theme::Colors;
use gpui::{
    div, px, AppContext, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};

/// Marker lane mouse-down: `(beat, marker_id, click_count)`.
///
/// `marker_id` is `None` when the press landed on empty lane, which is what
/// separates "select this marker" from "seek / create here".
pub type MarkerTrackDownCallback = std::sync::Arc<
    dyn Fn(&(f64, Option<String>, u32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

/// Marker lane right-click: `(beat, marker_id, screen_x, screen_y)`.
pub type MarkerTrackContextCallback = std::sync::Arc<
    dyn Fn(&(f64, Option<String>, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

pub type GlobalLaneVoidCallback =
    std::sync::Arc<dyn Fn(&(), &mut gpui::Window, &mut gpui::App) + 'static>;
pub type GlobalLaneMenuCallback =
    std::sync::Arc<dyn Fn(&(f32, f32), &mut gpui::Window, &mut gpui::App) + 'static>;

/// Pointer slop for hitting a marker flag, in pixels. Matched to the flag body
/// so the whole visible chip is grabbable, not just its stem.
const MARKER_HIT_SLOP_PX: f32 = 10.0;

/// Global Marker lane — named cue points over the arrangement timeline.
#[allow(clippy::too_many_arguments)]
pub fn marker_track_lane(
    state: &TimelineState,
    lane_height: f32,
    on_down: Option<MarkerTrackDownCallback>,
    on_context: Option<MarkerTrackContextCallback>,
    on_add: Option<GlobalLaneVoidCallback>,
    on_header_menu: Option<GlobalLaneMenuCallback>,
    on_hide: Option<GlobalLaneVoidCallback>,
    on_toggle_collapsed: Option<GlobalLaneVoidCallback>,
    on_resize_arm: Option<GlobalLaneResizeArmCb>,
    on_resize_reset: Option<GlobalLaneResizeResetCb>,
) -> impl IntoElement {
    let lane_w = state.viewport.viewport_width.max(1.0);
    let selected = state.selected_marker_id.clone();

    let flags: Vec<MarkerFlag> = state
        .markers
        .iter()
        .filter_map(|marker| {
            let x = state.beats_to_x(marker.beat as f32);
            // Overscan by a flag's worth so a marker scrolling in from either
            // edge does not pop.
            if x < -160.0 || x > lane_w + 160.0 {
                return None;
            }
            Some(MarkerFlag {
                x,
                label: marker.name.clone(),
                selected: selected.as_deref() == Some(marker.id.as_str()),
            })
        })
        .collect();
    let (flag_layer, flag_labels) = marker_flag_layer(flags, lane_w, lane_height);

    // Each marker also gets an invisible drag handle over its flag. The flag
    // layer itself is one canvas for the whole lane (cheap to paint, but not
    // hit-testable per marker), so identity lives in these thin overlays.
    let drag_handles: Vec<gpui::Stateful<gpui::Div>> = state
        .markers
        .iter()
        .enumerate()
        .filter_map(|(index, marker)| {
            let x = state.beats_to_x(marker.beat as f32);
            if x < -160.0 || x > lane_w + 160.0 {
                return None;
            }
            let label_w = crate::theme::menu::estimate_label_width(&marker.name);
            let width = (label_w + 18.0).clamp(26.0, 160.0);
            let drag = TimelineMarkerDrag {
                marker_id: marker.id.clone(),
                pointer_offset_x: 0.0,
            };
            Some(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(0.0))
                    .bottom_0()
                    .w(px(width))
                    .id(("marker-lane-flag", index))
                    .cursor(gpui::CursorStyle::PointingHand)
                    .on_drag(drag, move |drag, offset, _window, cx| {
                        cx.new(|_| TimelineMarkerDrag {
                            pointer_offset_x: offset.x.into(),
                            ..drag.clone()
                        })
                    }),
            )
        })
        .collect();

    // One hit layer under the flags resolves both cases: it maps the pointer to
    // a beat, then asks the state whether a marker is close enough. Keeping the
    // hit test in one place is why a click on a flag and a click 3 px beside it
    // cannot disagree about which marker was meant.
    let interaction = on_down.map(|cb| {
        let state_hit = state.clone();
        let mut layer = div()
            .absolute()
            .inset_0()
            .id("marker-track-hit")
            .on_mouse_down(
                gpui::MouseButton::Left,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let wx: f32 = event.position.x.into();
                    let lane_x = state_hit.lane_x_from_window_x(wx);
                    let beat = state_hit.x_to_beat(lane_x).max(0.0);
                    let marker_id = state_hit.marker_at(beat, marker_hit_tolerance(&state_hit));
                    // Creating and moving both snap; only the hit test reads the
                    // raw beat, so a marker just left of a grid line is still
                    // grabbable.
                    let snapped = state_hit.snap_beats(beat as f32).max(0.0) as f64;
                    cb(&(snapped, marker_id, event.click_count as u32), window, cx);
                },
            );
        if let Some(ctx_cb) = on_context {
            let state_ctx = state.clone();
            layer = layer.on_mouse_down(
                gpui::MouseButton::Right,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let wx: f32 = event.position.x.into();
                    let sx: f32 = event.position.x.into();
                    let sy: f32 = event.position.y.into();
                    let lane_x = state_ctx.lane_x_from_window_x(wx);
                    let beat = state_ctx.x_to_beat(lane_x).max(0.0);
                    let marker_id = state_ctx.marker_at(beat, marker_hit_tolerance(&state_ctx));
                    ctx_cb(&(beat, marker_id, sx, sy), window, cx);
                },
            );
        }
        layer
    });

    let header = global_lane_header(
        "marker",
        "Markers",
        state.marker_lane_header_subtitle(),
        state.marker_track_collapsed,
        "Hide Marker Track",
        GlobalLaneHeaderActions {
            on_add,
            on_menu: on_header_menu,
            on_hide,
            on_toggle_collapsed,
        },
    );

    let resize_handle = on_resize_arm
        .zip(on_resize_reset)
        .map(|(arm, reset)| global_lane_resize_handle(GlobalLaneKind::Marker, arm, reset));

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
                .children(interaction)
                .child(flag_layer)
                .children(flag_labels)
                .children(drag_handles)
                .children(crate::perf::debug_clip_outline()),
        )
        .children(resize_handle)
}

/// Hit slop expressed in beats at the current zoom, so grabbing a marker feels
/// the same whether one bar is 30 px or 300 px wide.
pub fn marker_hit_tolerance(state: &TimelineState) -> f64 {
    MARKER_HIT_SLOP_PX as f64 / state.viewport.pixels_per_beat.max(1.0) as f64
}
