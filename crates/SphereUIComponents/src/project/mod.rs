pub mod format;
pub mod import;
pub mod io;
pub mod recent;
pub mod routing_migration;
pub mod session;
pub mod template;

pub use format::{decode_project, encode_project, ProjectError, PROJECT_MAGIC, PROJECT_VERSION};
pub use import::{is_import_path, IMPORT_PROJECT_FILE_EXTS};
pub use io::{
    create_project_folder, default_projects_dir, import_audio_file_to_project, load_project,
    project_backup_path, project_temp_path, sanitize_project_name, save_project,
    validate_project_file, verify_project_file, LEGACY_PROJECT_FILE_EXT, PROJECT_FILE_EXT,
    SUPPORTED_PROJECT_FILE_EXTS,
};
pub use recent::{RecentProject, RecentProjectsStore};
pub use session::ProjectSession;
pub use template::{ProjectCreateOptions, ProjectTemplate};

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub use sphere_soundfont_player::{SoundfontEnvelope, SoundfontRenderQuality};

// ── Identifiers ───────────────────────────────────────────────────────────────

fn new_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Cheap non-crypto ID: timestamp + stack address mix.
    let addr = &ts as *const _ as u64;
    format!("{:016x}{:016x}", ts as u64, addr ^ 0xDEAD_BEEF_CAFE_BABE)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Enumerations ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTrackType {
    Audio,
    Midi,
    Instrument,
    Bus,
    Return,
    Group,
    Master,
    /// Reference/preview video lane (v33+).
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMonitorMode {
    #[default]
    Off,
    /// Monitor input whenever this mode is selected (Input).
    Always,
    /// Monitor input whenever the track is record-armed (Auto).
    WhenRecordArmed,
}

impl InputMonitorMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::WhenRecordArmed,
            Self::WhenRecordArmed => Self::Always,
            Self::Always => Self::Off,
        }
    }

    pub fn is_active(self, armed: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Always => true,
            Self::WhenRecordArmed => armed,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::WhenRecordArmed => "Auto",
            Self::Always => "Input",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ClipSource {
    Audio {
        asset_id: String,
        source_path: Option<PathBuf>,
    },
    Rauf {
        asset_id: String,
        source_path: PathBuf,
        metadata_path: Option<PathBuf>,
        sample_format: String,
        sample_rate: u32,
        channels: u16,
        start_frame: u64,
        length_frames: u64,
    },
    Midi {
        notes: Vec<MidiNote>,
        controller_lanes: Vec<MidiControllerLane>,
        sysex_events: Vec<MidiSysExEvent>,
        /// Direction articulation events (v25+). Older projects have none.
        articulations: Vec<MidiArticulation>,
    },
    /// Reference video placed on the Video track (v33+). Only the media
    /// reference is stored; frames are always decoded from the source file.
    Video {
        asset_id: String,
        source_path: Option<PathBuf>,
    },
    Empty,
}

#[derive(Debug, Clone)]
pub struct MidiNote {
    /// Stable note identity (v26+). `0` on older files means "mint on load".
    pub id: u64,
    pub pitch: u8,
    pub start_beats: f32,
    pub duration_beats: f32,
    pub velocity: u8,
    /// Note Off velocity 1..=127, or `0` when unset (v26+).
    pub release_velocity: u8,
    pub muted: bool,
    /// UI-facing channel number, 1..=16. Older projects have no per-note
    /// channel data and default to 1 on load.
    pub channel: u8,
    /// Per-note articulation tag ([`ArticulationId::to_tag`]); `0` = none.
    /// Older projects (< v25) have no articulation data and default to `0`.
    pub articulation: u8,
}

/// Serialized direction articulation event. Mirrors
/// [`timeline_state::MidiArticulationEvent`] minus the transient editor id
/// (fresh ids are minted on load, like MIDI note ids).
#[derive(Debug, Clone, PartialEq)]
pub struct MidiArticulation {
    /// Beats relative to the clip start.
    pub beat: f32,
    /// [`ArticulationId::to_tag`] value; always a valid non-zero tag on save.
    pub articulation: u8,
}

/// Serialized MIDI controller stream selector. Mirrors
/// [`timeline_state::MidiControllerKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiControllerKind {
    CC(u8),
    PitchBend,
    ChannelPressure,
    PolyPressure,
}

#[derive(Debug, Clone)]
pub struct MidiControllerPoint {
    /// Stable point identity (v26+). `0` on older files means "mint on load".
    pub id: u64,
    pub beat: f32,
    /// Normalized `0.0..=1.0`.
    pub value: f32,
}

#[derive(Debug, Clone)]
pub struct MidiControllerLane {
    pub kind: MidiControllerKind,
    pub points: Vec<MidiControllerPoint>,
    pub visible: bool,
    pub height: f32,
    pub collapsed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiSysExKind {
    Normal,
    Escaped,
}

#[derive(Debug, Clone)]
pub struct MidiSysExEvent {
    pub kind: MidiSysExKind,
    pub tick: u64,
    pub beat: f32,
    pub data: Vec<u8>,
}

use crate::components::timeline::timeline_state::MidiControllerKind as TlControllerKind;

/// Map a live controller kind to its serialized form.
fn controller_kind_to_project(k: TlControllerKind) -> MidiControllerKind {
    match k {
        TlControllerKind::CC(n) => MidiControllerKind::CC(n),
        TlControllerKind::PitchBend => MidiControllerKind::PitchBend,
        TlControllerKind::ChannelPressure => MidiControllerKind::ChannelPressure,
        TlControllerKind::PolyPressure => MidiControllerKind::PolyPressure,
    }
}

/// Map a serialized controller kind back to the live form.
fn controller_kind_from_project(k: MidiControllerKind) -> TlControllerKind {
    match k {
        MidiControllerKind::CC(n) => TlControllerKind::CC(n),
        MidiControllerKind::PitchBend => TlControllerKind::PitchBend,
        MidiControllerKind::ChannelPressure => TlControllerKind::ChannelPressure,
        MidiControllerKind::PolyPressure => TlControllerKind::PolyPressure,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginFormat {
    Vst3,
    Clap,
    Au,
    Lv2,
    Unknown,
}

// ── Plugin state (binary blobs — future VST/CLAP ready) ──────────────────────

/// Raw binary snapshot of a plugin's internal state. Never JSON/base64.
/// Empty `state_bytes` is valid and means "use plugin defaults".
#[derive(Debug, Clone, Default)]
pub struct PluginStateBlob {
    pub plugin_id: String,
    pub format: Option<PluginFormat>,
    pub state_bytes: Vec<u8>,
    pub vendor: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectPluginInstance {
    pub instance_id: String,
    pub format: PluginFormat,
    pub plugin_path: Option<PathBuf>,
    pub plugin_uid: String,
    pub display_name: String,
    pub state: PluginStateBlob,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectInsert {
    pub id: String,
    pub slot_index: u32,
    pub bypassed: bool,
    pub enabled_audio_output_channels: Vec<u8>,
    /// Mixer-only collapsed/expanded view flag for this instrument's VSTi
    /// multi-out group. Visual state only — never affects routing.
    pub multiout_collapsed: bool,
    pub plugin: Option<ProjectPluginInstance>,
}

// ── Track routing ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTrackInputRouting {
    None,
    AllInputs,
    AudioDeviceChannel {
        device_id: String,
        channel: u32,
    },
    AudioDeviceChannels {
        device_id: String,
        channels: Vec<u32>,
    },
    MidiDevice {
        device_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTrackOutputRouting {
    Main,
    Bus { bus_id: String },
    HardwareOutput { device_id: String, channel: u32 },
    Instrument { track_id: String },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTrackAudioFormat {
    Mono,
    Stereo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectTrackMidiInputRouting {
    None,
    AllInputs,
    MidiDevice { device_id: String },
}

#[derive(Debug, Clone)]
pub struct TrackRouting {
    pub input: ProjectTrackInputRouting,
    pub output: ProjectTrackOutputRouting,
    pub audio_format: ProjectTrackAudioFormat,
    pub midi_input: ProjectTrackMidiInputRouting,
    pub midi_channel: Option<u8>,
    /// `true` plays each note back on its own channel; `false` (default)
    /// forces every note onto `midi_channel` (or channel 1). Added alongside
    /// per-note MIDI channels; missing/old data defaults to `false`, matching
    /// the pre-existing single-channel-per-track behavior.
    pub midi_output_per_note: bool,
    pub sends: Vec<ProjectSend>,
}

impl Default for TrackRouting {
    fn default() -> Self {
        Self {
            input: ProjectTrackInputRouting::None,
            output: ProjectTrackOutputRouting::Main,
            audio_format: ProjectTrackAudioFormat::Stereo,
            midi_input: ProjectTrackMidiInputRouting::None,
            midi_channel: None,
            midi_output_per_note: false,
            sends: Vec::new(),
        }
    }
}

impl TrackRouting {
    pub fn default_for_track_type(track_type: ProjectTrackType) -> Self {
        match track_type {
            ProjectTrackType::Audio => Self::default(),
            ProjectTrackType::Instrument => Self {
                midi_input: ProjectTrackMidiInputRouting::AllInputs,
                ..Self::default()
            },
            ProjectTrackType::Midi => Self {
                output: ProjectTrackOutputRouting::None,
                midi_input: ProjectTrackMidiInputRouting::AllInputs,
                ..Self::default()
            },
            ProjectTrackType::Bus
            | ProjectTrackType::Return
            | ProjectTrackType::Group
            | ProjectTrackType::Master
            // A Video track has no audio or MIDI routing at all.
            | ProjectTrackType::Video => Self::default(),
        }
    }
}

/// Persisted aux send (Phase 3). Mirrors `timeline_state::SendSlotState`
/// minus the transient resolved `target_name`.
#[derive(Debug, Clone)]
pub struct ProjectSend {
    pub id: String,
    pub target_track_id: String,
    pub enabled: bool,
    pub pre_fader: bool,
    pub gain_db: f32,
}

// ── Automation ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AutomationPoint {
    pub beat: f32,
    pub value: f32,
    /// [`AutomationCurve`](crate::components::timeline::timeline_state::AutomationCurve)
    /// tag. Persisted from project version 2 onward; defaults to Linear (0)
    /// when loading older files.
    pub curve: u8,
    /// Per-segment curve tension in `-1.0..=1.0`. Persisted from project version
    /// 21 onward; defaults to `0.0` (straight) for older files.
    pub tension: f32,
}

/// Flattened automation target descriptor for persistence. `tag` matches
/// `AutomationTarget::to_tag`; the descriptor strings are only meaningful for
/// the plugin/send variants and are empty otherwise.
#[derive(Debug, Clone, Default)]
pub struct AutomationTargetDesc {
    pub tag: u8,
    pub insert_id: String,
    pub parameter_id: String,
    pub parameter_name: String,
    pub send_id: String,
}

#[derive(Debug, Clone)]
pub struct AutomationLane {
    pub id: String,
    pub parameter_name: String,
    /// Persisted from project version 2 onward; derived from `parameter_name`
    /// for older files.
    pub target: AutomationTargetDesc,
    pub enabled: bool,
    pub points: Vec<AutomationPoint>,
    pub visible: bool,
}

// ── Tracks & clips ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProjectClip {
    pub id: String,
    pub name: String,
    pub start_beat: f64,
    pub duration_beats: f64,
    pub offset_beats: f32,
    pub gain: f32,
    pub muted: bool,
    pub source: ClipSource,
    /// Non-destructive clip-level stretch / pitch state (persisted v16+). Loads
    /// as [`AudioClipStretchState::default`] (mode Off, ratio 1.0,
    /// preserve_pitch false) for older projects.
    pub stretch: AudioClipStretchState,
}

#[derive(Debug, Clone)]
pub struct ProjectTrack {
    pub id: String,
    pub name: String,
    pub track_type: ProjectTrackType,
    /// Arrangement group membership (v30+). Independent from audio routing.
    pub parent_group_id: Option<String>,
    /// Arrangement folder collapse state (v31+).
    pub group_collapsed: bool,
    /// RGBA hex string e.g. "#56C7C9". Chosen to be human-readable in the file.
    pub color_hex: String,
    pub volume_norm: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub record_arm: bool,
    pub input_monitor: InputMonitorMode,
    pub routing: TrackRouting,
    pub inserts: Vec<ProjectInsert>,
    pub automation_lanes: Vec<AutomationLane>,
    pub clips: Vec<ProjectClip>,
    /// Arrangement row height in px (v17+). `None` uses the default height.
    pub row_height_px: Option<f32>,
    /// Built-in Soundfont Player instrument state (v28+). The player is a track
    /// instrument rather than an insert, so it has no `ProjectInsert` to carry
    /// its settings.
    pub soundfont: Option<ProjectSoundfontPlayer>,
    /// Whether the persisted Track Volume automation lane drives the effective
    /// fader value (v32+). Older projects default to enabled.
    pub volume_automation_read: bool,
}

/// Persisted state of a track's built-in Soundfont Player.
///
/// The `.sf2` itself is referenced by absolute path, not copied into the
/// project: General MIDI banks are large, shared between projects, and often
/// live outside the project folder. A missing file loads as a track with no
/// audible instrument rather than failing the project open.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSoundfontPlayer {
    pub path: Option<PathBuf>,
    pub preset_bank: Option<i32>,
    pub preset_patch: Option<i32>,
    pub volume: f32,
    pub reverb_chorus: bool,
    pub polyphony: u32,
    /// v29: amp envelope over the player's output.
    pub envelope: SoundfontEnvelope,
    /// v29: internal synthesis oversampling.
    pub quality: SoundfontRenderQuality,
}

impl Default for ProjectSoundfontPlayer {
    fn default() -> Self {
        Self {
            path: None,
            preset_bank: None,
            preset_patch: None,
            volume: 1.0,
            reverb_chorus: true,
            polyphony: 64,
            envelope: SoundfontEnvelope::default(),
            quality: SoundfontRenderQuality::default(),
        }
    }
}

// ── Mixer ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProjectMixer {
    pub master_volume_norm: f32,
    pub master_inserts: Vec<ProjectInsert>,
    /// v20: persisted mixer tree expanded node ids.
    pub tree_expanded_node_ids: Vec<String>,
    pub tree_pinned_channel_ids: Vec<String>,
    pub tree_hidden_channel_ids: Vec<String>,
}

impl Default for ProjectMixer {
    fn default() -> Self {
        Self {
            master_volume_norm: crate::components::timeline::timeline_state::volume::db_to_norm(
                0.0,
            ),
            master_inserts: Vec::new(),
            tree_expanded_node_ids: Vec::new(),
            tree_pinned_channel_ids: Vec::new(),
            tree_hidden_channel_ids: Vec::new(),
        }
    }
}

// ── Assets ───────────────────────────────────────────────────────────────────

/// An audio (or other media) file referenced by the project.
#[derive(Debug, Clone)]
pub struct ProjectAsset {
    pub id: String,
    pub original_filename: String,
    /// Path relative to project folder root, e.g. "Media/Audio/kick.wav"
    pub relative_path: Option<String>,
    /// Absolute fallback — used when file isn't inside project folder.
    pub absolute_path: Option<PathBuf>,
    pub duration_secs: Option<f64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// Content fingerprint (`"<len:x>-<crc:08x>"`) of the copied audio bytes.
    /// Persisted from project version 11 so re-imports of identical content can
    /// be deduplicated without re-hashing the whole asset folder on save.
    /// `None` for assets written by older versions.
    pub source_fingerprint: Option<String>,
    /// Project-relative peak cache path, e.g. `Cache/Waveforms/Assets__Audio__kick.wav.peaks`.
    pub waveform_peak_relative_path: Option<String>,
    /// Total PCM frames in the asset (v12+).
    pub duration_samples: Option<u64>,
}

/// Alias used in specs/docs for persisted audio registry entries.
pub type AudioAsset = ProjectAsset;

// ── Settings ──────────────────────────────────────────────────────────────────

/// A persisted tempo marker. `curve` is the `TempoCurve` tag (0=Hold,
/// 1=Linear, 2=Smooth). `id` is empty in v7 files and assigned on load.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectTempoPoint {
    pub id: String,
    pub beat: f64,
    pub bpm: f64,
    pub curve: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectTimeSignaturePoint {
    pub id: String,
    pub beat: f64,
    pub numerator: u16,
    pub denominator: u16,
    pub grouping: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectTimelineMarker {
    pub id: String,
    pub beat: f64,
    pub name: String,
    pub color_hex: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectTimelineRegion {
    pub id: String,
    pub start_beat: f64,
    pub end_beat: f64,
    pub name: String,
    pub color_hex: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectLyricSyllableMode {
    Phrase,
    Syllables,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectLyricSyllable {
    pub text: String,
    pub offset_beats: f64,
    pub duration_beats: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSongSectionType {
    Custom,
    Intro,
    Verse,
    PreChorus,
    Chorus,
    Bridge,
    Solo,
    Outro,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectSongTextEventKind {
    Chord {
        symbol: String,
    },
    Lyric {
        text: String,
        syllable_mode: ProjectLyricSyllableMode,
        continuation: bool,
        duration_beats: Option<f64>,
        syllables: Vec<ProjectLyricSyllable>,
    },
    Section {
        name: String,
        section_type: ProjectSongSectionType,
        color_hex: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSongTextEvent {
    pub id: String,
    pub beat: f64,
    pub kind: ProjectSongTextEventKind,
}

#[derive(Debug, Clone)]
pub struct ProjectSettings {
    pub bpm: f64,
    /// Project-level tempo automation markers. Empty = static tempo at `bpm`.
    pub tempo_points: Vec<ProjectTempoPoint>,
    /// Global time signature markers. Empty on disk = migrate from legacy pair.
    pub time_signature_points: Vec<ProjectTimeSignaturePoint>,
    pub timeline_markers: Vec<ProjectTimelineMarker>,
    pub timeline_regions: Vec<ProjectTimelineRegion>,
    pub song_text_events: Vec<ProjectSongTextEvent>,
    pub time_sig_num: u32,
    pub time_sig_den: u32,
    pub sample_rate: u32,
    pub bit_depth: u32,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            tempo_points: Vec::new(),
            time_signature_points: Vec::new(),
            timeline_markers: Vec::new(),
            timeline_regions: Vec::new(),
            song_text_events: Vec::new(),
            time_sig_num: 4,
            time_sig_den: 4,
            sample_rate: 48000,
            bit_depth: 24,
        }
    }
}

// ── Root project ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FutureboardProject {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub modified_at: u64,
    pub settings: ProjectSettings,
    pub tracks: Vec<ProjectTrack>,
    pub mixer: ProjectMixer,
    pub assets: Vec<ProjectAsset>,
}

impl FutureboardProject {
    pub fn new(name: impl Into<String>) -> Self {
        let now = now_secs();
        Self {
            id: new_id(),
            name: name.into(),
            created_at: now,
            modified_at: now,
            settings: ProjectSettings::default(),
            tracks: Vec::new(),
            mixer: ProjectMixer::default(),
            assets: Vec::new(),
        }
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Converts a `gpui::Rgba` to a hex color string "#RRGGBB".
/// Format an `Rgba` as a stable `#RRGGBB` string. Delegates to the canonical
/// [`crate::color`] helper so there is one color implementation project-wide.
pub fn rgba_to_hex(c: gpui::Rgba) -> String {
    crate::color::rgba_to_hex(c)
}

/// Converts a hex color string to `gpui::Rgba`. Unparseable values fall back to
/// the first default-palette color rather than panicking.
pub fn hex_to_rgba(hex: &str) -> gpui::Rgba {
    crate::color::parse_hex_color(hex).unwrap_or_else(|_| crate::color::auto_color_for_index(0))
}

// ── From TimelineState ────────────────────────────────────────────────────────

use crate::components::timeline::timeline_state::{
    AudioClipStretchState, ClipType, InsertSlotState, TimelineMarkerState, TimelineRegionState,
    TimelineState, TrackType as TlTrackType,
};

fn timeline_insert_to_project(idx: usize, slot: &InsertSlotState) -> ProjectInsert {
    use crate::components::timeline::timeline_state::InsertPluginFormat;

    let plugin = slot.plugin_id.as_ref().map(|pid| {
        let format = match slot.plugin_format {
            Some(InsertPluginFormat::Vst3) => PluginFormat::Vst3,
            Some(InsertPluginFormat::Clap) => PluginFormat::Clap,
            Some(InsertPluginFormat::Au) => PluginFormat::Au,
            Some(InsertPluginFormat::Lv2) => PluginFormat::Lv2,
            _ => PluginFormat::Unknown,
        };
        ProjectPluginInstance {
            instance_id: slot.id.clone(),
            format,
            plugin_path: slot.plugin_path.clone(),
            plugin_uid: pid.clone(),
            display_name: slot.display_name.clone(),
            state: PluginStateBlob {
                plugin_id: pid.clone(),
                format: Some(format),
                state_bytes: slot
                    .vst3_state
                    .as_ref()
                    .map(|state| state.as_ref().clone())
                    .unwrap_or_default(),
                vendor: slot.vendor.clone(),
                name: Some(slot.display_name.clone()),
                version: None,
            },
        }
    });
    ProjectInsert {
        id: slot.id.clone(),
        slot_index: idx as u32,
        bypassed: slot.bypassed,
        enabled_audio_output_channels: slot.enabled_audio_output_channels.clone(),
        multiout_collapsed: slot.multiout_collapsed,
        plugin,
    }
}

fn project_insert_to_timeline(pi: &ProjectInsert) -> InsertSlotState {
    use crate::components::timeline::timeline_state::{
        InsertLoadStatus, InsertPluginFormat, PluginRuntimeBackend, PluginRuntimeState,
    };

    match &pi.plugin {
        Some(plugin) => {
            let plugin_format = match plugin.format {
                PluginFormat::Vst3 => InsertPluginFormat::Vst3,
                PluginFormat::Clap => InsertPluginFormat::Clap,
                PluginFormat::Au => InsertPluginFormat::Au,
                PluginFormat::Lv2 => InsertPluginFormat::Lv2,
                PluginFormat::Unknown => InsertPluginFormat::Unknown,
            };
            let is_builtin = SpherePluginHost::builtin_audio_bridge_supported(&plugin.plugin_uid);
            let bridge = SpherePluginHost::plugin_host_client::plugin_host_bridge_enabled()
                && (matches!(
                    plugin_format,
                    InsertPluginFormat::Vst3 | InsertPluginFormat::Au
                ) || is_builtin);
            // Only a format with a module file can be missing from disk; an
            // Audio Unit's absence surfaces when the host tries to instantiate
            // its component id.
            let path_missing = !is_builtin
                && plugin_format.has_module_file()
                && plugin
                    .plugin_path
                    .as_ref()
                    .is_none_or(|path| !path.exists());
            let (load_status, runtime_state, runtime_backend) = if path_missing {
                (
                    InsertLoadStatus::Missing("Plugin file not found".to_string()),
                    PluginRuntimeState::Missing("Plugin file not found".to_string()),
                    if bridge {
                        PluginRuntimeBackend::ExternalBridge
                    } else {
                        PluginRuntimeBackend::InProcess
                    },
                )
            } else {
                (
                    InsertLoadStatus::Loading,
                    PluginRuntimeState::NotLoaded,
                    if bridge {
                        PluginRuntimeBackend::ExternalBridge
                    } else {
                        PluginRuntimeBackend::InProcess
                    },
                )
            };
            InsertSlotState {
                id: pi.id.clone(),
                plugin_id: Some(plugin.plugin_uid.clone()),
                plugin_path: plugin.plugin_path.clone(),
                plugin_format: Some(plugin_format),
                vendor: plugin
                    .state
                    .vendor
                    .clone()
                    .filter(|vendor| !vendor.trim().is_empty()),
                display_name: plugin.display_name.clone(),
                enabled: true,
                bypassed: pi.bypassed,
                load_status,
                runtime_backend,
                runtime_state,
                host_pid: None,
                parameters: Vec::new(),
                enabled_audio_output_channels: pi.enabled_audio_output_channels.clone(),
                // Re-detected from the host on ProcessingPrepared after load.
                output_bus_channel_counts: Vec::new(),
                multiout_collapsed: pi.multiout_collapsed,
                pending_open_editor: false,
                vst3_state: (!plugin.state.state_bytes.is_empty())
                    .then(|| std::sync::Arc::new(plugin.state.state_bytes.clone())),
            }
        }
        None => InsertSlotState::empty(pi.id.clone()),
    }
}

impl From<&TimelineState> for FutureboardProject {
    fn from(tl: &TimelineState) -> Self {
        let tracks = tl
            .tracks
            .iter()
            // VSTi multi-out child strips (`vsti-out:{insert}:bus:{n}`) ARE
            // persisted: their ids are deterministic, so
            // `ensure_vsti_output_child_tracks` retains the loaded rows (never
            // duplicates them) once the plugin reports its bus layout, and
            // removes rows the layout no longer supports. Persisting them is
            // what carries per-bus mixer state and substrip insert chains
            // (including plugin state) across save/load.
            .map(|t| {
                let track_type = match t.track_type {
                    TlTrackType::Audio => ProjectTrackType::Audio,
                    TlTrackType::Midi => ProjectTrackType::Midi,
                    TlTrackType::Instrument => ProjectTrackType::Instrument,
                    TlTrackType::Bus => ProjectTrackType::Bus,
                    TlTrackType::Return => ProjectTrackType::Return,
                    TlTrackType::Group => ProjectTrackType::Group,
                    TlTrackType::Master => ProjectTrackType::Master,
                    TlTrackType::Video => ProjectTrackType::Video,
                };
                let clips = t
                    .clips
                    .iter()
                    .map(|c| {
                        let source = match &c.clip_type {
                            ClipType::Audio {
                                file_id,
                                source_path,
                            } => {
                                let path = source_path.as_deref().map(PathBuf::from);
                                if path
                                    .as_ref()
                                    .and_then(|p| p.extension())
                                    .and_then(|ext| ext.to_str())
                                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rauf"))
                                {
                                    let metadata_path = path.as_ref().map(|p| {
                                        let mut value = p.as_os_str().to_os_string();
                                        value.push(".json");
                                        PathBuf::from(value)
                                    });
                                    ClipSource::Rauf {
                                        asset_id: file_id.clone(),
                                        source_path: path.unwrap_or_default(),
                                        metadata_path,
                                        sample_format: "s32le".to_string(),
                                        sample_rate: 48_000,
                                        channels: 0,
                                        start_frame: 0,
                                        length_frames: 0,
                                    }
                                } else {
                                    ClipSource::Audio {
                                        asset_id: file_id.clone(),
                                        source_path: path,
                                    }
                                }
                            }
                            ClipType::Midi {
                                notes,
                                controller_lanes,
                                sysex_events,
                                articulations,
                            } => ClipSource::Midi {
                                notes: notes
                                    .iter()
                                    .map(|n| MidiNote {
                                        id: n.id,
                                        pitch: n.pitch,
                                        start_beats: n.start,
                                        duration_beats: n.duration,
                                        velocity: n.velocity,
                                        release_velocity: n.release_velocity.unwrap_or(0),
                                        muted: n.muted,
                                        channel: n.channel.ui(),
                                        articulation: n
                                            .articulation
                                            .map(|a| a.to_tag())
                                            .unwrap_or(0),
                                    })
                                    .collect(),
                                controller_lanes: controller_lanes
                                    .iter()
                                    .map(|lane| MidiControllerLane {
                                        kind: controller_kind_to_project(lane.kind),
                                        points: lane
                                            .points
                                            .iter()
                                            .map(|p| MidiControllerPoint {
                                                id: p.id,
                                                beat: p.beat,
                                                value: p.value,
                                            })
                                            .collect(),
                                        visible: lane.visible,
                                        height: lane.height,
                                        collapsed: lane.collapsed,
                                    })
                                    .collect(),
                                sysex_events: sysex_events
                                    .iter()
                                    .map(|event| MidiSysExEvent {
                                        kind: match event.kind {
                                            crate::components::timeline::timeline_state::MidiSysExKind::Normal => {
                                                MidiSysExKind::Normal
                                            }
                                            crate::components::timeline::timeline_state::MidiSysExKind::Escaped => {
                                                MidiSysExKind::Escaped
                                            }
                                        },
                                        tick: event.tick,
                                        beat: event.beat,
                                        data: event.data.clone(),
                                    })
                                    .collect(),
                                articulations: articulations
                                    .iter()
                                    .map(|event| MidiArticulation {
                                        beat: event.beat,
                                        articulation: event.articulation.to_tag(),
                                    })
                                    .collect(),
                            },
                            ClipType::Video {
                                file_id,
                                source_path,
                            } => ClipSource::Video {
                                asset_id: file_id.clone(),
                                source_path: source_path.as_deref().map(PathBuf::from),
                            },
                        };
                        ProjectClip {
                            id: c.id.clone(),
                            name: c.name.clone(),
                            start_beat: c.start_beat as f64,
                            duration_beats: c.duration_beats as f64,
                            offset_beats: c.offset_beats,
                            gain: c.gain,
                            muted: c.muted,
                            source,
                            stretch: c.stretch.clone(),
                        }
                    })
                    .collect();
                let automation_lanes = t
                    .automation_lanes
                    .iter()
                    .map(|al| AutomationLane {
                        id: al.id.clone(),
                        parameter_name: al.name.clone(),
                        target: target_to_desc(&al.target),
                        enabled: al.enabled,
                        points: al
                            .points
                            .iter()
                            .map(|p| AutomationPoint {
                                beat: p.beat,
                                value: p.value,
                                curve: p.curve.to_tag(),
                                tension: p.tension,
                            })
                            .collect(),
                        visible: al.visible,
                    })
                    .collect();
                ProjectTrack {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    track_type,
                    parent_group_id: t.parent_group_id.clone(),
                    group_collapsed: t.group_collapsed,
                    color_hex: rgba_to_hex(t.color),
                    volume_norm: t.volume,
                    pan: t.pan,
                    muted: t.muted,
                    solo: t.solo,
                    record_arm: t.armed,
                    input_monitor: t.input_monitor,
                    routing: TrackRouting {
                        input: runtime_to_v33_track_input_routing(
                            t.routing.audio_input_connection_id.as_ref(),
                            &t.routing.midi_input,
                            &tl.audio_connections,
                            t.track_type,
                        )
                        .routing,
                        output: timeline_output_to_project(&t.routing.output),
                        audio_format: timeline_audio_format_to_project(t.routing.audio_format),
                        midi_input: timeline_midi_input_to_project(&t.routing.midi_input),
                        midi_channel: t.routing.midi_channel.map(|ch| ch.clamp(1, 16)),
                        midi_output_per_note: t.routing.midi_output_per_note,
                        sends: t
                            .sends
                            .iter()
                            .map(|s| ProjectSend {
                                id: s.id.clone(),
                                target_track_id: s.target_track_id.clone(),
                                enabled: s.enabled,
                                pre_fader: s.pre_fader,
                                gain_db: s.gain_db,
                            })
                            .collect(),
                    },
                    inserts: t
                        .inserts
                        .iter()
                        .enumerate()
                        .map(|(idx, slot)| timeline_insert_to_project(idx, slot))
                        .collect(),
                    automation_lanes,
                    clips,
                    row_height_px: tl.track_view_layout.height_for(&t.id).filter(|h| {
                        (*h - crate::components::timeline::timeline_state::DEFAULT_TRACK_HEIGHT)
                            .abs()
                            >= 0.01
                    }),
                    soundfont: t.builtin_soundfont_player.then(|| ProjectSoundfontPlayer {
                        path: t.soundfont_path.as_ref().map(PathBuf::from),
                        preset_bank: t.soundfont_preset.map(|(bank, _)| bank),
                        preset_patch: t.soundfont_preset.map(|(_, patch)| patch),
                        volume: t.soundfont_volume,
                        reverb_chorus: t.soundfont_reverb_chorus,
                        polyphony: t.soundfont_polyphony as u32,
                        envelope: t.soundfont_envelope,
                        quality: t.soundfont_quality,
                    }),
                    volume_automation_read: t.volume_automation_read,
                }
            })
            .collect();
        let mut project = FutureboardProject::new("Untitled Project");
        project.settings.bpm = tl.bpm as f64;
        project.settings.tempo_points = tl
            .tempo_map
            .points
            .iter()
            .map(|p| ProjectTempoPoint {
                id: p.id.clone(),
                beat: p.beat,
                bpm: p.bpm,
                curve: p.curve.to_tag(),
            })
            .collect();
        project.settings.time_signature_points = tl
            .time_signature_map
            .points
            .iter()
            .map(|p| ProjectTimeSignaturePoint {
                id: p.id.clone(),
                beat: p.beat,
                numerator: p.numerator,
                denominator: p.denominator,
                grouping: p.effective_grouping(),
            })
            .collect();
        project.settings.timeline_markers = tl
            .markers
            .iter()
            .map(|marker| ProjectTimelineMarker {
                id: marker.id.clone(),
                beat: marker.beat,
                name: marker.name.clone(),
                color_hex: marker.color_hex.clone(),
            })
            .collect();
        project.settings.timeline_regions = tl
            .regions
            .iter()
            .map(|region| ProjectTimelineRegion {
                id: region.id.clone(),
                start_beat: region.start_beat,
                end_beat: region.end_beat,
                name: region.name.clone(),
                color_hex: region.color_hex.clone(),
            })
            .collect();
        project.settings.song_text_events = tl
            .song_text_events
            .iter()
            .map(|event| ProjectSongTextEvent {
                id: event.id.clone(),
                beat: event.beat,
                kind: match &event.kind {
                    crate::components::timeline::timeline_state::SongTextEventKind::Chord(
                        chord,
                    ) => ProjectSongTextEventKind::Chord {
                        symbol: chord.symbol.clone(),
                    },
                    crate::components::timeline::timeline_state::SongTextEventKind::Lyric(
                        lyric,
                    ) => ProjectSongTextEventKind::Lyric {
                        text: lyric.text.clone(),
                        syllable_mode: match lyric.syllable_mode {
                            crate::components::timeline::timeline_state::LyricSyllableMode::Phrase => {
                                ProjectLyricSyllableMode::Phrase
                            }
                            crate::components::timeline::timeline_state::LyricSyllableMode::Syllables => {
                                ProjectLyricSyllableMode::Syllables
                            }
                        },
                        continuation: lyric.continuation,
                        duration_beats: lyric.duration_beats,
                        syllables: lyric
                            .syllables
                            .iter()
                            .map(|syllable| ProjectLyricSyllable {
                                text: syllable.text.clone(),
                                offset_beats: syllable.offset_beats,
                                duration_beats: syllable.duration_beats,
                            })
                            .collect(),
                    },
                    crate::components::timeline::timeline_state::SongTextEventKind::Section(
                        section,
                    ) => ProjectSongTextEventKind::Section {
                        name: section.name.clone(),
                        section_type: match section.section_type {
                            crate::components::timeline::timeline_state::SongSectionType::Custom => {
                                ProjectSongSectionType::Custom
                            }
                            crate::components::timeline::timeline_state::SongSectionType::Intro => {
                                ProjectSongSectionType::Intro
                            }
                            crate::components::timeline::timeline_state::SongSectionType::Verse => {
                                ProjectSongSectionType::Verse
                            }
                            crate::components::timeline::timeline_state::SongSectionType::PreChorus => {
                                ProjectSongSectionType::PreChorus
                            }
                            crate::components::timeline::timeline_state::SongSectionType::Chorus => {
                                ProjectSongSectionType::Chorus
                            }
                            crate::components::timeline::timeline_state::SongSectionType::Bridge => {
                                ProjectSongSectionType::Bridge
                            }
                            crate::components::timeline::timeline_state::SongSectionType::Solo => {
                                ProjectSongSectionType::Solo
                            }
                            crate::components::timeline::timeline_state::SongSectionType::Outro => {
                                ProjectSongSectionType::Outro
                            }
                        },
                        color_hex: section.color_hex.clone(),
                    },
                },
            })
            .collect();
        project.settings.time_sig_num = tl.time_signature_num;
        project.settings.time_sig_den = tl.time_signature_den;
        project.tracks = tracks;
        project.mixer.master_volume_norm = tl.master.volume;
        project.mixer.master_inserts = tl
            .master
            .inserts
            .iter()
            .enumerate()
            .map(|(idx, slot)| timeline_insert_to_project(idx, slot))
            .collect();
        project.mixer.tree_expanded_node_ids = tl.mixer_tree.expanded_list();
        project.mixer.tree_pinned_channel_ids = tl.mixer_tree.pinned_list();
        project.mixer.tree_hidden_channel_ids = tl.mixer_tree.hidden_list();
        project
    }
}

/// Apply a loaded `FutureboardProject` back onto an existing `TimelineState`.
pub fn apply_to_timeline(project: &FutureboardProject, tl: &mut TimelineState) {
    // Audio Connections generated while migrating v33 routing. Populated as
    // tracks are converted below, then installed on the timeline state.
    let migration_ports = crate::audio_connections::current_available_ports();
    let mut migrated_connections = tl.audio_connections.clone();
    let mut migration_warnings: Vec<crate::project::routing_migration::RoutingMigrationWarning> =
        Vec::new();
    use crate::components::timeline::timeline_state::{
        AutomationLaneState, AutomationPoint as TlAutoPoint, ClipState, MidiChannel,
        MidiControllerLane as TlControllerLane, MidiControllerPoint as TlControllerPoint,
        MidiNoteState, SendSlotState, TrackState,
    };

    tl.bpm = project.settings.bpm as f32;
    tl.tempo_map = crate::components::timeline::timeline_state::TempoMap::with_points(
        project
            .settings
            .tempo_points
            .iter()
            .map(|p| {
                crate::components::timeline::timeline_state::TempoPoint::with_id(
                    p.id.clone(),
                    p.beat,
                    p.bpm,
                    crate::components::timeline::timeline_state::TempoCurve::from_tag(p.curve),
                )
            })
            .collect(),
    );
    tl.tempo_map.ensure_point_ids();
    tl.markers = project
        .settings
        .timeline_markers
        .iter()
        .map(|marker| {
            TimelineMarkerState::with_id(
                marker.id.clone(),
                marker.beat,
                marker.name.clone(),
                marker.color_hex.clone(),
            )
        })
        .collect();
    tl.markers
        .sort_by(|a, b| a.beat.total_cmp(&b.beat).then_with(|| a.id.cmp(&b.id)));
    tl.regions = project
        .settings
        .timeline_regions
        .iter()
        .map(|region| {
            TimelineRegionState::with_id(
                region.id.clone(),
                region.start_beat,
                region.end_beat,
                region.name.clone(),
                region.color_hex.clone(),
            )
        })
        .collect();
    tl.regions.sort_by(|a, b| {
        a.start_beat
            .total_cmp(&b.start_beat)
            .then_with(|| a.id.cmp(&b.id))
    });
    let song_text_events = project
        .settings
        .song_text_events
        .iter()
        .filter_map(|event| {
            use crate::components::timeline::timeline_state::{
                ChordEvent, LyricEvent, LyricSyllable, LyricSyllableMode, SectionEvent,
                SongSectionType, SongTextEvent, SongTextEventKind,
            };

            let kind = match &event.kind {
                ProjectSongTextEventKind::Chord { symbol } => {
                    SongTextEventKind::Chord(ChordEvent {
                        symbol: symbol.clone(),
                    })
                }
                ProjectSongTextEventKind::Lyric {
                    text,
                    syllable_mode,
                    continuation,
                    duration_beats,
                    syllables,
                } => SongTextEventKind::Lyric(LyricEvent {
                    text: text.clone(),
                    syllable_mode: match syllable_mode {
                        ProjectLyricSyllableMode::Phrase => LyricSyllableMode::Phrase,
                        ProjectLyricSyllableMode::Syllables => LyricSyllableMode::Syllables,
                    },
                    continuation: *continuation,
                    duration_beats: *duration_beats,
                    syllables: syllables
                        .iter()
                        .map(|syllable| LyricSyllable {
                            text: syllable.text.clone(),
                            offset_beats: syllable.offset_beats,
                            duration_beats: syllable.duration_beats,
                        })
                        .collect(),
                }),
                ProjectSongTextEventKind::Section {
                    name,
                    section_type,
                    color_hex,
                } => SongTextEventKind::Section(SectionEvent {
                    name: name.clone(),
                    section_type: match section_type {
                        ProjectSongSectionType::Custom => SongSectionType::Custom,
                        ProjectSongSectionType::Intro => SongSectionType::Intro,
                        ProjectSongSectionType::Verse => SongSectionType::Verse,
                        ProjectSongSectionType::PreChorus => SongSectionType::PreChorus,
                        ProjectSongSectionType::Chorus => SongSectionType::Chorus,
                        ProjectSongSectionType::Bridge => SongSectionType::Bridge,
                        ProjectSongSectionType::Solo => SongSectionType::Solo,
                        ProjectSongSectionType::Outro => SongSectionType::Outro,
                    },
                    color_hex: color_hex.clone(),
                }),
            };
            SongTextEvent::with_id(event.id.clone(), event.beat, kind)
        })
        .collect();
    tl.replace_song_text_events(song_text_events);
    if project.settings.time_signature_points.is_empty() {
        tl.time_signature_map =
            crate::components::timeline::timeline_state::TimeSignatureMap::with_default_4_4();
        tl.time_signature_map.points[0].numerator =
            project.settings.time_sig_num.clamp(1, 64) as u16;
        tl.time_signature_map.points[0].denominator =
            project.settings.time_sig_den.clamp(1, 32) as u16;
    } else {
        tl.time_signature_map =
            crate::components::timeline::timeline_state::TimeSignatureMap::with_points(
                project
                    .settings
                    .time_signature_points
                    .iter()
                    .map(|p| {
                        crate::components::timeline::timeline_state::TimeSignaturePoint::with_grouping(
                            p.id.clone(),
                            p.beat,
                            p.numerator,
                            p.denominator,
                            p.grouping.clone(),
                        )
                    })
                    .collect(),
            );
        tl.time_signature_map.ensure_point_ids();
    }
    tl.sync_legacy_time_signature_fields();
    tl.master.volume = project.mixer.master_volume_norm;
    tl.master.inserts = project
        .mixer
        .master_inserts
        .iter()
        .map(project_insert_to_timeline)
        .collect();
    tl.mixer_tree =
        crate::components::timeline::timeline_state::MixerTreeViewState::from_project_lists(
            &project.mixer.tree_expanded_node_ids,
            &project.mixer.tree_pinned_channel_ids,
            &project.mixer.tree_hidden_channel_ids,
        );

    tl.tracks = project
        .tracks
        .iter()
        .map(|pt| {
            let track_type = match pt.track_type {
                ProjectTrackType::Audio => TlTrackType::Audio,
                ProjectTrackType::Midi => TlTrackType::Midi,
                ProjectTrackType::Instrument => TlTrackType::Instrument,
                ProjectTrackType::Bus => TlTrackType::Bus,
                ProjectTrackType::Return => TlTrackType::Return,
                ProjectTrackType::Group => TlTrackType::Group,
                ProjectTrackType::Master => TlTrackType::Master,
                ProjectTrackType::Video => TlTrackType::Video,
            };
            let clips = pt
                .clips
                .iter()
                .map(|pc| {
                    let clip_type = match &pc.source {
                        ClipSource::Audio {
                            asset_id,
                            source_path,
                        } => ClipType::Audio {
                            file_id: asset_id.clone(),
                            source_path: source_path
                                .as_ref()
                                .map(|p| p.to_string_lossy().into_owned()),
                        },
                        ClipSource::Rauf {
                            asset_id,
                            source_path,
                            ..
                        } => ClipType::Audio {
                            file_id: asset_id.clone(),
                            source_path: Some(source_path.to_string_lossy().into_owned()),
                        },
                        ClipSource::Midi {
                            notes,
                            controller_lanes,
                            sysex_events,
                            articulations,
                        } => ClipType::Midi {
                            notes: notes
                                .iter()
                                .map(|n| {
                                    let mut note = MidiNoteState::from_persisted(
                                        n.id,
                                        n.pitch,
                                        n.start_beats,
                                        n.duration_beats,
                                        n.velocity,
                                        if n.release_velocity == 0 {
                                            None
                                        } else {
                                            Some(n.release_velocity)
                                        },
                                    );
                                    note.muted = n.muted;
                                    note.channel = MidiChannel::from_ui(n.channel);
                                    note.articulation =
                                        crate::components::timeline::timeline_state::ArticulationId::from_tag(
                                            n.articulation,
                                        );
                                    note
                                })
                                .collect(),
                            controller_lanes: controller_lanes
                                .iter()
                                .map(|lane| TlControllerLane {
                                    kind: controller_kind_from_project(lane.kind),
                                    points: lane
                                        .points
                                        .iter()
                                        .map(|p| {
                                            TlControllerPoint::from_persisted(p.id, p.beat, p.value)
                                        })
                                        .collect(),
                                    visible: lane.visible,
                                    height: lane.height,
                                    collapsed: lane.collapsed,
                                })
                                .collect(),
                            sysex_events: sysex_events
                                .iter()
                                .map(|event| crate::components::timeline::timeline_state::MidiSysExEvent {
                                    kind: match event.kind {
                                        MidiSysExKind::Normal => crate::components::timeline::timeline_state::MidiSysExKind::Normal,
                                        MidiSysExKind::Escaped => crate::components::timeline::timeline_state::MidiSysExKind::Escaped,
                                    },
                                    tick: event.tick,
                                    beat: event.beat,
                                    data: event.data.clone(),
                                })
                                .collect(),
                            // Fresh transient event ids on load, like note ids.
                            // Unknown tags from newer files degrade to "none".
                            articulations: articulations
                                .iter()
                                .filter_map(|event| {
                                    crate::components::timeline::timeline_state::ArticulationId::from_tag(
                                        event.articulation,
                                    )
                                    .map(|articulation| {
                                        crate::components::timeline::timeline_state::MidiArticulationEvent::new(
                                            event.beat,
                                            articulation,
                                        )
                                    })
                                })
                                .collect(),
                        },
                        ClipSource::Video {
                            asset_id,
                            source_path,
                        } => ClipType::Video {
                            file_id: asset_id.clone(),
                            source_path: source_path
                                .as_ref()
                                .map(|p| p.to_string_lossy().into_owned()),
                        },
                        ClipSource::Empty => ClipType::Midi {
                            notes: Vec::new(),
                            controller_lanes: Vec::new(),
                            sysex_events: Vec::new(),
                            articulations: Vec::new(),
                        },
                    };
                    ClipState {
                        id: pc.id.clone(),
                        name: pc.name.clone(),
                        start_beat: pc.start_beat as f32,
                        duration_beats: pc.duration_beats as f32,
                        source_duration_seconds: match &pc.source {
                            ClipSource::Audio { asset_id, .. }
                            | ClipSource::Rauf { asset_id, .. } => project
                                .assets
                                .iter()
                                .find(|asset| asset.id == *asset_id)
                                .and_then(|asset| asset.duration_secs),
                            _ => None,
                        },
                        offset_beats: pc.offset_beats,
                        gain: pc.gain,
                        clip_type,
                        muted: pc.muted,
                        audio_import: crate::components::timeline::timeline_state::AudioImportState::default(),
                        stretch: pc.stretch.clone(),
                    }
                })
                .collect();
            let automation_lanes = pt
                .automation_lanes
                .iter()
                .map(|al| AutomationLaneState {
                    id: al.id.clone(),
                    name: al.parameter_name.clone(),
                    target: desc_to_target(&al.target, &al.parameter_name),
                    enabled: al.enabled,
                    visible: al.visible,
                    points: al
                        .points
                        .iter()
                        .map(|p| {
                            let mut point = TlAutoPoint::with_curve(
                                p.beat,
                                p.value,
                                crate::components::timeline::timeline_state::AutomationCurve::from_tag(
                                    p.curve,
                                ),
                            );
                            point.set_tension(p.tension);
                            point
                        })
                        .collect(),
                })
                .collect();
            let inserts: Vec<InsertSlotState> = pt
                .inserts
                .iter()
                .map(project_insert_to_timeline)
                .collect();
            let sends = pt
                .routing
                .sends
                .iter()
                .map(|s| {
                    let target_name = project
                        .tracks
                        .iter()
                        .find(|t| t.id == s.target_track_id)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| s.target_track_id.clone());
                    SendSlotState {
                        id: s.id.clone(),
                        target_track_id: s.target_track_id.clone(),
                        target_name,
                        enabled: s.enabled,
                        pre_fader: s.pre_fader,
                        gain_db: s.gain_db,
                    }
                })
                .collect();
            let instrument_plugin_instance_id = match track_type {
                crate::components::timeline::timeline_state::TrackType::Instrument
                | crate::components::timeline::timeline_state::TrackType::Midi => inserts
                    .first()
                    .filter(|slot| slot.plugin_id.is_some())
                    .map(|slot| slot.id.clone()),
                _ => None,
            };
            TrackState {
                listen: crate::components::timeline::timeline_state::ListenMode::Off,
                id: pt.id.clone(),
                name: pt.name.clone(),
                track_type,
                parent_group_id: pt.parent_group_id.clone(),
                group_collapsed: pt.group_collapsed,
                color: hex_to_rgba(&pt.color_hex),
                volume: pt.volume_norm,
                // Effective volume is derived (recomputed from automation at the
                // playhead after load); seed it from the persisted base so the
                // first frame before any recompute shows the saved value.
                volume_effective: pt.volume_norm,
                volume_automation_read: pt.volume_automation_read,
                pan: pt.pan,
                muted: pt.muted,
                solo: pt.solo,
                armed: pt.record_arm,
                input_monitor: pt.input_monitor,
                meter_level_l: 0.0,
                meter_level_r: 0.0,
                meter_peak_hold_l: 0.0,
                meter_peak_hold_r: 0.0,
                meter_clip: false,
                clips,
                automation_lanes,
                lane_mode: crate::components::timeline::timeline_state::TrackLaneMode::Clips,
                selected_automation_target: None,
                inserts,
                sends,
                routing: {
                    // v33 stores one combined input field. Run the migration
                    // adapter here, where the project registry is in scope, so
                    // the runtime track ends up with the split fields.
                    let mut routing = project_routing_to_timeline(&pt.routing, track_type);
                    let (connection_id, midi_input) = legacy_routing_to_runtime(
                        &pt.routing.input,
                        routing.midi_input.clone(),
                        track_type,
                        &pt.id,
                        &pt.name,
                        &mut migrated_connections,
                        &migration_ports,
                        &mut migration_warnings,
                    );
                    routing.audio_input_connection_id = connection_id;
                    routing.midi_input = midi_input;
                    routing
                },
                instrument_plugin_instance_id,
                builtin_soundfont_player: pt.soundfont.is_some(),
                soundfont_path: pt
                    .soundfont
                    .as_ref()
                    .and_then(|sf| sf.path.as_ref())
                    .map(|path| path.to_string_lossy().into_owned()),
                soundfont_preset: pt
                    .soundfont
                    .as_ref()
                    .and_then(|sf| sf.preset_bank.zip(sf.preset_patch)),
                soundfont_volume: pt
                    .soundfont
                    .as_ref()
                    .map(|sf| sf.volume.clamp(0.0, 1.0))
                    .unwrap_or(1.0),
                soundfont_reverb_chorus: pt
                    .soundfont
                    .as_ref()
                    .map(|sf| sf.reverb_chorus)
                    .unwrap_or(true),
                soundfont_polyphony: pt
                    .soundfont
                    .as_ref()
                    .map(|sf| sf.polyphony.clamp(1, 256) as usize)
                    .unwrap_or(64),
                soundfont_envelope: pt
                    .soundfont
                    .as_ref()
                    .map(|sf| sf.envelope.sanitized())
                    .unwrap_or_default(),
                soundfont_quality: pt
                    .soundfont
                    .as_ref()
                    .map(|sf| sf.quality)
                    .unwrap_or_default(),
            }
        })
        .collect();

    let valid_group_ids: std::collections::HashSet<String> = tl
        .tracks
        .iter()
        .filter(|track| track.track_type == TlTrackType::Group)
        .map(|track| track.id.clone())
        .collect();
    for track in &mut tl.tracks {
        if track
            .parent_group_id
            .as_ref()
            .is_some_and(|group_id| !valid_group_ids.contains(group_id))
        {
            track.parent_group_id = None;
        }
    }

    tl.track_view_layout.clear();
    tl.track_height_resize = None;
    tl.track_height_resize_arm = None;
    for pt in &project.tracks {
        let Some(height) = pt.row_height_px else {
            continue;
        };
        let Some(track) = tl.tracks.iter().find(|t| t.id == pt.id) else {
            continue;
        };
        let clamped = crate::components::timeline::timeline_state::clamp_track_row_height(
            track.track_type,
            height,
        );
        tl.track_view_layout.set_height(pt.id.clone(), clamped);
    }

    // Install the Audio Connections generated while converting v33 routing,
    // then validate them against the current hardware. A device that is not
    // present yields DeviceMissing — the connection and every track reference
    // survive, so reconnecting restores the route.
    migrated_connections.revalidate(&migration_ports);
    tl.audio_connections = migrated_connections;
    for warning in &migration_warnings {
        eprintln!("[project] {}", warning.message());
    }
}

// ── v33 persistence boundary adapter ────────────────────────────────────────
//
// Turn A keeps the on-disk format at v33, which stores ONE combined
// `ProjectTrackInputRouting` covering both audio device/channel routing and a
// MIDI device. The runtime model is split into two independent fields, so this
// boundary is the only place the two representations meet.
//
// These helpers are deliberately private to the persistence layer: nothing in
// the runtime may reach for the combined union.

/// What a v33 encode decided to write, plus whether anything could not be
/// represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct V33TrackInputRoutingEncodeResult {
    pub(crate) routing: ProjectTrackInputRouting,
    /// `Some` when the runtime held both an audio connection and a meaningful
    /// MIDI device but v33 can only store one. The runtime keeps both; the
    /// caller surfaces this so the save is never claimed to be lossless.
    pub(crate) dropped: Option<String>,
}

/// Encode split runtime routing into the single v33 field.
///
/// Track type decides which route is primary, because that is what the old
/// format and the old UI actually supported:
///
/// * **Audio** tracks: the audio connection wins; a MIDI device is only
///   written when there is no audio connection.
/// * **MIDI / Instrument** tracks: the MIDI route wins; an audio connection is
///   never folded into the legacy field for these types.
///
/// Nothing is cleared from the runtime state either way.
pub(crate) fn runtime_to_v33_track_input_routing(
    audio_input_connection_id: Option<&crate::audio_connections::AudioConnectionId>,
    midi_input: &crate::components::timeline::timeline_state::TrackMidiInputRouting,
    registry: &crate::audio_connections::AudioConnectionRegistry,
    track_type: TlTrackType,
) -> V33TrackInputRoutingEncodeResult {
    use crate::components::timeline::timeline_state::TrackMidiInputRouting as M;

    // The audio side, expressed the way v33 stored it. `None` when there is no
    // assignment or the connection no longer resolves to concrete ports.
    let audio = audio_input_connection_id.and_then(|id| {
        let connection = registry.get(id)?;
        let device_id = connection.device_id.clone()?;
        let channels: Vec<u32> = (0..connection.channel_layout.channel_count())
            .filter_map(|logical| {
                connection
                    .binding(logical)
                    .map(|binding| binding.physical_port_id.port_index)
            })
            .collect();
        if channels.is_empty() {
            return None;
        }
        Some(if channels.len() == 1 {
            ProjectTrackInputRouting::AudioDeviceChannel {
                device_id,
                channel: channels[0],
            }
        } else {
            ProjectTrackInputRouting::AudioDeviceChannels {
                device_id,
                channels,
            }
        })
    });

    let midi = match midi_input {
        M::MidiDevice { device_id } => Some(ProjectTrackInputRouting::MidiDevice {
            device_id: device_id.clone(),
        }),
        // `None` / `AllInputs` carry no device identity that v33's combined
        // field could hold, and the dedicated `midi_input` field stores them
        // losslessly anyway.
        M::None | M::AllInputs => None,
    };

    let audio_is_primary = matches!(track_type, TlTrackType::Audio);
    let (routing, dropped) = match (audio_is_primary, audio, midi) {
        // Audio track holding both: audio wins, MIDI device is reported.
        (true, Some(audio), Some(_)) => (
            audio,
            Some(format!("MIDI input \"{}\"", midi_input.label())),
        ),
        (true, Some(audio), None) => (audio, None),
        (true, None, Some(midi)) => (midi, None),
        // MIDI / instrument track holding both: MIDI wins, audio is reported.
        (false, Some(_), Some(midi)) => (midi, Some("audio input connection".to_string())),
        (false, Some(_), None) => (
            ProjectTrackInputRouting::None,
            Some("audio input connection".to_string()),
        ),
        (false, None, Some(midi)) => (midi, None),
        (_, None, None) => (ProjectTrackInputRouting::None, None),
    };
    V33TrackInputRoutingEncodeResult { routing, dropped }
}

/// Decode the v33 combined field into the split runtime fields.
///
/// Audio routing becomes a project-local `AudioConnection` in `registry`,
/// reusing one connection per distinct `(device_id, ordered channel list)`.
/// MIDI routing follows the confirmed Case A/B/C rules from
/// [`crate::project::routing_migration`].
pub(crate) fn legacy_routing_to_runtime(
    legacy: &ProjectTrackInputRouting,
    existing_midi_input: crate::components::timeline::timeline_state::TrackMidiInputRouting,
    track_type: TlTrackType,
    track_id: &str,
    track_name: &str,
    registry: &mut crate::audio_connections::AudioConnectionRegistry,
    ports: &crate::audio_connections::AvailablePorts,
    warnings: &mut Vec<crate::project::routing_migration::RoutingMigrationWarning>,
) -> (
    Option<crate::audio_connections::AudioConnectionId>,
    crate::components::timeline::timeline_state::TrackMidiInputRouting,
) {
    use crate::project::routing_migration::{
        migrate_track_routing, LegacyTrackInputRouting, LegacyTrackRouting,
    };

    let legacy_input = match legacy {
        ProjectTrackInputRouting::None => LegacyTrackInputRouting::None,
        ProjectTrackInputRouting::AllInputs => LegacyTrackInputRouting::AllInputs,
        ProjectTrackInputRouting::AudioDeviceChannel { device_id, channel } => {
            LegacyTrackInputRouting::AudioDeviceChannel {
                device_id: device_id.clone(),
                channel: *channel,
            }
        }
        ProjectTrackInputRouting::AudioDeviceChannels {
            device_id,
            channels,
        } => LegacyTrackInputRouting::AudioDeviceChannels {
            device_id: device_id.clone(),
            channels: channels.clone(),
        },
        ProjectTrackInputRouting::MidiDevice { device_id } => LegacyTrackInputRouting::MidiDevice {
            device_id: device_id.clone(),
        },
    };

    // The Case A test must compare against the default for *this* track type.
    let midi_input_default =
        crate::components::timeline::timeline_state::TrackRoutingState::for_track_type(track_type)
            .midi_input;

    let mut result = migrate_track_routing(
        &[LegacyTrackRouting {
            track_id: track_id.to_string(),
            track_name: track_name.to_string(),
            legacy_input,
            midi_input: existing_midi_input.clone(),
            midi_input_default,
        }],
        ports,
        PROJECT_VERSION,
    );
    warnings.append(&mut result.warnings);

    // Reuse an equivalent connection already in the registry (several tracks
    // sharing one legacy source must share one bus) before adding a new one.
    let migrated = result.tracks.pop();
    let generated = result.generated_connections.pop();
    let connection_id = match (
        migrated
            .as_ref()
            .and_then(|t| t.audio_input_connection_id.clone()),
        generated,
    ) {
        (Some(_), Some(connection)) => {
            let channels: Vec<u32> = (0..connection.channel_layout.channel_count())
                .filter_map(|logical| {
                    connection
                        .binding(logical)
                        .map(|binding| binding.physical_port_id.port_index)
                })
                .collect();
            let device_id = connection.device_id.clone().unwrap_or_default();
            registry.get_or_create_audio_connection_for_physical_input(
                &crate::audio_connections::PhysicalInputChoice::Ports {
                    device_id,
                    channels,
                },
                ports,
            )
        }
        // The source resolved to an existing connection during this load.
        (Some(id), None) => Some(id),
        _ => None,
    };

    let midi_input = migrated
        .map(|t| t.midi_input)
        .unwrap_or(existing_midi_input);
    (connection_id, midi_input)
}

fn timeline_output_to_project(
    output: &crate::components::timeline::timeline_state::TrackOutputRouting,
) -> ProjectTrackOutputRouting {
    use crate::components::timeline::timeline_state::TrackOutputRouting as T;
    match output {
        T::Main => ProjectTrackOutputRouting::Main,
        T::Bus { bus_id } => ProjectTrackOutputRouting::Bus {
            bus_id: bus_id.clone(),
        },
        T::HardwareOutput { device_id, channel } => ProjectTrackOutputRouting::HardwareOutput {
            device_id: device_id.clone(),
            channel: *channel,
        },
        T::Instrument { track_id } => ProjectTrackOutputRouting::Instrument {
            track_id: track_id.clone(),
        },
        T::None => ProjectTrackOutputRouting::None,
    }
}

fn timeline_audio_format_to_project(
    audio_format: crate::components::timeline::timeline_state::TrackAudioFormat,
) -> ProjectTrackAudioFormat {
    match audio_format {
        crate::components::timeline::timeline_state::TrackAudioFormat::Mono => {
            ProjectTrackAudioFormat::Mono
        }
        crate::components::timeline::timeline_state::TrackAudioFormat::Stereo => {
            ProjectTrackAudioFormat::Stereo
        }
    }
}

fn timeline_midi_input_to_project(
    input: &crate::components::timeline::timeline_state::TrackMidiInputRouting,
) -> ProjectTrackMidiInputRouting {
    use crate::components::timeline::timeline_state::TrackMidiInputRouting as T;
    match input {
        T::None => ProjectTrackMidiInputRouting::None,
        T::AllInputs => ProjectTrackMidiInputRouting::AllInputs,
        T::MidiDevice { device_id } => ProjectTrackMidiInputRouting::MidiDevice {
            device_id: device_id.clone(),
        },
    }
}

fn project_routing_to_timeline(
    routing: &TrackRouting,
    track_type: TlTrackType,
) -> crate::components::timeline::timeline_state::TrackRoutingState {
    use crate::components::timeline::timeline_state::{
        TrackAudioFormat, TrackMidiInputRouting, TrackOutputRouting, TrackRoutingState,
    };
    let mut state = TrackRoutingState::for_track_type(track_type);
    // v33 stores one combined field. The caller runs the migration adapter and
    // assigns `audio_input_connection_id` afterwards, because that needs the
    // project registry which this pure conversion does not own.
    state.output = match &routing.output {
        ProjectTrackOutputRouting::Main => TrackOutputRouting::Main,
        ProjectTrackOutputRouting::Bus { bus_id } => TrackOutputRouting::Bus {
            bus_id: bus_id.clone(),
        },
        ProjectTrackOutputRouting::HardwareOutput { device_id, channel } => {
            TrackOutputRouting::HardwareOutput {
                device_id: device_id.clone(),
                channel: *channel,
            }
        }
        ProjectTrackOutputRouting::Instrument { track_id } => TrackOutputRouting::Instrument {
            track_id: track_id.clone(),
        },
        ProjectTrackOutputRouting::None => TrackOutputRouting::None,
    };
    state.audio_format = match routing.audio_format {
        ProjectTrackAudioFormat::Mono => TrackAudioFormat::Mono,
        ProjectTrackAudioFormat::Stereo => TrackAudioFormat::Stereo,
    };
    state.midi_input = match &routing.midi_input {
        ProjectTrackMidiInputRouting::None => TrackMidiInputRouting::None,
        ProjectTrackMidiInputRouting::AllInputs => TrackMidiInputRouting::AllInputs,
        ProjectTrackMidiInputRouting::MidiDevice { device_id } => {
            TrackMidiInputRouting::MidiDevice {
                device_id: device_id.clone(),
            }
        }
    };
    state.midi_channel = routing.midi_channel.map(|ch| ch.clamp(1, 16));
    state.midi_output_per_note = routing.midi_output_per_note;
    state
}

/// Flatten an [`AutomationTarget`] into its persisted descriptor.
fn target_to_desc(
    target: &crate::components::timeline::timeline_state::AutomationTarget,
) -> AutomationTargetDesc {
    use crate::components::timeline::timeline_state::AutomationTarget as T;
    let mut desc = AutomationTargetDesc {
        tag: target.to_tag(),
        ..Default::default()
    };
    match target {
        T::PluginParameter {
            insert_id,
            parameter_id,
            parameter_name,
        } => {
            desc.insert_id = insert_id.clone();
            desc.parameter_id = parameter_id.clone();
            desc.parameter_name = parameter_name.clone();
        }
        T::SendLevel { send_id } => desc.send_id = send_id.clone(),
        _ => {}
    }
    desc
}

/// Rebuild an [`AutomationTarget`] from a persisted descriptor. Falls back to
/// deriving from `parameter_name` when the descriptor is from an older file
/// (tag 0 with no plugin/send descriptor strings).
fn desc_to_target(
    desc: &AutomationTargetDesc,
    parameter_name: &str,
) -> crate::components::timeline::timeline_state::AutomationTarget {
    use crate::components::timeline::timeline_state::AutomationTarget as T;
    match desc.tag {
        1 => T::TrackPan,
        2 => T::TrackMute,
        3 => T::PluginParameter {
            insert_id: desc.insert_id.clone(),
            parameter_id: desc.parameter_id.clone(),
            parameter_name: if desc.parameter_name.is_empty() {
                parameter_name.to_string()
            } else {
                desc.parameter_name.clone()
            },
        },
        4 => T::SendLevel {
            send_id: desc.send_id.clone(),
        },
        // tag 0: TrackVolume, or a legacy file — derive from the lane name.
        _ => {
            if desc.insert_id.is_empty() && desc.send_id.is_empty() {
                T::from_legacy_name(parameter_name)
            } else {
                T::TrackVolume
            }
        }
    }
}

#[cfg(test)]
mod v33_routing_adapter_tests {
    use super::*;
    use crate::audio_connections::{
        AudioConnectionRegistry, AudioConnectionStatus, AvailablePorts, ChannelLayout,
    };
    use crate::components::timeline::timeline_state::TrackMidiInputRouting;

    fn ports() -> AvailablePorts {
        AvailablePorts::for_device("input-device", "Interface", 4, 2)
    }

    #[allow(clippy::type_complexity)]
    fn load(
        legacy: ProjectTrackInputRouting,
        midi: TrackMidiInputRouting,
        track_type: TlTrackType,
        registry: &mut AudioConnectionRegistry,
    ) -> (
        Option<crate::audio_connections::AudioConnectionId>,
        TrackMidiInputRouting,
        Vec<crate::project::routing_migration::RoutingMigrationWarning>,
    ) {
        let mut warnings = Vec::new();
        let (id, midi_input) = legacy_routing_to_runtime(
            &legacy,
            midi,
            track_type,
            "track-1",
            "Track 1",
            registry,
            &ports(),
            &mut warnings,
        );
        (id, midi_input, warnings)
    }

    // ── v33 load ────────────────────────────────────────────────────────────

    #[test]
    fn v33_mono_audio_route_becomes_a_mono_connection() {
        let mut registry = AudioConnectionRegistry::new();
        let (id, _, warnings) = load(
            ProjectTrackInputRouting::AudioDeviceChannel {
                device_id: "input-device".to_string(),
                channel: 2,
            },
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        let id = id.expect("audio route migrates");
        let connection = registry.get(&id).unwrap();
        assert_eq!(connection.channel_layout, ChannelLayout::Mono);
        assert_eq!(
            connection.binding(0).unwrap().physical_port_id.port_index,
            2
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn v33_stereo_route_preserves_left_right_ordering() {
        let mut registry = AudioConnectionRegistry::new();
        let (id, _, _) = load(
            ProjectTrackInputRouting::AudioDeviceChannels {
                device_id: "input-device".to_string(),
                channels: vec![2, 3],
            },
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        let connection = registry.get(&id.unwrap()).unwrap();
        assert_eq!(connection.channel_layout, ChannelLayout::Stereo);
        assert_eq!(
            connection.binding(0).unwrap().physical_port_id.port_index,
            2
        );
        assert_eq!(
            connection.binding(1).unwrap().physical_port_id.port_index,
            3
        );
    }

    #[test]
    fn two_tracks_with_the_same_legacy_source_share_one_connection() {
        let mut registry = AudioConnectionRegistry::new();
        let source = ProjectTrackInputRouting::AudioDeviceChannel {
            device_id: "input-device".to_string(),
            channel: 1,
        };
        let (a, _, _) = load(
            source.clone(),
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        let (b, _, _) = load(
            source,
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        assert_eq!(a, b);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_missing_legacy_device_survives_as_device_missing() {
        let mut registry = AudioConnectionRegistry::new();
        let (id, _, _) = load(
            ProjectTrackInputRouting::AudioDeviceChannel {
                device_id: "unplugged".to_string(),
                channel: 0,
            },
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        let id = id.expect("assignment preserved");
        registry.revalidate(&ports());
        assert_eq!(
            registry.get(&id).unwrap().status,
            AudioConnectionStatus::DeviceMissing
        );
    }

    #[test]
    fn v33_none_leaves_the_track_unassigned_and_midi_untouched() {
        let mut registry = AudioConnectionRegistry::new();
        let dedicated = TrackMidiInputRouting::MidiDevice {
            device_id: "Keystation".to_string(),
        };
        let (id, midi, warnings) = load(
            ProjectTrackInputRouting::None,
            dedicated.clone(),
            TlTrackType::Audio,
            &mut registry,
        );
        assert!(id.is_none());
        assert_eq!(midi, dedicated);
        assert!(warnings.is_empty());
    }

    /// Case C: two explicit MIDI assignments — the dedicated field wins and a
    /// structured warning is emitted.
    #[test]
    fn conflicting_midi_assignments_retain_the_dedicated_field_and_warn() {
        let mut registry = AudioConnectionRegistry::new();
        let (_, midi, warnings) = load(
            ProjectTrackInputRouting::MidiDevice {
                device_id: "MPK Mini".to_string(),
            },
            TrackMidiInputRouting::MidiDevice {
                device_id: "Keystation".to_string(),
            },
            TlTrackType::Audio,
            &mut registry,
        );
        assert_eq!(
            midi,
            TrackMidiInputRouting::MidiDevice {
                device_id: "Keystation".to_string()
            }
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message().contains("dedicated MIDI input"));
    }

    /// Case A against the per-track-type default: an untouched MIDI track sits
    /// at AllInputs, so the legacy device is taken with no warning.
    #[test]
    fn an_untouched_midi_track_takes_the_legacy_device() {
        let mut registry = AudioConnectionRegistry::new();
        let (_, midi, warnings) = load(
            ProjectTrackInputRouting::MidiDevice {
                device_id: "MPK Mini".to_string(),
            },
            TrackMidiInputRouting::AllInputs,
            TlTrackType::Midi,
            &mut registry,
        );
        assert_eq!(
            midi,
            TrackMidiInputRouting::MidiDevice {
                device_id: "MPK Mini".to_string()
            }
        );
        assert!(warnings.is_empty());
    }

    // ── v33 save ────────────────────────────────────────────────────────────

    #[test]
    fn v33_save_round_trips_an_audio_connection() {
        let mut registry = AudioConnectionRegistry::new();
        let legacy = ProjectTrackInputRouting::AudioDeviceChannels {
            device_id: "input-device".to_string(),
            channels: vec![0, 1],
        };
        let (id, _, _) = load(
            legacy.clone(),
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        registry.revalidate(&ports());

        let encoded = runtime_to_v33_track_input_routing(
            id.as_ref(),
            &TrackMidiInputRouting::None,
            &registry,
            TlTrackType::Audio,
        );
        assert_eq!(encoded.routing, legacy, "the v33 shape round-trips");
        assert!(encoded.dropped.is_none());
    }

    /// The lossy boundary: v33 has one field, the runtime has two. The
    /// track-type-primary route is written and the loss is reported — nothing
    /// is cleared from the runtime.
    #[test]
    fn v33_save_reports_what_it_cannot_represent_on_an_audio_track() {
        let mut registry = AudioConnectionRegistry::new();
        let (id, _, _) = load(
            ProjectTrackInputRouting::AudioDeviceChannel {
                device_id: "input-device".to_string(),
                channel: 0,
            },
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        registry.revalidate(&ports());

        let encoded = runtime_to_v33_track_input_routing(
            id.as_ref(),
            &TrackMidiInputRouting::MidiDevice {
                device_id: "MPK Mini".to_string(),
            },
            &registry,
            TlTrackType::Audio,
        );
        assert!(
            matches!(
                encoded.routing,
                ProjectTrackInputRouting::AudioDeviceChannel { .. }
            ),
            "audio is primary for an audio track"
        );
        let dropped = encoded.dropped.expect("the MIDI side must be reported");
        assert!(dropped.contains("MIDI"));
    }

    #[test]
    fn v33_save_keeps_midi_primary_on_a_midi_track() {
        let mut registry = AudioConnectionRegistry::new();
        let (id, _, _) = load(
            ProjectTrackInputRouting::AudioDeviceChannel {
                device_id: "input-device".to_string(),
                channel: 0,
            },
            TrackMidiInputRouting::None,
            TlTrackType::Audio,
            &mut registry,
        );
        registry.revalidate(&ports());

        let encoded = runtime_to_v33_track_input_routing(
            id.as_ref(),
            &TrackMidiInputRouting::MidiDevice {
                device_id: "MPK Mini".to_string(),
            },
            &registry,
            TlTrackType::Midi,
        );
        assert_eq!(
            encoded.routing,
            ProjectTrackInputRouting::MidiDevice {
                device_id: "MPK Mini".to_string()
            }
        );
        assert!(encoded
            .dropped
            .expect("the audio side must be reported")
            .contains("audio"));
    }

    #[test]
    fn v33_save_of_an_unassigned_track_is_none_without_a_warning() {
        let registry = AudioConnectionRegistry::new();
        let encoded = runtime_to_v33_track_input_routing(
            None,
            &TrackMidiInputRouting::None,
            &registry,
            TlTrackType::Audio,
        );
        assert_eq!(encoded.routing, ProjectTrackInputRouting::None);
        assert!(encoded.dropped.is_none());
    }
}

#[cfg(test)]
mod group_track_persistence_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{
        CreateTrackOptions, InputMonitorMode, TimelineState, TrackType,
    };

    fn add_track(state: &mut TimelineState, track_type: TrackType, name: &str) -> String {
        state.create_track(CreateTrackOptions {
            track_type,
            name: name.to_string(),
            color: crate::theme::Colors::accent_primary(),
            volume: crate::components::timeline::timeline_state::volume::db_to_norm(0.0),
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        })
    }

    #[test]
    fn group_membership_survives_binary_roundtrip() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let group_id = add_track(&mut state, TrackType::Group, "Drums");
        let child_id = add_track(&mut state, TrackType::Audio, "Kick");
        assert!(state.assign_track_to_group(&child_id, &group_id));
        assert_eq!(state.toggle_group_collapsed(&group_id), Some(true));

        let bytes = encode_project(&FutureboardProject::from(&state));
        let decoded = decode_project(&bytes).expect("decode");
        let mut restored = TimelineState::default();
        apply_to_timeline(&decoded, &mut restored);

        assert_eq!(
            restored
                .find_track(&child_id)
                .unwrap()
                .parent_group_id
                .as_deref(),
            Some(group_id.as_str())
        );
        assert_eq!(
            restored.find_track(&group_id).unwrap().track_type,
            TrackType::Group
        );
        assert!(restored.find_track(&group_id).unwrap().group_collapsed);
        assert!(restored.remove_track_from_group(&child_id));
        assert!(restored
            .find_track(&child_id)
            .unwrap()
            .parent_group_id
            .is_none());
    }
}

#[cfg(test)]
mod inspector_property_persistence_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{StretchMode, TimelineState};

    #[test]
    fn pan_and_audio_inspector_properties_survive_project_roundtrip() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_audio_track();
        state.set_track_pan(&track_id, -0.42);
        assert!(state.set_track_volume_automation_read(&track_id, false));
        let clip_id = state.insert_audio_clip_with_duration(
            track_id.clone(),
            "C:/Audio/source.wav".to_string(),
            "Source".to_string(),
            2.5,
            8.0,
            Some(4.0),
        );
        assert!(state.set_clip_gain(&clip_id, 0.63));
        assert!(state.set_clip_muted(&clip_id, true));
        let mut stretch = state.clip_stretch(&clip_id).cloned().expect("stretch");
        stretch.mode = StretchMode::Manual;
        stretch.pitch_shift_semitones = 3.25;
        stretch.transient_sensitivity = 0.7;
        stretch.fade_in_ms = 125.0;
        stretch.fade_out_ms = 250.0;
        stretch.gain_db = -1.5;
        stretch.pan = 0.2;
        assert!(state.set_clip_stretch(&clip_id, stretch.clone()));

        let bytes = encode_project(&FutureboardProject::from(&state));
        let decoded = decode_project(&bytes).expect("decode");
        let mut restored = TimelineState::default();
        apply_to_timeline(&decoded, &mut restored);

        let track = restored.find_track(&track_id).expect("track");
        assert!((track.pan - -0.42).abs() < 1.0e-6);
        assert!(!track.volume_automation_read);
        let (_, clip) = restored.find_clip(&clip_id).expect("clip");
        assert!((clip.start_beat - 2.5).abs() < 1.0e-6);
        assert!((clip.duration_beats - 8.0).abs() < 1.0e-6);
        assert!((clip.gain - 0.63).abs() < 1.0e-6);
        assert!(clip.muted);
        assert_eq!(clip.stretch, stretch);
    }
}

#[cfg(test)]
mod articulation_persistence_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{ArticulationId, TimelineState};

    /// Per-note articulations and the clip's direction articulation lane must
    /// survive save → binary encode/decode → load. Event ids are transient and
    /// re-minted on load; beats and articulation identities are what persist.
    #[test]
    fn midi_articulations_survive_save_and_reload() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_midi_track();
        let clip_id = state.create_midi_clip(&track_id, 0.0, 8.0).expect("clip");
        let plain = state
            .add_midi_note(&clip_id, 60, 0.0, 1.0, 100)
            .expect("note");
        let accented = state
            .add_midi_note(&clip_id, 64, 1.0, 1.0, 100)
            .expect("note");
        state.set_midi_notes_articulation(&clip_id, &[accented], Some(ArticulationId::Accent));
        state.add_midi_articulation(&clip_id, 0.0, ArticulationId::Sustain);
        state.add_midi_articulation(&clip_id, 4.0, ArticulationId::Staccato);
        let _ = plain;

        let project = FutureboardProject::from(&state);
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");
        let mut restored = TimelineState::default();
        apply_to_timeline(&decoded, &mut restored);

        let notes = restored.midi_clip_notes(&clip_id).expect("notes restored");
        assert_eq!(notes.len(), 2);
        let by_pitch = |p: u8| notes.iter().find(|n| n.pitch == p).expect("pitch");
        assert_eq!(by_pitch(60).articulation, None);
        assert_eq!(by_pitch(64).articulation, Some(ArticulationId::Accent));
        // Restored notes keep raw duration/velocity (playback-only modifiers).
        assert_eq!(by_pitch(64).duration, 1.0);
        assert_eq!(by_pitch(64).velocity, 100);

        let events = restored
            .midi_clip_articulations(&clip_id)
            .expect("articulation lane restored");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].beat, 0.0);
        assert_eq!(events[0].articulation, ArticulationId::Sustain);
        assert_eq!(events[1].beat, 4.0);
        assert_eq!(events[1].articulation, ArticulationId::Staccato);
    }
}

#[cfg(test)]
mod vsti_substrip_persistence_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{
        vsti_output_child_track_id, CreateTrackOptions, InsertPluginFormat, TimelineState,
        TrackType,
    };

    /// Substrip (VSTi multi-out child strip) mixer state and FX insert chains —
    /// including opaque plugin state bytes — must survive save -> binary
    /// encode/decode -> load. Child strips have deterministic ids, so
    /// `ensure_vsti_output_child_tracks` retains (never duplicates) the loaded
    /// rows once the plugin reports its layout.
    #[test]
    fn substrip_insert_chain_and_mixer_state_roundtrip() {
        let mut state = TimelineState::default();
        let track_id = state.create_track(CreateTrackOptions {
            track_type: TrackType::Instrument,
            name: "Drums".into(),
            color: crate::color::auto_color_for_index(0),
            volume: 0.8,
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        let slot = state.ensure_insert_slot_at(&track_id, 0).expect("slot");
        state.set_insert_plugin(
            &track_id,
            &slot,
            "drums".to_string(),
            Some(PathBuf::from("C:/p/drums.vst3")),
            InsertPluginFormat::Vst3,
            None,
            "Drums".to_string(),
        );
        state.set_insert_output_bus_layout(&track_id, &slot, &[2, 2]);
        state.auto_enable_detected_insert_outputs(&track_id, &slot, 4);

        let child_id = vsti_output_child_track_id(&slot, 1);
        assert!(
            state.tracks.iter().any(|t| t.id == child_id),
            "multi-out layout should create the bus-1 child strip"
        );

        // FX insert on the substrip, with plugin state bytes and bypass set.
        let fx_slot = state
            .add_insert(&child_id)
            .expect("substrip accepts inserts");
        state.set_insert_plugin(
            &child_id,
            &fx_slot,
            "comp".to_string(),
            Some(PathBuf::from("C:/p/comp.vst3")),
            InsertPluginFormat::Vst3,
            None,
            "Comp".to_string(),
        );
        {
            let slots = state.insert_slots_mut(&child_id).expect("child slots");
            let fx = slots.iter_mut().find(|s| s.id == fx_slot).expect("fx slot");
            fx.vst3_state = Some(std::sync::Arc::new(vec![1, 2, 3, 4]));
            fx.bypassed = true;
        }
        // Per-bus mixer state.
        state.toggle_track_mute(&child_id);
        state.set_track_pan(&child_id, -0.25);

        let project = FutureboardProject::from(&state);
        assert!(
            project.tracks.iter().any(|t| t.id == child_id),
            "child strip must be persisted"
        );
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");

        let mut restored = TimelineState::default();
        apply_to_timeline(&decoded, &mut restored);

        let child = restored
            .tracks
            .iter()
            .find(|t| t.id == child_id)
            .expect("substrip restored");
        assert!(child.muted, "substrip mute state restored");
        assert!((child.pan + 0.25).abs() < 1e-6, "substrip pan restored");
        let fx = child
            .inserts
            .iter()
            .find(|s| s.id == fx_slot)
            .expect("substrip insert restored");
        assert_eq!(fx.plugin_id.as_deref(), Some("comp"));
        assert!(fx.bypassed, "substrip insert bypass restored");
        assert_eq!(
            fx.vst3_state.as_ref().map(|s| s.as_ref().clone()),
            Some(vec![1, 2, 3, 4]),
            "substrip insert plugin state bytes restored"
        );
    }

    /// A built-in plugin (no VST3 runtime, `InsertPluginFormat::Unknown`)
    /// persists its DSP state through the same `vst3_state` byte channel as
    /// any other insert — the field is opaque-bytes-keyed-by-plugin_id, not
    /// format-gated (see `InsertSlotState::vst3_state`'s doc comment). This
    /// is what `collect_builtin_instances` (`plugin_ops.rs`) reads back to
    /// populate a shared editor's `selectInstance.state`.
    #[test]
    fn builtin_plugin_state_bytes_roundtrip_through_save_and_load() {
        let mut state = TimelineState::default();
        let track_id = state.create_track(CreateTrackOptions {
            track_type: TrackType::Audio,
            name: "Guitar".into(),
            color: crate::color::auto_color_for_index(0),
            volume: 0.8,
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        let slot = state.ensure_insert_slot_at(&track_id, 0).expect("slot");
        state.set_insert_plugin(
            &track_id,
            &slot,
            "rodharerist".to_string(),
            None,
            InsertPluginFormat::Unknown,
            None,
            "Rodhareist".to_string(),
        );
        let json_state = br#"{"schema_version":1,"params":{"amp_gain":7.5}}"#.to_vec();
        {
            let slots = state.insert_slots_mut(&track_id).expect("slots");
            let fx = slots.iter_mut().find(|s| s.id == slot).expect("fx slot");
            fx.vst3_state = Some(std::sync::Arc::new(json_state.clone()));
        }

        let project = FutureboardProject::from(&state);
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");

        let mut restored = TimelineState::default();
        apply_to_timeline(&decoded, &mut restored);

        let track = restored
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .expect("track restored");
        let fx = track
            .inserts
            .iter()
            .find(|s| s.id == slot)
            .expect("builtin insert restored");
        assert_eq!(fx.plugin_id.as_deref(), Some("rodharerist"));
        assert_eq!(
            fx.vst3_state.as_ref().map(|s| s.as_ref().clone()),
            Some(json_state),
            "built-in plugin's JSON state bytes must survive save/load"
        );
    }

    /// An Audio Unit is addressed by component id, so `plugin_path` holds a
    /// string that will never exist on disk. Project load must not read that as
    /// a broken plugin: the slot has to come back loadable, bridge-hosted, and
    /// still carrying its opaque ClassInfo bytes.
    #[test]
    fn audio_unit_insert_loads_without_a_module_file_on_disk() {
        use crate::components::timeline::timeline_state::{InsertLoadStatus, PluginRuntimeBackend};

        let mut state = TimelineState::default();
        let track_id = state.create_track(CreateTrackOptions {
            track_type: TrackType::Audio,
            name: "Vocal".into(),
            color: crate::color::auto_color_for_index(0),
            volume: 1.0,
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        let slot = state.ensure_insert_slot_at(&track_id, 0).expect("slot");
        let component = "au:61756678:64656c79:6170706c";
        state.set_insert_plugin(
            &track_id,
            &slot,
            component.to_string(),
            Some(PathBuf::from(component)),
            InsertPluginFormat::Au,
            None,
            "AUDelay".to_string(),
        );
        let class_info = vec![7u8, 8, 9];
        {
            let slots = state.insert_slots_mut(&track_id).expect("slots");
            let fx = slots.iter_mut().find(|s| s.id == slot).expect("fx slot");
            fx.vst3_state = Some(std::sync::Arc::new(class_info.clone()));
        }

        let bytes = encode_project(&FutureboardProject::from(&state));
        let decoded = decode_project(&bytes).expect("decode");
        let mut restored = TimelineState::default();
        apply_to_timeline(&decoded, &mut restored);

        let fx = restored
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .and_then(|t| t.inserts.iter().find(|s| s.id == slot))
            .expect("audio unit insert restored");
        assert_eq!(fx.plugin_format, Some(InsertPluginFormat::Au));
        assert_eq!(
            fx.load_status,
            InsertLoadStatus::Loading,
            "a component id that is not a file must not read as a missing plugin"
        );
        assert_eq!(fx.runtime_backend, PluginRuntimeBackend::ExternalBridge);
        assert!(fx.is_bridge_hosted_external_module());
        assert_eq!(
            fx.vst3_state.as_ref().map(|s| s.as_ref().clone()),
            Some(class_info),
            "the AU's opaque ClassInfo bytes must survive save/load"
        );
    }
}
