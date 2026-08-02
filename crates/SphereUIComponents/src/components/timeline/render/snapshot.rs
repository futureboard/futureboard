//! Immutable render snapshots built on the UI thread from [`TimelineState`].
//!
//! Renderers must treat these as read-only: no audio decode, no peak generation.

use std::sync::Arc;

use super::viewport::TimelineViewport;
use crate::components::timeline::timeline_state::{
    clip_output_local_to_source_sample, is_arrangement_hidden_track, ClipState, ClipType,
    GridLineLevel, TimelineState, TrackRowLayoutEntry, TrackState, DEFAULT_TRACK_HEIGHT,
};
use crate::components::timeline::waveform_cache::{
    self, WaveformDisplayStatus, CHUNK_PEAKS, PEAK_FINE_SPP,
};
use crate::components::timeline::waveform_canvas::resolve_waveform_source_range;
use gpui::Rgba;

/// Track rows included in this snapshot (after vertical virtualization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleTrackRange {
    pub start_index: usize,
    pub end_index: usize,
}

/// Beat interval visible in the lane viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleBeatRange {
    pub start_beat: f32,
    pub end_beat: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderClipKind {
    Audio,
    Midi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformReadyKind {
    Pending,
    Partial,
    Ready,
    Error,
}

/// Opaque handle to precomputed peak chunks — WGPU path binds GPU buffers from cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveformChunkHandle {
    /// Stable waveform-cache key (clip `file_id` / asset id), not the on-disk
    /// path — so the GPU binding survives a `source_path` rewrite.
    pub asset_key: String,
    pub samples_per_peak: u32,
    pub chunk_index_start: u32,
    pub chunk_index_end: u32,
    pub peak_index_start: usize,
    pub peak_index_end: usize,
    pub ready: WaveformReadyKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderClipSnapshot {
    pub id: String,
    pub track_id: String,
    pub track_index: usize,
    pub name: String,
    pub kind: RenderClipKind,
    pub color: [f32; 4],
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub selected: bool,
    pub muted: bool,
    pub waveform: Option<WaveformChunkHandle>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderLaneSnapshot {
    pub track_index: usize,
    pub track_id: String,
    pub y: f32,
    pub height: f32,
    pub even_row: bool,
    pub selected: bool,
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLineSnapshot {
    pub x: f32,
    pub beat: f32,
    pub level: GridLineLevel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarShadeSnapshot {
    pub x: f32,
    pub width: f32,
    pub bar: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayheadSnapshot {
    pub beat: f32,
    pub x: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionSnapshot {
    pub selected_track_id: Option<String>,
    pub selected_clip_ids: Vec<String>,
}

/// Immutable description of one arrangement paint pass.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRenderSnapshot {
    pub viewport: TimelineViewport,
    pub bpm: f32,
    pub beats_per_bar: f32,
    pub time_signature_revision: u64,
    pub visible_tracks: VisibleTrackRange,
    pub visible_beats: VisibleBeatRange,
    pub lanes: Vec<RenderLaneSnapshot>,
    pub clips: Vec<RenderClipSnapshot>,
    pub grid_lines: Vec<GridLineSnapshot>,
    pub bar_shades: Vec<BarShadeSnapshot>,
    pub playhead: PlayheadSnapshot,
    pub selection: SelectionSnapshot,
    pub track_insert_y: Option<f32>,
}

pub struct SnapshotBuildOptions {
    pub scale_factor: f32,
    pub track_overscan: usize,
}

impl Default for SnapshotBuildOptions {
    fn default() -> Self {
        Self {
            scale_factor: 1.0,
            track_overscan: 2,
        }
    }
}

impl TimelineRenderSnapshot {
    pub fn from_state(state: &TimelineState, options: SnapshotBuildOptions) -> Self {
        let grid_width = state.viewport.viewport_width.max(1.0);
        let grid_height = state.viewport.viewport_height.max(DEFAULT_TRACK_HEIGHT);
        let seconds_per_beat = state.seconds_per_beat();
        let pixels_per_beat = state.viewport.pixels_per_second * seconds_per_beat;

        let viewport = TimelineViewport::new(
            grid_width,
            grid_height,
            options.scale_factor,
            state.viewport.scroll_x,
            state.viewport.scroll_y,
            pixels_per_beat,
            state.viewport.pixels_per_second,
            seconds_per_beat,
        );

        // Row layout is O(track_count) and owns cloned track ids. Build it once
        // per snapshot; 1k-track sessions previously rebuilt it four times for
        // visibility, lanes, clips, and the drag insertion marker.
        let row_layout = state.track_row_layout();
        let visible_tracks = visible_track_range(state, &row_layout, options.track_overscan);
        let visible_beats = VisibleBeatRange {
            start_beat: viewport.visible_beat_range().0,
            end_beat: viewport.visible_beat_range().1,
        };

        let lanes = build_lanes(state, &row_layout, &visible_tracks);
        let clips = build_clips(state, &row_layout, &visible_tracks, &viewport);
        let grid_lines = state
            .get_arrangement_grid_lines(grid_width)
            .into_iter()
            .map(|line| GridLineSnapshot {
                x: line.x,
                beat: line.beat,
                level: line.level,
            })
            .collect();
        let bar_shades = build_bar_shades(state, &viewport);

        let playhead = PlayheadSnapshot {
            beat: state.transport.playhead_beats,
            x: viewport.beat_to_x(state.transport.playhead_beats),
        };

        let selection = SelectionSnapshot {
            selected_track_id: state.selection.selected_track_id.clone(),
            selected_clip_ids: state.selection.selected_clip_ids.clone(),
        };

        let track_insert_y = state.drag_target_index.and_then(|index| {
            row_layout.row_for_index(index).map(|row| {
                (row.y - state.viewport.scroll_y).clamp(0.0, grid_height.max(DEFAULT_TRACK_HEIGHT))
            })
        });

        Self {
            viewport,
            bpm: state.bpm,
            beats_per_bar: state.beats_per_bar(),
            time_signature_revision: state.time_signature_map.revision(),
            visible_tracks,
            visible_beats,
            lanes,
            clips,
            grid_lines,
            bar_shades,
            playhead,
            selection,
            track_insert_y,
        }
    }
}

fn visible_track_range(
    state: &TimelineState,
    row_layout: &crate::components::timeline::timeline_state::TrackRowLayout,
    overscan: usize,
) -> VisibleTrackRange {
    let track_count = row_layout.rows.len();
    if track_count == 0 {
        return VisibleTrackRange {
            start_index: 0,
            end_index: 0,
        };
    }
    let scroll_y = state.viewport.scroll_y;
    let viewport_height = state.viewport.viewport_height;
    let (visible_start, visible_end, _, _) =
        crate::components::timeline::track_resize::visible_track_row_range(
            row_layout,
            scroll_y,
            viewport_height,
            overscan,
        );
    VisibleTrackRange {
        start_index: visible_start,
        end_index: visible_end,
    }
}

fn build_lanes(
    state: &TimelineState,
    row_layout: &crate::components::timeline::timeline_state::TrackRowLayout,
    range: &VisibleTrackRange,
) -> Vec<RenderLaneSnapshot> {
    state.tracks[range.start_index..range.end_index]
        .iter()
        .enumerate()
        // Mixer-only channels (Bus/Return + VSTi multi-out children) are excluded
        // from the arrangement canvas the same way the GPUI track list does.
        .filter(|(_, track)| !is_arrangement_hidden_track(track))
        .map(|(rel, track)| {
            let index = range.start_index + rel;
            let row = row_layout
                .row_for_track(&track.id)
                .cloned()
                .unwrap_or_else(|| TrackRowLayoutEntry {
                    track_id: track.id.clone(),
                    index,
                    y: index as f32 * DEFAULT_TRACK_HEIGHT,
                    height: DEFAULT_TRACK_HEIGHT,
                    automation_height: 0.0,
                });
            let y = row.y - state.viewport.scroll_y;
            RenderLaneSnapshot {
                track_index: index,
                track_id: track.id.clone(),
                y,
                height: row.height,
                even_row: index % 2 == 0,
                selected: state.selection.selected_track_id.as_deref() == Some(track.id.as_str()),
                color: rgba_to_array(track.color),
            }
        })
        .collect()
}

fn build_clips(
    state: &TimelineState,
    row_layout: &crate::components::timeline::timeline_state::TrackRowLayout,
    range: &VisibleTrackRange,
    viewport: &TimelineViewport,
) -> Vec<RenderClipSnapshot> {
    let mut clips = Vec::new();
    let pad = 7.0_f32;
    for (rel, track) in state.tracks[range.start_index..range.end_index]
        .iter()
        .enumerate()
    {
        // Mixer-only channels never carry arrangement clips.
        if is_arrangement_hidden_track(track) {
            continue;
        }
        let track_index = range.start_index + rel;
        let row = row_layout
            .row_for_track(&track.id)
            .cloned()
            .unwrap_or_else(|| TrackRowLayoutEntry {
                track_id: track.id.clone(),
                index: track_index,
                y: track_index as f32 * DEFAULT_TRACK_HEIGHT,
                height: DEFAULT_TRACK_HEIGHT,
                automation_height: 0.0,
            });
        let clip_h = row.height - pad * 2.0;
        for clip in &track.clips {
            let clip_left = viewport.beat_to_x(clip.start_beat);
            let clip_width =
                (clip.duration_beats * viewport.seconds_per_beat * viewport.pixels_per_second)
                    .max(10.0);
            if clip_left + clip_width < 0.0 || clip_left > viewport.width {
                continue;
            }
            let clip_y = row.y - state.viewport.scroll_y + pad;
            clips.push(build_clip_snapshot(
                clip,
                track,
                track_index,
                clip_left,
                clip_y,
                clip_width,
                clip_h,
                state,
                viewport,
            ));
        }
    }
    clips
}

fn build_clip_snapshot(
    clip: &ClipState,
    track: &TrackState,
    track_index: usize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    state: &TimelineState,
    viewport: &TimelineViewport,
) -> RenderClipSnapshot {
    let kind = match clip.clip_type {
        ClipType::Audio { .. } => RenderClipKind::Audio,
        ClipType::Midi { .. } => RenderClipKind::Midi,
    };
    let waveform = clip
        .audio_asset_key()
        .map(|asset_key| waveform_handle_for_clip(asset_key, clip, state, viewport));

    RenderClipSnapshot {
        id: clip.id.clone(),
        track_id: track.id.clone(),
        track_index,
        name: clip.name.clone(),
        kind,
        color: rgba_to_array(track.color),
        x,
        y,
        width,
        height,
        selected: state.selection.selected_clip_ids.contains(&clip.id),
        muted: clip.muted,
        waveform,
    }
}

fn waveform_handle_for_clip(
    asset_key: &str,
    clip: &ClipState,
    state: &TimelineState,
    viewport: &TimelineViewport,
) -> WaveformChunkHandle {
    let status = waveform_cache::get_file_status(asset_key);
    let ready = match &status {
        WaveformDisplayStatus::Ready { .. } => WaveformReadyKind::Ready,
        WaveformDisplayStatus::Partial { .. } => WaveformReadyKind::Partial,
        WaveformDisplayStatus::Pending => WaveformReadyKind::Pending,
        WaveformDisplayStatus::Error(_) => WaveformReadyKind::Error,
    };

    let (samples_per_peak, peak_count) = match status {
        WaveformDisplayStatus::Ready { meta } | WaveformDisplayStatus::Partial { meta, .. } => {
            let spp = waveform_cache::pick_best_samples_per_peak(
                viewport.pixels_per_second,
                meta.sample_rate,
            ) as u32;
            (spp, meta.peak_count)
        }
        _ => (PEAK_FINE_SPP as u32, 0),
    };

    let sample_rate = waveform_cache::get_file_status(asset_key)
        .ready_meta()
        .map(|m| m.sample_rate)
        .unwrap_or(48_000);
    let total_frames = waveform_cache::get_file_status(asset_key)
        .ready_meta()
        .map(|m| m.total_frames)
        .unwrap_or(clip.stretch.original_duration_samples);
    let (source_start, source_end, effective_time_ratio) =
        resolve_waveform_source_range(clip, total_frames, sample_rate, state.bpm as f64);
    let output_len =
        (source_end.saturating_sub(source_start) as f64 * effective_time_ratio).max(1.0);
    let s0 = clip_output_local_to_source_sample(
        0.0,
        source_start,
        source_end,
        effective_time_ratio,
        clip.stretch.reverse,
    );
    let s1 = clip_output_local_to_source_sample(
        output_len,
        source_start,
        source_end,
        effective_time_ratio,
        clip.stretch.reverse,
    );
    let p0 = sample_to_peak_index(s0.min(s1), samples_per_peak as usize);
    let p1 = sample_to_peak_index(s0.max(s1), samples_per_peak as usize)
        .max(p0)
        .min(peak_count.saturating_sub(1));

    WaveformChunkHandle {
        asset_key: asset_key.to_string(),
        samples_per_peak,
        chunk_index_start: (p0 / CHUNK_PEAKS) as u32,
        chunk_index_end: (p1 / CHUNK_PEAKS) as u32,
        peak_index_start: p0,
        peak_index_end: p1,
        ready,
    }
}

fn sample_to_peak_index(sample: f64, samples_per_peak: usize) -> usize {
    sample.max(0.0) as usize / samples_per_peak.max(1)
}

fn build_bar_shades(state: &TimelineState, viewport: &TimelineViewport) -> Vec<BarShadeSnapshot> {
    let (visible_start, visible_end) = viewport.visible_beat_range();
    let rects = state
        .time_signature_map
        .visible_bar_rects(visible_start as f64, visible_end as f64);
    let mut shades = Vec::with_capacity(rects.len());
    for rect in rects {
        // Alternate by global bar number: even bars get the subtle region fill.
        if rect.bar % 2 != 0 {
            continue;
        }
        let x0 = viewport.beat_to_x(rect.start_beat as f32);
        let x1 = viewport.beat_to_x(rect.end_beat as f32);
        let width = x1 - x0;
        if width < 2.0 {
            continue;
        }
        shades.push(BarShadeSnapshot {
            x: x0.round(),
            width: width.round(),
            bar: rect.bar,
        });
    }
    shades
}

fn rgba_to_array(c: Rgba) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

trait WaveformStatusExt {
    fn ready_meta(&self) -> Option<&Arc<waveform_cache::WaveformFileMeta>>;
}

impl WaveformStatusExt for WaveformDisplayStatus {
    fn ready_meta(&self) -> Option<&Arc<waveform_cache::WaveformFileMeta>> {
        match self {
            WaveformDisplayStatus::Ready { meta } | WaveformDisplayStatus::Partial { meta, .. } => {
                Some(meta)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod stress_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{
        CreateTrackOptions, InputMonitorMode, TrackType,
    };

    #[test]
    fn snapshot_virtualizes_one_thousand_tracks() {
        let mut state = TimelineState::default();
        state.viewport.viewport_width = 1_200.0;
        state.viewport.viewport_height = 500.0;
        for index in 0..1_000 {
            state.create_track(CreateTrackOptions {
                track_type: if index % 2 == 0 {
                    TrackType::Audio
                } else {
                    TrackType::Midi
                },
                name: format!("Track {index}"),
                color: crate::theme::Colors::track_color_for_index(index),
                volume: 1.0,
                pan: 0.0,
                armed: false,
                input_monitor: InputMonitorMode::Off,
            });
        }

        let snapshot = TimelineRenderSnapshot::from_state(&state, SnapshotBuildOptions::default());
        assert_eq!(state.tracks.len(), 1_000);
        assert!(snapshot.lanes.len() <= 12, "only viewport rows are drawn");
        assert!(
            snapshot.visible_tracks.end_index - snapshot.visible_tracks.start_index <= 12,
            "vertical overscan must stay bounded independently of track count"
        );
    }
}
