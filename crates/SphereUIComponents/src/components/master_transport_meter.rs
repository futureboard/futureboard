//! Master level control in the transport bar — one strip that is both the
//! master meter and the master fader.
//!
//! Two things justify combining them rather than putting a meter beside a
//! slider. A master fader is only ever set *against* what the meter is showing,
//! so the value and the evidence for it belong on the same axis; and the
//! transport bar has one row of height to spend, which is not enough for a
//! meter and a rail stacked. The thumb is a 2 px rule rather than a block for
//! the same reason a real console fader cap is narrow: it must mark the value
//! without hiding the signal under it.
//!
//! This is a GPUI entity, not a function in `app_chrome`, because the master
//! meter moves at the audio-callback poll rate. Rendering it inside the chrome
//! would repaint the whole shell on every meter tick, which is exactly what
//! `DESIGN.md` forbids. `StudioLayout` owns it and pokes it from the meter
//! poll; the transport bar only embeds the view.

use std::sync::Arc;

use gpui::{
    div, px, App, AppContext, Context, DragMoveEvent, Empty, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};

use crate::components::timeline::timeline::Timeline;
use crate::components::timeline::timeline_state::volume;
use crate::components::timeline::vu_meter::meter_surface_horizontal;
use crate::theme::{radius, space, typography, Colors};

/// Overall width of the strip. Wide enough that one pixel is under a third of
/// a dB across the useful part of the taper, which is the resolution a master
/// trim is actually set at.
pub const MASTER_TRANSPORT_METER_WIDTH: f32 = 168.0;

/// Below this window width the strip is dropped rather than squeezed: a master
/// meter that has been compressed until its segments alias is worse than no
/// meter, and the mixer still has the full one.
pub const MASTER_TRANSPORT_METER_MIN_VIEWPORT: f32 = 1180.0;

/// Height of one channel bar, and the gap between the pair.
const BAR_H: f32 = 5.0;
const BAR_GAP: f32 = 1.0;
/// Inset of the meter inside its plate, so the fill never touches the border.
const PLATE_PAD_X: f32 = 5.0;
/// Half-width of the fader thumb, and therefore the travel inset: the value is
/// the thumb's centre, so it has to stop half a thumb short of each end.
const THUMB_HALF_W: f32 = 3.0;

/// The master-volume gesture, wired to the same preview/commit path the mixer's
/// master fader uses so both surfaces produce one engine update per frame and
/// one dirty flag per gesture.
#[derive(Clone)]
pub struct MasterTransportMeterCallbacks {
    pub on_drag_start: Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>,
    pub on_drag_preview: Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>,
    pub on_drag_commit: Arc<dyn Fn(&mut Window, &mut App) + 'static>,
    /// Double-click: back to unity. Takes the value so it shares the commit
    /// path rather than inventing a second way to set the master.
    pub on_reset: Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>,
}

/// Callbacks that do nothing, for the window between constructing the layout
/// and having an engine to talk to. The strip still draws the meter; the
/// gesture simply has nowhere to land yet.
pub fn inert_master_volume_callbacks() -> MasterTransportMeterCallbacks {
    let noop_value: Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static> = Arc::new(|_, _, _| {});
    MasterTransportMeterCallbacks {
        on_drag_start: noop_value.clone(),
        on_drag_preview: noop_value.clone(),
        on_drag_commit: Arc::new(|_, _| {}),
        on_reset: noop_value,
    }
}

/// Drag payload. Identity-only: the value is resolved from the pointer against
/// the strip's measured bounds on every move, so nothing here can go stale.
#[derive(Clone, Debug)]
pub struct MasterTransportFaderDrag;

impl Render for MasterTransportFaderDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub struct MasterTransportMeter {
    timeline: Entity<Timeline>,
    callbacks: MasterTransportMeterCallbacks,
    last_sig: u64,
}

impl MasterTransportMeter {
    pub fn new(timeline: Entity<Timeline>, callbacks: MasterTransportMeterCallbacks) -> Self {
        Self {
            timeline,
            callbacks,
            last_sig: u64::MAX,
        }
    }

    pub fn set_callbacks(&mut self, callbacks: MasterTransportMeterCallbacks) {
        self.callbacks = callbacks;
    }

    /// Meter poll tick. Repaints only when the quantised signature moves, so a
    /// silent master costs nothing and a moving one costs this entity alone.
    pub fn on_meter_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let sig = self.signature(cx);
        if sig == self.last_sig {
            return false;
        }
        self.last_sig = sig;
        cx.notify();
        true
    }

    fn signature(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let state = &self.timeline.read(cx).state;
        let master = &state.master;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
        q(master.meter_level_l).hash(&mut hasher);
        q(master.meter_level_r).hash(&mut hasher);
        q(master.meter_peak_hold_l).hash(&mut hasher);
        q(master.meter_peak_hold_r).hash(&mut hasher);
        master.meter_clip.hash(&mut hasher);
        q(state.display_master_volume()).hash(&mut hasher);
        hasher.finish()
    }
}

/// Pointer x -> normalized master volume, against the strip's own bounds.
///
/// Travel is inset by half a thumb at each end so that dragging to either
/// extreme puts the *thumb* there, not its edge. Shared with the tests below;
/// the drawing uses the same inset, which is what keeps the thumb under the
/// cursor for the whole gesture.
fn pointer_x_to_norm(pointer_x: f32, bounds_x: f32, bounds_w: f32) -> f32 {
    let rail_left = PLATE_PAD_X + THUMB_HALF_W;
    let rail_w = (bounds_w - 2.0 * rail_left).max(1.0);
    ((pointer_x - bounds_x - rail_left) / rail_w).clamp(0.0, 1.0)
}

/// Fraction across the strip's *travel* for a normalized value.
fn norm_to_travel_fraction(norm: f32) -> f32 {
    norm.clamp(0.0, 1.0)
}

impl Render for MasterTransportMeter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::perf::PerfScope::enter("MasterTransportMeter");
        crate::perf::count("master_transport_meter_paint_count", 1);

        self.last_sig = self.signature(cx);

        let state = &self.timeline.read(cx).state;
        let master = master_snapshot(state);
        let norm = state.display_master_volume().clamp(0.0, 1.0);
        let db_text = volume::format_db(norm);

        let cbs = self.callbacks.clone();
        let start = cbs.on_drag_start.clone();
        let preview = cbs.on_drag_preview.clone();
        let commit = cbs.on_drag_commit.clone();
        let reset = cbs.on_reset.clone();
        let unity_norm = volume::db_to_norm(0.0);

        // The thumb and the unity tick are positioned against the same travel
        // the pointer maps through, so a value set by dragging lands under the
        // cursor and a value of 0 dB lands on the tick.
        let travel_inset = PLATE_PAD_X + THUMB_HALF_W;
        let thumb_pct = norm_to_travel_fraction(norm);
        let unity_pct = norm_to_travel_fraction(unity_norm);

        let strip = div()
            .id("master-transport-strip")
            .relative()
            .flex_1()
            .min_w(px(64.0))
            .h(px(BAR_H * 2.0 + BAR_GAP + 8.0))
            .flex()
            .flex_col()
            .justify_center()
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .px(px(PLATE_PAD_X))
            .child(meter_surface_horizontal(
                master.level_l,
                master.level_r,
                master.hold_l,
                master.hold_r,
                master.clip,
                BAR_H,
                BAR_GAP,
            ))
            // Unity mark: 1 px, dim, and inset from both edges. The thumb is
            // 2 px, full bleed, at full strength, and carries grip caps — the
            // reference and the value differ in width, colour and shape, so
            // they cannot be read for one another on a lit meter.
            .child(
                div()
                    .absolute()
                    .top(px(1.0))
                    .bottom(px(1.0))
                    .left(gpui::relative(unity_pct))
                    .ml(px(travel_inset * (1.0 - 2.0 * unity_pct) - 0.5))
                    .w(px(1.0))
                    .bg(Colors::with_alpha(Colors::text_primary(), 0.28)),
            )
            .child(fader_thumb(travel_inset, thumb_pct))
            // The press itself never moves the master: a click on a meter is
            // how you read it, and jumping the mix to the click point would be
            // a destructive surprise mid-take. The gesture opens on the first
            // drag move, which is also when the preview snapshot is taken.
            .on_drag(
                MasterTransportFaderDrag,
                move |_drag, _offset, window, cx| {
                    start(&norm, window, cx);
                    cx.new(|_| MasterTransportFaderDrag)
                },
            )
            .on_drag_move::<MasterTransportFaderDrag>({
                let preview = preview.clone();
                move |event: &DragMoveEvent<MasterTransportFaderDrag>, window, cx| {
                    let x: f32 = event.event.position.x.into();
                    let ox: f32 = event.bounds.origin.x.into();
                    let ow: f32 = event.bounds.size.width.into();
                    let next = pointer_x_to_norm(x, ox, ow);
                    preview(&next, window, cx);
                }
            })
            .on_mouse_up(gpui::MouseButton::Left, {
                let commit = commit.clone();
                move |_event, window, cx| commit(window, cx)
            })
            .on_mouse_up_out(gpui::MouseButton::Left, {
                let commit = commit.clone();
                move |_event, window, cx| commit(window, cx)
            })
            .on_click(move |event, window, cx| {
                if event.click_count() >= 2 {
                    reset(&unity_norm, window, cx);
                }
            });

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::SNUG))
            .flex_none()
            .w(px(MASTER_TRANSPORT_METER_WIDTH))
            .h(px(crate::shell_metrics::TRANSPORT_CONTROL_HEIGHT))
            .px(px(space::SNUG))
            .rounded(px(radius::CONTROL))
            .bg(Colors::surface_canvas())
            .border(px(1.0))
            .border_color(Colors::border_normal())
            .child(
                div()
                    .flex_none()
                    .text_size(px(typography::UI_XS))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if master.clip {
                        Colors::status_error()
                    } else {
                        Colors::text_faint()
                    })
                    .child("MST"),
            )
            .child(strip)
            .child(
                div()
                    .flex_none()
                    .w(px(38.0))
                    .text_size(px(typography::UI_XS))
                    .text_color(Colors::text_secondary())
                    .child(db_text),
            )
    }
}

/// The value marker. A rule, not a block: it has to be findable against a lit
/// meter without covering the level it is being set against.
fn fader_thumb(travel_inset: f32, pct: f32) -> impl IntoElement {
    let cap = || {
        div()
            .flex_none()
            .w(px(THUMB_HALF_W * 2.0))
            .h(px(2.0))
            .bg(Colors::text_primary())
    };
    div()
        .absolute()
        .top(px(0.0))
        .bottom(px(0.0))
        // Centre the marker on `travel_inset + pct * (w - 2 * travel_inset)`,
        // expressed against the parent's width so it tracks a flexible strip:
        // the fraction carries `pct * w` and the margin carries the rest.
        .left(gpui::relative(pct))
        .ml(px(travel_inset * (1.0 - 2.0 * pct) - THUMB_HALF_W))
        .w(px(THUMB_HALF_W * 2.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_between()
        .child(cap())
        .child(
            div()
                .absolute()
                .top(px(0.0))
                .bottom(px(0.0))
                .w(px(2.0))
                .bg(Colors::text_primary()),
        )
        .child(cap())
}

struct MasterMeterSnapshot {
    level_l: f32,
    level_r: f32,
    hold_l: f32,
    hold_r: f32,
    clip: bool,
}

fn master_snapshot(
    state: &crate::components::timeline::timeline_state::TimelineState,
) -> MasterMeterSnapshot {
    let m = &state.master;
    MasterMeterSnapshot {
        level_l: m.meter_level_l,
        level_r: m.meter_level_r,
        hold_l: m.meter_peak_hold_l,
        hold_r: m.meter_peak_hold_r,
        clip: m.meter_clip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pointer maps through the thumb's travel, not the plate's outer box:
    /// dragging to the far right has to reach unity+6 dB, and dragging to the
    /// far left has to reach silence, with the thumb still fully drawn.
    #[test]
    fn pointer_mapping_spans_the_thumb_travel() {
        let w = MASTER_TRANSPORT_METER_WIDTH;
        let inset = PLATE_PAD_X + THUMB_HALF_W;
        assert!((pointer_x_to_norm(inset, 0.0, w) - 0.0).abs() < 1.0e-6);
        assert!((pointer_x_to_norm(w - inset, 0.0, w) - 1.0).abs() < 1.0e-6);
        assert!((pointer_x_to_norm(w * 0.5, 0.0, w) - 0.5).abs() < 1.0e-6);
    }

    /// A pointer outside the strip clamps rather than running the master past
    /// its ends.
    #[test]
    fn pointer_mapping_clamps_outside_the_strip() {
        let w = MASTER_TRANSPORT_METER_WIDTH;
        assert_eq!(pointer_x_to_norm(-500.0, 0.0, w), 0.0);
        assert_eq!(pointer_x_to_norm(500.0, 0.0, w), 1.0);
    }

    /// The strip is offset inside the window; the mapping is relative to its
    /// own origin, or the master jumps by the chrome's left gutter.
    #[test]
    fn pointer_mapping_is_relative_to_the_strips_origin() {
        let w = MASTER_TRANSPORT_METER_WIDTH;
        let origin = 640.0;
        let inset = PLATE_PAD_X + THUMB_HALF_W;
        assert!((pointer_x_to_norm(origin + inset, origin, w) - 0.0).abs() < 1.0e-6);
        assert!((pointer_x_to_norm(origin + w - inset, origin, w) - 1.0).abs() < 1.0e-6);
    }

    /// Unity has to land somewhere a user can actually hit — the taper puts
    /// 0 dB well inside the strip, not against its right edge.
    #[test]
    fn unity_sits_inside_the_travel() {
        let unity = volume::db_to_norm(0.0);
        assert!(
            unity > 0.5 && unity < 1.0,
            "0 dB should sit in the upper half of the taper, got {unity}"
        );
    }
}
