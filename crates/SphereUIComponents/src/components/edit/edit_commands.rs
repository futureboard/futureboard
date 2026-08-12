//! Undo/redo edit commands — all timeline mutations go through here.

use std::collections::VecDeque;

use crate::components::timeline::timeline_state::{
    AudioClipStretchState, ClipState, MidiArticulationEvent, MidiControllerKind,
    MidiControllerPoint, MidiNoteState, SongTextEvent, TimelineState, TrackState,
};

/// Snapshot of a clip plus its owning track for undo/redo.
#[derive(Debug, Clone)]
pub struct ClipSnapshot {
    pub track_id: String,
    pub clip: ClipState,
}

impl ClipSnapshot {
    pub fn capture(state: &TimelineState, clip_id: &str) -> Option<Self> {
        for track in &state.tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) {
                return Some(Self {
                    track_id: track.id.clone(),
                    clip: clip.clone(),
                });
            }
        }
        None
    }
}

/// Snapshot of a track plus its original index for undo/redo.
#[derive(Debug, Clone)]
pub struct TrackSnapshot {
    pub index: usize,
    pub track: TrackState,
}

impl TrackSnapshot {
    pub fn capture(state: &TimelineState, track_id: &str) -> Option<Self> {
        state
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .map(|index| Self {
                index,
                track: state.tracks[index].clone(),
            })
    }
}

/// Editable command with perfect undo.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum EditCommand {
    CreateClip {
        track_id: String,
        clip: ClipState,
    },
    BatchCreateClips {
        clips: Vec<(String, ClipState)>,
    },
    DeleteClip {
        snapshot: ClipSnapshot,
    },
    /// Replaces one existing clip with an exact post-gesture snapshot. Used by
    /// trim and Inspector property gestures so each gesture is one reversible
    /// history entry.
    UpdateClip {
        previous: ClipSnapshot,
        next: ClipSnapshot,
    },
    BatchDeleteClips {
        snapshots: Vec<ClipSnapshot>,
    },
    ReplaceClipWithClips {
        snapshot: ClipSnapshot,
        clips: Vec<(String, ClipState)>,
    },
    DeleteTrack {
        snapshot: TrackSnapshot,
    },
    CreateMidiNote {
        clip_id: String,
        note: MidiNoteState,
    },
    /// Batch note insert (paste / duplicate) — one undo entry for the group.
    CreateMidiNotes {
        clip_id: String,
        notes: Vec<MidiNoteState>,
    },
    DeleteMidiNotes {
        clip_id: String,
        notes: Vec<MidiNoteState>,
    },
    /// Set the muted flag on a set of notes. `prev` snapshots each note's
    /// original muted state so undo restores it exactly.
    SetMidiNotesMuted {
        clip_id: String,
        prev: Vec<(u64, bool)>,
        muted: bool,
    },
    /// In-place transform of a fixed set of notes (move / resize / velocity /
    /// quantize / transpose / nudge). The note set is unchanged — only field
    /// values differ — so `prev`/`next` carry full per-id snapshots and
    /// execute/undo simply overwrite matching notes by id.
    EditMidiNotes {
        clip_id: String,
        prev: Vec<MidiNoteState>,
        next: Vec<MidiNoteState>,
    },
    /// Replace a controller lane's points (draw / erase gesture). One entry per
    /// gesture; `prev`/`next` are full point snapshots of the lane.
    SetControllerPoints {
        clip_id: String,
        kind: MidiControllerKind,
        prev: Vec<MidiControllerPoint>,
        next: Vec<MidiControllerPoint>,
    },
    /// Replace a clip's direction articulation events (insert / move / delete
    /// gesture). One entry per gesture; `prev`/`next` are full event snapshots
    /// of the clip's articulation lane, mirroring `SetControllerPoints`.
    SetMidiArticulations {
        clip_id: String,
        prev: Vec<MidiArticulationEvent>,
        next: Vec<MidiArticulationEvent>,
    },
    /// Split one note into `parts` (two or more contiguous notes). Atomic so a
    /// single undo restores the original note and removes every part.
    SplitMidiNote {
        clip_id: String,
        original: MidiNoteState,
        parts: Vec<MidiNoteState>,
    },
    /// Set an audio clip's non-destructive stretch/pitch state. `prev`/`next`
    /// snapshot the whole struct so one inspector edit (or one stretch-drag
    /// gesture) is a single, perfectly reversible undo entry. The clip's
    /// timeline length is coupled to the time-stretch ratio (so visual =
    /// audible length, spec §10), hence the duration is snapshotted too.
    SetClipStretch {
        clip_id: String,
        prev: AudioClipStretchState,
        next: AudioClipStretchState,
        prev_duration_beats: f32,
        next_duration_beats: f32,
    },
    /// Reorder a track's FX / insert chain. Stores the full before/after
    /// slot-id order so undo and redo are exact regardless of how the new order
    /// was computed (drag gap math lives in the drop handler, not here). The
    /// operation only reorders the existing slots — it never recreates a plugin
    /// instance — so bypass / enabled / preset / parameter / editor state all
    /// follow each `plugin_instance_id`. One undo entry per completed drag.
    ReorderFxSlot {
        track_id: String,
        before_order: Vec<String>,
        after_order: Vec<String>,
    },
    /// Reorder a track's aux sends. Sends are routed by stable send id, so the
    /// existing send structs move in place and keep gain/enabled/pre-fader state.
    ReorderSendSlot {
        track_id: String,
        before_order: Vec<String>,
        after_order: Vec<String>,
    },
    /// Batch per-track row height changes (layout/view — one undo entry per gesture).
    SetTrackHeights {
        prev: Vec<(String, f32)>,
        next: Vec<(String, f32)>,
    },
    /// One fader gesture (preview → commit). Never reloads the project/graph.
    SetTrackVolume {
        track_id: String,
        prev: f32,
        next: f32,
    },
    /// One pan knob gesture. Never reloads the project/graph.
    SetTrackPan {
        track_id: String,
        prev: f32,
        next: f32,
    },
    SetTrackVolumeAutomationRead {
        track_id: String,
        prev: bool,
        next: bool,
    },
    /// Replace only the affected Song Text events. Empty `previous` creates,
    /// empty `next` deletes, and populated snapshots edit/move atomically.
    SetSongTextEvents {
        label: &'static str,
        previous: Vec<SongTextEvent>,
        next: Vec<SongTextEvent>,
    },
}

impl EditCommand {
    /// MIDI content changes are cheap to identify at the command boundary.
    /// The owner uses this to publish a note edit immediately instead of
    /// waiting for the general 75 ms gesture-sync throttle.
    pub fn is_midi_edit(&self) -> bool {
        matches!(
            self,
            EditCommand::CreateMidiNote { .. }
                | EditCommand::CreateMidiNotes { .. }
                | EditCommand::DeleteMidiNotes { .. }
                | EditCommand::SetMidiNotesMuted { .. }
                | EditCommand::EditMidiNotes { .. }
                | EditCommand::SetControllerPoints { .. }
                | EditCommand::SetMidiArticulations { .. }
                | EditCommand::SplitMidiNote { .. }
        )
    }

    pub fn is_metadata_only(&self) -> bool {
        matches!(self, EditCommand::SetSongTextEvents { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::CreateClip { .. } => "Create Clip",
            EditCommand::BatchCreateClips { .. } => "Create Clips",
            EditCommand::DeleteClip { .. } => "Delete Clip",
            EditCommand::UpdateClip { .. } => "Edit Clip",
            EditCommand::BatchDeleteClips { .. } => "Delete Clips",
            EditCommand::ReplaceClipWithClips { .. } => "Split Clip",
            EditCommand::DeleteTrack { .. } => "Delete Track",
            EditCommand::CreateMidiNote { .. } => "Create MIDI Note",
            EditCommand::CreateMidiNotes { .. } => "Add MIDI Notes",
            EditCommand::DeleteMidiNotes { .. } => "Delete MIDI Notes",
            EditCommand::SetMidiNotesMuted { muted, .. } => {
                if *muted {
                    "Mute Notes"
                } else {
                    "Unmute Notes"
                }
            }
            EditCommand::EditMidiNotes { .. } => "Edit MIDI Notes",
            EditCommand::SetControllerPoints { .. } => "Edit CC Lane",
            EditCommand::SetMidiArticulations { .. } => "Edit Articulations",
            EditCommand::SplitMidiNote { .. } => "Split MIDI Note",
            EditCommand::SetClipStretch { .. } => "Edit Stretch",
            EditCommand::ReorderFxSlot { .. } => "Reorder FX",
            EditCommand::ReorderSendSlot { .. } => "Reorder Sends",
            EditCommand::SetTrackHeights { .. } => "Resize Track Height",
            EditCommand::SetTrackVolume { .. } => "Set Volume",
            EditCommand::SetTrackPan { .. } => "Set Pan",
            EditCommand::SetTrackVolumeAutomationRead { .. } => "Set Volume Automation Read",
            EditCommand::SetSongTextEvents { label, .. } => label,
        }
    }

    pub fn execute(&self, state: &mut TimelineState) {
        match self {
            EditCommand::CreateClip { track_id, clip } => {
                if let Some(track) = state.tracks.iter_mut().find(|t| t.id == *track_id) {
                    track.clips.push(clip.clone());
                    state.selection.selected_track_id = Some(track_id.clone());
                    state.selection.selected_clip_ids = vec![clip.id.clone()];
                }
            }
            EditCommand::BatchCreateClips { clips } => {
                let mut selected = Vec::new();
                let mut selected_track = None;
                for (track_id, clip) in clips {
                    if let Some(track) = state.tracks.iter_mut().find(|t| t.id == *track_id) {
                        track.clips.push(clip.clone());
                        selected_track = Some(track_id.clone());
                        selected.push(clip.id.clone());
                    }
                }
                if !selected.is_empty() {
                    state.selection.selected_track_id = selected_track;
                    state.selection.selected_clip_ids = selected;
                }
            }
            EditCommand::DeleteClip { snapshot } => {
                state.delete_clip(&snapshot.clip.id);
            }
            EditCommand::UpdateClip { previous, next } => {
                state.delete_clip(&previous.clip.id);
                restore_clip_snapshot(state, next);
            }
            EditCommand::BatchDeleteClips { snapshots } => {
                for snap in snapshots {
                    state.delete_clip(&snap.clip.id);
                }
            }
            EditCommand::ReplaceClipWithClips { snapshot, clips } => {
                state.delete_clip(&snapshot.clip.id);
                for (track_id, clip) in clips {
                    if let Some(track) = state.tracks.iter_mut().find(|t| t.id == *track_id) {
                        if !track.clips.iter().any(|c| c.id == clip.id) {
                            track.clips.push(clip.clone());
                        }
                    }
                }
                state.selection.selected_track_id = Some(snapshot.track_id.clone());
                state.selection.selected_clip_ids =
                    clips.iter().map(|(_, clip)| clip.id.clone()).collect();
            }
            EditCommand::DeleteTrack { snapshot } => {
                state.delete_track(&snapshot.track.id);
            }
            EditCommand::CreateMidiNote { clip_id, note } => {
                if let Some(notes) = state.midi_clip_notes_mut(clip_id) {
                    if !notes.iter().any(|n| n.id == note.id) {
                        notes.push(note.clone());
                    }
                }
                // A note drawn past the clip end auto-expands the clip so it is
                // always contained. Applies to redo too.
                state.expand_clip_to_contain_notes(clip_id);
            }
            EditCommand::CreateMidiNotes { clip_id, notes } => {
                if let Some(existing) = state.midi_clip_notes_mut(clip_id) {
                    for note in notes {
                        if !existing.iter().any(|n| n.id == note.id) {
                            existing.push(note.clone());
                        }
                    }
                }
                state.expand_clip_to_contain_notes(clip_id);
            }
            EditCommand::DeleteMidiNotes { clip_id, notes } => {
                let ids: Vec<u64> = notes.iter().map(|n| n.id).collect();
                state.delete_midi_notes(clip_id, &ids);
            }
            EditCommand::SetMidiNotesMuted {
                clip_id,
                prev,
                muted,
            } => {
                let ids: Vec<u64> = prev.iter().map(|(id, _)| *id).collect();
                state.set_midi_notes_muted(clip_id, &ids, *muted);
            }
            EditCommand::EditMidiNotes { clip_id, next, .. } => {
                state.overwrite_midi_notes(clip_id, next);
            }
            EditCommand::SetControllerPoints {
                clip_id,
                kind,
                next,
                ..
            } => {
                state.set_controller_lane_points(clip_id, *kind, next.clone());
            }
            EditCommand::SetMidiArticulations { clip_id, next, .. } => {
                state.set_midi_articulations(clip_id, next.clone());
            }
            EditCommand::SplitMidiNote {
                clip_id,
                original,
                parts,
            } => {
                state.delete_midi_notes(clip_id, &[original.id]);
                if let Some(existing) = state.midi_clip_notes_mut(clip_id) {
                    for note in parts {
                        if !existing.iter().any(|n| n.id == note.id) {
                            existing.push(note.clone());
                        }
                    }
                }
                state.expand_clip_to_contain_notes(clip_id);
            }
            EditCommand::SetClipStretch {
                clip_id,
                next,
                next_duration_beats,
                ..
            } => {
                state.set_clip_stretch(clip_id, next.clone());
                state.set_clip_length(clip_id, *next_duration_beats);
            }
            EditCommand::ReorderFxSlot {
                track_id,
                after_order,
                ..
            } => {
                state.set_insert_order(track_id, after_order);
            }
            EditCommand::ReorderSendSlot {
                track_id,
                after_order,
                ..
            } => {
                state.set_send_order(track_id, after_order);
            }
            EditCommand::SetTrackHeights { next, .. } => {
                apply_track_heights_snapshot(state, next);
            }
            EditCommand::SetTrackVolume { track_id, next, .. } => {
                state.set_track_volume(track_id, *next);
            }
            EditCommand::SetTrackPan { track_id, next, .. } => {
                state.set_track_pan(track_id, *next);
            }
            EditCommand::SetTrackVolumeAutomationRead { track_id, next, .. } => {
                let beat = state.transport.playhead_beats;
                state.set_track_volume_automation_read(track_id, *next);
                state.recompute_effective_volumes(beat, "automation_read_edit");
            }
            EditCommand::SetSongTextEvents { previous, next, .. } => {
                apply_song_text_snapshot(state, previous, next);
            }
        }
    }

    pub fn undo(&self, state: &mut TimelineState) {
        match self {
            EditCommand::CreateClip { clip, .. } => {
                state.delete_clip(&clip.id);
            }
            EditCommand::BatchCreateClips { clips } => {
                for (_, clip) in clips {
                    state.delete_clip(&clip.id);
                }
            }
            EditCommand::DeleteClip { snapshot } => {
                restore_clip_snapshot(state, snapshot);
            }
            EditCommand::UpdateClip { previous, next } => {
                state.delete_clip(&next.clip.id);
                restore_clip_snapshot(state, previous);
            }
            EditCommand::BatchDeleteClips { snapshots } => {
                for snap in snapshots {
                    restore_clip_snapshot(state, snap);
                }
            }
            EditCommand::ReplaceClipWithClips { snapshot, clips } => {
                for (_, clip) in clips {
                    state.delete_clip(&clip.id);
                }
                restore_clip_snapshot(state, snapshot);
                state.selection.selected_track_id = Some(snapshot.track_id.clone());
                state.selection.selected_clip_ids = vec![snapshot.clip.id.clone()];
            }
            EditCommand::DeleteTrack { snapshot } => {
                restore_track_snapshot(state, snapshot);
            }
            EditCommand::CreateMidiNote { clip_id, note } => {
                state.delete_midi_notes(clip_id, &[note.id]);
            }
            EditCommand::CreateMidiNotes { clip_id, notes } => {
                let ids: Vec<u64> = notes.iter().map(|n| n.id).collect();
                state.delete_midi_notes(clip_id, &ids);
            }
            EditCommand::DeleteMidiNotes { clip_id, notes } => {
                if let Some(existing) = state.midi_clip_notes_mut(clip_id) {
                    for note in notes {
                        if !existing.iter().any(|n| n.id == note.id) {
                            existing.push(note.clone());
                        }
                    }
                }
            }
            EditCommand::SetMidiNotesMuted { clip_id, prev, .. } => {
                if let Some(existing) = state.midi_clip_notes_mut(clip_id) {
                    for (id, was) in prev {
                        if let Some(note) = existing.iter_mut().find(|n| n.id == *id) {
                            note.muted = *was;
                        }
                    }
                }
            }
            EditCommand::EditMidiNotes { clip_id, prev, .. } => {
                state.overwrite_midi_notes(clip_id, prev);
            }
            EditCommand::SetControllerPoints {
                clip_id,
                kind,
                prev,
                ..
            } => {
                state.set_controller_lane_points(clip_id, *kind, prev.clone());
            }
            EditCommand::SetMidiArticulations { clip_id, prev, .. } => {
                state.set_midi_articulations(clip_id, prev.clone());
            }
            EditCommand::SplitMidiNote {
                clip_id,
                original,
                parts,
            } => {
                let ids: Vec<u64> = parts.iter().map(|n| n.id).collect();
                state.delete_midi_notes(clip_id, &ids);
                if let Some(existing) = state.midi_clip_notes_mut(clip_id) {
                    if !existing.iter().any(|n| n.id == original.id) {
                        existing.push(original.clone());
                    }
                }
                state.expand_clip_to_contain_notes(clip_id);
            }
            EditCommand::SetClipStretch {
                clip_id,
                prev,
                prev_duration_beats,
                ..
            } => {
                state.set_clip_stretch(clip_id, prev.clone());
                state.set_clip_length(clip_id, *prev_duration_beats);
            }
            EditCommand::ReorderFxSlot {
                track_id,
                before_order,
                ..
            } => {
                state.set_insert_order(track_id, before_order);
            }
            EditCommand::ReorderSendSlot {
                track_id,
                before_order,
                ..
            } => {
                state.set_send_order(track_id, before_order);
            }
            EditCommand::SetTrackHeights { prev, .. } => {
                apply_track_heights_snapshot(state, prev);
            }
            EditCommand::SetTrackVolume { track_id, prev, .. } => {
                state.set_track_volume(track_id, *prev);
            }
            EditCommand::SetTrackPan { track_id, prev, .. } => {
                state.set_track_pan(track_id, *prev);
            }
            EditCommand::SetTrackVolumeAutomationRead { track_id, prev, .. } => {
                let beat = state.transport.playhead_beats;
                state.set_track_volume_automation_read(track_id, *prev);
                state.recompute_effective_volumes(beat, "automation_read_edit");
            }
            EditCommand::SetSongTextEvents { previous, next, .. } => {
                apply_song_text_snapshot(state, next, previous);
            }
        }
    }
}

fn apply_song_text_snapshot(
    state: &mut TimelineState,
    remove: &[SongTextEvent],
    insert: &[SongTextEvent],
) {
    state.apply_song_text_patch(remove, insert);
}

fn apply_track_heights_snapshot(state: &mut TimelineState, heights: &[(String, f32)]) {
    for (track_id, height) in heights {
        if (*height - crate::components::timeline::timeline_state::DEFAULT_TRACK_HEIGHT).abs()
            < 0.01
        {
            state.track_view_layout.remove_track(track_id);
        } else if state.tracks.iter().any(|t| t.id == *track_id) {
            state
                .track_view_layout
                .set_height(track_id.clone(), *height);
        }
    }
}

fn restore_clip_snapshot(state: &mut TimelineState, snapshot: &ClipSnapshot) {
    if let Some(track) = state.tracks.iter_mut().find(|t| t.id == snapshot.track_id) {
        if !track.clips.iter().any(|c| c.id == snapshot.clip.id) {
            track.clips.push(snapshot.clip.clone());
        }
    }
}

#[cfg(test)]
mod song_text_command_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{SongTextEvent, SongTextEventType};

    #[test]
    fn chord_and_lyric_pair_is_one_undoable_command() {
        let mut state = TimelineState::default();
        let chord = SongTextEvent::chord(4.0, "Am7").unwrap();
        let lyric = SongTextEvent::lyric(4.0, "I remember").unwrap();
        let command = EditCommand::SetSongTextEvents {
            label: "Add Chord and Lyric",
            previous: Vec::new(),
            next: vec![chord.clone(), lyric.clone()],
        };
        let mut history = EditHistory::new(10);
        command.execute(&mut state);
        history.push(command);
        assert_eq!(state.song_text_events.len(), 2);
        assert!(history.undo(&mut state));
        assert!(state.song_text_events.is_empty());
        assert!(
            !history.undo(&mut state),
            "the pair must consume one history item"
        );
        assert!(history.redo(&mut state));
        assert_eq!(
            state
                .song_text_events
                .iter()
                .map(SongTextEvent::event_type)
                .collect::<Vec<_>>(),
            vec![SongTextEventType::Chord, SongTextEventType::Lyric]
        );
    }

    #[test]
    fn text_edit_undo_restores_exact_content_and_id() {
        let mut state = TimelineState::default();
        let previous = SongTextEvent::lyric(2.5, "before").unwrap();
        state.upsert_song_text_event(previous.clone());
        let mut next = previous.clone();
        if let crate::components::timeline::timeline_state::SongTextEventKind::Lyric(lyric) =
            &mut next.kind
        {
            lyric.text = "after".to_string();
        }
        let command = EditCommand::SetSongTextEvents {
            label: "Edit Song Text",
            previous: vec![previous.clone()],
            next: vec![next.clone()],
        };
        command.execute(&mut state);
        assert_eq!(state.song_text_event(&previous.id), Some(&next));
        command.undo(&mut state);
        assert_eq!(state.song_text_event(&previous.id), Some(&previous));
    }
}

#[cfg(test)]
mod inspector_gesture_command_tests {
    use super::*;

    #[test]
    fn pan_preview_commits_as_one_undoable_command() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_audio_track();
        state.begin_track_pan_preview(&track_id);
        assert!(state.set_track_pan_preview(&track_id, 0.25));
        assert!(state.set_track_pan_preview(&track_id, 0.6));
        let (prev, next) = state
            .commit_track_pan_preview(&track_id)
            .expect("pan gesture");

        let mut history = EditHistory::new(8);
        history.push(EditCommand::SetTrackPan {
            track_id: track_id.clone(),
            prev,
            next,
        });
        assert!((state.find_track(&track_id).unwrap().pan - 0.6).abs() < 1.0e-6);
        assert!(history.undo(&mut state));
        assert!((state.find_track(&track_id).unwrap().pan - prev).abs() < 1.0e-6);
        assert!(!history.undo(&mut state), "one drag must create one entry");
        assert!(history.redo(&mut state));
        assert!((state.find_track(&track_id).unwrap().pan - 0.6).abs() < 1.0e-6);
    }

    #[test]
    fn volume_automation_read_is_undoable() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_audio_track();
        let command = EditCommand::SetTrackVolumeAutomationRead {
            track_id: track_id.clone(),
            prev: true,
            next: false,
        };
        let mut history = EditHistory::new(8);
        command.execute(&mut state);
        history.push(command);
        assert!(!state.find_track(&track_id).unwrap().volume_automation_read);
        assert!(history.undo(&mut state));
        assert!(state.find_track(&track_id).unwrap().volume_automation_read);
        assert!(history.redo(&mut state));
        assert!(!state.find_track(&track_id).unwrap().volume_automation_read);
    }
}

#[cfg(test)]
mod stretch_command_tests {
    use super::*;
    use crate::components::timeline::timeline_state::StretchMode;

    #[test]
    fn set_clip_stretch_executes_undoes_and_redoes() {
        let mut state = TimelineState::demo_project();
        let prev = state
            .clip_stretch("clip-1")
            .cloned()
            .expect("demo audio clip-1");
        let prev_len = state.clip_duration_beats("clip-1").expect("clip-1 length");
        let mut next = prev.clone();
        next.mode = StretchMode::Manual;
        next.set_stretch_ratio(1.5);
        let next_len = prev_len * 1.5; // length follows the ratio (spec §10)
        assert_ne!(prev, next, "test setup must produce a real change");

        let cmd = EditCommand::SetClipStretch {
            clip_id: "clip-1".to_string(),
            prev: prev.clone(),
            next: next.clone(),
            prev_duration_beats: prev_len,
            next_duration_beats: next_len,
        };
        assert_eq!(cmd.label(), "Edit Stretch");

        cmd.execute(&mut state);
        assert_eq!(state.clip_stretch("clip-1"), Some(&next));
        assert!((state.clip_duration_beats("clip-1").unwrap() - next_len).abs() < 0.001);

        cmd.undo(&mut state);
        assert_eq!(state.clip_stretch("clip-1"), Some(&prev));
        assert!((state.clip_duration_beats("clip-1").unwrap() - prev_len).abs() < 0.001);

        // Redo re-applies `next` and its coupled length.
        cmd.execute(&mut state);
        assert_eq!(state.clip_stretch("clip-1"), Some(&next));
        assert!((state.clip_duration_beats("clip-1").unwrap() - next_len).abs() < 0.001);
    }

    #[test]
    fn edit_history_keeps_the_newest_bounded_steps() {
        let mut history = EditHistory::new(2);
        let command = || EditCommand::SetTrackHeights {
            prev: Vec::new(),
            next: Vec::new(),
        };
        history.push(command());
        history.push(command());
        history.push(command());

        let mut state = TimelineState::default();
        assert!(history.undo(&mut state));
        assert!(history.undo(&mut state));
        assert!(!history.undo(&mut state), "oldest entry must be evicted");
        assert!(history.redo(&mut state));
        assert!(history.redo(&mut state));
        assert!(!history.redo(&mut state));
    }
}

#[cfg(test)]
mod articulation_command_tests {
    use super::*;
    use crate::components::timeline::timeline_state::{ArticulationId, MidiArticulationEvent};

    fn midi_state() -> (TimelineState, String) {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_midi_track();
        let clip_id = state.create_midi_clip(&track_id, 0.0, 8.0).expect("clip");
        (state, clip_id)
    }

    #[test]
    fn set_midi_articulations_executes_undoes_and_redoes() {
        let (mut state, clip_id) = midi_state();
        let prev = state.articulations_snapshot(&clip_id);
        assert!(prev.is_empty());
        let next = vec![
            MidiArticulationEvent::new(0.0, ArticulationId::Sustain),
            MidiArticulationEvent::new(4.0, ArticulationId::Legato),
        ];
        let cmd = EditCommand::SetMidiArticulations {
            clip_id: clip_id.clone(),
            prev: prev.clone(),
            next: next.clone(),
        };
        assert_eq!(cmd.label(), "Edit Articulations");

        cmd.execute(&mut state);
        assert_eq!(state.articulations_snapshot(&clip_id), next);
        cmd.undo(&mut state);
        assert!(state.articulations_snapshot(&clip_id).is_empty());
        cmd.execute(&mut state);
        assert_eq!(state.articulations_snapshot(&clip_id), next);
    }

    #[test]
    fn edit_midi_notes_round_trips_per_note_articulation() {
        let (mut state, clip_id) = midi_state();
        let id = state.add_midi_note(&clip_id, 60, 0.0, 1.0, 100).unwrap();
        let prev = vec![state.midi_clip_notes(&clip_id).unwrap()[0].clone()];
        let mut next = prev.clone();
        next[0].articulation = Some(ArticulationId::Staccato);

        let cmd = EditCommand::EditMidiNotes {
            clip_id: clip_id.clone(),
            prev,
            next,
        };
        cmd.execute(&mut state);
        let articulation_of = |state: &TimelineState| {
            state
                .midi_clip_notes(&clip_id)
                .unwrap()
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .articulation
        };
        assert_eq!(articulation_of(&state), Some(ArticulationId::Staccato));
        cmd.undo(&mut state);
        assert_eq!(articulation_of(&state), None);
        cmd.execute(&mut state);
        assert_eq!(articulation_of(&state), Some(ArticulationId::Staccato));
    }
}

fn restore_track_snapshot(state: &mut TimelineState, snapshot: &TrackSnapshot) {
    if state
        .tracks
        .iter()
        .any(|track| track.id == snapshot.track.id)
    {
        return;
    }
    let index = snapshot.index.min(state.tracks.len());
    state.tracks.insert(index, snapshot.track.clone());
    state.selection.selected_track_id = Some(snapshot.track.id.clone());
    state.selection.selected_clip_ids.clear();
}

/// Bounded undo/redo stack.
#[derive(Debug, Clone, Default)]
pub struct EditHistory {
    undo_stack: VecDeque<EditCommand>,
    redo_stack: VecDeque<EditCommand>,
    max_steps: usize,
}

impl EditHistory {
    pub fn new(max_steps: usize) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            max_steps: max_steps.max(1),
        }
    }

    pub fn push(&mut self, cmd: EditCommand) {
        self.undo_stack.push_back(cmd);
        if self.undo_stack.len() > self.max_steps {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
    }

    pub fn undo_with_impact(&mut self, state: &mut TimelineState) -> Option<bool> {
        let cmd = self.undo_stack.pop_back()?;
        let metadata_only = cmd.is_metadata_only();
        cmd.undo(state);
        self.redo_stack.push_back(cmd);
        Some(metadata_only)
    }

    pub fn undo(&mut self, state: &mut TimelineState) -> bool {
        self.undo_with_impact(state).is_some()
    }

    pub fn redo_with_impact(&mut self, state: &mut TimelineState) -> Option<bool> {
        let cmd = self.redo_stack.pop_back()?;
        let metadata_only = cmd.is_metadata_only();
        cmd.execute(state);
        self.undo_stack.push_back(cmd);
        Some(metadata_only)
    }

    pub fn redo(&mut self, state: &mut TimelineState) -> bool {
        self.redo_with_impact(state).is_some()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
}
