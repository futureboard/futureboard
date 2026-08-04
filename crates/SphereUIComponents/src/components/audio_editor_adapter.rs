//! Builds audio editor view models from timeline + peak cache (no PCM).

use sphere_audio_editor::{WaveformColumn, WaveformViewModel};

use crate::components::timeline::timeline_state::{
    clip_output_local_to_source_sample, AudioImportState, ClipState, ClipType, TimelineState,
};
use crate::components::timeline::waveform_cache::{self, WaveformDisplayStatus, WaveformPeak};
use crate::theme::Colors;

const MAX_EDITOR_COLUMNS: usize = 2048;

pub fn import_status_label(import: &AudioImportState) -> (String, bool, bool) {
    match import {
        AudioImportState::Pending => ("Queued".to_string(), false, true),
        AudioImportState::Probing => ("Probing metadata…".to_string(), false, true),
        AudioImportState::Decoding { .. } => ("Preparing waveform…".to_string(), false, true),
        AudioImportState::GeneratingPeaks { progress } => {
            let pct = ((*progress * 100.0) as u32).min(100);
            (format!("Building waveform… {pct}%"), false, true)
        }
        AudioImportState::Ready => ("Ready".to_string(), false, false),
        AudioImportState::Failed { message } => (message.clone(), true, false),
    }
}

pub fn build_waveform_view_model(
    clip: &ClipState,
    state: &TimelineState,
    pixels_per_beat: f32,
    scroll_x: f32,
    viewport_width: f32,
) -> WaveformViewModel {
    let (label, is_error, show_progress) = import_status_label(&clip.audio_import);

    let ClipType::Audio {
        source_path: Some(path),
        ..
    } = &clip.clip_type
    else {
        return WaveformViewModel::loading("No source file");
    };

    waveform_cache::with_file_entry(path, |entry| {
        let Some(entry) = entry else {
            return WaveformViewModel {
                columns: Vec::new(),
                ready: false,
                status_label: label.clone(),
                is_error,
                show_progress,
            };
        };

        match waveform_cache::display_status_from_entry(entry) {
            WaveformDisplayStatus::Ready { meta } | WaveformDisplayStatus::Partial { meta, .. } => {
                let columns = build_peak_columns(
                    entry,
                    meta.as_ref(),
                    clip,
                    state,
                    pixels_per_beat,
                    scroll_x,
                    viewport_width,
                );
                let ready = !columns.is_empty();
                WaveformViewModel {
                    ready,
                    columns,
                    status_label: if ready { String::new() } else { label },
                    is_error: false,
                    show_progress: !ready && show_progress,
                }
            }
            WaveformDisplayStatus::Pending => WaveformViewModel {
                columns: Vec::new(),
                ready: false,
                status_label: label,
                is_error: false,
                show_progress: true,
            },
            WaveformDisplayStatus::Error(message) => WaveformViewModel::error(message),
        }
    })
}

fn build_peak_columns(
    entry: &waveform_cache::FileEntry,
    meta: &waveform_cache::WaveformFileMeta,
    clip: &ClipState,
    state: &TimelineState,
    pixels_per_beat: f32,
    scroll_x: f32,
    viewport_width: f32,
) -> Vec<WaveformColumn> {
    let clip_width_px = (clip.duration_beats * pixels_per_beat).max(1.0);
    let visible_start = scroll_x.max(0.0);
    let visible_end = (scroll_x + viewport_width).min(clip_width_px);
    let visible_w = (visible_end - visible_start).max(1.0);
    let num_cols = (visible_w.ceil() as usize).clamp(16, MAX_EDITOR_COLUMNS);

    let seconds_per_beat = state.seconds_per_beat();
    let effective_time_ratio = clip.stretch.effective_time_ratio(state.bpm as f64);
    let source_start = clip.stretch.source_start_samples;
    let source_end = if clip.stretch.source_end_samples > source_start {
        clip.stretch.source_end_samples
    } else {
        let source_duration_beats =
            clip.duration_beats.max(0.0) as f64 / effective_time_ratio.max(1e-6);
        ((clip.offset_beats.max(0.0) as f64 + source_duration_beats)
            * seconds_per_beat as f64
            * meta.sample_rate as f64)
            .round()
            .max(source_start as f64) as u64
    };
    let output_len = SphereAudioProcessor::stretched_duration_samples(
        source_end.saturating_sub(source_start),
        &clip.stretch.to_sphere_stretch_params(state.bpm as f64),
        Some(state.bpm),
    )
    .max(1) as f64;

    let pixels_per_second = pixels_per_beat / seconds_per_beat.max(1e-6);
    let desired_spp =
        waveform_cache::pick_best_samples_per_peak(pixels_per_second, meta.sample_rate);
    let spp = waveform_cache::best_available_samples_per_peak_in_entry(entry, desired_spp);

    (0..num_cols)
        .filter_map(|col| {
            let x0 = visible_start + (col as f32 / num_cols as f32) * visible_w;
            let x1 = visible_start + ((col + 1) as f32 / num_cols as f32) * visible_w;
            let frac0 = (x0 / clip_width_px).clamp(0.0, 1.0) as f64;
            let frac1 = (x1 / clip_width_px).clamp(0.0, 1.0) as f64;
            let out0 = frac0 * output_len;
            let out1 = frac1 * output_len;
            let s0 = clip_output_local_to_source_sample(
                out0,
                source_start,
                source_end,
                effective_time_ratio,
                clip.stretch.reverse,
            );
            let s1 = clip_output_local_to_source_sample(
                out1,
                source_start,
                source_end,
                effective_time_ratio,
                clip.stretch.reverse,
            );
            let p0 = sample_to_peak_index(s0.min(s1), spp);
            let p1 = sample_to_peak_index(s0.max(s1), spp).max(p0);
            let WaveformPeak { min, max } =
                waveform_cache::aggregate_peak_range_in_entry(entry, spp, p0, p1 + 1);
            if min == 0.0 && max == 0.0 {
                return None;
            }
            Some(WaveformColumn {
                x: x0 - scroll_x,
                min,
                max,
            })
        })
        .collect()
}

fn sample_to_peak_index(sample: f64, samples_per_peak: usize) -> usize {
    sample.max(0.0) as usize / samples_per_peak.max(1)
}

pub fn audio_editor_theme() -> sphere_audio_editor::AudioEditorTheme {
    sphere_audio_editor::AudioEditorTheme {
        surface_base: Colors::surface_base(),
        surface_panel: Colors::surface_panel(),
        text_primary: Colors::text_primary(),
        text_secondary: Colors::text_secondary(),
        text_muted: Colors::text_muted(),
        border_subtle: Colors::border_subtle(),
        accent: Colors::accent_primary(),
        playhead: Colors::timeline_playhead(),
        error: Colors::status_error(),
        selection: Colors::accent_primary(),
    }
}

pub fn selected_audio_clip<'a>(
    state: &'a TimelineState,
) -> Option<(
    &'a crate::components::timeline::timeline_state::TrackState,
    &'a ClipState,
)> {
    let clip_id = state.selection.selected_clip_ids.first()?;
    let (track, clip) = state.find_clip(clip_id)?;
    match clip.clip_type {
        ClipType::Audio { .. } => Some((track, clip)),
        ClipType::Midi { .. } | ClipType::Video { .. } => None,
    }
}

pub fn clip_type_hint_for_selection(
    state: &TimelineState,
) -> Option<sphere_audio_editor::ClipTypeHint> {
    let clip_id = state.selection.selected_clip_ids.first()?;
    let (_, clip) = state.find_clip(clip_id)?;
    match clip.clip_type {
        ClipType::Audio { .. } => Some(sphere_audio_editor::ClipTypeHint::Audio),
        ClipType::Midi { .. } => Some(sphere_audio_editor::ClipTypeHint::Midi),
        // The audio editor has nothing to show for a reference video clip.
        ClipType::Video { .. } => None,
    }
}
