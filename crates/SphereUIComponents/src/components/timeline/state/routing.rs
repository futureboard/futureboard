use super::*;

pub use crate::project::InputMonitorMode;

/// Whether a track is a user-created Bus/Return that may receive normal track
/// outputs and aux sends. VSTi multi-output child strips deliberately use the
/// `Bus` type so the engine can mix them, but they are runtime-derived mixer
/// channels rather than project routing tracks and must never appear as normal
/// Send/Output destinations.
pub fn is_project_routing_track(track: &TrackState) -> bool {
    track.track_type.is_routing() && !is_vsti_output_child_track_id(&track.id)
}

/// A single aux send from this track to a Bus/Return track (Phase 3). The
/// runtime sums `gain_db`-scaled signal into the target's input. UI stores the
/// descriptor; DirectAudio owns the realtime accumulation.
#[derive(Debug, Clone, PartialEq)]
pub struct SendSlotState {
    pub id: String,
    /// Id of the destination Bus/Return track.
    pub target_track_id: String,
    /// Display label for the destination (resolved at edit time; refreshed
    /// from the track list on render).
    pub target_name: String,
    pub enabled: bool,
    /// `true` = tap before the source track fader; `false` = post-fader.
    /// Realtime currently honours post-fader only (pre-fader is a refinement).
    pub pre_fader: bool,
    pub gain_db: f32,
}

impl SendSlotState {
    /// Linear send gain from `gain_db` (clamped to a sane range).
    pub fn gain_linear(&self) -> f32 {
        10f32.powf(self.gain_db.clamp(-60.0, 6.0) / 20.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackInputRouting {
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

impl TrackInputRouting {
    pub fn label(&self) -> String {
        match self {
            Self::None => "None".to_string(),
            Self::AllInputs => "All Inputs".to_string(),
            Self::AudioDeviceChannel { device_id, channel } => {
                format!("{device_id} ch {}", channel + 1)
            }
            Self::AudioDeviceChannels {
                device_id,
                channels,
            } => {
                let labels = channels
                    .iter()
                    .map(|channel| (channel + 1).to_string())
                    .collect::<Vec<_>>()
                    .join("+");
                format!("{device_id} ch {labels}")
            }
            Self::MidiDevice { device_id } => device_id.clone(),
        }
    }

    pub fn is_compatible_with_audio_format(&self, audio_format: TrackAudioFormat) -> bool {
        match self {
            Self::None | Self::AllInputs | Self::MidiDevice { .. } => true,
            Self::AudioDeviceChannel { .. } => true,
            Self::AudioDeviceChannels { channels, .. } => match audio_format {
                TrackAudioFormat::Mono => channels.len() == 1,
                TrackAudioFormat::Stereo => {
                    channels.len() == 1 || (channels.len() == 2 && channels[0] != channels[1])
                }
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackOutputRouting {
    Main,
    Bus {
        bus_id: String,
    },
    HardwareOutput {
        device_id: String,
        channel: u32,
    },
    /// A MIDI track's notes/controllers are redirected to the named
    /// Instrument track's own plugin instead of that instrument's own clips.
    /// Only meaningful on `TrackType::Midi` tracks; see
    /// `TimelineState::effective_instrument_track_id`.
    Instrument {
        track_id: String,
    },
    None,
}

impl TrackOutputRouting {
    pub fn label(&self) -> String {
        match self {
            Self::Main => "Main".to_string(),
            Self::Bus { bus_id } => bus_id.clone(),
            Self::HardwareOutput { device_id, channel } => {
                format!("{device_id} ch {}", channel + 1)
            }
            // Callers that know the live track list should prefer
            // `panel::midi_output_combo_label`, which resolves the target's
            // display name; this is the id-only fallback.
            Self::Instrument { track_id } => format!("Instrument - {track_id}"),
            Self::None => "None".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackAudioFormat {
    Mono,
    Stereo,
}

impl TrackAudioFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::Stereo => "Stereo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackMidiInputRouting {
    None,
    AllInputs,
    MidiDevice { device_id: String },
}

impl TrackMidiInputRouting {
    pub fn label(&self) -> String {
        match self {
            Self::None => "None".to_string(),
            Self::AllInputs => "All MIDI Inputs".to_string(),
            Self::MidiDevice { device_id } => device_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRoutingState {
    pub input: TrackInputRouting,
    pub output: TrackOutputRouting,
    pub audio_format: TrackAudioFormat,
    pub midi_input: TrackMidiInputRouting,
    /// `None` means All channels. `Some` is clamped to 1..=16 by mutation
    /// helpers and project-load conversion.
    pub midi_channel: Option<u8>,
    /// Which incoming MIDI channels this track listens to. Model only in this
    /// pass — not yet enforced on the recording input path.
    pub midi_input_filter: MidiInputChannelFilter,
    /// `true` plays each note back on its own channel ([`MidiOutputChannelMode::PerNote`]);
    /// `false` (default) forces every note onto `midi_channel` (or channel 1),
    /// matching the pre-existing single-channel-per-track behavior.
    pub midi_output_per_note: bool,
}

impl TrackRoutingState {
    /// The effective output channel policy, derived from `midi_channel` /
    /// `midi_output_per_note` so there is exactly one field driving the
    /// existing channel selector UI and no duplicated state to fall out of
    /// sync.
    pub fn output_channel_mode(&self) -> MidiOutputChannelMode {
        if self.midi_output_per_note {
            MidiOutputChannelMode::PerNote
        } else {
            MidiOutputChannelMode::Fixed(MidiChannel::from_ui(self.midi_channel.unwrap_or(1)))
        }
    }

    /// Channel newly drawn notes on this track should default to.
    pub fn default_note_channel(&self) -> MidiChannel {
        MidiChannel::from_ui(self.midi_channel.unwrap_or(1))
    }
}

impl TrackRoutingState {
    pub fn for_track_type(track_type: TrackType) -> Self {
        match track_type {
            TrackType::Audio => Self {
                input: TrackInputRouting::None,
                output: TrackOutputRouting::Main,
                audio_format: TrackAudioFormat::Stereo,
                midi_input: TrackMidiInputRouting::None,
                midi_channel: None,
                midi_input_filter: MidiInputChannelFilter::All,
                midi_output_per_note: false,
            },
            TrackType::Instrument => Self {
                input: TrackInputRouting::None,
                output: TrackOutputRouting::Main,
                audio_format: TrackAudioFormat::Stereo,
                midi_input: TrackMidiInputRouting::AllInputs,
                midi_channel: None,
                midi_input_filter: MidiInputChannelFilter::All,
                midi_output_per_note: false,
            },
            TrackType::Midi => Self {
                input: TrackInputRouting::None,
                output: TrackOutputRouting::None,
                audio_format: TrackAudioFormat::Stereo,
                midi_input: TrackMidiInputRouting::AllInputs,
                midi_channel: None,
                midi_input_filter: MidiInputChannelFilter::All,
                midi_output_per_note: false,
            },
            TrackType::Bus | TrackType::Return | TrackType::Group => Self {
                input: TrackInputRouting::None,
                output: TrackOutputRouting::Main,
                audio_format: TrackAudioFormat::Stereo,
                midi_input: TrackMidiInputRouting::None,
                midi_channel: None,
                midi_input_filter: MidiInputChannelFilter::All,
                midi_output_per_note: false,
            },
            TrackType::Master => Self {
                input: TrackInputRouting::None,
                output: TrackOutputRouting::Main,
                audio_format: TrackAudioFormat::Stereo,
                midi_input: TrackMidiInputRouting::None,
                midi_channel: None,
                midi_input_filter: MidiInputChannelFilter::All,
                midi_output_per_note: false,
            },
        }
    }
}

impl TimelineState {
    pub fn set_track_input_routing(&mut self, track_id: &str, input: TrackInputRouting) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if !input.is_compatible_with_audio_format(t.routing.audio_format) {
                return false;
            }
            if t.routing.input != input {
                if routing_debug_enabled() {
                    eprintln!(
                        "[routing] input track={} old={:?} new={:?}",
                        track_id, t.routing.input, input
                    );
                }
                t.routing.input = input;
                return true;
            }
        }
        false
    }

    /// Resolve which track's plugin instance should actually receive a
    /// track's MIDI events during playback/preview: an Instrument track
    /// plays its own clips; a MIDI track routed via
    /// `TrackOutputRouting::Instrument` plays through that target instead
    /// (only while the target still exists and is still an Instrument
    /// track — a stale/retyped target yields `None`, i.e. silence, rather
    /// than guessing a different destination).
    pub fn effective_instrument_track_id(&self, track_id: &str) -> Option<String> {
        let track = self.tracks.iter().find(|t| t.id == track_id)?;
        match track.track_type {
            TrackType::Instrument => Some(track.id.clone()),
            TrackType::Midi => match &track.routing.output {
                TrackOutputRouting::Instrument {
                    track_id: target_id,
                } => self
                    .tracks
                    .iter()
                    .find(|t| t.id == *target_id && t.track_type == TrackType::Instrument)
                    .map(|t| t.id.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn set_track_output_routing(&mut self, track_id: &str, output: TrackOutputRouting) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.routing.output != output {
                if routing_debug_enabled() {
                    eprintln!(
                        "[routing] output track={} old={:?} new={:?}",
                        track_id, t.routing.output, output
                    );
                }
                t.routing.output = output;
                return true;
            }
        }
        false
    }

    pub fn set_track_audio_format(
        &mut self,
        track_id: &str,
        audio_format: TrackAudioFormat,
    ) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.routing.audio_format != audio_format {
                if routing_debug_enabled() {
                    eprintln!(
                        "[routing] audio_format track={} old={:?} new={:?}",
                        track_id, t.routing.audio_format, audio_format
                    );
                }
                t.routing.audio_format = audio_format;
                if !t
                    .routing
                    .input
                    .is_compatible_with_audio_format(audio_format)
                {
                    t.routing.input = TrackInputRouting::None;
                }
                return true;
            }
        }
        false
    }

    pub fn set_track_midi_input(
        &mut self,
        track_id: &str,
        midi_input: TrackMidiInputRouting,
    ) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.routing.midi_input != midi_input {
                if routing_debug_enabled() {
                    eprintln!(
                        "[routing] midi_input track={} old={:?} new={:?}",
                        track_id, t.routing.midi_input, midi_input
                    );
                }
                t.routing.midi_input = midi_input;
                return true;
            }
        }
        false
    }

    pub fn set_track_midi_channel(&mut self, track_id: &str, channel: Option<u8>) -> bool {
        let channel = channel.map(|ch| ch.clamp(1, 16));
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.routing.midi_channel != channel {
                if routing_debug_enabled() {
                    eprintln!(
                        "[routing] midi_channel track={} old={:?} new={:?}",
                        track_id, t.routing.midi_channel, channel
                    );
                }
                t.routing.midi_channel = channel;
                return true;
            }
        }
        false
    }

    /// Set the track's output channel policy (see [`TrackRoutingState::output_channel_mode`]).
    /// Returns `true` if it changed — callers should panic/all-notes-off the
    /// track afterwards so notes already sounding on the old channel don't stick.
    pub fn set_track_midi_output_per_note(&mut self, track_id: &str, per_note: bool) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.routing.midi_output_per_note != per_note {
                if routing_debug_enabled() {
                    eprintln!(
                        "[routing] midi_output_per_note track={} old={} new={}",
                        track_id, t.routing.midi_output_per_note, per_note
                    );
                }
                t.routing.midi_output_per_note = per_note;
                return true;
            }
        }
        false
    }

    /// Add an aux send from `track_id` to the first Bus/Return track that
    /// isn't already a target (Phase 3 — a richer target picker is a follow-up,
    /// mirroring how inserts auto-seeded before the picker overlay). Returns
    /// the new send id, or `None` if there is no eligible routing track or the
    /// track already sends to every routing track.
    pub fn add_send(&mut self, track_id: &str) -> Option<String> {
        let existing: Vec<String> = self
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .map(|t| t.sends.iter().map(|s| s.target_track_id.clone()).collect())
            .unwrap_or_default();
        let target = self.tracks.iter().find(|t| {
            t.id != track_id && is_project_routing_track(t) && !existing.contains(&t.id)
        })?;
        let target_id = target.id.clone();
        self.add_send_to_target(track_id, &target_id)
    }

    pub fn add_send_to_target(&mut self, track_id: &str, target_track_id: &str) -> Option<String> {
        if track_id == target_track_id {
            return None;
        }
        let (target_id, target_name) = self
            .tracks
            .iter()
            .find(|t| t.id == target_track_id && is_project_routing_track(t))
            .map(|target| (target.id.clone(), target.name.clone()))?;

        let track = self.tracks.iter_mut().find(|t| t.id == track_id)?;
        // Allow bus/return → bus/return chains; engine rejects cycles at plan time.
        // VSTi multi-out children remain send sources as before.
        if track.sends.iter().any(|s| s.target_track_id == target_id) {
            return None;
        }
        let send_id = format!("send-{}-{}", track.id, track.sends.len() + 1);
        track.sends.push(SendSlotState {
            id: send_id.clone(),
            target_track_id: target_id.clone(),
            target_name,
            enabled: true,
            pre_fader: false,
            gain_db: 0.0,
        });
        if routing_debug_enabled() {
            eprintln!(
                "[routing] add_send track={} send={} -> {}",
                track_id, send_id, target_id
            );
        }
        Some(send_id)
    }

    pub fn create_return_and_send(&mut self, track_id: &str) -> Option<(String, String)> {
        if !self.tracks.iter().any(|track| track.id == track_id) {
            return None;
        }
        let next_return = self
            .tracks
            .iter()
            .filter(|track| track.track_type == TrackType::Return)
            .count()
            + 1;
        let return_id = self.create_track(CreateTrackOptions {
            track_type: TrackType::Return,
            name: format!("Return {next_return}"),
            color: self.track_color_for_index(self.tracks.len()),
            volume: volume::db_to_norm(0.0),
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        let send_id = self.add_send_to_target(track_id, &return_id)?;
        Some((return_id, send_id))
    }

    /// Create a mixer-only Bus immediately (no Add Track dialog). Optionally
    /// routes the given tracks' main outs into the new bus.
    pub fn create_bus_track(&mut self, route_from_track_ids: &[String]) -> String {
        let next_bus = self
            .tracks
            .iter()
            .filter(|track| track.track_type == TrackType::Bus)
            .count()
            + 1;
        let bus_id = self.create_track(CreateTrackOptions {
            track_type: TrackType::Bus,
            name: format!("Bus {next_bus}"),
            color: self.track_color_for_index(self.tracks.len()),
            volume: volume::db_to_norm(0.0),
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        for track_id in route_from_track_ids {
            if track_id == &bus_id {
                continue;
            }
            let Some(track) = self.find_track(track_id) else {
                continue;
            };
            if track.track_type.is_routing() || track.track_type == TrackType::Master {
                continue;
            }
            self.set_track_output_routing(
                track_id,
                TrackOutputRouting::Bus {
                    bus_id: bus_id.clone(),
                },
            );
        }
        self.select_track(&bus_id);
        bus_id
    }

    pub fn remove_send(&mut self, track_id: &str, send_id: &str) {
        if let Some(track) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            track.sends.retain(|s| s.id != send_id);
            if routing_debug_enabled() {
                eprintln!("[routing] remove_send track={} send={}", track_id, send_id);
            }
        }
    }

    pub fn set_send_gain_db(&mut self, track_id: &str, send_id: &str, gain_db: f32) -> bool {
        let Some(send) = self
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .and_then(|track| track.sends.iter_mut().find(|send| send.id == send_id))
        else {
            return false;
        };
        let next = gain_db.clamp(-60.0, 6.0);
        if (send.gain_db - next).abs() <= 1.0e-4 {
            return false;
        }
        send.gain_db = next;
        true
    }

    pub fn send_order(&self, track_id: &str) -> Vec<String> {
        self.tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.sends.iter().map(|send| send.id.clone()).collect())
            .unwrap_or_default()
    }

    pub fn set_send_order(&mut self, track_id: &str, ordered_ids: &[String]) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|t| t.id == track_id) else {
            return false;
        };
        let before: Vec<String> = track.sends.iter().map(|send| send.id.clone()).collect();
        let mut remaining = std::mem::take(&mut track.sends);
        let mut reordered = Vec::with_capacity(remaining.len());
        for wanted in ordered_ids {
            if let Some(pos) = remaining.iter().position(|send| send.id == *wanted) {
                reordered.push(remaining.remove(pos));
            }
        }
        reordered.append(&mut remaining);
        let after: Vec<String> = reordered.iter().map(|send| send.id.clone()).collect();
        track.sends = reordered;
        before != after
    }

    pub fn reordered_send_ids(
        ids: &[String],
        dragged_send_id: &str,
        insertion_index: usize,
    ) -> Vec<String> {
        let Some(origin) = ids.iter().position(|id| id == dragged_send_id) else {
            return ids.to_vec();
        };
        let dragged = ids[origin].clone();
        let mut remaining = ids.to_vec();
        remaining.remove(origin);
        let target = if insertion_index > origin {
            insertion_index.saturating_sub(1)
        } else {
            insertion_index
        }
        .min(remaining.len());
        remaining.insert(target, dragged);
        remaining
    }

    pub fn toggle_send_enabled(&mut self, track_id: &str, send_id: &str) -> Option<bool> {
        let track = self.tracks.iter_mut().find(|t| t.id == track_id)?;
        let send = track.sends.iter_mut().find(|s| s.id == send_id)?;
        send.enabled = !send.enabled;
        Some(send.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_track(state: &mut TimelineState, track_type: TrackType, name: &str) -> String {
        state.create_track(CreateTrackOptions {
            track_type,
            name: name.to_string(),
            color: gpui::Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            volume: volume::db_to_norm(0.0),
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        })
    }

    #[test]
    fn audio_input_routes_follow_track_format_compatibility() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = create_track(&mut state, TrackType::Audio, "Audio");
        let mono_route = TrackInputRouting::AudioDeviceChannel {
            device_id: "input-device".to_string(),
            channel: 1,
        };
        let stereo_route = TrackInputRouting::AudioDeviceChannels {
            device_id: "input-device".to_string(),
            channels: vec![0, 1],
        };

        assert!(state.set_track_input_routing(&track_id, mono_route.clone()));
        assert!(state.set_track_audio_format(&track_id, TrackAudioFormat::Mono));
        assert_eq!(
            state.find_track(&track_id).unwrap().routing.input,
            mono_route
        );

        assert!(!state.set_track_input_routing(&track_id, stereo_route.clone()));
        assert!(state.set_track_audio_format(&track_id, TrackAudioFormat::Stereo));
        assert!(state.set_track_input_routing(&track_id, stereo_route));
        assert!(state.set_track_audio_format(&track_id, TrackAudioFormat::Mono));
        assert_eq!(
            state.find_track(&track_id).unwrap().routing.input,
            TrackInputRouting::None
        );
    }

    #[test]
    fn stereo_routes_reject_duplicate_channel_pairs() {
        let route = TrackInputRouting::AudioDeviceChannels {
            device_id: "input-device".to_string(),
            channels: vec![1, 1],
        };

        assert!(!route.is_compatible_with_audio_format(TrackAudioFormat::Stereo));
    }

    #[test]
    fn sends_ignore_vsti_multiout_child_tracks() {
        let mut state = TimelineState::default();
        state.tracks.clear();

        let source_id = create_track(&mut state, TrackType::Audio, "Audio");
        let child_id = create_track(&mut state, TrackType::Bus, "VSTi Out 1");
        state
            .tracks
            .iter_mut()
            .find(|track| track.id == child_id)
            .unwrap()
            .id = vsti_output_child_track_id("insert-track-1-1", 0);
        let child_id = vsti_output_child_track_id("insert-track-1-1", 0);
        let return_id = create_track(&mut state, TrackType::Return, "Return 1");

        assert!(!is_project_routing_track(
            state.find_track(&child_id).unwrap()
        ));
        assert!(is_project_routing_track(
            state.find_track(&return_id).unwrap()
        ));
        assert!(state.add_send_to_target(&source_id, &child_id).is_none());

        let send_id = state
            .add_send(&source_id)
            .expect("real return should be selected");
        let source = state.find_track(&source_id).unwrap();
        let send = source.sends.iter().find(|send| send.id == send_id).unwrap();
        assert_eq!(send.target_track_id, return_id);

        let child_send_id = state
            .add_send_to_target(&child_id, &return_id)
            .expect("VSTi output child may send to a project routing track");
        let child = state.find_track(&child_id).unwrap();
        assert!(child.sends.iter().any(|send| send.id == child_send_id));

        let return_count_before = state
            .tracks
            .iter()
            .filter(|track| track.track_type == TrackType::Return)
            .count();
        let (created_return_id, created_send_id) = state
            .create_return_and_send(&child_id)
            .expect("VSTi output child may create a return and send to it");
        assert_eq!(
            state
                .tracks
                .iter()
                .filter(|track| track.track_type == TrackType::Return)
                .count(),
            return_count_before + 1
        );
        let child = state.find_track(&child_id).unwrap();
        assert!(child.sends.iter().any(|send| {
            send.id == created_send_id && send.target_track_id == created_return_id
        }));
    }

    #[test]
    fn send_gain_updates_and_clamps_in_db() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let source_id = create_track(&mut state, TrackType::Audio, "Audio");
        let return_id = create_track(&mut state, TrackType::Return, "Return");
        let send_id = state
            .add_send_to_target(&source_id, &return_id)
            .expect("send");

        assert!(state.set_send_gain_db(&source_id, &send_id, -12.5));
        assert_eq!(
            state.find_track(&source_id).unwrap().sends[0].gain_db,
            -12.5
        );
        assert!(state.set_send_gain_db(&source_id, &send_id, 30.0));
        assert_eq!(state.find_track(&source_id).unwrap().sends[0].gain_db, 6.0);
        assert!(!state.set_send_gain_db(&source_id, &send_id, 6.0));
    }

    #[test]
    fn create_bus_routes_sources_and_hides_from_arrangement() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let audio_id = create_track(&mut state, TrackType::Audio, "Drums");
        let bus_id = state.create_bus_track(std::slice::from_ref(&audio_id));
        let bus = state.find_track(&bus_id).unwrap();
        assert_eq!(bus.track_type, TrackType::Bus);
        assert!(is_arrangement_hidden_track(bus));
        assert_eq!(
            state.find_track(&audio_id).unwrap().routing.output,
            TrackOutputRouting::Bus {
                bus_id: bus_id.clone()
            }
        );
        let layout = state.track_row_layout();
        assert_eq!(layout.row_for_track(&bus_id).unwrap().height, 0.0);
        assert!(layout.row_for_track(&audio_id).unwrap().height > 0.0);
    }
}
