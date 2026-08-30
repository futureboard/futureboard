use crate::theme::Colors;
use gpui::{
    canvas, div, fill, px, Bounds, IntoElement, ParentElement, Pixels, Point, Size, Styled,
};

/// Segment thresholds shared by every meter variant (fraction of full scale).
const METER_GREEN_TOP: f32 = 0.70;
const METER_YELLOW_TOP: f32 = 0.90;

/// GPU-composited meter renderer.
///
/// Replaces the per-segment nested-`div` meter (`vu_meter_vertical_full`) with a
/// single `canvas` element that paints the rail + green/yellow/red segments of
/// both channels directly via `window.paint_quad`. GPUI composites these quads
/// on the GPU in its own render pass — same backend the timeline/chrome use — so
/// the whole mixer's meters cost one element each with no intermediate div tree.
///
/// (A standalone `wgpu::Device` pipeline was evaluated and rejected: GPUI 0.2.2
/// can only composite an *external* texture via `paint_surface(CVPixelBuffer)`,
/// which is macOS-only. On Windows the only GPU→GPUI path is `paint_image` with
/// CPU bytes — i.e. a per-frame GPU→CPU readback, which is slower than letting
/// GPUI rasterize the quads. This is also why the timeline's offscreen wgpu
/// renderer discards its texture and falls back to GPUI paint.)
pub fn meter_surface(
    level_l: f32,
    level_r: f32,
    hold_l: f32,
    hold_r: f32,
    clip: bool,
) -> impl IntoElement {
    let bar_w = 5.0_f32;
    let gap = 1.0_f32;
    let total_w = bar_w * 2.0 + gap;
    div().w(px(total_w)).h_full().child(
        canvas(
            |_bounds, _window, _cx| (),
            move |bounds, _state, window, _cx| {
                paint_meter_bar(bounds, 0.0, bar_w, level_l, hold_l, window);
                paint_meter_bar(bounds, bar_w + gap, bar_w, level_r, hold_r, window);
                if clip {
                    paint_clip_cap(bounds, total_w, window);
                }
            },
        )
        .size_full(),
    )
}

/// Paint a clip-indicator cap across the top of the meter (both bars) when a
/// channel reached 0 dBFS. Latched/released by the meter poll.
fn paint_clip_cap(canvas_bounds: Bounds<Pixels>, width: f32, window: &mut gpui::Window) {
    let cap_h = 3.0_f32;
    let rect = Bounds {
        origin: canvas_bounds.origin,
        size: Size {
            width: px(width),
            height: px(cap_h),
        },
    };
    window.paint_quad(fill(rect, Colors::status_error()));
}

/// Paint one channel bar (rail + level segments) at `x_offset` from the canvas
/// origin, filling the canvas height bottom-up. Quads are emitted directly into
/// the GPUI scene for the current frame.
fn paint_meter_bar(
    canvas_bounds: Bounds<Pixels>,
    x_offset: f32,
    width: f32,
    level: f32,
    hold: f32,
    window: &mut gpui::Window,
) {
    let origin_x = f32::from(canvas_bounds.origin.x) + x_offset;
    let origin_y = f32::from(canvas_bounds.origin.y);
    let h = f32::from(canvas_bounds.size.height).max(0.0);
    if h <= 0.0 {
        return;
    }
    let bottom = origin_y + h;

    let rect = |y: f32, height: f32| Bounds {
        origin: Point {
            x: px(origin_x),
            y: px(y),
        },
        size: Size {
            width: px(width),
            height: px(height.max(0.0)),
        },
    };

    // Rail (full-height background track).
    window.paint_quad(fill(rect(origin_y, h), Colors::meter_rail()));

    let level_n = level.clamp(0.0, 1.0);
    let green_n = level_n.min(METER_GREEN_TOP);
    let yellow_n = if level_n > green_n {
        (level_n - green_n).min(METER_YELLOW_TOP - METER_GREEN_TOP)
    } else {
        0.0
    };
    let red_n = (level_n - green_n - yellow_n).max(0.0);

    let green_h = green_n * h;
    let yellow_h = yellow_n * h;
    let red_h = red_n * h;

    if green_h > 0.0 {
        window.paint_quad(fill(rect(bottom - green_h, green_h), Colors::meter_low()));
    }
    if yellow_h > 0.0 {
        window.paint_quad(fill(
            rect(bottom - green_h - yellow_h, yellow_h),
            Colors::meter_mid(),
        ));
    }
    if red_h > 0.0 {
        window.paint_quad(fill(
            rect(bottom - green_h - yellow_h - red_h, red_h),
            Colors::meter_high(),
        ));
    }

    // Peak-hold tick: a thin bright marker at the held-peak position.
    let hold_n = hold.clamp(0.0, 1.0);
    if hold_n > 0.0 {
        let tick_h = 2.0_f32;
        let tick_y = (bottom - hold_n * h - tick_h * 0.5).clamp(origin_y, bottom - tick_h);
        window.paint_quad(fill(rect(tick_y, tick_h), Colors::text_primary()));
    }
}

/// Legacy zero meter, kept so old call sites still link. New code should use
/// [`vu_meter_with_levels`] and pass real engine-backed meter state.
pub fn vu_meter(track_id: &str) -> impl IntoElement {
    let _ = track_id;
    vu_meter_with_levels(0.0, 0.0)
}

pub fn vu_meter_with_levels(level_l: f32, level_r: f32) -> impl IntoElement {
    vu_meter_sized(level_l, level_r, 4.0, 16.0, 2.0)
}

pub fn vu_meter_vertical(level_l: f32, level_r: f32, height: f32) -> impl IntoElement {
    vu_meter_sized(level_l, level_r, 5.0, height, 1.0)
}

/// Full-height variant used by the mixer fader area: the meter stretches to
/// fill the parent's height, so it scales with the channel strip's flex_1
/// fader slot. Bars are positioned as a fraction of parent height (`top` /
/// `h(relative(...))`).
/// Legacy nested-`div` full-height meter. Superseded by [`meter_surface`],
/// which paints the same bars as GPU quads in a single element. Kept for
/// reference / non-mixer call sites.
#[allow(dead_code)]
pub fn vu_meter_vertical_full(level_l: f32, level_r: f32) -> impl IntoElement {
    let width = 5.0_f32;
    let gap = 1.0_f32;

    let draw_bar = |level: f32| {
        let green_pct = 0.70_f32;
        let yellow_pct = 0.90_f32;

        let level_n = level.clamp(0.0, 1.0);
        let green_n = level_n.min(green_pct);
        let yellow_n = if level_n > green_n {
            (level_n - green_n).min(yellow_pct - green_pct)
        } else {
            0.0
        };
        let red_n = if level_n > green_n + yellow_n {
            level_n - green_n - yellow_n
        } else {
            0.0
        };

        let mut bar = div()
            .w(px(width))
            .h_full()
            .bg(Colors::meter_rail())
            .rounded(px(crate::theme::radius::CONTROL))
            .relative();

        if green_n > 0.0 {
            bar = bar.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .bottom(px(0.0))
                    .h(gpui::relative(green_n))
                    .bg(Colors::meter_low()),
            );
        }
        if yellow_n > 0.0 {
            bar = bar.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .bottom(gpui::relative(green_n))
                    .h(gpui::relative(yellow_n))
                    .bg(Colors::meter_mid()),
            );
        }
        if red_n > 0.0 {
            bar = bar.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .bottom(gpui::relative(green_n + yellow_n))
                    .h(gpui::relative(red_n))
                    .bg(Colors::meter_high()),
            );
        }
        bar
    };

    div()
        .flex()
        .flex_row()
        .gap(px(gap))
        .w(px(width * 2.0 + gap))
        .h_full()
        .child(draw_bar(level_l))
        .child(draw_bar(level_r))
}

fn vu_meter_sized(
    level_l: f32,
    level_r: f32,
    width: f32,
    height: f32,
    gap: f32,
) -> impl IntoElement {
    let draw_bar = |level: f32| {
        let total_height = height.max(1.0);
        let green_pct = 0.70;
        let yellow_pct = 0.90;

        let level_h = (level.clamp(0.0, 1.0) * total_height).round();
        let green_h = level_h.min((green_pct * total_height).round());
        let yellow_h = if level_h > green_h {
            (level_h - green_h).min(((yellow_pct - green_pct) * total_height).round())
        } else {
            0.0
        };
        let red_h = if level_h > green_h + yellow_h {
            level_h - green_h - yellow_h
        } else {
            0.0
        };

        div()
            .w(px(width))
            .h(px(total_height))
            .bg(Colors::meter_rail()) // background track
            .rounded(px(crate::theme::radius::CONTROL))
            .relative()
            // Green segment
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .w_full()
                    .h(px(green_h))
                    .bg(Colors::meter_low()),
            )
            // Yellow segment
            .child(
                div()
                    .absolute()
                    .bottom(px(green_h))
                    .w_full()
                    .h(px(yellow_h))
                    .bg(Colors::meter_mid()),
            )
            // Red segment
            .child(
                div()
                    .absolute()
                    .bottom(px(green_h + yellow_h))
                    .w_full()
                    .h(px(red_h))
                    .bg(Colors::meter_high()),
            )
    };

    div()
        .flex()
        .flex_row()
        .gap(px(gap))
        .w(px(width * 2.0 + gap))
        .h(px(height.max(1.0)))
        .child(draw_bar(level_l))
        .child(draw_bar(level_r))
}

/// Horizontal twin of [`meter_surface`], filling left→right.
///
/// Same thresholds, same tokens, same single-canvas strategy — only the axis
/// changes. Used by the transport bar's master meter, where the shell has
/// width to spare and no height at all.
pub fn meter_surface_horizontal(
    level_l: f32,
    level_r: f32,
    hold_l: f32,
    hold_r: f32,
    clip: bool,
    bar_h: f32,
    gap: f32,
) -> impl IntoElement {
    let total_h = bar_h * 2.0 + gap;
    div().h(px(total_h)).w_full().child(
        canvas(
            |_bounds, _window, _cx| (),
            move |bounds, _state, window, _cx| {
                paint_meter_bar_horizontal(bounds, 0.0, bar_h, level_l, hold_l, window);
                paint_meter_bar_horizontal(bounds, bar_h + gap, bar_h, level_r, hold_r, window);
                if clip {
                    paint_clip_cap_horizontal(bounds, total_h, window);
                }
            },
        )
        .size_full(),
    )
}

/// Clip indicator for the horizontal meter: a cap on the *right* edge, which is
/// where full scale is on this axis.
fn paint_clip_cap_horizontal(
    canvas_bounds: Bounds<Pixels>,
    height: f32,
    window: &mut gpui::Window,
) {
    let cap_w = 3.0_f32;
    let right = f32::from(canvas_bounds.origin.x) + f32::from(canvas_bounds.size.width);
    let rect = Bounds {
        origin: Point {
            x: px(right - cap_w),
            y: canvas_bounds.origin.y,
        },
        size: Size {
            width: px(cap_w),
            height: px(height),
        },
    };
    window.paint_quad(fill(rect, Colors::status_error()));
}

fn paint_meter_bar_horizontal(
    canvas_bounds: Bounds<Pixels>,
    y_offset: f32,
    height: f32,
    level: f32,
    hold: f32,
    window: &mut gpui::Window,
) {
    let origin_x = f32::from(canvas_bounds.origin.x);
    let origin_y = f32::from(canvas_bounds.origin.y) + y_offset;
    let w = f32::from(canvas_bounds.size.width).max(0.0);
    if w <= 0.0 {
        return;
    }

    let rect = |x: f32, width: f32| Bounds {
        origin: Point {
            x: px(x),
            y: px(origin_y),
        },
        size: Size {
            width: px(width.max(0.0)),
            height: px(height),
        },
    };

    window.paint_quad(fill(rect(origin_x, w), Colors::meter_rail()));

    let level_n = level.clamp(0.0, 1.0);
    let green_n = level_n.min(METER_GREEN_TOP);
    let yellow_n = if level_n > green_n {
        (level_n - green_n).min(METER_YELLOW_TOP - METER_GREEN_TOP)
    } else {
        0.0
    };
    let red_n = (level_n - green_n - yellow_n).max(0.0);

    let green_w = green_n * w;
    let yellow_w = yellow_n * w;
    let red_w = red_n * w;

    if green_w > 0.0 {
        window.paint_quad(fill(rect(origin_x, green_w), Colors::meter_low()));
    }
    if yellow_w > 0.0 {
        window.paint_quad(fill(
            rect(origin_x + green_w, yellow_w),
            Colors::meter_mid(),
        ));
    }
    if red_w > 0.0 {
        window.paint_quad(fill(
            rect(origin_x + green_w + yellow_w, red_w),
            Colors::meter_high(),
        ));
    }

    let hold_n = hold.clamp(0.0, 1.0);
    if hold_n > 0.0 {
        let tick_w = 2.0_f32;
        let tick_x = (origin_x + hold_n * w - tick_w * 0.5).clamp(origin_x, origin_x + w - tick_w);
        window.paint_quad(fill(rect(tick_x, tick_w), Colors::text_primary()));
    }
}

/// A track header's VU meter as its own GPUI entity.
///
/// Meters move on every audio poll — at display refresh while anything is
/// audible — and the track headers' meters used to be refreshed by notifying
/// the whole `Timeline`, which rebuilt the ruler, the grid, every visible
/// track row, every clip and every waveform so that a few pixels of green
/// could move. That single line is why a playing project stuttered while the
/// audio engine sat near idle.
///
/// GPUI invalidates per entity, so each header's meter owns one. The mixer's
/// master strip and the transport's readout already work this way; this is the
/// same pattern applied to the one surface that was missing it.
pub struct TrackMeterView {
    level_l: f32,
    level_r: f32,
    last_sig: u32,
}

impl Default for TrackMeterView {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackMeterView {
    pub fn new() -> Self {
        Self {
            level_l: 0.0,
            level_r: 0.0,
            // Not zero: a meter that has never been fed must repaint once, and
            // a real signature of 0 is silence.
            last_sig: u32::MAX,
        }
    }

    /// Quantised identity. The bar is a handful of pixels tall, so anything
    /// finer than 1/255 of full scale draws the same meter.
    fn signature(level_l: f32, level_r: f32) -> u32 {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u32;
        q(level_l) | (q(level_r) << 8)
    }

    /// Push one poll tick. Repaints only when a drawn pixel would change.
    pub fn apply(&mut self, level_l: f32, level_r: f32, cx: &mut gpui::Context<Self>) -> bool {
        let sig = Self::signature(level_l, level_r);
        self.level_l = level_l;
        self.level_r = level_r;
        if sig == self.last_sig {
            return false;
        }
        self.last_sig = sig;
        cx.notify();
        true
    }
}

impl gpui::Render for TrackMeterView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        crate::perf::count("track_meter_paint_count", 1);
        vu_meter_with_levels(self.level_l, self.level_r)
    }
}

/// The header meters, keyed by track id. Owned by `Timeline`, handed to the
/// header renderer by reference.
pub type TrackMeterViews = std::collections::HashMap<String, gpui::Entity<TrackMeterView>>;

#[cfg(test)]
mod track_meter_tests {
    use super::*;

    /// The gate is what makes a per-track entity worth having: the poll runs at
    /// display refresh, and most ticks move a level by less than a drawn pixel.
    #[test]
    fn sub_pixel_drift_is_the_same_meter() {
        let a = TrackMeterView::signature(0.5000, 0.2500);
        let b = TrackMeterView::signature(0.5001, 0.2501);
        assert_eq!(a, b);
    }

    /// A visible step must not be swallowed by it.
    #[test]
    fn a_visible_step_is_a_different_meter() {
        let quiet = TrackMeterView::signature(0.10, 0.10);
        let loud = TrackMeterView::signature(0.90, 0.10);
        assert_ne!(quiet, loud);
    }

    /// Left and right occupy their own bits, so one channel moving cannot be
    /// mistaken for the other moving back.
    #[test]
    fn the_two_channels_do_not_alias() {
        let left = TrackMeterView::signature(1.0, 0.0);
        let right = TrackMeterView::signature(0.0, 1.0);
        assert_ne!(left, right);
        assert_ne!(left, TrackMeterView::signature(0.0, 0.0));
    }

    /// Levels arrive from the engine and are not guaranteed in range; a meter
    /// is drawn clamped, so its identity has to be clamped the same way.
    #[test]
    fn out_of_range_levels_clamp_like_the_drawing_does() {
        assert_eq!(
            TrackMeterView::signature(1.5, -0.5),
            TrackMeterView::signature(1.0, 0.0)
        );
    }

    /// A freshly built meter has never drawn, so its first tick must repaint
    /// even if the levels are silent.
    #[test]
    fn a_new_meter_has_not_drawn_silence_yet() {
        let meter = TrackMeterView::new();
        assert_ne!(meter.last_sig, TrackMeterView::signature(0.0, 0.0));
    }
}
