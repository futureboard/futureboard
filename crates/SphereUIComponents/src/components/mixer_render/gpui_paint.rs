//! GPUI-paint mixer backend — the working GPU-batched primitive layer.
//!
//! Replaces the dense per-strip decoration `div`s with a single `canvas` that
//! issues batched `window.paint_quad` calls (GPUI rasterises quads on the GPU).
//! Static geometry (backgrounds, accent bars, separators, selection) is cached
//! and only rebuilt when [`MixerRenderSnapshot::static_key`] changes; the dynamic
//! batch (hover, future meters) is rebuilt per frame. Mirrors the timeline's
//! [`crate::components::timeline::render::gpui_paint`].

use std::sync::Arc;

use gpui::{canvas, fill, point, px, size, Bounds, IntoElement, Pixels, Rgba, Styled};

use super::renderer::{MixerRenderOutput, MixerRenderer};
use super::snapshot::{MixerRenderSnapshot, MixerStripGeom};
use crate::theme::Colors;

/// One batched rectangle. `scrolls` distinguishes channel strips (shifted by
/// `scroll_x`) from the pinned master (drawn at a fixed panel-local x).
#[derive(Clone, Copy)]
struct Quad {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Rgba,
    scrolls: bool,
}

pub struct GpuiPaintMixerRenderer {
    static_key: Option<u64>,
    static_quads: Arc<Vec<Quad>>,
    /// Reused dynamic scratch buffer — cleared + refilled, not reallocated.
    dynamic_scratch: Vec<Quad>,
    last_meter_sig: u64,
    static_rebuild_count: u64,
    dynamic_update_count: u64,
    meter_update_count: u64,
}

impl GpuiPaintMixerRenderer {
    pub fn new() -> Self {
        Self {
            static_key: None,
            static_quads: Arc::new(Vec::new()),
            dynamic_scratch: Vec::new(),
            last_meter_sig: u64::MAX,
            static_rebuild_count: 0,
            dynamic_update_count: 0,
            meter_update_count: 0,
        }
    }

    fn rebuild_static(&mut self, snapshot: &MixerRenderSnapshot) {
        let mut quads: Vec<Quad> = Vec::with_capacity(snapshot.strips.len() * 4 + 4);
        for strip in &snapshot.strips {
            push_static_strip(&mut quads, strip, snapshot.plate_h, snapshot.separator_w);
        }
        if let Some(master) = &snapshot.master {
            push_static_strip(&mut quads, master, snapshot.plate_h, snapshot.separator_w);
        }
        self.static_quads = Arc::new(quads);
    }

    fn rebuild_dynamic(&mut self, snapshot: &MixerRenderSnapshot) {
        self.dynamic_scratch.clear();
        let hover = Colors::surface_hover();
        for strip in snapshot.strips.iter().chain(snapshot.master.iter()) {
            if strip.hovered && !strip.selected {
                self.dynamic_scratch.push(Quad {
                    x: strip.x,
                    y: 0.0,
                    w: strip.width,
                    h: strip.height,
                    color: hover,
                    scrolls: !strip.is_master,
                });
            }
        }
    }
}

impl MixerRenderer for GpuiPaintMixerRenderer {
    fn backend_name(&self) -> &'static str {
        "gpui-paint"
    }

    fn render(&mut self, snapshot: &MixerRenderSnapshot) -> MixerRenderOutput {
        let layout_start = std::time::Instant::now();
        let _s = crate::perf::PerfScope::enter("MixerSnapshotBuild");

        if self.static_key != Some(snapshot.static_key) {
            self.rebuild_static(snapshot);
            self.static_key = Some(snapshot.static_key);
            self.static_rebuild_count = self.static_rebuild_count.saturating_add(1);
            crate::perf::count(
                "mixer_static_batch_rebuild_count",
                self.static_rebuild_count,
            );
            if crate::perf::mixer_gpu_debug_enabled() {
                eprintln!(
                    "[mixer-gpu] static rebuild key={} quads={}",
                    snapshot.static_key,
                    self.static_quads.len()
                );
            }
        }

        self.rebuild_dynamic(snapshot);
        self.dynamic_update_count = self.dynamic_update_count.saturating_add(1);
        crate::perf::count(
            "mixer_dynamic_snapshot_update_count",
            self.dynamic_update_count,
        );

        let meter_sig = snapshot.meter_signature();
        if meter_sig != self.last_meter_sig {
            self.last_meter_sig = meter_sig;
            self.meter_update_count = self.meter_update_count.saturating_add(1);
            crate::perf::count("meter_buffer_update_count", self.meter_update_count);
        }

        crate::perf::count("gpu_enabled", 1);
        crate::perf::count("gpu_draw_call_count", 1); // one batched canvas pass
        crate::perf::count(
            "gpu_instance_count",
            (self.static_quads.len() + self.dynamic_scratch.len()) as u64,
        );
        // `*_ms` counters are recorded in integer microseconds.
        crate::perf::count(
            "cpu_layout_time_ms",
            layout_start.elapsed().as_micros() as u64,
        );

        // `static_quads` is an Arc — cloning is a refcount bump, not a copy. The
        // dynamic batch is tiny (hover only in this slice; usually empty); the
        // persistent `dynamic_scratch` keeps its capacity for building and we hand
        // a small snapshot to the deferred paint closure.
        let static_quads = self.static_quads.clone();
        let dynamic_quads = Arc::new(self.dynamic_scratch.clone());
        let scroll_x = snapshot.viewport.scroll_x;
        let channel_w = snapshot.viewport.channel_area_width;

        let element = canvas(
            |_bounds, _window, _cx| {},
            move |bounds, (), window, _cx| {
                let paint_start = std::time::Instant::now();
                let _p = crate::perf::PerfScope::enter("MixerGpuPaint");
                paint_quads(&static_quads, bounds, scroll_x, channel_w, window);
                paint_quads(&dynamic_quads, bounds, scroll_x, channel_w, window);
                let micros = paint_start.elapsed().as_micros() as u64;
                crate::perf::count("gpu_frame_time_ms", micros);
                crate::perf::count("cpu_paint_time_ms", micros);
            },
        )
        .absolute()
        .inset_0()
        .into_any_element();

        MixerRenderOutput::Gpui(element)
    }
}

fn push_static_strip(quads: &mut Vec<Quad>, strip: &MixerStripGeom, plate_h: f32, sep_w: f32) {
    let scrolls = !strip.is_master;
    // Background.
    quads.push(Quad {
        x: strip.x,
        y: 0.0,
        w: strip.width,
        h: strip.height,
        color: strip.bg,
        scrolls,
    });
    // Name plate at the foot of the strip — the strip's one piece of colour.
    // The div layer draws the number and name over this quad and paints no fill
    // of its own, so the two layers can never disagree about the colour.
    quads.push(Quad {
        x: strip.x,
        y: (strip.height - plate_h).max(0.0),
        w: strip.width,
        h: plate_h.min(strip.height),
        color: strip.plate,
        scrolls,
    });
    // Right separator line (already resolved stronger when selected).
    quads.push(Quad {
        x: strip.x + strip.width - sep_w,
        y: 0.0,
        w: sep_w,
        h: strip.height,
        color: strip.separator,
        scrolls,
    });
}

/// Paint a quad batch, applying the scroll transform and clamping scrolling quads
/// to the channel viewport so strip decoration never bleeds over the gutter or
/// pinned master.
fn paint_quads(
    quads: &[Quad],
    bounds: Bounds<Pixels>,
    scroll_x: f32,
    channel_w: f32,
    window: &mut gpui::Window,
) {
    for q in quads {
        let mut left = if q.scrolls { q.x - scroll_x } else { q.x };
        let mut right = left + q.w;
        if q.scrolls {
            left = left.max(0.0);
            right = right.min(channel_w);
            if right <= left {
                continue;
            }
        }
        let rect = Bounds::new(
            bounds.origin + point(px(left), px(q.y)),
            size(px((right - left).max(0.0)), px(q.h.max(0.0))),
        );
        window.paint_quad(fill(rect, q.color));
    }
}
