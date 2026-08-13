//! Arrangement background surface (grid + bar shades) via renderer abstraction.

use std::cell::RefCell;

use gpui::{div, IntoElement, Styled};

use crate::components::timeline::render::{
    create_timeline_renderer_with_fallback, gpui_paint, renderer::TimelineRenderOutput,
    snapshot::SnapshotBuildOptions, snapshot::TimelineRenderSnapshot, TimelineRenderer,
    TimelineRendererBackend,
};
use crate::components::timeline::timeline_state::{TimelineState, TrackRowLayout};

thread_local! {
    static TIMELINE_RENDERER: RefCell<Option<(TimelineRendererBackend, Box<dyn TimelineRenderer>)>> =
        RefCell::new(None);
}

fn render_arrangement_with(
    snapshot: &TimelineRenderSnapshot,
    renderer: &mut dyn TimelineRenderer,
    backend: TimelineRendererBackend,
) -> gpui::AnyElement {
    gpui_paint::log_snapshot_stats(snapshot, backend.label());
    match renderer.render_arrangement(snapshot) {
        TimelineRenderOutput::Gpui(element) => element,
        #[cfg(feature = "gpu-renderer")]
        TimelineRenderOutput::WgpuOffscreen(frame) => {
            // This branch is retained for the future texture-composite path.
            // Today `create_timeline_renderer_with_fallback` keeps WGPU disabled
            // for user-visible timeline paint because this offscreen texture is
            // not yet composited into GPUI.
            let _ = frame;
            div().absolute().inset_0().into_any_element()
        }
    }
}

fn with_renderer(snapshot: &TimelineRenderSnapshot) -> gpui::AnyElement {
    if TIMELINE_RENDERER.try_with(|_| ()).is_err() {
        let preferred = TimelineRendererBackend::from_env();
        let (mut renderer, backend) = create_timeline_renderer_with_fallback(preferred);
        return render_arrangement_with(snapshot, renderer.as_mut(), backend);
    }
    TIMELINE_RENDERER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let preferred = TimelineRendererBackend::from_env();
            let (renderer, backend) = create_timeline_renderer_with_fallback(preferred);
            *slot = Some((backend, renderer));
        }
        let (backend, renderer) = slot.as_mut().expect("renderer slot");
        render_arrangement_with(snapshot, renderer.as_mut(), *backend)
    })
}

/// Eagerly construct the thread-local timeline renderer — and, for the WGPU
/// backend, create the GPU adapter/device — so the first studio frame doesn't
/// stall on initialization. Call on the main UI thread (the thread that paints
/// the studio) during the loading screen; subsequent calls are no-ops. Returns
/// the active backend label for status display.
///
/// The renderer preference (CPU vs GPU) must already be applied via
/// `set_preferred_backend` / `set_preferred_gpu_device_id` before this runs.
pub fn warm_up_timeline_renderer() -> &'static str {
    TIMELINE_RENDERER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let preferred = TimelineRendererBackend::from_env();
            let (renderer, backend) = create_timeline_renderer_with_fallback(preferred);
            *slot = Some((backend, renderer));
        }
        slot.as_ref().expect("renderer slot").0.label()
    })
}

pub fn active_timeline_renderer_backend() -> &'static str {
    TIMELINE_RENDERER
        .try_with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|(backend, _)| backend.label())
                .unwrap_or_else(|| TimelineRendererBackend::from_env().label())
        })
        .unwrap_or("unknown")
}

/// Scrollable arrangement grid layer (behind track lanes / clips).
///
/// `row_layout` is the frame's arrangement geometry, shared with the track list
/// so a single O(track_count) build serves the whole timeline repaint.
pub fn timeline_surface(
    state: &TimelineState,
    row_layout: &TrackRowLayout,
    grid_width: f32,
    grid_height: f32,
) -> impl IntoElement {
    let _s = crate::perf::PerfScope::enter("TimelineSurface");

    let mut options = SnapshotBuildOptions::default();
    options.scale_factor = 1.0;

    let mut snapshot = TimelineRenderSnapshot::from_row_layout(state, row_layout, options);
    snapshot.viewport.width = grid_width.max(1.0);
    snapshot.viewport.height = grid_height.max(1.0);

    with_renderer(&snapshot)
}
