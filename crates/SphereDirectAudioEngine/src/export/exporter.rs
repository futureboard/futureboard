//! Arrangement exporter: render offline → encode with `sphere_encoder` → write
//! atomically. Streaming, cancellable, progress-reporting.

use std::path::{Path, PathBuf};

use sphere_encoder::{
    create_encoder, AudioEncodeOptions, AudioEncodeSpec, AudioFileFormat, AudioSampleFormat,
};

use crate::plugin_bridge::PluginBridgeSinkMap;
use crate::types::EngineProjectSnapshot;

use super::offline_renderer::{render_offline_tracks_with_bridges, render_offline_with_bridges};
use super::render_progress::{ExportCancelToken, ExportProgress, ExportStage};
use super::render_request::{ExportNormalizeMode, OfflineRenderRequest};
use super::ExportError;

/// A full arrangement export request: where to write, in what container, and
/// how to render.
#[derive(Debug, Clone)]
pub struct ArrangementExportRequest {
    pub output_path: PathBuf,
    pub format: AudioFileFormat,
    pub sample_format: AudioSampleFormat,
    pub render: OfflineRenderRequest,
    /// Per-format encoder options (WAV/FLAC/MP3 + metadata).
    pub encode_options: AudioEncodeOptions,
}

#[derive(Debug, Clone)]
pub struct TrackExportTarget {
    pub track_id: String,
    pub request: ArrangementExportRequest,
}

#[derive(Debug, Clone)]
pub struct ArrangementExportSummary {
    pub output_path: PathBuf,
    pub format: AudioFileFormat,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames_written: u64,
    pub duration_seconds: f64,
    pub peak_db: Option<f32>,
}

/// Temp path an export writes to before atomically replacing the final file.
pub fn partial_path_for(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_os_string();
    s.push(".partial");
    PathBuf::from(s)
}

fn linear_to_db(peak: f32) -> Option<f32> {
    if peak <= 0.0 {
        None
    } else {
        Some(20.0 * peak.log10())
    }
}

/// Export the arrangement to `request.output_path`.
///
/// Flow: Preparing → (AnalyzingPeak if normalizing) → Rendering/Encoding →
/// Finalizing → Complete. Writes to a `.partial` temp file and only replaces the
/// final output once the encoder finalizes successfully. On cancel or error the
/// partial file is removed and an existing final file is left untouched.
pub fn export_arrangement(
    snapshot: &EngineProjectSnapshot,
    request: &ArrangementExportRequest,
    cancel: &ExportCancelToken,
    on_progress: impl FnMut(ExportProgress),
) -> Result<ArrangementExportSummary, ExportError> {
    export_arrangement_with_bridges(snapshot, request, cancel, None, on_progress)
}

pub fn export_arrangement_with_bridges(
    snapshot: &EngineProjectSnapshot,
    request: &ArrangementExportRequest,
    cancel: &ExportCancelToken,
    bridge_sinks: Option<&PluginBridgeSinkMap>,
    mut on_progress: impl FnMut(ExportProgress),
) -> Result<ArrangementExportSummary, ExportError> {
    request.render.validate().map_err(ExportError::Settings)?;

    let total = request
        .render
        .content_frames()
        .saturating_add(request.render.max_tail_frames());

    on_progress(ExportProgress::stage_only(ExportStage::Preparing, total));

    if let Some(parent) = request.output_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(ExportError::Settings(format!(
                "output directory does not exist: {}",
                parent.display()
            )));
        }
    }

    let spec = AudioEncodeSpec {
        sample_rate: request.render.sample_rate,
        channels: request.render.channels,
        sample_format: request.sample_format,
    };

    // ── Pass 1 (optional): peak analysis for normalization ──────────────────
    let gain = match request.render.normalize {
        ExportNormalizeMode::None => 1.0,
        ExportNormalizeMode::PeakDb(target_db) => {
            on_progress(ExportProgress::stage_only(
                ExportStage::AnalyzingPeak,
                total,
            ));
            let analysis = render_offline_with_bridges(
                snapshot,
                &request.render,
                cancel,
                1.0,
                bridge_sinks,
                |_block| Ok(()),
                |_p| {},
            )?;
            if analysis.peak <= f32::EPSILON {
                1.0
            } else {
                let target_lin = 10f32.powf(target_db / 20.0);
                (target_lin / analysis.peak).clamp(0.0, 64.0)
            }
        }
    };

    // ── Encode pass: render → encoder, written to a .partial temp file ──────
    let partial = partial_path_for(&request.output_path);
    // Remove any stale partial from a previous aborted run.
    let _ = std::fs::remove_file(&partial);

    let mut encoder = create_encoder(&partial, spec, request.encode_options.clone())?;

    on_progress(ExportProgress::new(ExportStage::Encoding, 0, total));

    let render_result = render_offline_with_bridges(
        snapshot,
        &request.render,
        cancel,
        gain,
        bridge_sinks,
        |block| {
            encoder.write_interleaved_f32(block)?;
            Ok(())
        },
        |p| {
            // Surface render progress as the Encoding stage (render+encode are
            // fused in this single streaming pass).
            on_progress(ExportProgress::new(
                ExportStage::Encoding,
                p.rendered_frames,
                p.total_frames,
            ));
        },
    );

    let summary = match render_result {
        Ok(summary) => summary,
        Err(err) => {
            // finalize() is intentionally skipped; drop the encoder and remove
            // the partial so an existing final file is never touched.
            drop(encoder);
            let _ = std::fs::remove_file(&partial);
            return Err(err);
        }
    };

    on_progress(ExportProgress::new(ExportStage::Finalizing, total, total));
    let encode_summary = match encoder.finalize() {
        Ok(s) => s,
        Err(err) => {
            let _ = std::fs::remove_file(&partial);
            return Err(ExportError::Encode(err));
        }
    };

    // Atomic-ish replace: only now, after a successful finalize, do we touch the
    // final path. Remove an existing file then rename the partial into place.
    if request.output_path.exists() {
        if let Err(err) = std::fs::remove_file(&request.output_path) {
            let _ = std::fs::remove_file(&partial);
            return Err(ExportError::Io(err));
        }
    }
    if let Err(err) = std::fs::rename(&partial, &request.output_path) {
        let _ = std::fs::remove_file(&partial);
        return Err(ExportError::Io(err));
    }

    let duration_seconds = if request.render.sample_rate > 0 {
        encode_summary.frames_written as f64 / request.render.sample_rate as f64
    } else {
        0.0
    };

    on_progress(ExportProgress::stage_only(ExportStage::Complete, total));

    Ok(ArrangementExportSummary {
        output_path: request.output_path.clone(),
        format: request.format,
        sample_rate: encode_summary.sample_rate,
        channels: encode_summary.channels,
        frames_written: encode_summary.frames_written,
        duration_seconds,
        peak_db: linear_to_db(summary.peak),
    })
}

/// Export mixer-channel taps in one offline graph/timeline pass. Each target is
/// encoded independently, but plug-ins and routing are processed only once.
pub fn export_tracks_single_pass(
    snapshot: &EngineProjectSnapshot,
    targets: &[TrackExportTarget],
    cancel: &ExportCancelToken,
    on_progress: impl FnMut(ExportProgress),
) -> Result<Vec<ArrangementExportSummary>, ExportError> {
    export_tracks_single_pass_with_bridges(snapshot, targets, cancel, None, on_progress)
}

pub fn export_tracks_single_pass_with_bridges(
    snapshot: &EngineProjectSnapshot,
    targets: &[TrackExportTarget],
    cancel: &ExportCancelToken,
    bridge_sinks: Option<&PluginBridgeSinkMap>,
    mut on_progress: impl FnMut(ExportProgress),
) -> Result<Vec<ArrangementExportSummary>, ExportError> {
    let Some(first) = targets.first() else {
        return Ok(Vec::new());
    };
    if targets.iter().any(|target| {
        target.request.render.sample_rate != first.request.render.sample_rate
            || target.request.render.channels != first.request.render.channels
            || target.request.render.start_sample != first.request.render.start_sample
            || target.request.render.end_sample != first.request.render.end_sample
    }) {
        return Err(ExportError::Settings(
            "batch export targets must share render settings".to_string(),
        ));
    }
    if targets
        .iter()
        .any(|target| !matches!(target.request.render.normalize, ExportNormalizeMode::None))
    {
        return Err(ExportError::Settings(
            "normalization is unavailable for single-pass stem export".to_string(),
        ));
    }

    let track_indices: Vec<usize> = targets
        .iter()
        .map(|target| {
            snapshot
                .tracks
                .iter()
                .position(|track| track.id == target.track_id)
                .ok_or_else(|| {
                    ExportError::Settings(format!("export track not found: {}", target.track_id))
                })
        })
        .collect::<Result<_, _>>()?;
    let mut partials = Vec::with_capacity(targets.len());
    let mut encoders = Vec::with_capacity(targets.len());
    for target in targets {
        let setup = (|| -> Result<_, ExportError> {
            if let Some(parent) = target.request.output_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let partial = partial_path_for(&target.request.output_path);
            let _ = std::fs::remove_file(&partial);
            let spec = AudioEncodeSpec {
                sample_rate: target.request.render.sample_rate,
                channels: target.request.render.channels,
                sample_format: target.request.sample_format,
            };
            let encoder = create_encoder(&partial, spec, target.request.encode_options.clone())?;
            Ok((partial, encoder))
        })();
        match setup {
            Ok((partial, encoder)) => {
                encoders.push(encoder);
                partials.push(partial);
            }
            Err(error) => {
                // Opening one encoder failed: close the already-opened encoder
                // handles, then remove every partial created so far (plus the
                // failing target's, in case create_encoder touched the file).
                drop(encoders);
                for partial in &partials {
                    let _ = std::fs::remove_file(partial);
                }
                let _ = std::fs::remove_file(partial_path_for(&target.request.output_path));
                return Err(error);
            }
        }
    }

    let channels = first.request.render.channels as usize;
    let mut mono_scratch = vec![Vec::<f32>::new(); targets.len()];
    let mut target_peaks = vec![0.0_f32; targets.len()];
    let render_result = render_offline_tracks_with_bridges(
        snapshot,
        &first.request.render,
        cancel,
        bridge_sinks,
        |taps, frames| {
            for (target_index, &track_index) in track_indices.iter().enumerate() {
                let tap = taps
                    .get(track_index)
                    .ok_or_else(|| ExportError::Build("missing offline track tap".to_string()))?;
                target_peaks[target_index] = tap[..frames * 2]
                    .iter()
                    .fold(target_peaks[target_index], |peak, sample| {
                        peak.max(sample.abs())
                    });
                if channels == 1 {
                    let mono = &mut mono_scratch[target_index];
                    mono.resize(frames, 0.0);
                    for frame in 0..frames {
                        mono[frame] = (tap[frame * 2] + tap[frame * 2 + 1]) * 0.5;
                    }
                    encoders[target_index].write_interleaved_f32(mono)?;
                } else {
                    encoders[target_index].write_interleaved_f32(&tap[..frames * 2])?;
                }
            }
            Ok(())
        },
        |progress| {
            on_progress(ExportProgress::new(
                ExportStage::Encoding,
                progress.rendered_frames,
                progress.total_frames,
            ));
        },
    );
    match render_result {
        Ok(_) => {}
        Err(error) => {
            drop(encoders);
            for partial in &partials {
                let _ = std::fs::remove_file(partial);
            }
            return Err(error);
        }
    }

    let mut summaries = Vec::with_capacity(targets.len());
    let mut encoders = encoders.into_iter();
    for (index, ((target, partial), peak)) in targets
        .iter()
        .zip(partials.iter())
        .zip(target_peaks)
        .enumerate()
    {
        let mut encoder = encoders
            .next()
            .ok_or_else(|| ExportError::Build("missing encoder for export target".to_string()))?;
        let finalized = (|| -> Result<_, ExportError> {
            let encoded = encoder.finalize()?;
            if target.request.output_path.exists() {
                std::fs::remove_file(&target.request.output_path)?;
            }
            std::fs::rename(partial, &target.request.output_path)?;
            Ok(encoded)
        })();
        let encoded = match finalized {
            Ok(encoded) => encoded,
            Err(error) => {
                // One target failed to finalize/replace: close every remaining
                // encoder handle first (Windows cannot delete open files), then
                // sweep this and all not-yet-finalized partials off disk.
                // Already-renamed outputs are complete files and stay.
                drop(encoder);
                drop(encoders);
                for partial in &partials[index..] {
                    let _ = std::fs::remove_file(partial);
                }
                return Err(error);
            }
        };
        summaries.push(ArrangementExportSummary {
            output_path: target.request.output_path.clone(),
            format: target.request.format,
            sample_rate: encoded.sample_rate,
            channels: encoded.channels,
            frames_written: encoded.frames_written,
            duration_seconds: encoded.frames_written as f64 / encoded.sample_rate.max(1) as f64,
            peak_db: linear_to_db(peak),
        });
    }
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::offline_renderer::{make_track_snapshot, silence_snapshot};
    use crate::export::render_request::{ExportNormalizeMode, ExportTailMode};
    use sphere_encoder::AudioEncodeOptions;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "futureboard-export-{name}-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    fn wav_request(output: PathBuf, end: u64) -> ArrangementExportRequest {
        let encode_options = AudioEncodeOptions {
            format: AudioFileFormat::Wav,
            ..Default::default()
        };
        ArrangementExportRequest {
            output_path: output,
            format: AudioFileFormat::Wav,
            sample_format: AudioSampleFormat::F32,
            render: OfflineRenderRequest {
                sample_rate: 48_000,
                channels: 2,
                start_sample: 0,
                end_sample: end,
                master_volume: 1.0,
                block_size: 256,
                tail: ExportTailMode::None,
                normalize: ExportNormalizeMode::None,
            },
            encode_options,
        }
    }

    #[test]
    fn exports_silence_to_wav_atomically() {
        let out = temp_path("ok");
        let req = wav_request(out.clone(), 1000);
        let snapshot = silence_snapshot(48_000);
        let cancel = ExportCancelToken::new();
        let summary = export_arrangement(&snapshot, &req, &cancel, |_p| {}).unwrap();
        assert_eq!(summary.frames_written, 1000);
        assert!(out.exists());
        assert!(
            !partial_path_for(&out).exists(),
            "partial should be renamed away"
        );
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn single_pass_track_export_writes_all_targets() {
        let first = temp_path("single-pass-a");
        let second = temp_path("single-pass-b");
        let targets = vec![
            TrackExportTarget {
                track_id: "track-1".to_string(),
                request: wav_request(first.clone(), 1_000),
            },
            TrackExportTarget {
                track_id: "track-1".to_string(),
                request: wav_request(second.clone(), 1_000),
            },
        ];
        let snapshot = silence_snapshot(48_000);
        let summaries = export_tracks_single_pass(
            &snapshot,
            &targets,
            &ExportCancelToken::new(),
            |_progress| {},
        )
        .unwrap();

        assert_eq!(summaries.len(), 2);
        assert!(summaries
            .iter()
            .all(|summary| summary.frames_written == 1_000));
        for output in [first, second] {
            assert_eq!(&std::fs::read(&output).unwrap()[..4], b"RIFF");
            assert!(!partial_path_for(&output).exists());
            let _ = std::fs::remove_file(output);
        }
    }

    /// Deterministic stand-in for the shared-memory plugin host: a 4-channel
    /// multi-out "instrument" that emits constant per-channel values with the
    /// real one-block freshness contract (each produced block is handed out at
    /// most once; a read before the next `request_block` returns 0).
    #[derive(Debug, Default)]
    struct ConstantMultiOutSink {
        fresh: std::sync::atomic::AtomicBool,
    }

    impl ConstantMultiOutSink {
        const CHANNELS: usize = 4;

        /// Channel `c` (0-based) carries the constant `0.1 * (c + 1)`.
        fn channel_value(channel: usize) -> f32 {
            0.1 * (channel + 1) as f32
        }
    }

    impl crate::plugin_bridge::PluginBridgeSink for ConstantMultiOutSink {
        fn dsp_ready(&self) -> bool {
            true
        }
        fn plugin_output_channels(&self) -> u32 {
            Self::CHANNELS as u32
        }
        fn read_output(&self, out_l: &mut [f32], out_r: &mut [f32], frames: usize) -> usize {
            if !self.fresh.swap(false, std::sync::atomic::Ordering::AcqRel) {
                return 0;
            }
            let n = frames.min(out_l.len()).min(out_r.len());
            out_l[..n].fill(Self::channel_value(0));
            out_r[..n].fill(Self::channel_value(1));
            n
        }
        fn read_output_multichannel(&self, out: &mut [f32], frames: usize) -> (usize, usize) {
            if !self.fresh.swap(false, std::sync::atomic::Ordering::AcqRel) {
                return (0, 0);
            }
            let n = frames.min(out.len() / Self::CHANNELS);
            for frame in 0..n {
                for channel in 0..Self::CHANNELS {
                    out[frame * Self::CHANNELS + channel] = Self::channel_value(channel);
                }
            }
            (n, Self::CHANNELS)
        }
        fn push_midi(&self, _: u8, _: u8, _: u8, _: u32) {}
        fn write_input(&self, _: &[f32], _: &[f32], _: usize) {}
        fn request_block(&self, _frames: u32) {
            self.fresh.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// End-to-end single-pass stem export through a bridged multi-out
    /// instrument: parent VSTi track plus two `vsti-out:` child bus strips, all
    /// captured from ONE offline graph pass. Verifies the offline bridge
    /// handoff produces non-silent files (the Addictive Drums silence bug) and
    /// that each child gets exactly its own channel pair.
    #[test]
    fn single_pass_bridged_multiout_stems_are_not_silent() {
        use crate::types::{EngineInsertSnapshot, EngineTrackSnapshot};
        use std::collections::HashMap;
        use std::sync::Arc;

        let mut snapshot = silence_snapshot(48_000);
        let child_a_id = "vsti-out:insert-1:bus:0".to_string();
        let child_b_id = "vsti-out:insert-1:bus:1".to_string();

        let mut params: HashMap<String, serde_json::Value> = HashMap::new();
        params.insert("role".to_string(), serde_json::json!("instrument"));
        params.insert(
            "vstiOutputChildren".to_string(),
            serde_json::json!([
                { "trackId": child_a_id, "channelL": 1, "channelR": 2, "busIndex": 0 },
                { "trackId": child_b_id, "channelL": 3, "channelR": 4, "busIndex": 1 },
            ]),
        );
        snapshot.tracks[0].track_type = "instrument".to_string();
        snapshot.tracks[0].inserts.push(EngineInsertSnapshot {
            id: "insert-1".to_string(),
            kind: "external-bridge-plugin".to_string(),
            enabled: true,
            params,
            state: None,
        });
        for child_id in [&child_a_id, &child_b_id] {
            snapshot.tracks.push(EngineTrackSnapshot {
                track_type: "bus".to_string(),
                ..make_track_snapshot(child_id)
            });
        }

        let mut bridge_sinks = crate::plugin_bridge::PluginBridgeSinkMap::new();
        bridge_sinks.insert(
            "insert-1".to_string(),
            Arc::new(ConstantMultiOutSink::default())
                as crate::plugin_bridge::SharedPluginBridgeSink,
        );

        let parent_out = temp_path("multiout-parent");
        let child_a_out = temp_path("multiout-child-a");
        let child_b_out = temp_path("multiout-child-b");
        let targets = vec![
            TrackExportTarget {
                track_id: "track-1".to_string(),
                request: wav_request(parent_out.clone(), 1_000),
            },
            TrackExportTarget {
                track_id: child_a_id,
                request: wav_request(child_a_out.clone(), 1_000),
            },
            TrackExportTarget {
                track_id: child_b_id,
                request: wav_request(child_b_out.clone(), 1_000),
            },
        ];

        let summaries = export_tracks_single_pass_with_bridges(
            &snapshot,
            &targets,
            &ExportCancelToken::new(),
            Some(&bridge_sinks),
            |_progress| {},
        )
        .unwrap();

        assert_eq!(summaries.len(), 3);
        assert!(summaries
            .iter()
            .all(|summary| summary.frames_written == 1_000));
        for output in [&parent_out, &child_a_out, &child_b_out] {
            assert_eq!(&std::fs::read(output).unwrap()[..4], b"RIFF");
            assert!(!partial_path_for(output).exists());
        }

        // With explicit child routes the parent instrument strip receives no
        // fallback downmix — its stem is silent by contract.
        assert_eq!(
            summaries[0].peak_db, None,
            "parent with explicit multi-out children must not receive a downmix"
        );
        // Each child stem carries exactly its own channel pair: A = plugin
        // channels 1/2 (0.1/0.2), B = channels 3/4 (0.3/0.4). Unity fader,
        // centered pan → the tap peak is the pair's larger constant.
        let peak_lin = |db: Option<f32>| db.map(|d| 10f32.powf(d / 20.0)).unwrap_or(0.0);
        let child_a_peak = peak_lin(summaries[1].peak_db);
        let child_b_peak = peak_lin(summaries[2].peak_db);
        assert!(
            (child_a_peak - 0.2).abs() < 1e-3,
            "child A should carry plugin channels 1/2, got peak {child_a_peak}"
        );
        assert!(
            (child_b_peak - 0.4).abs() < 1e-3,
            "child B should carry plugin channels 3/4, got peak {child_b_peak}"
        );

        for output in [parent_out, child_a_out, child_b_out] {
            let _ = std::fs::remove_file(output);
        }
    }

    /// Bridged multi-out instrument (parent + two `vsti-out:` child bus strips)
    /// with the solo flags under test. `ConstantMultiOutSink` writes a constant
    /// per plugin channel: ch1=0.1, ch2=0.2, ch3=0.3, ch4=0.4, so child A peaks
    /// at 0.2 and child B at 0.4 when audible, and 0 when solo silences them.
    fn multiout_solo_snapshot(
        parent_solo: bool,
        child_solos: [bool; 2],
    ) -> (
        EngineProjectSnapshot,
        crate::plugin_bridge::PluginBridgeSinkMap,
        [String; 2],
    ) {
        use crate::types::{EngineInsertSnapshot, EngineTrackSnapshot};
        use std::collections::HashMap;
        use std::sync::Arc;

        let mut snapshot = silence_snapshot(48_000);
        let child_ids = [
            "vsti-out:insert-1:bus:0".to_string(),
            "vsti-out:insert-1:bus:1".to_string(),
        ];

        let mut params: HashMap<String, serde_json::Value> = HashMap::new();
        params.insert("role".to_string(), serde_json::json!("instrument"));
        params.insert(
            "vstiOutputChildren".to_string(),
            serde_json::json!([
                { "trackId": child_ids[0], "channelL": 1, "channelR": 2, "busIndex": 0 },
                { "trackId": child_ids[1], "channelL": 3, "channelR": 4, "busIndex": 1 },
            ]),
        );
        snapshot.tracks[0].track_type = "instrument".to_string();
        snapshot.tracks[0].solo = parent_solo;
        snapshot.tracks[0].inserts.push(EngineInsertSnapshot {
            id: "insert-1".to_string(),
            kind: "external-bridge-plugin".to_string(),
            enabled: true,
            params,
            state: None,
        });
        for (child_id, solo) in child_ids.iter().zip(child_solos) {
            snapshot.tracks.push(EngineTrackSnapshot {
                track_type: "bus".to_string(),
                solo,
                ..make_track_snapshot(child_id)
            });
        }

        let mut bridge_sinks = crate::plugin_bridge::PluginBridgeSinkMap::new();
        bridge_sinks.insert(
            "insert-1".to_string(),
            Arc::new(ConstantMultiOutSink::default())
                as crate::plugin_bridge::SharedPluginBridgeSink,
        );
        (snapshot, bridge_sinks, child_ids)
    }

    /// Peak of each `vsti-out:` child stem, in linear amplitude, from one
    /// single-pass export of the snapshot.
    fn multiout_child_peaks(
        snapshot: &EngineProjectSnapshot,
        bridge_sinks: &crate::plugin_bridge::PluginBridgeSinkMap,
        child_ids: &[String; 2],
    ) -> [f32; 2] {
        let outputs = [temp_path("solo-child-a"), temp_path("solo-child-b")];
        let targets: Vec<TrackExportTarget> = child_ids
            .iter()
            .zip(&outputs)
            .map(|(track_id, output)| TrackExportTarget {
                track_id: track_id.clone(),
                request: wav_request(output.clone(), 1_000),
            })
            .collect();

        let summaries = export_tracks_single_pass_with_bridges(
            snapshot,
            &targets,
            &ExportCancelToken::new(),
            Some(bridge_sinks),
            |_progress| {},
        )
        .unwrap();

        for output in outputs {
            let _ = std::fs::remove_file(output);
        }
        let peak_lin = |db: Option<f32>| db.map(|d| 10f32.powf(d / 20.0)).unwrap_or(0.0);
        [
            peak_lin(summaries[0].peak_db),
            peak_lin(summaries[1].peak_db),
        ]
    }

    /// Solo on the main VSTi track is a solo of the whole instrument: every one
    /// of its separate-output channels stays audible. The child strips carry no
    /// solo flag of their own here — before the parent link existed they were
    /// all silenced the moment anything was soloed, so soloing a multi-out
    /// instrument from its main track produced silence.
    #[test]
    fn soloing_parent_vsti_track_sounds_every_multi_out_channel() {
        let (snapshot, bridge_sinks, child_ids) = multiout_solo_snapshot(true, [false, false]);
        let [child_a_peak, child_b_peak] =
            multiout_child_peaks(&snapshot, &bridge_sinks, &child_ids);
        assert!(
            (child_a_peak - 0.2).abs() < 1e-3,
            "parent solo must keep channel pair 1/2 audible, got peak {child_a_peak}"
        );
        assert!(
            (child_b_peak - 0.4).abs() < 1e-3,
            "parent solo must keep channel pair 3/4 audible, got peak {child_b_peak}"
        );
    }

    /// The other half of the contract: a channel soloed on its own is heard by
    /// itself, so a single drum pad / bus can still be auditioned even though
    /// its parent instrument is not soloed.
    #[test]
    fn soloing_one_multi_out_channel_isolates_that_channel() {
        let (snapshot, bridge_sinks, child_ids) = multiout_solo_snapshot(false, [false, true]);
        let [child_a_peak, child_b_peak] =
            multiout_child_peaks(&snapshot, &bridge_sinks, &child_ids);
        assert_eq!(
            child_a_peak, 0.0,
            "an unsoloed sibling channel must be silent"
        );
        assert!(
            (child_b_peak - 0.4).abs() < 1e-3,
            "the soloed channel must still be audible, got peak {child_b_peak}"
        );
    }

    #[test]
    fn cancelled_export_removes_partial_and_leaves_existing_output() {
        let out = temp_path("cancel");
        std::fs::write(&out, b"ORIGINAL").unwrap();
        let req = wav_request(out.clone(), 500_000);
        let snapshot = silence_snapshot(48_000);
        let cancel = ExportCancelToken::new();
        cancel.cancel();
        let result = export_arrangement(&snapshot, &req, &cancel, |_p| {});
        assert!(matches!(result, Err(ExportError::Cancelled)));
        // Existing file untouched, no leftover partial.
        assert_eq!(std::fs::read(&out).unwrap(), b"ORIGINAL");
        assert!(!partial_path_for(&out).exists());
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn rejects_missing_output_directory() {
        let mut out = std::env::temp_dir();
        out.push("futureboard-nonexistent-dir-xyz");
        out.push("file.wav");
        let req = wav_request(out, 1000);
        let snapshot = silence_snapshot(48_000);
        let cancel = ExportCancelToken::new();
        let result = export_arrangement(&snapshot, &req, &cancel, |_p| {});
        assert!(matches!(result, Err(ExportError::Settings(_))));
    }

    #[test]
    fn success_replaces_existing_output_file() {
        let out = temp_path("replace");
        std::fs::write(&out, b"OLD-SMALL").unwrap();
        let req = wav_request(out.clone(), 2000);
        let snapshot = silence_snapshot(48_000);
        let cancel = ExportCancelToken::new();
        let summary = export_arrangement(&snapshot, &req, &cancel, |_p| {}).unwrap();
        assert_eq!(summary.frames_written, 2000);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert!(bytes.len() > 9, "new export should be larger than the stub");
        let _ = std::fs::remove_file(out);
    }
}
