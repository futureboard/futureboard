use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridLineLevel {
    Bar,
    Beat,
    Sub,
}

pub struct GridLine {
    pub x: f32,
    pub beat: f32,
    pub level: GridLineLevel,
    pub show_label: bool,
}

/// Inputs for [`resolve_timeline_grid_lod`]. A snapshot of the current timeline
/// zoom and musical context — enough to choose how dense the bar/beat/sub grid
/// should be. Kept as a plain value type so the resolver stays a pure function
/// that is trivial to unit test and reuse from the ruler, the GPUI grid, and the
/// WGPU snapshot path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineGridLodParams {
    /// Pixels per quarter-note beat at the current zoom (zoom already baked in,
    /// equals `pixels_per_second * seconds_per_beat`).
    pub pixels_per_beat: f32,
    /// Project tempo. Not used by the level math today, but carried so a future
    /// tempo-map-aware resolver can vary density across the viewport.
    pub bpm: f32,
    /// Active time-signature numerator (musical beats per bar).
    pub numerator: u16,
    /// Active time-signature denominator (note value of one beat).
    pub denominator: u16,
    /// Visible content width in px. Carried for future viewport-aware tuning.
    pub viewport_width: f32,
    /// Horizontal scroll offset in px. Carried for future viewport-aware tuning.
    pub scroll_x: f32,
}

/// Resolved grid level-of-detail for one timeline render. Pure data; the
/// renderers turn this into actual lines/labels. All steps are expressed in
/// musical units so the same struct works for any tempo / time signature.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineGridLod {
    /// Draw a bar line only every Nth bar (1 = every bar). Thins bar lines when
    /// zoomed out so they never collapse into a solid stripe.
    pub major_bar_step: u32,
    /// Whether any bar lines are drawn. Always true today; kept for symmetry and
    /// a possible future "beats-only" / "grid off" mode.
    pub show_bar_lines: bool,
    /// Whether per-beat lines are drawn inside each bar.
    pub show_beat_lines: bool,
    /// Whether sub-beat (1/8, 1/16) lines are drawn.
    pub show_subdivision_lines: bool,
    /// Draw a beat line every Nth musical beat (1 = every beat).
    pub beat_step: u32,
    /// Number of subdivision lines per musical beat (2 = 1/8, 4 = 1/16).
    pub subdivision_per_beat: u32,
    /// Place a ruler bar label every Nth bar. Always a multiple of
    /// `major_bar_step` so labels land on a drawn bar line, and spaced at least
    /// `min_label_px` apart.
    pub label_bar_step: u32,
    /// Whether per-beat ("bar.beat") labels may be drawn (only when zoomed in
    /// far enough that every beat has room for its own label).
    pub show_beat_labels: bool,
    /// Minimum spacing between any two ruler labels, in px. Labels closer than
    /// this are suppressed so text never overlaps.
    pub min_label_px: f32,
}

/// Pure resolver: choose an adaptive musical grid level-of-detail from the
/// current zoom and time signature.
///
/// The point is that callers *iterate only the visible musical positions at the
/// chosen level* instead of emitting a line per beat and culling later. As the
/// user zooms out, bar lines thin (every 2 / 4 / 8 / … bars), beat lines drop
/// out, and labels collapse to clean major bars (1.1, 9.1, 17.1, …). As the user
/// zooms in, beats and then 1/8 / 1/16 subdivisions appear.
///
/// This assumes a single (constant) time signature for the decision; it takes
/// the meter at the visible start. The structure leaves room to resolve per
/// tempo-map / per time-signature segment later without changing callers.
pub fn resolve_timeline_grid_lod(p: &TimelineGridLodParams) -> TimelineGridLod {
    // Bar-line thinning thresholds, in px per bar.
    const BAR_EVERY_1_PX: f32 = 96.0; // >= -> every bar
    const BAR_EVERY_2_PX: f32 = 48.0; // >= -> every 2 bars
    const BAR_EVERY_4_PX: f32 = 24.0; // >= -> every 4 bars
                                      // Never let drawn bar lines pack tighter than this at extreme zoom-out.
    const BAR_MIN_PX: f32 = 24.0;
    // px per musical beat required before beat / subdivision lines appear.
    const BEAT_LINE_MIN_PX: f32 = 18.0;
    const SUBDIV_8_MIN_PX: f32 = 48.0; // 1/8 lines
    const SUBDIV_16_MIN_PX: f32 = 96.0; // 1/16 lines
                                        // px per musical beat required before per-beat ("bar.beat") labels appear.
    const BEAT_LABEL_MIN_PX: f32 = 48.0;
    // Minimum spacing between any two ruler labels.
    const MIN_LABEL_PX: f32 = 48.0;

    let ppb = p.pixels_per_beat.max(0.0001);
    // Quarter-note beats per bar, and one musical beat in quarter-note beats.
    let bar_beats = beats_per_bar_from_sig(p.numerator, p.denominator).max(0.0001) as f32;
    let beat_unit = denominator_unit_quarter_beats(p.denominator).max(0.0001) as f32;

    let px_per_bar = (ppb * bar_beats).max(0.0001);
    let px_per_beat = (ppb * beat_unit).max(0.0001);

    let show_beat_lines = px_per_beat >= BEAT_LINE_MIN_PX;

    // Bar thinning. When beats are visible there is always room for every bar, so
    // force step 1 — otherwise beat lines would cross a "missing" bar line.
    let major_bar_step = if show_beat_lines || px_per_bar >= BAR_EVERY_1_PX {
        1
    } else if px_per_bar >= BAR_EVERY_2_PX {
        2
    } else if px_per_bar >= BAR_EVERY_4_PX {
        4
    } else {
        // Extreme zoom-out: keep doubling from 8 until bar lines are far enough
        // apart to read as bars instead of a stripe.
        let mut step = 8u32;
        while (step as f32) * px_per_bar < BAR_MIN_PX && step < (1 << 20) {
            step *= 2;
        }
        step
    };

    // Subdivisions only matter once beats themselves are visible.
    let subdivision_per_beat = if !show_beat_lines {
        1
    } else if px_per_beat >= SUBDIV_16_MIN_PX {
        4
    } else if px_per_beat >= SUBDIV_8_MIN_PX {
        2
    } else {
        1
    };
    let show_subdivision_lines = subdivision_per_beat > 1;

    // Label thinning: start at the major-bar step, then keep doubling until the
    // labelled bars sit at least MIN_LABEL_PX apart so text never collides.
    let mut label_bar_step = major_bar_step.max(1);
    while (label_bar_step as f32) * px_per_bar < MIN_LABEL_PX && label_bar_step < (1 << 20) {
        label_bar_step *= 2;
    }

    // bar.beat labels only when each beat has its own comfortable room.
    let show_beat_labels = show_beat_lines && px_per_beat >= BEAT_LABEL_MIN_PX;

    TimelineGridLod {
        major_bar_step,
        show_bar_lines: true,
        show_beat_lines,
        show_subdivision_lines,
        beat_step: 1,
        subdivision_per_beat,
        label_bar_step,
        show_beat_labels,
        min_label_px: MIN_LABEL_PX,
    }
}

impl TimelineState {
    pub fn build_interval_list(&self) -> Vec<f32> {
        let bpb = self.beats_per_bar();
        let mut result = Vec::new();
        for &sub in &[
            1.0 / 32.0,
            1.0 / 16.0,
            1.0 / 8.0,
            1.0 / 4.0,
            1.0 / 2.0,
            1.0,
            2.0,
        ] {
            if sub < bpb {
                result.push(sub);
            }
        }
        for &mult in &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0] {
            result.push(bpb * mult);
        }
        result
    }

    pub fn get_grid_interval_beats(&self, ppb: f32) -> f32 {
        let min_beats = 100.0 / ppb.max(1.0);
        let intervals = self.build_interval_list();
        for &n in &intervals {
            if n >= min_beats {
                return n;
            }
        }
        *intervals.last().unwrap_or(&4.0)
    }

    pub fn get_grid_sub_beats(&self, ppb: f32) -> f32 {
        let _bpb = self.beats_per_bar();
        let interval = self.get_grid_interval_beats(ppb);
        let intervals = self.build_interval_list();
        if let Some(idx) = intervals.iter().position(|&x| x == interval) {
            if idx > 0 {
                return intervals[idx - 1];
            }
        }
        interval
    }

    /// The arrangement grid for this frame, built at most once.
    ///
    /// The ruler, the arrangement snapshot, and each visible conductor lane all
    /// want the same lines with the same arguments, so a default layout built
    /// the identical `Vec` six times per frame — dedupe, sort and all. This
    /// hands out one `Arc` and rebuilds only when something the geometry
    /// actually depends on has moved.
    ///
    /// Thread-local rather than a field on `TimelineState`: the state derives
    /// `Clone` and `PartialEq`, and a render cache is neither cloneable state
    /// nor part of a project's identity.
    pub fn arrangement_grid_lines(&self, viewport_width: f32) -> std::rc::Rc<Vec<GridLine>> {
        use std::cell::RefCell;
        use std::hash::{Hash, Hasher};

        let key = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            // Everything `beat_to_x`, the visible range, and the LOD resolver
            // read. The meter map goes in by revision, which it already keeps.
            self.viewport.pixels_per_beat.to_bits().hash(&mut hasher);
            self.viewport.pixels_per_second.to_bits().hash(&mut hasher);
            self.viewport.scroll_x.to_bits().hash(&mut hasher);
            viewport_width.to_bits().hash(&mut hasher);
            self.bpm.to_bits().hash(&mut hasher);
            // The map by content, not only by revision: the cache is
            // thread-wide, and two different maps can sit at the same revision
            // number. Meter changes are a handful of points at most.
            self.time_signature_map.revision().hash(&mut hasher);
            self.time_signature_map.points.len().hash(&mut hasher);
            for point in &self.time_signature_map.points {
                point.beat.to_bits().hash(&mut hasher);
                point.numerator.hash(&mut hasher);
                point.denominator.hash(&mut hasher);
            }
            hasher.finish()
        };

        thread_local! {
            static CACHE: RefCell<Option<(u64, std::rc::Rc<Vec<GridLine>>)>> =
                const { RefCell::new(None) };
        }
        if let Some(hit) = CACHE.with(|cache| {
            cache
                .borrow()
                .as_ref()
                .filter(|(cached, _)| *cached == key)
                .map(|(_, lines)| std::rc::Rc::clone(lines))
        }) {
            crate::perf::count("grid_line_cache_hit", 1);
            return hit;
        }
        crate::perf::count("grid_line_cache_miss", 1);
        let lines = std::rc::Rc::new(self.get_arrangement_grid_lines(viewport_width));
        CACHE.with(|cache| {
            *cache.borrow_mut() = Some((key, std::rc::Rc::clone(&lines)));
        });
        lines
    }

    pub fn get_arrangement_grid_lines(&self, viewport_width: f32) -> Vec<GridLine> {
        let power = crate::perf::power_mode();
        const MAX_GRID_LINES_BASE: usize = 1200;
        // Merge any grid line that would land within this many px of one already
        // placed. Honors the "never draw lines closer than 3px" rule and collapses
        // coincident bar/beat/sub positions onto the first (strongest) level.
        const MIN_GRID_LINE_SPACING_PX: i32 = 3;
        let max_grid_lines = (MAX_GRID_LINES_BASE as f32 * power.grid_line_budget_scale()) as usize;

        let ppb = self.pixels_per_beat().max(0.0001);
        let viewport_width = viewport_width.max(1.0);
        let (start_beat, end_beat) = self.visible_beat_range(viewport_width);
        let start_beat = start_beat.max(0.0);
        let end_beat = end_beat.max(start_beat);
        let max_bpb = self.beats_per_bar_at_beat(end_beat as f64).max(1.0) as f32;

        // One adaptive level-of-detail per frame, resolved from the meter at the
        // left edge of the visible range. This decides how many bar lines to thin
        // away, whether beats / subdivisions appear, and how far apart ruler
        // labels must sit. See [`resolve_timeline_grid_lod`].
        let (start_numerator, start_denominator) = self
            .time_signature_map
            .time_signature_values_at_beat(start_beat as f64);
        let lod = resolve_timeline_grid_lod(&TimelineGridLodParams {
            pixels_per_beat: ppb,
            bpm: self.bpm,
            numerator: start_numerator,
            denominator: start_denominator,
            viewport_width,
            scroll_x: self.viewport.scroll_x,
        });

        let mut lines: Vec<GridLine> = Vec::new();
        // Placed x positions, kept sorted so the "is anything already within
        // 3 px" test is a binary search plus two neighbour comparisons.
        //
        // It used to be a linear scan of every placed line for every candidate,
        // which is O(n^2): a few thousand comparisons at typical zoom and about
        // three quarters of a million at the 1200-line budget — per call, and
        // this is called once per conductor lane plus the ruler plus the
        // arrangement snapshot on every frame.
        let mut occupied_x: Vec<i32> = Vec::new();

        let mut add_line = |beat: f32, level: GridLineLevel, label_candidate: bool| {
            if beat < start_beat - max_bpb || beat > end_beat + max_bpb {
                return;
            }
            let rb = (beat * 100000.0).round() / 100000.0;
            let x = self.beat_to_x(rb).round();
            let x_key = x as i32;
            if x < -1.0 || x > viewport_width + 1.0 {
                return;
            }
            let slot = match occupied_x.binary_search(&x_key) {
                // An exact duplicate is trivially inside the spacing.
                Ok(_) => return,
                Err(slot) => slot,
            };
            let too_close = |index: usize| {
                occupied_x
                    .get(index)
                    .is_some_and(|existing| (x_key - *existing).abs() < MIN_GRID_LINE_SPACING_PX)
            };
            // Everything placed is at least `MIN_GRID_LINE_SPACING_PX` apart, so
            // only the immediate neighbours can be inside that distance.
            if slot > 0 && too_close(slot - 1) {
                return;
            }
            if too_close(slot) {
                return;
            }
            occupied_x.insert(slot, x_key);
            lines.push(GridLine {
                x,
                beat: rb,
                level,
                show_label: label_candidate,
            });
        };

        // Bar + per-beat lines follow time-signature segments. Only the visible
        // musical positions at the chosen LOD are iterated: when zoomed out we
        // step over whole groups of bars instead of emitting every beat and
        // culling later.
        let ts_points = if self.time_signature_map.points.is_empty() {
            vec![TimeSignaturePoint::with_id("implicit-4-4", 0.0, 4, 4)]
        } else {
            self.time_signature_map.points.clone()
        };
        let bar_step = lod.major_bar_step.max(1) as f32;
        let label_step = lod.label_bar_step.max(1) as i64;
        let beat_step = lod.beat_step.max(1);
        for (i, pt) in ts_points.iter().enumerate() {
            let seg_start = pt.beat as f32;
            let seg_end = ts_points
                .get(i + 1)
                .map(|p| p.beat as f32)
                .unwrap_or(f32::INFINITY);
            let bpb = beats_per_bar_from_sig(pt.numerator, pt.denominator) as f32;
            let denom_unit = denominator_unit_quarter_beats(pt.denominator) as f32;
            if seg_end < start_beat {
                continue;
            }
            let rel_start = start_beat.max(seg_start);
            let rel_end = end_beat.min(seg_end);
            // First visible bar (segment-relative), snapped down onto a major-bar
            // boundary so thinned bar lines stay aligned to bar 1 of the segment.
            let first_bar_raw = ((rel_start - seg_start) / bpb).floor() - 1.0;
            let first_bar = ((first_bar_raw / bar_step).floor() * bar_step).max(-bar_step);
            let last_bar = ((rel_end - seg_start) / bpb).ceil() + 1.0;
            let mut bar = first_bar;
            while bar <= last_bar {
                let bar_start = seg_start + bar * bpb;
                if bar_start >= seg_start - TS_BEAT_EPSILON as f32
                    && bar_start < seg_end - TS_BEAT_EPSILON as f32
                {
                    let bar_idx = bar.round() as i64;
                    let is_label_bar = bar_idx.rem_euclid(label_step) == 0;
                    add_line(bar_start, GridLineLevel::Bar, is_label_bar);
                    if lod.show_beat_lines {
                        let mut beat_idx = beat_step;
                        while beat_idx < pt.numerator as u32 {
                            let tick = bar_start + beat_idx as f32 * denom_unit;
                            if tick < seg_end - TS_BEAT_EPSILON as f32 {
                                add_line(tick, GridLineLevel::Beat, lod.show_beat_labels);
                            }
                            beat_idx += beat_step;
                        }
                    }
                }
                bar += bar_step;
            }
        }

        // Sub-beat (1/8, 1/16) lines. Only generated once the per-beat spacing is
        // wide enough (resolved into `subdivision_per_beat`), and never on a
        // denominator-beat position where a beat line already sits.
        let show_subdivisions = lod.show_subdivision_lines && power.allow_sub_grid_lines();
        if show_subdivisions {
            let beat_unit = denominator_unit_quarter_beats(start_denominator) as f32;
            let step = (beat_unit / lod.subdivision_per_beat.max(1) as f32).max(1.0e-4);
            let first_sub = (start_beat / step).floor() - 1.0;
            let last_sub = (end_beat / step).ceil() + 1.0;
            let mut slot = first_sub;
            while slot <= last_sub {
                let beat = slot * step;
                // Values, not a point: this runs once per sub-beat slot across
                // the whole ruler, and the point form clones a `String` id to
                // hand back a number.
                let denom_unit = denominator_unit_quarter_beats(
                    self.time_signature_map
                        .time_signature_values_at_beat(beat as f64)
                        .1,
                ) as f32;
                let on_denom_grid = if denom_unit > TS_BEAT_EPSILON as f32 {
                    ((beat / denom_unit).fract()).abs() < 1e-4
                        || ((beat / denom_unit).fract() - 1.0).abs() < 1e-4
                } else {
                    false
                };
                if !on_denom_grid {
                    add_line(beat, GridLineLevel::Sub, false);
                }
                slot += 1.0;
            }
        }

        lines.sort_by(|a, b| a.x.total_cmp(&b.x));

        if lines.len() > max_grid_lines {
            lines.truncate(max_grid_lines);
        }

        // Enforce minimum label spacing. Candidates were chosen at clean musical
        // steps above; this only suppresses the rare too-close pair (e.g. either
        // side of a time-signature change) so ruler text never overlaps.
        let mut last_label_x = f32::NEG_INFINITY;
        let mut ruler_labels = 0u64;
        for line in &mut lines {
            if line.show_label {
                if line.x - last_label_x >= lod.min_label_px {
                    last_label_x = line.x;
                    ruler_labels += 1;
                } else {
                    line.show_label = false;
                }
            }
        }

        if crate::perf::enabled() {
            let major = lines
                .iter()
                .filter(|l| matches!(l.level, GridLineLevel::Bar))
                .count() as u64;
            let minor = lines.len() as u64 - major;
            crate::perf::count("visible_major_lines", major);
            crate::perf::count("visible_minor_lines", minor);
            crate::perf::count("ruler_labels_drawn", ruler_labels);
        }

        lines
    }

    pub fn format_bar_beat(&self, beats: f32) -> String {
        self.format_bar_beat_at(beats as f64)
    }

    pub fn format_bar_beat_at(&self, beats: f64) -> String {
        let bb = self.time_signature_map.bar_beat_at_beat(beats);
        format!("{}.{}", bb.bar, bb.beat_in_bar)
    }
}
