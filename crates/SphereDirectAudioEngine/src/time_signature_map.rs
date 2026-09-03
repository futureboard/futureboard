//! Runtime time-signature map for metronome accents and bar-boundary evaluation.

const TS_BEAT_EPSILON: f64 = 1e-6;
const ALLOWED_DENOMINATORS: [u16; 6] = [1, 2, 4, 8, 16, 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetronomeAccent {
    Downbeat,
    Group,
    Normal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeTimeSignaturePointSnapshot {
    pub beat: f64,
    pub numerator: u16,
    pub denominator: u16,
    pub grouping: Vec<u16>,
}

/// Normalized, beat-sorted time-signature map.
///
/// **Invariant: `points` is never empty.** Every constructor either builds a
/// point or falls back to 4/4, because the metronome reads this map from the
/// audio thread by index — an empty vector there is a panic across the device
/// callback, not a missing accent.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeTimeSignatureMapSnapshot {
    points: Vec<RuntimeTimeSignaturePointSnapshot>,
}

impl Default for RuntimeTimeSignatureMapSnapshot {
    fn default() -> Self {
        Self::static_sig(4, 4)
    }
}

fn normalize_denominator(denominator: u16) -> u16 {
    ALLOWED_DENOMINATORS
        .iter()
        .copied()
        .min_by_key(|allowed| (denominator as i32 - *allowed as i32).unsigned_abs())
        .unwrap_or(4)
}

pub fn beats_per_bar_from_sig(numerator: u16, denominator: u16) -> f64 {
    let num = numerator.clamp(1, 64) as f64;
    let den = normalize_denominator(denominator).max(1) as f64;
    num * (4.0 / den)
}

pub fn denominator_unit_quarter_beats(denominator: u16) -> f64 {
    4.0 / normalize_denominator(denominator).max(1) as f64
}

pub fn default_time_signature_grouping(numerator: u16, denominator: u16) -> Vec<u16> {
    let numerator = numerator.clamp(1, 64);
    let denominator = normalize_denominator(denominator);
    match (numerator, denominator) {
        (2, 4) => vec![2],
        (3, 4) => vec![3],
        (4, 4) => vec![4],
        (5, 8) => vec![2, 3],
        (6, 8) => vec![3, 3],
        (7, 8) => vec![2, 2, 3],
        (9, 8) => vec![3, 3, 3],
        (12, 8) => vec![3, 3, 3, 3],
        (n, 8) if n % 2 == 1 && n > 3 => {
            let pairs = ((n - 3) / 2) as usize;
            let mut groups = vec![2; pairs];
            groups.push(3);
            groups
        }
        _ => vec![numerator],
    }
}

fn normalize_grouping(numerator: u16, denominator: u16, grouping: &[u16]) -> Vec<u16> {
    let numerator = numerator.clamp(1, 64);
    if grouping.is_empty()
        || grouping.contains(&0)
        || grouping.iter().map(|&g| g as u32).sum::<u32>() != numerator as u32
    {
        default_time_signature_grouping(numerator, denominator)
    } else {
        grouping.to_vec()
    }
}

/// Whether `denom_index` starts a beat group other than the downbeat.
///
/// The allocation-free form of "collect the group start offsets, then look for
/// this one": the metronome asks this per click, on the audio thread, so it
/// must not build a `Vec` to answer.
#[inline]
fn is_group_start(grouping: &[u16], denom_index: u16) -> bool {
    let mut acc = 0u16;
    for (i, &grp) in grouping.iter().enumerate() {
        if i > 0 && acc == denom_index {
            return true;
        }
        acc = acc.saturating_add(grp);
    }
    false
}

impl RuntimeTimeSignaturePointSnapshot {
    pub fn effective_grouping(&self) -> Vec<u16> {
        normalize_grouping(self.numerator, self.denominator, &self.grouping)
    }

    /// The already-normalized grouping. Points reach the audio thread only
    /// through [`RuntimeTimeSignatureMapSnapshot`], whose constructors
    /// normalize every grouping in place, so the click path reads the slice
    /// instead of rebuilding it per click.
    #[inline]
    pub fn grouping_slice(&self) -> &[u16] {
        &self.grouping
    }
}

impl RuntimeTimeSignatureMapSnapshot {
    pub fn static_sig(numerator: u16, denominator: u16) -> Self {
        let numerator = numerator.clamp(1, 64);
        let denominator = normalize_denominator(denominator);
        Self {
            points: vec![RuntimeTimeSignaturePointSnapshot {
                beat: 0.0,
                numerator,
                denominator,
                grouping: default_time_signature_grouping(numerator, denominator),
            }],
        }
    }

    pub fn from_points(points: Vec<RuntimeTimeSignaturePointSnapshot>) -> Self {
        let mut map = Self { points };
        map.sort();
        map
    }

    pub fn points(&self) -> &[RuntimeTimeSignaturePointSnapshot] {
        &self.points
    }

    /// The signature in force at `beat`. Borrowed, not cloned: the metronome
    /// calls this per click from the audio callback.
    pub fn time_signature_at_beat(&self, beat: f64) -> &RuntimeTimeSignaturePointSnapshot {
        let beat = beat.max(0.0);
        let points = self.sorted_points();
        let mut idx = 0usize;
        for (i, p) in points.iter().enumerate() {
            if p.beat <= beat + TS_BEAT_EPSILON {
                idx = i;
            } else {
                break;
            }
        }
        // `points` is never empty (see the type invariant), so this cannot
        // panic on the audio thread.
        &points[idx]
    }

    pub fn bar_beat_at_beat(&self, beat: f64) -> (i64, u16, u16, u16) {
        let beat = beat.max(0.0);
        let points = self.sorted_points();
        let mut global_bar: i64 = 1;

        for (i, pt) in points.iter().enumerate() {
            let seg_start = pt.beat;
            let seg_end = points.get(i + 1).map(|p| p.beat).unwrap_or(f64::INFINITY);
            if beat + TS_BEAT_EPSILON < seg_start {
                continue;
            }
            let bpb = beats_per_bar_from_sig(pt.numerator, pt.denominator);
            let denom_unit = denominator_unit_quarter_beats(pt.denominator);
            if beat < seg_end - TS_BEAT_EPSILON || i + 1 == points.len() {
                let rel = (beat - seg_start).max(0.0);
                let bar_offset = (rel / bpb).floor() as i64;
                let beat_in_bar_q = rel - bar_offset as f64 * bpb;
                let denom_idx = (beat_in_bar_q / denom_unit).floor();
                return (
                    global_bar + bar_offset,
                    (denom_idx as u16).saturating_add(1),
                    pt.numerator,
                    pt.denominator,
                );
            }
            let bars_in_seg = ((seg_end - seg_start) / bpb).floor() as i64;
            global_bar += bars_in_seg.max(0);
        }
        (1, 1, 4, 4)
    }

    pub fn bar_start_beat_at(&self, beat: f64) -> f64 {
        let (bar, _, _, _) = self.bar_beat_at_beat(beat);
        self.bar_start_beat(bar)
    }

    pub fn bar_start_beat(&self, bar: i64) -> f64 {
        let bar = bar.max(1);
        let points = self.sorted_points();
        let mut global_bar: i64 = 1;

        for (i, pt) in points.iter().enumerate() {
            let seg_start = pt.beat;
            let seg_end = points.get(i + 1).map(|p| p.beat).unwrap_or(f64::INFINITY);
            let bpb = beats_per_bar_from_sig(pt.numerator, pt.denominator);
            let bars_in_seg = if seg_end.is_finite() {
                ((seg_end - seg_start) / bpb).floor() as i64
            } else {
                i64::MAX / 2
            };

            if bar < global_bar + bars_in_seg || i + 1 == points.len() {
                let bar_offset = (bar - global_bar).max(0);
                return seg_start + bar_offset as f64 * bpb;
            }
            global_bar += bars_in_seg;
        }
        0.0
    }

    pub fn metronome_accent_at_beat(&self, beat: f64) -> MetronomeAccent {
        let grouping = self.time_signature_at_beat(beat).grouping_slice();
        let (_, beat_in_bar, _, _) = self.bar_beat_at_beat(beat);
        let denom_index = beat_in_bar.saturating_sub(1);
        if denom_index == 0 {
            return MetronomeAccent::Downbeat;
        }
        if is_group_start(grouping, denom_index) {
            MetronomeAccent::Group
        } else {
            MetronomeAccent::Normal
        }
    }

    pub fn next_metronome_click_at_or_after(&self, beat: f64) -> f64 {
        let beat = beat.max(0.0);
        let pt = self.time_signature_at_beat(beat);
        let denom_unit = denominator_unit_quarter_beats(pt.denominator);
        let bpb = beats_per_bar_from_sig(pt.numerator, pt.denominator);
        let bar_start = self.bar_start_beat_at(beat);
        let rel = beat - bar_start;
        let idx = (rel / denom_unit).floor() as u16;
        let frac = rel - idx as f64 * denom_unit;
        if frac < TS_BEAT_EPSILON {
            return bar_start + idx as f64 * denom_unit;
        }
        let next_idx = idx.saturating_add(1);
        if next_idx < pt.numerator {
            bar_start + next_idx as f64 * denom_unit
        } else {
            bar_start + bpb
        }
    }

    pub fn next_metronome_click_after(&self, beat: f64) -> f64 {
        let beat = beat.max(0.0);
        let pt = self.time_signature_at_beat(beat);
        let denom_unit = denominator_unit_quarter_beats(pt.denominator);
        let bpb = beats_per_bar_from_sig(pt.numerator, pt.denominator);
        let bar_start = self.bar_start_beat_at(beat);
        let rel = beat - bar_start;
        let idx = (rel / denom_unit).floor() as u16;
        let frac = rel - idx as f64 * denom_unit;
        let on_tick = frac < TS_BEAT_EPSILON;
        if on_tick {
            if idx.saturating_add(1) < pt.numerator {
                bar_start + (idx + 1) as f64 * denom_unit
            } else {
                bar_start + bpb
            }
        } else {
            let next_idx = idx.saturating_add(1);
            if next_idx < pt.numerator {
                bar_start + next_idx as f64 * denom_unit
            } else {
                bar_start + bpb
            }
        }
    }

    pub fn is_downbeat(&self, beat: f64) -> bool {
        matches!(
            self.metronome_accent_at_beat(beat),
            MetronomeAccent::Downbeat
        )
    }

    /// The points, already sorted by beat and grouping-normalized by [`sort`].
    ///
    /// This used to clone the whole vector (plus a `Vec<u16>` per point) on
    /// every call, i.e. roughly a dozen heap allocations per metronome click,
    /// on the audio thread. `sort` establishes the normalization once, at
    /// construction, so the click path can read the slice.
    ///
    /// [`sort`]: Self::sort
    #[inline]
    fn sorted_points(&self) -> &[RuntimeTimeSignaturePointSnapshot] {
        &self.points
    }

    /// Normalize + sort in place, and uphold the never-empty invariant.
    fn sort(&mut self) {
        if self.points.is_empty() {
            // A project with no time-signature markers is a real state (the UI
            // resolves an implicit 4/4 for it), and it reaches the engine as an
            // empty point list. Materialize that implicit signature here so the
            // audio thread always has a point to index.
            self.points.push(RuntimeTimeSignaturePointSnapshot {
                beat: 0.0,
                numerator: 4,
                denominator: 4,
                grouping: default_time_signature_grouping(4, 4),
            });
            return;
        }
        for pt in &mut self.points {
            pt.grouping = normalize_grouping(pt.numerator, pt.denominator, &pt.grouping);
        }
        self.points.sort_by(|a, b| {
            a.beat
                .partial_cmp(&b.beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_5_8() -> RuntimeTimeSignatureMapSnapshot {
        RuntimeTimeSignatureMapSnapshot::from_points(vec![RuntimeTimeSignaturePointSnapshot {
            beat: 0.0,
            numerator: 5,
            denominator: 8,
            grouping: vec![2, 3],
        }])
    }

    fn map_6_8() -> RuntimeTimeSignatureMapSnapshot {
        RuntimeTimeSignatureMapSnapshot::from_points(vec![RuntimeTimeSignaturePointSnapshot {
            beat: 0.0,
            numerator: 6,
            denominator: 8,
            grouping: vec![3, 3],
        }])
    }

    #[test]
    fn five_eight_denominator_ticks() {
        let map = map_5_8();
        assert!((map.bar_start_beat(1) - 0.0).abs() < 1e-9);
        assert!((map.bar_start_beat(2) - 2.5).abs() < 1e-9);
        assert_eq!(map.bar_beat_at_beat(0.0), (1, 1, 5, 8));
        assert_eq!(map.bar_beat_at_beat(0.5), (1, 2, 5, 8));
        assert_eq!(map.bar_beat_at_beat(2.0), (1, 5, 5, 8));
        assert_eq!(map.bar_beat_at_beat(2.5), (2, 1, 5, 8));
    }

    #[test]
    fn six_eight_denominator_ticks() {
        let map = map_6_8();
        assert!((map.bar_start_beat(2) - 3.0).abs() < 1e-9);
        assert_eq!(map.bar_beat_at_beat(2.5), (1, 6, 6, 8));
        assert_eq!(map.bar_beat_at_beat(3.0), (2, 1, 6, 8));
    }

    #[test]
    fn five_eight_metronome_click_count() {
        let map = map_5_8();
        let mut beat = 0.0;
        let mut accents = Vec::new();
        for _ in 0..5 {
            accents.push(map.metronome_accent_at_beat(beat));
            beat = map.next_metronome_click_after(beat);
        }
        assert!((beat - 2.5).abs() < 1e-9);
        assert_eq!(
            accents,
            vec![
                MetronomeAccent::Downbeat,
                MetronomeAccent::Normal,
                MetronomeAccent::Group,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
            ]
        );
    }

    #[test]
    fn six_eight_metronome_click_count() {
        let map = map_6_8();
        let mut beat = 0.0;
        let mut accents = Vec::new();
        for _ in 0..6 {
            accents.push(map.metronome_accent_at_beat(beat));
            beat = map.next_metronome_click_after(beat);
        }
        assert!((beat - 3.0).abs() < 1e-9);
        assert_eq!(
            accents,
            vec![
                MetronomeAccent::Downbeat,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
                MetronomeAccent::Group,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
            ]
        );
    }

    #[test]
    fn marker_boundary_meter_change() {
        let map = RuntimeTimeSignatureMapSnapshot::from_points(vec![
            RuntimeTimeSignaturePointSnapshot {
                beat: 0.0,
                numerator: 5,
                denominator: 8,
                grouping: vec![2, 3],
            },
            RuntimeTimeSignaturePointSnapshot {
                beat: 2.5,
                numerator: 6,
                denominator: 8,
                grouping: vec![3, 3],
            },
        ]);
        assert_eq!(map.bar_beat_at_beat(2.0), (1, 5, 5, 8));
        assert_eq!(map.bar_beat_at_beat(2.5), (2, 1, 6, 8));
        let mut beat = 0.0;
        let mut count_5_8 = 0usize;
        while beat < 2.5 - TS_BEAT_EPSILON {
            count_5_8 += 1;
            beat = map.next_metronome_click_after(beat);
        }
        assert_eq!(count_5_8, 5);
        let bar2_end = 2.5 + beats_per_bar_from_sig(6, 8);
        let mut count_6_8 = 0usize;
        while beat < bar2_end - TS_BEAT_EPSILON {
            count_6_8 += 1;
            beat = map.next_metronome_click_after(beat);
        }
        assert_eq!(count_6_8, 6);
    }

    /// Walk a bar the way `metronome_sample` does and collect what it would
    /// play. These three pin the accent/next-click behaviour across the switch
    /// from cloned points to borrowed ones.
    fn walk_bar(
        map: &RuntimeTimeSignatureMapSnapshot,
        start: f64,
        clicks: usize,
    ) -> (Vec<MetronomeAccent>, f64) {
        let mut beat = start;
        let mut accents = Vec::new();
        for _ in 0..clicks {
            accents.push(map.metronome_accent_at_beat(beat));
            beat = map.next_metronome_click_after(beat);
        }
        (accents, beat)
    }

    #[test]
    fn four_four_accents_and_next_clicks() {
        let map = RuntimeTimeSignatureMapSnapshot::static_sig(4, 4);
        let (accents, next) = walk_bar(&map, 0.0, 4);
        assert!((next - 4.0).abs() < 1e-9);
        assert_eq!(
            accents,
            vec![
                MetronomeAccent::Downbeat,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
            ]
        );
    }

    #[test]
    fn seven_eight_accents_and_next_clicks() {
        let map =
            RuntimeTimeSignatureMapSnapshot::from_points(vec![RuntimeTimeSignaturePointSnapshot {
                beat: 0.0,
                numerator: 7,
                denominator: 8,
                // Empty grouping normalizes to the 2+2+3 default.
                grouping: Vec::new(),
            }]);
        let (accents, next) = walk_bar(&map, 0.0, 7);
        assert!((next - 3.5).abs() < 1e-9);
        assert_eq!(
            accents,
            vec![
                MetronomeAccent::Downbeat,
                MetronomeAccent::Normal,
                MetronomeAccent::Group,
                MetronomeAccent::Normal,
                MetronomeAccent::Group,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
            ]
        );
    }

    #[test]
    fn two_point_map_keeps_accents_after_the_marker() {
        let map = RuntimeTimeSignatureMapSnapshot::from_points(vec![
            RuntimeTimeSignaturePointSnapshot {
                beat: 0.0,
                numerator: 4,
                denominator: 4,
                grouping: vec![4],
            },
            RuntimeTimeSignaturePointSnapshot {
                beat: 8.0,
                numerator: 7,
                denominator: 8,
                grouping: vec![2, 2, 3],
            },
        ]);
        let (before, at_marker) = walk_bar(&map, 4.0, 4);
        assert!((at_marker - 8.0).abs() < 1e-9);
        assert_eq!(before[0], MetronomeAccent::Downbeat);
        assert_eq!(before[1], MetronomeAccent::Normal);

        let (after, next_bar) = walk_bar(&map, 8.0, 7);
        assert!((next_bar - 11.5).abs() < 1e-9);
        assert_eq!(
            after,
            vec![
                MetronomeAccent::Downbeat,
                MetronomeAccent::Normal,
                MetronomeAccent::Group,
                MetronomeAccent::Normal,
                MetronomeAccent::Group,
                MetronomeAccent::Normal,
                MetronomeAccent::Normal,
            ]
        );
    }

    /// A project with no time-signature markers reaches the engine as an empty
    /// point list. The audio thread indexes this map, so it must never stay
    /// empty — this is the test that keeps `points[idx]` from panicking in the
    /// device callback.
    #[test]
    fn an_empty_point_list_materializes_four_four() {
        let map = RuntimeTimeSignatureMapSnapshot::from_points(Vec::new());
        assert_eq!(map.points().len(), 1);
        assert_eq!(map.bar_beat_at_beat(0.0), (1, 1, 4, 4));
        assert!(map.is_downbeat(0.0));
        assert!((map.next_metronome_click_after(0.0) - 1.0).abs() < 1e-9);
        assert_eq!(
            RuntimeTimeSignatureMapSnapshot::default().points().len(),
            1,
            "Default must satisfy the never-empty invariant too"
        );
    }

    #[test]
    fn downbeat_detection_with_changing_signatures() {
        let map = RuntimeTimeSignatureMapSnapshot::from_points(vec![
            RuntimeTimeSignaturePointSnapshot {
                beat: 0.0,
                numerator: 4,
                denominator: 4,
                grouping: vec![4],
            },
            RuntimeTimeSignaturePointSnapshot {
                beat: 16.0,
                numerator: 3,
                denominator: 4,
                grouping: vec![3],
            },
        ]);
        assert!(map.is_downbeat(0.0));
        assert!(map.is_downbeat(16.0));
        assert!(map.is_downbeat(19.0));
        assert!(!map.is_downbeat(17.0));
    }
}
