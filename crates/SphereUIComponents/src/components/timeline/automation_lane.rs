use crate::components::timeline::timeline_state::{
    automation_value_to_y, automation_y_to_value, evaluate_automation, AutomationHover,
    AutomationLaneState, AutomationMarquee, AutomationTarget, TimelineGestureContext,
    TimelineState, HEADER_WIDTH,
};
use crate::theme::Colors;
use gpui::{
    canvas, div, fill, point, px, size, AnyView, App, AppContext, Background, Bounds, Context,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, PathStyle, Pixels, Point, Render,
    StatefulInteractiveElement, StrokeOptions, Styled, Window,
};

/// Tiny tooltip surface for sub-lane control buttons. Matches the global lane
/// header tooltip styling so hover hints read consistently across the timeline.
struct LaneTooltipText(&'static str);

impl Render for LaneTooltipText {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(crate::theme::radius::CONTROL))
            .bg(Colors::surface_raised())
            .border(px(1.0))
            .border_color(Colors::border_subtle())
            .text_size(px(10.0))
            .text_color(Colors::text_secondary())
            .child(self.0)
    }
}

fn lane_tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_window, cx| cx.new(|_| LaneTooltipText(text)).into()
}

/// Top chrome height above the timeline ruler, so a window-space click can be
/// mapped into a sub-lane-local value.
use crate::shell_metrics::APP_CHROME_HEIGHT;

/// Left inset for automation sub-lane header content. Keeps lane titles visually
/// nested under the parent track without shifting the timeline grid.
const AUTOMATION_SUBLANE_HEADER_INDENT: f32 = 28.0;

/// X-position of the vertical child-lane guide inside the header column.
const AUTOMATION_SUBLANE_RAIL_X: f32 = 18.0;

/// Action fired from a sub-lane header control. One callback handles them all so
/// new lane controls land without re-threading the whole call stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationLaneAction {
    /// Focus the editor on this lane (header body click).
    Activate,
    /// Toggle the lane's read on/off (enabled flag).
    ToggleEnable,
    /// Remove every point but keep the lane.
    Clear,
    /// Hide the sub-lane row (lane + points preserved).
    Hide,
}

/// Sub-lane mouse-down payload:
/// `(track_id, lane_id, beat, value_norm, additive, alt, click_count)`.
/// `alt` enables the curve-tension edit; `click_count` distinguishes a double
/// click (Alt+double-click resets a segment to linear).
pub type AutomationDownCallback = std::sync::Arc<
    dyn Fn(&(String, String, f32, f32, bool, bool, u32), &mut gpui::Window, &mut gpui::App)
        + 'static,
>;

/// Sub-lane header action payload: `(track_id, lane_id, action)`.
pub type AutomationLaneActionCallback = std::sync::Arc<
    dyn Fn(&(String, String, AutomationLaneAction), &mut gpui::Window, &mut gpui::App) + 'static,
>;

/// Sub-lane hover payload: `(track_id, lane_id, beat, value_norm)`. Fired on
/// mouse-move over a lane so the editor can resolve the hovered point/segment;
/// `beat` is snapped exactly like the mouse-down path so hover and click agree.
pub type AutomationHoverCallback = std::sync::Arc<
    dyn Fn(&(String, String, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static,
>;

/// Human category shown under the lane name in the sub-lane header.
fn target_category(target: &AutomationTarget) -> &'static str {
    match target {
        AutomationTarget::TrackVolume
        | AutomationTarget::TrackPan
        | AutomationTarget::TrackMute => "Track",
        AutomationTarget::PluginParameter { .. } => "Plugin Parameter",
        AutomationTarget::SendLevel { .. } => "Send",
    }
}

/// One expanded automation sub-lane rendered directly below its parent track.
/// Left header carries the target name/category + lane controls; the right area
/// draws the envelope (line + points) and captures point edits scoped to this
/// lane's own row bounds.
#[allow(clippy::too_many_arguments)]
pub fn automation_lane(
    track_id: &str,
    lane: &AutomationLaneState,
    track_color: gpui::Rgba,
    is_active: bool,
    lane_y_abs: f32,
    lane_height: f32,
    state: &TimelineState,
    gesture: &std::rc::Rc<TimelineGestureContext>,
    on_automation_down: Option<AutomationDownCallback>,
    on_lane_action: Option<AutomationLaneActionCallback>,
    on_automation_hover: Option<AutomationHoverCallback>,
    marquee: Option<&AutomationMarquee>,
    hover: Option<&AutomationHover>,
) -> impl IntoElement {
    let track_id = track_id.to_string();
    let lane_id = lane.id.clone();
    // Hover that targets THIS lane (drives the segment highlight + cursor).
    let lane_hover = hover
        .filter(|h| h.matches_lane(&track_id, &lane_id))
        .cloned();
    let id_num = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        track_id.hash(&mut hasher);
        lane.id.hash(&mut hasher);
        hasher.finish() as usize
    };

    let category = target_category(&lane.target);
    // Source/category text stays in the muted ramp — never bright accent — so
    // the only saturated element in the lane is the envelope curve itself.
    let category_color = if is_active {
        Colors::text_secondary()
    } else {
        Colors::with_alpha(Colors::text_muted(), 0.62)
    };
    let mut accent = track_color;
    accent.a = if is_active { 0.92 } else { 0.55 };

    // ── Left header (indented child lane — timeline grid stays flush right) ───
    let activate_action = on_lane_action.clone();
    let mut header = div()
        .relative()
        .w(px(HEADER_WIDTH))
        .h_full()
        .flex_none()
        .border_r(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(if is_active {
            Colors::automation_lane_bg_selected()
        } else {
            Colors::automation_lane_header_bg()
        })
        .id(("automation-lane-header", id_num))
        .cursor(gpui::CursorStyle::PointingHand);
    if let Some(cb) = activate_action {
        let tid = track_id.clone();
        let lid = lane_id.clone();
        header = header.on_mouse_down(gpui::MouseButton::Left, move |_e, window, cx| {
            cx.stop_propagation();
            cb(
                &(tid.clone(), lid.clone(), AutomationLaneAction::Activate),
                window,
                cx,
            );
        });
    }

    // Nesting gutter — slightly darker band in the indent column so child lanes
    // read as belonging to the parent, not as peer tracks.
    header = header.child(
        div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(AUTOMATION_SUBLANE_HEADER_INDENT))
            .bg(Colors::with_alpha(Colors::surface_muted(), 0.5)),
    );

    // Vertical child-lane guide shared by every automation sub-row. Active lanes
    // light the rail with the automation accent; idle lanes stay quiet graphite.
    header = header.child(
        div()
            .absolute()
            .left(px(AUTOMATION_SUBLANE_RAIL_X))
            .top(px(9.0))
            .bottom(px(9.0))
            .w(px(1.0))
            .bg(if is_active {
                Colors::automation_rail_active()
            } else {
                Colors::automation_rail()
            }),
    );

    // Accent bar + title — indented, smaller than the parent track name.
    let name_row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .min_w(px(0.0))
        .child(
            div()
                .w(px(2.0))
                .h(px(9.0))
                .rounded(px(crate::theme::radius::PILL))
                .bg(accent),
        )
        .child(
            // Parameter name must stay on a single line — `truncate` applies
            // nowrap + ellipsis so "Volume" can never wrap to "Volu / me".
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if lane.enabled {
                    Colors::text_primary()
                } else {
                    Colors::text_muted()
                })
                .truncate()
                .child(lane.name.clone()),
        );

    let category_row = div()
        .text_size(px(8.0))
        .text_color(category_color)
        .pl(px(6.0))
        .truncate()
        .child(category);

    // Lane controls stay flush to the right edge of the header column.
    let control_buttons = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .child(lane_button(
            ("automation-lane-enable", id_num).into(),
            "E",
            "Enable automation lane",
            LaneButtonStyle::Toggle,
            lane.enabled,
            track_id.clone(),
            lane_id.clone(),
            AutomationLaneAction::ToggleEnable,
            on_lane_action.clone(),
        ))
        // Clear is the destructive action here (removes every point), so it is
        // the one that reads danger on hover.
        .child(lane_button(
            ("automation-lane-clear", id_num).into(),
            "C",
            "Clear automation points",
            LaneButtonStyle::Danger,
            false,
            track_id.clone(),
            lane_id.clone(),
            AutomationLaneAction::Clear,
            on_lane_action.clone(),
        ))
        .child(lane_button(
            ("automation-lane-hide", id_num).into(),
            "x",
            "Hide lane",
            LaneButtonStyle::Neutral,
            false,
            track_id.clone(),
            lane_id.clone(),
            AutomationLaneAction::Hide,
            on_lane_action.clone(),
        ));

    let header = header.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h_full()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(1.0))
                    .pl(px(AUTOMATION_SUBLANE_HEADER_INDENT))
                    .pr(px(4.0))
                    .child(name_row)
                    .child(category_row),
            )
            .child(div().flex_none().pr(px(8.0)).child(control_buttons)),
    );

    // ── Right envelope + interaction area ────────────────────────────────────
    let envelope = lane_envelope(lane, state, lane_height, marquee, lane_hover.as_ref());

    // Cursor reflects what the hovered region edits: a point handle (pointer) or a
    // curve segment (vertical-resize = drag to shape tension). Built-in OS cursors
    // — no custom-PNG hotspot, so they stay correct at 125% / 150% / 200% DPI.
    // Empty lane is left untouched (keeps the active tool's cursor).
    let hover_cursor: Option<gpui::CursorStyle> = match lane_hover.as_ref() {
        Some(h) if h.point_id.is_some() => Some(gpui::CursorStyle::PointingHand),
        Some(h) if h.segment_left_id.is_some() => Some(gpui::CursorStyle::ResizeUpDown),
        _ => None,
    };

    let interaction = on_automation_down.clone().map(|cb| {
        // Per-frame geometry snapshot, not a full project clone — see
        // [`TimelineGestureContext`].
        let state_for = std::rc::Rc::clone(gesture);
        let tid = track_id.clone();
        let lid = lane_id.clone();
        let mut hit = div()
            .absolute()
            .inset_0()
            .id(("automation-lane-hit", id_num))
            .on_mouse_down(
                gpui::MouseButton::Left,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let wx: f32 = event.position.x.into();
                    let wy: f32 = event.position.y.into();
                    let lane_x = state_for.lane_x_from_window_x(wx);
                    let raw_beat = state_for.x_to_beats(lane_x);
                    let snapped_sec = state_for.snap_time(raw_beat * state_for.seconds_per_beat());
                    let beat = (snapped_sec / state_for.seconds_per_beat()).max(0.0);
                    let content_y = wy - APP_CHROME_HEIGHT - state_for.arrangement_content_top()
                        + state_for.viewport.scroll_y;
                    let local_y = content_y - lane_y_abs;
                    let value = automation_y_to_value(local_y, lane_height);
                    let additive = event.modifiers.shift || event.modifiers.control;
                    let alt = event.modifiers.alt;
                    let click_count = event.click_count.max(1) as u32;
                    cb(
                        &(
                            tid.clone(),
                            lid.clone(),
                            beat,
                            value,
                            additive,
                            alt,
                            click_count,
                        ),
                        window,
                        cx,
                    );
                },
            );

        if let Some(cursor) = hover_cursor {
            hit = hit.cursor(cursor);
        }

        // Hover tracking: resolve the point/segment under the cursor on move, and
        // clear it when the cursor leaves the lane. Same snapped beat as the click
        // path so the hovered target matches what a click would grab.
        if let Some(hover_cb) = on_automation_hover.clone() {
            let state_for = std::rc::Rc::clone(gesture);
            let tid = track_id.clone();
            let lid = lane_id.clone();
            hit = hit.on_mouse_move(move |event: &gpui::MouseMoveEvent, window, cx| {
                // Only resolve hover when not dragging — a pressed-button move is a
                // gesture and is handled by the global timeline move handler.
                if event.pressed_button.is_some() {
                    return;
                }
                let wx: f32 = event.position.x.into();
                let wy: f32 = event.position.y.into();
                let lane_x = state_for.lane_x_from_window_x(wx);
                let raw_beat = state_for.x_to_beats(lane_x);
                let snapped_sec = state_for.snap_time(raw_beat * state_for.seconds_per_beat());
                let beat = (snapped_sec / state_for.seconds_per_beat()).max(0.0);
                let content_y = wy - APP_CHROME_HEIGHT - state_for.arrangement_content_top()
                    + state_for.viewport.scroll_y;
                let local_y = content_y - lane_y_abs;
                let value = automation_y_to_value(local_y, lane_height);
                hover_cb(&(tid.clone(), lid.clone(), beat, value), window, cx);
            });
        }
        if let Some(hover_cb) = on_automation_hover.clone() {
            let tid = track_id.clone();
            let lid = lane_id.clone();
            // Hover-out: an out-of-range beat/value signals "clear" to the handler
            // (it resolves no point/segment there and drops the highlight).
            hit = hit.on_hover(move |hovered, window, cx| {
                if !*hovered {
                    hover_cb(&(tid.clone(), lid.clone(), -1.0, -1.0), window, cx);
                }
            });
        }
        hit
    });

    // Right-side lane body: a TRANSLUCENT overlay so the timeline grid behind
    // the rows stays visible. The lane reads as a sublane overlay on the
    // arrangement canvas, never an opaque dark block. The selected lane only
    // gets a whisper of purple — the rail/curve/label carry the selection.
    let lane_area = div()
        .flex_1()
        .h_full()
        .relative()
        .overflow_hidden()
        .bg(if is_active {
            Colors::automation_canvas_bg_selected()
        } else {
            Colors::automation_canvas_bg()
        })
        .child(envelope)
        .children(interaction);

    div()
        .flex()
        .flex_row()
        .w_full()
        .h(px(lane_height))
        // No row-level fill — the header paints the left label, the lane_area
        // paints a translucent right body. Only a subtle separator hairline.
        .border_b(px(1.0))
        .border_color(Colors::with_alpha(Colors::automation_separator(), 0.7))
        .child(header)
        .child(lane_area)
}

/// Visual weight for a sub-lane control. Only the active toggle carries the
/// accent; the destructive action stays neutral until hovered.
#[derive(Clone, Copy)]
enum LaneButtonStyle {
    /// Accent when `active`, neutral otherwise (Enable).
    Toggle,
    /// Always neutral with a quiet hover (Hide).
    Neutral,
    /// Neutral by default, danger/red only on hover (Clear).
    Danger,
}

/// Small square control button used in the sub-lane header.
#[allow(clippy::too_many_arguments)]
fn lane_button(
    id: gpui::ElementId,
    label: &'static str,
    tooltip: &'static str,
    style: LaneButtonStyle,
    active: bool,
    track_id: String,
    lane_id: String,
    action: AutomationLaneAction,
    cb: Option<AutomationLaneActionCallback>,
) -> impl IntoElement {
    let mut btn = div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(20.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::BOLD)
        .id(id)
        .cursor(gpui::CursorStyle::PointingHand)
        .tooltip(lane_tooltip(tooltip));
    match style {
        LaneButtonStyle::Toggle if active => {
            // Active enable is the one place the accent shows.
            btn = btn
                .bg(Colors::accent_primary())
                .text_color(Colors::text_inverse());
        }
        LaneButtonStyle::Danger => {
            btn = btn
                .bg(Colors::button_bg())
                .text_color(Colors::button_text_muted())
                .hover(|s| {
                    s.bg(Colors::with_alpha(Colors::status_error(), 0.18))
                        .text_color(Colors::status_error())
                });
        }
        LaneButtonStyle::Toggle | LaneButtonStyle::Neutral => {
            btn = btn
                .bg(Colors::button_bg())
                .text_color(Colors::button_text_muted())
                .hover(|s| {
                    s.bg(Colors::button_bg_hover())
                        .text_color(Colors::button_text())
                });
        }
    }
    if let Some(cb) = cb {
        btn = btn.on_mouse_down(gpui::MouseButton::Left, move |_e, window, cx| {
            cx.stop_propagation();
            cb(&(track_id.clone(), lane_id.clone(), action), window, cx);
        });
    }
    btn.child(label)
}

/// Logical stroke widths for the automation envelope. Kept comfortably above
/// 1px so HiDPI scaling can never thin the line into subpixel shimmer —
/// `paint_path` applies the device scale exactly once, so these stay visually
/// stable at 125% / 150% / 175% / 200%.
const AUTOMATION_LINE_WIDTH: f32 = 1.6;
const AUTOMATION_LINE_WIDTH_HOVER: f32 = 2.2;
const AUTOMATION_LINE_WIDTH_ACTIVE: f32 = 2.6;

/// Paint a polyline as a single anti-aliased stroked path.
///
/// `pts` are lane-local (x/y in lane pixels) and `origin` is the canvas'
/// window-space top-left, so the path lands in the right place. Coordinates stay
/// in floating point on purpose: GPUI tessellates + anti-aliases the stroke and
/// `paint_path` applies the DPI scale once, so diagonals and curves come out
/// smooth at any zoom / HiDPI scale instead of the old pixel-stepped quads. The
/// whole curve is one continuous path (not per-segment quads), so there are no
/// gaps or unpainted pixels at segment boundaries.
fn paint_automation_stroke(
    window: &mut Window,
    origin: Point<Pixels>,
    pts: &[(f32, f32)],
    width: f32,
    color: impl Into<Background>,
) {
    if pts.len() < 2 {
        return;
    }
    // Tight miter limit: a continuous single-path stroke that bevels (rather than
    // spiking) at sharp peaks. No `lyon::LineJoin` import is needed — the default
    // join already produces gap-free joins; the limit just tames sharp corners.
    let options = StrokeOptions::default()
        .with_line_width(width)
        .with_miter_limit(2.0);
    let mut builder = PathBuilder::stroke(px(width)).with_style(PathStyle::Stroke(options));
    let (x0, y0) = pts[0];
    builder.move_to(origin + point(px(x0), px(y0)));
    for &(x, y) in &pts[1..] {
        builder.line_to(origin + point(px(x), px(y)));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Draw the automation line + points for one lane inside its sub-lane area.
/// Pure render of state. The curve is sampled per visible column so Hold steps
/// and Linear ramps are both correct.
fn lane_envelope(
    lane: &AutomationLaneState,
    state: &TimelineState,
    lane_height: f32,
    marquee: Option<&AutomationMarquee>,
    hover: Option<&AutomationHover>,
) -> impl IntoElement {
    let default_value = lane.target.default_value();
    let points = lane.points.clone();

    let lane_w = state.viewport.viewport_width.max(1.0);
    // Screen-space adaptive sampling: ~one sample per logical pixel of visible
    // width. That keeps the polyline continuous and smooth at any zoom (a wider
    // visible segment / higher zoom yields more samples), while a hard cap stops
    // an ultra-wide window from blowing up the per-frame stroke tessellation. A
    // 1px screen step is below what a thin AA stroke can resolve, so curves read
    // as smooth without oversampling tiny offscreen detail.
    const MAX_SAMPLES: usize = 4096;
    let sample_step = (lane_w / MAX_SAMPLES as f32).max(1.0);
    let sample_count = (lane_w / sample_step).ceil().max(1.0) as usize;

    // Hovered / actively-dragged segment → column range to emphasize. Uses the
    // SAME point geometry the curve is sampled from, so the highlight tracks the
    // visible curve at any zoom/scroll. `(c0, c1, active)`.
    let highlight: Option<(usize, usize, bool)> = hover
        .and_then(|h| h.segment_left_id.map(|id| (id, h.active)))
        .and_then(|(left_id, active)| {
            let i = points.iter().position(|p| p.id == left_id)?;
            if i + 1 >= points.len() {
                return None;
            }
            let x0 = state.beats_to_x(points[i].beat);
            let x1 = state.beats_to_x(points[i + 1].beat);
            let c0 = (x0 / sample_step).floor().max(0.0) as usize;
            let c1 = ((x1 / sample_step).ceil().max(0.0) as usize).min(sample_count);
            (c1 > c0).then_some((c0, c1, active))
        });

    let mut samples: Vec<(f32, f32)> = Vec::with_capacity(sample_count + 1);
    for sample in 0..=sample_count {
        let x = (sample as f32 * sample_step).min(lane_w);
        let beat = state.x_to_beat(x);
        let v = evaluate_automation(&points, beat, default_value);
        samples.push((x, automation_value_to_y(v, lane_height)));
    }
    let baseline_y = automation_value_to_y(default_value, lane_height);

    let enabled = lane.enabled;
    // Curve at ~0.85 so it stays clearly readable without the razor-sharp edge.
    let line_color = if enabled {
        Colors::with_alpha(Colors::automation_curve(), 0.85)
    } else {
        Colors::with_alpha(Colors::automation_curve(), 0.32)
    };
    // Hovered / dragged segment: same hue at full alpha, thicker line (width
    // carries the emphasis). No accent, glow or gloss — keeps the lane's "only
    // saturated element is the curve" rule. Active drag is thicker than hover.
    let highlight_color = if enabled {
        Colors::automation_curve()
    } else {
        Colors::with_alpha(Colors::automation_curve(), 0.5)
    };
    // Center/value reference line + a soft band behind the curve so the lane has
    // a quiet value guide rather than a single sharp line on a flat block.
    let baseline_color = Colors::automation_center_line();
    let band_color = Colors::automation_center_band();
    let band_h = (lane_height * 0.5).clamp(10.0, 30.0);

    let line = canvas(
        |_b, _w, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            // Soft center band behind everything.
            let band = Bounds::new(
                bounds.origin + point(px(0.0), px((baseline_y - band_h / 2.0).max(0.0))),
                size(px(lane_w), px(band_h)),
            );
            window.paint_quad(fill(band, band_color));
            // Center/value guide line.
            let bl = Bounds::new(
                bounds.origin + point(px(0.0), px(baseline_y)),
                size(px(lane_w), px(1.0)),
            );
            window.paint_quad(fill(bl, baseline_color));

            // Base envelope: ONE continuous anti-aliased stroke for the whole
            // visible curve. Replaces the old per-column hard quads, so diagonals
            // and curved segments are smooth instead of stair-stepped.
            paint_automation_stroke(
                window,
                bounds.origin,
                &samples,
                AUTOMATION_LINE_WIDTH,
                line_color,
            );

            // Hovered / actively-dragged segment: the SAME sampled path, redrawn
            // thicker and at full alpha over the base. The emphasis is a clean
            // weight change with no second jagged 1px line and no doubled pixels.
            if let Some((c0, c1, active)) = highlight {
                if c1 > c0 && c1 <= sample_count {
                    let width = if active {
                        AUTOMATION_LINE_WIDTH_ACTIVE
                    } else {
                        AUTOMATION_LINE_WIDTH_HOVER
                    };
                    paint_automation_stroke(
                        window,
                        bounds.origin,
                        &samples[c0..=c1],
                        width,
                        highlight_color,
                    );
                }
            }
        },
    )
    .absolute()
    .inset_0();

    let markers: Vec<_> = points
        .iter()
        .filter_map(|p| {
            let x = state.beats_to_x(p.beat);
            if x < -8.0 || x > lane_w + 8.0 {
                return None;
            }
            let y = automation_value_to_y(p.value, lane_height);
            let (fill_color, ring) = if p.selected {
                (Colors::text_primary(), Colors::automation_curve())
            } else {
                (Colors::automation_point(), Colors::automation_curve())
            };
            let size_px = if p.selected { 9.0 } else { 7.0 };
            Some(
                div()
                    .absolute()
                    .left(px(x - size_px / 2.0))
                    .top(px(y - size_px / 2.0))
                    .w(px(size_px))
                    .h(px(size_px))
                    .rounded(px(crate::theme::radius::PILL))
                    .bg(fill_color)
                    .border(px(1.0))
                    .border_color(ring),
            )
        })
        .collect();

    let marquee_el = marquee.filter(|m| m.lane_id == lane.id).map(|m| {
        let x0 = state.beats_to_x(m.start_beat.min(m.cur_beat));
        let x1 = state.beats_to_x(m.start_beat.max(m.cur_beat));
        let y0 = automation_value_to_y(m.start_value.max(m.cur_value), lane_height);
        let y1 = automation_value_to_y(m.start_value.min(m.cur_value), lane_height);
        div()
            .absolute()
            .left(px(x0))
            .top(px(y0))
            .w(px((x1 - x0).max(1.0)))
            .h(px((y1 - y0).max(1.0)))
            .bg(Colors::with_alpha(Colors::accent_primary(), 0.14))
            .border(px(1.0))
            .border_color(Colors::with_alpha(Colors::accent_primary(), 0.7))
    });

    div()
        .absolute()
        .inset_0()
        .child(line)
        .children(markers)
        .children(marquee_el)
}
