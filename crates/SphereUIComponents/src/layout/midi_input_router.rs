use gpui::{App, Context};

use crate::components;
use crate::components::timeline::timeline_state::TrackType;
use sphere_midi_service::{
    MidiInputEvent, MidiInputRouteStatus, MidiInputRouter, MidiInputSource, MidiInputTarget,
    VirtualKeyboardEvent,
};

use super::StudioLayout;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VirtualKeyboardTargetStatus {
    pub target: Option<MidiInputTarget>,
    pub label: Option<String>,
    pub hint: Option<String>,
}

impl StudioLayout {
    pub(super) fn update_virtual_keyboard_target_status(&mut self, cx: &mut Context<Self>) {
        let status = self.resolve_virtual_keyboard_target(cx);
        let label = status.label.clone();
        let hint = status.hint.clone();
        let _ = self.virtual_keyboard.update(cx, |panel, cx| {
            panel.set_target_status(label, hint);
            cx.notify();
        });
    }

    /// Release virtual-keyboard notes on a later turn, with **only** the panel
    /// entity leased.
    ///
    /// The panel's event sink re-enters `StudioLayout::update` to route the
    /// NoteOffs. Calling [`Self::release_virtual_keyboard_notes`] directly from
    /// `render` or a command dispatch — where `StudioLayout` is already leased —
    /// double-leases and panics (the multi-window crash). Deferring runs the
    /// flush after the current lease is released, so the sink re-entry is safe.
    pub(super) fn defer_release_virtual_keyboard_notes(&self, cx: &mut Context<Self>) {
        let panel = self.virtual_keyboard.clone();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.release_all(cx));
        });
    }

    /// Panic the virtual-keyboard service (all-notes-off + clear pressed/active)
    /// on a later turn, panel-only. Used when quiescing a session for project
    /// load. Deferred for the same re-entrancy reason as
    /// [`Self::defer_release_virtual_keyboard_notes`].
    pub(super) fn defer_panic_virtual_keyboard(&self, cx: &mut Context<Self>) {
        let panel = self.virtual_keyboard.clone();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.panic(cx));
        });
    }

    /// Unregister a window from the virtual-keyboard service and release any
    /// notes it still held, deferred and panel-only (see
    /// [`Self::defer_release_virtual_keyboard_notes`] for why). Safe for a window
    /// that was never registered. Called on MIDI-editor-popout / project close so
    /// a destroyed window never leaves stuck notes or a stale registration.
    pub(super) fn unregister_virtual_keyboard_window(
        &self,
        window_id: gpui::WindowId,
        cx: &mut Context<Self>,
    ) {
        let panel = self.virtual_keyboard.clone();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.unregister_window(window_id, cx));
        });
    }

    pub(super) fn route_virtual_keyboard_event(
        &mut self,
        event: VirtualKeyboardEvent,
        cx: &App,
    ) -> MidiInputRouteStatus {
        let status = self.resolve_virtual_keyboard_target(cx);
        let Some(target) = status.target else {
            if virtual_keyboard_debug() {
                eprintln!("[vkbd] route event={event:?} target=none -> NoTarget");
            }
            return MidiInputRouteStatus::NoTarget;
        };
        if virtual_keyboard_debug() {
            eprintln!(
                "[vkbd] route event={event:?} target_track={} instance={:?}",
                target.track_id, target.plugin_instance_id
            );
        }
        self.route_midi_input_event(
            MidiInputSource::VirtualKeyboard,
            target,
            MidiInputEvent::from(event),
            cx,
        )
    }

    /// Routes a gesture from the Soundfont Player window's keyboard or Test
    /// button. The built-in player is a track instrument with no plugin
    /// instance, so this always lands on the engine's track MIDI preview.
    pub(crate) fn dispatch_soundfont_preview(
        &mut self,
        track_id: &str,
        event: components::soundfont_player_window::SoundfontPlayerPreview,
        cx: &mut Context<Self>,
    ) {
        use components::soundfont_player_window::SoundfontPlayerPreview;

        // The engine builds its player from the project snapshot, so a font or
        // preset chosen moments ago has to reach the graph before the note
        // does. Only forced when something is actually pending.
        if self.audio_bridge.project_dirty {
            self.schedule_audio_project_sync(cx, true, "soundfont_preview");
        }

        let event = match event {
            SoundfontPlayerPreview::NoteOn {
                channel,
                pitch,
                velocity,
            } => MidiInputEvent::NoteOn {
                note: pitch,
                velocity,
                channel,
            },
            SoundfontPlayerPreview::NoteOff { channel, pitch } => MidiInputEvent::NoteOff {
                note: pitch,
                channel,
            },
            SoundfontPlayerPreview::AllNotesOff => MidiInputEvent::AllNotesOff,
        };
        let status = self.route_midi_input_event(
            MidiInputSource::VirtualKeyboard,
            MidiInputTarget {
                track_id: track_id.to_string(),
                plugin_instance_id: None,
            },
            event,
            cx,
        );
        match status {
            MidiInputRouteStatus::Routed => {}
            other => eprintln!("[soundfont-player] preview not routed: {other:?}"),
        }
    }

    pub(super) fn route_midi_input_event(
        &mut self,
        source: MidiInputSource,
        target: MidiInputTarget,
        event: MidiInputEvent,
        cx: &App,
    ) -> MidiInputRouteStatus {
        let _source = source;
        self.capture_midi_record_event(&target.track_id, &event, cx);
        let bridge_instance = target.plugin_instance_id.clone();
        let sink_ready = match self
            .plugin_editors
            .bridge_runtime
            .as_ref()
            .map(|rt| rt.try_lock())
        {
            Some(Ok(bridge)) => bridge_instance
                .as_ref()
                .is_some_and(|id| bridge.audio_sink_for(id).is_some()),
            Some(Err(_)) => bridge_instance.is_some(),
            None => false,
        };

        if sink_ready {
            let Some(engine) = self.audio_bridge.engine.as_ref() else {
                return MidiInputRouteStatus::EngineUnavailable;
            };
            let instance_id = bridge_instance.unwrap_or_default();
            let result = match event {
                MidiInputEvent::NoteOn {
                    note,
                    velocity,
                    channel,
                } => engine.plugin_preview_note_on(
                    target.track_id.clone(),
                    instance_id,
                    MidiInputRouter::sanitize_channel(channel),
                    MidiInputRouter::sanitize_note(note),
                    MidiInputRouter::sanitize_velocity(velocity),
                ),
                MidiInputEvent::NoteOff { note, channel } => engine.plugin_preview_note_off(
                    target.track_id.clone(),
                    instance_id,
                    MidiInputRouter::sanitize_channel(channel),
                    MidiInputRouter::sanitize_note(note),
                ),
                MidiInputEvent::ControlChange {
                    controller,
                    value,
                    channel,
                } => engine.plugin_preview_control_change(
                    target.track_id.clone(),
                    instance_id,
                    MidiInputRouter::sanitize_channel(channel),
                    controller.min(127),
                    value.min(127),
                ),
                MidiInputEvent::AllNotesOff | MidiInputEvent::Panic => {
                    engine.plugin_preview_all_notes_off(target.track_id.clone(), instance_id)
                }
            };
            return route_result(result);
        }

        if let Some(instance_id) = bridge_instance.clone() {
            if let Some(runtime) = self.plugin_editors.bridge_runtime.as_ref() {
                if let Ok(mut bridge) = runtime.try_lock() {
                    let result = match event {
                        MidiInputEvent::NoteOn {
                            note,
                            velocity,
                            channel,
                        } => bridge.preview_note_on(
                            instance_id,
                            MidiInputRouter::sanitize_channel(channel),
                            MidiInputRouter::sanitize_note(note),
                            MidiInputRouter::sanitize_velocity(velocity),
                        ),
                        MidiInputEvent::NoteOff { note, channel } => bridge.preview_note_off(
                            instance_id,
                            MidiInputRouter::sanitize_channel(channel),
                            MidiInputRouter::sanitize_note(note),
                        ),
                        MidiInputEvent::ControlChange {
                            controller,
                            value,
                            channel,
                        } => bridge.preview_control_change(
                            instance_id,
                            MidiInputRouter::sanitize_channel(channel),
                            controller.min(127),
                            value.min(127),
                        ),
                        MidiInputEvent::AllNotesOff => bridge.preview_all_notes_off(instance_id),
                        MidiInputEvent::Panic => bridge.midi_panic(instance_id),
                    };
                    return route_result(result);
                }
            }
        }

        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return MidiInputRouteStatus::EngineUnavailable;
        };
        let result = match event {
            MidiInputEvent::NoteOn {
                note,
                velocity,
                channel,
            } => engine.midi_preview_note_on(
                target.track_id.clone(),
                MidiInputRouter::sanitize_channel(channel),
                MidiInputRouter::sanitize_note(note),
                MidiInputRouter::sanitize_velocity(velocity),
            ),
            MidiInputEvent::NoteOff { note, channel } => engine.midi_preview_note_off(
                target.track_id.clone(),
                MidiInputRouter::sanitize_channel(channel),
                MidiInputRouter::sanitize_note(note),
            ),
            MidiInputEvent::ControlChange {
                controller,
                value,
                channel,
            } => engine.midi_preview_control_change(
                target.track_id.clone(),
                MidiInputRouter::sanitize_channel(channel),
                controller.min(127),
                value.min(127),
            ),
            MidiInputEvent::AllNotesOff | MidiInputEvent::Panic => {
                engine.midi_preview_all_notes_off(target.track_id.clone())
            }
        };
        route_result(result)
    }

    pub(super) fn resolve_virtual_keyboard_target(&self, cx: &App) -> VirtualKeyboardTargetStatus {
        let timeline = self.timeline.read(cx);
        let state = &timeline.state;

        let selected = state
            .selection
            .selected_track_id
            .as_deref()
            .and_then(|track_id| state.find_track(track_id))
            .filter(|track| is_keyboard_target_candidate(track));

        let track = selected.or_else(|| {
            state
                .tracks
                .iter()
                .find(|track| track.armed && is_keyboard_target_candidate(track))
        });

        let Some(track) = track else {
            return VirtualKeyboardTargetStatus {
                target: None,
                label: None,
                hint: Some("Select or arm an instrument track to play.".to_string()),
            };
        };

        let plugin_instance_id = track
            .instrument_plugin_instance_id
            .clone()
            .or_else(|| first_instrument_insert_id(track));

        let Some(plugin_instance_id) = plugin_instance_id else {
            // The built-in Soundfont Player is a track instrument rather than a
            // hosted plugin, so it has no instance id — the engine's track MIDI
            // preview reaches it directly.
            if track.builtin_soundfont_player {
                let loaded = track
                    .soundfont_path
                    .as_deref()
                    .is_some_and(|path| !path.is_empty());
                return VirtualKeyboardTargetStatus {
                    target: loaded.then(|| MidiInputTarget {
                        track_id: track.id.clone(),
                        plugin_instance_id: None,
                    }),
                    label: Some(track.name.clone()),
                    hint: (!loaded).then(|| "Load a SoundFont on the selected track.".to_string()),
                };
            }
            return VirtualKeyboardTargetStatus {
                target: None,
                label: Some(track.name.clone()),
                hint: Some("Load an instrument on the selected track.".to_string()),
            };
        };

        VirtualKeyboardTargetStatus {
            target: Some(MidiInputTarget {
                track_id: track.id.clone(),
                plugin_instance_id: Some(plugin_instance_id),
            }),
            label: Some(track.name.clone()),
            hint: None,
        }
    }
}

/// `FUTUREBOARD_VIRTUAL_KEYBOARD_DEBUG=1` also traces routed virtual-keyboard
/// events here (event + resolved target track), complementing the per-key
/// classification trace in `virtual_keyboard.rs`. Cached on first read.
fn virtual_keyboard_debug() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_VIRTUAL_KEYBOARD_DEBUG").is_some())
}

fn route_result<E: std::fmt::Display>(result: Result<(), E>) -> MidiInputRouteStatus {
    match result {
        Ok(()) => MidiInputRouteStatus::Routed,
        Err(error) => MidiInputRouteStatus::DispatchFailed(error.to_string()),
    }
}

fn is_keyboard_target_candidate(track: &components::timeline::timeline_state::TrackState) -> bool {
    matches!(track.track_type, TrackType::Instrument | TrackType::Midi)
}

fn first_instrument_insert_id(
    track: &components::timeline::timeline_state::TrackState,
) -> Option<String> {
    match track.track_type {
        TrackType::Instrument => track
            .instrument_insert()
            .filter(|insert| insert.plugin_id.is_some())
            .map(|insert| insert.id.clone()),
        TrackType::Midi => track
            .inserts
            .first()
            .filter(|insert| insert.plugin_id.is_some())
            .map(|insert| insert.id.clone()),
        _ => None,
    }
}
