//! Vertical normalized fader used by the mixer channel strips.
//!
//! Drag pattern matches [`super::slider`] — start a drag on mouse-down,
//! receive value updates via `on_drag_move`, never on plain click.
//!
//! The rail/scale/thumb geometry now uses `h_full` instead of a hard pixel
//! height: the parent (mixer fader area) is the flex_1 slot inside the channel
//! strip, so resizing the bottom panel makes the fader travel grow/shrink with
//! the remaining space. Thumb position uses a flex-spacer pair sized
//! proportionally to `norm`, so the thumb stays anchored on the rail at any
//! container height. Tick labels and rail ticks use `top(relative(pct))`,
//! which lays out as a fraction of parent height.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, relative, App, AppContext, DragMoveEvent, Empty, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};

use crate::components::timeline::timeline_state::volume;
use crate::theme::Colors;

/// Minimum recommended rail travel height. The fader will still render at
/// smaller heights, but below this the dB labels start to crowd.
pub const FADER_TRACK_HEIGHT: f32 = 130.0;
/// Cap height. Tall enough to be a grip and to leave the grip line room to sit
/// centred in it; every travel calculation is inset by half of this.
pub const FADER_THUMB_HEIGHT: f32 = 14.0;
pub const FADER_THUMB_WIDTH: f32 = 24.0;
/// Total width of the fader element — the cap, plus the tick tape beside the
/// groove, plus a little slack so a drag can wander horizontally.
const FADER_RAIL_W: f32 = 26.0;
const GROOVE_CENTER_X: f32 = 16.0;
const GROOVE_W: f32 = 5.0;
const GRIP_LINE_H: f32 = 2.0;
const HORIZONTAL_FADER_HEIGHT: f32 = 24.0;
const HORIZONTAL_THUMB_W: f32 = 10.0;
const HORIZONTAL_THUMB_H: f32 = 22.0;
const HORIZONTAL_RAIL_CENTER_Y: f32 = HORIZONTAL_FADER_HEIGHT / 2.0;
const HORIZONTAL_RAIL_H: f32 = 2.0;
const HORIZONTAL_ACCENT_LINE_W: f32 = 2.0;

#[derive(Clone, Debug)]
pub struct FaderDrag {
    pub id: String,
}

impl Render for FaderDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// dB tick marks. Used by [`db_scale_column`] and the fader rail so the scale
/// tape lines up with the shared `volume::db_to_norm` mapping.
///
/// The third field is whether the mark is *named* in the scale column. Every
/// mark still draws its tick on the rail — the tape has to stay complete — but
/// printing all eight numbers beside every strip turned a wall of eight-and-a-
/// half-point digits into the loudest thing in the mixer, competing with the
/// faders and meters it exists to annotate. Naming the anchors a mix engineer
/// actually reads against (unity, −12, −24, −∞) keeps the scale legible and
/// gives the level itself the attention.
pub const SCALE_MARKS: [(f32, &str, bool); 8] = [
    (volume::MAX_DB, "+6", false),
    (0.0, "0", true),
    (-6.0, "6", false),
    (-12.0, "12", true),
    (-24.0, "24", true),
    (-36.0, "36", false),
    (-48.0, "48", false),
    (volume::MIN_DB, "∞", true),
];

/// Fraction down from the top of the rail for a dB mark (0.0 = top, 1.0 = bot).
fn db_to_top_fraction(db: f32) -> f32 {
    1.0 - volume::db_to_norm(db)
}

fn pointer_y_to_norm(pointer_y: f32, bounds_y: f32, bounds_h: f32) -> f32 {
    let rail_top = FADER_THUMB_HEIGHT / 2.0;
    let rail_h = (bounds_h - FADER_THUMB_HEIGHT).max(1.0);
    let rail_y = (pointer_y - bounds_y - rail_top).clamp(0.0, rail_h);
    1.0 - rail_y / rail_h
}

fn pointer_x_to_norm(pointer_x: f32, bounds_x: f32, bounds_w: f32) -> f32 {
    let rail_left = HORIZONTAL_THUMB_W / 2.0;
    let rail_w = (bounds_w - HORIZONTAL_THUMB_W).max(1.0);
    let rail_x = (pointer_x - bounds_x - rail_left).clamp(0.0, rail_w);
    rail_x / rail_w
}

/// How far the pointer may travel between press and release and still count as
/// a click rather than a drag, in window pixels.
const RESET_CLICK_SLOP_PX: f32 = 3.0;

/// Whether a click should reset the fader to its default.
///
/// `click_count() >= 2` on its own is not enough. A drag that starts and ends on
/// the fader also produces a click, and the platform counts two gestures begun
/// near the same point within the double-click interval as one double-click — so
/// **two fader drags in a row fired the reset**, snapping the channel back to
/// 0 dB and discarding the level the user had just set. Repeatedly grabbing the
/// same fader to audition a change is exactly that gesture, which is why the
/// value would not stay down: the engine received the new gain and then received
/// `1.0000` right behind it.
///
/// Requiring the pointer to have stayed put separates the two. A keyboard-driven
/// click has no travel to measure and is always a real activation.
pub(crate) fn is_reset_double_click(event: &gpui::ClickEvent) -> bool {
    if event.click_count() < 2 {
        return false;
    }
    match event {
        gpui::ClickEvent::Mouse(mouse) => {
            let dx = f32::from(mouse.up.position.x - mouse.down.position.x).abs();
            let dy = f32::from(mouse.up.position.y - mouse.down.position.y).abs();
            dx <= RESET_CLICK_SLOP_PX && dy <= RESET_CLICK_SLOP_PX
        }
        gpui::ClickEvent::Keyboard(_) => true,
    }
}

/// `FUTUREBOARD_FADER_DEBUG=1` — trace the pointer-to-value mapping.
///
/// A fader that reports the rail maximum while the user drags it down is a
/// mapping fault, not an audio fault, and the four numbers below are the whole
/// mapping. Cached: this is read once per pointer sample during a drag.
fn fader_map_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_FADER_DEBUG").is_some())
}

/// dB scale column — uses `h_full` so it stretches with the strip's flex_1
/// fader slot. Labels are anchored via fractional `top` positions; a small
/// negative `mt` centers each ~7px label vertically on its tick.
pub fn db_scale_column() -> gpui::Div {
    let mut col = div().relative().w(px(15.0)).h_full();
    for &(db, label, named) in SCALE_MARKS.iter() {
        let pct = db_to_top_fraction(db);
        if named {
            col = col.child(
                div()
                    .absolute()
                    .top(relative(pct))
                    .right(px(0.0))
                    .mt(-px(4.0))
                    .text_size(px(crate::theme::typography::DENSE_CAPTION))
                    .text_color(if db == 0.0 {
                        // Unity is the one value the eye returns to.
                        Colors::text_secondary()
                    } else {
                        Colors::text_muted()
                    })
                    .child(label),
            );
        } else {
            // Unnamed marks keep their position on the tape as a hairline, so
            // the scale still reads as continuous.
            col = col.child(
                div()
                    .absolute()
                    .top(relative(pct))
                    .right(px(0.0))
                    .w(px(4.0))
                    .h(px(1.0))
                    .bg(Colors::border_subtle()),
            );
        }
    }
    col
}

/// Render the console fader: a recessed groove, a scale of ticks beside it,
/// and a cap at `value_norm`.
///
/// The cap is the point of this shape. A slim thumb on a rail reads as a
/// slider — a value being set — and a mixer fader is a thing the hand rests on
/// and rides. So it is wide, light, and physically proportioned: 24 px across a
/// 5 px groove, with the grip line the finger is meant to find and a shadowed
/// bottom edge so it sits *on* the strip rather than in it.
///
/// The ticks live to the left of the groove and carry no numbers. The number
/// that matters is the channel's dB readout under the bay, printed once at a
/// size worth reading; eight labelled ticks beside every strip is the same
/// number said eight times in type too small to be said well.
fn fader_rail(value_norm: f32) -> gpui::Div {
    let value = value_norm.clamp(0.0, 1.0);

    let top_basis = (1.0 - value).clamp(0.0, 1.0);
    let bot_basis = value.clamp(0.0, 1.0);

    let mut col = div()
        .relative()
        .w(px(FADER_RAIL_W))
        .h_full()
        .flex()
        .flex_col()
        .items_center();

    // The groove, inset by half a cap at each end so the cap's travel stops
    // flush with the bay rather than hanging off it.
    col = col.child(
        div()
            .absolute()
            .top(px(FADER_THUMB_HEIGHT / 2.0))
            .bottom(px(FADER_THUMB_HEIGHT / 2.0))
            .left(px(GROOVE_CENTER_X - GROOVE_W / 2.0))
            .w(px(GROOVE_W))
            .rounded(px(crate::theme::radius::MICRO))
            .bg(Colors::fader_groove())
            .border(px(1.0))
            .border_color(Colors::fader_rail()),
    );

    // Scale ticks, left of the groove. Unity and the top of the range are the
    // two the eye returns to, so they are longer and brighter; the rest are
    // there to make the tape read as continuous.
    for &(db, _, _) in SCALE_MARKS.iter() {
        let pct = db_to_top_fraction(db);
        let anchor = db == 0.0 || db == volume::MAX_DB;
        let w = if anchor { 8.0_f32 } else { 5.0_f32 };
        col = col.child(
            div()
                .absolute()
                .top(relative(pct))
                .left(px(GROOVE_CENTER_X - GROOVE_W / 2.0 - 2.0 - w))
                .h(px(1.0))
                .w(px(w))
                .bg(if anchor {
                    Colors::fader_tick()
                } else {
                    Colors::with_alpha(Colors::fader_tick(), 0.45)
                }),
        );
    }

    // Flex flow: top spacer / cap / bottom spacer. Sizing the spacers by the
    // value keeps the cap on the groove at any bay height, which is what lets
    // the mixer's fader travel grow with the panel.
    col.child(div().w(px(0.0)).flex_basis(relative(top_basis)))
        .child(
            div()
                .flex_none()
                .w(px(FADER_THUMB_WIDTH))
                .h(px(FADER_THUMB_HEIGHT))
                .rounded(px(crate::theme::radius::MICRO))
                .bg(Colors::fader_thumb())
                .relative()
                // Top highlight and bottom shadow: two hairlines are the whole
                // difference between a rectangle and a moulded cap.
                .child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(1.0))
                        .right(px(1.0))
                        .h(px(1.0))
                        .bg(Colors::with_alpha(gpui::white().into(), 0.45)),
                )
                .child(
                    div()
                        .absolute()
                        .bottom(px(0.0))
                        .left(px(1.0))
                        .right(px(1.0))
                        .h(px(1.0))
                        .bg(Colors::with_alpha(gpui::black().into(), 0.35)),
                )
                // The grip line, dead centre — this is the row the pointer is
                // aligning to and the row the value is read against.
                .child(
                    div()
                        .absolute()
                        .top(px((FADER_THUMB_HEIGHT - GRIP_LINE_H) / 2.0))
                        .left(px(3.0))
                        .right(px(3.0))
                        .h(px(GRIP_LINE_H))
                        .bg(Colors::with_alpha(gpui::black().into(), 0.45)),
                ),
        )
        .child(div().w(px(0.0)).flex_basis(relative(bot_basis)))
}

/// Horizontal orientation of the Mixer fader rail. Geometry and theme tokens
/// intentionally mirror [`fader_rail`]; only the travel axis is rotated.
fn horizontal_fader_rail(value_norm: f32, accent: gpui::Rgba) -> gpui::Div {
    let value = value_norm.clamp(0.0, 1.0);
    let thumb_accent = Colors::with_alpha(accent, 0.9);

    let mut row = div()
        .relative()
        .w_full()
        .h(px(HORIZONTAL_FADER_HEIGHT))
        .flex()
        .flex_row()
        .items_center();

    row = row.child(
        div()
            .absolute()
            .left(px(HORIZONTAL_THUMB_W / 2.0))
            .right(px(HORIZONTAL_THUMB_W / 2.0))
            .top(px(HORIZONTAL_RAIL_CENTER_Y - HORIZONTAL_RAIL_H / 2.0))
            .h(px(HORIZONTAL_RAIL_H))
            .bg(Colors::fader_rail())
            .border(px(1.0))
            .border_color(Colors::fader_groove())
            .rounded(px(crate::theme::radius::PILL)),
    );

    for &(db, _, _) in SCALE_MARKS.iter() {
        let pct = volume::db_to_norm(db);
        let h = if db == 0.0 || db == volume::MAX_DB {
            14.0_f32
        } else {
            9.0_f32
        };
        row = row.child(
            div()
                .absolute()
                .left(relative(pct))
                .ml(-px(0.5))
                .top(px(HORIZONTAL_RAIL_CENTER_Y - h / 2.0))
                .w(px(1.0))
                .h(px(h))
                .bg(if db == 0.0 || db == volume::MAX_DB {
                    Colors::fader_tick()
                } else {
                    Colors::with_alpha(Colors::fader_tick(), 0.3)
                }),
        );
    }

    row.child(div().h(px(0.0)).flex_basis(relative(value)))
        .child(
            div()
                .flex_none()
                .w(px(HORIZONTAL_THUMB_W))
                .h(px(HORIZONTAL_THUMB_H))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::surface_input())
                .border(px(1.0))
                .border_color(Colors::fader_thumb_border())
                .relative()
                .child(
                    div()
                        .absolute()
                        .top(px(1.0))
                        .left(px(1.0))
                        .bottom(px(1.0))
                        .w(px(1.0))
                        .bg(Colors::with_alpha(Colors::text_primary(), 0.15)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .bottom(px(2.0))
                        .left(px((HORIZONTAL_THUMB_W - HORIZONTAL_ACCENT_LINE_W) / 2.0))
                        .w(px(HORIZONTAL_ACCENT_LINE_W))
                        .bg(thumb_accent),
                ),
        )
        .child(
            div()
                .h(px(0.0))
                .flex_basis(relative((1.0 - value).clamp(0.0, 1.0))),
        )
}

/// Bordered dB readout pill. Use this above the fader instead of plain text so
/// the value reads as a proper integrated control.
pub fn db_value_pill(db_text: impl Into<gpui::SharedString>, highlight: bool) -> impl IntoElement {
    let border = if highlight {
        Colors::accent_primary()
    } else {
        Colors::border_default()
    };

    div()
        .flex()
        .flex_row()
        .items_baseline()
        .justify_center()
        .gap(px(2.0))
        .w(px(52.0))
        .flex_none()
        .h(px(18.0))
        .px(px(6.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .bg(Colors::button_bg())
        .border(px(1.0))
        .border_color(border)
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_primary())
                .child(db_text.into()),
        )
        .child(
            div()
                .text_size(px(7.5))
                .text_color(Colors::text_muted())
                .child("dB"),
        )
}

/// Render a vertical fader and wire drag updates. Uses `h_full` — the parent
/// must constrain height (e.g. via flex_1) so the rail/thumb scale with the
/// available channel-strip slot.
pub fn fader(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    fader_with_drag_callbacks(
        id,
        value_norm,
        None::<fn(&f32, &mut Window, &mut App)>,
        Some(on_change),
        None::<fn(&mut Window, &mut App)>,
        None::<fn(&mut Window, &mut App)>,
    )
}

pub fn fader_with_drag_callbacks(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    on_drag_start: Option<impl Fn(&f32, &mut Window, &mut App) + 'static>,
    on_drag_preview: Option<impl Fn(&f32, &mut Window, &mut App) + 'static>,
    on_drag_commit: Option<impl Fn(&mut Window, &mut App) + 'static>,
    on_double_click_reset: Option<impl Fn(&mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let id_str: gpui::SharedString = id.into();
    let id_string = id_str.to_string();
    let value = value_norm.clamp(0.0, 1.0);

    div()
        .id(gpui::ElementId::Name(id_str.clone()))
        // Hit area: rail width + horizontal slack so users can wander
        // horizontally without losing the drag.
        .w(px(FADER_RAIL_W))
        .h_full()
        .relative()
        .cursor(gpui::CursorStyle::ResizeUpDown)
        .flex()
        .flex_row()
        .justify_center()
        .child(fader_rail(value))
        .on_drag(
            FaderDrag {
                id: id_string.clone(),
            },
            move |drag, _offset, window, cx| {
                if let Some(start) = on_drag_start.as_ref() {
                    start(&value, window, cx);
                }
                cx.new(|_| FaderDrag {
                    id: drag.id.clone(),
                })
            },
        )
        .on_drag_move::<FaderDrag>(move |event: &DragMoveEvent<FaderDrag>, window, cx| {
            if event.drag(cx).id != id_string {
                return;
            }
            let bounds = event.bounds;
            let y: f32 = event.event.position.y.into();
            let oy: f32 = bounds.origin.y.into();
            let oh: f32 = f32::from(bounds.size.height).max(FADER_THUMB_HEIGHT + 1.0);
            let new_value = pointer_y_to_norm(y, oy, oh);
            if fader_map_debug_enabled() {
                eprintln!(
                    "[fader-map] v id={id_string} pointer_y={y:.1} bounds_y={oy:.1}                      bounds_h={oh:.1} norm={new_value:.4}"
                );
            }
            if let Some(preview) = on_drag_preview.as_ref() {
                preview(&new_value, window, cx);
            }
        })
        .when_some(on_drag_commit, |this, commit| {
            use std::sync::Arc;
            let commit: Arc<dyn Fn(&mut Window, &mut App) + 'static> = Arc::new(commit);
            this.on_mouse_up(gpui::MouseButton::Left, {
                let commit = commit.clone();
                move |_event, window, cx| commit(window, cx)
            })
            .on_mouse_up_out(gpui::MouseButton::Left, move |_event, window, cx| {
                commit(window, cx)
            })
        })
        .when_some(on_double_click_reset, |this, reset| {
            this.on_click(move |event, window, cx| {
                if is_reset_double_click(event) {
                    reset(window, cx);
                }
            })
        })
}

/// Mixer-style fader rotated for compact horizontal surfaces such as a
/// TrackHeader. Drag lifecycle matches [`fader_with_drag_callbacks`].
pub fn horizontal_fader_with_drag_callbacks(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    accent: gpui::Rgba,
    on_drag_start: Option<impl Fn(&f32, &mut Window, &mut App) + 'static>,
    on_drag_preview: Option<impl Fn(&f32, &mut Window, &mut App) + 'static>,
    on_drag_commit: Option<impl Fn(&mut Window, &mut App) + 'static>,
    on_double_click_reset: Option<impl Fn(&mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let id_str: gpui::SharedString = id.into();
    let id_string = id_str.to_string();
    let value = value_norm.clamp(0.0, 1.0);

    div()
        .id(gpui::ElementId::Name(id_str))
        .h(px(HORIZONTAL_FADER_HEIGHT))
        .flex_1()
        .min_w(px(36.0))
        .relative()
        .cursor(gpui::CursorStyle::ResizeLeftRight)
        .child(horizontal_fader_rail(value, accent))
        .on_drag(
            FaderDrag {
                id: id_string.clone(),
            },
            move |drag, _offset, window, cx| {
                if let Some(start) = on_drag_start.as_ref() {
                    start(&value, window, cx);
                }
                cx.new(|_| FaderDrag {
                    id: drag.id.clone(),
                })
            },
        )
        .on_drag_move::<FaderDrag>(move |event: &DragMoveEvent<FaderDrag>, window, cx| {
            if event.drag(cx).id != id_string {
                return;
            }
            let bounds = event.bounds;
            let x: f32 = event.event.position.x.into();
            let ox: f32 = bounds.origin.x.into();
            let ow: f32 = f32::from(bounds.size.width).max(HORIZONTAL_THUMB_W + 1.0);
            let new_value = pointer_x_to_norm(x, ox, ow);
            if fader_map_debug_enabled() {
                eprintln!(
                    "[fader-map] h id={id_string} pointer_x={x:.1} bounds_x={ox:.1}                      bounds_w={ow:.1} norm={new_value:.4}"
                );
            }
            if let Some(preview) = on_drag_preview.as_ref() {
                preview(&new_value, window, cx);
            }
        })
        .when_some(on_drag_commit, |this, commit| {
            use std::sync::Arc;
            let commit: Arc<dyn Fn(&mut Window, &mut App) + 'static> = Arc::new(commit);
            this.on_mouse_up(gpui::MouseButton::Left, {
                let commit = commit.clone();
                move |_event, window, cx| commit(window, cx)
            })
            .on_mouse_up_out(gpui::MouseButton::Left, move |_event, window, cx| {
                commit(window, cx)
            })
        })
        .when_some(on_double_click_reset, |this, reset| {
            this.on_click(move |event, window, cx| {
                if is_reset_double_click(event) {
                    reset(window, cx);
                }
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_ticks_follow_shared_volume_mapping() {
        assert!((db_to_top_fraction(volume::MAX_DB) - 0.0).abs() < 1.0e-6);
        assert!((db_to_top_fraction(volume::MIN_DB) - 1.0).abs() < 1.0e-6);
        assert!((db_to_top_fraction(0.0) - (1.0 - volume::db_to_norm(0.0))).abs() < 1.0e-6);
    }

    #[test]
    fn pointer_mapping_uses_rail_travel_not_outer_hitbox() {
        let h = 210.0;
        let top = FADER_THUMB_HEIGHT / 2.0;
        let bottom = h - FADER_THUMB_HEIGHT / 2.0;

        assert!((pointer_y_to_norm(top, 0.0, h) - 1.0).abs() < 1.0e-6);
        assert!((pointer_y_to_norm(bottom, 0.0, h) - 0.0).abs() < 1.0e-6);
        assert!((pointer_y_to_norm(h / 2.0, 0.0, h) - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn horizontal_pointer_mapping_uses_thumb_center_travel() {
        let w = 210.0;
        let left = HORIZONTAL_THUMB_W / 2.0;
        let right = w - HORIZONTAL_THUMB_W / 2.0;

        assert!((pointer_x_to_norm(left, 0.0, w) - 0.0).abs() < 1.0e-6);
        assert!((pointer_x_to_norm(right, 0.0, w) - 1.0).abs() < 1.0e-6);
        assert!((pointer_x_to_norm(w / 2.0, 0.0, w) - 0.5).abs() < 1.0e-6);
    }

    fn mouse_click(
        down: gpui::Point<gpui::Pixels>,
        up: gpui::Point<gpui::Pixels>,
        click_count: usize,
    ) -> gpui::ClickEvent {
        gpui::ClickEvent::Mouse(gpui::MouseClickEvent {
            down: gpui::MouseDownEvent {
                position: down,
                ..Default::default()
            },
            up: gpui::MouseUpEvent {
                position: up,
                click_count,
                ..Default::default()
            },
        })
    }

    /// The second of two quick drags on the same fader arrives with
    /// `click_count == 2`, and used to fire the double-click reset — pushing the
    /// channel back to 0 dB right after the drag had set it somewhere else.
    #[test]
    fn a_drag_never_counts_as_a_double_click_reset() {
        let down = gpui::point(px(100.0), px(80.0));
        let dragged_up = gpui::point(px(101.0), px(150.0));
        assert!(
            !is_reset_double_click(&mouse_click(down, dragged_up, 2)),
            "a gesture that travelled 70px down the rail is a drag, not a reset"
        );
        assert!(!is_reset_double_click(&mouse_click(down, dragged_up, 1)));
    }

    /// A real double-click still resets: the pointer stays put between press and
    /// release, give or take a pixel of hand tremor.
    #[test]
    fn a_stationary_double_click_still_resets() {
        let down = gpui::point(px(100.0), px(80.0));
        assert!(is_reset_double_click(&mouse_click(down, down, 2)));
        assert!(is_reset_double_click(&mouse_click(
            down,
            gpui::point(px(101.0), px(82.0)),
            2
        )));
        // One click is never a reset, however still the pointer was held.
        assert!(!is_reset_double_click(&mouse_click(down, down, 1)));
    }
}
