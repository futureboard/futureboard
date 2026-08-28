//! Shared state and model discovery for the native Solfege instrument.
//!
//! Solfege tracks deliberately keep their host-facing settings in the DAW
//! project model. The realtime renderer can consume the same state later,
//! while the UI already has a stable place for instrument, voice, preset and
//! performance controls.

pub mod accent;
pub mod capabilities;
pub mod loading;
pub mod performance;

pub use accent::{AccentAnalysisStats, AccentAnalyzer, AccentReplacePolicy};
pub use capabilities::{
    capabilities_for_family, family_for_instrument, InstrumentCapabilities, InstrumentFamily,
    LaneGroup, LaneSource, LaneSpec,
};
pub use loading::{
    model_load_state, ModelLoadError, ModelLoadProgress, ModelLoadStage, ModelLoadState,
};

use std::path::{Path, PathBuf};

/// Default height of one performance lane in the MIDI tab, in pixels.
/// Inside the 50-100px band the editor spec calls for.
pub const DEFAULT_LANE_HEIGHT: f32 = 72.0;
pub const MIN_LANE_HEIGHT: f32 = 40.0;
pub const MAX_LANE_HEIGHT: f32 = 220.0;

/// One visible performance lane in the Solfege MIDI tab.
///
/// This is editor *layout* state, not DSP state: it says which of the
/// instrument's lanes are on screen and how tall they are. It is persisted
/// with the track so reopening a project restores the working surface, and it
/// never reaches the audio thread.
#[derive(Debug, Clone, PartialEq)]
pub struct SolfegeLaneVisibility {
    /// Matches [`LaneSpec::id`] of the instrument's capability table. An id
    /// that no longer resolves (instrument changed) is simply not rendered,
    /// but is preserved on save so switching back restores the layout.
    pub lane_id: String,
    pub height: f32,
}

impl SolfegeLaneVisibility {
    pub fn new(lane_id: impl Into<String>) -> Self {
        Self {
            lane_id: lane_id.into(),
            height: DEFAULT_LANE_HEIGHT,
        }
    }

    pub fn sanitized(self) -> Self {
        Self {
            height: self.height.clamp(MIN_LANE_HEIGHT, MAX_LANE_HEIGHT),
            ..self
        }
    }
}

/// Track-local Solfege controls. Values are normalized so the audio adapter
/// can map them to the engine's event/control contract without UI coupling.
#[derive(Debug, Clone, PartialEq)]
pub struct SolfegeTrackState {
    pub model_path: Option<String>,
    pub instrument: String,
    pub voice: String,
    pub preset: String,
    pub bow_pressure: f32,
    pub vibrato: f32,
    pub dynamics: f32,
    pub expression: f32,
    /// Performance lanes currently shown under the piano roll, in display
    /// order. Editor layout only — see [`SolfegeLaneVisibility`].
    pub visible_lanes: Vec<SolfegeLaneVisibility>,
}

impl SolfegeTrackState {
    pub fn violin(model_path: Option<String>) -> Self {
        Self {
            model_path,
            instrument: "Violin".to_string(),
            voice: "Solo Bowed String".to_string(),
            preset: "VSCO Solo Violin".to_string(),
            bow_pressure: 0.62,
            vibrato: 0.18,
            dynamics: 0.78,
            expression: 1.0,
            visible_lanes: default_visible_lanes(&capabilities_for_family(family_for_instrument(
                "Violin",
            ))),
        }
    }

    /// What the loaded instrument can perform. The MIDI and Pitch editors ask
    /// this instead of assuming any particular instrument.
    pub fn capabilities(&self) -> InstrumentCapabilities {
        capabilities_for_family(family_for_instrument(&self.instrument))
    }

    pub fn sanitized(self) -> Self {
        Self {
            bow_pressure: self.bow_pressure.clamp(0.0, 1.0),
            vibrato: self.vibrato.clamp(0.0, 1.0),
            dynamics: self.dynamics.clamp(0.0, 1.0),
            expression: self.expression.clamp(0.0, 1.0),
            visible_lanes: self
                .visible_lanes
                .into_iter()
                .map(SolfegeLaneVisibility::sanitized)
                .collect(),
            ..self
        }
    }
}

/// The lane layout a freshly opened instrument starts with.
pub fn default_visible_lanes(caps: &InstrumentCapabilities) -> Vec<SolfegeLaneVisibility> {
    caps.default_visible_lanes
        .iter()
        .map(|id| SolfegeLaneVisibility::new(*id))
        .collect()
}

impl Default for SolfegeTrackState {
    fn default() -> Self {
        Self::violin(default_model_path().map(|path| path.to_string_lossy().into_owned()))
    }
}

/// Solfege model metadata shown by the inspector after a model is verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolfegeModelInfo {
    pub name: String,
    pub model_type: String,
    pub architecture: String,
    pub sample_rate: u32,
    pub source_type: String,
    pub validated: bool,
    pub parameter_count: u64,
    pub file_size_bytes: u64,
    pub voicebank_profiles: usize,
    pub voicebank_entries: usize,
    pub voicebank_audio_bytes: u64,
    pub voicebank_source_files: u32,
    /// The embedded FBMX Performer, when the model carries one.
    ///
    /// A Performer is not part of the sound. It predicts *how a score is
    /// played* — where notes land, how loud they are, whether and how they
    /// vibrate — and its output becomes ordinary editable project data before
    /// any audio is rendered. A model without one plays exactly what it is
    /// given, which is what every model did before this field existed.
    pub performer: Option<SolfegePerformerInfo>,
    /// The embedded FBMX Accent Analyzer, when the model carries one.
    ///
    /// `None` is the normal case and not a defect: Analyze Accent runs on the
    /// built-in fitted rule, and a package only carries a model when a corpus
    /// has been shown to support one. The Solo Violin package deliberately
    /// carries none — see `solfege::accent::analyzer` for the fourteen-fold
    /// measurement that decided it.
    pub accent: Option<SolfegeAccentInfo>,
}

/// What an embedded Accent Analyzer is, for the inspector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolfegeAccentInfo {
    pub input_size: usize,
    pub output_size: usize,
    pub weight_bytes: u64,
    /// `false` when the section is present but this build cannot run it — a
    /// different feature vocabulary, or a shape it does not recognise. Shown
    /// rather than hidden, because the analysis silently falling back to the
    /// rule is exactly the situation a user would otherwise not be told about.
    pub usable: bool,
}

/// What an embedded Performer is, for the inspector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolfegePerformerInfo {
    /// Score features in, performance values out.
    pub input_size: usize,
    pub output_size: usize,
    /// `true` when the model reads the whole phrase before deciding, which is
    /// the studio mode a DAW uses; `false` is the causal live variant.
    pub bidirectional: bool,
    pub weight_bytes: u64,
    /// Feature vocabulary the model was trained against. See
    /// [`Self::reads_accent`].
    pub feature_schema_version: u32,
}

impl SolfegeAccentInfo {
    pub fn summary(&self) -> String {
        if self.usable {
            format!(
                "Embedded ({} in -> {} out, correction to the built-in rule)",
                self.input_size, self.output_size
            )
        } else {
            "Present, but not runnable by this build — analysis uses the rule".to_string()
        }
    }
}

impl SolfegePerformerInfo {
    /// Whether this Performer reads per-note accent.
    ///
    /// Schema 1 took sixteen score features; schema 2 appends the four accent
    /// components, so a Performer that does not read them cannot be conditioned
    /// by the Accent lane no matter what the editor sends. Reported rather than
    /// assumed: a package built before accent existed still loads and still
    /// plays, it just plays the same way whatever the accents say.
    pub fn reads_accent(&self) -> bool {
        self.feature_schema_version >= 2
    }

    pub fn mode(&self) -> &'static str {
        if self.bidirectional {
            "Studio (reads the whole phrase)"
        } else {
            "Live (causal)"
        }
    }
}

/// The exact user model directory requested by the Solfege workflow.
pub fn model_directory() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("Documents"))
        .join("Futureboard Studio")
        .join("Utilities")
        .join("Model")
        .join("Solfage")
}

/// Discover available `.sfm` and standalone `.fbmx` models in the Solfage
/// model directory.
pub fn discover_model_paths() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(model_directory()) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("sfm")
                            || extension.eq_ignore_ascii_case("fbmx")
                    })
                && !is_performer_file(path)
        })
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
    paths
}

/// Whether a loose `.fbmx` in the model folder is a Performer rather than an
/// instrument.
///
/// The two are both FBMX files and both belong in this folder, but only one of
/// them is something a track can be set to play. A Performer predicts playing
/// decisions and produces no audio, so offering it in the instrument list
/// would give the user a model that loads and then makes no sound. Performers
/// normally travel inside an `.sfm`'s `PERF` section; this covers one dropped
/// in loose, and it reads the header rather than trusting the filename.
fn is_performer_file(path: &Path) -> bool {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fbmx"))
    {
        return false;
    }
    // Only the header is needed, but the reader wants the whole file; these are
    // tens of kilobytes, not the voicebank.
    std::fs::read(path)
        .ok()
        .and_then(|bytes| fbmx_runtime::FbmxModel::from_bytes(&bytes).ok())
        .is_some_and(|model| model.info().model_type.as_str() == "performer-gru")
}

pub fn default_model_path() -> Option<PathBuf> {
    discover_model_paths().into_iter().next()
}

/// Load and verify one Solfege model, returning the metadata needed by the DAW.
///
/// Blocking. Every section digest of a packaged voicebank is paid on the
/// calling thread, which for the shipped violin is a 146 MB read and ~293 MB of
/// SHA-256 — never call this from a render pass or a GPUI callback. Rendering
/// surfaces use [`loading::model_load_state`], which reports the same load in
/// stages from a worker thread; both share one cache keyed on the file's path,
/// length and mtime, so a replaced model is re-read instead of serving a stale
/// success or a stuck failure.
pub fn load_model_info(path: &Path) -> Result<SolfegeModelInfo, String> {
    loading::load_blocking(path)
}

#[cfg(test)]
mod installed_model_tests {
    use super::*;

    /// Probe the model folder this machine actually has.
    ///
    /// Ignored by default because it depends on what is installed, which is not
    /// something CI can know. It is the check worth running by hand after
    /// putting a model in place: `cargo test -p sphere_ui_components --lib
    /// installed_model -- --ignored --nocapture`.
    #[test]
    #[ignore = "depends on the installed model folder"]
    fn the_installed_solfage_models_load_and_report_what_they_carry() {
        let directory = model_directory();
        println!("model directory: {}", directory.display());
        let paths = discover_model_paths();
        assert!(
            !paths.is_empty(),
            "no models discovered in {}",
            directory.display()
        );
        for path in paths {
            let info = load_model_info(&path).expect("the installed model loads");
            println!(
                "  {}
    {} | {} | voicebank {} entries | performer {}",
                path.display(),
                info.model_type,
                info.architecture,
                info.voicebank_entries,
                match info.performer.as_ref() {
                    Some(performer) => format!(
                        "{} ({} in -> {} out, {} bytes)",
                        performer.mode(),
                        performer.input_size,
                        performer.output_size,
                        performer.weight_bytes
                    ),
                    None => "none".to_string(),
                }
            );
        }
    }

    /// A loose Performer `.fbmx` must not be offered as an instrument.
    #[test]
    #[ignore = "depends on the installed model folder"]
    fn a_loose_performer_is_not_listed_as_an_instrument() {
        for path in discover_model_paths() {
            assert!(
                !is_performer_file(&path),
                "{} is a Performer and was offered as a playable instrument",
                path.display()
            );
        }
    }
}
