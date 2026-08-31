//! Platform-neutral description of the project as ARA sees it, plus the traits
//! the host application implements to serve an ARA plug-in.
//!
//! Nothing here mentions `ara2_bridge`, GPUI, or the audio engine. The app fills
//! these records from its own state; [`crate::AraSession`] turns them into ARA
//! graph objects and keeps the two in sync.

use crate::error::AraResult;

/// Stable identity of one decoded audio asset (Futureboard's clip asset key).
///
/// Becomes the `persistentID` of an `ARAAudioSource`, so it must survive save,
/// load, and undo — a plug-in restoring an archive matches its stored objects by
/// this string.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AraSourceKey(pub String);

/// Stable identity of one audio clip (Futureboard's `ClipState::id`).
///
/// Becomes the `persistentID` of both the `ARAAudioModification` and its
/// `ARAPlaybackRegion`: edits belong to a clip, not to the underlying file, so
/// two clips of the same file can be tuned differently.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AraClipKey(pub String);

/// Stable identity of one track, backing an `ARARegionSequence`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AraTrackKey(pub String);

macro_rules! key_str {
    ($name:ident) => {
        impl $name {
            /// Borrows the underlying identifier.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

key_str!(AraSourceKey);
key_str!(AraClipKey);
key_str!(AraTrackKey);

/// RGB colour in the 0..=1 range, as ARA expresses object colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AraColor {
    /// Red channel.
    pub red: f32,
    /// Green channel.
    pub green: f32,
    /// Blue channel.
    pub blue: f32,
}

/// One decoded audio asset offered to the plug-in.
#[derive(Clone, Debug, PartialEq)]
pub struct AraAudioSourceDesc {
    /// Persistent identity.
    pub key: AraSourceKey,
    /// Display name (usually the file name).
    pub name: String,
    /// Native sample rate of the asset, not the project rate.
    pub sample_rate: f64,
    /// Length in frames per channel.
    pub frame_count: i64,
    /// Channel count of the asset.
    pub channel_count: i32,
}

/// One track, as an ARA region sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct AraRegionSequenceDesc {
    /// Persistent identity.
    pub key: AraTrackKey,
    /// Display name.
    pub name: String,
    /// Arrangement order, used by plug-ins for lane ordering.
    pub order_index: i32,
    /// Track colour, when the track has one.
    pub color: Option<AraColor>,
}

/// Playback transformations the host is asking the plug-in to perform.
///
/// Only flags the plug-in advertises in its factory are actually requested;
/// [`crate::AraFactoryInfo::supported_transforms`] reports what is available and
/// [`AraPlaybackTransform::intersect`] narrows a request to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AraPlaybackTransform {
    /// Stretch modification time to playback time.
    pub timestretch: bool,
    /// Stretch while following the musical context tempo map.
    pub timestretch_reflecting_tempo: bool,
    /// Let the plug-in shape the head of the region.
    pub content_based_fade_at_head: bool,
    /// Let the plug-in shape the tail of the region.
    pub content_based_fade_at_tail: bool,
}

impl AraPlaybackTransform {
    /// No transformation: playback time maps 1:1 onto modification time.
    pub const NONE: Self = Self {
        timestretch: false,
        timestretch_reflecting_tempo: false,
        content_based_fade_at_head: false,
        content_based_fade_at_tail: false,
    };

    /// Whether any transformation is requested.
    pub fn is_none(self) -> bool {
        self == Self::NONE
    }

    /// Keeps only the flags present in `supported`.
    pub fn intersect(self, supported: Self) -> Self {
        Self {
            timestretch: self.timestretch && supported.timestretch,
            timestretch_reflecting_tempo: self.timestretch_reflecting_tempo
                && supported.timestretch_reflecting_tempo,
            content_based_fade_at_head: self.content_based_fade_at_head
                && supported.content_based_fade_at_head,
            content_based_fade_at_tail: self.content_based_fade_at_tail
                && supported.content_based_fade_at_tail,
        }
    }
}

/// One audio clip, as an ARA audio modification plus its playback region.
///
/// All four time values are in seconds. Modification time is measured from the
/// start of the audio source; playback time is timeline position.
#[derive(Clone, Debug, PartialEq)]
pub struct AraPlaybackRegionDesc {
    /// Persistent identity of the clip.
    pub key: AraClipKey,
    /// The asset this clip reads from.
    pub source: AraSourceKey,
    /// The track this clip sits on.
    pub track: AraTrackKey,
    /// Display name.
    pub name: String,
    /// Offset of the clip's first frame inside the source.
    pub start_in_modification: f64,
    /// Trimmed length inside the source.
    pub duration_in_modification: f64,
    /// Timeline position of the clip.
    pub start_in_playback: f64,
    /// Timeline length of the clip.
    pub duration_in_playback: f64,
    /// Requested playback transformations.
    pub transform: AraPlaybackTransform,
    /// Clip colour, when the clip has one.
    pub color: Option<AraColor>,
}

/// The whole ARA-visible project, supplied as a snapshot and diffed on apply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AraGraph {
    /// Document name shown by the plug-in.
    pub name: Option<String>,
    /// Every asset referenced by at least one region.
    pub sources: Vec<AraAudioSourceDesc>,
    /// Every track that carries at least one region.
    pub sequences: Vec<AraRegionSequenceDesc>,
    /// Every ARA-managed clip.
    pub regions: Vec<AraPlaybackRegionDesc>,
}

impl AraGraph {
    /// Returns the first structural problem, or `Ok(())`.
    ///
    /// Checked before touching the plug-in so a malformed snapshot fails loudly
    /// on the host side rather than half-applying across the ABI.
    pub fn validate(&self) -> AraResult<()> {
        use crate::error::AraHostError;
        use std::collections::HashSet;

        let mut sources = HashSet::new();
        for source in &self.sources {
            if source.frame_count <= 0 || source.sample_rate <= 0.0 || source.channel_count <= 0 {
                return Err(AraHostError::invalid(format!(
                    "audio source '{}' has an empty or negative shape",
                    source.key.as_str()
                )));
            }
            if !sources.insert(source.key.clone()) {
                return Err(AraHostError::invalid(format!(
                    "duplicate audio source '{}'",
                    source.key.as_str()
                )));
            }
        }

        let mut sequences = HashSet::new();
        for sequence in &self.sequences {
            if !sequences.insert(sequence.key.clone()) {
                return Err(AraHostError::invalid(format!(
                    "duplicate region sequence '{}'",
                    sequence.key.as_str()
                )));
            }
        }

        let mut regions = HashSet::new();
        for region in &self.regions {
            if !regions.insert(region.key.clone()) {
                return Err(AraHostError::invalid(format!(
                    "duplicate playback region '{}'",
                    region.key.as_str()
                )));
            }
            if !sources.contains(&region.source) {
                return Err(AraHostError::invalid(format!(
                    "region '{}' references unknown source '{}'",
                    region.key.as_str(),
                    region.source.as_str()
                )));
            }
            if !sequences.contains(&region.track) {
                return Err(AraHostError::invalid(format!(
                    "region '{}' references unknown track '{}'",
                    region.key.as_str(),
                    region.track.as_str()
                )));
            }
            if region.duration_in_modification <= 0.0 || region.duration_in_playback <= 0.0 {
                return Err(AraHostError::invalid(format!(
                    "region '{}' has a non-positive duration",
                    region.key.as_str()
                )));
            }
            if region.start_in_modification < 0.0 {
                return Err(AraHostError::invalid(format!(
                    "region '{}' starts before its source",
                    region.key.as_str()
                )));
            }
        }

        Ok(())
    }
}

/// One tempo map entry: a timeline instant and its position in quarter notes.
///
/// ARA requires at least two entries and a strictly increasing sequence in both
/// dimensions; [`AraMusicalTimeline::validate`] enforces that.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AraTempoEntry {
    /// Position in seconds.
    pub time_seconds: f64,
    /// Position in quarter notes.
    pub quarter_position: f64,
}

/// One bar-signature change, positioned in quarter notes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AraBarSignature {
    /// Beats per bar.
    pub numerator: i32,
    /// Beat unit.
    pub denominator: i32,
    /// Position in quarter notes.
    pub quarter_position: f64,
}

/// The project's musical context: tempo map and bar signatures.
///
/// Futureboard has no key-signature or chord track, so those ARA content types
/// are reported unavailable rather than filled with invented values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AraMusicalTimeline {
    /// Tempo entries, ascending.
    pub tempo: Vec<AraTempoEntry>,
    /// Bar signatures, ascending.
    pub bars: Vec<AraBarSignature>,
}

impl AraMusicalTimeline {
    /// Returns the first structural problem, or `Ok(())`.
    pub fn validate(&self) -> AraResult<()> {
        use crate::error::AraHostError;

        if self.tempo.len() < 2 {
            return Err(AraHostError::invalid(
                "an ARA tempo map needs at least two entries",
            ));
        }
        for pair in self.tempo.windows(2) {
            if pair[1].time_seconds <= pair[0].time_seconds
                || pair[1].quarter_position <= pair[0].quarter_position
            {
                return Err(AraHostError::invalid(
                    "ARA tempo entries must strictly increase in time and quarters",
                ));
            }
        }
        if self.bars.is_empty() {
            return Err(AraHostError::invalid(
                "an ARA musical context needs at least one bar signature",
            ));
        }
        for bar in &self.bars {
            if bar.numerator <= 0 || bar.denominator <= 0 {
                return Err(AraHostError::invalid(
                    "ARA bar signatures need positive numerator and denominator",
                ));
            }
        }
        for pair in self.bars.windows(2) {
            if pair[1].quarter_position <= pair[0].quarter_position {
                return Err(AraHostError::invalid(
                    "ARA bar signatures must strictly increase in quarters",
                ));
            }
        }
        Ok(())
    }
}

/// A random-access reader over one audio asset.
///
/// ARA calls a reader from at most one thread at a time, but may drive several
/// readers of the same source concurrently, so each reader owns its own cursor.
/// Reads happen off the model thread and must not block on it.
pub trait AraSampleReader: Send + 'static {
    /// Channels this reader always fills.
    fn channel_count(&self) -> usize;

    /// Total frames per channel.
    fn frame_count(&self) -> i64;

    /// Reads planar samples starting at `start_frame`.
    ///
    /// `out` has exactly [`Self::channel_count`] slices of equal length. Frames
    /// outside the source must be written as silence rather than refused: ARA
    /// permits reads that run past the end.
    fn read_planar_f32(&mut self, start_frame: i64, out: &mut [&mut [f32]]) -> AraResult<()>;
}

/// Resolves an asset key into independent readers.
pub trait AraAudioAccess: Send + Sync + 'static {
    /// Opens a fresh reader positioned at the start of the asset.
    fn open_reader(&self, source: &AraSourceKey) -> AraResult<Box<dyn AraSampleReader>>;
}

/// A transport action requested by the plug-in's editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AraTransportRequest {
    /// Begin playback.
    Start,
    /// Stop playback.
    Stop,
    /// Locate to a timeline position in seconds.
    SetPosition(f64),
    /// Set the cycle range in seconds.
    SetCycleRange {
        /// Cycle start.
        start: f64,
        /// Cycle length.
        duration: f64,
    },
    /// Enable or disable cycling.
    EnableCycle(bool),
}

/// Receives transport requests coming from a plug-in editor.
///
/// Called from whatever thread the plug-in uses, so an implementation must post
/// the request to the transport owner instead of acting inline.
pub trait AraTransportControl: Send + Sync + 'static {
    /// Handles one request.
    fn request(&self, request: AraTransportRequest);
}

/// An asynchronous model change reported by the plug-in.
#[derive(Clone, Debug, PartialEq)]
pub enum AraModelUpdate {
    /// Analysis of one source started, progressed, or completed.
    AnalysisProgress {
        /// The analysed asset, when the host still knows it.
        source: Option<AraSourceKey>,
        /// Raw ARA analysis-progress state (start / update / complete).
        state: i32,
        /// Progress in the 0..=1 range.
        value: f32,
    },
    /// Content of one source changed.
    SourceContentChanged {
        /// The affected asset, when the host still knows it.
        source: Option<AraSourceKey>,
    },
    /// Content of one clip's modification changed.
    ModificationContentChanged {
        /// The affected clip, when the host still knows it.
        clip: Option<AraClipKey>,
    },
    /// Content of one clip's playback region changed.
    RegionContentChanged {
        /// The affected clip, when the host still knows it.
        clip: Option<AraClipKey>,
    },
    /// Persistent document data changed, so the project is dirty.
    DocumentDataChanged,
}

/// Receives model updates.
///
/// Called from plug-in threads. Implementations must be allocation-light and
/// non-blocking: push onto a bounded queue and let the UI drain it.
pub trait AraModelObserver: Send + Sync + 'static {
    /// Handles one update.
    fn notify(&self, update: AraModelUpdate);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(key: &str) -> AraAudioSourceDesc {
        AraAudioSourceDesc {
            key: key.into(),
            name: key.to_owned(),
            sample_rate: 48_000.0,
            frame_count: 48_000,
            channel_count: 2,
        }
    }

    fn sequence(key: &str) -> AraRegionSequenceDesc {
        AraRegionSequenceDesc {
            key: key.into(),
            name: key.to_owned(),
            order_index: 0,
            color: None,
        }
    }

    fn region(key: &str, source_key: &str, track_key: &str) -> AraPlaybackRegionDesc {
        AraPlaybackRegionDesc {
            key: key.into(),
            source: source_key.into(),
            track: track_key.into(),
            name: key.to_owned(),
            start_in_modification: 0.0,
            duration_in_modification: 1.0,
            start_in_playback: 0.0,
            duration_in_playback: 1.0,
            transform: AraPlaybackTransform::NONE,
            color: None,
        }
    }

    #[test]
    fn valid_graph_passes() {
        let graph = AraGraph {
            name: Some("project".into()),
            sources: vec![source("asset-1")],
            sequences: vec![sequence("track-1")],
            regions: vec![region("clip-1", "asset-1", "track-1")],
        };
        assert!(graph.validate().is_ok());
    }

    #[test]
    fn region_referencing_unknown_source_is_rejected() {
        let graph = AraGraph {
            sources: vec![source("asset-1")],
            sequences: vec![sequence("track-1")],
            regions: vec![region("clip-1", "asset-missing", "track-1")],
            ..AraGraph::default()
        };
        assert!(graph.validate().is_err());
    }

    #[test]
    fn duplicate_clip_keys_are_rejected() {
        let graph = AraGraph {
            sources: vec![source("asset-1")],
            sequences: vec![sequence("track-1")],
            regions: vec![
                region("clip-1", "asset-1", "track-1"),
                region("clip-1", "asset-1", "track-1"),
            ],
            ..AraGraph::default()
        };
        assert!(graph.validate().is_err());
    }

    #[test]
    fn transform_narrows_to_supported_flags() {
        let requested = AraPlaybackTransform {
            timestretch: true,
            timestretch_reflecting_tempo: true,
            content_based_fade_at_head: true,
            content_based_fade_at_tail: false,
        };
        let supported = AraPlaybackTransform {
            timestretch: true,
            timestretch_reflecting_tempo: false,
            content_based_fade_at_head: false,
            content_based_fade_at_tail: true,
        };
        assert_eq!(
            requested.intersect(supported),
            AraPlaybackTransform {
                timestretch: true,
                ..AraPlaybackTransform::NONE
            }
        );
    }

    #[test]
    fn tempo_map_needs_two_increasing_entries() {
        let mut timeline = AraMusicalTimeline {
            tempo: vec![AraTempoEntry {
                time_seconds: 0.0,
                quarter_position: 0.0,
            }],
            bars: vec![AraBarSignature {
                numerator: 4,
                denominator: 4,
                quarter_position: 0.0,
            }],
        };
        assert!(timeline.validate().is_err());

        timeline.tempo.push(AraTempoEntry {
            time_seconds: 0.5,
            quarter_position: 1.0,
        });
        assert!(timeline.validate().is_ok());

        timeline.tempo[1].quarter_position = 0.0;
        assert!(timeline.validate().is_err());
    }
}
