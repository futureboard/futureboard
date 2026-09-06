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
static LAYOUT_NODES: AtomicU64 = AtomicU64::new(0);
static PREPAINT_US: AtomicU64 = AtomicU64::new(0);
static PAINT_US: AtomicU64 = AtomicU64::new(0);
static A11Y_US: AtomicU64 = AtomicU64::new(0);
static SOLVE_US_ACC: AtomicU64 = AtomicU64::new(0);
static MEASURE_NS_ACC: AtomicU64 = AtomicU64::new(0);
static MEASURE_CALLS_ACC: AtomicU64 = AtomicU64::new(0);
static SOLVE_US: AtomicU64 = AtomicU64::new(0);
static MEASURE_NS: AtomicU64 = AtomicU64::new(0);
static MEASURE_CALLS: AtomicU64 = AtomicU64::new(0);
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
    /// Nodes the layout pass walked. Containers lay out without drawing, so
    /// this is normally far larger than `scene_primitives` — and it, not the
    /// primitive count, is what prepaint cost tracks.
    pub layout_nodes: u64,
    /// Building the element tree and laying it out. Includes the app's own
    /// `render` functions, which GPUI calls during this phase.
    pub prepaint_us: u64,
    /// Walking the laid-out tree and emitting scene primitives.
    pub paint_us: u64,
    /// Building the accessibility tree, when a client has activated it. Scales
    /// with the element tree, so it can rival the whole rest of the frame.
    pub a11y_us: u64,
    /// Time inside the layout engine's solve, measure callbacks included.
    pub layout_solve_us: u64,
    /// Time inside measure callbacks alone.
    pub measure_ns: u64,
    /// Measure callbacks invoked. Taffy runs several sizing passes, so this can
    /// dwarf the node count — and when it does, that is the pathology.
    pub measure_calls: u64,
}

impl FrameProfile {
    /// Element tree build, layout, and paint, in milliseconds.
    pub fn draw_ms(self) -> f32 {
        self.draw_us as f32 / 1000.0
    }

    /// Scene handoff to the platform window, in milliseconds.
    pub fn present_ms(self) -> f32 {
        self.present_us as f32 / 1000.0
    }

    /// Text shaping that missed the line-layout cache, in milliseconds.
    pub fn shape_ms(self) -> f32 {
        self.shape_us as f32 / 1000.0
    }

    /// Element tree build plus layout, in milliseconds.
    pub fn prepaint_ms(self) -> f32 {
        self.prepaint_us as f32 / 1000.0
    }

    /// Primitive emission, in milliseconds.
    pub fn paint_ms(self) -> f32 {
        self.paint_us as f32 / 1000.0
    }

    /// Accessibility tree rebuild, in milliseconds.
    pub fn a11y_ms(self) -> f32 {
        self.a11y_us as f32 / 1000.0
    }

    /// The layout engine's solve, measure callbacks included, in milliseconds.
    pub fn layout_solve_ms(self) -> f32 {
        self.layout_solve_us as f32 / 1000.0
    }

    /// Measure callbacks alone, in milliseconds.
    pub fn measure_ms(self) -> f32 {
        self.measure_ns as f32 / 1_000_000.0
    }

    /// True once at least one frame has been measured.
    pub fn has_sample(self) -> bool {
        self.draw_us > 0 || self.present_us > 0
    }
}

pub(crate) fn begin_frame() {
    SHAPE_US_ACC.store(0, Ordering::Relaxed);
    SHAPE_MISSES_ACC.store(0, Ordering::Relaxed);
    SOLVE_US_ACC.store(0, Ordering::Relaxed);
    MEASURE_NS_ACC.store(0, Ordering::Relaxed);
    MEASURE_CALLS_ACC.store(0, Ordering::Relaxed);
}

pub(crate) fn record_layout_solve(micros: u64) {
    SOLVE_US_ACC.fetch_add(micros, Ordering::Relaxed);
}

pub(crate) fn record_layout_measure(nanos: u64) {
    MEASURE_NS_ACC.fetch_add(nanos, Ordering::Relaxed);
    MEASURE_CALLS_ACC.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_draw(micros: u64) {
    census_tick();
    DRAW_US.store(micros, Ordering::Relaxed);
    SHAPE_US.store(SHAPE_US_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
    SHAPE_MISSES.store(SHAPE_MISSES_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
    SOLVE_US.store(SOLVE_US_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
    MEASURE_NS.store(MEASURE_NS_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
    MEASURE_CALLS.store(MEASURE_CALLS_ACC.load(Ordering::Relaxed), Ordering::Relaxed);
}

pub(crate) fn record_present(micros: u64) {
    PRESENT_US.store(micros, Ordering::Relaxed);
}

pub(crate) fn record_scene_primitives(count: u64) {
    SCENE_PRIMITIVES.store(count, Ordering::Relaxed);
}

pub(crate) fn record_layout_nodes(count: u64) {
    LAYOUT_NODES.store(count, Ordering::Relaxed);
}

pub(crate) fn record_phases(prepaint_us: u64, paint_us: u64, a11y_us: u64) {
    PREPAINT_US.store(prepaint_us, Ordering::Relaxed);
    PAINT_US.store(paint_us, Ordering::Relaxed);
    A11Y_US.store(a11y_us, Ordering::Relaxed);
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
        layout_nodes: LAYOUT_NODES.load(Ordering::Relaxed),
        prepaint_us: PREPAINT_US.load(Ordering::Relaxed),
        paint_us: PAINT_US.load(Ordering::Relaxed),
        a11y_us: A11Y_US.load(Ordering::Relaxed),
        layout_solve_us: SOLVE_US.load(Ordering::Relaxed),
        measure_ns: MEASURE_NS.load(Ordering::Relaxed),
        measure_calls: MEASURE_CALLS.load(Ordering::Relaxed),
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

// ---------------------------------------------------------------------------
// Element census
// ---------------------------------------------------------------------------
//
// `layout_nodes` and `measure_calls` say how big the tree is; they do not say
// which code built it. In an app whose frame is 73% layout, that is the only
// question worth answering, and it cannot be answered from the app side —
// `request_layout` runs during GPUI's own tree walk, long after the app's
// `render` function returned, so an app-level timing scope never encloses it.
//
// Enable with `FUTUREBOARD_ELEMENT_CENSUS=1`. Every `CENSUS_PERIOD` frames the
// tallies are printed to stderr and cleared: node counts per `div()` call site,
// and measure-call counts per distinct text string. Disabled builds pay one
// relaxed atomic load per node.

use std::cell::RefCell;
use std::collections::HashMap;

/// Frames between stderr dumps. Long enough not to flood the log, short enough
/// to catch a transient state (a menu open, a track added) while it is up.
const CENSUS_PERIOD: u64 = 120;
/// Rows printed per section.
const CENSUS_ROWS: usize = 24;

/// `u64::MAX` = not yet resolved from the environment.
static CENSUS_ENABLED: AtomicU64 = AtomicU64::new(u64::MAX);
static CENSUS_FRAMES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static SITE_CENSUS: RefCell<HashMap<&'static str, SiteTally>> =
        RefCell::new(HashMap::new());
    static TEXT_CENSUS: RefCell<HashMap<String, TextTally>> = RefCell::new(HashMap::new());
}

#[derive(Default, Clone, Copy)]
struct SiteTally {
    nodes: u64,
}

#[derive(Default, Clone, Copy)]
struct TextTally {
    nodes: u64,
    measures: u64,
    measure_ns: u64,
}

/// Whether the census is collecting. Read on every node, so the env lookup is
/// resolved once into an atomic rather than hitting the environment each time.
pub fn census_enabled() -> bool {
    match CENSUS_ENABLED.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("FUTUREBOARD_ELEMENT_CENSUS").is_some();
            CENSUS_ENABLED.store(on as u64, Ordering::Relaxed);
            on
        }
    }
}

/// Attribute one layout node to the call site that created its element.
pub(crate) fn record_node_site(location: Option<&'static core::panic::Location<'static>>) {
    if !census_enabled() {
        return;
    }
    let (file, line) = match location {
        Some(l) => (l.file(), l.line()),
        None => ("<unknown>", 0),
    };
    SITE_CENSUS.with(|c| {
        c.borrow_mut()
            .entry(intern_site(file, line))
            .or_insert_with(SiteTally::default)
            .nodes += 1;
    });
}

/// Intern a `file:line` key so the per-frame tally hashes a `&'static str`
/// instead of allocating. Bounded by the number of element call sites in the
/// binary, and only ever reached while the census is enabled.
fn intern_site(file: &'static str, line: u32) -> &'static str {
    thread_local! {
        static INTERNED: RefCell<HashMap<(&'static str, u32), &'static str>> =
            RefCell::new(HashMap::new());
    }
    INTERNED.with(|m| {
        *m.borrow_mut()
            .entry((file, line))
            .or_insert_with(|| Box::leak(format!("{file}:{line}").into_boxed_str()))
    })
}

/// Attribute one text element to its content.
pub(crate) fn record_text_node(text: &str) {
    if !census_enabled() {
        return;
    }
    TEXT_CENSUS.with(|c| {
        c.borrow_mut()
            .entry(census_text_key(text))
            .or_insert_with(TextTally::default)
            .nodes += 1;
    });
}

/// Attribute one measure callback, with its cost, to the text it measured.
pub(crate) fn record_text_measure(text: &str, nanos: u64) {
    if !census_enabled() {
        return;
    }
    TEXT_CENSUS.with(|c| {
        let mut c = c.borrow_mut();
        let entry = c
            .entry(census_text_key(text))
            .or_insert_with(TextTally::default);
        entry.measures += 1;
        entry.measure_ns += nanos;
    });
}

/// Collapse a label to a bounded key. Timeline labels are mostly unique (clip
/// names, bar numbers), so keeping them verbatim would make every row a count
/// of one and hide the pattern; the first few characters plus the length group
/// them by shape instead.
fn census_text_key(text: &str) -> String {
    const HEAD: usize = 12;
    let chars = text.chars().count();
    if chars <= HEAD {
        return text.to_string();
    }
    let head: String = text.chars().take(HEAD).collect();
    format!("{head}..[{chars}]")
}

/// Print and clear the census. Called once every `CENSUS_PERIOD` frames.
fn dump_census(frame: u64) {
    let mut sites: Vec<(&'static str, SiteTally)> =
        SITE_CENSUS.with(|c| c.borrow().iter().map(|(k, v)| (*k, *v)).collect());
    let mut texts: Vec<(String, TextTally)> =
        TEXT_CENSUS.with(|c| c.borrow().iter().map(|(k, v)| (k.clone(), *v)).collect());
    SITE_CENSUS.with(|c| c.borrow_mut().clear());
    TEXT_CENSUS.with(|c| c.borrow_mut().clear());

    let frames = CENSUS_PERIOD.max(1) as f64;
    sites.sort_by(|a, b| b.1.nodes.cmp(&a.1.nodes));
    texts.sort_by(|a, b| b.1.measure_ns.cmp(&a.1.measure_ns));

    let total_nodes: u64 = sites.iter().map(|(_, t)| t.nodes).sum();
    let total_measures: u64 = texts.iter().map(|(_, t)| t.measures).sum();
    let total_measure_ns: u64 = texts.iter().map(|(_, t)| t.measure_ns).sum();

    use std::fmt::Write as _;
    let mut report = String::with_capacity(4096);
    let _ = writeln!(
        report,
        "\n[element-census] frame {frame} - averages over {frames:.0} frames"
    );
    let _ = writeln!(
        report,
        "[element-census] nodes/frame {:.0}   text measures/frame {:.0}   measure {:.2} ms/frame",
        total_nodes as f64 / frames,
        total_measures as f64 / frames,
        total_measure_ns as f64 / frames / 1.0e6,
    );
    let _ = writeln!(report, "[element-census] --- layout nodes by call site ---");
    for (site, tally) in sites.iter().take(CENSUS_ROWS) {
        let _ = writeln!(
            report,
            "[element-census] {:>8.1}  {site}",
            tally.nodes as f64 / frames
        );
    }
    let _ = writeln!(report, "[element-census] --- text measure cost by label ---");
    for (text, tally) in texts.iter().take(CENSUS_ROWS) {
        let _ = writeln!(
            report,
            "[element-census] {:>8.3} ms  x{:<7.1} n{:<6.1} {text:?}",
            tally.measure_ns as f64 / frames / 1.0e6,
            tally.measures as f64 / frames,
            tally.nodes as f64 / frames,
        );
    }

    // A release build is linked against the Windows subsystem and has no
    // console, so stderr alone would drop the report on exactly the build whose
    // numbers matter. Write it to a file as well, and let the user redirect it.
    eprint!("{report}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(census_log_path())
    {
        use std::io::Write as _;
        let _ = file.write_all(report.as_bytes());
    }
}

/// Where the census report is appended. `FUTUREBOARD_ELEMENT_CENSUS_LOG`
/// overrides it; otherwise it lands next to the other temp artifacts.
fn census_log_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("FUTUREBOARD_ELEMENT_CENSUS_LOG") {
        return std::path::PathBuf::from(path);
    }
    std::env::temp_dir().join("futureboard-element-census.log")
}

/// Advance the census frame counter, dumping on the period boundary.
pub(crate) fn census_tick() {
    if !census_enabled() {
        return;
    }
    let frame = CENSUS_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if frame.is_multiple_of(CENSUS_PERIOD) {
        dump_census(frame);
    }
}

/// Times one text measure callback and attributes it to the measured string.
/// Allocates only while the census is enabled.
pub(crate) struct TextMeasureGuard(Option<(String, std::time::Instant)>);

impl TextMeasureGuard {
    pub(crate) fn new(text: &str) -> Self {
        if !census_enabled() {
            return Self(None);
        }
        Self(Some((text.to_string(), std::time::Instant::now())))
    }
}

impl Drop for TextMeasureGuard {
    fn drop(&mut self) {
        if let Some((text, start)) = self.0.take() {
            let nanos = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            record_text_measure(&text, nanos);
        }
    }
}
