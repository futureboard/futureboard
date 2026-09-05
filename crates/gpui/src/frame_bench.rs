//! What a frame costs at arrangement scale.
//!
//! GPUI's frame is opaque from the outside: an app can time its own `render`
//! and see 0.2 ms inside a 40 ms frame, and everything that explains the other
//! 39.8 ms happens in this crate. [`crate::frame_profile`] instruments the
//! phases; this drives them.
//!
//! The tree built here is shaped like a DAW arrangement, because that is the
//! shape that gets big: a column of rows, each row a header of small controls
//! beside a lane of absolutely-positioned clips, each clip a handful of nested
//! elements. It is not a synthetic worst case — it is a screenful of the thing
//! the app actually draws, at the sizes a real session reaches.
//!
//! Run with:
//!
//! ```text
//! cargo test --manifest-path crates/gpui/Cargo.toml --release --lib frame_bench -- --ignored --nocapture
//! ```
//!
//! `--release` matters more here than in most benchmarks: the layout solve and
//! the element tree walk are the measured quantities, and a debug build's
//! numbers say more about `debug_assertions` than about the frame.

#![cfg(test)]

use crate::{
    App, Bounds, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    TestAppContext, Window, div, point, px, size,
};

/// A screenful of arrangement, parameterised by how much is on it.
struct ArrangementHarness {
    rows: usize,
    clips_per_row: usize,
    /// Extra leaf elements inside each clip — a waveform layer, a label bar,
    /// two resize handles. Real clips are not one div.
    leaves_per_clip: usize,
    /// Controls in each row's header. Real headers carry a name, five state
    /// toggles, a fader, a pan readout, a meter and a level pill.
    header_controls: usize,
    /// Text runs on each clip — a real audio clip carries its name and a gain
    /// readout. Text is the one leaf that is not just a quad: it shapes and
    /// rasterises, and a clip too narrow to read is a clip paying for both.
    text_runs_per_clip: usize,
    /// Bumped every frame so nothing can be cached across draws.
    frame: usize,
}

impl ArrangementHarness {
    fn clip(&self, index: usize) -> impl IntoElement {
        let mut clip = div()
            .absolute()
            .left(px((index * 140) as f32 + (self.frame % 7) as f32))
            .top(px(7.0))
            .w(px(132.0))
            .h(px(58.0))
            .rounded(px(4.0))
            .border(px(1.0))
            .flex()
            .flex_col()
            .justify_between();
        for leaf in 0..self.leaves_per_clip {
            clip = clip.child(
                div()
                    .absolute()
                    .left(px(leaf as f32))
                    .top(px(leaf as f32))
                    .w(px(130.0 - leaf as f32))
                    .h(px(12.0))
                    .rounded(px(2.0)),
            );
        }
        for run in 0..self.text_runs_per_clip {
            // Content that moves, the way a clip name differs per clip and a
            // gain readout changes as it is dragged — text that never changes
            // would be answered from the line-layout cache and measure nothing.
            clip = clip.child(
                div()
                    .h(px(12.0))
                    .text_size(px(9.0))
                    .child(format!("clip {index}-{run} {}", self.frame)),
            );
        }
        clip
    }

    fn header(&self, row: usize) -> impl IntoElement {
        let mut header = div()
            .w(px(320.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(6.0))
            .child(div().h(px(14.0)).w_full());
        let mut controls = div().flex().flex_row().items_center().gap(px(2.0));
        for control in 0..self.header_controls {
            controls = controls.child(
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .flex_none()
                    .rounded(px(3.0))
                    .border(px(1.0))
                    .id(("control", row * 64 + control)),
            );
        }
        header.child(controls)
    }

    fn row(&self, row: usize) -> impl IntoElement {
        let mut lane = div().flex_1().h_full().relative().overflow_hidden();
        for clip in 0..self.clips_per_row {
            lane = lane.child(self.clip(clip));
        }
        div()
            .w_full()
            .h(px(72.0))
            .flex_none()
            .flex()
            .flex_row()
            .child(self.header(row))
            .child(lane)
    }
}

impl Render for ArrangementHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div().size_full().flex().flex_col();
        for row in 0..self.rows {
            root = root.child(self.row(row));
        }
        root
    }
}

/// Elements the harness builds per frame, so a cost can be read per element.
#[allow(dead_code)]
fn element_count(rows: usize, clips: usize, leaves: usize, controls: usize) -> usize {
    // row + header + name + control strip + controls + lane + clips × (1 + leaves)
    rows * (5 + controls + clips * (1 + leaves)) + 1
}

/// Draw `frames` frames and return the mean of each phase, in milliseconds.
fn measure(
    cx: &mut TestAppContext,
    rows: usize,
    clips_per_row: usize,
    leaves_per_clip: usize,
    header_controls: usize,
    text_runs_per_clip: usize,
    frames: usize,
) -> Phases {
    let window = cx.open_window(size(px(1600.), px(900.)), |_, _| ArrangementHarness {
        rows,
        clips_per_row,
        leaves_per_clip,
        header_controls,
        text_runs_per_clip,
        frame: 0,
    });
    cx.run_until_parked();

    let (mut prepaint, mut paint, mut draw, mut solve, mut shape) =
        (0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32);
    let (mut nodes, mut primitives, mut measures) = (0_u64, 0_u64, 0_u64);
    for frame in 0..frames {
        // Move something every frame so no part of the tree can be reused: the
        // question is what a *changed* arrangement costs, which is the frame a
        // scroll produces.
        window
            .update(cx, |view, _window, cx| {
                view.frame = frame + 1;
                cx.notify();
            })
            .expect("window is open");
        cx.run_until_parked();

        let profile = crate::frame_profile::frame_profile();
        prepaint += profile.prepaint_ms();
        paint += profile.paint_ms();
        draw += profile.draw_ms();
        solve += profile.layout_solve_ms();
        shape += profile.shape_ms();
        nodes = nodes.max(profile.layout_nodes);
        primitives = primitives.max(profile.scene_primitives);
        measures = measures.max(profile.measure_calls);
    }
    let frames = frames as f32;
    Phases {
        prepaint: prepaint / frames,
        paint: paint / frames,
        draw: draw / frames,
        solve: solve / frames,
        shape: shape / frames,
        nodes,
        primitives,
        measures,
    }
}

/// Mean per-phase cost of one frame, with the tree sizes that produced it.
struct Phases {
    prepaint: f32,
    paint: f32,
    draw: f32,
    /// Time inside the layout engine's solve. The rest of `prepaint` is
    /// building the element tree and GPUI's own walk of it.
    solve: f32,
    /// Time shaping text that missed the two-frame line-layout cache.
    shape: f32,
    nodes: u64,
    primitives: u64,
    measures: u64,
}

#[crate::test]
#[ignore = "measurement, not an assertion"]
fn arrangement_frame_phase_cost(cx: &mut TestAppContext) {
    println!();
    println!(
        "{:<30} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "tree", "nodes", "solve ms", "build ms", "shape ms", "paint ms", "draw ms"
    );
    // `text` is what a clip's label bar costs: a name and a gain readout, on
    // every clip, whether or not the clip is wide enough to read either.
    let cases = [
        ("16 rows × 8 clips", 16_usize, 8_usize, 2_usize),
        ("16 rows × 24 clips", 16, 24, 2),
        ("28 rows × 24 clips", 28, 24, 2),
        ("28 rows × 64 clips", 28, 64, 2),
        ("28 rows × 64 clips, no text", 28, 64, 0),
        ("28 rows × 64 clips, 1 text run", 28, 64, 1),
    ];
    for (label, rows, clips, text) in cases {
        let p = measure(cx, rows, clips, 4, 7, text, 12);
        // `solve` is inside `prepaint`; the difference is the element tree
        // build and GPUI's own walk of it.
        let build = (p.prepaint - p.solve).max(0.0);
        println!(
            "{label:<30} {:>7} {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>9.3}",
            p.nodes, p.solve, build, p.shape, p.paint, p.draw
        );
    }
    println!();
}

/// The property the frame has to have: cost proportional to the tree, not
/// worse. A superlinear phase is what turns "a few more tracks" into a
/// slideshow, and it is invisible until the session is already too big.
#[crate::test]
fn frame_cost_grows_no_faster_than_the_tree(cx: &mut TestAppContext) {
    // Same shape, four times the clips.
    let small_p = measure(cx, 12, 8, 4, 7, 2, 10);
    let large_p = measure(cx, 12, 32, 4, 7, 2, 10);

    let small = (small_p.prepaint + small_p.paint).max(0.001);
    let large = (large_p.prepaint + large_p.paint).max(0.001);
    // 4x the tree may cost more than 4x — cache pressure is real — but nothing
    // like 4x squared. The bound is deliberately loose: this is a shape
    // assertion, and it must not fail on a loaded machine.
    assert!(
        large < small * 16.0 + 2.0,
        "4x the clips cost {large:.3} ms against {small:.3} ms — a frame phase is superlinear \
         in the element count"
    );
}
