//! Frame-pacing instrumentation for the CEF off-screen-rendering path.
//!
//! The CEF OSR pipeline spans three crates — the browser callback lands in
//! `SphereWebView`, the GPU copy happens in the platform atlas, and the present
//! happens in the platform renderer — so the timing state lives here, in the
//! one crate all of them already depend on.
//!
//! Everything is **off** unless `FUTUREBOARD_OSR_PROFILING` is set in the
//! environment, in which case each stage keeps a fixed-size ring of samples and
//! a rolling summary is printed every [`REPORT_INTERVAL`]. Disabled, a
//! measurement site costs one relaxed atomic load.
//!
//! ## Why a ring and not a log line per frame
//!
//! Average frame rate hides exactly the problem this module exists to find. A
//! pipeline that alternates 4 ms and 28 ms reports the same FPS as one that
//! holds 16.6 ms, so the summary reports p50/p95/p99 per stage and the raw ring
//! stays available through [`snapshot`] for a developer overlay.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// How often the rolling summary is printed while profiling is enabled.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// Samples retained per stage. At 144 Hz this is ~7 s of history, comfortably
/// more than one report window, so p99 is computed over real data rather than
/// over whatever happened to arrive in the last few milliseconds.
const RING_CAPACITY: usize = 1024;

/// A timed stage of the off-screen frame pipeline.
///
/// The `T`-numbers refer to the pipeline the audit describes: a frame is
/// produced by Chromium, copied into GPU memory the compositor owns, then drawn
/// and presented on a schedule that is deliberately independent of the producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// T0 — wall-clock gap between consecutive CEF accelerated paints. This is
    /// the producer's real cadence, which is *not* required to match the
    /// display's.
    CefFrameInterval,
    /// T0..T3 — the whole synchronous CEF callback, including the GPU copy.
    /// Chromium is blocked for this long, so it bounds the browser's own frame
    /// rate.
    CefCallback,
    /// T1 — `OpenSharedResource1` on the handle CEF handed over.
    TextureOpen,
    /// T2..T3 — `CopySubresourceRegion` from the CEF texture into the tile.
    TextureCopy,
    /// Explicit `ID3D11DeviceContext::Flush` after the copy.
    TextureFlush,
    /// T4 — how long a finished CEF frame waited before a compositor frame
    /// picked it up. This is the decoupling latency, and it should be small and
    /// *stable*, not zero.
    RedrawWait,
    /// T5..T6 — building and submitting the compositor's frame.
    CompositorFrame,
    /// T6..T7 — the present call itself.
    Present,
    /// Wall-clock gap between consecutive presents. The headline pacing number:
    /// its spread, not its mean, is what the user sees as stutter.
    PresentInterval,
    /// Native input event received by the host handler → the event handed to
    /// CEF. This is the number that answers "is rendering blocking input": it
    /// covers only our own translation and dispatch, so if it grows with
    /// resolution the cause is contention on the shared UI thread, not the
    /// conversion itself.
    InputNativeToCef,
}

impl Stage {
    const ALL: [Stage; 10] = [
        Stage::CefFrameInterval,
        Stage::CefCallback,
        Stage::TextureOpen,
        Stage::TextureCopy,
        Stage::TextureFlush,
        Stage::RedrawWait,
        Stage::CompositorFrame,
        Stage::Present,
        Stage::PresentInterval,
        Stage::InputNativeToCef,
    ];

    fn index(self) -> usize {
        match self {
            Stage::CefFrameInterval => 0,
            Stage::CefCallback => 1,
            Stage::TextureOpen => 2,
            Stage::TextureCopy => 3,
            Stage::TextureFlush => 4,
            Stage::RedrawWait => 5,
            Stage::CompositorFrame => 6,
            Stage::Present => 7,
            Stage::PresentInterval => 8,
            Stage::InputNativeToCef => 9,
        }
    }

    /// Label used in the rolling report and any developer overlay.
    pub fn label(self) -> &'static str {
        match self {
            Stage::CefFrameInterval => "cef interval",
            Stage::CefCallback => "cef callback",
            Stage::TextureOpen => "texture open",
            Stage::TextureCopy => "texture copy",
            Stage::TextureFlush => "texture flush",
            Stage::RedrawWait => "redraw wait",
            Stage::CompositorFrame => "compositor",
            Stage::Present => "present",
            Stage::PresentInterval => "present interval",
            Stage::InputNativeToCef => "input->cef",
        }
    }
}

/// A monotonically increasing event count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Counter {
    /// Accelerated frames CEF delivered.
    CefFrames,
    /// CEF frames superseded by a newer one before any compositor frame drew
    /// them. Non-zero is expected and healthy — latest-frame-wins means old
    /// frames are meant to be dropped.
    CefFramesDropped,
    /// Frames the compositor presented.
    CompositorFrames,
    /// Compositor frames that reused the previous web texture because no new
    /// CEF frame had arrived. Should dominate on a high-refresh display.
    CompositorFramesReusingWebTexture,
    /// `OpenSharedResource1` calls that actually hit the driver.
    SharedHandleOpens,
    /// Shared-handle opens served from the cache instead.
    SharedHandleCacheHits,
    /// Atlas tiles allocated for an external image. Should stay near zero
    /// during steady-state animation; a climbing count means per-frame
    /// reallocation.
    TileAllocations,
    /// Atlas tiles released for an external image.
    TileReleases,
    /// Failed GPU copies, which force the browser back to software OSR.
    CopyFailures,
    /// `OnAcceleratedPaint` callbacks. Should account for every frame while the
    /// GPU path is healthy.
    AcceleratedPaints,
    /// `OnPaint` (software) callbacks. **Any** non-zero value while the browser
    /// was created accelerated means Chromium fell back without telling us.
    SoftwarePaints,
    /// Dirty rectangles CEF reported across all accelerated paints.
    DirtyRects,
    /// Dirty pixels CEF reported, summed. Compared against [`Self::SurfacePixels`]
    /// this is the share of each frame that actually changed — the copy the
    /// pipeline *could* be doing.
    DirtyPixels,
    /// Full-surface pixels copied, summed: one whole surface per accelerated
    /// frame, which is what the pipeline copies today.
    SurfacePixels,
    /// Mouse presses forwarded to CEF.
    InputMouseDown,
    /// Mouse releases forwarded to CEF.
    InputMouseUp,
    /// Wheel events forwarded to CEF.
    InputWheel,
    /// Mouse moves forwarded to CEF. Counted, never traced individually.
    InputMouseMove,
    /// `SendCaptureLostEvent` calls.
    InputCaptureLost,
    /// Presses forwarded with `clickCount > 1`. A user who is not
    /// double-clicking should see this stay at zero.
    InputMultiClick,
    /// Presses whose click count was reset because the previous click landed
    /// outside the browser region. Each one is a spurious double-click that the
    /// browser-scoped tracker prevented.
    InputClickResetOutside,
}

impl Counter {
    const ALL: [Counter; 21] = [
        Counter::CefFrames,
        Counter::CefFramesDropped,
        Counter::CompositorFrames,
        Counter::CompositorFramesReusingWebTexture,
        Counter::SharedHandleOpens,
        Counter::SharedHandleCacheHits,
        Counter::TileAllocations,
        Counter::TileReleases,
        Counter::CopyFailures,
        Counter::AcceleratedPaints,
        Counter::SoftwarePaints,
        Counter::DirtyRects,
        Counter::DirtyPixels,
        Counter::SurfacePixels,
        Counter::InputMouseDown,
        Counter::InputMouseUp,
        Counter::InputWheel,
        Counter::InputMouseMove,
        Counter::InputCaptureLost,
        Counter::InputMultiClick,
        Counter::InputClickResetOutside,
    ];

    fn index(self) -> usize {
        match self {
            Counter::CefFrames => 0,
            Counter::CefFramesDropped => 1,
            Counter::CompositorFrames => 2,
            Counter::CompositorFramesReusingWebTexture => 3,
            Counter::SharedHandleOpens => 4,
            Counter::SharedHandleCacheHits => 5,
            Counter::TileAllocations => 6,
            Counter::TileReleases => 7,
            Counter::CopyFailures => 8,
            Counter::AcceleratedPaints => 9,
            Counter::SoftwarePaints => 10,
            Counter::DirtyRects => 11,
            Counter::DirtyPixels => 12,
            Counter::SurfacePixels => 13,
            Counter::InputMouseDown => 14,
            Counter::InputMouseUp => 15,
            Counter::InputWheel => 16,
            Counter::InputMouseMove => 17,
            Counter::InputCaptureLost => 18,
            Counter::InputMultiClick => 19,
            Counter::InputClickResetOutside => 20,
        }
    }

    /// Label used in the rolling report and any developer overlay.
    pub fn label(self) -> &'static str {
        match self {
            Counter::CefFrames => "cef frames",
            Counter::CefFramesDropped => "cef dropped",
            Counter::CompositorFrames => "compositor frames",
            Counter::CompositorFramesReusingWebTexture => "reused web texture",
            Counter::SharedHandleOpens => "handle opens",
            Counter::SharedHandleCacheHits => "handle cache hits",
            Counter::TileAllocations => "tile allocs",
            Counter::TileReleases => "tile frees",
            Counter::CopyFailures => "copy failures",
            Counter::AcceleratedPaints => "accel paints",
            Counter::SoftwarePaints => "software paints",
            Counter::DirtyRects => "dirty rects",
            Counter::DirtyPixels => "dirty px",
            Counter::SurfacePixels => "surface px",
            Counter::InputMouseDown => "mouse down",
            Counter::InputMouseUp => "mouse up",
            Counter::InputWheel => "wheel",
            Counter::InputMouseMove => "mouse move",
            Counter::InputCaptureLost => "capture lost",
            Counter::InputMultiClick => "multi-click",
            Counter::InputClickResetOutside => "click reset (outside)",
        }
    }
}

/// Read one counter's current value.
pub fn counter(counter: Counter) -> u64 {
    if !enabled() {
        return 0;
    }
    counters()[counter.index()].load(Ordering::Relaxed)
}

/// Whether OSR profiling is enabled for this process.
///
/// Read from `FUTUREBOARD_OSR_PROFILING` exactly once. Any value enables it;
/// `0`, `false` and the empty string disable it, so the variable can be pinned
/// off in a shell that exports it globally.
#[inline]
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("FUTUREBOARD_OSR_PROFILING") {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "off"),
        Err(_) => false,
    })
}

/// Fixed-size ring of microsecond samples.
struct Ring {
    samples: Box<[f32; RING_CAPACITY]>,
    len: usize,
    next: usize,
}

impl Ring {
    fn new() -> Self {
        Self {
            samples: Box::new([0.0; RING_CAPACITY]),
            len: 0,
            next: 0,
        }
    }

    fn push(&mut self, micros: f32) {
        self.samples[self.next] = micros;
        self.next = (self.next + 1) % RING_CAPACITY;
        self.len = (self.len + 1).min(RING_CAPACITY);
    }

    fn stats(&self) -> Option<StageStats> {
        if self.len == 0 {
            return None;
        }
        let mut sorted: Vec<f32> = self.samples[..self.len].to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let percentile = |p: f64| {
            // Nearest-rank: with a handful of samples this reports a value that
            // actually occurred rather than an interpolation between two.
            let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
            sorted[rank - 1]
        };
        let sum: f64 = sorted.iter().map(|value| *value as f64).sum();
        Some(StageStats {
            count: sorted.len(),
            avg_ms: (sum / sorted.len() as f64) as f32 / 1000.0,
            p50_ms: percentile(0.50) / 1000.0,
            p95_ms: percentile(0.95) / 1000.0,
            p99_ms: percentile(0.99) / 1000.0,
            max_ms: sorted[sorted.len() - 1] / 1000.0,
        })
    }
}

/// Rolling distribution for one [`Stage`], in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct StageStats {
    /// Samples the distribution was computed over.
    pub count: usize,
    /// Arithmetic mean.
    pub avg_ms: f32,
    /// Median.
    pub p50_ms: f32,
    /// 95th percentile.
    pub p95_ms: f32,
    /// 99th percentile — where stutter shows up first.
    pub p99_ms: f32,
    /// Largest sample in the ring.
    pub max_ms: f32,
}

struct State {
    rings: Vec<Ring>,
    /// Last observation per stage, for the stages measured as intervals.
    last_mark: Vec<Option<Instant>>,
    last_report: Instant,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            rings: (0..Stage::ALL.len()).map(|_| Ring::new()).collect(),
            last_mark: vec![None; Stage::ALL.len()],
            last_report: Instant::now(),
        })
    })
}

fn counters() -> &'static [AtomicU64; Counter::ALL.len()] {
    static COUNTERS: OnceLock<[AtomicU64; Counter::ALL.len()]> = OnceLock::new();
    COUNTERS.get_or_init(|| std::array::from_fn(|_| AtomicU64::new(0)))
}

/// Record a completed measurement for `stage`.
#[inline]
pub fn record(stage: Stage, elapsed: Duration) {
    if !enabled() {
        return;
    }
    state().lock().rings[stage.index()].push(elapsed.as_secs_f32() * 1_000_000.0);
}

/// Record the gap since the previous call to `mark` for the same stage. The
/// first call only establishes the baseline.
#[inline]
pub fn mark(stage: Stage) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut state = state().lock();
    let index = stage.index();
    if let Some(previous) = state.last_mark[index].replace(now) {
        let micros = now.duration_since(previous).as_secs_f32() * 1_000_000.0;
        state.rings[index].push(micros);
    }
}

/// Add `amount` to `counter`.
#[inline]
pub fn count(counter: Counter, amount: u64) {
    if !enabled() {
        return;
    }
    counters()[counter.index()].fetch_add(amount, Ordering::Relaxed);
}

/// Scoped timer. Records into its stage when dropped, so a measured region does
/// not need an explicit end call on every early return.
pub struct Span {
    stage: Stage,
    start: Instant,
}

impl Span {
    /// Begin timing `stage`, or return `None` when profiling is disabled.
    #[inline]
    pub fn new(stage: Stage) -> Option<Self> {
        enabled().then(|| Self {
            stage,
            start: Instant::now(),
        })
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        record(self.stage, self.start.elapsed());
    }
}

/// Begin timing `stage`. Shorthand for [`Span::new`].
#[inline]
pub fn span(stage: Stage) -> Option<Span> {
    Span::new(stage)
}

/// Process-wide baseline for the compact nanosecond timestamps producers hand
/// to consumers.
///
/// `Instant` is not atomic, and taking a mutex on a callback path to carry one
/// timestamp would be instrumentation that changes what it measures. Everything
/// that needs to publish a time through an atomic uses nanoseconds since this
/// epoch instead.
pub fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Nanoseconds since [`epoch`].
#[inline]
pub fn epoch_nanos() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// What a traced input event was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// A button press forwarded to the browser.
    MouseDown,
    /// A button release forwarded to the browser.
    MouseUp,
    /// A wheel event forwarded to the browser.
    Wheel,
    /// The host told the browser its pointer grab ended.
    CaptureLost,
}

impl InputKind {
    fn label(self) -> &'static str {
        match self {
            InputKind::MouseDown => "DOWN",
            InputKind::MouseUp => "UP  ",
            InputKind::Wheel => "WHEL",
            InputKind::CaptureLost => "CAPL",
        }
    }
}

/// One discrete input event on its way to CEF, with every coordinate space it
/// passed through.
///
/// Mouse *moves* are deliberately absent: they are counted
/// ([`Counter::InputMouseMove`]) but never recorded, because a 4K high-refresh
/// mouse produces thousands per second and tracing them would dominate the
/// thread being measured.
#[derive(Debug, Clone, Copy)]
pub struct InputRecord {
    /// Monotonic per-process sequence number. Gaps or reordering in a trace are
    /// the signal that events were dropped or replayed.
    pub seq: u64,
    /// What kind of event this was.
    pub kind: InputKind,
    /// `None` for events with no button (wheel, capture lost).
    pub button: Option<&'static str>,
    /// Click count as forwarded to CEF; `0` where the concept does not apply.
    pub click_count: i32,
    /// Physical pixels, window client space — what Win32 delivered.
    pub native_physical: (i32, i32),
    /// Logical pixels, window client space — what GPUI reported.
    pub gpui_logical: (f32, f32),
    /// Logical pixels, browser view space — what CEF was told.
    pub cef_view: (i32, i32),
    /// Device scale factor in force when the event was translated.
    pub scale_factor: f32,
    /// Nanoseconds since [`epoch`] when the host handler received the event.
    pub received_nanos: u64,
    /// Nanoseconds since [`epoch`] when the event had been handed to CEF.
    pub dispatched_nanos: u64,
}

impl InputRecord {
    fn latency_ms(&self) -> f32 {
        self.dispatched_nanos.saturating_sub(self.received_nanos) as f32 / 1_000_000.0
    }

    /// Whether this event is worth printing in the periodic report on its own.
    ///
    /// A clean single click at low latency is noise; a multi-click or a slow
    /// dispatch is exactly what the audit is looking for.
    fn is_anomalous(&self) -> bool {
        self.click_count > 1 || self.latency_ms() >= ANOMALOUS_INPUT_LATENCY_MS
    }

    fn format(&self) -> String {
        // Capture loss has no pointer position — it is a state transition, not
        // a location. Printing zeroes for it would read as a click at the
        // window's top-left corner.
        if self.kind == InputKind::CaptureLost {
            return format!(
                "#{:<6} {} scale={:.2} latency={:.3}ms",
                self.seq,
                self.kind.label(),
                self.scale_factor,
                self.latency_ms(),
            );
        }
        format!(
            "#{:<6} {} btn={:<6} clicks={} native=({},{}) gpui=({:.1},{:.1}) cef=({},{}) scale={:.2} latency={:.3}ms",
            self.seq,
            self.kind.label(),
            self.button.unwrap_or("-"),
            self.click_count,
            self.native_physical.0,
            self.native_physical.1,
            self.gpui_logical.0,
            self.gpui_logical.1,
            self.cef_view.0,
            self.cef_view.1,
            self.scale_factor,
            self.latency_ms(),
        )
    }
}

/// Dispatch latency at or above which a single input event is printed in the
/// periodic report. One display frame at 120 Hz; anything slower than this is
/// perceptible on a knob drag.
const ANOMALOUS_INPUT_LATENCY_MS: f32 = 8.0;

/// Discrete input events retained for the periodic report. Deep enough to hold
/// a full click burst plus its surrounding wheel activity.
const INPUT_RING_CAPACITY: usize = 64;

static INPUT_SEQ: AtomicU64 = AtomicU64::new(0);

fn input_ring() -> &'static Mutex<Vec<InputRecord>> {
    static RING: OnceLock<Mutex<Vec<InputRecord>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(Vec::with_capacity(INPUT_RING_CAPACITY)))
}

/// Whether every discrete input event should be printed as it happens.
///
/// Separate from [`enabled`] because a live trace is what you want while
/// performing a scripted click test, and is far too loud to leave on during a
/// pacing measurement. Moves are never traced either way.
pub fn input_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("FUTUREBOARD_OSR_INPUT_TRACE") {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "off"),
        Err(_) => false,
    })
}

/// Whether any input instrumentation is active.
#[inline]
pub fn input_enabled() -> bool {
    enabled() || input_trace_enabled()
}

/// Allocate the next input sequence number. Returns `0` when instrumentation is
/// off, so callers pay nothing for a number nobody will read.
#[inline]
pub fn next_input_seq() -> u64 {
    if !input_enabled() {
        return 0;
    }
    INPUT_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Record one discrete input event.
pub fn record_input(event: InputRecord) {
    if !input_enabled() {
        return;
    }
    if input_trace_enabled() {
        eprintln!("[osr-input] {}", event.format());
    }
    if !enabled() {
        return;
    }
    record(
        Stage::InputNativeToCef,
        Duration::from_nanos(event.dispatched_nanos.saturating_sub(event.received_nanos)),
    );
    let mut ring = input_ring().lock();
    if ring.len() == INPUT_RING_CAPACITY {
        ring.remove(0);
    }
    ring.push(event);
}

/// A point-in-time view of every stage and counter, for a developer overlay.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Per-stage distributions, in [`Stage::ALL`] order. `None` where no sample
    /// has been recorded yet.
    pub stages: Vec<(Stage, Option<StageStats>)>,
    /// Cumulative counters since process start.
    pub counters: Vec<(Counter, u64)>,
}

/// Capture the current rolling statistics without printing anything.
pub fn snapshot() -> Snapshot {
    let state = state().lock();
    Snapshot {
        stages: Stage::ALL
            .iter()
            .map(|stage| (*stage, state.rings[stage.index()].stats()))
            .collect(),
        counters: Counter::ALL
            .iter()
            .map(|counter| {
                (
                    *counter,
                    counters()[counter.index()].load(Ordering::Relaxed),
                )
            })
            .collect(),
    }
}

/// Print the rolling summary if [`REPORT_INTERVAL`] has elapsed.
///
/// Call this once per presented frame: it is a cheap deadline check on all but
/// one frame in every couple of hundred, and tying it to the present keeps the
/// report out of the pipeline it is measuring.
pub fn maybe_report() {
    if !enabled() {
        return;
    }
    {
        let mut state = state().lock();
        if state.last_report.elapsed() < REPORT_INTERVAL {
            return;
        }
        state.last_report = Instant::now();
    }
    eprintln!("{}", format_report(&snapshot()));
}

/// Render `snapshot` as the multi-line rolling report.
fn format_report(snapshot: &Snapshot) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("[osr-profile] rolling window\n");
    for (stage, stats) in &snapshot.stages {
        match stats {
            Some(stats) => {
                let _ = writeln!(
                    out,
                    "  {:<17} avg {:>7.3}  p50 {:>7.3}  p95 {:>7.3}  p99 {:>7.3}  max {:>7.3}  (n={})",
                    stage.label(),
                    stats.avg_ms,
                    stats.p50_ms,
                    stats.p95_ms,
                    stats.p99_ms,
                    stats.max_ms,
                    stats.count,
                );
            }
            None => {
                let _ = writeln!(out, "  {:<17} (no samples)", stage.label());
            }
        }
    }
    let value_of = |wanted: Counter| {
        snapshot
            .counters
            .iter()
            .find(|(counter, _)| *counter == wanted)
            .map(|(_, value)| *value)
            .unwrap_or(0)
    };

    // Derived answers, spelled out rather than left as raw sums the reader has
    // to divide in their head while staring at a stuttering editor.
    let dirty = value_of(Counter::DirtyPixels);
    let surface = value_of(Counter::SurfacePixels);
    if surface > 0 {
        let _ = writeln!(
            out,
            "  dirty coverage    {:.2}%  ({} dirty px over {} copied px, {} rects)",
            (dirty as f64 / surface as f64) * 100.0,
            dirty,
            surface,
            value_of(Counter::DirtyRects),
        );
    }
    let software = value_of(Counter::SoftwarePaints);
    let _ = writeln!(
        out,
        "  osr backend       {}  (accelerated={} software={})",
        if software == 0 {
            "ACCELERATED"
        } else {
            "MIXED — software fallback frames present"
        },
        value_of(Counter::AcceleratedPaints),
        software,
    );

    out.push_str("  counters:");
    for (counter, value) in &snapshot.counters {
        let _ = write!(out, " {}={}", counter.label(), value);
    }

    let recent = input_ring().lock();
    let anomalies: Vec<&InputRecord> = recent
        .iter()
        .filter(|record| record.is_anomalous())
        .collect();
    if anomalies.is_empty() {
        let _ = write!(
            out,
            "\n  input: {} recent discrete events, none anomalous",
            recent.len()
        );
    } else {
        let _ = write!(
            out,
            "\n  input anomalies (clickCount>1 or >{ANOMALOUS_INPUT_LATENCY_MS}ms dispatch):"
        );
        for record in anomalies {
            let _ = write!(out, "\n    {}", record.format());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_reports_percentiles_over_the_samples_it_holds() {
        let mut ring = Ring::new();
        assert!(ring.stats().is_none(), "an empty ring has no distribution");
        // 1..=100 milliseconds, pushed as microseconds.
        for value in 1..=100 {
            ring.push(value as f32 * 1000.0);
        }
        let stats = ring.stats().expect("samples were pushed");
        assert_eq!(stats.count, 100);
        assert!((stats.avg_ms - 50.5).abs() < 0.01);
        assert!((stats.p50_ms - 50.0).abs() < 0.01);
        assert!((stats.p95_ms - 95.0).abs() < 0.01);
        assert!((stats.p99_ms - 99.0).abs() < 0.01);
        assert!((stats.max_ms - 100.0).abs() < 0.01);
    }

    #[test]
    fn a_full_ring_keeps_only_the_newest_samples() {
        let mut ring = Ring::new();
        for value in 0..RING_CAPACITY * 2 {
            ring.push(value as f32);
        }
        let stats = ring.stats().expect("samples were pushed");
        assert_eq!(
            stats.count, RING_CAPACITY,
            "the ring never grows past its capacity"
        );
        // Only the second half survives, so the smallest retained sample is
        // RING_CAPACITY, not 0.
        assert!(
            (stats.max_ms - (RING_CAPACITY * 2 - 1) as f32 / 1000.0).abs() < 0.01,
            "the newest sample is retained"
        );
    }

    /// A distribution with the same mean can have wildly different pacing. This
    /// is the entire reason the module reports percentiles.
    #[test]
    fn percentiles_separate_stable_pacing_from_stutter() {
        let mut stable = Ring::new();
        let mut stuttering = Ring::new();
        for _ in 0..50 {
            stable.push(16_600.0);
            stable.push(16_700.0);
            stuttering.push(4_000.0);
            stuttering.push(29_300.0);
        }
        let stable = stable.stats().expect("samples");
        let stuttering = stuttering.stats().expect("samples");
        assert!(
            (stable.avg_ms - stuttering.avg_ms).abs() < 0.01,
            "both distributions have the same mean frame time"
        );
        assert!(
            stuttering.p99_ms > stable.p99_ms * 1.5,
            "but only the stuttering one has a heavy tail: {} vs {}",
            stuttering.p99_ms,
            stable.p99_ms
        );
    }

    #[test]
    fn stage_and_counter_indices_are_unique_and_dense() {
        let mut stage_indices: Vec<usize> = Stage::ALL.iter().map(|s| s.index()).collect();
        stage_indices.sort_unstable();
        assert_eq!(stage_indices, (0..Stage::ALL.len()).collect::<Vec<_>>());

        let mut counter_indices: Vec<usize> = Counter::ALL.iter().map(|c| c.index()).collect();
        counter_indices.sort_unstable();
        assert_eq!(counter_indices, (0..Counter::ALL.len()).collect::<Vec<_>>());
    }

    fn input_record(click_count: i32, latency_ms: f32) -> InputRecord {
        InputRecord {
            seq: 1,
            kind: InputKind::MouseDown,
            button: Some("left"),
            click_count,
            native_physical: (1820, 920),
            gpui_logical: (1213.3, 613.3),
            cef_view: (1213, 613),
            scale_factor: 1.5,
            received_nanos: 0,
            dispatched_nanos: (latency_ms * 1_000_000.0) as u64,
        }
    }

    /// The periodic report must not drown the interesting events in clean ones.
    #[test]
    fn only_multi_clicks_and_slow_dispatches_are_anomalous() {
        assert!(
            !input_record(1, 0.2).is_anomalous(),
            "a clean click is noise"
        );
        assert!(
            input_record(2, 0.2).is_anomalous(),
            "a double click is exactly what the audit is hunting"
        );
        assert!(
            input_record(1, ANOMALOUS_INPUT_LATENCY_MS + 1.0).is_anomalous(),
            "a slow dispatch means rendering is blocking input"
        );
    }

    /// Capture loss is a state transition, not a location; printing zeroed
    /// coordinates for it would read as a click in the corner of the window.
    #[test]
    fn capture_loss_is_traced_without_a_position() {
        let mut record = input_record(0, 0.4);
        record.kind = InputKind::CaptureLost;
        record.button = None;
        let formatted = record.format();
        assert!(formatted.contains("CAPL"));
        assert!(!formatted.contains("native="));
        assert!(!formatted.contains("clicks="));
    }

    #[test]
    fn an_input_record_reports_every_coordinate_space_it_passed_through() {
        let formatted = input_record(1, 0.5).format();
        for expected in [
            "native=(1820,920)",
            "gpui=(1213.3,613.3)",
            "cef=(1213,613)",
            "scale=1.50",
            "clicks=1",
        ] {
            assert!(
                formatted.contains(expected),
                "{expected:?} missing from {formatted:?}"
            );
        }
    }

    #[test]
    fn latency_is_the_gap_between_receive_and_dispatch() {
        let record = input_record(1, 12.5);
        assert!((record.latency_ms() - 12.5).abs() < 0.001);
        // A dispatch timestamp older than the receive timestamp is a clock
        // ordering bug, not a negative latency.
        let mut inverted = record;
        inverted.dispatched_nanos = 0;
        inverted.received_nanos = 5_000_000;
        assert_eq!(inverted.latency_ms(), 0.0);
    }

    #[test]
    fn measurement_sites_are_inert_when_profiling_is_disabled() {
        // The test process does not set FUTUREBOARD_OSR_PROFILING, so every
        // entry point must be a no-op rather than allocating or recording.
        assert!(!enabled());
        assert!(span(Stage::TextureCopy).is_none());
        record(Stage::TextureCopy, Duration::from_millis(5));
        mark(Stage::PresentInterval);
        count(Counter::CefFrames, 1);
        assert_eq!(next_input_seq(), 0, "sequence numbers nobody will read");
        record_input(input_record(3, 40.0));
        assert!(input_ring().lock().is_empty());
        let snapshot = snapshot();
        assert!(snapshot.stages.iter().all(|(_, stats)| stats.is_none()));
        assert!(snapshot.counters.iter().all(|(_, value)| *value == 0));
        assert_eq!(counter(Counter::InputMouseDown), 0);
    }
}
