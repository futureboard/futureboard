//! Export settings model: the plain, GPUI-free contract between the export
//! window's controls and the engine's [`ArrangementExportRequest`].
//!
//! Nothing here touches the timeline, the engine runtime, or GPUI — the window
//! edits an `ExportSettings`, validates it, and converts it to an engine request
//! against a plain [`EngineProjectSnapshot`] + [`ExportProjectDefaults`].

use std::path::PathBuf;

use sphere_encoder::{
    AudioEncodeOptions, AudioFileFormat, AudioSampleFormat, FlacEncodeOptions, Mp3Bitrate,
    Mp3EncodeOptions,
};
use DirectAudio::types::EngineProjectSnapshot;
use DirectAudio::{
    arrangement_bounds_samples, beats_to_samples, ArrangementExportRequest, ExportNormalizeMode,
    ExportTailMode, OfflineRenderRequest,
};

/// Output sample-rate choice in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportSampleRateChoice {
    Project,
    Hz44100,
    Hz48000,
    Hz88200,
    Hz96000,
}

impl ExportSampleRateChoice {
    pub fn resolve(self, project_sample_rate: u32) -> u32 {
        match self {
            Self::Project => project_sample_rate.max(1),
            Self::Hz44100 => 44_100,
            Self::Hz48000 => 48_000,
            Self::Hz88200 => 88_200,
            Self::Hz96000 => 96_000,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Hz44100 => "44100 Hz",
            Self::Hz48000 => "48000 Hz",
            Self::Hz88200 => "88200 Hz",
            Self::Hz96000 => "96000 Hz",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportChannelMode {
    Stereo,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    Mixdown,
    Stems,
    Multitrack,
}

impl ExportMode {
    /// Labels describe what the engine actually does. Stems and Multitrack use
    /// the identical post-fader tap — they differ only in whether routing
    /// channels (bus/return/group) get their own file — so naming them "two
    /// render modes" would be a lie.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mixdown => "Mixdown",
            Self::Stems => "All mixer channels",
            Self::Multitrack => "Source tracks only",
        }
    }

    /// Whether a batch export in this mode writes a file for `target`.
    pub fn selects(self, target: &ExportTrackTarget) -> bool {
        match self {
            Self::Mixdown => false,
            Self::Stems => true,
            Self::Multitrack => target.include_in_multitrack,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportTrackTarget {
    pub id: String,
    pub name: String,
    pub include_in_multitrack: bool,
}

impl ExportChannelMode {
    pub fn channels(self) -> u16 {
        match self {
            Self::Stereo => 2,
            Self::Mono => 1,
        }
    }
}

/// Range to export. Beat ranges are resolved to samples against the snapshot's
/// tempo map at conversion time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportRangeChoice {
    EntireArrangement,
    TimeSelection { start_beat: f64, end_beat: f64 },
    LoopRange { start_beat: f64, end_beat: f64 },
    Custom { start_beat: f64, end_beat: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportNormalizeChoice {
    Off,
    /// Peak-normalize so the loudest sample hits the given dBFS (UI default -1.0).
    PeakDb(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportTailChoice {
    None,
    FixedSeconds(f64),
    UntilSilence { max_seconds: f64, threshold_db: f32 },
}

/// Peak ceilings the dialog offers. Every entry is a real
/// `ExportNormalizeMode::PeakDb` value the engine applies verbatim — the UI
/// names the number instead of hiding one behind a generic "Normalize".
pub const PEAK_TARGETS_DB: [f32; 4] = [-0.1, -0.3, -1.0, -3.0];

/// Tail length used by [`ExportTailChoice::FixedSeconds`] when the user picks
/// the fixed option. Lives here (not in the window) so the default settings and
/// the dropdown cannot drift apart.
pub const TAIL_FIXED_SECONDS: f64 = 5.0;
/// Cap for [`ExportTailChoice::UntilSilence`].
pub const TAIL_SILENCE_MAX_SECONDS: f64 = 10.0;
/// Block-peak threshold that ends an "until silence" tail.
pub const TAIL_SILENCE_THRESHOLD_DB: f32 = -60.0;

/// Compression presets `FlacEncodeOptions::compression_level` accepts.
pub const FLAC_COMPRESSION_RANGE: (u8, u8) = (0, 8);

/// Bytes of RIFF/`fmt `/`data` header a canonical WAV file carries ahead of its
/// samples. Only used to make the size readout honest about its overhead.
const WAV_HEADER_BYTES: u64 = 44;

/// Project-derived values the settings need to build a request without reaching
/// into live timeline/engine state.
#[derive(Debug, Clone)]
pub struct ExportProjectDefaults {
    pub project_sample_rate: u32,
    /// Linear master gain to bake into the export (mirrors the engine atomic).
    pub master_volume: f32,
    /// End beat of the latest content (for the EntireArrangement estimate).
    pub content_end_beat: f64,
    pub time_selection: Option<(f64, f64)>,
    pub loop_range: Option<(f64, f64)>,
    /// Whether the build can encode MP3 (the `mp3` feature is compiled in).
    pub mp3_available: bool,
    pub track_targets: Vec<ExportTrackTarget>,
}

#[derive(Debug, Clone)]
pub struct ExportSettings {
    pub output_path: Option<PathBuf>,
    pub format: AudioFileFormat,
    pub sample_rate: ExportSampleRateChoice,
    pub channels: ExportChannelMode,
    pub range: ExportRangeChoice,
    pub wav_sample_format: AudioSampleFormat,
    pub flac_bit_depth: u16,
    pub flac_compression_level: Option<u8>,
    pub mp3_bitrate_kbps: u16,
    pub normalize: ExportNormalizeChoice,
    pub tail: ExportTailChoice,
    pub mode: ExportMode,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            output_path: None,
            format: AudioFileFormat::Wav,
            sample_rate: ExportSampleRateChoice::Project,
            channels: ExportChannelMode::Stereo,
            range: ExportRangeChoice::EntireArrangement,
            wav_sample_format: AudioSampleFormat::I24,
            flac_bit_depth: 24,
            flac_compression_level: Some(5),
            mp3_bitrate_kbps: 256,
            normalize: ExportNormalizeChoice::Off,
            // Capture reverb/delay/instrument-release tails past the last content
            // by default so exports don't hard-cut the decay.
            tail: ExportTailChoice::FixedSeconds(TAIL_FIXED_SECONDS),
            mode: ExportMode::Mixdown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportSettingsError {
    NoOutputPath,
    OutputDirMissing(PathBuf),
    InvalidRange,
    NoContent,
    UnsupportedSampleRate(u32),
    Mp3Unavailable,
    FlacUnsupportedBitDepth(u16),
    NoTracksForBatchExport,
}

impl ExportSettingsError {
    /// Fluent key for the localized form of this error.
    ///
    /// This module stays GPUI- and globals-free, so it publishes the *key* and
    /// lets the window resolve it through `I18n::tr_or(key, &user_message())`.
    pub fn message_key(&self) -> &'static str {
        match self {
            Self::NoOutputPath => "export.error.no-output-path",
            Self::OutputDirMissing(_) => "export.error.output-dir-missing",
            Self::InvalidRange => "export.error.invalid-range",
            Self::NoContent => "export.error.no-content",
            Self::UnsupportedSampleRate(_) => "export.error.unsupported-sample-rate",
            Self::Mp3Unavailable => "export.error.mp3-unavailable",
            Self::FlacUnsupportedBitDepth(_) => "export.error.flac-bit-depth",
            Self::NoTracksForBatchExport => "export.error.no-batch-tracks",
        }
    }

    /// A concise, DAW-appropriate user-facing message.
    pub fn user_message(&self) -> String {
        match self {
            Self::NoOutputPath => "Choose an output file.".to_string(),
            Self::OutputDirMissing(p) => {
                format!("Output folder does not exist: {}", p.display())
            }
            Self::InvalidRange => "No valid export range selected.".to_string(),
            Self::NoContent => "The arrangement is empty — nothing to export.".to_string(),
            Self::UnsupportedSampleRate(sr) => format!("Unsupported sample rate: {sr} Hz."),
            Self::Mp3Unavailable => "MP3 export is not available in this build.".to_string(),
            Self::FlacUnsupportedBitDepth(b) => {
                format!("FLAC supports 16-bit or 24-bit, not {b}-bit.")
            }
            Self::NoTracksForBatchExport => {
                "No source tracks are available for stem export.".to_string()
            }
        }
    }
}

const SUPPORTED_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

/// Everything the dialog's readouts show, derived once from the *same*
/// [`ArrangementExportRequest`] the engine will receive.
///
/// Deriving it from the request rather than recomputing the arithmetic is what
/// keeps the summary honest: if a number here is wrong, the export is wrong the
/// same way. Building one walks the snapshot's tempo map and stats the output
/// folder, so callers cache it and refresh on mutation — never per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportEstimate {
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: AudioSampleFormat,
    pub format: AudioFileFormat,
    /// MP3 is lossy, so its "depth" is a bitrate. `None` for PCM containers.
    pub mp3_bitrate_kbps: Option<u16>,
    pub start_sample: u64,
    pub end_sample: u64,
    pub content_frames: u64,
    /// Frames the tail mode may add. `UntilSilence` reports its cap, so the
    /// real file can be shorter — readouts built from it say "up to".
    pub max_tail_frames: u64,
    pub content_seconds: f64,
    pub start_seconds: f64,
    pub end_seconds: f64,
    /// Number of files this export will write.
    pub file_count: usize,
    /// Size of one uncompressed output file, when the container stores
    /// fixed-width PCM. `None` for FLAC/MP3 — only the encoder knows those.
    pub uncompressed_bytes: Option<u64>,
}

impl ExportSettings {
    /// Default output file name for a project, e.g. `MyProject.wav`.
    pub fn default_file_name(project_name: &str, format: AudioFileFormat) -> String {
        let stem = if project_name.trim().is_empty() {
            "Export"
        } else {
            project_name.trim()
        };
        format!("{stem}.{}", format.extension())
    }

    /// Replace the output path's extension to match the selected format.
    pub fn normalized_output_path(&self) -> Option<PathBuf> {
        self.output_path
            .as_ref()
            .map(|p| p.with_extension(self.format.extension()))
    }

    /// Resolve the chosen range to absolute `[start, end)` samples at the output
    /// sample rate, honoring the snapshot tempo map.
    fn resolve_range_samples(
        &self,
        snapshot: &EngineProjectSnapshot,
        sample_rate: u32,
    ) -> Result<(u64, u64), ExportSettingsError> {
        let beats_to = |b: f64| beats_to_samples(snapshot, b, sample_rate);
        let (start, end) = match self.range {
            ExportRangeChoice::EntireArrangement => {
                arrangement_bounds_samples(snapshot, sample_rate)
            }
            ExportRangeChoice::TimeSelection {
                start_beat,
                end_beat,
            }
            | ExportRangeChoice::LoopRange {
                start_beat,
                end_beat,
            }
            | ExportRangeChoice::Custom {
                start_beat,
                end_beat,
            } => (beats_to(start_beat), beats_to(end_beat)),
        };
        if end <= start {
            return match self.range {
                ExportRangeChoice::EntireArrangement => Err(ExportSettingsError::NoContent),
                _ => Err(ExportSettingsError::InvalidRange),
            };
        }
        Ok((start, end))
    }

    pub fn resolved_sample_rate(&self, defaults: &ExportProjectDefaults) -> u32 {
        self.sample_rate.resolve(defaults.project_sample_rate)
    }

    /// How many files a batch export in the current mode would write.
    ///
    /// Mixdown is not a batch, so it counts zero here; [`ExportEstimate`]
    /// reports its single file separately.
    pub fn batch_target_count(&self, defaults: &ExportProjectDefaults) -> usize {
        defaults
            .track_targets
            .iter()
            .filter(|target| self.mode.selects(target))
            .count()
    }

    fn sample_format(&self) -> AudioSampleFormat {
        match self.format {
            AudioFileFormat::Wav => self.wav_sample_format,
            AudioFileFormat::Flac => {
                if self.flac_bit_depth == 16 {
                    AudioSampleFormat::I16
                } else {
                    AudioSampleFormat::I24
                }
            }
            // MP3 is lossy; sample format is informational only.
            AudioFileFormat::Mp3 => AudioSampleFormat::I16,
            AudioFileFormat::Rauf => AudioSampleFormat::F32,
        }
    }

    /// Validate the settings against project defaults. Does not require a
    /// snapshot (range-content checks happen in [`to_request`]).
    pub fn validate(&self, defaults: &ExportProjectDefaults) -> Result<(), ExportSettingsError> {
        let path = self
            .normalized_output_path()
            .ok_or(ExportSettingsError::NoOutputPath)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(ExportSettingsError::OutputDirMissing(parent.to_path_buf()));
            }
        }
        let sr = self.resolved_sample_rate(defaults);
        if !SUPPORTED_RATES.contains(&sr) {
            return Err(ExportSettingsError::UnsupportedSampleRate(sr));
        }
        if self.format == AudioFileFormat::Mp3 && !defaults.mp3_available {
            return Err(ExportSettingsError::Mp3Unavailable);
        }
        if self.format == AudioFileFormat::Flac
            && self.flac_bit_depth != 16
            && self.flac_bit_depth != 24
        {
            return Err(ExportSettingsError::FlacUnsupportedBitDepth(
                self.flac_bit_depth,
            ));
        }
        // Mode-aware: Multitrack can filter every available target away, so
        // testing "the project has tracks" would let Export stay enabled and
        // fail later with a second, hand-written message.
        if self.mode != ExportMode::Mixdown && self.batch_target_count(defaults) == 0 {
            return Err(ExportSettingsError::NoTracksForBatchExport);
        }
        Ok(())
    }

    /// Resolve the readouts the dialog shows, from the request the engine gets.
    pub fn estimate(
        &self,
        snapshot: &EngineProjectSnapshot,
        defaults: &ExportProjectDefaults,
    ) -> Result<ExportEstimate, ExportSettingsError> {
        let request = self.to_request(snapshot, defaults)?;
        let sample_rate = request.render.sample_rate;
        let rate = sample_rate.max(1) as f64;
        let content_frames = request.render.content_frames();
        let max_tail_frames = request.render.max_tail_frames();
        let file_count = if self.mode == ExportMode::Mixdown {
            1
        } else {
            self.batch_target_count(defaults)
        };
        let uncompressed_bytes = (request.format == AudioFileFormat::Wav).then(|| {
            content_frames
                .saturating_add(max_tail_frames)
                .saturating_mul(request.render.channels as u64)
                .saturating_mul(request.sample_format.bytes_per_sample() as u64)
                .saturating_add(WAV_HEADER_BYTES)
        });
        Ok(ExportEstimate {
            sample_rate,
            channels: request.render.channels,
            sample_format: request.sample_format,
            format: request.format,
            mp3_bitrate_kbps: (request.format == AudioFileFormat::Mp3)
                .then_some(self.mp3_bitrate_kbps),
            start_sample: request.render.start_sample,
            end_sample: request.render.end_sample,
            content_frames,
            max_tail_frames,
            content_seconds: content_frames as f64 / rate,
            start_seconds: request.render.start_sample as f64 / rate,
            end_seconds: request.render.end_sample as f64 / rate,
            file_count,
            uncompressed_bytes,
        })
    }

    /// Build the engine [`ArrangementExportRequest`]. Validates first, then
    /// resolves the range against the snapshot tempo map.
    pub fn to_request(
        &self,
        snapshot: &EngineProjectSnapshot,
        defaults: &ExportProjectDefaults,
    ) -> Result<ArrangementExportRequest, ExportSettingsError> {
        self.validate(defaults)?;
        let output_path = self
            .normalized_output_path()
            .ok_or(ExportSettingsError::NoOutputPath)?;
        let sample_rate = self.resolved_sample_rate(defaults);
        let (start_sample, end_sample) = self.resolve_range_samples(snapshot, sample_rate)?;

        let tail = match self.tail {
            ExportTailChoice::None => ExportTailMode::None,
            ExportTailChoice::FixedSeconds(s) => ExportTailMode::FixedSeconds(s),
            ExportTailChoice::UntilSilence {
                max_seconds,
                threshold_db,
            } => ExportTailMode::UntilSilence {
                max_seconds,
                threshold_db,
            },
        };
        let normalize = match self.normalize {
            ExportNormalizeChoice::Off => ExportNormalizeMode::None,
            ExportNormalizeChoice::PeakDb(db) => ExportNormalizeMode::PeakDb(db),
        };

        let mut encode_options = AudioEncodeOptions {
            format: self.format,
            ..Default::default()
        };
        encode_options.flac = FlacEncodeOptions {
            compression_level: self.flac_compression_level.unwrap_or(5),
            block_size: 4096,
        };
        encode_options.mp3 = Mp3EncodeOptions {
            bitrate: Mp3Bitrate::from_kbps(self.mp3_bitrate_kbps as u32)
                .unwrap_or(Mp3Bitrate::Kbps256),
            quality: 2,
        };

        Ok(ArrangementExportRequest {
            output_path,
            format: self.format,
            sample_format: self.sample_format(),
            render: OfflineRenderRequest {
                sample_rate,
                channels: self.channels.channels(),
                start_sample,
                end_sample,
                master_volume: defaults.master_volume,
                block_size: 1024,
                tail,
                normalize,
            },
            encode_options,
        })
    }
}
