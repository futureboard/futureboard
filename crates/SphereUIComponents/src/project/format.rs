use super::{
    AutomationLane, AutomationPoint, AutomationTargetDesc, ClipSource, FutureboardProject,
    InputMonitorMode, MidiArticulation, MidiControllerKind, MidiControllerLane,
    MidiControllerPoint, MidiNote, MidiSysExEvent, MidiSysExKind, PluginFormat, PluginStateBlob,
    ProjectAsset, ProjectClip, ProjectInsert, ProjectLyricSyllable, ProjectLyricSyllableMode,
    ProjectMixer, ProjectPluginInstance, ProjectSend, ProjectSongSectionType, ProjectSongTextEvent,
    ProjectSongTextEventKind, ProjectSoundfontPlayer, ProjectTempoPoint, ProjectTimelineMarker,
    ProjectTimelineRegion, ProjectTrack, ProjectTrackAudioFormat, ProjectTrackInputRouting,
    ProjectTrackMidiInputRouting, ProjectTrackOutputRouting, ProjectTrackType, TrackRouting,
};
use crate::components::timeline::timeline_state::{
    AudioClipStretchState, StretchAlgorithm, StretchMode, WarpMarker,
};
use std::io::{self, Cursor, Read};
use std::path::PathBuf;

pub const PROJECT_MAGIC: &[u8; 8] = b"FBSTUD1\0";
/// On-disk format version. v6 adds multi-channel audio input routing.
/// v5 adds MIDI controller (CC) lanes per MIDI clip.
/// v4 adds a per-MIDI-note muted flag. v3 adds persisted track routing fields.
/// Older files still load: v1/v2 use stable per-track routing defaults,
/// v1/v2/v3 notes default to unmuted, and pre-v5 MIDI clips have no CC lanes.
/// v7 adds project-level tempo automation markers (TempoMap); pre-v7 files have
/// no tempo points and play at the static `bpm`.
/// v8 adds stable ids on tempo points for independent marker editing.
/// v11 adds a content fingerprint per project asset for cross-session import
/// dedup. Pre-v11 files have no asset fingerprint and load with `None`.
/// v13 adds timeline markers and regions. v14 adds internal RAUF clip sources.
/// v15 adds persisted master-bus inserts.
/// v16 adds a per-clip non-destructive stretch/pitch block (mode, algorithm,
/// ratio, BPM pair, pitch/formant/transient/fade/gain/pan, warp markers). Pre-v16
/// clips load with [`AudioClipStretchState::default`] (mode Off, ratio 1.0,
/// preserve_pitch false).
/// v18 persists enabled VSTi output channels per insert.
/// v19 persists the per-instrument VSTi multi-out mixer collapse flag.
/// v20 persists mixer tree expanded/pinned/hidden channel state.
/// v21 persists per-automation-point curve tension. Pre-v21 points load with
/// tension 0.0 (a straight segment), so older projects are unchanged.
/// v22 adds a per-note MIDI channel (pre-v22 notes default to channel 1) and
/// a per-track "play each note on its own channel" toggle (pre-v22 tracks
/// default to `false`, matching the pre-existing fixed-channel behavior).
/// v23 preserves imported MIDI SysEx events on MIDI clips.
/// v24 adds project-owned chord and lyric cues.
/// v25 adds MIDI articulations: a per-note articulation tag (0 = none) and a
/// per-clip direction articulation event list. Pre-v25 data loads with no
/// articulations; unknown tags from newer files degrade to "none" on load.
/// v26 persists stable MIDI note and CC-point identities plus optional note
/// release velocity. Pre-v26 notes/points mint fresh ids on load; release
/// velocity defaults to unset (`0`).
/// v27 replaces combined chord/lyric cues with typed Song Text events and adds
/// lyric timing/syllable data plus section metadata. v24-v26 cues migrate on load;
/// pre-v24 projects have no Song Text events.
/// v28 persists the built-in Soundfont Player instrument per track (`.sf2`
/// path, preset bank/patch, volume, reverb/chorus, polyphony). Pre-v28 tracks
/// load with no soundfont, which is what they had before the field existed.
pub const PROJECT_VERSION: u32 = 28;

/// Minimum on-disk header size: magic (8) + version (4) + reserved (4) + body_len (4).
pub const PROJECT_HEADER_SIZE: usize = 20;

#[derive(Debug)]
pub enum ProjectError {
    Io(io::Error),
    InvalidMagic,
    UnsupportedVersion(u32),
    /// File is shorter than the header or declared payload.
    IncompleteFile {
        reason: String,
    },
    UnexpectedEof {
        needed: usize,
        remaining: usize,
        field: &'static str,
    },
    Corrupted(String),
    ChecksumMismatch {
        expected: u32,
        got: u32,
    },
}

impl ProjectError {
    /// Primary message shown in UI dialogs (no raw parser tokens).
    pub fn user_message(&self) -> &'static str {
        match self {
            ProjectError::Io(_) => {
                "Could not read the project file. Check that the file exists and is accessible."
            }
            ProjectError::InvalidMagic => "This file is not a Futureboard project.",
            ProjectError::UnsupportedVersion(version) if *version > PROJECT_VERSION => {
                "This project was created by a newer unsupported version of Futureboard."
            }
            ProjectError::UnsupportedVersion(_) => {
                "This project version is not supported by this build of Futureboard."
            }
            ProjectError::IncompleteFile { .. }
            | ProjectError::UnexpectedEof { .. }
            | ProjectError::ChecksumMismatch { .. } => {
                "Could not open this project because the file appears to be incomplete or corrupted."
            }
            ProjectError::Corrupted(msg) if is_truncation_detail(msg) => {
                "Could not open this project because the file appears to be incomplete or corrupted."
            }
            ProjectError::Corrupted(_) => {
                "Could not open this project because the file appears to be incomplete or corrupted."
            }
        }
    }

    /// Optional secondary line for dialogs and logs.
    pub fn technical_detail(&self) -> String {
        match self {
            ProjectError::Io(e) => format!("I/O error: {e}"),
            ProjectError::InvalidMagic => "invalid magic bytes".to_string(),
            ProjectError::UnsupportedVersion(v) => format!("unsupported version: {v}"),
            ProjectError::IncompleteFile { reason } => reason.clone(),
            ProjectError::UnexpectedEof {
                needed,
                remaining,
                field,
            } => format!("unexpected EOF reading {field} (needed {needed}, remaining {remaining})"),
            ProjectError::Corrupted(msg) => msg.clone(),
            ProjectError::ChecksumMismatch { expected, got } => {
                format!("checksum mismatch: expected {expected:#010x}, got {got:#010x}")
            }
        }
    }
}

fn is_truncation_detail(msg: &str) -> bool {
    msg.contains("truncated")
        || msg.contains("too small")
        || msg.contains("file truncated")
        || msg.contains("unexpected EOF")
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.technical_detail())
    }
}

impl From<io::Error> for ProjectError {
    fn from(e: io::Error) -> Self {
        ProjectError::Io(e)
    }
}

// ── Low-level writer ──────────────────────────────────────────────────────────

pub struct FbWriter {
    buf: Vec<u8>,
}

impl FbWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }

    fn write_str(&mut self, s: &str) {
        let bytes = s.as_bytes();
        self.write_u32(bytes.len() as u32);
        self.buf.extend_from_slice(bytes);
    }

    fn write_opt_str(&mut self, s: &Option<String>) {
        match s {
            None => self.write_u8(0),
            Some(v) => {
                self.write_u8(1);
                self.write_str(v);
            }
        }
    }

    fn write_opt_path(&mut self, p: &Option<PathBuf>) {
        match p {
            None => self.write_u8(0),
            Some(v) => {
                self.write_u8(1);
                self.write_str(&v.to_string_lossy());
            }
        }
    }

    fn write_opt_u32(&mut self, v: &Option<u32>) {
        match v {
            None => self.write_u8(0),
            Some(x) => {
                self.write_u8(1);
                self.write_u32(*x);
            }
        }
    }

    fn write_opt_u8(&mut self, v: &Option<u8>) {
        match v {
            None => self.write_u8(0),
            Some(x) => {
                self.write_u8(1);
                self.write_u8(*x);
            }
        }
    }

    fn write_opt_f64(&mut self, v: &Option<f64>) {
        match v {
            None => self.write_u8(0),
            Some(x) => {
                self.write_u8(1);
                self.write_f64(*x);
            }
        }
    }

    fn write_opt_f32(&mut self, v: &Option<f32>) {
        match v {
            None => self.write_u8(0),
            Some(x) => {
                self.write_u8(1);
                self.write_f32(*x);
            }
        }
    }

    fn write_opt_u64(&mut self, v: &Option<u64>) {
        match v {
            None => self.write_u8(0),
            Some(x) => {
                self.write_u8(1);
                self.write_u64(*x);
            }
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u32(bytes.len() as u32);
        self.buf.extend_from_slice(bytes);
    }
}

// ── Low-level reader ──────────────────────────────────────────────────────────

pub struct FbReader<'a> {
    cur: Cursor<&'a [u8]>,
}

impl<'a> FbReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            cur: Cursor::new(data),
        }
    }

    fn remaining(&self) -> usize {
        let pos = self.cur.position() as usize;
        let len = self.cur.get_ref().len();
        len.saturating_sub(pos)
    }

    fn read_exact_field(
        &mut self,
        buf: &mut [u8],
        field: &'static str,
    ) -> Result<(), ProjectError> {
        let needed = buf.len();
        let remaining = self.remaining();
        if remaining < needed {
            return Err(ProjectError::UnexpectedEof {
                needed,
                remaining,
                field,
            });
        }
        self.cur
            .read_exact(buf)
            .map_err(|_| ProjectError::UnexpectedEof {
                needed,
                remaining,
                field,
            })
    }

    fn read_u8(&mut self) -> Result<u8, ProjectError> {
        let mut b = [0u8; 1];
        self.read_exact_field(&mut b, "u8")?;
        Ok(b[0])
    }

    fn read_u32(&mut self) -> Result<u32, ProjectError> {
        let mut b = [0u8; 4];
        self.read_exact_field(&mut b, "u32")?;
        Ok(u32::from_le_bytes(b))
    }

    fn read_u64(&mut self) -> Result<u64, ProjectError> {
        let mut b = [0u8; 8];
        self.read_exact_field(&mut b, "u64")?;
        Ok(u64::from_le_bytes(b))
    }

    fn read_f32(&mut self) -> Result<f32, ProjectError> {
        let mut b = [0u8; 4];
        self.read_exact_field(&mut b, "f32")?;
        Ok(f32::from_le_bytes(b))
    }

    fn read_f64(&mut self) -> Result<f64, ProjectError> {
        let mut b = [0u8; 8];
        self.read_exact_field(&mut b, "f64")?;
        Ok(f64::from_le_bytes(b))
    }

    fn read_bool(&mut self) -> Result<bool, ProjectError> {
        Ok(self.read_u8()? != 0)
    }

    fn read_str(&mut self) -> Result<String, ProjectError> {
        let len = self.read_u32()? as usize;
        let mut buf = vec![0u8; len];
        self.read_exact_field(&mut buf, "string bytes")?;
        String::from_utf8(buf).map_err(|_| ProjectError::Corrupted("invalid UTF-8 string".into()))
    }

    fn read_opt_str(&mut self) -> Result<Option<String>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_str()?)),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_opt_path(&mut self) -> Result<Option<PathBuf>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(PathBuf::from(self.read_str()?))),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_opt_u32(&mut self) -> Result<Option<u32>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_u32()?)),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_opt_u8(&mut self) -> Result<Option<u8>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_u8()?)),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_opt_f64(&mut self) -> Result<Option<f64>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_f64()?)),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_opt_f32(&mut self) -> Result<Option<f32>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_f32()?)),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_opt_u64(&mut self) -> Result<Option<u64>, ProjectError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_u64()?)),
            t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
        }
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, ProjectError> {
        let len = self.read_u32()? as usize;
        let mut buf = vec![0u8; len];
        self.read_exact_field(&mut buf, "byte blob")?;
        Ok(buf)
    }
}

// ── Encoding ──────────────────────────────────────────────────────────────────

fn encode_plugin_format(w: &mut FbWriter, f: PluginFormat) {
    w.write_u8(match f {
        PluginFormat::Vst3 => 0,
        PluginFormat::Clap => 1,
        PluginFormat::Au => 2,
        PluginFormat::Lv2 => 3,
        PluginFormat::Unknown => 0xFF,
    });
}

fn encode_opt_plugin_format(w: &mut FbWriter, f: &Option<PluginFormat>) {
    match f {
        None => w.write_u8(0),
        Some(fmt) => {
            w.write_u8(1);
            encode_plugin_format(w, *fmt);
        }
    }
}

fn encode_plugin_state_blob(w: &mut FbWriter, blob: &PluginStateBlob) {
    w.write_str(&blob.plugin_id);
    encode_opt_plugin_format(w, &blob.format);
    w.write_bytes(&blob.state_bytes);
    w.write_opt_str(&blob.vendor);
    w.write_opt_str(&blob.name);
    w.write_opt_str(&blob.version);
}

fn encode_plugin_instance(w: &mut FbWriter, inst: &ProjectPluginInstance) {
    w.write_str(&inst.instance_id);
    encode_plugin_format(w, inst.format);
    w.write_opt_path(&inst.plugin_path);
    w.write_str(&inst.plugin_uid);
    w.write_str(&inst.display_name);
    encode_plugin_state_blob(w, &inst.state);
}

fn encode_insert(w: &mut FbWriter, ins: &ProjectInsert) {
    w.write_str(&ins.id);
    w.write_u32(ins.slot_index);
    w.write_bool(ins.bypassed);
    w.write_u32(ins.enabled_audio_output_channels.len() as u32);
    for channel in &ins.enabled_audio_output_channels {
        w.write_u8(*channel);
    }
    // v19: mixer-only multi-out collapse flag (visual; never affects routing).
    w.write_bool(ins.multiout_collapsed);
    match &ins.plugin {
        None => w.write_u8(0),
        Some(inst) => {
            w.write_u8(1);
            encode_plugin_instance(w, inst);
        }
    }
}

fn encode_automation_lane(w: &mut FbWriter, lane: &AutomationLane) {
    w.write_str(&lane.id);
    w.write_str(&lane.parameter_name);
    w.write_bool(lane.visible);
    // Target descriptor + enabled (v2).
    w.write_u8(lane.target.tag);
    w.write_str(&lane.target.insert_id);
    w.write_str(&lane.target.parameter_id);
    w.write_str(&lane.target.parameter_name);
    w.write_str(&lane.target.send_id);
    w.write_bool(lane.enabled);
    w.write_u32(lane.points.len() as u32);
    for p in &lane.points {
        w.write_f32(p.beat);
        w.write_f32(p.value);
        w.write_u8(p.curve); // v2
        w.write_f32(p.tension); // v21
    }
}

fn encode_midi_note(w: &mut FbWriter, n: &MidiNote) {
    w.write_u8(n.pitch);
    w.write_f32(n.start_beats);
    w.write_f32(n.duration_beats);
    w.write_u8(n.velocity);
    w.write_bool(n.muted); // v4
    w.write_u8(n.channel.clamp(1, 16)); // v22
    w.write_u8(n.articulation); // v25 (0 = none)
    w.write_u64(n.id); // v26 (0 = mint on load for legacy writers)
    w.write_u8(n.release_velocity); // v26 (0 = unset)
}

/// v5: controller kind tag. CC carries its number; the rest are tag-only.
fn encode_controller_kind(w: &mut FbWriter, kind: MidiControllerKind) {
    match kind {
        MidiControllerKind::CC(n) => {
            w.write_u8(0);
            w.write_u8(n);
        }
        MidiControllerKind::PitchBend => w.write_u8(1),
        MidiControllerKind::ChannelPressure => w.write_u8(2),
        MidiControllerKind::PolyPressure => w.write_u8(3),
    }
}

/// v5: a controller lane and its points.
fn encode_controller_lane(w: &mut FbWriter, lane: &MidiControllerLane) {
    encode_controller_kind(w, lane.kind);
    w.write_bool(lane.visible);
    w.write_f32(lane.height);
    w.write_bool(lane.collapsed);
    w.write_u32(lane.points.len() as u32);
    for p in &lane.points {
        w.write_f32(p.beat);
        w.write_f32(p.value);
        w.write_u64(p.id); // v26
    }
}

fn encode_sysex_event(w: &mut FbWriter, event: &MidiSysExEvent) {
    w.write_u8(match event.kind {
        MidiSysExKind::Normal => 0,
        MidiSysExKind::Escaped => 1,
    });
    w.write_u64(event.tick);
    w.write_f32(event.beat);
    w.write_bytes(&event.data);
}

/// v16: per-clip non-destructive stretch/pitch block. `dirty` is transient and
/// intentionally not persisted (decodes as `false`).
fn encode_stretch(w: &mut FbWriter, s: &AudioClipStretchState) {
    w.write_u8(s.mode.to_tag());
    w.write_u8(s.algorithm.to_tag());
    w.write_u32(s.original_sample_rate);
    w.write_u32(s.project_sample_rate);
    w.write_u64(s.original_duration_samples);
    w.write_u64(s.source_start_samples);
    w.write_u64(s.source_end_samples);
    w.write_f64(s.clip_timeline_start_beats);
    w.write_f64(s.clip_timeline_duration_beats);
    w.write_f64(s.stretch_ratio);
    w.write_opt_f64(&s.bpm_source);
    w.write_opt_f64(&s.bpm_target);
    w.write_bool(s.preserve_pitch);
    w.write_f32(s.pitch_shift_semitones);
    w.write_bool(s.formant_preserve);
    w.write_bool(s.transient_preserve);
    w.write_f32(s.transient_sensitivity);
    w.write_bool(s.reverse);
    w.write_bool(s.normalize_gain);
    w.write_f32(s.fade_in_ms);
    w.write_f32(s.fade_out_ms);
    w.write_f32(s.gain_db);
    w.write_f32(s.pan);
    w.write_u32(s.warp_markers.len() as u32);
    for m in &s.warp_markers {
        w.write_u64(m.id);
        w.write_u64(m.source_sample);
        w.write_f64(m.timeline_beat);
        w.write_bool(m.locked);
    }
}

fn encode_clip(w: &mut FbWriter, c: &ProjectClip) {
    w.write_str(&c.id);
    w.write_str(&c.name);
    w.write_f64(c.start_beat);
    w.write_f64(c.duration_beats);
    w.write_f32(c.offset_beats);
    w.write_f32(c.gain);
    w.write_bool(c.muted);
    match &c.source {
        ClipSource::Empty => w.write_u8(0),
        ClipSource::Audio {
            asset_id,
            source_path,
        } => {
            w.write_u8(1);
            w.write_str(asset_id);
            w.write_opt_path(source_path);
        }
        ClipSource::Rauf {
            asset_id,
            source_path,
            metadata_path,
            sample_format,
            sample_rate,
            channels,
            start_frame,
            length_frames,
        } => {
            w.write_u8(3);
            w.write_str(asset_id);
            w.write_str(&source_path.to_string_lossy());
            w.write_opt_path(metadata_path);
            w.write_str(sample_format);
            w.write_u32(*sample_rate);
            w.write_u32(*channels as u32);
            w.write_u64(*start_frame);
            w.write_u64(*length_frames);
        }
        ClipSource::Midi {
            notes,
            controller_lanes,
            sysex_events,
            articulations,
        } => {
            w.write_u8(2);
            w.write_u32(notes.len() as u32);
            for n in notes {
                encode_midi_note(w, n);
            }
            // v5: controller lanes follow the notes.
            w.write_u32(controller_lanes.len() as u32);
            for lane in controller_lanes {
                encode_controller_lane(w, lane);
            }
            // v23: SysEx events are preserved for future playback/export.
            w.write_u32(sysex_events.len() as u32);
            for event in sysex_events {
                encode_sysex_event(w, event);
            }
            // v25: direction articulation events follow the SysEx block.
            w.write_u32(articulations.len() as u32);
            for event in articulations {
                w.write_f32(event.beat);
                w.write_u8(event.articulation);
            }
        }
    }
    // v16: stretch/pitch block trails the source for every clip.
    encode_stretch(w, &c.stretch);
}

fn encode_input_monitor(w: &mut FbWriter, m: InputMonitorMode) {
    w.write_u8(match m {
        InputMonitorMode::Off => 0,
        InputMonitorMode::Always => 1,
        InputMonitorMode::WhenRecordArmed => 2,
    });
}

fn encode_track_type(w: &mut FbWriter, t: ProjectTrackType) {
    w.write_u8(match t {
        ProjectTrackType::Audio => 0,
        ProjectTrackType::Midi => 1,
        ProjectTrackType::Instrument => 2,
        ProjectTrackType::Bus => 3,
        ProjectTrackType::Return => 4,
        ProjectTrackType::Group => 5,
        ProjectTrackType::Master => 6,
    });
}

fn encode_track_input_routing(w: &mut FbWriter, input: &ProjectTrackInputRouting) {
    match input {
        ProjectTrackInputRouting::None => w.write_u8(0),
        ProjectTrackInputRouting::AllInputs => w.write_u8(1),
        ProjectTrackInputRouting::AudioDeviceChannel { device_id, channel } => {
            w.write_u8(2);
            w.write_str(device_id);
            w.write_u32(*channel);
        }
        ProjectTrackInputRouting::AudioDeviceChannels {
            device_id,
            channels,
        } => {
            w.write_u8(4);
            w.write_str(device_id);
            w.write_u32(channels.len() as u32);
            for channel in channels {
                w.write_u32(*channel);
            }
        }
        ProjectTrackInputRouting::MidiDevice { device_id } => {
            w.write_u8(3);
            w.write_str(device_id);
        }
    }
}

fn encode_track_output_routing(w: &mut FbWriter, output: &ProjectTrackOutputRouting) {
    match output {
        ProjectTrackOutputRouting::Main => w.write_u8(0),
        ProjectTrackOutputRouting::Bus { bus_id } => {
            w.write_u8(1);
            w.write_str(bus_id);
        }
        ProjectTrackOutputRouting::HardwareOutput { device_id, channel } => {
            w.write_u8(2);
            w.write_str(device_id);
            w.write_u32(*channel);
        }
        ProjectTrackOutputRouting::None => w.write_u8(3),
        ProjectTrackOutputRouting::Instrument { track_id } => {
            w.write_u8(4);
            w.write_str(track_id);
        }
    }
}

fn encode_track_audio_format(w: &mut FbWriter, audio_format: ProjectTrackAudioFormat) {
    w.write_u8(match audio_format {
        ProjectTrackAudioFormat::Mono => 0,
        ProjectTrackAudioFormat::Stereo => 1,
    });
}

fn encode_track_midi_input_routing(w: &mut FbWriter, input: &ProjectTrackMidiInputRouting) {
    match input {
        ProjectTrackMidiInputRouting::None => w.write_u8(0),
        ProjectTrackMidiInputRouting::AllInputs => w.write_u8(1),
        ProjectTrackMidiInputRouting::MidiDevice { device_id } => {
            w.write_u8(2);
            w.write_str(device_id);
        }
    }
}

fn routing_output_bus_id(output: &ProjectTrackOutputRouting) -> Option<String> {
    match output {
        ProjectTrackOutputRouting::Bus { bus_id } => Some(bus_id.clone()),
        _ => None,
    }
}

fn encode_track(w: &mut FbWriter, t: &ProjectTrack) {
    w.write_str(&t.id);
    w.write_str(&t.name);
    encode_track_type(w, t.track_type);
    w.write_str(&t.color_hex);
    w.write_f32(t.volume_norm);
    w.write_f32(t.pan);
    w.write_bool(t.muted);
    w.write_bool(t.solo);
    w.write_bool(t.record_arm);
    encode_input_monitor(w, t.input_monitor);
    // routing
    encode_track_input_routing(w, &t.routing.input);
    encode_track_output_routing(w, &t.routing.output);
    encode_track_audio_format(w, t.routing.audio_format);
    encode_track_midi_input_routing(w, &t.routing.midi_input);
    w.write_opt_u8(&t.routing.midi_channel.map(|ch| ch.clamp(1, 16)));
    w.write_bool(t.routing.midi_output_per_note); // v22
    let output_bus = routing_output_bus_id(&t.routing.output);
    w.write_opt_str(&output_bus);
    w.write_u32(t.routing.sends.len() as u32);
    for s in &t.routing.sends {
        w.write_str(&s.id);
        w.write_str(&s.target_track_id);
        w.write_bool(s.enabled);
        w.write_bool(s.pre_fader);
        w.write_f32(s.gain_db);
    }
    // inserts
    w.write_u32(t.inserts.len() as u32);
    for ins in &t.inserts {
        encode_insert(w, ins);
    }
    // automation lanes
    w.write_u32(t.automation_lanes.len() as u32);
    for lane in &t.automation_lanes {
        encode_automation_lane(w, lane);
    }
    // clips
    w.write_u32(t.clips.len() as u32);
    for c in &t.clips {
        encode_clip(w, c);
    }
    w.write_opt_f32(&t.row_height_px);
    encode_soundfont_player(w, t.soundfont.as_ref()); // v28
}

/// v28: built-in Soundfont Player instrument state. A leading flag keeps the
/// common case (no soundfont on this track) to a single byte.
fn encode_soundfont_player(w: &mut FbWriter, soundfont: Option<&ProjectSoundfontPlayer>) {
    let Some(soundfont) = soundfont else {
        w.write_bool(false);
        return;
    };
    w.write_bool(true);
    w.write_opt_path(&soundfont.path);
    // Bank and patch are only meaningful together, and both are non-negative
    // (MIDI bank select / program change), so one flag plus two u32s round-trips
    // the pair without a signed encoding.
    let preset = soundfont.preset_bank.zip(soundfont.preset_patch);
    w.write_bool(preset.is_some());
    let (bank, patch) = preset.unwrap_or((0, 0));
    w.write_u32(bank.max(0) as u32);
    w.write_u32(patch.max(0) as u32);
    w.write_f32(soundfont.volume);
    w.write_bool(soundfont.reverb_chorus);
    w.write_u32(soundfont.polyphony);
}

fn decode_soundfont_player(
    r: &mut FbReader,
) -> Result<Option<ProjectSoundfontPlayer>, ProjectError> {
    if !r.read_bool()? {
        return Ok(None);
    }
    let path = r.read_opt_path()?;
    let has_preset = r.read_bool()?;
    let bank = r.read_u32()? as i32;
    let patch = r.read_u32()? as i32;
    Ok(Some(ProjectSoundfontPlayer {
        path,
        preset_bank: has_preset.then_some(bank),
        preset_patch: has_preset.then_some(patch),
        volume: r.read_f32()?.clamp(0.0, 1.0),
        reverb_chorus: r.read_bool()?,
        polyphony: r.read_u32()?.clamp(1, 256),
    }))
}

fn encode_asset(w: &mut FbWriter, a: &ProjectAsset) {
    w.write_str(&a.id);
    w.write_str(&a.original_filename);
    w.write_opt_str(&a.relative_path);
    w.write_opt_path(&a.absolute_path);
    w.write_opt_f64(&a.duration_secs);
    w.write_opt_u32(&a.sample_rate);
    w.write_opt_u8(&a.channels);
    w.write_opt_str(&a.source_fingerprint); // v11
    w.write_opt_str(&a.waveform_peak_relative_path); // v12
    w.write_opt_u64(&a.duration_samples); // v12
}

fn encode_song_text_event(w: &mut FbWriter, event: &ProjectSongTextEvent) {
    w.write_str(&event.id);
    w.write_f64(event.beat);
    match &event.kind {
        ProjectSongTextEventKind::Chord { symbol } => {
            w.write_u8(0);
            w.write_str(symbol);
        }
        ProjectSongTextEventKind::Lyric {
            text,
            syllable_mode,
            continuation,
            duration_beats,
            syllables,
        } => {
            w.write_u8(1);
            w.write_str(text);
            w.write_u8(match syllable_mode {
                ProjectLyricSyllableMode::Phrase => 0,
                ProjectLyricSyllableMode::Syllables => 1,
            });
            w.write_bool(*continuation);
            w.write_opt_f64(duration_beats);
            w.write_u32(syllables.len() as u32);
            for syllable in syllables {
                w.write_str(&syllable.text);
                w.write_f64(syllable.offset_beats);
                w.write_opt_f64(&syllable.duration_beats);
            }
        }
        ProjectSongTextEventKind::Section {
            name,
            section_type,
            color_hex,
        } => {
            w.write_u8(2);
            w.write_str(name);
            w.write_u8(match section_type {
                ProjectSongSectionType::Custom => 0,
                ProjectSongSectionType::Intro => 1,
                ProjectSongSectionType::Verse => 2,
                ProjectSongSectionType::PreChorus => 3,
                ProjectSongSectionType::Chorus => 4,
                ProjectSongSectionType::Bridge => 5,
                ProjectSongSectionType::Solo => 6,
                ProjectSongSectionType::Outro => 7,
            });
            w.write_str(color_hex);
        }
    }
}

fn decode_song_text_event(r: &mut FbReader) -> Result<ProjectSongTextEvent, ProjectError> {
    let id = r.read_str()?;
    let beat = r.read_f64()?;
    let kind = match r.read_u8()? {
        0 => ProjectSongTextEventKind::Chord {
            symbol: r.read_str()?,
        },
        1 => {
            let text = r.read_str()?;
            let syllable_mode = match r.read_u8()? {
                0 => ProjectLyricSyllableMode::Phrase,
                1 => ProjectLyricSyllableMode::Syllables,
                tag => {
                    return Err(ProjectError::Corrupted(format!(
                        "bad lyric syllable mode tag {tag}"
                    )))
                }
            };
            let continuation = r.read_bool()?;
            let duration_beats = r.read_opt_f64()?;
            let syllable_count = r.read_u32()? as usize;
            const MIN_SYLLABLE_BYTES: usize = 13;
            if syllable_count > 100_000 || syllable_count > r.remaining() / MIN_SYLLABLE_BYTES {
                return Err(ProjectError::Corrupted(
                    "invalid Song Text syllable count".to_string(),
                ));
            }
            let mut syllables = Vec::with_capacity(syllable_count);
            for _ in 0..syllable_count {
                syllables.push(ProjectLyricSyllable {
                    text: r.read_str()?,
                    offset_beats: r.read_f64()?,
                    duration_beats: r.read_opt_f64()?,
                });
            }
            ProjectSongTextEventKind::Lyric {
                text,
                syllable_mode,
                continuation,
                duration_beats,
                syllables,
            }
        }
        2 => {
            let name = r.read_str()?;
            let section_type = match r.read_u8()? {
                0 => ProjectSongSectionType::Custom,
                1 => ProjectSongSectionType::Intro,
                2 => ProjectSongSectionType::Verse,
                3 => ProjectSongSectionType::PreChorus,
                4 => ProjectSongSectionType::Chorus,
                5 => ProjectSongSectionType::Bridge,
                6 => ProjectSongSectionType::Solo,
                7 => ProjectSongSectionType::Outro,
                tag => {
                    return Err(ProjectError::Corrupted(format!(
                        "bad song section type tag {tag}"
                    )))
                }
            };
            ProjectSongTextEventKind::Section {
                name,
                section_type,
                color_hex: r.read_str()?,
            }
        }
        tag => {
            return Err(ProjectError::Corrupted(format!(
                "bad song text event kind tag {tag}"
            )))
        }
    };
    Ok(ProjectSongTextEvent { id, beat, kind })
}

#[derive(Debug, Clone, PartialEq)]
struct LegacyProjectSongTextCue {
    id: String,
    beat: f64,
    chord: String,
    lyric: String,
}

fn decode_legacy_song_text_cue(r: &mut FbReader) -> Result<LegacyProjectSongTextCue, ProjectError> {
    Ok(LegacyProjectSongTextCue {
        id: r.read_str()?,
        beat: r.read_f64()?,
        chord: r.read_str()?,
        lyric: r.read_str()?,
    })
}

fn migrate_legacy_song_text_cue(
    cue: LegacyProjectSongTextCue,
    events: &mut Vec<ProjectSongTextEvent>,
) {
    let has_chord = !cue.chord.trim().is_empty();
    let has_lyric = !cue.lyric.trim().is_empty();
    match (has_chord, has_lyric) {
        (true, true) => {
            events.push(ProjectSongTextEvent {
                id: format!("{}:chord", cue.id),
                beat: cue.beat,
                kind: ProjectSongTextEventKind::Chord { symbol: cue.chord },
            });
            events.push(ProjectSongTextEvent {
                id: format!("{}:lyric", cue.id),
                beat: cue.beat,
                kind: ProjectSongTextEventKind::Lyric {
                    text: cue.lyric,
                    syllable_mode: ProjectLyricSyllableMode::Phrase,
                    continuation: false,
                    duration_beats: None,
                    syllables: Vec::new(),
                },
            });
        }
        (true, false) => events.push(ProjectSongTextEvent {
            id: cue.id,
            beat: cue.beat,
            kind: ProjectSongTextEventKind::Chord { symbol: cue.chord },
        }),
        (false, true) => events.push(ProjectSongTextEvent {
            id: cue.id,
            beat: cue.beat,
            kind: ProjectSongTextEventKind::Lyric {
                text: cue.lyric,
                syllable_mode: ProjectLyricSyllableMode::Phrase,
                continuation: false,
                duration_beats: None,
                syllables: Vec::new(),
            },
        }),
        (false, false) => {}
    }
}

fn encode_body(project: &FutureboardProject) -> Vec<u8> {
    let mut w = FbWriter::new();

    // Header fields
    w.write_str(&project.id);
    w.write_str(&project.name);
    w.write_u64(project.created_at);
    w.write_u64(project.modified_at);

    // Settings
    w.write_f64(project.settings.bpm);
    w.write_u32(project.settings.time_sig_num);
    w.write_u32(project.settings.time_sig_den);
    w.write_u32(project.settings.sample_rate);
    w.write_u32(project.settings.bit_depth);

    // Mixer
    w.write_f32(project.mixer.master_volume_norm);
    w.write_u32(project.mixer.master_inserts.len() as u32);
    for ins in &project.mixer.master_inserts {
        encode_insert(&mut w, ins);
    }

    // Tracks
    w.write_u32(project.tracks.len() as u32);
    for t in &project.tracks {
        encode_track(&mut w, t);
    }

    // Assets
    w.write_u32(project.assets.len() as u32);
    for a in &project.assets {
        encode_asset(&mut w, a);
    }

    // Tempo automation markers (v7+). Appended at the end of the body so older
    // readers that stop after assets are unaffected. v8+ includes stable ids.
    w.write_u32(project.settings.tempo_points.len() as u32);
    for p in &project.settings.tempo_points {
        if PROJECT_VERSION >= 8 {
            w.write_str(&p.id);
        }
        w.write_f64(p.beat);
        w.write_f64(p.bpm);
        w.write_u8(p.curve);
    }

    // Time signature markers (v9+).
    w.write_u32(project.settings.time_signature_points.len() as u32);
    for p in &project.settings.time_signature_points {
        w.write_str(&p.id);
        w.write_f64(p.beat);
        w.write_u32(p.numerator as u32);
        w.write_u32(p.denominator as u32);
        w.write_u32(p.grouping.len() as u32);
        for g in &p.grouping {
            w.write_u32(*g as u32);
        }
    }

    // Timeline arrangement markers and regions (v13+).
    w.write_u32(project.settings.timeline_markers.len() as u32);
    for marker in &project.settings.timeline_markers {
        w.write_str(&marker.id);
        w.write_f64(marker.beat);
        w.write_str(&marker.name);
        w.write_str(&marker.color_hex);
    }
    w.write_u32(project.settings.timeline_regions.len() as u32);
    for region in &project.settings.timeline_regions {
        w.write_str(&region.id);
        w.write_f64(region.start_beat);
        w.write_f64(region.end_beat);
        w.write_str(&region.name);
        w.write_str(&region.color_hex);
    }

    // Mixer tree UI state (v20+).
    w.write_u32(project.mixer.tree_expanded_node_ids.len() as u32);
    for id in &project.mixer.tree_expanded_node_ids {
        w.write_str(id);
    }
    w.write_u32(project.mixer.tree_pinned_channel_ids.len() as u32);
    for id in &project.mixer.tree_pinned_channel_ids {
        w.write_str(id);
    }
    w.write_u32(project.mixer.tree_hidden_channel_ids.len() as u32);
    for id in &project.mixer.tree_hidden_channel_ids {
        w.write_str(id);
    }

    // Typed Song Text events (v27+). Kept at the body tail for backwards loading.
    w.write_u32(project.settings.song_text_events.len() as u32);
    for event in &project.settings.song_text_events {
        encode_song_text_event(&mut w, event);
    }

    w.into_bytes()
}

/// Encodes a `FutureboardProject` into the full `.fbproj` binary format.
pub fn encode_project(project: &FutureboardProject) -> Vec<u8> {
    let body = encode_body(project);
    let checksum = crc32fast::hash(&body);

    let mut out = Vec::with_capacity(8 + 4 + 4 + 4 + body.len() + 4);
    out.extend_from_slice(PROJECT_MAGIC);
    out.extend_from_slice(&PROJECT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

// ── Decoding ──────────────────────────────────────────────────────────────────

fn decode_plugin_format(r: &mut FbReader) -> Result<PluginFormat, ProjectError> {
    Ok(match r.read_u8()? {
        0 => PluginFormat::Vst3,
        1 => PluginFormat::Clap,
        2 => PluginFormat::Au,
        3 => PluginFormat::Lv2,
        _ => PluginFormat::Unknown,
    })
}

fn decode_opt_plugin_format(r: &mut FbReader) -> Result<Option<PluginFormat>, ProjectError> {
    match r.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(decode_plugin_format(r)?)),
        t => Err(ProjectError::Corrupted(format!("bad option tag {t}"))),
    }
}

fn decode_plugin_state_blob(r: &mut FbReader) -> Result<PluginStateBlob, ProjectError> {
    Ok(PluginStateBlob {
        plugin_id: r.read_str()?,
        format: decode_opt_plugin_format(r)?,
        state_bytes: r.read_bytes()?,
        vendor: r.read_opt_str()?,
        name: r.read_opt_str()?,
        version: r.read_opt_str()?,
    })
}

fn decode_plugin_instance(r: &mut FbReader) -> Result<ProjectPluginInstance, ProjectError> {
    Ok(ProjectPluginInstance {
        instance_id: r.read_str()?,
        format: decode_plugin_format(r)?,
        plugin_path: r.read_opt_path()?,
        plugin_uid: r.read_str()?,
        display_name: r.read_str()?,
        state: decode_plugin_state_blob(r)?,
    })
}

fn decode_insert(r: &mut FbReader, version: u32) -> Result<ProjectInsert, ProjectError> {
    let id = r.read_str()?;
    let slot_index = r.read_u32()?;
    let bypassed = r.read_bool()?;
    let enabled_audio_output_channels = if version >= 18 {
        let count = r.read_u32()? as usize;
        let mut channels = Vec::with_capacity(count.min(32));
        for _ in 0..count {
            let channel = r.read_u8()?;
            if (1..=32).contains(&channel) && !channels.contains(&channel) {
                channels.push(channel);
            }
        }
        channels
    } else {
        Vec::new()
    };
    let multiout_collapsed = if version >= 19 { r.read_bool()? } else { false };
    let plugin = match r.read_u8()? {
        0 => None,
        1 => Some(decode_plugin_instance(r)?),
        t => {
            return Err(ProjectError::Corrupted(format!(
                "bad plugin option tag {t}"
            )))
        }
    };
    Ok(ProjectInsert {
        id,
        slot_index,
        bypassed,
        enabled_audio_output_channels,
        multiout_collapsed,
        plugin,
    })
}

fn decode_automation_lane(r: &mut FbReader, version: u32) -> Result<AutomationLane, ProjectError> {
    let id = r.read_str()?;
    let parameter_name = r.read_str()?;
    let visible = r.read_bool()?;
    let (target, enabled) = if version >= 2 {
        let tag = r.read_u8()?;
        let insert_id = r.read_str()?;
        let parameter_id = r.read_str()?;
        let target_param_name = r.read_str()?;
        let send_id = r.read_str()?;
        let enabled = r.read_bool()?;
        (
            AutomationTargetDesc {
                tag,
                insert_id,
                parameter_id,
                parameter_name: target_param_name,
                send_id,
            },
            enabled,
        )
    } else {
        (AutomationTargetDesc::default(), true)
    };
    let count = r.read_u32()? as usize;
    let mut points = Vec::with_capacity(count);
    for _ in 0..count {
        let beat = r.read_f32()?;
        let value = r.read_f32()?;
        let curve = if version >= 2 { r.read_u8()? } else { 0 };
        let tension = if version >= 21 { r.read_f32()? } else { 0.0 };
        points.push(AutomationPoint {
            beat,
            value,
            curve,
            tension,
        });
    }
    Ok(AutomationLane {
        id,
        parameter_name,
        target,
        enabled,
        visible,
        points,
    })
}

fn decode_midi_note(r: &mut FbReader, version: u32) -> Result<MidiNote, ProjectError> {
    Ok(MidiNote {
        pitch: r.read_u8()?,
        start_beats: r.read_f32()?,
        duration_beats: r.read_f32()?,
        velocity: r.read_u8()?,
        // v4 added the muted flag; older files default to unmuted.
        muted: if version >= 4 { r.read_bool()? } else { false },
        // v22 added a per-note MIDI channel; older files default to channel 1.
        channel: if version >= 22 {
            r.read_u8()?.clamp(1, 16)
        } else {
            1
        },
        // v25 added the per-note articulation tag; older files have none.
        articulation: if version >= 25 { r.read_u8()? } else { 0 },
        // v26 adds stable id + optional release velocity.
        id: if version >= 26 { r.read_u64()? } else { 0 },
        release_velocity: if version >= 26 { r.read_u8()? } else { 0 },
    })
}

fn decode_controller_kind(r: &mut FbReader) -> Result<MidiControllerKind, ProjectError> {
    Ok(match r.read_u8()? {
        0 => MidiControllerKind::CC(r.read_u8()?),
        1 => MidiControllerKind::PitchBend,
        2 => MidiControllerKind::ChannelPressure,
        3 => MidiControllerKind::PolyPressure,
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown controller kind tag {t}"
            )))
        }
    })
}

fn decode_controller_lane(
    r: &mut FbReader,
    version: u32,
) -> Result<MidiControllerLane, ProjectError> {
    let kind = decode_controller_kind(r)?;
    let visible = r.read_bool()?;
    let height = r.read_f32()?;
    let collapsed = r.read_bool()?;
    let count = r.read_u32()? as usize;
    let mut points = Vec::with_capacity(count);
    for _ in 0..count {
        let beat = r.read_f32()?;
        let value = r.read_f32()?;
        let id = if version >= 26 { r.read_u64()? } else { 0 };
        points.push(MidiControllerPoint { id, beat, value });
    }
    Ok(MidiControllerLane {
        kind,
        points,
        visible,
        height,
        collapsed,
    })
}

fn decode_sysex_event(r: &mut FbReader) -> Result<MidiSysExEvent, ProjectError> {
    let kind = match r.read_u8()? {
        0 => MidiSysExKind::Normal,
        1 => MidiSysExKind::Escaped,
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown SysEx event kind tag {t}"
            )))
        }
    };
    Ok(MidiSysExEvent {
        kind,
        tick: r.read_u64()?,
        beat: r.read_f32()?,
        data: r.read_bytes()?,
    })
}

/// v16: per-clip stretch/pitch block. See [`encode_stretch`].
fn decode_stretch(r: &mut FbReader) -> Result<AudioClipStretchState, ProjectError> {
    let mode = StretchMode::from_tag(r.read_u8()?);
    let algorithm = StretchAlgorithm::from_tag(r.read_u8()?);
    let original_sample_rate = r.read_u32()?;
    let project_sample_rate = r.read_u32()?;
    let original_duration_samples = r.read_u64()?;
    let source_start_samples = r.read_u64()?;
    let source_end_samples = r.read_u64()?;
    let clip_timeline_start_beats = r.read_f64()?;
    let clip_timeline_duration_beats = r.read_f64()?;
    let stretch_ratio = r.read_f64()?;
    let bpm_source = r.read_opt_f64()?;
    let bpm_target = r.read_opt_f64()?;
    let preserve_pitch = r.read_bool()?;
    let pitch_shift_semitones = r.read_f32()?;
    let formant_preserve = r.read_bool()?;
    let transient_preserve = r.read_bool()?;
    let transient_sensitivity = r.read_f32()?;
    let reverse = r.read_bool()?;
    let normalize_gain = r.read_bool()?;
    let fade_in_ms = r.read_f32()?;
    let fade_out_ms = r.read_f32()?;
    let gain_db = r.read_f32()?;
    let pan = r.read_f32()?;
    let marker_count = r.read_u32()? as usize;
    let mut warp_markers = Vec::with_capacity(marker_count);
    for _ in 0..marker_count {
        warp_markers.push(WarpMarker {
            id: r.read_u64()?,
            source_sample: r.read_u64()?,
            timeline_beat: r.read_f64()?,
            locked: r.read_bool()?,
        });
    }
    Ok(AudioClipStretchState {
        mode,
        algorithm,
        original_sample_rate,
        project_sample_rate,
        original_duration_samples,
        source_start_samples,
        source_end_samples,
        clip_timeline_start_beats,
        clip_timeline_duration_beats,
        stretch_ratio,
        bpm_source,
        bpm_target,
        preserve_pitch,
        pitch_shift_semitones,
        formant_preserve,
        transient_preserve,
        transient_sensitivity,
        reverse,
        normalize_gain,
        fade_in_ms,
        fade_out_ms,
        gain_db,
        pan,
        // Transient: a freshly loaded clip is not pending re-process.
        dirty: false,
        warp_markers,
    })
}

fn decode_clip(r: &mut FbReader, version: u32) -> Result<ProjectClip, ProjectError> {
    let id = r.read_str()?;
    let name = r.read_str()?;
    let start_beat = r.read_f64()?;
    let duration_beats = r.read_f64()?;
    let offset_beats = r.read_f32()?;
    let gain = r.read_f32()?;
    let muted = r.read_bool()?;
    let source = match r.read_u8()? {
        0 => ClipSource::Empty,
        1 => ClipSource::Audio {
            asset_id: r.read_str()?,
            source_path: r.read_opt_path()?,
        },
        3 if version >= 14 => ClipSource::Rauf {
            asset_id: r.read_str()?,
            source_path: PathBuf::from(r.read_str()?),
            metadata_path: r.read_opt_path()?,
            sample_format: r.read_str()?,
            sample_rate: r.read_u32()?,
            channels: r.read_u32()? as u16,
            start_frame: r.read_u64()?,
            length_frames: r.read_u64()?,
        },
        2 => {
            let count = r.read_u32()? as usize;
            let mut notes = Vec::with_capacity(count);
            for _ in 0..count {
                notes.push(decode_midi_note(r, version)?);
            }
            // v5: controller lanes follow the notes; older files have none.
            let controller_lanes = if version >= 5 {
                let lane_count = r.read_u32()? as usize;
                let mut lanes = Vec::with_capacity(lane_count);
                for _ in 0..lane_count {
                    lanes.push(decode_controller_lane(r, version)?);
                }
                lanes
            } else {
                Vec::new()
            };
            let sysex_events = if version >= 23 {
                let event_count = r.read_u32()? as usize;
                let mut events = Vec::with_capacity(event_count);
                for _ in 0..event_count {
                    events.push(decode_sysex_event(r)?);
                }
                events
            } else {
                Vec::new()
            };
            // v25: direction articulation events; older files have none.
            let articulations = if version >= 25 {
                let event_count = r.read_u32()? as usize;
                let mut events = Vec::with_capacity(event_count);
                for _ in 0..event_count {
                    events.push(MidiArticulation {
                        beat: r.read_f32()?,
                        articulation: r.read_u8()?,
                    });
                }
                events
            } else {
                Vec::new()
            };
            ClipSource::Midi {
                notes,
                controller_lanes,
                sysex_events,
                articulations,
            }
        }
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown clip source tag {t}"
            )))
        }
    };
    // v16: stretch/pitch block trails the source. Older files have none and
    // default to an un-stretched clip.
    let stretch = if version >= 16 {
        decode_stretch(r)?
    } else {
        AudioClipStretchState::default()
    };
    Ok(ProjectClip {
        id,
        name,
        start_beat,
        duration_beats,
        offset_beats,
        gain,
        muted,
        source,
        stretch,
    })
}

fn decode_track_type(r: &mut FbReader) -> Result<ProjectTrackType, ProjectError> {
    Ok(match r.read_u8()? {
        0 => ProjectTrackType::Audio,
        1 => ProjectTrackType::Midi,
        2 => ProjectTrackType::Instrument,
        3 => ProjectTrackType::Bus,
        4 => ProjectTrackType::Return,
        5 => ProjectTrackType::Group,
        6 => ProjectTrackType::Master,
        t => return Err(ProjectError::Corrupted(format!("unknown track type {t}"))),
    })
}

fn decode_input_monitor(r: &mut FbReader) -> Result<InputMonitorMode, ProjectError> {
    Ok(match r.read_u8()? {
        0 => InputMonitorMode::Off,
        1 => InputMonitorMode::Always,
        2 => InputMonitorMode::WhenRecordArmed,
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown input monitor mode {t}"
            )))
        }
    })
}

fn decode_track_input_routing(r: &mut FbReader) -> Result<ProjectTrackInputRouting, ProjectError> {
    Ok(match r.read_u8()? {
        0 => ProjectTrackInputRouting::None,
        1 => ProjectTrackInputRouting::AllInputs,
        2 => ProjectTrackInputRouting::AudioDeviceChannel {
            device_id: r.read_str()?,
            channel: r.read_u32()?,
        },
        3 => ProjectTrackInputRouting::MidiDevice {
            device_id: r.read_str()?,
        },
        4 => {
            let device_id = r.read_str()?;
            let count = r.read_u32()? as usize;
            if count == 0 {
                ProjectTrackInputRouting::None
            } else {
                let mut channels = Vec::with_capacity(count);
                for _ in 0..count {
                    channels.push(r.read_u32()?);
                }
                ProjectTrackInputRouting::AudioDeviceChannels {
                    device_id,
                    channels,
                }
            }
        }
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown track input routing {t}"
            )))
        }
    })
}

fn decode_track_output_routing(
    r: &mut FbReader,
) -> Result<ProjectTrackOutputRouting, ProjectError> {
    Ok(match r.read_u8()? {
        0 => ProjectTrackOutputRouting::Main,
        1 => ProjectTrackOutputRouting::Bus {
            bus_id: r.read_str()?,
        },
        2 => ProjectTrackOutputRouting::HardwareOutput {
            device_id: r.read_str()?,
            channel: r.read_u32()?,
        },
        3 => ProjectTrackOutputRouting::None,
        4 => ProjectTrackOutputRouting::Instrument {
            track_id: r.read_str()?,
        },
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown track output routing {t}"
            )))
        }
    })
}

fn decode_track_audio_format(r: &mut FbReader) -> Result<ProjectTrackAudioFormat, ProjectError> {
    Ok(match r.read_u8()? {
        0 => ProjectTrackAudioFormat::Mono,
        1 => ProjectTrackAudioFormat::Stereo,
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown track audio format {t}"
            )))
        }
    })
}

fn decode_track_midi_input_routing(
    r: &mut FbReader,
) -> Result<ProjectTrackMidiInputRouting, ProjectError> {
    Ok(match r.read_u8()? {
        0 => ProjectTrackMidiInputRouting::None,
        1 => ProjectTrackMidiInputRouting::AllInputs,
        2 => ProjectTrackMidiInputRouting::MidiDevice {
            device_id: r.read_str()?,
        },
        t => {
            return Err(ProjectError::Corrupted(format!(
                "unknown track MIDI input routing {t}"
            )))
        }
    })
}

fn decode_track(r: &mut FbReader, version: u32) -> Result<ProjectTrack, ProjectError> {
    let id = r.read_str()?;
    let name = r.read_str()?;
    let track_type = decode_track_type(r)?;
    let color_hex = r.read_str()?;
    let volume_norm = r.read_f32()?;
    let pan = r.read_f32()?;
    let muted = r.read_bool()?;
    let solo = r.read_bool()?;
    let record_arm = r.read_bool()?;
    let input_monitor = decode_input_monitor(r)?;

    let mut routing = if version >= 3 {
        let input = decode_track_input_routing(r)?;
        let output = decode_track_output_routing(r)?;
        let audio_format = decode_track_audio_format(r)?;
        let midi_input = decode_track_midi_input_routing(r)?;
        let midi_channel = r.read_opt_u8()?.map(|ch| ch.clamp(1, 16));
        // v22 added a per-track "play each note on its own channel" toggle;
        // older files default to `false` (the pre-existing fixed-channel behavior).
        let midi_output_per_note = if version >= 22 { r.read_bool()? } else { false };
        TrackRouting {
            input,
            output,
            audio_format,
            midi_input,
            midi_channel,
            midi_output_per_note,
            sends: Vec::new(),
        }
    } else {
        TrackRouting::default_for_track_type(track_type)
    };
    let output_bus = r.read_opt_str()?;
    if version < 3 {
        if let Some(bus_id) = output_bus {
            routing.output = ProjectTrackOutputRouting::Bus { bus_id };
        }
    }
    let send_count = r.read_u32()? as usize;
    let mut sends = Vec::with_capacity(send_count);
    for _ in 0..send_count {
        let id = r.read_str()?;
        let target_track_id = r.read_str()?;
        let enabled = r.read_bool()?;
        let pre_fader = r.read_bool()?;
        let gain_db = r.read_f32()?;
        sends.push(ProjectSend {
            id,
            target_track_id,
            enabled,
            pre_fader,
            gain_db,
        });
    }
    routing.sends = sends;

    let insert_count = r.read_u32()? as usize;
    let mut inserts = Vec::with_capacity(insert_count);
    for _ in 0..insert_count {
        inserts.push(decode_insert(r, version)?);
    }

    let lane_count = r.read_u32()? as usize;
    let mut automation_lanes = Vec::with_capacity(lane_count);
    for _ in 0..lane_count {
        automation_lanes.push(decode_automation_lane(r, version)?);
    }

    let clip_count = r.read_u32()? as usize;
    let mut clips = Vec::with_capacity(clip_count);
    for _ in 0..clip_count {
        clips.push(decode_clip(r, version)?);
    }

    let row_height_px = if version >= 17 {
        r.read_opt_f32()?
    } else {
        None
    };

    let soundfont = if version >= 28 {
        decode_soundfont_player(r)?
    } else {
        None
    };

    Ok(ProjectTrack {
        id,
        name,
        track_type,
        color_hex,
        volume_norm,
        pan,
        muted,
        solo,
        record_arm,
        input_monitor,
        routing,
        inserts,
        automation_lanes,
        clips,
        row_height_px,
        soundfont,
    })
}

fn decode_asset(r: &mut FbReader, version: u32) -> Result<ProjectAsset, ProjectError> {
    Ok(ProjectAsset {
        id: r.read_str()?,
        original_filename: r.read_str()?,
        relative_path: r.read_opt_str()?,
        absolute_path: r.read_opt_path()?,
        duration_secs: r.read_opt_f64()?,
        sample_rate: r.read_opt_u32()?,
        channels: r.read_opt_u8()?,
        // v11 appended a content fingerprint; older files stop before it.
        source_fingerprint: if version >= 11 {
            r.read_opt_str()?
        } else {
            None
        },
        waveform_peak_relative_path: if version >= 12 {
            r.read_opt_str()?
        } else {
            None
        },
        duration_samples: if version >= 12 {
            r.read_opt_u64()?
        } else {
            None
        },
    })
}

fn decode_body(body: &[u8], version: u32) -> Result<FutureboardProject, ProjectError> {
    let mut r = FbReader::new(body);

    let id = r.read_str()?;
    let name = r.read_str()?;
    let created_at = r.read_u64()?;
    let modified_at = r.read_u64()?;

    let bpm = r.read_f64()?;
    let time_sig_num = r.read_u32()?;
    let time_sig_den = r.read_u32()?;
    let sample_rate = r.read_u32()?;
    let bit_depth = r.read_u32()?;

    let master_volume_norm = r.read_f32()?;
    let master_inserts = if version >= 15 {
        let insert_count = r.read_u32()? as usize;
        let mut inserts = Vec::with_capacity(insert_count);
        for _ in 0..insert_count {
            inserts.push(decode_insert(&mut r, version)?);
        }
        inserts
    } else {
        Vec::new()
    };

    let track_count = r.read_u32()? as usize;
    let mut tracks = Vec::with_capacity(track_count);
    for _ in 0..track_count {
        tracks.push(decode_track(&mut r, version)?);
    }

    let asset_count = r.read_u32()? as usize;
    let mut assets = Vec::with_capacity(asset_count);
    for _ in 0..asset_count {
        assets.push(decode_asset(&mut r, version)?);
    }

    // Tempo automation markers (v7+). Pre-v7 files have none. v8+ stores ids.
    let tempo_points = if version >= 7 {
        let count = r.read_u32()? as usize;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let id = if version >= 8 {
                r.read_str()?
            } else {
                String::new()
            };
            let beat = r.read_f64()?;
            let bpm = r.read_f64()?;
            let curve = r.read_u8()?;
            points.push(ProjectTempoPoint {
                id,
                beat,
                bpm,
                curve,
            });
        }
        points
    } else {
        Vec::new()
    };

    let time_signature_points = if version >= 9 {
        let count = r.read_u32()? as usize;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            // Field order must match `encode_body`: id, beat, numerator,
            // denominator, grouping. (A previous build read these out of order,
            // which desynced the cursor and produced spurious EOF errors when a
            // project contained any time-signature point — including the default
            // 4/4 marker every new project carries.)
            let id = r.read_str()?;
            let beat = r.read_f64()?;
            let numerator = r.read_u32()? as u16;
            let denominator = r.read_u32()? as u16;
            let grouping = if version >= 10 {
                let count = r.read_u32()? as usize;
                let mut groups = Vec::with_capacity(count);
                for _ in 0..count {
                    groups.push(r.read_u32()? as u16);
                }
                groups
            } else {
                Vec::new()
            };
            points.push(super::ProjectTimeSignaturePoint {
                id,
                beat,
                numerator,
                denominator,
                grouping,
            });
        }
        points
    } else {
        Vec::new()
    };

    let (timeline_markers, timeline_regions) = if version >= 13 {
        let marker_count = r.read_u32()? as usize;
        let mut markers = Vec::with_capacity(marker_count);
        for _ in 0..marker_count {
            markers.push(ProjectTimelineMarker {
                id: r.read_str()?,
                beat: r.read_f64()?,
                name: r.read_str()?,
                color_hex: r.read_str()?,
            });
        }
        let region_count = r.read_u32()? as usize;
        let mut regions = Vec::with_capacity(region_count);
        for _ in 0..region_count {
            regions.push(ProjectTimelineRegion {
                id: r.read_str()?,
                start_beat: r.read_f64()?,
                end_beat: r.read_f64()?,
                name: r.read_str()?,
                color_hex: r.read_str()?,
            });
        }
        (markers, regions)
    } else {
        (Vec::new(), Vec::new())
    };

    let (tree_expanded_node_ids, tree_pinned_channel_ids, tree_hidden_channel_ids) =
        if version >= 20 {
            let expanded_count = r.read_u32()? as usize;
            let mut tree_expanded_node_ids = Vec::with_capacity(expanded_count);
            for _ in 0..expanded_count {
                tree_expanded_node_ids.push(r.read_str()?);
            }
            let pinned_count = r.read_u32()? as usize;
            let mut tree_pinned_channel_ids = Vec::with_capacity(pinned_count);
            for _ in 0..pinned_count {
                tree_pinned_channel_ids.push(r.read_str()?);
            }
            let hidden_count = r.read_u32()? as usize;
            let mut tree_hidden_channel_ids = Vec::with_capacity(hidden_count);
            for _ in 0..hidden_count {
                tree_hidden_channel_ids.push(r.read_str()?);
            }
            (
                tree_expanded_node_ids,
                tree_pinned_channel_ids,
                tree_hidden_channel_ids,
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

    let song_text_events = if version >= 27 {
        let count = r.read_u32()? as usize;
        const MIN_EVENT_BYTES: usize = 17;
        if count > 1_000_000 || count > r.remaining() / MIN_EVENT_BYTES {
            return Err(ProjectError::Corrupted(
                "invalid Song Text event count".to_string(),
            ));
        }
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            events.push(decode_song_text_event(&mut r)?);
        }
        events
    } else if version >= 24 {
        let count = r.read_u32()? as usize;
        const MIN_LEGACY_CUE_BYTES: usize = 20;
        if count > 1_000_000 || count > r.remaining() / MIN_LEGACY_CUE_BYTES {
            return Err(ProjectError::Corrupted(
                "invalid legacy Song Text cue count".to_string(),
            ));
        }
        let mut events = Vec::with_capacity(count.saturating_mul(2));
        for _ in 0..count {
            let cue = decode_legacy_song_text_cue(&mut r)?;
            migrate_legacy_song_text_cue(cue, &mut events);
        }
        events
    } else {
        Vec::new()
    };

    Ok(FutureboardProject {
        id,
        name,
        created_at,
        modified_at,
        settings: super::ProjectSettings {
            bpm,
            tempo_points,
            time_signature_points,
            timeline_markers,
            timeline_regions,
            song_text_events,
            time_sig_num,
            time_sig_den,
            sample_rate,
            bit_depth,
        },
        tracks,
        mixer: ProjectMixer {
            master_volume_norm,
            master_inserts,
            tree_expanded_node_ids,
            tree_pinned_channel_ids,
            tree_hidden_channel_ids,
        },
        assets,
    })
}

/// Cheaply validate that `data` begins with a supported Futureboard project
/// header (magic + version) without decoding the body or verifying the
/// checksum. Returns the on-disk format version. Used for fast pre-load
/// validation (e.g. the Welcome → Open Project flow) so an invalid pick can be
/// reported inline without reading/decoding the whole file.
pub fn peek_project_header(data: &[u8]) -> Result<u32, ProjectError> {
    if data.len() < PROJECT_HEADER_SIZE {
        return Err(ProjectError::IncompleteFile {
            reason: format!(
                "file too small for project header ({} bytes, need {})",
                data.len(),
                PROJECT_HEADER_SIZE
            ),
        });
    }
    if &data[0..8] != PROJECT_MAGIC {
        return Err(ProjectError::InvalidMagic);
    }
    let version = u32::from_le_bytes(data[8..12].try_into().unwrap());
    if version == 0 || version > PROJECT_VERSION {
        return Err(ProjectError::UnsupportedVersion(version));
    }
    Ok(version)
}

/// Decodes a `.fbproj` binary blob into a `FutureboardProject`.
pub fn decode_project(data: &[u8]) -> Result<FutureboardProject, ProjectError> {
    project_load_log(format_args!("file size: {} bytes", data.len()));

    if data.len() < PROJECT_HEADER_SIZE {
        let err = ProjectError::IncompleteFile {
            reason: format!(
                "file too small for project header ({} bytes, need {})",
                data.len(),
                PROJECT_HEADER_SIZE
            ),
        };
        project_load_log(format_args!("failed: {}", err.technical_detail()));
        return Err(err);
    }

    if &data[0..8] != PROJECT_MAGIC {
        let err = ProjectError::InvalidMagic;
        project_load_log(format_args!("failed: {}", err.technical_detail()));
        return Err(err);
    }

    let version = u32::from_le_bytes(data[8..12].try_into().unwrap());
    if version == 0 || version > PROJECT_VERSION {
        let err = ProjectError::UnsupportedVersion(version);
        project_load_log(format_args!("failed: {}", err.technical_detail()));
        return Err(err);
    }
    project_load_log(format_args!("header ok version={version}"));

    let body_len = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;
    let required = PROJECT_HEADER_SIZE
        .checked_add(body_len)
        .and_then(|n| n.checked_add(4));
    let Some(required) = required else {
        let err = ProjectError::IncompleteFile {
            reason: "project payload length overflow".to_string(),
        };
        project_load_log(format_args!("failed: {}", err.technical_detail()));
        return Err(err);
    };

    if data.len() < required {
        let err = ProjectError::IncompleteFile {
            reason: format!(
                "file truncated: declared payload {body_len} bytes but only {} bytes on disk",
                data.len().saturating_sub(PROJECT_HEADER_SIZE + 4)
            ),
        };
        project_load_log(format_args!("failed: {}", err.technical_detail()));
        return Err(err);
    }

    let body = &data[PROJECT_HEADER_SIZE..PROJECT_HEADER_SIZE + body_len];
    project_load_log(format_args!("payload bytes={body_len}"));
    let stored_crc = u32::from_le_bytes(
        data[PROJECT_HEADER_SIZE + body_len..PROJECT_HEADER_SIZE + body_len + 4]
            .try_into()
            .unwrap(),
    );
    let computed_crc = crc32fast::hash(body);

    if computed_crc != stored_crc {
        let err = ProjectError::ChecksumMismatch {
            expected: stored_crc,
            got: computed_crc,
        };
        project_load_log(format_args!("failed: {}", err.technical_detail()));
        return Err(err);
    }

    match decode_body(body, version) {
        Ok(project) => Ok(project),
        Err(err) => {
            project_load_log(format_args!("failed: {}", err.technical_detail()));
            Err(err)
        }
    }
}

fn project_load_log(args: std::fmt::Arguments<'_>) {
    eprintln!("[ProjectLoad] {args}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, muted: bool) -> MidiNote {
        MidiNote {
            id: 0,
            pitch,
            start_beats: 1.5,
            duration_beats: 0.5,
            velocity: 90,
            release_velocity: 0,
            muted,
            channel: 1,
            articulation: 0,
        }
    }

    fn project_bytes_with_version(body: Vec<u8>, version: u32) -> Vec<u8> {
        let checksum = crc32fast::hash(&body);
        let mut bytes = Vec::with_capacity(PROJECT_HEADER_SIZE + body.len() + 4);
        bytes.extend_from_slice(PROJECT_MAGIC);
        bytes.extend_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&checksum.to_le_bytes());
        bytes
    }

    fn encode_legacy_song_text_project(version: u32, cues: &[LegacyProjectSongTextCue]) -> Vec<u8> {
        let mut body = encode_body(&FutureboardProject::new("Legacy Song Text"));
        body.truncate(body.len() - std::mem::size_of::<u32>());

        let mut tail = FbWriter::new();
        tail.write_u32(cues.len() as u32);
        for cue in cues {
            tail.write_str(&cue.id);
            tail.write_f64(cue.beat);
            tail.write_str(&cue.chord);
            tail.write_str(&cue.lyric);
        }
        body.extend_from_slice(&tail.into_bytes());
        project_bytes_with_version(body, version)
    }

    #[test]
    fn midi_note_articulation_roundtrips_v25() {
        let mut w = FbWriter::new();
        let mut n = note(60, false);
        n.articulation = 2; // Staccato tag
        encode_midi_note(&mut w, &n);
        encode_midi_note(&mut w, &note(64, false)); // articulation 0 = none
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let a = decode_midi_note(&mut r, PROJECT_VERSION).unwrap();
        let b = decode_midi_note(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(a.articulation, 2);
        assert_eq!(b.articulation, 0);
    }

    #[test]
    fn pre_v25_midi_note_decodes_with_no_articulation() {
        // v24 and earlier wrote no articulation byte.
        let mut w = FbWriter::new();
        w.write_u8(60);
        w.write_f32(1.5);
        w.write_f32(0.5);
        w.write_u8(90);
        w.write_bool(false); // muted (v4)
        w.write_u8(1); // channel (v22)
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let got = decode_midi_note(&mut r, 24).unwrap();
        assert_eq!(got.articulation, 0);
    }

    #[test]
    fn midi_clip_articulation_events_roundtrip_v25() {
        let clip = ProjectClip {
            id: "clip-1".to_string(),
            name: "MIDI".to_string(),
            start_beat: 0.0,
            duration_beats: 8.0,
            offset_beats: 0.0,
            gain: 1.0,
            muted: false,
            source: ClipSource::Midi {
                notes: vec![note(60, false)],
                controller_lanes: Vec::new(),
                sysex_events: Vec::new(),
                articulations: vec![
                    MidiArticulation {
                        beat: 0.0,
                        articulation: 1,
                    },
                    MidiArticulation {
                        beat: 4.0,
                        articulation: 4,
                    },
                ],
            },
            stretch: AudioClipStretchState::default(),
        };
        let mut w = FbWriter::new();
        encode_clip(&mut w, &clip);
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let decoded = decode_clip(&mut r, PROJECT_VERSION).unwrap();
        let ClipSource::Midi { articulations, .. } = decoded.source else {
            panic!("expected midi source");
        };
        assert_eq!(
            articulations,
            vec![
                MidiArticulation {
                    beat: 0.0,
                    articulation: 1
                },
                MidiArticulation {
                    beat: 4.0,
                    articulation: 4
                },
            ]
        );
    }

    #[test]
    fn midi_note_muted_roundtrips_v4() {
        let mut w = FbWriter::new();
        encode_midi_note(&mut w, &note(60, true));
        encode_midi_note(&mut w, &note(64, false));
        let bytes = w.into_bytes();

        let mut r = FbReader::new(&bytes);
        let a = decode_midi_note(&mut r, PROJECT_VERSION).unwrap();
        let b = decode_midi_note(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(a.pitch, 60);
        assert!(a.muted);
        assert_eq!(b.pitch, 64);
        assert!(!b.muted);
    }

    #[test]
    fn pre_v4_notes_decode_unmuted() {
        // v3 and earlier wrote no muted byte: pitch, start, dur, velocity only.
        let mut w = FbWriter::new();
        w.write_u8(72);
        w.write_f32(0.0);
        w.write_f32(1.0);
        w.write_u8(100);
        let bytes = w.into_bytes();

        let mut r = FbReader::new(&bytes);
        let n = decode_midi_note(&mut r, 3).unwrap();
        assert_eq!(n.pitch, 72);
        assert!(!n.muted, "older files must default to unmuted");
    }

    #[test]
    fn midi_note_channel_roundtrips_v22() {
        let mut n = note(60, false);
        n.channel = 5;
        let mut w = FbWriter::new();
        encode_midi_note(&mut w, &n);
        let bytes = w.into_bytes();

        let mut r = FbReader::new(&bytes);
        let got = decode_midi_note(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(got.channel, 5);
    }

    #[test]
    fn pre_v22_notes_decode_channel_one() {
        // v21 and earlier wrote no channel byte: pitch, start, dur, velocity, muted.
        let mut w = FbWriter::new();
        w.write_u8(72);
        w.write_f32(0.0);
        w.write_f32(1.0);
        w.write_u8(100);
        w.write_bool(false);
        let bytes = w.into_bytes();

        let mut r = FbReader::new(&bytes);
        let n = decode_midi_note(&mut r, 21).unwrap();
        assert_eq!(n.channel, 1, "older files must default to channel 1");
    }

    #[test]
    fn controller_lane_roundtrips() {
        let lane = MidiControllerLane {
            kind: MidiControllerKind::CC(11),
            points: vec![
                MidiControllerPoint {
                    id: 11,
                    beat: 0.0,
                    value: 0.0,
                },
                MidiControllerPoint {
                    id: 22,
                    beat: 2.5,
                    value: 1.0,
                },
            ],
            visible: true,
            height: 72.0,
            collapsed: false,
        };
        let mut w = FbWriter::new();
        encode_controller_lane(&mut w, &lane);
        let bytes = w.into_bytes();

        let mut r = FbReader::new(&bytes);
        let got = decode_controller_lane(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(got.kind, MidiControllerKind::CC(11));
        assert_eq!(got.points.len(), 2);
        assert_eq!(got.points[1].id, 22);
        assert_eq!(got.points[1].beat, 2.5);
        assert_eq!(got.points[1].value, 1.0);
        assert_eq!(got.height, 72.0);
        assert!(got.visible);
    }

    #[test]
    fn midi_note_id_and_release_velocity_roundtrip_v26() {
        let mut n = note(60, false);
        n.id = 0xABCD_EF01;
        n.release_velocity = 64;
        let mut w = FbWriter::new();
        encode_midi_note(&mut w, &n);
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let got = decode_midi_note(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(got.id, 0xABCD_EF01);
        assert_eq!(got.release_velocity, 64);
    }

    #[test]
    fn pre_v26_midi_note_decodes_without_id() {
        let mut w = FbWriter::new();
        w.write_u8(60);
        w.write_f32(1.5);
        w.write_f32(0.5);
        w.write_u8(90);
        w.write_bool(false);
        w.write_u8(1);
        w.write_u8(0); // articulation (v25)
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let got = decode_midi_note(&mut r, 25).unwrap();
        assert_eq!(got.id, 0);
        assert_eq!(got.release_velocity, 0);
    }

    #[test]
    fn multi_channel_audio_input_routing_roundtrips_v6() {
        let routing = ProjectTrackInputRouting::AudioDeviceChannels {
            device_id: "Interface 8i6".to_string(),
            channels: vec![2, 3],
        };
        let mut w = FbWriter::new();
        encode_track_input_routing(&mut w, &routing);
        let bytes = w.into_bytes();

        let mut r = FbReader::new(&bytes);
        assert_eq!(decode_track_input_routing(&mut r).unwrap(), routing);
    }

    #[test]
    fn insert_audio_output_channels_roundtrip_v18() {
        let mut project = FutureboardProject::new("Insert Outputs");
        project.mixer.master_inserts.push(ProjectInsert {
            id: "insert-1".to_string(),
            slot_index: 0,
            bypassed: false,
            enabled_audio_output_channels: vec![1, 2, 3, 4],
            multiout_collapsed: true,
            plugin: None,
        });

        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");
        assert!(
            decoded.mixer.master_inserts[0].multiout_collapsed,
            "collapse flag must roundtrip"
        );
        assert_eq!(
            decoded.mixer.master_inserts[0].enabled_audio_output_channels,
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn peek_header_accepts_encoded_project() {
        let project = FutureboardProject::new("Peek Test");
        let bytes = encode_project(&project);
        let version = peek_project_header(&bytes).expect("valid header");
        assert_eq!(version, PROJECT_VERSION);
    }

    #[test]
    fn tempo_points_roundtrip_v8() {
        let mut project = FutureboardProject::new("Tempo Test");
        project.settings.tempo_points = vec![
            ProjectTempoPoint {
                id: "tempo-a".to_string(),
                beat: 0.0,
                bpm: 120.0,
                curve: 0,
            },
            ProjectTempoPoint {
                id: "tempo-b".to_string(),
                beat: 8.0,
                bpm: 140.0,
                curve: 1,
            },
        ];
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");
        assert_eq!(decoded.settings.tempo_points, project.settings.tempo_points);
    }

    #[test]
    fn song_text_events_roundtrip_v27() {
        let mut project = FutureboardProject::new("Song Text");
        project.settings.song_text_events = vec![
            ProjectSongTextEvent {
                id: "chord-1".to_string(),
                beat: 4.0,
                kind: ProjectSongTextEventKind::Chord {
                    symbol: "F♯m7/C♯".to_string(),
                },
            },
            ProjectSongTextEvent {
                id: "lyric-1".to_string(),
                beat: 4.5,
                kind: ProjectSongTextEventKind::Lyric {
                    text: "คืนที่ดาวเต็มฟ้า".to_string(),
                    syllable_mode: ProjectLyricSyllableMode::Syllables,
                    continuation: true,
                    duration_beats: Some(3.5),
                    syllables: vec![
                        ProjectLyricSyllable {
                            text: "คืน".to_string(),
                            offset_beats: 0.0,
                            duration_beats: Some(0.75),
                        },
                        ProjectLyricSyllable {
                            text: "ที่ดาว".to_string(),
                            offset_beats: 0.75,
                            duration_beats: None,
                        },
                    ],
                },
            },
            ProjectSongTextEvent {
                id: "section-1".to_string(),
                beat: 16.0,
                kind: ProjectSongTextEventKind::Section {
                    name: "Final Chorus".to_string(),
                    section_type: ProjectSongSectionType::Chorus,
                    color_hex: "#C45CFF".to_string(),
                },
            },
        ];

        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode typed song text");
        assert_eq!(
            decoded.settings.song_text_events,
            project.settings.song_text_events
        );
    }

    #[test]
    fn legacy_song_text_cues_migrate_for_v24_through_v26() {
        let cues = vec![
            LegacyProjectSongTextCue {
                id: "chord-only".to_string(),
                beat: 1.0,
                chord: "Cmaj7".to_string(),
                lyric: String::new(),
            },
            LegacyProjectSongTextCue {
                id: "lyric-only".to_string(),
                beat: 2.0,
                chord: String::new(),
                lyric: "Hello".to_string(),
            },
            LegacyProjectSongTextCue {
                id: "combined".to_string(),
                beat: 3.0,
                chord: "Am7".to_string(),
                lyric: "world".to_string(),
            },
        ];
        let expected = vec![
            ProjectSongTextEvent {
                id: "chord-only".to_string(),
                beat: 1.0,
                kind: ProjectSongTextEventKind::Chord {
                    symbol: "Cmaj7".to_string(),
                },
            },
            ProjectSongTextEvent {
                id: "lyric-only".to_string(),
                beat: 2.0,
                kind: ProjectSongTextEventKind::Lyric {
                    text: "Hello".to_string(),
                    syllable_mode: ProjectLyricSyllableMode::Phrase,
                    continuation: false,
                    duration_beats: None,
                    syllables: Vec::new(),
                },
            },
            ProjectSongTextEvent {
                id: "combined:chord".to_string(),
                beat: 3.0,
                kind: ProjectSongTextEventKind::Chord {
                    symbol: "Am7".to_string(),
                },
            },
            ProjectSongTextEvent {
                id: "combined:lyric".to_string(),
                beat: 3.0,
                kind: ProjectSongTextEventKind::Lyric {
                    text: "world".to_string(),
                    syllable_mode: ProjectLyricSyllableMode::Phrase,
                    continuation: false,
                    duration_beats: None,
                    syllables: Vec::new(),
                },
            },
        ];

        for version in 24..=26 {
            let bytes = encode_legacy_song_text_project(version, &cues);
            let decoded = decode_project(&bytes).expect("decode legacy song text");
            assert_eq!(decoded.settings.song_text_events, expected, "v{version}");
        }

        let bytes = encode_legacy_song_text_project(23, &cues);
        let decoded = decode_project(&bytes).expect("decode pre-song-text project");
        assert!(decoded.settings.song_text_events.is_empty());
    }

    #[test]
    fn legacy_song_text_positions_are_sanitized_when_applied() {
        let cues = vec![
            LegacyProjectSongTextCue {
                id: "negative".to_string(),
                beat: -4.0,
                chord: "  Dm9  ".to_string(),
                lyric: String::new(),
            },
            LegacyProjectSongTextCue {
                id: "non-finite".to_string(),
                beat: f64::INFINITY,
                chord: String::new(),
                lyric: "discard me".to_string(),
            },
        ];
        let bytes = encode_legacy_song_text_project(26, &cues);
        let project = decode_project(&bytes).expect("decode legacy positions");
        let mut timeline = crate::components::timeline::timeline_state::TimelineState::default();

        super::super::apply_to_timeline(&project, &mut timeline);

        assert_eq!(timeline.song_text_events.len(), 1);
        assert_eq!(timeline.song_text_events[0].id, "negative");
        assert_eq!(timeline.song_text_events[0].beat, 0.0);
        assert_eq!(timeline.song_text_events[0].text(), "Dm9");
    }

    #[test]
    fn time_signature_points_roundtrip_v10() {
        let mut project = FutureboardProject::new("TimeSig Test");
        project.settings.time_signature_points = vec![
            super::super::ProjectTimeSignaturePoint {
                id: "ts-a".to_string(),
                beat: 0.0,
                numerator: 4,
                denominator: 4,
                grouping: Vec::new(),
            },
            super::super::ProjectTimeSignaturePoint {
                id: "ts-b".to_string(),
                beat: 16.0,
                numerator: 7,
                denominator: 8,
                grouping: vec![2, 2, 3],
            },
        ];
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");
        assert_eq!(
            decoded.settings.time_signature_points,
            project.settings.time_signature_points
        );
    }

    #[test]
    fn default_project_with_time_signature_point_roundtrips() {
        // Mirrors what New Project writes: a default 4/4 marker. This regressed
        // because the decoder read time-signature fields out of order.
        let mut project = FutureboardProject::new("Fresh");
        project.settings.time_signature_points = vec![super::super::ProjectTimeSignaturePoint {
            id: "ts-default".to_string(),
            beat: 0.0,
            numerator: 4,
            denominator: 4,
            grouping: vec![1, 1, 1, 1],
        }];
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("default project must decode");
        assert_eq!(decoded.name, "Fresh");
        assert_eq!(decoded.settings.time_signature_points.len(), 1);
        assert_eq!(decoded.settings.time_signature_points[0].numerator, 4);
        assert_eq!(decoded.settings.time_signature_points[0].denominator, 4);
    }

    #[test]
    fn peek_header_rejects_bad_magic() {
        let mut bytes = encode_project(&FutureboardProject::new("X"));
        bytes[0] = b'Z'; // corrupt the magic
        assert!(matches!(
            peek_project_header(&bytes),
            Err(ProjectError::InvalidMagic)
        ));
    }

    #[test]
    fn peek_header_rejects_future_version() {
        let mut bytes = encode_project(&FutureboardProject::new("X"));
        // Bump the version field (bytes 8..12) past the supported max.
        bytes[8..12].copy_from_slice(&(PROJECT_VERSION + 1).to_le_bytes());
        assert!(matches!(
            peek_project_header(&bytes),
            Err(ProjectError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn peek_header_rejects_tiny_input() {
        assert!(matches!(
            peek_project_header(&[0u8; 4]),
            Err(ProjectError::IncompleteFile { .. })
        ));
    }

    #[test]
    fn truncated_body_reports_unexpected_eof() {
        let bytes = encode_project(&FutureboardProject::new("Body"));
        let body = &bytes[PROJECT_HEADER_SIZE..bytes.len() - 4];
        let truncated_body = &body[..body.len().saturating_sub(3).max(1)];
        let err = decode_body(truncated_body, PROJECT_VERSION).unwrap_err();
        assert!(
            matches!(err, ProjectError::UnexpectedEof { .. })
                || matches!(err, ProjectError::Corrupted(_))
        );
        assert_eq!(
            err.user_message(),
            "Could not open this project because the file appears to be incomplete or corrupted."
        );
    }

    #[test]
    fn controller_kind_tags_roundtrip() {
        for kind in [
            MidiControllerKind::CC(64),
            MidiControllerKind::PitchBend,
            MidiControllerKind::ChannelPressure,
            MidiControllerKind::PolyPressure,
        ] {
            let mut w = FbWriter::new();
            encode_controller_kind(&mut w, kind);
            let bytes = w.into_bytes();
            let mut r = FbReader::new(&bytes);
            assert_eq!(decode_controller_kind(&mut r).unwrap(), kind);
        }
    }

    fn sample_stretch() -> AudioClipStretchState {
        AudioClipStretchState {
            mode: StretchMode::TempoSync,
            algorithm: StretchAlgorithm::PhaseVocoder,
            original_sample_rate: 44_100,
            project_sample_rate: 48_000,
            original_duration_samples: 88_200,
            source_start_samples: 100,
            source_end_samples: 80_000,
            clip_timeline_start_beats: 4.0,
            clip_timeline_duration_beats: 8.0,
            stretch_ratio: 0.857_142_857,
            bpm_source: Some(120.0),
            bpm_target: Some(140.0),
            preserve_pitch: true,
            pitch_shift_semitones: -3.0,
            formant_preserve: true,
            transient_preserve: false,
            transient_sensitivity: 0.65,
            reverse: true,
            normalize_gain: true,
            fade_in_ms: 5.0,
            fade_out_ms: 12.5,
            gain_db: -2.0,
            pan: -0.25,
            dirty: false,
            warp_markers: vec![
                WarpMarker {
                    id: 1,
                    source_sample: 0,
                    timeline_beat: 0.0,
                    locked: true,
                },
                WarpMarker {
                    id: 2,
                    source_sample: 22_050,
                    timeline_beat: 2.0,
                    locked: false,
                },
            ],
        }
    }

    fn empty_clip_with_stretch(stretch: AudioClipStretchState) -> ProjectClip {
        ProjectClip {
            id: "c1".to_string(),
            name: "clip".to_string(),
            start_beat: 1.0,
            duration_beats: 4.0,
            offset_beats: 0.0,
            gain: 1.0,
            muted: false,
            source: ClipSource::Empty,
            stretch,
        }
    }

    #[test]
    fn stretch_roundtrips_v16() {
        let clip = empty_clip_with_stretch(sample_stretch());
        let mut w = FbWriter::new();
        encode_clip(&mut w, &clip);
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let decoded = decode_clip(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(decoded.stretch, clip.stretch);
    }

    #[test]
    fn warp_marker_serialization() {
        let clip = empty_clip_with_stretch(sample_stretch());
        let mut w = FbWriter::new();
        encode_clip(&mut w, &clip);
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let decoded = decode_clip(&mut r, PROJECT_VERSION).unwrap();
        assert_eq!(decoded.stretch.warp_markers.len(), 2);
        assert_eq!(decoded.stretch.warp_markers[1].source_sample, 22_050);
        assert!(decoded.stretch.warp_markers[0].locked);
    }

    #[test]
    fn old_project_load_defaults() {
        // Hand-encode a pre-v16 clip body (no stretch trailer) and decode at v15:
        // the clip must fall back to the un-stretched defaults (spec §13).
        let mut w = FbWriter::new();
        w.write_str("c1");
        w.write_str("clip");
        w.write_f64(0.0);
        w.write_f64(4.0);
        w.write_f32(0.0);
        w.write_f32(1.0);
        w.write_bool(false);
        w.write_u8(0); // ClipSource::Empty
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let decoded = decode_clip(&mut r, 15).unwrap();
        assert_eq!(decoded.stretch, AudioClipStretchState::default());
        assert_eq!(decoded.stretch.mode, StretchMode::Off);
        assert_eq!(decoded.stretch.stretch_ratio, 1.0);
        assert!(!decoded.stretch.preserve_pitch);
    }

    #[test]
    fn soundfont_player_track_round_trips() {
        let mut track = track_with_clip(ProjectClip {
            id: "c1".to_string(),
            name: "clip".to_string(),
            start_beat: 0.0,
            duration_beats: 4.0,
            offset_beats: 0.0,
            gain: 1.0,
            muted: false,
            source: ClipSource::Empty,
            stretch: AudioClipStretchState::default(),
        });
        track.soundfont = Some(ProjectSoundfontPlayer {
            path: Some(PathBuf::from("/home/user/SoundFonts/GeneralUser-GS.sf2")),
            preset_bank: Some(128),
            preset_patch: Some(8),
            volume: 0.65,
            reverb_chorus: false,
            polyphony: 96,
        });

        let mut project = FutureboardProject::new("Soundfont");
        project.tracks.push(track);
        let decoded = decode_project(&encode_project(&project)).expect("round trip");
        let soundfont = decoded.tracks[0]
            .soundfont
            .as_ref()
            .expect("soundfont persisted");
        assert_eq!(
            soundfont.path.as_deref(),
            Some(std::path::Path::new(
                "/home/user/SoundFonts/GeneralUser-GS.sf2"
            ))
        );
        assert_eq!(soundfont.preset_bank, Some(128));
        assert_eq!(soundfont.preset_patch, Some(8));
        assert!((soundfont.volume - 0.65).abs() < 1.0e-6);
        assert!(!soundfont.reverb_chorus);
        assert_eq!(soundfont.polyphony, 96);
    }

    #[test]
    fn track_without_a_soundfont_round_trips_as_none() {
        let track = track_with_clip(ProjectClip {
            id: "c1".to_string(),
            name: "clip".to_string(),
            start_beat: 0.0,
            duration_beats: 4.0,
            offset_beats: 0.0,
            gain: 1.0,
            muted: false,
            source: ClipSource::Empty,
            stretch: AudioClipStretchState::default(),
        });
        let mut project = FutureboardProject::new("Plain");
        project.tracks.push(track);
        let decoded = decode_project(&encode_project(&project)).expect("round trip");
        assert!(decoded.tracks[0].soundfont.is_none());
    }

    #[test]
    fn pre_v28_track_loads_without_a_soundfont() {
        // A v27 track body ends at the row height; decoding it must not read
        // into the next record looking for a soundfont block.
        let mut w = FbWriter::new();
        encode_track_body_v27(&mut w);
        let bytes = w.into_bytes();
        let mut r = FbReader::new(&bytes);
        let decoded = decode_track(&mut r, 27).expect("v27 track decodes");
        assert!(decoded.soundfont.is_none());
        assert_eq!(decoded.id, "t1");
    }

    /// Writes exactly the track body a v27 writer produced — everything through
    /// the v17 row height, and nothing after it.
    fn encode_track_body_v27(w: &mut FbWriter) {
        w.write_str("t1");
        w.write_str("Audio 1");
        encode_track_type(w, ProjectTrackType::Audio);
        w.write_str("#56C7C9");
        w.write_f32(1.0);
        w.write_f32(0.0);
        w.write_bool(false);
        w.write_bool(false);
        w.write_bool(false);
        encode_input_monitor(w, InputMonitorMode::Off);
        let routing = TrackRouting::default();
        encode_track_input_routing(w, &routing.input);
        encode_track_output_routing(w, &routing.output);
        encode_track_audio_format(w, routing.audio_format);
        encode_track_midi_input_routing(w, &routing.midi_input);
        w.write_opt_u8(&None);
        w.write_bool(false);
        w.write_opt_str(&None);
        w.write_u32(0); // sends
        w.write_u32(0); // inserts
        w.write_u32(0); // automation lanes
        w.write_u32(0); // clips
        w.write_opt_f32(&None); // row height
    }

    fn track_with_clip(clip: ProjectClip) -> ProjectTrack {
        ProjectTrack {
            id: "t1".to_string(),
            name: "Audio 1".to_string(),
            track_type: ProjectTrackType::Audio,
            color_hex: "#56C7C9".to_string(),
            volume_norm: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            record_arm: false,
            input_monitor: InputMonitorMode::Off,
            routing: TrackRouting::default(),
            inserts: Vec::new(),
            automation_lanes: Vec::new(),
            clips: vec![clip],
            row_height_px: None,
            soundfont: None,
        }
    }

    #[test]
    fn stretch_survives_full_project_roundtrip() {
        let mut project = FutureboardProject::new("Stretch");
        project
            .tracks
            .push(track_with_clip(empty_clip_with_stretch(sample_stretch())));
        let bytes = encode_project(&project);
        let decoded = decode_project(&bytes).expect("decode");
        assert_eq!(decoded.tracks[0].clips[0].stretch, sample_stretch());
    }
}
