use gpui::{App, Context, Window};
use sphere_midi_service::{MidiInputEvent, MidiInputRouteStatus, MidiInputSource, MidiInputTarget};

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::components;
use crate::components::mixer_panel::vsti_output_meter_key;
use crate::components::timeline::timeline_state::{ClipType, TrackOutputRouting, TrackType};

use super::engine_snapshot::{build_engine_project_snapshot, log_engine_sync_snapshot};
use super::helpers::{smooth_meter_value, update_meter_clip, update_meter_hold};
use super::transport_freeze_debug::{self, PlayWatchdog};
use super::{ContextMenuRequest, ContextMenuTarget, ContextTarget, OpenPopover, StudioLayout};

/// Watchdog timeout for an in-flight native engine sync. A `load_project` that
/// hangs (deadlock) never returns and would otherwise pin `sync_in_flight` and
/// the "Sync native engine" task forever. Kept below the loading-session
/// `GRAPH_SYNC_WAIT` (20s) so a stuck sync still resolves to a recoverable error
/// before the session-install wait gives up. Generous enough that a large
/// project / plugin instantiation does not trip a false positive.
const SYNC_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(15);

/// Why the transport playhead is being repositioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekReason {
    UserDragStart,
    UserDragging,
    UserDragEnd,
    TimelineClick,
    RewindForward,
    Programmatic,
}

/// Transient state for the vertical BPM scrub gesture (FL Studio–style infinite
/// drag). Extracted from `StudioLayout` as the first god-struct decomposition
/// slice — every access lives in this module. Built via [`Default`] at studio
/// construction; the drag handler overwrites the relevant fields per gesture.
#[derive(Debug)]
pub(crate) struct BpmDragState {
    /// Active drag id (matches `BpmDragSample::drag_id`); `None` when idle.
    pub active_id: Option<u64>,
    /// Previous cursor Y from the last sample — the per-move delta source.
    pub prev_y: f32,
    /// Accumulated signed BPM offset for the active drag.
    pub accum: f32,
    /// Screen-space cursor anchor (physical px) warped back each move so the
    /// drag never stops at the screen edge.
    pub anchor: Option<(i32, i32)>,
    /// Window scale factor captured at drag start.
    pub scale: f32,
    /// `Some(id)` edits one tempo marker; `None` edits the fixed project BPM.
    pub target_point_id: Option<String>,
    /// BPM value captured at drag start — the accumulation base.
    pub start_value: f32,
}

impl Default for BpmDragState {
    fn default() -> Self {
        Self {
            active_id: None,
            prev_y: 0.0,
            accum: 0.0,
            anchor: None,
            scale: 1.0,
            target_point_id: None,
            start_value: 120.0,
        }
    }
}

/// Throttle / sync timestamps for engine ↔ UI bridging — last applied playhead,
/// last engine snapshot sync, last meter push, and last tempo commit. `Instant`
/// has no `Default`, so this is built via a manual `Default` (now()). Decomp
/// slice; all access lives in this module.
#[derive(Debug)]
pub(crate) struct EngineSyncState {
    /// Last engine playhead beat pushed into timeline state.
    pub playhead_beat: f32,
    /// Last time the engine snapshot was synced to the UI.
    pub synced_at: Instant,
    /// Last time engine meter levels were pushed into timeline state (PowerMode
    /// throttle so low-end GPUs don't repaint 60 Hz for sub-perceptual wiggles).
    pub meter_applied_at: Instant,
    /// Quantised meter signature last pushed to meter-isolated UI regions.
    pub last_meter_notify_sig: u64,
    /// Last rendered footer signature (left/audio/perf pill).
    pub last_status_sig: u64,
    /// Left+audio footer signature for perf-only coalescing.
    pub last_status_left_audio_sig: u64,
    /// Last time footer text was pushed to the status entity.
    pub last_status_poll_at: Instant,
    /// Last time `engine.set_bpm` was sent during a live BPM drag (~30 Hz cap).
    pub bpm_committed_at: Option<Instant>,
    /// Last time the heavy plugin-editor / bridge reconciliation ran. The poll
    /// loop ticks at the display refresh (up to 240 Hz); this caps that
    /// bookkeeping to ~60 Hz so a high-refresh monitor doesn't multiply it.
    pub bridge_reconciled_at: Instant,
}

impl Default for EngineSyncState {
    fn default() -> Self {
        Self {
            playhead_beat: 0.0,
            synced_at: Instant::now(),
            meter_applied_at: Instant::now(),
            last_meter_notify_sig: u64::MAX,
            last_status_sig: u64::MAX,
            last_status_left_audio_sig: u64::MAX,
            last_status_poll_at: Instant::now() - Duration::from_secs(1),
            bpm_committed_at: None,
            bridge_reconciled_at: Instant::now() - Duration::from_secs(1),
        }
    }
}

/// Audio-engine bridge / sync state — the live engine handle, transport stats,
/// last error, dirty flags driving project/media re-sync, and the background
/// sync handshake (in-flight / pending / play-after-sync). `StudioLayout`
/// decomposition slice. Manual `Default` (project/media start dirty so the first
/// sync runs).
pub(crate) struct AudioBridgeState {
    /// Live native audio engine handle; `None` until opened.
    pub engine: Option<DirectAudio::AudioEngine>,
    /// Whether the engine is currently running.
    pub running: bool,
    /// Last audio error surfaced to the UI.
    pub last_error: Option<String>,
    /// Latest engine transport/stats snapshot.
    pub stats: Option<DirectAudio::EngineStats>,
    /// Signature of the last project synced to the engine (skips redundant syncs).
    pub last_project_signature: Option<String>,
    /// Project graph changed and needs re-sync to the engine.
    pub project_dirty: bool,
    /// Media (clips/assets) changed and needs re-sync.
    pub media_dirty: bool,
    /// True while a background `load_project` (file decode) is running.
    pub sync_in_flight: bool,
    /// Fingerprint of the sync currently in flight (`Some` while `sync_in_flight`).
    /// Coalesce/dedup decisions for new requests are made against this.
    pub running_fingerprint: Option<u64>,
    /// At most one coalesced sync queued behind the in-flight one. Holds the graph
    /// fingerprint captured when it was queued; the reschedule rebuilds a fresh
    /// snapshot and re-checks. `None` when nothing is pending.
    pub pending_fingerprint: Option<u64>,
    /// Reason carried by the coalesced pending sync.
    pub pending_reason: Option<&'static str>,
    /// Preserves force=true for a pending sync queued behind an in-flight sync.
    pub pending_force: bool,
    /// Monotonic id of the in-flight sync. A `complete_audio_project_sync` whose
    /// generation no longer matches (the watchdog timeout fired and superseded it)
    /// is ignored — the freshness guard for an orphaned/hung load thread.
    pub sync_generation: u64,
    /// When the in-flight sync started — drives the watchdog timeout that prevents
    /// a hung `load_project` from pinning `sync_in_flight` (and the task) forever.
    pub sync_started_at: Option<Instant>,
    /// Last graph fingerprint whose load failed. Not auto-retried from the dirty
    /// poll (only a genuine graph change or an explicit forced sync retries it),
    /// so a failing graph cannot spin the resync loop.
    pub last_failed_fingerprint: Option<u64>,
    /// Monotonic UI-side route graph publication version for diagnostics.
    pub route_graph_version: u64,
    pub route_graph_in_flight_version: u64,
    pub route_graph_in_flight_child_channels: usize,
    pub route_graph_in_flight_master_routes: usize,
    /// Fingerprint (cheap hash of the engine snapshot) of the last graph actually
    /// published to the audio thread. `None` until the first publish. Used to skip
    /// a redundant route-graph rebuild / `load_project` when the graph is unchanged
    /// — drives the `engine_dirty_poll` and `audio_sync_pending` dedup.
    pub graph_fingerprint: Option<u64>,
    /// Diagnostics counters (drained via `FUTUREBOARD_PERF_DEBUG` / perf HUD).
    /// `engine_sync` = background syncs actually started; `audio_load_project` =
    /// `engine.load_project` calls dispatched; `route_graph_rebuild` = route graph
    /// version bumps. A deduped (unchanged-graph) sync increments none of these.
    pub engine_sync_count: u64,
    pub audio_load_project_count: u64,
    pub route_graph_rebuild_count: u64,
    /// Single-flight diagnostics (drained via `crate::perf::count`). `request` =
    /// every `schedule_audio_project_sync` call past the engine check; `completed`
    /// / `failed` = terminal sync outcomes; `coalesced` = requests folded into the
    /// running/pending sync without starting a new one; `timeout` = watchdog firings.
    pub sync_request_count: u64,
    pub sync_completed_count: u64,
    pub sync_failed_count: u64,
    pub sync_coalesced_count: u64,
    pub sync_timeout_count: u64,
    /// Snapshot construction serializes the whole project. Coalesce rapid UI
    /// gestures (numeric scrubs, sliders) before doing that control-thread work.
    pub last_sync_snapshot_at: Option<Instant>,
    /// Reason string of the most recent sync request (diagnostics only).
    pub last_sync_reason: &'static str,
    /// Start transport once the current background sync completes.
    pub play_after_sync: bool,
    /// Last `EngineStats::dropout_count` seen, to detect new dropouts in the poll.
    pub last_dropout_count: u64,
    /// While set in the future, the status bar shows a coalesced "Audio dropout
    /// detected" notice. Refreshed each time the dropout counter advances, so a
    /// burst of dropouts shows one steady notice instead of flickering per poll.
    pub dropout_notice_until: Option<Instant>,
    /// Reason of the most recent dropout, for the status notice.
    pub last_dropout_reason: String,
    /// Preferred sample rate the user deferred via the "Later" button (Hz), or 0
    /// when nothing is pending. Shared with the Settings latency provider so the
    /// Preferences "restart pending" warning reflects live state. Treated as
    /// resolved once the active device rate matches it.
    pub sample_rate_deferred_target: Arc<AtomicU32>,
    /// While set in the future, the status bar shows a coalesced sample-rate
    /// notice (e.g. an active-vs-requested mismatch after re-opening).
    pub sample_rate_notice_until: Option<Instant>,
    /// Text of the most recent sample-rate notice.
    pub sample_rate_notice_text: String,
}

impl Default for AudioBridgeState {
    fn default() -> Self {
        Self {
            engine: None,
            running: false,
            last_error: None,
            stats: None,
            last_project_signature: None,
            project_dirty: true,
            media_dirty: true,
            sync_in_flight: false,
            running_fingerprint: None,
            pending_fingerprint: None,
            pending_reason: None,
            pending_force: false,
            sync_generation: 0,
            sync_started_at: None,
            last_failed_fingerprint: None,
            route_graph_version: 0,
            route_graph_in_flight_version: 0,
            route_graph_in_flight_child_channels: 0,
            route_graph_in_flight_master_routes: 0,
            graph_fingerprint: None,
            engine_sync_count: 0,
            audio_load_project_count: 0,
            route_graph_rebuild_count: 0,
            sync_request_count: 0,
            sync_completed_count: 0,
            sync_failed_count: 0,
            sync_coalesced_count: 0,
            sync_timeout_count: 0,
            last_sync_snapshot_at: None,
            last_sync_reason: "init",
            play_after_sync: false,
            last_dropout_count: 0,
            dropout_notice_until: None,
            last_dropout_reason: String::new(),
            sample_rate_deferred_target: Arc::new(AtomicU32::new(0)),
            sample_rate_notice_until: None,
            sample_rate_notice_text: String::new(),
        }
    }
}

/// Cheap, stable fingerprint of a serialized engine snapshot. Identical graphs
/// hash identically, so a re-sync of an unchanged graph can be skipped without a
/// full string compare. Control-thread only (never the audio callback).
pub(crate) fn graph_fingerprint_of(signature: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    signature.hash(&mut hasher);
    hasher.finish()
}

impl StudioLayout {
    pub(super) fn dispatch_midi_preview_command(
        &mut self,
        command: components::piano_roll::UiMidiPreviewCommand,
        cx: &App,
    ) {
        let ui_start = Instant::now();
        let track_id = command.track_id().to_string();
        let plugin_instance_id = self.resolve_track_instrument_plugin(&track_id, cx);
        let event = match command {
            components::piano_roll::UiMidiPreviewCommand::NoteOn {
                channel,
                pitch,
                velocity,
                ..
            } => MidiInputEvent::NoteOn {
                note: pitch,
                velocity,
                channel,
            },
            components::piano_roll::UiMidiPreviewCommand::NoteOff { channel, pitch, .. } => {
                MidiInputEvent::NoteOff {
                    note: pitch,
                    channel,
                }
            }
            components::piano_roll::UiMidiPreviewCommand::AllNotesOff { .. } => {
                MidiInputEvent::AllNotesOff
            }
            components::piano_roll::UiMidiPreviewCommand::MidiPanic { .. } => MidiInputEvent::Panic,
        };
        let result = self.route_midi_input_event(
            MidiInputSource::PianoRollPreview,
            MidiInputTarget {
                track_id,
                plugin_instance_id,
            },
            event,
            cx,
        );
        if let MidiInputRouteStatus::DispatchFailed(error) = result {
            eprintln!("[EngineMidiPreview] dispatch failed: {error}");
        }
        if crate::forensic_trace::preview_perf_trace_enabled() {
            eprintln!(
                "[preview-perf] ui_handler_ms={:.3}",
                ui_start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    #[cfg(any())]
    pub(super) fn dispatch_midi_preview_command_legacy(
        &mut self,
        command: components::piano_roll::UiMidiPreviewCommand,
        cx: &App,
    ) {
        let ui_start = Instant::now();
        let track_id = match &command {
            components::piano_roll::UiMidiPreviewCommand::NoteOn { track_id, .. }
            | components::piano_roll::UiMidiPreviewCommand::NoteOff { track_id, .. }
            | components::piano_roll::UiMidiPreviewCommand::AllNotesOff { track_id }
            | components::piano_roll::UiMidiPreviewCommand::MidiPanic { track_id } => {
                track_id.clone()
            }
        };
        let bridge_instance = self.resolve_track_instrument_plugin(&track_id, cx);
        // Per-note hot path: never block the GPUI thread on the bridge runtime
        // mutex. A background save / plugin-state capture (`request_plugin_states`,
        // ~1.5s) or a plugin load can hold it, and pressing notes or muting while
        // it was held used to freeze the UI. Probe with `try_lock`; if it is busy
        // but this track has a bridged instrument, route through the lock-free
        // engine command bus anyway (the correct path once a bridge plugin exists).
        let sink_ready = match self
            .plugin_editors
            .bridge_runtime
            .as_ref()
            .map(|rt| rt.try_lock())
        {
            Some(Ok(bridge)) => bridge
                .loaded_instance_ids()
                .into_iter()
                .any(|id| bridge.audio_sink_for(&id).is_some()),
            Some(Err(_)) => bridge_instance.is_some(),
            None => false,
        };

        if sink_ready {
            // Shared-memory bridge is live — always route through the main DAW
            // engine (track → mixer → master), even if timeline runtime_backend
            // has not caught up yet.
            let Some(engine) = self.audio_bridge.engine.as_ref() else {
                eprintln!("[midi-preview-ui] engine_command_bus_connected=false");
                return;
            };
            let instance_id = bridge_instance
                .clone()
                .unwrap_or_else(|| "bridge".to_string());
            let result = match &command {
                components::piano_roll::UiMidiPreviewCommand::NoteOn {
                    channel,
                    pitch,
                    velocity,
                    ..
                } => {
                    eprintln!(
                        "[midi-preview-ui] note_on track={track_id} pitch={pitch} velocity={velocity} source=piano_key"
                    );
                    engine.plugin_preview_note_on(
                        track_id.clone(),
                        instance_id.clone(),
                        *channel,
                        *pitch,
                        *velocity,
                    )
                }
                components::piano_roll::UiMidiPreviewCommand::NoteOff {
                    channel, pitch, ..
                } => {
                    eprintln!(
                        "[midi-preview-ui] note_off track={track_id} pitch={pitch} source=piano_key"
                    );
                    engine.plugin_preview_note_off(
                        track_id.clone(),
                        instance_id.clone(),
                        *channel,
                        *pitch,
                    )
                }
                components::piano_roll::UiMidiPreviewCommand::AllNotesOff { .. } => {
                    engine.plugin_preview_all_notes_off(track_id.clone(), instance_id.clone())
                }
                components::piano_roll::UiMidiPreviewCommand::MidiPanic { .. } => {
                    engine.plugin_preview_all_notes_off(track_id.clone(), instance_id.clone())
                }
            };
            if let Err(error) = result {
                eprintln!("[EngineMidiPreview] bridge dispatch failed: {error}");
            }
            if crate::forensic_trace::preview_perf_trace_enabled() {
                eprintln!(
                    "[preview-perf] ui_handler_ms={:.3}",
                    ui_start.elapsed().as_secs_f64() * 1000.0
                );
            }
            return;
        }

        if let Some(instance_id) = bridge_instance {
            if let Some(runtime) = self.plugin_editors.bridge_runtime.as_ref() {
                // try_lock, not lock: this IPC fallback runs on the GPUI thread
                // and must not stall it if a background bridge op holds the mutex.
                // On contention we fall through to the engine MIDI-preview path.
                if let Ok(mut bridge) = runtime.try_lock() {
                    let result = match command {
                        components::piano_roll::UiMidiPreviewCommand::NoteOn {
                            channel,
                            pitch,
                            velocity,
                            ..
                        } => {
                            eprintln!(
                                "[midi-preview-ui] note_on source=piano_roll pitch={pitch} velocity={velocity} instance={instance_id} (IPC fallback — DSP bridge pending)"
                            );
                            bridge.preview_note_on(instance_id.clone(), channel, pitch, velocity)
                        }
                        components::piano_roll::UiMidiPreviewCommand::NoteOff {
                            channel,
                            pitch,
                            ..
                        } => bridge.preview_note_off(instance_id.clone(), channel, pitch),
                        components::piano_roll::UiMidiPreviewCommand::AllNotesOff { .. } => {
                            bridge.preview_all_notes_off(instance_id.clone())
                        }
                        components::piano_roll::UiMidiPreviewCommand::MidiPanic { .. } => {
                            bridge.midi_panic(instance_id.clone())
                        }
                    };
                    if let Err(error) = result {
                        eprintln!("[plugin-bridge] midi preview dispatch failed: {error}");
                    }
                    return;
                }
            }
        }

        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            eprintln!("[PopoutMidiEditor] engine_command_bus_connected=false");
            return;
        };
        eprintln!("[PopoutMidiEditor] engine_command_bus_connected=true");
        let result = match command {
            components::piano_roll::UiMidiPreviewCommand::NoteOn {
                channel,
                pitch,
                velocity,
                ..
            } => {
                eprintln!(
                    "[PopoutMidiEditor] active_track_id={} dispatch PreviewNoteOn -> engine",
                    track_id
                );
                engine.midi_preview_note_on(track_id, channel, pitch, velocity)
            }
            components::piano_roll::UiMidiPreviewCommand::NoteOff { channel, pitch, .. } => {
                eprintln!(
                    "[PopoutMidiEditor] active_track_id={} dispatch PreviewNoteOff -> engine",
                    track_id
                );
                engine.midi_preview_note_off(track_id, channel, pitch)
            }
            components::piano_roll::UiMidiPreviewCommand::AllNotesOff { .. } => {
                eprintln!(
                    "[PopoutMidiEditor] active_track_id={} dispatch PreviewAllNotesOff -> engine",
                    track_id
                );
                engine.midi_preview_all_notes_off(track_id)
            }
            components::piano_roll::UiMidiPreviewCommand::MidiPanic { .. } => {
                engine.midi_preview_all_notes_off(track_id)
            }
        };
        if let Err(error) = result {
            eprintln!("[EngineMidiPreview] dispatch failed: {error}");
        }
    }

    fn bridge_instrument_instance_id(&self, track_id: &str, cx: &App) -> Option<String> {
        use crate::components::timeline::timeline_state::{PluginRuntimeBackend, TrackType};
        let timeline = self.timeline.read(cx);
        let track = timeline.state.find_track(track_id)?;
        let insert = match track.track_type {
            TrackType::Instrument => track.instrument_insert()?,
            TrackType::Midi => track.inserts.first()?,
            _ => return None,
        };
        if insert.runtime_backend != PluginRuntimeBackend::ExternalBridge {
            return None;
        }
        Some(insert.id.clone())
    }

    pub(super) fn resolve_track_instrument_plugin(
        &self,
        track_id: &str,
        cx: &App,
    ) -> Option<String> {
        let timeline = self.timeline.read(cx);
        let track = timeline.state.find_track(track_id)?;
        if let Some(instance_id) = track.instrument_plugin_instance_id.as_ref() {
            return Some(instance_id.clone());
        }
        if let Some(instance_id) = self.bridge_instrument_instance_id(track_id, cx) {
            return Some(instance_id);
        }
        match track.track_type {
            crate::components::timeline::timeline_state::TrackType::Instrument => track
                .instrument_insert()
                .filter(|slot| slot.plugin_id.is_some())
                .map(|slot| slot.id.clone()),
            crate::components::timeline::timeline_state::TrackType::Midi => track
                .inserts
                .first()
                .filter(|slot| slot.plugin_id.is_some())
                .map(|slot| slot.id.clone()),
            _ => None,
        }
        .or_else(|| {
            self.plugin_editors
                .bridge_runtime
                .as_ref()
                .and_then(|runtime| {
                    runtime
                        .lock()
                        .ok()
                        .and_then(|bridge| bridge.loaded_for_track(track_id))
                        .map(|loaded| loaded.descriptor.insert_id)
                })
        })
    }

    pub(super) fn spawn_audio_poll(cx: &mut Context<Self>) {
        // The poll cadence is owned by the frame scheduler (display-synced by
        // default, fixed/battery caps for settings/debug). The loop reads the
        // scheduler's lock-free interval each tick, so a mode change applies on
        // the next iteration. The engine produces position snapshots at
        // audio-block boundaries (~5-10 ms); the UI never repaints faster than
        // the chosen cadence and `interpolated_playhead_beat` smooths between
        // engine snapshots. Until the entity exists we fall back to ~60 Hz.
        const FALLBACK_INTERVAL_NANOS: u64 = 16_666_667; // ~60 Hz
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let mut interval_handle: Option<Arc<AtomicU64>> = None;
            loop {
                if crate::shutdown::ShutdownState::global().is_shutting_down() {
                    break;
                }
                if interval_handle.is_none() {
                    interval_handle = this
                        .update(cx, |this, _| this.frame_scheduler.continuous_nanos_handle())
                        .ok();
                }
                let interval_nanos = interval_handle
                    .as_ref()
                    .map(|h| h.load(Ordering::Relaxed))
                    .filter(|n| *n > 0)
                    .unwrap_or(FALLBACK_INTERVAL_NANOS);
                executor.timer(Duration::from_nanos(interval_nanos)).await;
                if crate::shutdown::ShutdownState::global().is_shutting_down() {
                    break;
                }
                let Ok((changed, mixer_handle)) = this.update(cx, |this, cx| {
                    if crate::shutdown::ShutdownState::global().is_shutting_down() {
                        return (false, None);
                    }
                    let changed = this.poll_native_audio(cx);
                    (changed, this.external_windows.mixer.clone())
                }) else {
                    continue;
                };
                if changed && !crate::shutdown::ShutdownState::global().is_shutting_down() {
                    crate::perf::record_notify("transport");
                    let studio_id = this.entity_id();
                    let _ = cx.update(|app| app.notify(studio_id));
                    if mixer_handle.is_some() {
                        let _ = this.update(cx, |layout, cx| {
                            layout.push_mixer_meter_snapshot_throttled(cx);
                        });
                    }
                }
            }
        })
        .detach();
    }

    pub(super) fn poll_native_audio(&mut self, cx: &mut Context<Self>) -> bool {
        if crate::shutdown::ShutdownState::global().is_shutting_down() {
            return false;
        }
        let _s = crate::perf::PerfScope::enter("poll_native_audio");
        if self.audio_bridge.engine.is_none() {
            return false;
        }

        // Watchdog: a hung `load_project` must not pin the sync lifecycle. If the
        // in-flight sync has exceeded the timeout, fail it recoverably and free the
        // state so a retry can proceed and the engine returns to ready.
        if self.audio_bridge.sync_in_flight {
            if let Some(started) = self.audio_bridge.sync_started_at {
                if started.elapsed() >= SYNC_WATCHDOG_TIMEOUT {
                    self.timeout_audio_project_sync(cx);
                }
            }
        }

        if self.audio_bridge.project_dirty || self.audio_bridge.media_dirty {
            self.schedule_audio_project_sync(cx, false, "engine_dirty_poll");
        }

        // Backstop: close editors whose track/insert was removed by any path
        // (notably the track-header delete button, which mutates the Timeline
        // entity directly and never reaches the StudioLayout delete commands).
        //
        // This bookkeeping is UI-sync only (not realtime) and doesn't need to
        // run at the display refresh, so cap it to ~60 Hz. On a 60 Hz monitor
        // the poll already ticks at ~16 ms so this runs every tick (unchanged);
        // on a 144 Hz monitor it stops the work from tripling.
        if self.engine_sync.bridge_reconciled_at.elapsed() >= Duration::from_millis(16) {
            self.engine_sync.bridge_reconciled_at = Instant::now();
            self.reconcile_open_plugin_editors(cx);
            self.poll_plugin_bridge_runtime(cx);
            // Drive native main-owned editor shells: honor OS close + forward resizes.
            self.drive_bridge_editors(cx);
        }

        let engine = self.audio_bridge.engine.as_ref().expect("checked above");
        // Throttled raw/bus input-peak trace (gated by FUTUREBOARD_INPUT_DEBUG).
        engine.log_input_debug();
        let stats = engine.stats();
        // State-transition signal — used to notify the root layout even
        // when the transport is paused (e.g. error appears, stream opens).
        let state_changed = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|previous| {
                previous.transport_playing != stats.transport_playing
                    || previous.running != stats.running
                    || previous.last_error != stats.last_error
            })
            .unwrap_or(true);
        self.audio_bridge.running = stats.running;
        self.audio_bridge.last_error = stats.last_error.clone();

        let engine_beat = stats.position_beats.max(0.0) as f32;
        let sync_changed = (engine_beat - self.engine_sync.playhead_beat).abs() > 0.0001
            || self
                .audio_bridge
                .stats
                .as_ref()
                .map(|previous| previous.transport_playing != stats.transport_playing)
                .unwrap_or(true);
        if sync_changed {
            self.engine_sync.playhead_beat = engine_beat;
            self.engine_sync.synced_at = Instant::now();
        }
        let meter_changed = self.apply_engine_meters(cx);

        // Drain hardware MIDI input on the UI/control poll (never the audio
        // thread). Opens/refreshes enabled ports, then routes into the same
        // preview + recording path as the virtual keyboard.
        self.poll_hardware_midi_input(cx);

        // Realtime recording waveform preview (Part 1) — grow the preview clip
        // and append streamed peaks. Self-contained; notifies the timeline.
        self.update_recording_preview(cx);

        if stats.transport_playing {
            let bpm = {
                let timeline = self.timeline.read(cx);
                timeline.state.bpm
            };
            let interpolated = self.interpolated_playhead_beat(bpm);
            let _ = self.timeline.update(cx, move |timeline, cx| {
                timeline.state.transport.playing = true;
                // No threshold while playing — even sub-pixel beat motion
                // matters for the bar:beat:tick readout in the chrome.
                let next = interpolated.max(0.0);
                let mut dirty = false;
                if timeline.state.transport.playhead_beats != next {
                    timeline.state.transport.playhead_beats = next;
                    dirty = true;
                }
                // Follow Track Volume automation: refresh each track's effective
                // volume so the mixer/track-header/inspector fader track the curve
                // during playback. UI-only (faders read `display_volume`), so this
                // never writes the base value or fires a user-edit command.
                if timeline
                    .state
                    .recompute_effective_volumes(next, "playback_tick")
                {
                    dirty = true;
                }
                // Follow-playhead / auto-scroll. Keeps the playhead visible
                // during playback; user-scroll temporarily disables it via
                // `note_user_scrolled`. Cheap — no rebuild, just viewport
                // scroll_x update.
                if timeline.state.update_auto_scroll_for_playhead(next) {
                    dirty = true;
                }
                if dirty {
                    cx.notify();
                }
            });
        } else {
            let _ = self.timeline.update(cx, |timeline, cx| {
                if timeline.state.transport.playing {
                    timeline.state.transport.playing = false;
                    cx.notify();
                }
            });
        }

        // Coalesced dropout notice: when the realtime dropout counter advances,
        // hold a short status notice (refreshed on each new dropout) instead of
        // spamming the UI. Counters/atomics only — no audio-thread interaction.
        if stats.dropout_count > self.audio_bridge.last_dropout_count {
            let new_dropouts = stats.dropout_count - self.audio_bridge.last_dropout_count;
            self.audio_bridge.last_dropout_count = stats.dropout_count;
            self.audio_bridge.last_dropout_reason = stats.dropout_last_reason.clone();
            self.audio_bridge.dropout_notice_until = Some(Instant::now() + Duration::from_secs(4));
            crate::perf::count("audio_dropout_count", stats.dropout_count);
            crate::perf::count("audio_dropout_recent", new_dropouts);
        }

        let was_playing = stats.transport_playing;
        self.audio_bridge.stats = Some(stats);

        if meter_changed {
            self.notify_mixer_meter_regions(cx);
        }

        let status_due = state_changed
            || self.engine_sync.last_status_poll_at.elapsed() >= Duration::from_millis(250);
        if status_due {
            self.notify_status_bar_if_changed(cx);
        }

        // While playing the root layout must repaint every tick so the
        // transport chrome (bar:beat:tick, status line) tracks the
        // playhead. Meter-only changes route to isolated mixer/timeline
        // entities and must not invalidate the full studio shell when idle.
        state_changed || was_playing
    }

    /// Block-rate automation evaluation scaffolding. Evaluates each track's
    /// Volume/Pan automation lane at the current playhead beat. Engine
    /// application — pushing the value into the realtime parameter queue — is
    /// the next phase; doing it here at 60 Hz unconditionally would both spam
    /// the engine and fight the UI fader, so for now we only trace under
    /// `FUTUREBOARD_AUTOMATION_DEBUG`. The evaluation itself is real (see
    /// [`evaluate_automation`]) so the wiring point is ready for the param
    /// queue without faking any data.
    pub(super) fn evaluate_block_automation(&self, cx: &mut Context<Self>, beat: f32) {
        use crate::components::timeline::timeline_state::{
            automation_debug_enabled, evaluate_automation, AutomationTarget,
        };
        if !automation_debug_enabled() {
            return;
        }
        let timeline = self.timeline.read(cx);
        for track in &timeline.state.tracks {
            for lane in &track.automation_lanes {
                if lane.points.is_empty()
                    || !matches!(
                        lane.target,
                        AutomationTarget::TrackVolume | AutomationTarget::TrackPan
                    )
                {
                    continue;
                }
                let value =
                    evaluate_automation(&lane.points, beat as f64, lane.target.default_value());
                eprintln!(
                    "[automation] evaluate track={} beat={:.3} value={:.3} target={}",
                    track.id,
                    beat,
                    value,
                    lane.target.display_name()
                );
                // TODO(engine): push `value` into the realtime parameter queue
                // for `track.id` (volume/pan) — lock-free, no allocations on the
                // audio-control path — instead of only tracing here.
            }
        }
    }

    pub(super) fn interpolated_playhead_beat(&self, bpm: f32) -> f32 {
        let playing = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|stats| stats.transport_playing)
            .unwrap_or(false);
        if !playing {
            return self.engine_sync.playhead_beat;
        }
        self.engine_sync.playhead_beat
            + self.engine_sync.synced_at.elapsed().as_secs_f32() * bpm.max(1.0) / 60.0
    }

    /// Update smoothed meter levels in timeline state. Does not call
    /// `cx.notify` — repaints are driven by the audio poll when transport
    /// is active, or by user interaction when idle.
    pub(super) fn apply_engine_meters(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return false;
        };
        // Meter reads are lightweight snapshot pulls; render cadence comes from
        // the frame scheduler so 120/144 Hz displays do not step at a fixed low
        // FPS. Ballistics below use the actual elapsed time, so throttling or
        // brief stalls do not change the apparent attack/release speed.
        let min_interval = self.frame_scheduler.meter_min_interval();
        let now = Instant::now();
        let elapsed = now.duration_since(self.engine_sync.meter_applied_at);
        if elapsed < min_interval {
            return false;
        }
        self.engine_sync.meter_applied_at = now;
        let meter_dt = elapsed.as_secs_f32().clamp(1.0 / 240.0, 0.1);
        // Split the owned meter snapshot into its fields so the timeline closure
        // can take `tracks` by value without cloning `plugin_outputs` (consumed
        // afterwards). Moving fields is cheaper than the previous per-tick
        // `plugin_outputs.clone()` on this playback hot path. `master_peak_*` are
        // `f64` (Copy), so the forensic trace and the closure can both read them.
        let meters = engine.meters();
        let meter_tracks = meters.tracks;
        let plugin_output_meters = meters.plugin_outputs;
        let master_peak_l = meters.master_peak_l;
        let master_peak_r = meters.master_peak_r;
        if crate::forensic_trace::forensic_trace_enabled() {
            for track_meter in &meter_tracks {
                let peak = track_meter.peak_l.max(track_meter.peak_r);
                if peak > 0.0001 {
                    eprintln!("[Meter] track={} peak={peak:.6}", track_meter.track_id);
                }
            }
            let master_peak = master_peak_l.max(master_peak_r);
            if master_peak > 0.0001 {
                eprintln!("[Meter] master peak={master_peak:.6}");
            }
        }
        let mut changed = self.timeline.update(cx, |timeline, _cx| {
            let mut changed = false;
            for track_meter in meter_tracks {
                if let Some(track) = timeline
                    .state
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_meter.track_id)
                {
                    let next_l = track_meter.peak_l.clamp(0.0, 1.0) as f32;
                    let next_r = track_meter.peak_r.clamp(0.0, 1.0) as f32;
                    if crate::forensic_trace::forensic_trace_enabled()
                        && track.id.starts_with("vsti-out:")
                        && next_l.max(next_r) > 0.0001
                    {
                        let bus_index = track
                            .id
                            .rsplit_once(":bus:")
                            .and_then(|(_, bus)| bus.parse::<u8>().ok())
                            .unwrap_or(0);
                        let plugin_instance_id = track
                            .id
                            .strip_prefix("vsti-out:")
                            .and_then(|rest| rest.split_once(":bus:").map(|(plugin, _)| plugin))
                            .unwrap_or("");
                        eprintln!(
                            "[METER PUBLISH]\naudio_callback_seq=0\nplugin_instance_id={plugin_instance_id}\nbus_index={bus_index}\nmixer_channel_id={}\npeak_l={:.6}\npeak_r={:.6}\nrms_l={:.6}\nrms_r={:.6}\nsubscriber_count=1",
                            track.id,
                            track_meter.peak_l,
                            track_meter.peak_r,
                            track_meter.rms_l,
                            track_meter.rms_r
                        );
                    }
                    changed |= smooth_meter_value(&mut track.meter_level_l, next_l, meter_dt);
                    changed |= smooth_meter_value(&mut track.meter_level_r, next_r, meter_dt);
                    update_meter_hold(
                        &mut track.meter_peak_hold_l,
                        track.meter_level_l,
                        meter_dt,
                    );
                    update_meter_hold(
                        &mut track.meter_peak_hold_r,
                        track.meter_level_r,
                        meter_dt,
                    );
                    update_meter_clip(
                        &mut track.meter_clip,
                        track_meter.peak_l,
                        track_meter.peak_r,
                        track.meter_peak_hold_l.max(track.meter_peak_hold_r),
                    );
                }
            }
            let master = &mut timeline.state.master;
            changed |= smooth_meter_value(
                &mut master.meter_level_l,
                master_peak_l.clamp(0.0, 1.0) as f32,
                meter_dt,
            );
            changed |= smooth_meter_value(
                &mut master.meter_level_r,
                master_peak_r.clamp(0.0, 1.0) as f32,
                meter_dt,
            );
            update_meter_hold(&mut master.meter_peak_hold_l, master.meter_level_l, meter_dt);
            update_meter_hold(&mut master.meter_peak_hold_r, master.meter_level_r, meter_dt);
            update_meter_clip(
                &mut master.meter_clip,
                master_peak_l,
                master_peak_r,
                master.meter_peak_hold_l.max(master.meter_peak_hold_r),
            );
            changed
        });
        // Reuse the persistent scratch set (retains its capacity across ticks)
        // rather than allocating a fresh `HashSet` every meter update.
        let mut live_keys = std::mem::take(&mut self.mixer_view.vsti_meter_live_keys);
        live_keys.clear();
        for meter in plugin_output_meters {
            let channel = meter.channel.clamp(1, 32) as u8;
            let key = vsti_output_meter_key(&meter.track_id, &meter.insert_id, channel);
            live_keys.insert(key.clone());
            let entry = self.mixer_view.vsti_output_meters.entry(key).or_default();
            let next = meter.peak.clamp(0.0, 1.0) as f32;
            if crate::forensic_trace::forensic_trace_enabled() && next > 0.0001 {
                let bus_index = (channel.saturating_sub(1)) / 2;
                let mixer_channel_id =
                    crate::components::timeline::timeline_state::vsti_output_child_track_id(
                        &meter.insert_id,
                        bus_index,
                    );
                eprintln!(
                    "[METER PUBLISH]\naudio_callback_seq=0\nplugin_instance_id={}\nbus_index={}\nmixer_channel_id={}\npeak_l={:.6}\npeak_r={:.6}\nrms_l=0.000000\nrms_r=0.000000\nsubscriber_count=1",
                    meter.insert_id,
                    bus_index,
                    mixer_channel_id,
                    meter.peak,
                    meter.peak
                );
            }
            changed |= smooth_meter_value(&mut entry.level, next, meter_dt);
            update_meter_hold(&mut entry.peak_hold, entry.level, meter_dt);
            update_meter_clip(&mut entry.clip, meter.peak, meter.peak, entry.peak_hold);
        }
        self.mixer_view.vsti_output_meters.retain(|key, meter| {
            if live_keys.contains(key) {
                return true;
            }
            let mut keep = false;
            changed |= smooth_meter_value(&mut meter.level, 0.0, meter_dt);
            update_meter_hold(&mut meter.peak_hold, meter.level, meter_dt);
            update_meter_clip(&mut meter.clip, 0.0, 0.0, meter.peak_hold);
            if meter.level > 0.0 || meter.peak_hold > 0.0 || meter.clip {
                keep = true;
            }
            keep
        });
        // Hand the scratch set back so its allocation is reused next tick.
        self.mixer_view.vsti_meter_live_keys = live_keys;
        changed
    }

    /// Queue a background engine sync. `load_project` decodes media on the
    /// caller thread — never invoke it from the UI poll loop or render path.
    pub(crate) fn schedule_audio_project_sync(
        &mut self,
        cx: &mut Context<Self>,
        force: bool,
        reason: &'static str,
    ) {
        if crate::shutdown::ShutdownState::global().is_shutting_down() {
            return;
        }
        let Some(engine) = self.audio_bridge.engine.clone() else {
            self.audio_bridge.last_error = Some("audio engine unavailable".to_string());
            return;
        };
        self.audio_bridge.sync_request_count =
            self.audio_bridge.sync_request_count.saturating_add(1);
        crate::perf::count("sync_request_count", self.audio_bridge.sync_request_count);
        self.audio_bridge.last_sync_reason = reason;

        // Cheap gate: nothing to publish unless forced or the graph is dirty. Dirty
        // is cleared the moment a sync is committed below, so a quiescent graph
        // never reaches the (locking) snapshot build — this keeps the per-frame
        // `engine_dirty_poll` from rebuilding a snapshot every tick while a sync is
        // in flight.
        if !force && !self.audio_bridge.project_dirty && !self.audio_bridge.media_dirty {
            return;
        }

        // A scrub can emit dozens of UI updates per second. Building a full
        // serialized engine snapshot for each one blocks the UI and provides no
        // audible benefit; retain dirty state and let the poll publish the most
        // recent value on the next short control-rate slot.
        const UI_GESTURE_SYNC_INTERVAL: Duration = Duration::from_millis(75);
        if !force
            && self
                .audio_bridge
                .last_sync_snapshot_at
                .is_some_and(|last| last.elapsed() < UI_GESTURE_SYNC_INTERVAL)
        {
            return;
        }
        self.audio_bridge.last_sync_snapshot_at = Some(Instant::now());

        let sample_rate = self.current_audio_sample_rate();
        let graph_version_before = self.audio_bridge.route_graph_version;
        let project_root = self
            .project_folder
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let preferred_input_device = {
            let settings = self.settings.read(cx);
            settings.current.hardware.audio.device_in.clone()
        };
        let preferred_output_device = {
            let settings = self.settings.read(cx);
            settings.current.hardware.audio.device_out.clone()
        };
        eprintln!(
            "[AudioSettings] selected input device = {:?}",
            preferred_input_device
        );
        eprintln!(
            "[AudioSettings] selected output device = {:?}",
            preferred_output_device
        );
        let snapshot = {
            let timeline = self.timeline.read(cx);
            build_engine_project_snapshot(
                &timeline.state,
                sample_rate,
                project_root.as_deref(),
                Some(preferred_input_device.as_str()),
            )
        };
        log_engine_sync_snapshot(
            &snapshot,
            self.audio_bridge.project_dirty || self.audio_bridge.media_dirty,
            reason,
        );
        let signature = serde_json::to_string(&snapshot).unwrap_or_default();
        let fingerprint = graph_fingerprint_of(&signature);

        // Single-flight coalescing. A sync is already running; fold this request
        // into at most one pending sync rather than starting a second. Only a
        // genuinely different graph (or an explicit force) needs to run after the
        // current sync — a poll re-fire for the same graph is counted and dropped.
        // Dirty is cleared here because the running/pending sync now represents this
        // request; that is what stops the poll from re-queuing every tick and
        // pinning "Sync native engine" forever.
        if self.audio_bridge.sync_in_flight {
            // "Changed" vs the graph currently being synced and any already-queued
            // pending graph — NOT vs the last *successful* graph, so a revert to a
            // previously-published state mid-sync is still queued (the running sync
            // is publishing a different graph and would otherwise win).
            let changed = force
                || (Some(fingerprint) != self.audio_bridge.running_fingerprint
                    && Some(fingerprint) != self.audio_bridge.pending_fingerprint);
            if changed {
                self.audio_bridge.pending_fingerprint = Some(fingerprint);
                self.audio_bridge.pending_reason = Some(reason);
                self.audio_bridge.pending_force |= force;
                self.queue_background_task(
                    "native-sync-pending",
                    crate::components::BackgroundTaskKind::NativeSync,
                    "Sync native engine",
                    Some("Queued behind current engine sync".to_string()),
                );
            } else {
                self.audio_bridge.sync_coalesced_count =
                    self.audio_bridge.sync_coalesced_count.saturating_add(1);
                crate::perf::count(
                    "sync_coalesced_count",
                    self.audio_bridge.sync_coalesced_count,
                );
            }
            self.audio_bridge.project_dirty = false;
            self.audio_bridge.media_dirty = false;
            return;
        }

        // Graph-unchanged / known-failed dedup. The same graph was already
        // published to the audio thread (or just failed to load), so skip the
        // route-graph rebuild and `load_project` entirely. This covers
        // `engine_dirty_poll` re-firing after a completed sync. Fingerprint is the
        // documented key; the exact signature check guards the (astronomically
        // unlikely) hash collision. A known-failed graph is not auto-retried from
        // the poll — only a real change (different fingerprint) or an explicit
        // forced sync retries it, so a failing load cannot spin the resync loop.
        if !force
            && ((self.audio_bridge.graph_fingerprint == Some(fingerprint)
                && self.audio_bridge.last_project_signature.as_deref() == Some(signature.as_str()))
                || self.audio_bridge.last_failed_fingerprint == Some(fingerprint))
        {
            self.audio_bridge.project_dirty = false;
            self.audio_bridge.media_dirty = false;
            if self.audio_bridge.play_after_sync {
                self.audio_bridge.play_after_sync = false;
                self.start_native_playback(cx);
            }
            return;
        }

        // Commit to a new sync. Clear dirty now (not at completion): the in-flight
        // sync captures this exact graph, so the poll must not re-trigger for it.
        // A real edit during the sync re-dirties and is coalesced into `pending`.
        self.audio_bridge.project_dirty = false;
        self.audio_bridge.media_dirty = false;
        self.audio_bridge.running_fingerprint = Some(fingerprint);
        self.audio_bridge.sync_generation = self.audio_bridge.sync_generation.wrapping_add(1);
        self.audio_bridge.sync_started_at = Some(Instant::now());
        let sync_generation = self.audio_bridge.sync_generation;
        self.audio_bridge.sync_in_flight = true;
        self.audio_bridge.engine_sync_count = self.audio_bridge.engine_sync_count.saturating_add(1);
        crate::perf::count("engine_sync_count", self.audio_bridge.engine_sync_count);
        self.audio_bridge.route_graph_version =
            self.audio_bridge.route_graph_version.saturating_add(1);
        self.audio_bridge.route_graph_rebuild_count = self
            .audio_bridge
            .route_graph_rebuild_count
            .saturating_add(1);
        crate::perf::count(
            "route_graph_rebuild_count",
            self.audio_bridge.route_graph_rebuild_count,
        );
        let graph_version_after = self.audio_bridge.route_graph_version;
        let num_plugin_child_channels = snapshot
            .tracks
            .iter()
            .filter(|track| track.id.starts_with("vsti-out:"))
            .count();
        let num_routes_to_master = snapshot
            .tracks
            .iter()
            .filter(|track| track.id.starts_with("vsti-out:") && track.output_track_id.is_none())
            .count();
        self.audio_bridge.route_graph_in_flight_version = graph_version_after;
        self.audio_bridge.route_graph_in_flight_child_channels = num_plugin_child_channels;
        self.audio_bridge.route_graph_in_flight_master_routes = num_routes_to_master;
        // The routing graph just advanced (tracks/buses/sends/plugin child outputs
        // changed). Push the new version to the Mixer Tree Sidebar so it rebuilds
        // once, now — this is what makes the tree appear on first Studio open: the
        // handoff built the cache before the real graph was ready, and nothing else
        // re-triggered it. Meter/fader updates don't reach here (graph unchanged →
        // deduped above), so this does not rebuild the tree on transient updates.
        self.refresh_mixer_tree_sidebar_entity(cx);
        eprintln!(
            "[ROUTE GRAPH REBUILD]\nreason={reason}\ngraph_version_before={graph_version_before}\ngraph_version_after={graph_version_after}\nnum_plugin_child_channels={num_plugin_child_channels}\nnum_routes_to_master={num_routes_to_master}\npublished_to_audio_thread=false"
        );
        self.start_background_task(
            "native-sync",
            crate::components::BackgroundTaskKind::NativeSync,
            "Sync native engine",
            Some(reason.to_string()),
            None,
            false,
        );
        self.audio_bridge.audio_load_project_count =
            self.audio_bridge.audio_load_project_count.saturating_add(1);
        crate::perf::count(
            "audio_load_project_count",
            self.audio_bridge.audio_load_project_count,
        );
        let owner = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            let join = std::thread::Builder::new()
                .name("audio-project-load".into())
                .spawn(move || engine.load_project(snapshot));
            let result = match join {
                Ok(handle) => handle.join().unwrap_or_else(|_| {
                    Err(DirectAudio::SphereAudioError::NativeError(
                        "audio project load thread panicked".to_string(),
                    ))
                }),
                Err(error) => Err(DirectAudio::SphereAudioError::NativeError(format!(
                    "failed to spawn audio project load thread: {error}"
                ))),
            };
            let _ = owner.update(cx, |this, cx| {
                this.complete_audio_project_sync(cx, result, signature, sync_generation);
            });
            if !crate::shutdown::ShutdownState::global().is_shutting_down() {
                let studio_id = owner.entity_id();
                let _ = cx.update(|app| app.notify(studio_id));
            }
        })
        .detach();
    }

    pub(super) fn complete_audio_project_sync(
        &mut self,
        cx: &mut Context<Self>,
        result: Result<(), DirectAudio::SphereAudioError>,
        signature: String,
        generation: u64,
    ) {
        // Freshness guard: ignore a completion that belongs to a superseded sync.
        // The watchdog timeout (`timeout_audio_project_sync`) bumps the generation
        // and frees the lifecycle when a `load_project` hangs; if that orphaned
        // thread later returns, this stale completion must not clobber the current
        // state or re-flag a task that already reached a terminal status.
        if generation != self.audio_bridge.sync_generation {
            return;
        }
        self.audio_bridge.sync_in_flight = false;
        self.audio_bridge.sync_started_at = None;
        let finished_fingerprint = self.audio_bridge.running_fingerprint.take();
        match result {
            Ok(()) => {
                self.audio_bridge.sync_completed_count =
                    self.audio_bridge.sync_completed_count.saturating_add(1);
                crate::perf::count(
                    "sync_completed_count",
                    self.audio_bridge.sync_completed_count,
                );
                eprintln!(
                    "[ROUTE GRAPH REBUILD]\nreason=audio_project_sync_complete\ngraph_version_before={}\ngraph_version_after={}\nnum_plugin_child_channels={}\nnum_routes_to_master={}\npublished_to_audio_thread=true",
                    self.audio_bridge
                        .route_graph_in_flight_version
                        .saturating_sub(1),
                    self.audio_bridge.route_graph_in_flight_version,
                    self.audio_bridge.route_graph_in_flight_child_channels,
                    self.audio_bridge.route_graph_in_flight_master_routes
                );
                // Record the published graph's fingerprint so a later
                // engine_dirty_poll for the identical graph is deduped in
                // `schedule_audio_project_sync` (no second rebuild). A successful
                // publish also clears any prior failed-graph marker.
                self.audio_bridge.graph_fingerprint =
                    finished_fingerprint.or(Some(graph_fingerprint_of(&signature)));
                self.audio_bridge.last_project_signature = Some(signature);
                self.audio_bridge.last_failed_fingerprint = None;
                self.audio_bridge.last_error = None;
                self.complete_background_task(
                    "native-sync",
                    Some("Engine graph ready".to_string()),
                );
            }
            Err(error) => {
                self.audio_bridge.sync_failed_count =
                    self.audio_bridge.sync_failed_count.saturating_add(1);
                crate::perf::count("sync_failed_count", self.audio_bridge.sync_failed_count);
                // Remember the failing graph so the poll does not re-run it every
                // tick; a real change or an explicit forced retry resets this.
                self.audio_bridge.last_failed_fingerprint = finished_fingerprint;
                self.audio_bridge.last_error = Some(error.to_string());
                eprintln!("[audio] load_project failed: {error}");
                self.fail_background_task("native-sync", error.to_string());
            }
        }
        // Dirty was already cleared when the sync was committed; a real edit during
        // the sync re-dirtied and was coalesced into `pending_fingerprint` below.

        // Phase 2b: read back per-insert instantiation status now that the
        // runtime graph reflects this snapshot. A native-plugin insert that
        // the engine reports as not-ready failed to instantiate — flip its
        // UI slot to `Failed` (no panic; just surfaces the error).
        self.apply_engine_insert_statuses(cx);

        // Project load can finish plugin restore before the audio engine is
        // warm; re-bind bridge sinks whenever a graph swap completes.
        if self.sync_plugin_bridge_sinks_to_engine(cx, "audio_project_sync_complete") {
            eprintln!("[PluginRestore] graph snapshot swapped generation=post_sync");
        }

        // Reschedule a coalesced pending request — but only if it would actually do
        // something. The captured pending fingerprint is a cheap pre-gate; the real
        // dedup happens inside `schedule_audio_project_sync` against a freshly built
        // snapshot. Re-flag dirty so that call clears the cheap dirty gate.
        let pending = self.audio_bridge.pending_fingerprint.take();
        let pending_force = std::mem::take(&mut self.audio_bridge.pending_force);
        let pending_reason = self
            .audio_bridge
            .pending_reason
            .take()
            .unwrap_or("audio_sync_pending");
        if pending.is_some() {
            self.complete_background_task("native-sync-pending", None);
        }
        let needs_pending = match pending {
            Some(pf) => {
                pending_force
                    || (self.audio_bridge.graph_fingerprint != Some(pf)
                        && self.audio_bridge.last_failed_fingerprint != Some(pf))
            }
            None => false,
        };
        if needs_pending {
            self.audio_bridge.project_dirty = true;
            self.schedule_audio_project_sync(cx, pending_force, pending_reason);
            return;
        }

        if self.audio_bridge.play_after_sync {
            self.audio_bridge.play_after_sync = false;
            self.start_native_playback(cx);
        }
    }

    /// Watchdog: free the sync lifecycle when a `load_project` hangs (a true
    /// deadlock never returns, so the spawned join would otherwise pin
    /// `sync_in_flight` and the "Sync native engine" task forever). Bumping the
    /// generation makes the eventual (orphaned) completion a no-op. The engine
    /// thread stays alive; the user can retry via any forced sync / reopening
    /// audio settings. Driven from `poll_native_audio`.
    pub(super) fn timeout_audio_project_sync(&mut self, cx: &mut Context<Self>) {
        if !self.audio_bridge.sync_in_flight {
            return;
        }
        self.audio_bridge.sync_generation = self.audio_bridge.sync_generation.wrapping_add(1);
        self.audio_bridge.sync_in_flight = false;
        self.audio_bridge.sync_started_at = None;
        self.audio_bridge.last_failed_fingerprint = self.audio_bridge.running_fingerprint.take();
        self.audio_bridge.pending_fingerprint = None;
        self.audio_bridge.pending_reason = None;
        self.audio_bridge.pending_force = false;
        self.audio_bridge.project_dirty = false;
        self.audio_bridge.media_dirty = false;
        self.audio_bridge.sync_timeout_count =
            self.audio_bridge.sync_timeout_count.saturating_add(1);
        crate::perf::count("sync_timeout_count", self.audio_bridge.sync_timeout_count);
        self.audio_bridge.last_error = Some("native engine sync timed out".to_string());
        eprintln!(
            "[audio] native engine sync timed out after {}s (recoverable) reason={}",
            SYNC_WATCHDOG_TIMEOUT.as_secs(),
            self.audio_bridge.last_sync_reason
        );
        self.fail_background_task("native-sync", "Native engine sync timed out (recoverable)");
        self.complete_background_task("native-sync-pending", None);
        cx.notify();
    }

    /// Read structured per-insert status from the engine and reconcile each
    /// UI slot's `load_status` (Phase 2b). Only native-plugin inserts are
    /// reconciled — built-ins are always live, and the stub / unscanned slots
    /// aren't sent to the engine so they keep their optimistic UI status.
    /// Runs on the UI thread right after a sync completes — not in the poll
    /// loop — so the runtime mutex is locked at most once per project change.
    pub(super) fn apply_engine_insert_statuses(&mut self, cx: &mut Context<Self>) {
        use crate::components::timeline::timeline_state::InsertLoadStatus;
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return;
        };
        let statuses = engine.insert_statuses();
        if statuses.is_empty() {
            return;
        }
        let mut changed = false;
        self.timeline.update(cx, |timeline, _cx| {
            for st in &statuses {
                if !st.native {
                    continue;
                }
                let status = if st.ready {
                    InsertLoadStatus::Ready
                } else {
                    InsertLoadStatus::Failed("Plugin failed to instantiate".to_string())
                };
                if timeline
                    .state
                    .set_insert_load_status(&st.track_id, &st.insert_id, status)
                {
                    changed = true;
                }
            }
        });
        if changed {
            cx.notify();
        }
    }

    pub(super) fn mark_engine_project_dirty(&mut self) {
        self.audio_bridge.project_dirty = true;
    }

    /// Push an insert's effective bypass state (`enabled && !bypassed`) to the
    /// live engine as the runtime "enabled" insert param. The plugin instance
    /// stays loaded; the render pass skips (or resumes) it from the next block.
    /// This is the cheap live path for bypass/enable toggles — callers must NOT
    /// also set `audio_bridge.project_dirty`, which would trigger a full
    /// `load_project` graph rebuild for a non-structural change.
    pub(super) fn push_insert_enabled_to_engine(
        &mut self,
        track_id: &str,
        insert_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.audio_bridge.engine.clone() else {
            return;
        };
        let effective = self
            .timeline
            .read(cx)
            .state
            .find_insert_slot(track_id, insert_id)
            .map(|slot| slot.enabled && !slot.bypassed);
        let Some(effective) = effective else {
            return;
        };
        if let Err(error) = engine.set_insert_param(
            track_id.to_string(),
            insert_id.to_string(),
            "enabled".to_string(),
            if effective { 1.0 } else { 0.0 },
        ) {
            eprintln!(
                "[audio] insert enabled update failed: track={track_id} insert={insert_id} error={error}"
            );
        }
    }

    pub(crate) fn mark_engine_media_dirty(&mut self) {
        self.audio_bridge.project_dirty = true;
        self.audio_bridge.media_dirty = true;
    }

    pub(super) fn ensure_audio_stream_warm(&mut self) -> bool {
        transport_freeze_debug::log("ensure_audio_stream_warm enter");
        let stream_ready = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|stats| stats.stream_open && stats.running)
            .unwrap_or(false)
            || self.audio_bridge.running;
        if stream_ready {
            transport_freeze_debug::log("ensure_audio_stream_warm already warm");
            return true;
        }

        let Some(engine) = self.audio_bridge.engine.as_mut() else {
            self.audio_bridge.last_error = Some("audio engine unavailable".to_string());
            transport_freeze_debug::log("ensure_audio_stream_warm no engine");
            return false;
        };
        transport_freeze_debug::log("ensure_audio_stream_warm before engine.start");
        // `AudioEngine::start` resumes an open stream without reopening the
        // device or rebuilding/decoding the runtime graph on this thread.
        match engine.start() {
            Ok(()) => {
                transport_freeze_debug::log("ensure_audio_stream_warm after engine.start ok");
                self.audio_bridge.stats = Some(engine.stats());
                self.audio_bridge.running = true;
                self.audio_bridge.last_error = None;
                true
            }
            Err(error) => {
                self.audio_bridge.running = false;
                self.audio_bridge.last_error = Some(error.to_string());
                eprintln!("[audio] stream warm-up failed: {error}");
                transport_freeze_debug::log("ensure_audio_stream_warm engine.start failed");
                false
            }
        }
    }

    fn start_hardware_midi_playback(&mut self, playhead_beats: f32, cx: &mut Context<Self>) {
        let (start_sample, sample_rate) = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|stats| (stats.position_samples, stats.sample_rate.max(1)))
            .unwrap_or_else(|| {
                let sample_rate = self.current_audio_sample_rate().max(1);
                let start_sample = {
                    let timeline = self.timeline.read(cx);
                    timeline.state.tempo_map.samples_at_beat(
                        playhead_beats.max(0.0) as f64,
                        timeline.state.bpm as f64,
                        sample_rate as f64,
                    )
                };
                (start_sample, sample_rate)
            });
        let mut events = self.build_hardware_midi_events(playhead_beats, sample_rate, cx);
        for event in &mut events {
            if event.delay_seconds <= 0.0 && event.absolute_sample < start_sample {
                event.absolute_sample = start_sample;
            }
        }
        if std::env::var_os("FUTUREBOARD_MIDI_OUTPUT_DEBUG").is_some() {
            eprintln!(
                "[midi-output] start hardware playback events={} playhead_beats={:.3} start_sample={} sample_rate={}",
                events.len(),
                playhead_beats,
                start_sample,
                sample_rate,
            );
        }
        self.hardware_midi_playback
            .start_at_sample(events, start_sample, sample_rate);
    }

    fn stop_hardware_midi_playback(&mut self) {
        self.hardware_midi_playback.stop();
    }

    fn build_hardware_midi_events(
        &self,
        playhead_beats: f32,
        sample_rate: u32,
        cx: &mut Context<Self>,
    ) -> Vec<sphere_midi_service::HardwareMidiEvent> {
        let enabled_outputs = {
            let settings = self.settings.read(cx);
            let detected = crate::device_registry::cached_midi_devices();
            sphere_midi_service::resolve_midi_devices(
                &settings.current.hardware.midi.devices,
                &detected,
            )
            .into_iter()
            .filter(|d| {
                d.enabled
                    && d.connected
                    && matches!(
                        d.direction,
                        crate::settings::MidiDeviceDirection::Output
                            | crate::settings::MidiDeviceDirection::InputOutput
                    )
            })
            .flat_map(|d| [d.id, d.name])
            .collect::<HashSet<_>>()
        };
        if enabled_outputs.is_empty() {
            return Vec::new();
        }

        let timeline = self.timeline.read(cx);
        let state = &timeline.state;
        let base_bpm = state.bpm as f64;
        let playhead_seconds = state
            .tempo_map
            .seconds_at_beat(playhead_beats.max(0.0) as f64, base_bpm);
        let has_solo = state.tracks.iter().any(|track| track.solo);
        let mut events = Vec::new();

        for track in &state.tracks {
            if track.track_type != TrackType::Midi || track.muted || (has_solo && !track.solo) {
                continue;
            }
            let TrackOutputRouting::HardwareOutput { device_id, .. } = &track.routing.output else {
                continue;
            };
            if !enabled_outputs.contains(device_id) {
                continue;
            }
            let output_mode = track.routing.output_channel_mode();
            for clip in &track.clips {
                if clip.muted || clip.start_beat + clip.duration_beats <= playhead_beats {
                    continue;
                }
                let ClipType::Midi { notes, .. } = &clip.clip_type else {
                    continue;
                };
                for note in notes.iter().filter(|note| !note.muted) {
                    let note_start = clip.start_beat + note.start.max(0.0);
                    let note_end = (note_start + note.duration.max(0.0))
                        .min(clip.start_beat + clip.duration_beats);
                    if note_end <= playhead_beats || note_end <= note_start {
                        continue;
                    }
                    let channel = output_mode.resolve(note.channel).raw().min(15);
                    let pitch = note.pitch.min(127);
                    let velocity = note.velocity.clamp(1, 127);
                    if note_start <= playhead_beats {
                        events.push(sphere_midi_service::HardwareMidiEvent {
                            device_id: device_id.clone(),
                            delay_seconds: 0.0,
                            beat: playhead_beats.max(0.0) as f64,
                            absolute_sample: state.tempo_map.samples_at_beat(
                                playhead_beats.max(0.0) as f64,
                                base_bpm,
                                sample_rate as f64,
                            ),
                            message: vec![0x90 | channel, pitch, velocity],
                        });
                    } else {
                        let start_seconds = state
                            .tempo_map
                            .seconds_at_beat(note_start.max(0.0) as f64, base_bpm);
                        events.push(sphere_midi_service::HardwareMidiEvent {
                            device_id: device_id.clone(),
                            delay_seconds: (start_seconds - playhead_seconds).max(0.0),
                            beat: note_start as f64,
                            absolute_sample: state.tempo_map.samples_at_beat(
                                note_start.max(0.0) as f64,
                                base_bpm,
                                sample_rate as f64,
                            ),
                            message: vec![0x90 | channel, pitch, velocity],
                        });
                    }
                    let end_seconds = state
                        .tempo_map
                        .seconds_at_beat(note_end.max(0.0) as f64, base_bpm);
                    events.push(sphere_midi_service::HardwareMidiEvent {
                        device_id: device_id.clone(),
                        delay_seconds: (end_seconds - playhead_seconds).max(0.0),
                        beat: note_end as f64,
                        absolute_sample: state.tempo_map.samples_at_beat(
                            note_end.max(0.0) as f64,
                            base_bpm,
                            sample_rate as f64,
                        ),
                        message: vec![0x80 | channel, pitch, 0],
                    });
                }
            }
        }

        events
    }

    pub(super) fn current_audio_sample_rate(&self) -> u32 {
        self.audio_bridge
            .stats
            .as_ref()
            .map(|stats| stats.sample_rate)
            .filter(|sample_rate| *sample_rate > 0)
            .or_else(|| {
                self.audio_bridge
                    .engine
                    .as_ref()
                    .map(|engine| engine.config().sample_rate)
                    .filter(|sample_rate| *sample_rate > 0)
            })
            .unwrap_or(48_000)
    }

    pub(super) fn start_native_playback(&mut self, cx: &mut Context<Self>) {
        if !self.session_install_status.is_ready() {
            eprintln!("[SessionLoad] play blocked during session install");
            return;
        }
        transport_freeze_debug::reset_sequence();
        transport_freeze_debug::log("Play requested");
        eprintln!("[transport] Play requested");
        {
            let timeline = self.timeline.read(cx);
            crate::forensic_trace::dump_midi_model(&timeline.state);
        }

        if self.audio_bridge.engine.is_none() {
            self.audio_bridge.last_error = Some("audio engine unavailable".to_string());
            transport_freeze_debug::log("abort: no audio engine");
            return;
        }

        let already_playing = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|stats| stats.transport_playing)
            .unwrap_or(false);
        if already_playing {
            transport_freeze_debug::log("abort: already playing (idempotent)");
            return;
        }

        transport_freeze_debug::log("before sync/dirty gates");
        if self.audio_bridge.sync_in_flight {
            self.audio_bridge.play_after_sync = true;
            transport_freeze_debug::log("defer: audio_sync_in_flight");
            return;
        }
        if self.audio_bridge.project_dirty || self.audio_bridge.media_dirty {
            self.audio_bridge.play_after_sync = true;
            transport_freeze_debug::log("defer: schedule_audio_project_sync");
            self.schedule_audio_project_sync(cx, false, "transport_play_pending_sync");
            return;
        }

        transport_freeze_debug::log("before ensure_audio_stream_warm");
        if !self.ensure_audio_stream_warm() {
            transport_freeze_debug::log("abort: stream warm-up failed");
            return;
        }

        transport_freeze_debug::log("after ensure_audio_stream_warm");

        // Read transport state once; do not hold timeline lease across engine I/O.
        let (
            playhead_beats,
            bpm,
            tempo_points,
            metronome_enabled,
            loop_enabled,
            loop_start_beats,
            loop_end_beats,
            ts_points,
        ) = {
            transport_freeze_debug::log("before timeline.read");
            let timeline = self.timeline.read(cx);
            transport_freeze_debug::log("after timeline.read");
            let tempo_points = timeline
                .state
                .tempo_map
                .points
                .iter()
                .map(|p| DirectAudio::types::EngineTempoPointSnapshot {
                    beat: p.beat,
                    bpm: p.bpm,
                })
                .collect::<Vec<_>>();
            let ts_points = timeline
                .state
                .time_signature_map
                .points
                .iter()
                .map(
                    |p| DirectAudio::time_signature_map::RuntimeTimeSignaturePointSnapshot {
                        beat: p.beat,
                        numerator: p.numerator,
                        denominator: p.denominator,
                        grouping: p.effective_grouping(),
                    },
                )
                .collect::<Vec<_>>();
            (
                timeline.state.transport.playhead_beats,
                timeline.state.bpm,
                tempo_points,
                timeline.state.transport.metronome_enabled,
                timeline.state.transport.loop_enabled,
                timeline.state.transport.loop_start_beats,
                timeline.state.transport.loop_end_beats,
                ts_points,
            )
        };

        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            transport_freeze_debug::log("abort: engine handle missing");
            return;
        };

        transport_freeze_debug::log("before engine transport sync");
        if let Err(error) = engine.set_tempo_map(bpm as f64, tempo_points) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set tempo map failed: {error}");
            }
        }
        if let Err(error) = engine.set_time_signature_map(ts_points) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set time signature map failed: {error}");
            }
        }
        if let Err(error) = engine.set_metronome_enabled(metronome_enabled) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set metronome failed: {error}");
            }
        }
        let (loop_start_seconds, loop_end_seconds, playhead_seconds) = {
            let state = &self.timeline.read(cx).state;
            let base = state.bpm as f64;
            (
                state
                    .tempo_map
                    .seconds_at_beat(loop_start_beats as f64, base),
                state.tempo_map.seconds_at_beat(loop_end_beats as f64, base),
                state
                    .tempo_map
                    .seconds_at_beat(playhead_beats.max(0.0) as f64, base),
            )
        };
        if let Err(error) = engine.set_loop(loop_enabled, loop_start_seconds, loop_end_seconds) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set loop failed: {error}");
            }
        }
        transport_freeze_debug::log("after engine transport sync");

        transport_freeze_debug::log("before engine.seek");
        if let Err(error) = engine.seek(playhead_seconds) {
            self.audio_bridge.last_error = Some(error.to_string());
            eprintln!("[audio] seek before play failed: {error}");
            transport_freeze_debug::log("abort: seek failed");
            return;
        }
        transport_freeze_debug::log("after engine.seek");

        if std::env::var_os("FUTUREBOARD_PLAYBACK_DEBUG").is_some() {
            let debug = engine.debug_snapshot();
            eprintln!(
                "[playback] starting: sr={} loaded_clips={} ready_clips={} seek_seconds={:.3} bpm={:.1}",
                self.current_audio_sample_rate(),
                debug.loaded_clips,
                debug.ready_clips,
                playhead_seconds,
                bpm
            );
            if debug.loaded_clips == 0 {
                eprintln!(
                    "[playback] WARNING: no clips in realtime runtime — verify that imported clips \
                     have a non-empty media path"
                );
            } else if debug.ready_clips == 0 {
                eprintln!(
                    "[playback] WARNING: clips loaded but none decoded — check earlier \
                     '[SphereAudio] clip ... decode FAILED' lines"
                );
            }
        }

        transport_freeze_debug::log("before engine.play");
        if let Err(error) = engine.play() {
            self.audio_bridge.last_error = Some(error.to_string());
            eprintln!("[audio] play failed: {error}");
            transport_freeze_debug::log("abort: engine.play failed");
            return;
        }
        transport_freeze_debug::log("after engine.play");

        let stats = engine.stats();
        self.audio_bridge.stats = Some(stats);
        self.start_hardware_midi_playback(playhead_beats, cx);
        transport_freeze_debug::log("before timeline.update playing=true");
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.state.transport.playing = true;
            cx.notify();
        });
        transport_freeze_debug::log("after timeline.update");
        transport_freeze_debug::log("returning from start_native_playback");

        if let Some(watchdog) = PlayWatchdog::start() {
            let owner = cx.entity().clone();
            cx.spawn(async move |_this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let _ = owner.update(cx, |this, _cx| {
                    let playing = this
                        .audio_bridge
                        .stats
                        .as_ref()
                        .map(|stats| stats.transport_playing)
                        .unwrap_or(false);
                    watchdog.check(playing);
                });
            })
            .detach();
        }
    }

    pub(super) fn sync_transport_controls(&mut self, cx: &mut Context<Self>) {
        self.sync_metronome_controls(cx);
        self.sync_loop_controls(cx);
    }

    pub(super) fn sync_metronome_controls(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return;
        };
        let (enabled, bpm) = {
            let timeline = self.timeline.read(cx);
            (
                timeline.state.transport.metronome_enabled,
                timeline.state.bpm as f64,
            )
        };
        if let Err(error) = engine.set_bpm(bpm) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set BPM failed: {error}");
            }
        }
        if let Err(error) = engine.set_metronome_enabled(enabled) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set metronome failed: {error}");
            }
        }
        self.sync_time_signature_map_to_engine(cx);
    }

    pub(super) fn sync_loop_controls(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return;
        };
        let (enabled, start_beats, end_beats, bpm) = {
            let timeline = self.timeline.read(cx);
            let transport = &timeline.state.transport;
            (
                transport.loop_enabled,
                transport.loop_start_beats,
                transport.loop_end_beats,
                timeline.state.bpm,
            )
        };
        let bpm = bpm.max(1.0) as f64;
        let start_seconds = start_beats as f64 * 60.0 / bpm;
        let end_seconds = end_beats as f64 * 60.0 / bpm;
        if let Err(error) = engine.set_loop(enabled, start_seconds, end_seconds) {
            if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                eprintln!("[audio] set loop failed: {error}");
            }
        }
    }

    /// Apply a BPM drag sample. The drag is screen-edge independent: on Windows
    /// the OS cursor is warped back to a fixed anchor every move, so vertical
    /// dragging accumulates unbounded relative motion (true DAW-style infinite
    /// scrubbing). Tempo safety: when automation exists the drag edits the
    /// active tempo marker at the playhead instead of the fixed project BPM.
    pub(super) fn apply_bpm_drag_sample(
        &mut self,
        sample: components::BpmDragSample,
        cx: &mut Context<Self>,
    ) {
        let new_drag = self.bpm_drag.active_id != Some(sample.drag_id);
        if new_drag {
            self.bpm_drag.active_id = Some(sample.drag_id);
            self.bpm_drag.prev_y = sample.cur_y;
            self.bpm_drag.accum = 0.0;
            self.engine_sync.bpm_committed_at = None;
            // Capture the cursor anchor + window scale for warp-based dragging.
            self.bpm_drag.anchor = cursor_pos_phys();
            self.bpm_drag.scale = cx
                .windows()
                .first()
                .and_then(|w| w.update(cx, |_, window, _| window.scale_factor()).ok())
                .unwrap_or(1.0)
                .max(0.1);
            // Decide what this drag edits, and capture the start value there.
            let (target_point_id, start_value) = {
                let state = &self.timeline.read(cx).state;
                if state.tempo_has_automation() {
                    let beat = state.transport.playhead_beats as f64;
                    match state.tempo_map.point_id_at_or_before_beat(beat) {
                        Some(id) => {
                            let bpm = state
                                .tempo_map
                                .points
                                .iter()
                                .find(|p| p.id == id)
                                .map(|p| p.bpm as f32)
                                .unwrap_or(state.bpm);
                            (Some(id.to_string()), bpm)
                        }
                        // Before the first marker → edit the fixed base tempo.
                        None => (None, state.bpm),
                    }
                } else {
                    (None, state.bpm)
                }
            };
            self.bpm_drag.target_point_id = target_point_id;
            self.bpm_drag.start_value = start_value;
            if components::bpm_debug_enabled() {
                eprintln!(
                    "[transport-bpm] drag_start id={} start_value={:.2} target_point_id={:?} scale={:.2} anchor={:?}",
                    sample.drag_id,
                    start_value,
                    self.bpm_drag.target_point_id,
                    self.bpm_drag.scale,
                    self.bpm_drag.anchor
                );
            }
            return;
        }

        // Relative vertical movement in logical px. Prefer the warped OS cursor
        // delta (edge-independent); fall back to the window-relative position.
        let dy_logical = match self.warp_drag_delta_y() {
            Some(dy) => dy,
            None => {
                let raw_delta = sample.cur_y - self.bpm_drag.prev_y;
                if raw_delta.abs() < components::BPM_DRAG_DEADZONE_PX {
                    return;
                }
                self.bpm_drag.prev_y = sample.cur_y;
                raw_delta
            }
        };
        if dy_logical == 0.0 {
            return;
        }

        let coarse = sample.control || sample.platform || sample.alt;
        let sensitivity = components::bpm_drag_sensitivity(sample.shift, coarse);
        // Up = positive BPM change. Screen Y grows downward, so negate.
        self.bpm_drag.accum -= dy_logical * sensitivity;

        let raw = self.bpm_drag.start_value + self.bpm_drag.accum;
        let clamped = raw.clamp(components::BPM_MIN, components::BPM_MAX);
        let snapped = if sample.shift {
            (clamped * 10.0).round() / 10.0
        } else if coarse {
            (clamped / 5.0).round() * 5.0
        } else {
            clamped.round()
        };

        let now = Instant::now();
        let engine_due = match self.engine_sync.bpm_committed_at {
            Some(t) => now.duration_since(t) >= Duration::from_millis(33),
            None => true,
        };
        let target_point_id = self.bpm_drag.target_point_id.clone();
        self.apply_bpm_value(snapped, target_point_id.as_deref(), engine_due, cx);
        if engine_due {
            self.engine_sync.bpm_committed_at = Some(now);
        }
    }

    /// Edge-independent vertical drag delta (logical px) using OS cursor warp.
    /// Reads the absolute cursor, computes the delta from the anchor, then
    /// re-pins the cursor to the anchor so it can never reach the screen edge.
    /// Returns `None` on platforms without cursor warp so callers fall back to
    /// window-relative deltas.
    fn warp_drag_delta_y(&mut self) -> Option<f32> {
        let anchor = self.bpm_drag.anchor?;
        let cur = cursor_pos_phys()?;
        let dy_phys = cur.1 - anchor.1;
        if dy_phys == 0 {
            // Echo from our own warp, or no motion.
            return Some(0.0);
        }
        set_cursor_pos_phys(anchor.0, anchor.1);
        Some(dy_phys as f32 / self.bpm_drag.scale)
    }

    /// Cancel the active BPM drag, restoring the value captured at drag start.
    /// Wired to Escape.
    pub(super) fn cancel_bpm_drag(&mut self, cx: &mut Context<Self>) -> bool {
        if self.bpm_drag.active_id.is_none() {
            return false;
        }
        let start = self.bpm_drag.start_value;
        let target_point_id = self.bpm_drag.target_point_id.clone();
        self.apply_bpm_value(start, target_point_id.as_deref(), true, cx);
        self.end_bpm_drag();
        true
    }

    /// Clears transient BPM-drag bookkeeping. The next `on_drag` gets a fresh
    /// `drag_id`, so this is only needed for explicit cancel.
    pub(super) fn end_bpm_drag(&mut self) {
        self.bpm_drag.active_id = None;
        self.bpm_drag.anchor = None;
        self.bpm_drag.accum = 0.0;
    }

    /// Apply a BPM value to the correct target: a tempo marker (mapped mode) or
    /// the fixed project BPM. Centralizes the tempo-safety rule so dragging,
    /// inline edit, and menu commands all behave consistently.
    pub(super) fn apply_bpm_value(
        &mut self,
        bpm: f32,
        target_point_id: Option<&str>,
        commit_to_engine: bool,
        cx: &mut Context<Self>,
    ) {
        let bpm = bpm.clamp(components::BPM_MIN, components::BPM_MAX);
        match target_point_id {
            None => self.set_native_bpm_inner(bpm, commit_to_engine, cx),
            Some(point_id) => {
                let changed = self.timeline.update(cx, |timeline, cx| {
                    let updated = timeline
                        .state
                        .tempo_map
                        .update_point_bpm_by_id(point_id, bpm as f64);
                    if updated {
                        cx.notify();
                    }
                    updated
                });
                if changed {
                    self.mark_dirty();
                    if commit_to_engine {
                        self.sync_tempo_map_to_engine(cx);
                    }
                    cx.notify();
                }
            }
        }
    }

    /// Apply a new BPM from the transport BPM drag. Updates the timeline
    /// state and sends a lightweight `set_bpm` to the engine — never reloads
    /// the project. Skips notify when the value would round to the same
    /// stored value to avoid notify-spam during mouse motion.
    pub(super) fn set_native_bpm(&mut self, bpm: f32, cx: &mut Context<Self>) {
        self.set_native_bpm_inner(bpm, true, cx);
    }

    pub(super) fn set_native_bpm_inner(
        &mut self,
        bpm: f32,
        commit_to_engine: bool,
        cx: &mut Context<Self>,
    ) {
        let bpm = bpm.clamp(components::BPM_MIN, components::BPM_MAX);
        let changed = self.timeline.update(cx, |timeline, cx| {
            if (timeline.state.bpm - bpm).abs() > 0.005 {
                timeline.state.bpm = bpm;
                cx.notify();
                true
            } else {
                false
            }
        });
        if !changed {
            return;
        }
        if commit_to_engine {
            if let Some(engine) = self.audio_bridge.engine.as_ref() {
                if let Err(error) = engine.set_bpm(bpm as f64) {
                    if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                        eprintln!("[audio] set BPM failed: {error}");
                    }
                }
            }
            if std::env::var_os("FUTUREBOARD_TRANSPORT_DEBUG").is_some() {
                eprintln!("[transport-bpm] commit bpm={:.2}", bpm);
            }
        }
        cx.notify();
    }

    /// Open the compact tempo menu (anchored at screen `x`,`y`) from the
    /// transport BPM display. Reuses the shared context-menu overlay.
    pub(super) fn open_tempo_menu(
        &mut self,
        window: &Window,
        x: f32,
        y: f32,
        cx: &mut Context<Self>,
    ) {
        self.try_open_context_menu(
            ContextMenuRequest::from_window(
                window,
                x,
                y,
                ContextMenuTarget::Extended(ContextTarget::Tempo),
            ),
            cx,
        );
    }

    /// Add a tempo marker at the current playhead using the effective BPM there.
    /// This is the primary way to introduce tempo automation from the transport.
    pub(super) fn add_tempo_marker_at_playhead(&mut self, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            let beat = timeline.state.transport.playhead_beats as f64;
            let bpm = timeline.state.effective_bpm_at_beat(beat);
            timeline.state.tempo_map.add_or_update_point(
                beat,
                bpm,
                crate::components::timeline::timeline_state::TempoCurve::Hold,
            );
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    /// Remove all tempo automation, keeping the playhead BPM as a single fixed
    /// marker at beat 0.
    pub(super) fn clear_tempo_automation(&mut self, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            let bpm = timeline.state.effective_bpm_at_playhead();
            if timeline.state.tempo_map.points.len() == 1
                && timeline
                    .state
                    .tempo_map
                    .points
                    .first()
                    .is_some_and(|p| p.beat <= 1e-6 && (p.bpm - bpm).abs() < 1e-6)
            {
                return false;
            }
            timeline.state.bpm = bpm as f32;
            timeline.state.tempo_map.reset_to_single_point(
                0.0,
                bpm,
                crate::components::timeline::timeline_state::TempoCurve::Hold,
            );
            timeline.state.selected_tempo_point_id = None;
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn show_tempo_track(&mut self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, cx| {
            timeline.state.show_tempo_track_lane();
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn hide_tempo_track(&mut self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, cx| {
            timeline.state.hide_tempo_track_lane();
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn tempo_track_context_position(&self) -> Option<(f64, f64)> {
        match &self.overlay.open_popover {
            Some(OpenPopover::Context { request }) => match &request.target {
                ContextMenuTarget::Extended(ContextTarget::TempoTrack { beat, bpm, .. }) => {
                    Some((*beat, *bpm))
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn tempo_track_context_point_id(&self) -> Option<String> {
        match &self.overlay.open_popover {
            Some(OpenPopover::Context { request }) => match &request.target {
                ContextMenuTarget::Extended(ContextTarget::TempoTrack { point_id, .. }) => {
                    point_id.clone()
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn add_tempo_point_at_lane(&mut self, beat: f64, bpm: f64, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            if let Some(id) = timeline.state.add_tempo_point(beat, bpm) {
                timeline.state.select_tempo_point(&id);
                cx.notify();
                true
            } else {
                false
            }
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn set_fixed_tempo_from_lane(
        &mut self,
        beat: f64,
        bpm: f64,
        cx: &mut Context<Self>,
    ) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            timeline.state.set_fixed_tempo_from_beat(beat, bpm);
            timeline.state.bpm = bpm as f32;
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn delete_tempo_point(&mut self, id: &str, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            if timeline.state.delete_tempo_point(id) {
                cx.notify();
                true
            } else {
                false
            }
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn set_tempo_point_curve(
        &mut self,
        id: &str,
        curve: crate::components::timeline::timeline_state::TempoCurve,
        cx: &mut Context<Self>,
    ) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            if timeline.state.set_tempo_point_curve(id, curve) {
                cx.notify();
                true
            } else {
                false
            }
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    /// Convert fixed-tempo mode into a tempo map by seeding an initial marker at
    /// beat 0 using the current project BPM. No-op if automation already exists.
    pub(super) fn create_tempo_automation(&mut self, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            if timeline.state.tempo_map.has_automation() {
                return false;
            }
            let bpm = timeline.state.bpm as f64;
            timeline.state.tempo_map.add_or_update_point(
                0.0,
                bpm,
                crate::components::timeline::timeline_state::TempoCurve::Hold,
            );
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    /// Insert a tempo marker at an explicit beat using the effective BPM there
    /// (used by the timeline ruler context menu). `create_first` seeds tempo
    /// automation if none exists yet.
    pub(super) fn add_tempo_point_at_beat(
        &mut self,
        beat: f64,
        create_first: bool,
        cx: &mut Context<Self>,
    ) {
        let beat = beat.max(0.0);
        let changed = self.timeline.update(cx, |timeline, cx| {
            use crate::components::timeline::timeline_state::TempoCurve;
            if !timeline.state.tempo_map.has_automation() && create_first {
                // Seed an anchor at beat 0 so the new marker reads as a change
                // from the project's fixed tempo.
                let base = timeline.state.bpm as f64;
                if beat > 1e-6 {
                    timeline
                        .state
                        .tempo_map
                        .add_or_update_point(0.0, base, TempoCurve::Hold);
                }
            }
            let bpm = timeline.state.effective_bpm_at_beat(beat);
            timeline
                .state
                .tempo_map
                .add_or_update_point(beat, bpm, TempoCurve::Hold);
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_tempo_map_to_engine(cx);
            cx.notify();
        }
    }

    /// Open the inline numeric BPM editor, seeded with the current effective
    /// BPM at the playhead. Keys are routed by the layout's key handler while
    /// `bpm_editing` is set, so no separate focus grab is needed.
    pub(super) fn begin_bpm_edit(&mut self, cx: &mut Context<Self>) {
        // A drag and an edit are mutually exclusive.
        self.end_bpm_drag();
        let bpm = self.timeline.read(cx).state.effective_bpm_at_playhead();
        // The typed editor and the scrub drag both resolve through the effective
        // BPM at the playhead, so they share one value source. The session owns
        // range/format/commit policy; the draft text stays in `bpm_input.value`.
        let format = crate::components::numeric_edit::NumberFormat::decimal(
            components::BPM_MIN as f64,
            components::BPM_MAX as f64,
            2,
        );
        let session = crate::components::numeric_edit::NumericEditSession::begin(
            bpm,
            format,
            crate::components::numeric_edit::CommitPolicy::OnEnter,
        );
        self.tempo_edit.bpm_input.set_value(session.original_text());
        self.tempo_edit.bpm_input.select_all();
        self.tempo_edit.bpm_session = Some(session);
        self.tempo_edit.bpm_editing = true;
        cx.notify();
    }

    /// Nudge the inline BPM draft by `steps` arrow-key increments (fractional
    /// multipliers allowed for coarse/fine). No-op unless the editor is open.
    /// Returns `true` when the draft changed so the caller can consume the key.
    pub(super) fn nudge_bpm_edit(&mut self, steps: f64, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.tempo_edit.bpm_session else {
            return false;
        };
        let next = session.nudge_draft(&self.tempo_edit.bpm_input.value, steps);
        self.tempo_edit.bpm_input.set_value(next);
        self.tempo_edit.bpm_input.select_all();
        cx.notify();
        true
    }

    /// Commit the inline BPM editor: parse the field and apply to the active
    /// tempo target (marker at playhead when automation exists, else fixed BPM).
    pub(super) fn commit_bpm_edit(&mut self, cx: &mut Context<Self>) {
        if !self.tempo_edit.bpm_editing {
            return;
        }
        // Parse + clamp through the shared numeric session so an invalid or
        // intermediate draft ("", ".", "abc") commits nothing (no model
        // mutation), and a valid draft yields exactly one clamped value.
        let parsed = self
            .tempo_edit
            .bpm_session
            .and_then(|session| session.commit(&self.tempo_edit.bpm_input.value))
            .map(|bpm| bpm as f32);
        self.tempo_edit.bpm_editing = false;
        self.tempo_edit.bpm_session = None;
        if let Some(bpm) = parsed {
            let target_point_id = {
                let state = &self.timeline.read(cx).state;
                if state.tempo_has_automation() {
                    let beat = state.transport.playhead_beats as f64;
                    state
                        .tempo_map
                        .point_id_at_or_before_beat(beat)
                        .map(|id| id.to_string())
                } else {
                    None
                }
            };
            self.apply_bpm_value(bpm, target_point_id.as_deref(), true, cx);
        }
        cx.notify();
    }

    /// Cancel the inline BPM editor without applying.
    pub(super) fn cancel_bpm_edit(&mut self, cx: &mut Context<Self>) {
        if !self.tempo_edit.bpm_editing {
            return;
        }
        self.tempo_edit.bpm_editing = false;
        self.tempo_edit.bpm_session = None;
        cx.notify();
    }

    /// Push the project TempoMap to the audio engine so playback, metronome,
    /// and MIDI scheduling use the authoritative hold-mode segments.
    pub(super) fn sync_tempo_map_to_engine(&mut self, cx: &mut Context<Self>) {
        let (default_bpm, points) = {
            let state = &self.timeline.read(cx).state;
            (
                state.bpm as f64,
                state
                    .tempo_map
                    .points
                    .iter()
                    .map(|p| DirectAudio::types::EngineTempoPointSnapshot {
                        beat: p.beat,
                        bpm: p.bpm,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        if let Some(engine) = self.audio_bridge.engine.as_ref() {
            if let Err(error) = engine.set_tempo_map(default_bpm, points) {
                if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                    eprintln!("[audio] sync tempo map failed: {error}");
                }
            }
        }
    }

    pub(super) fn sync_time_signature_map_to_engine(&mut self, cx: &mut Context<Self>) {
        let points = {
            let state = &self.timeline.read(cx).state;
            state
                .time_signature_map
                .points
                .iter()
                .map(
                    |p| DirectAudio::time_signature_map::RuntimeTimeSignaturePointSnapshot {
                        beat: p.beat,
                        numerator: p.numerator,
                        denominator: p.denominator,
                        grouping: p.effective_grouping(),
                    },
                )
                .collect::<Vec<_>>()
        };
        if let Some(engine) = self.audio_bridge.engine.as_ref() {
            if let Err(error) = engine.set_time_signature_map(points) {
                if !matches!(error, DirectAudio::SphereAudioError::EngineNotOpen) {
                    eprintln!("[audio] sync time signature map failed: {error}");
                }
            }
        }
    }

    pub(super) fn open_time_signature_menu(
        &mut self,
        window: &Window,
        x: f32,
        y: f32,
        cx: &mut Context<Self>,
    ) {
        self.try_open_context_menu(
            ContextMenuRequest::from_window(
                window,
                x,
                y,
                ContextMenuTarget::Extended(ContextTarget::TimeSignature),
            ),
            cx,
        );
    }

    pub(super) fn show_time_signature_track(&mut self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, cx| {
            timeline.state.show_time_signature_track_lane();
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn hide_time_signature_track(&mut self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, cx| {
            timeline.state.hide_time_signature_track_lane();
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn add_time_signature_marker_at_playhead(&mut self, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            let beat = timeline.state.transport.playhead_beats as f64;
            let pt = timeline
                .state
                .time_signature_map
                .time_signature_at_beat(beat);
            timeline
                .state
                .add_time_signature_point(beat, pt.numerator, pt.denominator);
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_time_signature_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn add_time_signature_point_at_beat(&mut self, beat: f64, cx: &mut Context<Self>) {
        let beat = beat.max(0.0);
        let changed = self.timeline.update(cx, |timeline, cx| {
            let pt = timeline
                .state
                .time_signature_map
                .time_signature_at_beat(beat);
            timeline
                .state
                .add_time_signature_point(beat, pt.numerator, pt.denominator);
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_time_signature_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn clear_time_signature_markers(&mut self, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            let beat = timeline.state.transport.playhead_beats as f64;
            timeline.state.clear_time_signature_markers(beat);
            cx.notify();
            true
        });
        if changed {
            self.mark_dirty();
            self.sync_time_signature_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn delete_time_signature_point(&mut self, id: &str, cx: &mut Context<Self>) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            if timeline.state.delete_time_signature_point(id) {
                cx.notify();
                true
            } else {
                false
            }
        });
        if changed {
            self.mark_dirty();
            self.sync_time_signature_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn move_time_signature_point_to_playhead(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        let changed = self.timeline.update(cx, |timeline, cx| {
            let beat = timeline.state.transport.playhead_beats as f64;
            if timeline.state.move_time_signature_point(id, beat) {
                cx.notify();
                true
            } else {
                false
            }
        });
        if changed {
            self.mark_dirty();
            self.sync_time_signature_map_to_engine(cx);
            cx.notify();
        }
    }

    pub(super) fn ts_track_context_position(&self) -> Option<f64> {
        match &self.overlay.open_popover {
            Some(OpenPopover::Context { request }) => match &request.target {
                ContextMenuTarget::Extended(
                    ContextTarget::TimeSignatureTrack { beat, .. }
                    | ContextTarget::TimeSignaturePoint { beat, .. },
                ) => Some(*beat),
                ContextMenuTarget::Extended(ContextTarget::TimelineRuler { beat }) => Some(*beat),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn ts_track_context_point_id(&self) -> Option<String> {
        match &self.overlay.open_popover {
            Some(OpenPopover::Context { request }) => match &request.target {
                ContextMenuTarget::Extended(ContextTarget::TimeSignatureTrack {
                    point_id, ..
                }) => point_id.clone(),
                ContextMenuTarget::Extended(ContextTarget::TimeSignaturePoint {
                    point_id, ..
                }) => Some(point_id.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub(super) fn begin_ts_edit(&mut self, point_id: Option<String>, cx: &mut Context<Self>) {
        let (num, den) = {
            let state = &self.timeline.read(cx).state;
            if let Some(id) = point_id
                .as_deref()
                .or(state.selected_time_signature_point_id.as_deref())
            {
                if let Some(pt) = state.time_signature_map.points.iter().find(|p| p.id == id) {
                    (pt.numerator, pt.denominator)
                } else {
                    let pt = state.time_signature_at_playhead();
                    (pt.numerator, pt.denominator)
                }
            } else {
                let pt = state.time_signature_at_playhead();
                (pt.numerator, pt.denominator)
            }
        };
        self.tempo_edit.ts_edit_point_id = point_id.or_else(|| {
            self.timeline
                .read(cx)
                .state
                .selected_time_signature_point_id
                .clone()
        });
        self.tempo_edit.ts_num_input.set_value(num.to_string());
        self.tempo_edit.ts_den_input.set_value(den.to_string());
        self.tempo_edit.ts_num_input.select_all();
        self.tempo_edit.ts_editing = true;
        self.tempo_edit.ts_edit_focus_num = true;
        cx.notify();
    }

    pub(super) fn commit_ts_edit(&mut self, cx: &mut Context<Self>) {
        if !self.tempo_edit.ts_editing {
            return;
        }
        let num = self
            .tempo_edit
            .ts_num_input
            .value
            .trim()
            .parse::<u16>()
            .ok();
        let den = self
            .tempo_edit
            .ts_den_input
            .value
            .trim()
            .parse::<u16>()
            .ok();
        self.tempo_edit.ts_editing = false;
        self.tempo_edit.ts_edit_focus_num = true;
        let Some(num) = num.filter(|n| (1..=64).contains(n)) else {
            cx.notify();
            return;
        };
        let den = den
            .map(crate::components::timeline::timeline_state::normalize_time_signature_denominator)
            .filter(|d| {
                crate::components::timeline::timeline_state::TS_ALLOWED_DENOMINATORS.contains(d)
            });
        let Some(den) = den else {
            cx.notify();
            return;
        };

        let point_id = self.tempo_edit.ts_edit_point_id.clone();
        let changed = self.timeline.update(cx, |timeline, cx| {
            let changed = if let Some(id) = point_id {
                timeline.state.update_time_signature_point(&id, num, den)
            } else {
                let beat = timeline.state.transport.playhead_beats as f64;
                timeline.state.add_time_signature_point(beat, num, den);
                true
            };
            if changed {
                cx.notify();
            }
            changed
        });
        self.tempo_edit.ts_edit_point_id = None;
        if changed {
            self.mark_dirty();
            self.sync_time_signature_map_to_engine(cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_ts_edit(&mut self, cx: &mut Context<Self>) {
        if !self.tempo_edit.ts_editing {
            return;
        }
        self.tempo_edit.ts_editing = false;
        self.tempo_edit.ts_edit_point_id = None;
        self.tempo_edit.ts_edit_focus_num = true;
        cx.notify();
    }

    pub(super) fn stop_native_playback(&mut self, cx: &mut Context<Self>) {
        self.stop_hardware_midi_playback();
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return;
        };
        if let Err(error) = engine.pause() {
            self.audio_bridge.last_error = Some(error.to_string());
            eprintln!("[audio] stop transport failed: {error}");
            return;
        }
        self.audio_bridge.stats = Some(engine.stats());
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.state.transport.playing = false;
            cx.notify();
        });
    }

    pub(super) fn set_playhead_scrub_active(&mut self, active: bool, _cx: &mut Context<Self>) {
        if let Some(engine) = self.audio_bridge.engine.as_ref() {
            let _ = engine.set_metronome_suspended(active);
        }
    }

    pub(super) fn seek_native_playhead(&mut self, cx: &mut Context<Self>, beat: f32) {
        self.seek_native_playhead_with_reason(cx, beat, SeekReason::Programmatic);
    }

    pub(super) fn seek_native_playhead_with_reason(
        &mut self,
        cx: &mut Context<Self>,
        beat: f32,
        reason: SeekReason,
    ) {
        let beat = beat.max(0.0);
        let was_playing = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|stats| stats.transport_playing)
            .unwrap_or(false);
        self.stop_hardware_midi_playback();
        let bpm = {
            let timeline = self.timeline.read(cx);
            timeline.state.bpm
        };
        if let Some(engine) = self.audio_bridge.engine.as_ref() {
            match reason {
                SeekReason::UserDragStart | SeekReason::UserDragging => {
                    let _ = engine.set_metronome_suspended(true);
                }
                SeekReason::UserDragEnd
                | SeekReason::TimelineClick
                | SeekReason::RewindForward
                | SeekReason::Programmatic => {
                    let _ = engine.set_metronome_suspended(false);
                }
            }
            let seconds = beat as f64 * 60.0 / bpm.max(1.0) as f64;
            if let Err(error) = engine.seek(seconds) {
                self.audio_bridge.last_error = Some(error.to_string());
                eprintln!("[audio] seek failed: {error}");
            }
        }
        self.engine_sync.playhead_beat = beat;
        self.engine_sync.synced_at = Instant::now();
        let _ = self.timeline.update(cx, move |timeline, cx| {
            timeline.state.transport.playhead_beats = beat;
            // Preview Track Volume automation at the new playhead so a stopped
            // seek updates the fader/inspector to the value under the cursor.
            timeline.state.recompute_effective_volumes(beat, "seek");
            cx.notify();
        });
        if was_playing {
            self.start_hardware_midi_playback(beat, cx);
        }
    }
}

/// Current OS cursor position in physical screen pixels, if the platform
/// supports querying it. Used to drive screen-edge-independent BPM scrubbing.
#[cfg(target_os = "windows")]
fn cursor_pos_phys() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}

/// Re-pin the OS cursor to `(x, y)` physical pixels so an active scrub never
/// reaches the screen edge.
#[cfg(target_os = "windows")]
fn set_cursor_pos_phys(x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

#[cfg(not(target_os = "windows"))]
fn cursor_pos_phys() -> Option<(i32, i32)> {
    None
}

#[cfg(not(target_os = "windows"))]
fn set_cursor_pos_phys(_x: i32, _y: i32) {}
