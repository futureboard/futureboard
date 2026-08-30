use crate::theme::Colors;
use gpui::{
    canvas, div, fill, px, size, svg, Bounds, Context, IntoElement, ParentElement, Pixels, Render,
    Styled, Window,
};

/// `FUTUREBOARD_PLAYHEAD_LAYER_DEBUG=1` — trace the playhead body layer.
/// Cached: this is read inside the per-frame paint closure, which runs on
/// every repaint while the transport is playing.
fn playhead_layer_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_PLAYHEAD_LAYER_DEBUG").is_some())
}

/// Content-viewport playhead line (no head/marker) at a precomputed x.
pub fn playhead_line_at(x: f32) -> impl IntoElement {
    let _s = crate::perf::PerfScope::enter("PlayheadLine");
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(x))
        .w(px(1.0))
        .bg(Colors::timeline_playhead())
}

/// Ruler-only playhead head/marker (no vertical line) at a precomputed x.
pub fn playhead_head_at(x: f32) -> impl IntoElement {
    let _s = crate::perf::PerfScope::enter("PlayheadHead");
    svg()
        .path(crate::assets::ICON_PLAYHEAD_HANDLE_PATH)
        .absolute()
        .top_0()
        .left(px(x - 5.5))
        .w(px(12.0))
        .h(px(12.0))
        .text_color(Colors::timeline_playhead())
}

/// Dedicated foreground overlay: playhead vertical body line.
/// Rendered after grid/content so it cannot be covered.
pub fn playhead_body_overlay_at(x: f32) -> impl IntoElement {
    let color = Colors::timeline_playhead();

    // Canvas draws the body-only line in a dedicated paint layer.
    let line = canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let w: f32 = bounds.size.width.into();
            let h: f32 = bounds.size.height.into();
            if x < -2.0 || x > w + 2.0 || h <= 0.0 {
                return;
            }

            if playhead_layer_debug_enabled() {
                eprintln!("[playhead body] x={x:.1} w={w:.1} h={h:.1}");
            }

            window.paint_layer(bounds, |window| {
                let line_bounds = Bounds::new(
                    bounds.origin + gpui::point(px(x), px(0.0)),
                    size(px(1.0), px(h.max(0.0))),
                );
                window.paint_quad(fill(line_bounds, color));
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element();

    div().absolute().inset_0().child(line)
}

pub fn playhead_head_overlay_at(x: f32) -> impl IntoElement {
    let color = Colors::timeline_playhead();
    let y = 2.0; // keep the head inside the ruler strip
    div().absolute().inset_0().child(
        svg()
            .path(crate::assets::ICON_PLAYHEAD_HANDLE_PATH)
            .absolute()
            .top(px(y))
            .left(px(x - 5.5))
            .w(px(12.0))
            .h(px(12.0))
            .text_color(color),
    )
}

/// Where the playhead is this frame, shared between the arrangement and the
/// overlay that draws it.
///
/// A cell rather than an entity field so `Timeline::render` can refresh it
/// without leasing the overlay: the arrangement recomputes x whenever it lays
/// itself out (a scroll, a zoom, a resize), and the playback poll recomputes it
/// between those.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayheadFrame {
    /// x within the lane column, in the same space `beats_to_x` returns.
    pub x: f32,
}

pub type PlayheadFrameCell = std::rc::Rc<std::cell::Cell<PlayheadFrame>>;

/// The playhead as its own GPUI entity.
///
/// The line moves on every playback tick — up to the display refresh rate — and
/// it used to move by notifying the whole `Timeline`, which rebuilt the entire
/// arrangement (row layout, every lane, every clip, every waveform, every
/// label) for a one-pixel translation. Playing a project therefore cost a full
/// arrangement rebuild per frame, and it is why the interface stuttered while
/// the audio engine sat near idle.
///
/// GPUI invalidates per entity, so the fix is an entity: notifying this one
/// repaints the line and nothing else. It carries no state of its own beyond
/// the shared frame — the geometry is the arrangement's, and duplicating it
/// here would be a second coordinate system to keep in step.
pub struct PlayheadOverlay {
    frame: PlayheadFrameCell,
    /// y of the first row under the ruler. The head is clipped above it and the
    /// body starts at it.
    ruler_height: f32,
    /// Left edge of the lane column — the track headers keep their own strip.
    header_width: f32,
}

impl PlayheadOverlay {
    pub fn new(frame: PlayheadFrameCell, ruler_height: f32, header_width: f32) -> Self {
        Self {
            frame,
            ruler_height,
            header_width,
        }
    }
}

impl Render for PlayheadOverlay {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::perf::PerfScope::enter("PlayheadOverlay");
        crate::perf::count("playhead_overlay_paint_count", 1);
        let x = self.frame.get().x;
        div()
            .absolute()
            .left(px(self.header_width))
            .right_0()
            .top_0()
            .bottom_0()
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top_0()
                    .h(px(self.ruler_height))
                    .overflow_hidden()
                    .child(playhead_head_overlay_at(x)),
            )
            .child(
                // From directly under the ruler, so the line stays continuous
                // through the conductor lanes instead of restarting below them.
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(self.ruler_height))
                    .bottom_0()
                    .overflow_hidden()
                    .child(playhead_body_overlay_at(x)),
            )
    }
}
