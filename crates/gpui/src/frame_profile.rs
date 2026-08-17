//! Coarse per-frame timings for the host application's profiler HUD.
//!
//! GPUI's frame is opaque to an embedder: an app can time its own `render`
//! functions, but everything after that — building the element tree's layout,
//! prepaint, paint, and handing the scene to the platform — happens inside this
//! crate. When an app measures 0.2 ms of its own work inside a 40 ms frame, the
//! missing 39.8 ms is here, and without a breakdown there is nothing to act on.
//!
//! These are plain relaxed atomics written once per phase per frame on the
//! window thread, so the cost is negligible and readers never block. Values are
//! microseconds from the most recently completed frame.

use std::sync::atomic::{AtomicU64, Ordering};

static DRAW_US: AtomicU64 = AtomicU64::new(0);
static PRESENT_US: AtomicU64 = AtomicU64::new(0);
static SCENE_PRIMITIVES: AtomicU64 = AtomicU64::new(0);
/// Accumulated during the frame in progress.
static SHAPE_US_ACC: AtomicU64 = AtomicU64::new(0);
static SHAPE_MISSES_ACC: AtomicU64 = AtomicU64::new(0);
/// Snapshotted from the accumulators when the frame ends.
static SHAPE_US: AtomicU64 = AtomicU64::new(0);
static SHAPE_MISSES: AtomicU64 = AtomicU64::new(0);

/// One frame's phase timings, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameProfile {
    /// Element tree render + prepaint + paint (everything `Window::draw` does).
    pub draw_us: u64,
    /// Handing the finished scene to the platform window / GPU.
    pub present_us: u64,
    /// Time spent shaping text that missed the two-frame line-layout cache.
    /// A large share here means the frame is re-shaping text rather than
    /// reusing it — usually text whose content changes every frame.
    pub shape_us: u64,
    /// Line layouts that had to be shaped from scratch this frame.
    pub shape_misses: u64,
    /// Primitives in the finished scene. The direct measure of "how much is
    /// this frame actually drawing".
    pub scene_primitives: u64,
}

impl FrameProfile {
    pub fn draw_ms(self) -> f32 {
        self.draw_us as f32 / 1000.0
    }

    pub fn present_ms(self) -> f32 {
        self.present_us as f32 / 1000.0
    }

    pub fn shape_ms(self) -> f32 {
        self.shape_us as f32 / 1000.0
    }

    /// True once at least one frame has been measured.
    pub fn has_sample(self) -> bool {
        self.draw_us > 0 || self.present_us > 0
    }
}

pub(crate) fn begin_frame() {
    SHAPE_US_ACC.store(0, Ordering::Relaxed);
    SHAPE_MISSES_ACC.store(0, Ordering::Relaxed);
}

pub(crate) fn record_draw(micros: u64) {
    DRAW_US.store(micros, Ordering::Relaxed);
    SHAPE_US.store(SHAPE_US_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
    SHAPE_MISSES.store(SHAPE_MISSES_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
}

pub(crate) fn record_present(micros: u64) {
    PRESENT_US.store(micros, Ordering::Relaxed);
}

pub(crate) fn record_scene_primitives(count: u64) {
    SCENE_PRIMITIVES.store(count, Ordering::Relaxed);
}

/// Add one cache-missing text shape to the frame in progress.
pub(crate) fn record_text_shape(micros: u64) {
    SHAPE_US_ACC.fetch_add(micros, Ordering::Relaxed);
    SHAPE_MISSES_ACC.fetch_add(1, Ordering::Relaxed);
}

/// Timings from the most recently completed frame.
pub fn frame_profile() -> FrameProfile {
    FrameProfile {
        draw_us: DRAW_US.load(Ordering::Relaxed),
        present_us: PRESENT_US.load(Ordering::Relaxed),
        shape_us: SHAPE_US.load(Ordering::Relaxed),
        shape_misses: SHAPE_MISSES.load(Ordering::Relaxed),
        scene_primitives: SCENE_PRIMITIVES.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_round_trip_and_report_samples() {
        assert!(!FrameProfile::default().has_sample());
        begin_frame();
        record_text_shape(1_200);
        record_text_shape(800);
        record_scene_primitives(4_096);
        record_draw(31_500);
        record_present(8_250);
        let profile = frame_profile();
        assert_eq!(profile.draw_us, 31_500);
        assert_eq!(profile.shape_us, 2_000, "shape time accumulates per frame");
        assert_eq!(profile.shape_misses, 2);
        assert_eq!(profile.scene_primitives, 4_096);
        // A new frame must not inherit the previous frame's shaping total.
        begin_frame();
        record_draw(1);
        assert_eq!(frame_profile().shape_us, 0);
        assert_eq!(frame_profile().shape_misses, 0);
        record_draw(31_500);
        assert!((profile.draw_ms() - 31.5).abs() < 1.0e-3);
        assert!((profile.present_ms() - 8.25).abs() < 1.0e-3);
        assert!(profile.has_sample());
        record_draw(0);
        record_present(0);
    }
}
