//! Shared marker flag for the conductor lanes (tempo, time signature, and any
//! future arrangement marker).
//!
//! A marker denotes an event that begins **at** a beat and stays in force until
//! the next one. The lanes used to draw it as a pill centred on that beat, which
//! reads as "this value is centred here" and puts half the chip in the region
//! *before* the change. The flag is anchored with its left edge on the beat and
//! tapers to a point on the right, so the shape itself says "starts here, runs
//! that way" — the same reason a real score puts the signature at the barline.
//!
//! Every flag in a lane is painted by ONE canvas rather than one element each:
//! the shapes are a handful of polygons, and the label is the only part that has
//! to be a real text element.

use gpui::{
    canvas, div, fill, point, px, Bounds, IntoElement, ParentElement, PathBuilder, Pixels, Rgba,
    Styled,
};

use crate::theme::{typography, Colors};

/// Drawn height of a flag body.
pub const MARKER_FLAG_H: f32 = 18.0;
/// Horizontal run of the tapered right edge.
pub const MARKER_TAIL_W: f32 = 7.0;
/// Space between the beat line and the first glyph.
const MARKER_PAD_L: f32 = 6.0;
/// Space between the last glyph and where the taper begins.
const MARKER_PAD_R: f32 = 4.0;
const MARKER_LABEL_SIZE: f32 = 10.0;
/// Narrowest body that still reads as a flag rather than a wedge.
const MARKER_MIN_W: f32 = 26.0;

/// One marker to draw. `x` is the beat's position in lane-local pixels.
#[derive(Clone)]
pub struct MarkerFlag {
    pub x: f32,
    pub label: String,
    pub selected: bool,
}

impl MarkerFlag {
    /// Total body width including the taper.
    pub fn width(&self) -> f32 {
        // `estimate_label_width` is calibrated for 11px and is script-aware, so
        // Thai/CJK marker labels reserve the room they actually need instead of
        // being truncated by a flat chars * n figure.
        let text = crate::theme::menu::estimate_label_width(&self.label)
            * (MARKER_LABEL_SIZE / typography::UI_XS);
        (text + MARKER_PAD_L + MARKER_PAD_R + MARKER_TAIL_W).max(MARKER_MIN_W)
    }
}

fn flag_fill(selected: bool) -> Rgba {
    if selected {
        Colors::accent_primary()
    } else {
        // A lifted neutral, not an accent wash: cyan already means selection in
        // this lane, and a lane full of accent-tinted chips would make the one
        // selected marker impossible to pick out.
        Colors::surface_hover()
    }
}

fn flag_text(selected: bool) -> Rgba {
    if selected {
        Colors::text_inverse()
    } else {
        Colors::text_primary()
    }
}

/// Paints every flag body plus its stem in a single canvas, and returns the
/// label elements to be rendered on top.
///
/// `lane_h` is the full lane height; the flag is vertically centred and the stem
/// runs from the flag's bottom edge to the lane floor so the exact beat stays
/// findable when the label is clamped away from it.
pub fn marker_flag_layer(
    flags: Vec<MarkerFlag>,
    lane_w: f32,
    lane_h: f32,
) -> (impl IntoElement, Vec<gpui::Div>) {
    let top = ((lane_h - MARKER_FLAG_H) * 0.5).max(1.0);
    let stem_color = Colors::with_alpha(Colors::text_primary(), 0.22);

    let shapes: Vec<(f32, f32, bool)> =
        flags.iter().map(|f| (f.x, f.width(), f.selected)).collect();

    let layer = canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            for (x, w, selected) in &shapes {
                let (x, w, selected) = (*x, *w, *selected);
                // Stem first, so the flag body covers its own overlap.
                let stem = Bounds::new(
                    bounds.origin + point(px(x), px(top + MARKER_FLAG_H)),
                    gpui::size(px(1.0), px((lane_h - top - MARKER_FLAG_H).max(0.0))),
                );
                window.paint_quad(fill(stem, stem_color));

                let body_r = x + w - MARKER_TAIL_W;
                let mut path = PathBuilder::fill();
                path.add_polygon(
                    &[
                        bounds.origin + point(px(x), px(top)),
                        bounds.origin + point(px(body_r), px(top)),
                        bounds.origin + point(px(x + w), px(top + MARKER_FLAG_H * 0.5)),
                        bounds.origin + point(px(body_r), px(top + MARKER_FLAG_H)),
                        bounds.origin + point(px(x), px(top + MARKER_FLAG_H)),
                    ],
                    true,
                );
                if let Ok(path) = path.build() {
                    window.paint_path(path, flag_fill(selected));
                }
            }
        },
    )
    .absolute()
    .inset_0();

    let labels = flags
        .into_iter()
        .filter_map(|f| {
            let w = f.width();
            // Off-screen flags contribute nothing but layout cost.
            if f.x + w < 0.0 || f.x > lane_w {
                return None;
            }
            let text_w = (w - MARKER_TAIL_W - MARKER_PAD_L - MARKER_PAD_R).max(1.0);
            Some(
                div()
                    .absolute()
                    .left(px(f.x + MARKER_PAD_L))
                    .top(px(top))
                    .w(px(text_w))
                    .h(px(MARKER_FLAG_H))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .truncate()
                    .whitespace_nowrap()
                    .text_size(px(MARKER_LABEL_SIZE))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(flag_text(f.selected))
                    .child(f.label),
            )
        })
        .collect();

    (layer, labels)
}

/// Pointer slop to the *left* of a flag's beat line, in lane pixels.
///
/// Only the left needs slop: the drawn body already runs to the right of the
/// beat, so `flag_hit_index` covers that side by shape rather than by guess.
pub const MARKER_FLAG_HIT_SLOP: f32 = 6.0;

/// Index of the flag whose drawn body contains `lane_x`.
///
/// The conductor lanes used to resolve a hit from a symmetric beat tolerance
/// around the marker's beat, which is not the shape that is on screen: a flag
/// is a 26–160 px body extending *right* from its beat line, so a click on the
/// label — the obvious place to aim — landed outside the tolerance and read as
/// "empty lane", clearing the selection and moving the playhead instead.
///
/// Scanned back to front so that where two flags overlap you get the one
/// painted on top, which is the one you can see.
pub fn flag_hit_index(spans: &[(f32, f32)], lane_x: f32, slop: f32) -> Option<usize> {
    spans
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (x, w))| lane_x >= x - slop && lane_x <= x + w)
        .map(|(index, _)| index)
}
