//! Project timebase: which unit the timeline reads positions in.
//!
//! This changes how a position is *shown and labelled*, never where anything
//! sits. The arrangement's coordinate model stays musical — beats drive
//! `beat_to_x`, the snap grid, clip layout and hit-testing exactly as before —
//! so switching timebase can never move a clip. Only the ruler's tick spacing
//! and every position readout follow the choice.
//!
//! Real elapsed time is read through the tempo map, not from `pixels_per_second`
//! (which is a zoom factor at the base tempo). With tempo automation those two
//! disagree, and only the tempo map is right.

use super::TempoMap;

/// Unit the ruler and every position readout are expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeDisplayFormat {
    /// Musical position, `bar.beat`. The project default.
    #[default]
    BarsBeats,
    /// Elapsed wall-clock time, `m:ss.mmm` / `h:mm:ss.mmm`.
    Seconds,
    /// SMPTE-style `hh:mm:ss:ff` at the project frame rate.
    Timecode,
    /// Absolute sample index at the project sample rate.
    Samples,
}

impl TimeDisplayFormat {
    /// Every format, in the order the Project Settings dropdown lists them.
    pub const ALL: [Self; 4] = [
        Self::BarsBeats,
        Self::Seconds,
        Self::Timecode,
        Self::Samples,
    ];

    /// Stable persistence tag. Never renumber — these are written into project
    /// files.
    pub fn to_tag(self) -> u8 {
        match self {
            Self::BarsBeats => 0,
            Self::Seconds => 1,
            Self::Timecode => 2,
            Self::Samples => 3,
        }
    }

    pub fn from_tag(tag: u8) -> Self {
        match tag {
            1 => Self::Seconds,
            2 => Self::Timecode,
            3 => Self::Samples,
            _ => Self::BarsBeats,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::BarsBeats => "Bars+Beats",
            Self::Seconds => "Seconds",
            Self::Timecode => "Timecode",
            Self::Samples => "Samples",
        }
    }

    /// Whether positions in this format are spaced by real elapsed time rather
    /// than by musical position. Drives which ruler tick generator runs.
    pub fn is_time_based(self) -> bool {
        !matches!(self, Self::BarsBeats)
    }
}

/// Frame rate used to render [`TimeDisplayFormat::Timecode`].
///
/// All non-drop: the frame counter is `floor(seconds * fps)` with no dropped
/// frame numbers, so 29.97 drifts from wall clock exactly as non-drop timecode
/// is defined to. The label says so rather than implying drop-frame accuracy the
/// project does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimecodeRate {
    Fps23976,
    Fps24,
    Fps25,
    Fps2997,
    #[default]
    Fps30,
}

impl TimecodeRate {
    pub const ALL: [Self; 5] = [
        Self::Fps23976,
        Self::Fps24,
        Self::Fps25,
        Self::Fps2997,
        Self::Fps30,
    ];

    /// Stable persistence tag. Never renumber.
    pub fn to_tag(self) -> u8 {
        match self {
            Self::Fps23976 => 0,
            Self::Fps24 => 1,
            Self::Fps25 => 2,
            Self::Fps2997 => 3,
            Self::Fps30 => 4,
        }
    }

    pub fn from_tag(tag: u8) -> Self {
        match tag {
            0 => Self::Fps23976,
            1 => Self::Fps24,
            2 => Self::Fps25,
            3 => Self::Fps2997,
            _ => Self::Fps30,
        }
    }

    pub fn fps(self) -> f64 {
        match self {
            Self::Fps23976 => 24_000.0 / 1001.0,
            Self::Fps24 => 24.0,
            Self::Fps25 => 25.0,
            Self::Fps2997 => 30_000.0 / 1001.0,
            Self::Fps30 => 30.0,
        }
    }

    /// Frames per second rounded to the integer the counter wraps at — 29.97
    /// counts 0..=29 like 30, it just takes slightly longer to get there.
    pub fn frame_wrap(self) -> u32 {
        match self {
            Self::Fps23976 | Self::Fps24 => 24,
            Self::Fps25 => 25,
            Self::Fps2997 | Self::Fps30 => 30,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fps23976 => "23.976 fps",
            Self::Fps24 => "24 fps",
            Self::Fps25 => "25 fps",
            Self::Fps2997 => "29.97 fps (ND)",
            Self::Fps30 => "30 fps",
        }
    }
}

/// `m:ss.mmm`, widening to `h:mm:ss.mmm` past an hour. Negative input clamps to
/// zero: there is no position before the start of the project.
pub fn format_seconds(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let total_ms = (seconds * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// `hh:mm:ss:ff` at `rate`. Frames are truncated, not rounded, so the displayed
/// frame is the one actually being shown at that instant.
pub fn format_timecode(seconds: f64, rate: TimecodeRate) -> String {
    let seconds = seconds.max(0.0);
    let wrap = rate.frame_wrap().max(1);
    let total_frames = (seconds * rate.fps()).floor() as u64;
    let frames = total_frames % wrap as u64;
    let total_s = total_frames / wrap as u64;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h:02}:{m:02}:{s:02}:{frames:02}")
}

/// Absolute sample index. Plain digits — a separator would fight the tabular
/// figures the ruler and transport readouts are set in.
pub fn format_samples(seconds: f64, sample_rate: u32) -> String {
    let samples = (seconds.max(0.0) * sample_rate.max(1) as f64).round() as u64;
    samples.to_string()
}

/// Ruler tick spacing, in real seconds, for a time-based timebase.
///
/// `major` carries the labels; `minor` is the unlabelled tick between them.
/// Both are chosen so a label never lands closer than `min_label_px` to its
/// neighbour, which is the same rule the musical ruler enforces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeRulerStep {
    pub major: f64,
    pub minor: f64,
}

/// Steps that read as round numbers to a musician: fractions of a second, then
/// seconds, then the usual clock divisions.
const TIME_STEPS_SECONDS: [f64; 17] = [
    0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    1800.0,
];

/// Choose the tick spacing for `pixels_per_second` of real time.
///
/// For [`TimeDisplayFormat::Timecode`] the ladder is extended downward with
/// whole-frame steps, so a zoomed-in timecode ruler lands on frame boundaries
/// instead of on arbitrary fractions of a second.
pub fn resolve_time_ruler_step(
    pixels_per_second: f64,
    min_label_px: f64,
    frame_seconds: Option<f64>,
) -> TimeRulerStep {
    let pps = pixels_per_second.max(1.0e-6);
    let min_label_px = min_label_px.max(8.0);

    // Frame-aligned candidates first, so a timecode ruler never labels a
    // position that is not a whole frame.
    let mut candidates: Vec<f64> = Vec::with_capacity(TIME_STEPS_SECONDS.len() + 3);
    if let Some(frame) = frame_seconds.filter(|f| *f > 0.0) {
        candidates.extend([frame, frame * 5.0, frame * 10.0]);
    }
    candidates.extend(TIME_STEPS_SECONDS);

    let major = candidates
        .iter()
        .copied()
        .find(|step| step * pps >= min_label_px)
        // Past the top of the ladder, keep doubling the largest step rather
        // than collapsing every label onto one position.
        .unwrap_or_else(|| {
            let mut step = *candidates.last().unwrap_or(&1800.0);
            while step * pps < min_label_px && step < 1.0e9 {
                step *= 2.0;
            }
            step
        });

    // One unlabelled tick per major division, only while it stays legible.
    let minor = if major * pps >= 40.0 {
        major / 5.0
    } else if major * pps >= 20.0 {
        major / 2.0
    } else {
        major
    };
    TimeRulerStep { major, minor }
}

/// Real elapsed seconds at a musical beat, through the project's tempo map.
#[inline]
pub fn seconds_at_beat(tempo_map: &TempoMap, beat: f64, base_bpm: f64) -> f64 {
    tempo_map.seconds_at_beat(beat, base_bpm.max(1.0))
}

/// Musical beat at a real elapsed time, through the project's tempo map.
#[inline]
pub fn beat_at_seconds(tempo_map: &TempoMap, seconds: f64, base_bpm: f64) -> f64 {
    tempo_map.beat_at_seconds(seconds.max(0.0), base_bpm.max(1.0))
}

/// One Linear-timebase clip's wall-clock position, captured before a tempo
/// change so it can be put back afterwards.
///
/// Keyed by id rather than by index: a tempo edit does not add or remove clips,
/// but keying by position would silently re-anchor the wrong clip the first time
/// one ever did.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearClipAnchor {
    pub track_id: String,
    pub clip_id: String,
    pub start_seconds: f64,
    /// `None` for audio clips: their length is owned by
    /// `TimelineState::reconcile_audio_clip_lengths`, which already keeps
    /// wall-clock length for every stretch mode except Tempo Sync. Re-anchoring
    /// it here too would fight that and double-apply the correction.
    pub duration_seconds: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tags_round_trip() {
        for format in TimeDisplayFormat::ALL {
            assert_eq!(TimeDisplayFormat::from_tag(format.to_tag()), format);
        }
        for rate in TimecodeRate::ALL {
            assert_eq!(TimecodeRate::from_tag(rate.to_tag()), rate);
        }
        // An unknown tag from a newer project must land on the default rather
        // than on whatever variant happens to be first.
        assert_eq!(
            TimeDisplayFormat::from_tag(200),
            TimeDisplayFormat::BarsBeats
        );
        assert_eq!(TimecodeRate::from_tag(200), TimecodeRate::Fps30);
    }

    #[test]
    fn seconds_widen_to_hours_only_past_an_hour() {
        assert_eq!(format_seconds(0.0), "0:00.000");
        assert_eq!(format_seconds(9.5), "0:09.500");
        assert_eq!(format_seconds(61.25), "1:01.250");
        assert_eq!(format_seconds(3600.0), "1:00:00.000");
        // Before the project start is not a position.
        assert_eq!(format_seconds(-5.0), "0:00.000");
    }

    #[test]
    fn timecode_counts_frames_at_the_project_rate() {
        assert_eq!(format_timecode(0.0, TimecodeRate::Fps30), "00:00:00:00");
        // 30 fps: half a second is frame 15.
        assert_eq!(format_timecode(0.5, TimecodeRate::Fps30), "00:00:00:15");
        // The frame counter wraps at the rate, carrying into seconds.
        assert_eq!(format_timecode(1.0, TimecodeRate::Fps30), "00:00:01:00");
        assert_eq!(format_timecode(1.0, TimecodeRate::Fps25), "00:00:01:00");
        assert_eq!(format_timecode(0.5, TimecodeRate::Fps25), "00:00:00:12");
        assert_eq!(format_timecode(3661.0, TimecodeRate::Fps24), "01:01:01:00");
        // Truncated, not rounded: the frame being shown, not the nearest one.
        assert_eq!(format_timecode(0.999, TimecodeRate::Fps30), "00:00:00:29");
    }

    #[test]
    fn samples_follow_the_project_rate() {
        assert_eq!(format_samples(1.0, 48_000), "48000");
        assert_eq!(format_samples(0.5, 44_100), "22050");
        assert_eq!(format_samples(-1.0, 48_000), "0");
    }

    #[test]
    fn ruler_step_grows_until_labels_fit() {
        // Zoomed in: sub-second steps are legible.
        let tight = resolve_time_ruler_step(500.0, 64.0, None);
        assert!(tight.major <= 0.25, "got {}", tight.major);
        // Zoomed out: steps climb into minutes rather than overlapping.
        let wide = resolve_time_ruler_step(0.5, 64.0, None);
        assert!(wide.major >= 120.0, "got {}", wide.major);
        // Every chosen step must actually clear the label spacing.
        for pps in [0.05, 1.0, 7.5, 60.0, 400.0, 5000.0] {
            let step = resolve_time_ruler_step(pps, 64.0, None);
            assert!(
                step.major * pps >= 64.0,
                "pps={pps} step={} too tight",
                step.major
            );
        }
    }

    #[test]
    fn timecode_ruler_steps_land_on_whole_frames() {
        let frame = 1.0 / 30.0;
        // Zoomed far enough in that a sub-second step is chosen, it must be a
        // whole number of frames.
        let step = resolve_time_ruler_step(2000.0, 64.0, Some(frame));
        let frames = step.major / frame;
        assert!(
            (frames - frames.round()).abs() < 1.0e-9,
            "step {} is {frames} frames",
            step.major
        );
    }

    #[test]
    fn only_bars_beats_is_musical() {
        assert!(!TimeDisplayFormat::BarsBeats.is_time_based());
        assert!(TimeDisplayFormat::Seconds.is_time_based());
        assert!(TimeDisplayFormat::Timecode.is_time_based());
        assert!(TimeDisplayFormat::Samples.is_time_based());
    }
}
