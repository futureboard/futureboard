use gpui::{App, Context};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::components::panel::{InspectorAudioInputChannel, InspectorAudioInputDevice};
use crate::components::timeline::timeline_state::{
    AudioClipStretchState, AudioImportState, ClipState, ClipType, MidiControllerLane,
    MidiNoteState, TrackAudioFormat, TrackInputRouting, TrackState, TrackType, MIN_NOTE_BEATS,
};
use crate::components::timeline::waveform_cache::{self, WaveformPeak};
use sphere_midi_service::MidiInputEvent;

use super::{audio_recording_preview_clip_id, RecordingPreviewUi, RecordingUiState, StudioLayout};
use DirectAudio::types::{JsRecordingTrackConfig, JsStartRecordingConfig};

/// Active recording-session UI state — the take's start position, the UI phase,
/// and the live growing-waveform preview. `StudioLayout` decomposition slice;
/// accessed from the recording + transport ops modules.
pub(crate) struct RecordingSessionState {
    /// Beat position when the current recording session started.
    pub start_beat: f32,
    /// Recording UI phase (Idle / Arming / Recording / …).
    pub ui_state: RecordingUiState,
    /// Live recording waveform preview (Part 1), keyed by track id. Holds one
    /// entry per armed audio track currently drawing a growing preview clip
    /// in the arrangement — every simultaneously-armed track gets its own
    /// entry, mirroring `midi.tracks` below for MIDI takes.
    pub preview: HashMap<String, RecordingPreviewUi>,
    /// Native MIDI/instrument track capture. This is UI/control-path state — MIDI
    /// hardware/virtual-keyboard events already arrive on the control router, so
    /// no realtime audio callback work is required.
    pub midi: Option<MidiRecordingTake>,
    /// MIDI preview snapshots are expensive because they mirror the growing
    /// take into timeline state. Event edges render immediately; held-note
    /// growth is capped to 30 Hz so screen capture cannot multiply full-take
    /// clones and broad timeline redraws.
    pub midi_preview_dirty: bool,
    pub midi_preview_updated_at: Instant,
    /// Invalidates an outstanding count-in timer when the user cancels/restarts.
    pub count_in_token: u64,
    /// Metronome state to restore after the temporary audible count-in.
    pub count_in_restore_metronome: Option<bool>,
}

impl Default for RecordingSessionState {
    fn default() -> Self {
        Self {
            start_beat: 0.0,
            ui_state: RecordingUiState::Idle,
            preview: HashMap::new(),
            midi: None,
            midi_preview_dirty: false,
            midi_preview_updated_at: Instant::now() - Duration::from_secs(1),
            count_in_token: 0,
            count_in_restore_metronome: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MidiRecordingTake {
    pub start_beat: f32,
    pub tracks: HashMap<String, MidiRecordingTrack>,
}

#[derive(Debug, Clone)]
pub(crate) struct MidiRecordingTrack {
    pub track_id: String,
    pub track_name: String,
    pub notes: Vec<MidiNoteState>,
    pub active_notes: HashMap<(u8, u8), ActiveMidiNote>,
}

fn midi_recording_preview_clip_id(track_id: &str) -> String {
    format!("__recording_midi_preview__:{track_id}")
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveMidiNote {
    pub pitch: u8,
    pub velocity: u8,
    pub start_beat: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct MidiRecordingResult {
    pub track_id: String,
    pub track_name: String,
    pub start_beat: f32,
    pub duration_beats: f32,
    pub notes: Vec<MidiNoteState>,
}

impl StudioLayout {
    pub(super) fn toggle_native_recording(&mut self, cx: &mut Context<Self>) {
        if matches!(self.recording.ui_state, RecordingUiState::CountingIn { .. }) {
            self.cancel_record_count_in(cx);
            return;
        }
        let recording = self.is_recording_active(cx);
        if recording {
            self.stop_native_recording(cx);
        } else {
            self.start_native_recording(cx);
        }
    }

    pub(super) fn start_native_recording(&mut self, cx: &mut Context<Self>) {
        let (count_in_enabled, count_in_bars, playing, bpm, beats_per_bar, target_beat) = {
            let timeline = self.timeline.read(cx);
            let metronome = &self.settings.read(cx).current.recording.metronome;
            (
                metronome.count_in_enabled,
                metronome.count_in_bars.clamp(1, 4),
                timeline.state.transport.playing,
                timeline.state.effective_bpm_at_playhead().max(1.0) as f32,
                timeline.state.time_signature_at_playhead().numerator.max(1) as f32,
                timeline.state.transport.playhead_beats.max(0.0),
            )
        };
        if count_in_enabled && !playing {
            self.begin_record_count_in(count_in_bars, bpm, beats_per_bar, target_beat, cx);
            return;
        }
        self.start_native_recording_now(cx);
    }

    fn begin_record_count_in(
        &mut self,
        bars: u32,
        bpm: f32,
        beats_per_bar: f32,
        target_beat: f32,
        cx: &mut Context<Self>,
    ) {
        self.recording.count_in_token = self.recording.count_in_token.wrapping_add(1);
        let token = self.recording.count_in_token;
        let restore_metronome = self.timeline.update(cx, |timeline, cx| {
            let previous = timeline.state.transport.metronome_enabled;
            timeline.state.transport.metronome_enabled = true;
            cx.notify();
            previous
        });
        self.recording.count_in_restore_metronome = Some(restore_metronome);
        self.recording.ui_state = RecordingUiState::CountingIn { bars };
        self.sync_metronome_controls(cx);

        let count_beats = bars as f32 * beats_per_bar;
        self.seek_native_playhead(cx, (target_beat - count_beats).max(0.0));
        self.start_native_playback(cx);
        cx.notify();

        let seconds = (count_beats * 60.0 / bpm.max(1.0)).max(0.01);
        let owner = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs_f32(seconds))
                .await;
            let _ = owner.update(cx, |this, cx| {
                if this.recording.count_in_token != token
                    || !matches!(this.recording.ui_state, RecordingUiState::CountingIn { .. })
                {
                    return;
                }
                this.stop_native_playback(cx);
                this.seek_native_playhead(cx, target_beat);
                this.restore_record_count_in_metronome(cx);
                this.start_native_recording_now(cx);
            });
        })
        .detach();
    }

    fn restore_record_count_in_metronome(&mut self, cx: &mut Context<Self>) {
        let Some(enabled) = self.recording.count_in_restore_metronome.take() else {
            return;
        };
        self.timeline.update(cx, |timeline, cx| {
            timeline.state.transport.metronome_enabled = enabled;
            cx.notify();
        });
        self.sync_metronome_controls(cx);
    }

    fn cancel_record_count_in(&mut self, cx: &mut Context<Self>) {
        self.recording.count_in_token = self.recording.count_in_token.wrapping_add(1);
        self.stop_native_playback(cx);
        self.restore_record_count_in_metronome(cx);
        self.recording.ui_state = RecordingUiState::Idle;
        cx.notify();
    }

    fn start_native_recording_now(&mut self, cx: &mut Context<Self>) {
        self.recording.ui_state = RecordingUiState::Preparing;
        cx.notify();

        // Clone the engine handle (cheap, Arc-backed) so the local `engine`
        // borrow targets `engine_handle` instead of `self`. That lets the
        // `save_before_recording` auto-save run *after* every record
        // precondition is validated (see below), rather than before — clicking
        // Record on a project that cannot start a take (no engine, no armed
        // audio track, device conflict) must not silently save the project.
        let Some(engine_handle) = self.audio_bridge.engine.clone() else {
            self.fail_recording_start("audio engine unavailable", cx);
            return;
        };
        let engine = &engine_handle;

        let input_devices: Vec<RecordingInputDevice> = engine
            .list_input_devices()
            .into_iter()
            .map(|d| RecordingInputDevice {
                id: d.id,
                name: d.name,
                channels: d.channels,
                is_default: d.is_default,
            })
            .collect();

        // Auto-arm the record target. Both the "R" shortcut and the transport
        // Record button route here; the most common reason Record appears to do
        // nothing is a selected-but-not-armed track, which then fails the armed
        // check below and reports only into `last_error`. When nothing is armed,
        // arm the selected track (if it can record) so the ordinary "click a
        // track, press Record" flow just works instead of silently failing.
        let already_armed = {
            let timeline = self.timeline.read(cx);
            timeline.state.tracks.iter().any(|t| {
                t.armed
                    && matches!(
                        t.track_type,
                        TrackType::Audio | TrackType::Midi | TrackType::Instrument
                    )
            })
        };
        if !already_armed {
            let arm_target = {
                let timeline = self.timeline.read(cx);
                timeline
                    .state
                    .selection
                    .selected_track_id
                    .clone()
                    .filter(|id| {
                        timeline
                            .state
                            .find_track(id)
                            .map(|t| {
                                matches!(
                                    t.track_type,
                                    TrackType::Audio | TrackType::Midi | TrackType::Instrument
                                )
                            })
                            .unwrap_or(false)
                    })
            };
            if let Some(id) = arm_target {
                self.timeline.update(cx, |timeline, cx| {
                    timeline.state.toggle_track_arm(&id);
                    cx.notify();
                });
            }
        }

        let (bpm, start_beat, sample_rate, input_device_name, midi_tracks) = {
            let timeline = self.timeline.read(cx);
            let settings = self.settings.read(cx);
            let audio_armed = timeline
                .state
                .tracks
                .iter()
                .any(|t| t.armed && t.track_type == TrackType::Audio);
            let midi_tracks = timeline
                .state
                .tracks
                .iter()
                .filter(|t| {
                    t.armed && matches!(t.track_type, TrackType::Midi | TrackType::Instrument)
                })
                .map(|track| MidiRecordingTrack {
                    track_id: track.id.clone(),
                    track_name: track.name.clone(),
                    notes: Vec::new(),
                    active_notes: HashMap::new(),
                })
                .collect::<Vec<_>>();
            if !audio_armed && midi_tracks.is_empty() {
                self.fail_recording_start("no armed audio or MIDI tracks", cx);
                return;
            }
            (
                timeline.state.bpm,
                timeline.state.transport.playhead_beats,
                self.current_audio_sample_rate(),
                settings.current.hardware.audio.device_in.clone(),
                midi_tracks,
            )
        };

        let selected_input_device =
            select_recording_input_device(&input_devices, &input_device_name);

        let (tracks, explicit_device_id, monitor_channels): (
            Vec<JsRecordingTrackConfig>,
            Option<String>,
            Vec<u32>,
        ) = {
            let timeline = self.timeline.read(cx);
            let mut explicit_device_id: Option<String> = None;
            let mut monitor_channels = Vec::new();
            let mut configs = Vec::new();
            for track in timeline
                .state
                .tracks
                .iter()
                .filter(|t| t.armed && t.track_type == TrackType::Audio)
            {
                let route = match recording_input_channels_checked(
                    track,
                    &input_devices,
                    selected_input_device,
                ) {
                    Ok(route) => route,
                    Err(error) => {
                        self.fail_recording_start(&error, cx);
                        return;
                    }
                };
                if let Some(device_id) = route.device_id.clone() {
                    match explicit_device_id.as_ref() {
                        Some(existing) if existing != &device_id => {
                            self.fail_recording_start(
                                "armed tracks use different input devices; record one input device at a time",
                                cx,
                            );
                            return;
                        }
                        None => explicit_device_id = Some(device_id),
                        _ => {}
                    }
                }
                if monitor_channels.is_empty() && track.input_monitor.is_active(track.armed) {
                    monitor_channels = route.channels.clone();
                }
                configs.push(JsRecordingTrackConfig {
                    track_id: track.id.clone(),
                    input_channels: route.channels,
                    name: track.name.clone(),
                });
            }
            (configs, explicit_device_id, monitor_channels)
        };

        if !tracks.is_empty() {
            let Some(_project_root) = self.project_folder.as_ref() else {
                self.fail_recording_start("save the project to a folder before recording", cx);
                return;
            };

            // Every audio-record precondition has now passed (engine present, at
            // least one armed audio track, input devices/channels resolved). Only
            // now honour `save_before_recording`: the auto-save must never fire
            // for a Record click that won't actually start an audio take.
            let save_before_recording = self
                .settings
                .read(cx)
                .current
                .recording
                .audio
                .save_before_recording;
            if save_before_recording && self.project_session.is_dirty {
                let Some(project_path) = self.project_session.project_file_path.clone() else {
                    self.fail_recording_start("save the project to a folder before recording", cx);
                    return;
                };
                if !self.do_save_project(&project_path, cx) {
                    self.fail_recording_start("could not save the project before recording", cx);
                    return;
                }
            }
        }

        let input_device_id = explicit_device_id.or_else(|| {
            selected_input_device
                .as_ref()
                .map(|device| device.id.clone())
                .or_else(|| resolve_input_device_id(engine, &input_device_name))
        });
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
            .to_string();
        let session_id = format!("rec-{timestamp}");
        let project_name = self.project_session.name.clone();

        if !midi_tracks.is_empty() {
            self.recording.midi = Some(MidiRecordingTake {
                start_beat: start_beat.max(0.0),
                tracks: midi_tracks
                    .into_iter()
                    .map(|track| (track.track_id.clone(), track))
                    .collect(),
            });
            self.recording.midi_preview_dirty = true;
            self.recording.midi_preview_updated_at = Instant::now() - Duration::from_secs(1);
        } else {
            self.recording.midi = None;
        }

        if !tracks.is_empty() {
            let Some(project_root) = self.project_folder.as_ref() else {
                self.recording.midi = None;
                self.fail_recording_start("save the project to a folder before recording", cx);
                return;
            };
            let config = JsStartRecordingConfig {
                project_root: project_root.to_string_lossy().into_owned(),
                project_name,
                session_id,
                timestamp,
                bpm: bpm.max(1.0) as f64,
                start_beat: start_beat.max(0.0) as f64,
                capture_on_transport: Some(true),
                sample_rate: sample_rate.max(1),
                input_device_id,
                tracks,
                monitor_mix: !monitor_channels.is_empty(),
                monitor_channels,
            };

            if let Err(error) = engine.start_recording(config) {
                self.recording.midi = None;
                self.fail_recording_start(&error.to_string(), cx);
                return;
            }
        }

        self.recording.start_beat = start_beat.max(0.0);
        if let Some(take) = self.recording.midi.as_ref() {
            let preview_tracks: Vec<(String, String)> = take
                .tracks
                .values()
                .map(|track| {
                    (
                        midi_recording_preview_clip_id(&track.track_id),
                        track.track_id.clone(),
                    )
                })
                .collect();
            let start_beat = self.recording.start_beat;
            let _ = self.timeline.update(cx, |timeline, cx| {
                for (clip_id, track_id) in &preview_tracks {
                    timeline
                        .state
                        .begin_midi_recording_preview_clip(clip_id, track_id, start_beat);
                }
                if !preview_tracks.is_empty() {
                    cx.notify();
                }
            });
        }
        self.audio_bridge.last_error = None;
        self.recording.ui_state = RecordingUiState::Recording;
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.state.transport.recording = true;
            cx.notify();
        });

        let playing = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|s| s.transport_playing)
            .unwrap_or(false);
        if !playing {
            self.start_native_playback(cx);
        }
    }

    pub(super) fn stop_native_recording(&mut self, cx: &mut Context<Self>) {
        if matches!(self.recording.ui_state, RecordingUiState::CountingIn { .. }) {
            self.cancel_record_count_in(cx);
            return;
        }
        self.stop_recording_transport_ui(cx);
        self.recording.ui_state = RecordingUiState::Finalizing;
        cx.notify();

        self.clear_midi_recording_previews(cx);
        let midi_results = self.finish_midi_recording_take(cx);
        let midi_committed = !midi_results.is_empty();

        if let Some(engine) = self.audio_bridge.engine.clone() {
            let engine_recording = engine.recording_status().active;
            if let Err(error) = engine.pause() {
                self.audio_bridge.last_error = Some(error.to_string());
                eprintln!("[audio] stop transport while recording failed: {error}");
            }
            self.audio_bridge.stats = Some(engine.stats());

            if engine_recording {
                let results = match engine.stop_recording() {
                    Ok(results) => results,
                    Err(error) => {
                        self.audio_bridge.last_error = Some(error.to_string());
                        self.recording.ui_state = RecordingUiState::Failed {
                            reason: error.to_string(),
                        };
                        eprintln!("[recording] stop failed: {error}");
                        return;
                    }
                };
                self.commit_recording_results(cx, results);
            }
        } else if midi_results.is_empty() {
            self.recording.ui_state = RecordingUiState::Failed {
                reason: "audio engine unavailable".to_string(),
            };
            cx.notify();
            return;
        }

        self.commit_midi_recording_results(cx, midi_results);

        if !matches!(self.recording.ui_state, RecordingUiState::Failed { .. }) {
            self.recording.ui_state = RecordingUiState::Idle;
            if midi_committed {
                self.audio_bridge.project_dirty = true;
                self.audio_bridge.media_dirty = true;
                self.schedule_audio_project_sync(cx, true, "recording_commit");
            }
            cx.notify();
        }
    }

    fn stop_recording_transport_ui(&mut self, cx: &mut Context<Self>) {
        let _ = self.timeline.update(cx, |timeline, cx| {
            let mut changed = false;
            if timeline.state.transport.recording {
                timeline.state.transport.recording = false;
                changed = true;
            }
            if timeline.state.transport.playing {
                timeline.state.transport.playing = false;
                changed = true;
            }
            if changed {
                cx.notify();
            }
        });
        self.finish_recording_preview(cx);
    }

    pub(super) fn capture_midi_record_event(
        &mut self,
        track_id: &str,
        event: &MidiInputEvent,
        cx: &App,
    ) {
        self.capture_midi_record_event_at(track_id, event, None, cx);
    }

    pub(super) fn capture_midi_record_event_at(
        &mut self,
        track_id: &str,
        event: &MidiInputEvent,
        event_beat: Option<f32>,
        cx: &App,
    ) {
        let current_beat =
            event_beat.unwrap_or_else(|| self.timeline.read(cx).state.transport.playhead_beats);
        let Some(take) = self.recording.midi.as_mut() else {
            return;
        };
        let relative_beat = (current_beat - take.start_beat).max(0.0);
        let Some(track) = take.tracks.get_mut(track_id) else {
            return;
        };

        match *event {
            MidiInputEvent::NoteOn {
                note,
                velocity,
                channel,
            } => {
                let pitch = note.min(127);
                let velocity = velocity.min(127);
                let channel = channel.min(15);
                let key = (channel, pitch);
                if let Some(active) = track.active_notes.remove(&key) {
                    push_recorded_midi_note(track, active, relative_beat);
                }
                track.active_notes.insert(
                    key,
                    ActiveMidiNote {
                        pitch,
                        velocity,
                        start_beat: relative_beat,
                    },
                );
            }
            MidiInputEvent::NoteOff { note, channel } => {
                let key = (channel.min(15), note.min(127));
                if let Some(active) = track.active_notes.remove(&key) {
                    push_recorded_midi_note(track, active, relative_beat);
                }
            }
            MidiInputEvent::AllNotesOff | MidiInputEvent::Panic => {
                close_all_recorded_midi_notes(track, relative_beat);
            }
            MidiInputEvent::ControlChange { .. } => {}
        }
        self.recording.midi_preview_dirty = true;
    }

    fn finish_midi_recording_take(&mut self, cx: &mut Context<Self>) -> Vec<MidiRecordingResult> {
        let end_beat = self.timeline.read(cx).state.transport.playhead_beats;
        let Some(mut take) = self.recording.midi.take() else {
            return Vec::new();
        };
        let relative_end = (end_beat - take.start_beat).max(0.0);
        let mut results = Vec::new();
        for (_, mut track) in take.tracks.drain() {
            close_all_recorded_midi_notes(&mut track, relative_end);
            if track.notes.is_empty() {
                continue;
            }
            let note_end = track
                .notes
                .iter()
                .map(|note| note.start + note.duration)
                .fold(0.0_f32, f32::max);
            results.push(MidiRecordingResult {
                track_id: track.track_id,
                track_name: track.track_name,
                start_beat: take.start_beat,
                duration_beats: relative_end.max(note_end).max(MIN_NOTE_BEATS),
                notes: track.notes,
            });
        }
        results
    }

    fn commit_midi_recording_results(
        &mut self,
        cx: &mut Context<Self>,
        results: Vec<MidiRecordingResult>,
    ) {
        if results.is_empty() {
            return;
        }

        self.timeline.update(cx, |timeline, cx| {
            let mut selected_clip_ids = Vec::new();
            let mut selected_track_id = None;
            for result in results {
                let clip_id = timeline.state.next_clip_id();
                let clip = ClipState {
                    id: clip_id.clone(),
                    name: format!("{} MIDI Recording", result.track_name),
                    start_beat: result.start_beat.max(0.0),
                    duration_beats: result.duration_beats.max(MIN_NOTE_BEATS),
                    source_duration_seconds: None,
                    offset_beats: 0.0,
                    gain: 1.0,
                    clip_type: ClipType::Midi {
                        notes: result.notes,
                        controller_lanes: Vec::<MidiControllerLane>::new(),
                        sysex_events: Vec::new(),
                        articulations: Vec::new(),
                    },
                    muted: false,
                    audio_import: AudioImportState::default(),
                    stretch: AudioClipStretchState::default(),
                };
                if let Some(track) = timeline
                    .state
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == result.track_id)
                {
                    track.clips.push(clip);
                    selected_track_id = Some(track.id.clone());
                    selected_clip_ids.push(clip_id);
                }
            }
            if !selected_clip_ids.is_empty() {
                timeline.state.selection.selected_track_id = selected_track_id;
                timeline.state.selection.selected_clip_ids = selected_clip_ids;
                cx.notify();
            }
        });
    }

    fn commit_recording_results(
        &mut self,
        cx: &mut Context<Self>,
        results: Vec<DirectAudio::types::JsRecordingResult>,
    ) {
        let bpm = self.timeline.read(cx).state.bpm;
        let timeline = self.timeline.clone();
        let (generate_waveforms, recording_offset_ms) = {
            let settings = self.settings.read(cx);
            (
                settings
                    .current
                    .recording
                    .audio
                    .generate_waveform_after_record,
                settings.current.recording.audio.recording_offset_ms,
            )
        };
        let recording_offset_beats = recording_offset_ms as f32 / 1000.0 * bpm / 60.0;
        let mut import_paths: Vec<(PathBuf, String)> = Vec::new();
        let mut failed_tracks: Vec<String> = Vec::new();

        let _ = self.timeline.update(cx, |timeline, cx| {
            for result in &results {
                if !result.success {
                    failed_tracks.push(format!(
                        "{}: {}",
                        result.track_id,
                        result.error.as_deref().unwrap_or("unknown error")
                    ));
                    eprintln!(
                        "[recording] track {} failed: {}",
                        result.track_id,
                        result.error.as_deref().unwrap_or("unknown error")
                    );
                    continue;
                }
                let clip_name = Path::new(&result.relative_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Recording")
                    .to_string();
                // Auto latency compensation: pull the take earlier by the
                // engine-measured round-trip so an overdub lands where the
                // performer heard it. The manual `recording_offset_ms` is layered
                // on top for per-rig fine-tuning (it may be negative or positive).
                let latency_beats = result.latency_seconds as f32 * bpm / 60.0;
                let placed_beat =
                    (result.start_beat as f32 - latency_beats + recording_offset_beats).max(0.0);
                if latency_beats != 0.0
                    && std::env::var_os("FUTUREBOARD_RECORDING_DEBUG").is_some()
                {
                    eprintln!(
                        "[recording] latency comp track={} raw_start={:.3} latency={:.1}ms (-{:.3} beats) manual_offset={:.3} beats -> start={:.3}",
                        result.track_id,
                        result.start_beat,
                        result.latency_seconds * 1000.0,
                        latency_beats,
                        recording_offset_beats,
                        placed_beat
                    );
                }
                let clip_id = timeline.state.insert_recorded_clip(
                    &result.track_id,
                    result.file_path.clone(),
                    clip_name,
                    placed_beat,
                    result.duration_seconds,
                    bpm,
                );
                if generate_waveforms {
                    import_paths.push((PathBuf::from(&result.file_path), result.file_path.clone()));
                }
                eprintln!(
                    "[recording] clip created id={clip_id} track={} path={}",
                    result.track_id, result.relative_path
                );
            }
            cx.notify();
        });

        if !failed_tracks.is_empty() {
            let reason = format!("recording finalize failed ({})", failed_tracks.join("; "));
            self.audio_bridge.last_error = Some(reason.clone());
            self.recording.ui_state = RecordingUiState::Failed { reason };
            cx.notify();
            return;
        }

        self.audio_bridge.project_dirty = true;
        self.audio_bridge.media_dirty = true;
        self.schedule_audio_project_sync(cx, true, "recording_commit");

        for (path, path_key) in import_paths {
            self.spawn_timeline_audio_import_jobs(cx, timeline.clone(), path, path_key);
        }
    }

    /// Poll the engine's per-track recording preview rings and keep every
    /// armed audio track's temporary preview clip + streamed waveform in sync
    /// (Part 1, multi-track). Called every audio tick.
    pub(super) fn update_recording_preview(&mut self, cx: &mut Context<Self>) {
        self.update_midi_recording_previews(cx);
        if !self.timeline.read(cx).state.transport.recording {
            if !self.recording.preview.is_empty()
                && std::env::var_os("FUTUREBOARD_RECORDING_DEBUG").is_some()
            {
                eprintln!("[RecordingPreviewUI] ignored_late_peaks transport_recording=false");
            }
            self.finish_recording_preview(cx);
            return;
        }
        let Some(engine) = self.audio_bridge.engine.clone() else {
            return;
        };
        // Authoritative per-track list: the engine resolved exactly which
        // armed tracks (and their channels) are feeding a live preview ring
        // when the take started — no need to re-derive it from timeline
        // arm state here.
        let track_infos = engine.recording_preview_tracks();
        if track_infos.is_empty() {
            self.finish_recording_preview(cx);
            return;
        }

        // Drop any tracked preview whose track id fell out of the engine's
        // active set (take ended) or whose take id changed (stale from a
        // previous take reusing the same track).
        let stale_track_ids: Vec<String> = self
            .recording
            .preview
            .iter()
            .filter(|(id, p)| {
                track_infos
                    .iter()
                    .find(|t| &t.track_id == *id)
                    .map(|t| t.recording_id as u64 != p.recording_id)
                    .unwrap_or(true)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for track_id in stale_track_ids {
            self.teardown_recording_preview_track(&track_id, cx);
        }

        // (Re)initialize the preview clip for any track not yet tracked.
        for info in &track_infos {
            if self.recording.preview.contains_key(&info.track_id) {
                continue;
            }
            let rec_id = info.recording_id as u64;
            let start_beat = self.recording.start_beat.max(0.0);
            let clip_id = audio_recording_preview_clip_id(&info.track_id);
            waveform_cache::clear_recording_preview(&clip_id);
            if std::env::var_os("FUTUREBOARD_RECORDING_DEBUG").is_some() {
                eprintln!(
                    "[RecordingPreviewUI] started id={rec_id} track_id={} clip_id={clip_id}",
                    info.track_id
                );
            }
            let (cid, tid) = (clip_id.clone(), info.track_id.clone());
            self.timeline.update(cx, |timeline, cx| {
                timeline
                    .state
                    .begin_recording_preview_clip(&cid, &tid, start_beat);
                cx.notify();
            });
            self.recording.preview.insert(
                info.track_id.clone(),
                RecordingPreviewUi {
                    clip_id,
                    recording_id: rec_id,
                    track_id: info.track_id.clone(),
                    start_beat,
                    sample_rate: info.sample_rate,
                    peaks_per_second: info.peaks_per_second.max(1),
                    drained: 0,
                    peaks: Vec::new(),
                },
            );
        }

        let bpm = {
            let timeline = self.timeline.read(cx);
            timeline.state.bpm
        };

        // Drain newly produced peaks per track and republish each waveform.
        let mut length_updates: Vec<(String, f32)> = Vec::new();
        for info in &track_infos {
            let Some(p) = self.recording.preview.get_mut(&info.track_id) else {
                continue;
            };
            let new = engine
                .drain_recording_preview_peaks_for_track(info.track_id.clone(), p.drained as f64);
            if !new.is_empty() {
                p.drained += new.len() as u64;
                if std::env::var_os("FUTUREBOARD_RECORDING_DEBUG").is_some() {
                    eprintln!(
                        "[RecordingPreviewUI] peaks id={} track_id={} count={}",
                        p.recording_id,
                        p.track_id,
                        new.len()
                    );
                }
                p.peaks.extend(new.into_iter().map(|q| WaveformPeak {
                    min: q.min as f32,
                    max: q.max as f32,
                }));
                let wf = waveform_cache::preview_from_recording_peaks(
                    &p.peaks,
                    p.sample_rate,
                    p.peaks_per_second,
                );
                waveform_cache::set_recording_preview(&p.clip_id, std::sync::Arc::new(wf));
            }
            let length_seconds = p.peaks.len() as f64 / p.peaks_per_second.max(1) as f64;
            let length_beats = (length_seconds * bpm.max(1.0) as f64 / 60.0) as f32;
            length_updates.push((p.clip_id.clone(), length_beats));
        }
        if !length_updates.is_empty() {
            self.timeline.update(cx, |timeline, cx| {
                let mut changed = false;
                for (clip_id, length_beats) in length_updates {
                    changed |= timeline
                        .state
                        .set_recording_preview_clip_length(&clip_id, length_beats);
                }
                if changed {
                    cx.notify();
                }
            });
        }
    }

    /// Tear down one track's live recording preview clip + registry entry.
    fn teardown_recording_preview_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(preview) = self.recording.preview.remove(track_id) else {
            return;
        };
        if std::env::var_os("FUTUREBOARD_RECORDING_DEBUG").is_some() {
            eprintln!(
                "[RecordingPreviewUI] finished id={} track_id={track_id}",
                preview.recording_id
            );
        }
        waveform_cache::clear_recording_preview(&preview.clip_id);
        let clip_id = preview.clip_id;
        self.timeline.update(cx, |timeline, cx| {
            if timeline.state.remove_recording_preview_clip(&clip_id) {
                cx.notify();
            }
        });
    }

    /// Tear down every tracked live recording preview clip + registry entry.
    pub(super) fn finish_recording_preview(&mut self, cx: &mut Context<Self>) {
        let track_ids: Vec<String> = self.recording.preview.keys().cloned().collect();
        for track_id in track_ids {
            self.teardown_recording_preview_track(&track_id, cx);
        }
    }

    /// Mirror captured MIDI notes into temporary arrangement clips. This runs
    /// on the UI/control tick, never in the audio callback, and includes held
    /// notes with a growing duration so the user sees a note immediately on
    /// NoteOn rather than only after NoteOff/stop.
    fn update_midi_recording_previews(&mut self, cx: &mut Context<Self>) {
        const MIDI_PREVIEW_INTERVAL: Duration = Duration::from_millis(33);
        if self.recording.midi.is_none()
            || (!self.recording.midi_preview_dirty
                && self.recording.midi_preview_updated_at.elapsed() < MIDI_PREVIEW_INTERVAL)
        {
            return;
        }
        self.recording.midi_preview_dirty = false;
        self.recording.midi_preview_updated_at = Instant::now();
        let now = self.timeline.read(cx).state.transport.playhead_beats;
        let previews = self.recording.midi.as_ref().map(|take| {
            let relative_beat = (now - take.start_beat).max(0.0);
            take.tracks
                .values()
                .map(|track| {
                    let mut notes = track.notes.clone();
                    notes.extend(track.active_notes.values().map(|active| {
                        MidiNoteState::new(
                            active.pitch,
                            active.start_beat.max(0.0),
                            (relative_beat - active.start_beat).max(MIN_NOTE_BEATS),
                            active.velocity,
                        )
                    }));
                    (
                        midi_recording_preview_clip_id(&track.track_id),
                        relative_beat.max(MIN_NOTE_BEATS),
                        notes,
                    )
                })
                .collect::<Vec<_>>()
        });
        let Some(previews) = previews else {
            return;
        };
        let _ = self.timeline.update(cx, |timeline, cx| {
            let mut changed = false;
            for (clip_id, duration_beats, notes) in previews {
                changed |= timeline.state.update_midi_recording_preview_clip(
                    &clip_id,
                    duration_beats,
                    notes,
                );
            }
            if changed {
                cx.notify();
            }
        });
    }

    fn clear_midi_recording_previews(&mut self, cx: &mut Context<Self>) {
        let preview_ids = self
            .recording
            .midi
            .as_ref()
            .map(|take| {
                take.tracks
                    .keys()
                    .map(|track_id| midi_recording_preview_clip_id(track_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if preview_ids.is_empty() {
            return;
        }
        let _ = self.timeline.update(cx, |timeline, cx| {
            let mut changed = false;
            for clip_id in &preview_ids {
                changed |= timeline.state.remove_recording_preview_clip(clip_id);
            }
            if changed {
                cx.notify();
            }
        });
    }

    fn fail_recording_start(&mut self, message: &str, cx: &mut Context<Self>) {
        self.audio_bridge.last_error = Some(message.to_string());
        self.recording.ui_state = RecordingUiState::Failed {
            reason: message.to_string(),
        };
        self.recording.midi = None;
        eprintln!("[recording] start blocked: {message}");
        cx.notify();
    }
}

fn push_recorded_midi_note(track: &mut MidiRecordingTrack, active: ActiveMidiNote, end_beat: f32) {
    let duration = (end_beat - active.start_beat).max(MIN_NOTE_BEATS);
    track.notes.push(MidiNoteState::new(
        active.pitch,
        active.start_beat.max(0.0),
        duration,
        active.velocity,
    ));
}

fn close_all_recorded_midi_notes(track: &mut MidiRecordingTrack, end_beat: f32) {
    let active_notes = std::mem::take(&mut track.active_notes);
    for active in active_notes.into_values() {
        push_recorded_midi_note(track, active, end_beat);
    }
}

impl StudioLayout {
    /// Real channel topology for the selected global input device. ASIO data is
    /// sourced from the open duplex session; merely installed drivers remain at
    /// zero channels until opened and are never instantiated by this query.
    pub(super) fn selected_input_device_model(
        &self,
        cx: &Context<Self>,
    ) -> Option<InspectorAudioInputDevice> {
        let engine = self.audio_bridge.engine.as_ref()?;
        let wanted = self
            .settings
            .read(cx)
            .current
            .hardware
            .audio
            .device_in
            .clone();
        let devices = engine.list_input_devices();
        let selected_index = (!wanted.trim().is_empty())
            .then(|| {
                devices
                    .iter()
                    .position(|device| device.name == wanted || device.id == wanted)
            })
            .flatten()
            .or_else(|| devices.iter().position(|device| device.is_default))
            .or_else(|| (!devices.is_empty()).then_some(0))?;
        let device = devices.into_iter().nth(selected_index)?;
        Some(InspectorAudioInputDevice {
            id: device.id,
            display_name: device.name,
            backend: device.backend,
            channels: device
                .channel_info
                .into_iter()
                .map(|channel| InspectorAudioInputChannel {
                    index: channel.index,
                    name: channel.name,
                    active: channel.active,
                    sample_format: channel.sample_format,
                    stereo_pair: channel.stereo_pair,
                    label: channel.label,
                })
                .collect(),
        })
    }

    /// Legacy Add Track dialog adapter. Inspector routing uses the richer model
    /// above; this keeps the existing dialog contract until its channel picker
    /// is migrated.
    pub(super) fn selected_input_device_channels(
        &self,
        cx: &Context<Self>,
    ) -> Option<(String, u32)> {
        self.selected_input_device_model(cx)
            .map(|device| (device.id, device.channels.len() as u32))
    }

    /// `(device name, output channel count)` for the currently selected global
    /// output device, used to populate hardware output routes in the Inspector.
    pub(super) fn selected_output_device_channels(
        &self,
        cx: &Context<Self>,
    ) -> Option<(String, u32)> {
        let engine = self.audio_bridge.engine.as_ref()?;
        let wanted = self
            .settings
            .read(cx)
            .current
            .hardware
            .audio
            .device_out
            .clone();
        let devices = engine.list_output_devices();
        if !wanted.trim().is_empty() {
            if let Some(d) = devices.iter().find(|d| d.name == wanted || d.id == wanted) {
                return Some((d.name.clone(), d.channels));
            }
        }
        devices
            .iter()
            .find(|d| d.is_default)
            .or_else(|| devices.first())
            .map(|d| (d.name.clone(), d.channels))
    }
}

#[derive(Clone, Debug)]
struct RecordingInputDevice {
    id: String,
    name: String,
    channels: u32,
    is_default: bool,
}

#[derive(Clone, Debug)]
struct RecordingInputRoute {
    device_id: Option<String>,
    channels: Vec<u32>,
}

fn recording_input_channels_checked(
    track: &TrackState,
    devices: &[RecordingInputDevice],
    selected_device: Option<&RecordingInputDevice>,
) -> Result<RecordingInputRoute, String> {
    match &track.routing.input {
        // A fresh audio track intentionally starts with no *explicit* input
        // route. The live-input engine already resolves that state to the
        // selected/default device while a track is armed; recording must use
        // the same effective route or the normal Arm -> Record workflow fails
        // before the input stream is ever opened.
        TrackInputRouting::None | TrackInputRouting::AllInputs => {
            let Some(device) = selected_device else {
                return Err("no input device selected or available".to_string());
            };
            let channels = match track.routing.audio_format {
                TrackAudioFormat::Mono => vec![0],
                TrackAudioFormat::Stereo => vec![0, 1],
            };
            validate_channels(&track.name, device, &channels)?;
            Ok(RecordingInputRoute {
                device_id: Some(device.id.clone()),
                channels,
            })
        }
        TrackInputRouting::AudioDeviceChannel { device_id, channel } => {
            let device = find_recording_input_device(devices, device_id).ok_or_else(|| {
                format!("{} input device is unavailable: {}", track.name, device_id)
            })?;
            let channels = vec![*channel];
            validate_channels(&track.name, device, &channels)?;
            Ok(RecordingInputRoute {
                device_id: Some(device.id.clone()),
                channels,
            })
        }
        TrackInputRouting::AudioDeviceChannels {
            device_id,
            channels,
        } => {
            if channels.is_empty() {
                return Err(format!("{} has no input channels selected.", track.name));
            }
            match track.routing.audio_format {
                TrackAudioFormat::Mono if channels.len() != 1 => {
                    return Err(format!(
                        "{} is mono but has a stereo/multi input route. Choose one input channel.",
                        track.name
                    ));
                }
                TrackAudioFormat::Stereo if !(1..=2).contains(&channels.len()) => {
                    return Err(format!(
                        "{} is stereo but has an incompatible input route. Choose one input channel or a stereo input pair.",
                        track.name
                    ));
                }
                _ => {}
            }
            let device = find_recording_input_device(devices, device_id).ok_or_else(|| {
                format!("{} input device is unavailable: {}", track.name, device_id)
            })?;
            validate_channels(&track.name, device, channels)?;
            Ok(RecordingInputRoute {
                device_id: Some(device.id.clone()),
                channels: channels.clone(),
            })
        }
        TrackInputRouting::MidiDevice { .. } => Err(format!(
            "{} has a MIDI input route; choose an audio input channel before recording.",
            track.name
        )),
    }
}

fn validate_channels(
    track_name: &str,
    device: &RecordingInputDevice,
    channels: &[u32],
) -> Result<(), String> {
    for channel in channels {
        if *channel >= device.channels {
            return Err(format!(
                "{} input channel {} is unavailable on {}.",
                track_name,
                channel + 1,
                device.name
            ));
        }
    }
    Ok(())
}

fn select_recording_input_device<'a>(
    devices: &'a [RecordingInputDevice],
    wanted: &str,
) -> Option<&'a RecordingInputDevice> {
    if !wanted.trim().is_empty() {
        if let Some(device) = find_recording_input_device(devices, wanted) {
            return Some(device);
        }
    }
    devices
        .iter()
        .find(|device| device.is_default)
        .or_else(|| devices.first())
}

fn find_recording_input_device<'a>(
    devices: &'a [RecordingInputDevice],
    id_or_name: &str,
) -> Option<&'a RecordingInputDevice> {
    devices
        .iter()
        .find(|device| device.id == id_or_name || device.name == id_or_name)
}

fn resolve_input_device_id(engine: &DirectAudio::AudioEngine, device_name: &str) -> Option<String> {
    if device_name.trim().is_empty() {
        return None;
    }
    engine
        .list_input_devices()
        .into_iter()
        .find(|d| d.name == device_name || d.id == device_name)
        .map(|d| d.id)
        .or_else(|| Some(device_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::timeline::timeline_state::TimelineState;

    fn input_device(channels: u32) -> RecordingInputDevice {
        RecordingInputDevice {
            id: "default-input".to_string(),
            name: "Default Input".to_string(),
            channels,
            is_default: true,
        }
    }

    #[test]
    fn fresh_armed_stereo_track_records_from_the_default_input() {
        let mut timeline = TimelineState::default();
        let track_id = timeline.create_audio_track();
        let track = timeline
            .find_track(&track_id)
            .expect("new audio track should exist");
        assert_eq!(track.routing.input, TrackInputRouting::None);
        assert_eq!(track.routing.audio_format, TrackAudioFormat::Stereo);

        let device = input_device(2);
        let route =
            recording_input_channels_checked(track, std::slice::from_ref(&device), Some(&device))
                .expect("fresh armed-track route should resolve to the default input");

        assert_eq!(route.device_id.as_deref(), Some("default-input"));
        assert_eq!(route.channels, vec![0, 1]);
    }

    #[test]
    fn recording_channel_count_follows_track_audio_format() {
        let mut timeline = TimelineState::default();
        let track_id = timeline.create_audio_track();
        let mut track = timeline.find_track(&track_id).unwrap().clone();
        let device = input_device(4);

        track.routing.input = TrackInputRouting::AudioDeviceChannel {
            device_id: device.id.clone(),
            channel: 2,
        };
        let mono_route =
            recording_input_channels_checked(&track, std::slice::from_ref(&device), Some(&device))
                .expect("stereo tracks may record one input channel");
        assert_eq!(mono_route.channels, vec![2]);

        track.routing.input = TrackInputRouting::AudioDeviceChannels {
            device_id: device.id.clone(),
            channels: vec![1, 2],
        };
        let stereo_route =
            recording_input_channels_checked(&track, std::slice::from_ref(&device), Some(&device))
                .expect("stereo tracks may record two input channels");
        assert_eq!(stereo_route.channels, vec![1, 2]);

        track.routing.audio_format = TrackAudioFormat::Mono;
        assert!(recording_input_channels_checked(
            &track,
            std::slice::from_ref(&device),
            Some(&device),
        )
        .is_err());
    }
}
