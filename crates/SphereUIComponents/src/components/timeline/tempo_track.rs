use crate::components::timeline::global_lane_header::{
    global_lane_header, GlobalLaneHeaderActions,
};
use crate::components::timeline::timeline_state::{
    bpm_to_y, TempoMap, TimelineState, TEMPO_LANE_PAD,
};
use crate::theme::Colors;
use gpui::{
    canvas, div, fill, point, px, size, Bounds, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, PathStyle, Pixels, StrokeOptions, Styled,
};

/// Tempo Track mouse-down: `(beat, bpm, point_id, additive, click_count)`.
pub type TempoTrackDownCallback = std::sync::Arc<
    dyn Fn(&(f64, f64, Option<String>, bool, u32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

/// Tempo Track context menu: `(beat, bpm, point_id, screen_x, screen_y)`.
pub type TempoTrackContextCallback = std::sync::Arc<
    dyn Fn(&(f64, f64, Option<String>, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

pub type GlobalLaneVoidCallback =
    std::sync::Arc<dyn Fn(&(), &mut gpui::Window, &mut gpui::App) + 'static>;
pub type GlobalLaneMenuCallback =
    std::sync::Arc<dyn Fn(&(f32, f32), &mut gpui::Window, &mut gpui::App) + 'static>;

const APP_CHROME_HEIGHT: f32 = 36.0;

/// Global Tempo Track lane — header + automation curve over the project TempoMap.
pub fn tempo_track_lane(
    state: &TimelineState,
    lane_height: f32,
    on_down: Option<TempoTrackDownCallback>,
    on_context: Option<TempoTrackContextCallback>,
    on_add: Option<GlobalLaneVoidCallback>,
    on_header_menu: Option<GlobalLaneMenuCallback>,
    on_hide: Option<GlobalLaneVoidCallback>,
    on_toggle_collapsed: Option<GlobalLaneVoidCallback>,
) -> impl IntoElement {
    let (min_bpm, max_bpm) = state.tempo_lane_bpm_range();
    let lane_w = state.viewport.viewport_width.max(1.0);
    let num_cols = lane_w.ceil().max(1.0) as usize;

    let mut samples: Vec<f32> = Vec::with_capacity(num_cols + 1);
    for col in 0..=num_cols {
        let beat = state.x_to_beat(col as f32);
        let bpm = state.effective_bpm_at_beat(beat);
        samples.push(bpm_to_y(bpm, lane_height, min_bpm, max_bpm));
    }

    let line_color = Colors::accent_primary();
    let fill_under = Colors::with_alpha(Colors::accent_primary(), 0.08);
    let baseline_bpm = if state.tempo_map.points.is_empty() {
        state.bpm as f64
    } else {
        state.tempo_map.points[0].bpm
    };
    let baseline_y = bpm_to_y(baseline_bpm, lane_height, min_bpm, max_bpm);
    let baseline_color = Colors::with_alpha(Colors::text_primary(), 0.12);

    let curve = canvas(
        |_b, _w, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let bl = Bounds::new(
                bounds.origin + point(px(0.0), px(baseline_y)),
                size(px(lane_w), px(1.0)),
            );
            window.paint_quad(fill(bl, baseline_color));

            // A single stroked path lets GPUI's tessellator generate coverage
            // for fractional y positions. Per-column quads produced stair-step
            // edges and shimmered when the lane was zoomed or displayed at 125%
            // / 150% Windows scaling.
            if samples.len() >= 2 {
                let options = StrokeOptions::default()
                    .with_line_width(1.6)
                    .with_miter_limit(2.0);
                let mut path = PathBuilder::stroke(px(1.6)).with_style(PathStyle::Stroke(options));
                path.move_to(bounds.origin + point(px(0.0), px(samples[0])));
                for (col, y) in samples.iter().enumerate().skip(1) {
                    path.line_to(bounds.origin + point(px(col as f32), px(*y)));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, line_color);
                }
            }

            for col in 0..num_cols {
                let top = samples[col].min(samples[col + 1]);
                if col % 3 == 0 {
                    let fill_h = (lane_height - top).max(0.0);
                    if fill_h > 0.5 {
                        let fr = Bounds::new(
                            bounds.origin + point(px(col as f32), px(top)),
                            size(px(2.0), px(fill_h)),
                        );
                        window.paint_quad(fill(fr, fill_under));
                    }
                }
            }
        },
    )
    .absolute()
    .inset_0();

    let points = state.tempo_map.points.clone();
    let selected_id = state.selected_tempo_point_id.clone();
    let show_all_labels = points.len() <= 1;
    // Compact single-line pill geometry. The label is drawn as a sibling overlay
    // (not a child of the tiny handle dot) so it is never width-constrained into
    // a vertical character stack, and so it can be clamped inside the lane.
    const TEMPO_LABEL_MIN_W: f32 = 44.0;
    const TEMPO_LABEL_H: f32 = 16.0;
    let mut markers: Vec<gpui::Div> = Vec::new();
    for p in &points {
        let x = state.beats_to_x(p.beat as f32);
        if x < -16.0 || x > lane_w + 16.0 {
            continue;
        }
        let y = bpm_to_y(p.bpm, lane_height, min_bpm, max_bpm);
        let selected = selected_id.as_deref() == Some(p.id.as_str());
        let show_label = selected || show_all_labels;
        let size_px = if selected { 10.0 } else { 8.0 };
        let (fill_color, ring) = if selected {
            (Colors::text_primary(), Colors::accent_primary())
        } else {
            (
                Colors::accent_primary(),
                Colors::with_alpha(Colors::text_primary(), 0.65),
            )
        };

        markers.push(
            div()
                .absolute()
                .left(px(x - size_px / 2.0))
                .top(px(y - size_px / 2.0))
                .cursor(gpui::CursorStyle::PointingHand)
                .child(
                    div()
                        .w(px(size_px))
                        .h(px(size_px))
                        .rounded_full()
                        .bg(fill_color)
                        .border(px(1.5))
                        .border_color(ring)
                        .shadow(vec![gpui::BoxShadow {
                            color: Colors::with_alpha(Colors::accent_primary(), 0.35).into(),
                            offset: point(px(0.0), px(0.0)),
                            blur_radius: px(4.0),
                            spread_radius: px(0.0),
                            inset: false,
                        }]),
                ),
        );

        if show_label {
            let label = TempoMap::format_marker_label(p.bpm);
            let label_text = format!("{label} BPM");
            // Single-line pill width: estimate from glyph count, never below the
            // minimum so it can't shrink to the handle width.
            let pill_w = (label_text.chars().count() as f32 * 5.5 + 14.0).max(TEMPO_LABEL_MIN_W);
            // Center the pill over the handle, then clamp it inside the lane.
            let pill_x = (x - pill_w / 2.0).clamp(2.0, (lane_w - pill_w - 2.0).max(2.0));
            // Prefer above the dot; drop below if there is no room, then clamp so
            // it stays fully inside the lane content (no top/bottom clipping).
            let mut pill_y = y - size_px / 2.0 - TEMPO_LABEL_H - 4.0;
            if pill_y < 2.0 {
                pill_y = y + size_px / 2.0 + 4.0;
            }
            pill_y = pill_y.clamp(2.0, (lane_height - TEMPO_LABEL_H - 2.0).max(2.0));

            markers.push(
                div()
                    .absolute()
                    .left(px(pill_x))
                    .top(px(pill_y))
                    .w(px(pill_w))
                    .h(px(TEMPO_LABEL_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .bg(Colors::with_alpha(Colors::surface_raised(), 0.95))
                    .border(px(1.0))
                    .border_color(Colors::with_alpha(Colors::accent_primary(), 0.35))
                    .text_size(px(9.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(Colors::text_primary())
                    .whitespace_nowrap()
                    .child(label_text),
            );
        }
    }

    let subtitle = state.tempo_lane_header_subtitle();
    let content_top = APP_CHROME_HEIGHT + crate::components::timeline::timeline_state::RULER_HEIGHT;

    let interaction = on_down.map(|cb| {
        let state_left = state.clone();
        let lane_h = lane_height;
        let min = min_bpm;
        let max = max_bpm;
        let mut layer = div()
            .absolute()
            .inset_0()
            .id("tempo-track-hit")
            .on_mouse_down(
                gpui::MouseButton::Left,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let wx: f32 = event.position.x.into();
                    let wy: f32 = event.position.y.into();
                    let lane_x = state_left.lane_x_from_window_x(wx);
                    let beat = state_left.x_to_beat(lane_x).max(0.0);
                    let snapped = state_left.snap_beats(beat as f32) as f64;
                    let local_y = wy - content_top - TEMPO_LANE_PAD;
                    let bpm = crate::components::timeline::timeline_state::y_to_bpm(
                        local_y, lane_h, min, max,
                    );
                    let ppb = state_left.viewport.pixels_per_beat.max(1.0) as f64;
                    let beat_tol = 10.0 / ppb;
                    let usable = (lane_h - 2.0 * TEMPO_LANE_PAD).max(1.0);
                    let bpm_tol = (max - min) * 10.0 / usable as f64;
                    let point_id = state_left.tempo_point_at(snapped, bpm, beat_tol, bpm_tol);
                    let additive = event.modifiers.shift || event.modifiers.control;
                    cb(
                        &(snapped, bpm, point_id, additive, event.click_count as u32),
                        window,
                        cx,
                    );
                },
            );
        if let Some(ctx_cb) = on_context {
            let state_right = state.clone();
            layer = layer.on_mouse_down(
                gpui::MouseButton::Right,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let wx: f32 = event.position.x.into();
                    let wy: f32 = event.position.y.into();
                    let sx: f32 = event.position.x.into();
                    let sy: f32 = event.position.y.into();
                    let lane_x = state_right.lane_x_from_window_x(wx);
                    let beat = state_right.x_to_beat(lane_x).max(0.0);
                    let local_y = wy - content_top - TEMPO_LANE_PAD;
                    let bpm = crate::components::timeline::timeline_state::y_to_bpm(
                        local_y, lane_h, min, max,
                    );
                    let ppb = state_right.viewport.pixels_per_beat.max(1.0) as f64;
                    let beat_tol = 10.0 / ppb;
                    let usable = (lane_h - 2.0 * TEMPO_LANE_PAD).max(1.0);
                    let bpm_tol = (max - min) * 10.0 / usable as f64;
                    let point_id = state_right.tempo_point_at(beat, bpm, beat_tol, bpm_tol);
                    ctx_cb(&(beat, bpm, point_id, sx, sy), window, cx);
                },
            );
        }
        layer
    });

    let header = global_lane_header(
        "tempo",
        "Tempo",
        subtitle,
        state.tempo_track_collapsed,
        "Hide Tempo Track",
        GlobalLaneHeaderActions {
            on_add,
            on_menu: on_header_menu,
            on_hide,
            on_toggle_collapsed,
        },
    );

    div()
        .flex()
        .flex_row()
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
                .child(curve)
                .children(markers)
                .children(interaction)
                // Debug: outline tempo_lane_content_rect (FUTUREBOARD_UI_DEBUG_CLIPS=1).
                .children(crate::perf::debug_clip_outline()),
        )
}
