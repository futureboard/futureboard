//! Native port of `apps/web/src/components/ui/Knob.tsx`.
//!
//! Visual model (copied from the web component):
//! * dark filled circle body (#17191d) with a 1.5 px stroke (#3a424c)
//! * for bipolar knobs, a small center tick at 12 o'clock (#56616e)
//! * an arc drawn in the accent color from the start angle to the current
//!   indicator angle. For bipolar knobs the arc starts at 12 o'clock and
//!   sweeps clockwise (positive) or counter-clockwise (negative). For
//!   unipolar knobs the arc starts at -135° (7 o'clock) and sweeps
//!   clockwise.
//! * a small accent indicator dot at (ix, iy) on the rim
//! * a small neutral center dot (#56616e)
//! * an optional caption label below the disk in `text-daw-faint` style
//!
//! GPUI has no first-class SVG-path primitive for runtime arc generation, so
//! the arc is approximated by densely packing 2 px accent dots along the
//!   parametric arc. At step ≤ 4° the dots overlap visually and read as a
//!   continuous stroke, which matches the web component's look at the sizes
//!   the mixer uses.
//!
//! Interaction (also copied from the web component):
//! * vertical drag: (startY − currentY) / 150 × range. Up = positive.
//! * value rounded to 1/1000 to match the web rounding.
//! * value clamped to `[min, max]`.
//! * double-click resets to `default_value` (the web component has no reset;
//!   this is a small native-only affordance for pan).

use std::f32::consts::PI;

use gpui::{
    div, px, App, AppContext, DragMoveEvent, Empty, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Window,
};

use crate::theme::Colors;

/// Default knob diameter — matches the web component's default `size = 38`,
/// shrunk slightly so it sits cleanly inside the 88 px mixer strip.
pub const KNOB_DEFAULT_SIZE: f32 = 30.0;

/// Number of arc samples in the full -135°…+135° sweep. Higher = smoother
/// at the cost of more elements per knob. 91 gives a 3° step.
const ARC_SAMPLES: usize = 91;
/// Sweep half-angle in degrees, matching the web component.
const SWEEP_DEG: f32 = 135.0;

#[derive(Clone, Debug)]
pub struct KnobDrag {
    pub id: String,
    pub start_y: f32,
    pub start_value: f32,
}

impl Render for KnobDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Render a unipolar knob (value sweeps from `min` at -135° to `max` at
/// +135°). The arc fills from the start angle clockwise.
pub fn knob(
    id: impl Into<gpui::SharedString>,
    value: f32,
    min: f32,
    max: f32,
    accent: gpui::Rgba,
    label: Option<gpui::SharedString>,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    render_knob(
        id.into(),
        value,
        min,
        max,
        KNOB_DEFAULT_SIZE,
        accent,
        false,
        label,
        min,
        on_change,
    )
}

/// Render a bipolar knob centered on `(min + max) / 2`. The arc starts at
/// 12 o'clock and sweeps either left (negative) or right (positive). Pan
/// knobs use this variant.
pub fn knob_bipolar(
    id: impl Into<gpui::SharedString>,
    value: f32,
    min: f32,
    max: f32,
    accent: gpui::Rgba,
    label: Option<gpui::SharedString>,
    default_value: f32,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    render_knob(
        id.into(),
        value,
        min,
        max,
        KNOB_DEFAULT_SIZE,
        accent,
        true,
        label,
        default_value,
        on_change,
    )
}

/// Format a bipolar pan value `[-1, 1]` into the conventional mixer label.
/// `0.0` → "C", negative → "Lxx", positive → "Rxx".
pub fn format_pan_label(pan: f32) -> String {
    if pan.abs() < 0.005 {
        "C".to_string()
    } else {
        let p = (pan.abs() * 100.0).round().clamp(1.0, 100.0) as i32;
        if pan < 0.0 {
            format!("L{}", p)
        } else {
            format!("R{}", p)
        }
    }
}

fn render_knob(
    id: gpui::SharedString,
    value: f32,
    min: f32,
    max: f32,
    size: f32,
    accent: gpui::Rgba,
    bipolar: bool,
    label: Option<gpui::SharedString>,
    default_value: f32,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let range = (max - min).max(0.0001);
    let value = value.clamp(min, max);
    let pct = ((value - min) / range).clamp(0.0, 1.0);
    let angle_deg = pct * 2.0 * SWEEP_DEG - SWEEP_DEG;
    let rad = angle_deg * PI / 180.0;

    let cx = size / 2.0;
    let cy = size / 2.0;
    let r = size / 2.0 - 3.0;

    let ix = cx + r * rad.sin();
    let iy = cy - r * rad.cos();

    let id_string = id.to_string();
    let on_change_shared: std::sync::Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static> =
        std::sync::Arc::new(on_change);
    let on_change_drag = on_change_shared.clone();
    let on_change_reset = on_change_shared.clone();

    // ── Arc dots ─────────────────────────────────────────────────────────
    // Build the active arc as a sequence of 2 px accent dots between the
    // start angle and the indicator angle.
    let (arc_start_deg, arc_end_deg) = if bipolar {
        let zero_pct = (0.0 - min) / range;
        let zero_angle = zero_pct * 2.0 * SWEEP_DEG - SWEEP_DEG;
        if (pct - zero_pct).abs() < 0.005 {
            (zero_angle, zero_angle)
        } else if value >= 0.0 {
            (zero_angle, angle_deg)
        } else {
            (angle_deg, zero_angle)
        }
    } else if pct > 0.005 {
        (-SWEEP_DEG, angle_deg)
    } else {
        (0.0, 0.0)
    };

    let mut arc_dots: Vec<gpui::AnyElement> = Vec::new();
    if (arc_end_deg - arc_start_deg).abs() > 0.001 {
        let span = (arc_end_deg - arc_start_deg).abs();
        let step = (2.0 * SWEEP_DEG) / (ARC_SAMPLES - 1) as f32;
        let samples = ((span / step).ceil() as usize).max(2);
        for i in 0..=samples {
            let t = i as f32 / samples as f32;
            let deg = arc_start_deg + t * (arc_end_deg - arc_start_deg);
            let rd = deg * PI / 180.0;
            let x = cx + r * rd.sin();
            let y = cy - r * rd.cos();
            arc_dots.push(
                div()
                    .absolute()
                    .left(px(x - 1.0))
                    .top(px(y - 1.0))
                    .w(px(2.0))
                    .h(px(2.0))
                    .rounded(px(crate::theme::radius::PILL))
                    .bg(accent)
                    .into_any_element(),
            );
        }
    }

    // ── Disk children: body strokes drawn via two stacked rounded divs ──
    let body = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w(px(size))
        .h(px(size))
        .rounded(px(crate::theme::radius::PILL))
        .bg(Colors::knob_bg())
        .border(px(1.5))
        .border_color(Colors::border_strong());

    // Center tick at 12 o'clock for bipolar knobs (#56616e, 1.5 px wide).
    let center_tick = if bipolar {
        Some(
            div()
                .absolute()
                .left(px(cx - 0.75))
                .top(px(cy - r))
                .w(px(1.5))
                .h(px(3.0))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::text_muted()),
        )
    } else {
        None
    };

    // Indicator dot at (ix, iy), r = 2.
    let indicator = div()
        .absolute()
        .left(px(ix - 2.0))
        .top(px(iy - 2.0))
        .w(px(4.0))
        .h(px(4.0))
        .rounded(px(crate::theme::radius::PILL))
        .bg(accent);

    // Center dot, r = 2.5.
    let center = div()
        .absolute()
        .left(px(cx - 2.5))
        .top(px(cy - 2.5))
        .w(px(5.0))
        .h(px(5.0))
        .rounded(px(crate::theme::radius::PILL))
        .bg(Colors::text_muted());

    let disk = div()
        .id(gpui::ElementId::Name(id.clone()))
        .relative()
        .w(px(size))
        .h(px(size))
        .cursor(gpui::CursorStyle::ResizeUpDown)
        .child(body)
        .children(center_tick)
        .children(arc_dots)
        .child(indicator)
        .child(center)
        .on_drag(
            KnobDrag {
                id: id_string.clone(),
                start_y: 0.0,
                start_value: value,
            },
            move |drag, _offset, _window, cx| cx.new(|_| drag.clone()),
        )
        .on_drag_move::<KnobDrag>({
            let id_string = id_string.clone();
            move |event: &DragMoveEvent<KnobDrag>, window, cx| {
                let drag = event.drag(cx);
                if drag.id != id_string {
                    return;
                }
                // Web sensitivity: (startY − currentY) / 150 × range.
                let bounds = event.bounds;
                let cur_y: f32 = event.event.position.y.into();
                let oy: f32 = bounds.origin.y.into();
                let oh: f32 = f32::from(bounds.size.height).max(1.0);
                // Center of the drag origin = the knob center at start.
                let start_y = oy + oh / 2.0;
                let delta = (start_y - cur_y) / 150.0 * range;
                let new_value = (drag.start_value + delta).clamp(min, max);
                // Match the web component's 1/1000 rounding.
                let new_value = (new_value * 1000.0).round() / 1000.0;
                on_change_drag(&new_value, window, cx);
            }
        })
        .on_click(move |event, window, cx| {
            if event.click_count() >= 2 {
                on_change_reset(&default_value, window, cx);
            }
        });

    let mut stack = div()
        .flex()
        .flex_col()
        .items_center()
        .w(px(size))
        .child(disk);

    if let Some(label_text) = label {
        stack = stack.child(
            div()
                .mt(px(2.0))
                .text_size(px(10.0))
                .text_color(Colors::text_faint())
                .child(label_text),
        );
    }

    stack
}
