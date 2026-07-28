//! DAUx shared audio render kernel.
//!
//! `fill_output_f32` is the realtime hot path shared by all backends.
//! It is realtime-safe: no allocation, no locks, no I/O.
//!
//! Each backend creates a `LocalAudioState` per-stream and passes it along
//! with the shared `SharedState` and the mutable `RuntimeProject`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use crate::audio_file::AudioFileAudition;
use crate::command::EngineCommand;
use crate::dsp::{meter::smooth_peak, oscillator::SineOscillator};
use crate::engine::{SharedState, PEAK_DECAY, TEST_TONE_AMPLITUDE};
use crate::runtime::{RuntimePreviewMode, RuntimeProject};
use crate::transport;

// Re-export helpers so wasapi_exclusive.rs can use them through render.
pub use crate::engine::{
    render_project_block_interleaved, render_project_block_interleaved_with_live_input,
    render_project_sample,
};

fn command_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_AUDIO_COMMAND_DEBUG").is_some())
}

/// `FUTUREBOARD_AUDIO_CALLBACK_DEBUG=1` enables the realtime callback's
/// occasional eprintln traces (graph swap, mute, render-path). Off by default
/// so the audio thread never formats strings or writes to stdio — see
/// `tasks/native/audio-system-spec.md` §1 and Phase A finding A.2.2. Cached on
/// first read so the callback never touches the environment.
fn callback_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_AUDIO_CALLBACK_DEBUG").is_some())
}

fn transport_freeze_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_TRANSPORT_FREEZE_DEBUG").is_some())
}

/// Cached `FUTUREBOARD_PDC_DEBUG` check. Used to gate the one-shot realtime
/// latency-compensation dump on transport start/seek so the audio thread never
/// touches the environment in steady state.
fn pdc_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_PDC_DEBUG").is_some())
}

/// `FUTUREBOARD_METRONOME_DEBUG=1` prints click scheduling decisions. Cached so
/// the audio callback never reads the environment in steady state.
fn metronome_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_METRONOME_DEBUG").is_some())
}

/// Logs the first N audio blocks after `StartTransport` when freeze debug is on.
static POST_PLAY_CALLBACK_LOGS: AtomicU32 = AtomicU32::new(0);

#[inline]
pub(crate) fn post_stop_tail_samples(sample_rate: u32) -> u64 {
    (sample_rate as u64).saturating_mul(4)
}

#[inline]
fn log_post_play_callback(step: &str) {
    let remaining = POST_PLAY_CALLBACK_LOGS.load(Ordering::Relaxed);
    if remaining == 0 || !transport_freeze_debug_enabled() {
        return;
    }
    let left = POST_PLAY_CALLBACK_LOGS
        .fetch_sub(1, Ordering::Relaxed)
        .saturating_sub(1);
    eprintln!("[play-debug callback] {step} (remaining={left})");
}

// ── Per-stream oscillator + local playback state ──────────────────────────────

/// Local (non-shared) state for one audio stream.
/// Lives on the audio thread — no locks needed.
pub struct LocalAudioState {
    pub osc_l: SineOscillator,
    pub osc_r: SineOscillator,
    pub osc_freq: f32,
    pub osc_on: bool,
    pub playing_local: bool,
    pub prev_peak_l: f32,
    pub prev_peak_r: f32,
    /// Read cursor into the shared input ring (Layer 4 consumer state).
    pub input_read_frames: u64,
    /// Smoothed input-bus peaks for diagnostics (Layer 4 verification).
    pub prev_input_bus_l: f32,
    pub prev_input_bus_r: f32,
    /// Preallocated live-input block injected into monitored track buffers before
    /// the normal graph pass. Never resized from the callback.
    pub monitor_input_l: Vec<f32>,
    pub monitor_input_r: Vec<f32>,
    pub render_path_logged: bool,
    /// Samples of instrument processing still owed after the last preview note
    /// went off, so the plugin's release tail renders out instead of being cut
    /// dead when transport is stopped. Counts down per block; refreshed while a
    /// preview note is held.
    pub preview_tail_samples: u64,
    /// Samples of graph processing still owed after transport stop/pause so
    /// instruments, delays, reverbs, and bridged plugin tails decay naturally
    /// instead of being hard-cut as soon as the playhead stops.
    pub stop_tail_samples: u64,
    /// Last logged preview-note count (gates PreviewRenderWake spam).
    pub prev_logged_preview_notes: u32,
    /// Blocks until next PreviewRenderWake log while preview is active.
    pub preview_wake_log_cooldown: u32,
    pub metronome_enabled: bool,
    pub metronome_ts_num: u32,
    pub metronome_ts_den: u32,
    pub time_signature_map: crate::time_signature_map::RuntimeTimeSignatureMapSnapshot,
    /// Next click position in quarter-note beats.
    pub metronome_next_beat: f64,
    pub tempo_map: crate::tempo_map::RuntimeTempoMapSnapshot,
    pub metronome_click_remaining: u32,
    pub metronome_click_len: u32,
    pub metronome_click_phase: f64,
    pub metronome_click_phase_inc: f64,
    pub metronome_click_gain: f32,
    /// When true, metronome scheduling and output are suppressed (playhead scrub).
    pub metronome_suspended: bool,
    /// Standalone File Browser audition, owned by this stream/callback.
    pub audition: Option<AudioFileAudition>,
}

impl LocalAudioState {
    pub fn new(sample_rate: f64) -> Self {
        Self::with_monitor_capacity(sample_rate, 0)
    }

    pub fn with_monitor_capacity(sample_rate: f64, monitor_capacity: usize) -> Self {
        Self {
            osc_l: SineOscillator::new(440.0, sample_rate),
            osc_r: SineOscillator::new(440.0, sample_rate),
            osc_freq: 440.0,
            osc_on: false,
            playing_local: false,
            prev_peak_l: 0.0,
            prev_peak_r: 0.0,
            input_read_frames: 0,
            prev_input_bus_l: 0.0,
            prev_input_bus_r: 0.0,
            monitor_input_l: vec![0.0; monitor_capacity],
            monitor_input_r: vec![0.0; monitor_capacity],
            render_path_logged: false,
            preview_tail_samples: 0,
            stop_tail_samples: 0,
            prev_logged_preview_notes: u32::MAX,
            preview_wake_log_cooldown: 0,
            metronome_enabled: false,
            metronome_ts_num: 4,
            metronome_ts_den: 4,
            time_signature_map:
                crate::time_signature_map::RuntimeTimeSignatureMapSnapshot::static_sig(4, 4),
            metronome_next_beat: 0.0,
            tempo_map: crate::tempo_map::RuntimeTempoMapSnapshot::static_tempo(120.0),
            metronome_click_remaining: 0,
            metronome_click_len: (sample_rate * 0.024).round().max(1.0) as u32,
            metronome_click_phase: 0.0,
            metronome_click_phase_inc: 0.0,
            metronome_click_gain: 0.0,
            metronome_suspended: false,
            audition: None,
        }
    }

    pub fn set_metronome_enabled(&mut self, enabled: bool, position_sample: u64, sample_rate: u32) {
        self.metronome_enabled = enabled;
        self.metronome_click_remaining = 0;
        self.reset_metronome_schedule(position_sample, sample_rate);
    }

    pub fn set_bpm(&mut self, bpm: f64, position_sample: u64, sample_rate: u32) {
        self.tempo_map = crate::tempo_map::RuntimeTempoMapSnapshot::static_tempo(bpm);
        self.reset_metronome_schedule(position_sample, sample_rate);
    }

    pub fn set_tempo_map(
        &mut self,
        tempo_map: crate::tempo_map::RuntimeTempoMapSnapshot,
        position_sample: u64,
        sample_rate: u32,
    ) {
        self.tempo_map = tempo_map;
        self.reset_metronome_schedule(position_sample, sample_rate);
    }

    pub fn set_time_signature(
        &mut self,
        numerator: u32,
        denominator: u32,
        position_sample: u64,
        sample_rate: u32,
    ) {
        self.metronome_ts_num = numerator.clamp(1, 64);
        self.metronome_ts_den = denominator.clamp(1, 64);
        self.time_signature_map =
            crate::time_signature_map::RuntimeTimeSignatureMapSnapshot::static_sig(
                self.metronome_ts_num as u16,
                self.metronome_ts_den as u16,
            );
        self.reset_metronome_schedule(position_sample, sample_rate);
    }

    pub fn set_time_signature_map(
        &mut self,
        map: crate::time_signature_map::RuntimeTimeSignatureMapSnapshot,
        position_sample: u64,
        sample_rate: u32,
    ) {
        self.time_signature_map = map;
        if let Some(pt) = self.time_signature_map.points().first() {
            self.metronome_ts_num = pt.numerator as u32;
            self.metronome_ts_den = pt.denominator as u32;
        }
        self.reset_metronome_schedule(position_sample, sample_rate);
    }

    pub fn clear_metronome_clicks(&mut self, reason: &str) {
        self.metronome_click_remaining = 0;
        self.metronome_click_gain = 0.0;
        self.metronome_click_phase = 0.0;
        if callback_debug_enabled() {
            eprintln!("[Metronome] clear scheduled clicks reason={reason}");
        }
    }

    pub fn set_metronome_suspended(&mut self, suspended: bool) {
        if self.metronome_suspended == suspended {
            return;
        }
        self.metronome_suspended = suspended;
        self.clear_metronome_clicks(if suspended { "suspend" } else { "resume" });
        if callback_debug_enabled() {
            if suspended {
                eprintln!("[Metronome] suspend during drag");
            } else {
                eprintln!("[Metronome] resume after drag");
            }
        }
    }

    pub fn reset_metronome_schedule(&mut self, position_sample: u64, sample_rate: u32) {
        self.clear_metronome_clicks("seek");
        let sr = sample_rate.max(1) as f64;
        let current_beat = self.tempo_map.beat_at_samples(position_sample, sr);
        self.metronome_next_beat = self
            .time_signature_map
            .next_metronome_click_at_or_after(current_beat);
        if callback_debug_enabled() {
            eprintln!(
                "[Metronome] reset phase position={position_sample} next_beat={:.3}",
                self.metronome_next_beat
            );
        }
    }

    #[inline]
    pub fn metronome_sample(
        &mut self,
        output_sample_position: u64,
        click_render_sample_offset_in_block: u64,
        sample_rate: u32,
        transport_playing: bool,
        graph_max_latency_samples: u32,
        metronome_compensation_delay_samples: u32,
    ) -> f32 {
        if !self.metronome_enabled || self.metronome_suspended || !transport_playing {
            if !transport_playing {
                self.metronome_click_remaining = 0;
            }
            return 0.0;
        }

        let sr = sample_rate.max(1) as f64;
        // The transport/playhead remains raw project time. Clicks are emitted at
        // the output sample that carries the same project beat after realtime PDC
        // and master-insert latency have made rendered tracks audible.
        let compensation_delay = metronome_compensation_delay_samples as u64;
        while {
            let next_click_sample_raw =
                self.tempo_map.samples_at_beat(self.metronome_next_beat, sr);
            let next_click_sample_compensated =
                next_click_sample_raw.saturating_add(compensation_delay);
            output_sample_position >= next_click_sample_compensated
        } {
            let next_click_sample_raw =
                self.tempo_map.samples_at_beat(self.metronome_next_beat, sr);
            let next_click_sample_compensated =
                next_click_sample_raw.saturating_add(compensation_delay);
            let accent = self
                .time_signature_map
                .metronome_accent_at_beat(self.metronome_next_beat);
            let (freq, gain) = match accent {
                crate::time_signature_map::MetronomeAccent::Downbeat => (1760.0, 0.34),
                crate::time_signature_map::MetronomeAccent::Group => (1320.0, 0.28),
                crate::time_signature_map::MetronomeAccent::Normal => (980.0, 0.22),
            };
            self.metronome_click_phase = 0.0;
            self.metronome_click_phase_inc = freq / sr;
            self.metronome_click_gain = gain;
            self.metronome_click_remaining = self.metronome_click_len;
            if metronome_debug_enabled() {
                let compensated_audible_sample_position =
                    output_sample_position.saturating_sub(compensation_delay);
                eprintln!(
                    "[metronome-sync] metronome_enabled={} raw_transport_sample_position={} \
                     compensated_audible_sample_position={} graph_max_latency_samples={} \
                     metronome_compensation_delay_samples={} next_click_sample_raw={} \
                     next_click_sample_compensated={} click_render_sample_offset_in_block={} \
                     tempo_at_click={:.3} time_signature_at_click={}/{} playback_graph_version=unknown",
                    self.metronome_enabled,
                    output_sample_position,
                    compensated_audible_sample_position,
                    graph_max_latency_samples,
                    metronome_compensation_delay_samples,
                    next_click_sample_raw,
                    next_click_sample_compensated,
                    click_render_sample_offset_in_block,
                    self.tempo_map.bpm_at_beat(self.metronome_next_beat),
                    self.metronome_ts_num,
                    self.metronome_ts_den,
                );
            }
            self.metronome_next_beat = self
                .time_signature_map
                .next_metronome_click_after(self.metronome_next_beat);
        }

        if self.metronome_click_remaining == 0 {
            return 0.0;
        }

        let age = self
            .metronome_click_len
            .saturating_sub(self.metronome_click_remaining) as f32;
        let t = age / self.metronome_click_len.max(1) as f32;
        let env = (1.0 - t).max(0.0);
        let sample = (self.metronome_click_phase * std::f64::consts::TAU).sin() as f32
            * env
            * env
            * self.metronome_click_gain;
        self.metronome_click_phase += self.metronome_click_phase_inc;
        self.metronome_click_phase -= self.metronome_click_phase.floor();
        self.metronome_click_remaining = self.metronome_click_remaining.saturating_sub(1);
        sample
    }
}

#[inline]
pub(crate) fn metronome_graph_max_latency_samples(runtime: &RuntimeProject) -> u32 {
    if runtime.pdc_enabled {
        runtime.latency_graph.max_path_latency_samples
    } else {
        0
    }
}

#[inline]
pub(crate) fn metronome_compensation_delay_samples(runtime: &RuntimeProject) -> u32 {
    // The metronome is mixed after project graph/master processing, so it needs
    // the track-graph PDC delay plus latency added by master inserts.
    metronome_graph_max_latency_samples(runtime)
        .saturating_add(runtime.latency_graph.master_plugin_latency)
}

// ── f32 helper store/load ─────────────────────────────────────────────────────

#[inline]
pub fn f32_store(v: f32) -> u32 {
    v.to_bits()
}
#[inline]
pub fn f32_load(v: u32) -> f32 {
    f32::from_bits(v)
}

// ── Command drain ─────────────────────────────────────────────────────────────

/// Drain all pending engine commands.  Returns true if the engine should stop.
///
/// Realtime-safe: only modifies local state or atomics.
pub fn drain_commands(
    cmd_rx: &crossbeam_channel::Receiver<EngineCommand>,
    runtime: &mut RuntimeProject,
    shared: &Arc<SharedState>,
    local: &mut LocalAudioState,
    output_sample_rate: u32,
) -> bool {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            EngineCommand::LoadProject(next_runtime) => {
                if callback_debug_enabled() {
                    eprintln!(
                        "[DAUx] LoadProject: {} tracks, {} clips (sr={})",
                        next_runtime.tracks.len(),
                        next_runtime.clips.len(),
                        output_sample_rate,
                    );
                }
                // Swap in the new graph and retire the old one to the
                // background dropper — never run its destructor on this
                // realtime thread (frees buffers / munmaps sources / destroys
                // VST3 handles). See `crate::graveyard`.
                runtime.all_notes_off("project_load");
                let old = std::mem::replace(runtime, *next_runtime);
                runtime.sample_rate = output_sample_rate;
                // Preserve the plugin-bridge sinks across reloads (Stage 3b) — a
                // freshly built project never carries them.
                runtime.plugin_bridge_sinks = old.plugin_bridge_sinks.clone();
                // Re-cache the per-insert sink handles on the fresh graph (the
                // block path reads insert.bridge_sink, never the map).
                runtime.resolve_bridge_sinks();
                runtime.bridge_editor_active = old.bridge_editor_active.clone();
                // The panic the all_notes_off above pushed into the (preserved)
                // sinks still needs flushing through the new graph.
                runtime.bridge_panic_flush_samples = old.bridge_panic_flush_samples;
                runtime.bridge_preview_tail_samples = old.bridge_preview_tail_samples;
                // Keep the live master-fader ramp continuous across media/control
                // graph swaps (clip mute/gain sync). Prefer the live master
                // atomic (authoritative fader position) so a fresh runtime that
                // still carries the build-time seed of 1.0 cannot briefly
                // bypass a lowered master on the first blocks after LoadProject.
                runtime.smoothed_master_gain =
                    f32_load(shared.master_volume.load(Ordering::Relaxed));
                crate::graveyard::retire(old);
                // Transport/audio-graph separation: a graph swap must never
                // change the user's transport state.  If the transport was
                // Running when the swap arrived (e.g. an insert was added
                // during playback), keep rendering the new graph immediately —
                // the user must not have to press Play again.  If the
                // transport was stopped (project open/close paths call
                // StopTransport first, which clears `shared.playing`), the
                // swap lands in Paused exactly as before.
                let was_playing = shared.playing.load(Ordering::Relaxed);
                local.playing_local = was_playing;
                let pos = shared.position_samples.load(Ordering::Relaxed);
                runtime.reset_midi_playback(pos);
                local.set_tempo_map(runtime.tempo_map.clone(), pos, output_sample_rate);
                let old_state = crate::engine::AudioEngineState::from_u8(
                    shared.engine_state.load(Ordering::Relaxed),
                );
                let new_state = if was_playing {
                    crate::engine::AudioEngineState::Running
                } else {
                    crate::engine::AudioEngineState::Paused
                };
                shared
                    .engine_state
                    .store(new_state as u8, Ordering::Relaxed);
                if callback_debug_enabled() || command_debug_enabled() {
                    eprintln!(
                        "[AudioEngineState] old={old_state:?} new={new_state:?} source=graph_swap was_playing={was_playing}"
                    );
                }
            }
            EngineCommand::SetTestTone { enabled, frequency } => {
                local.osc_on = enabled;
                local.osc_freq = frequency;
                local.osc_l.set_frequency(frequency as f64);
                local.osc_r.set_frequency(frequency as f64);
            }
            EngineCommand::StartAudition { source } => {
                if let Some(old) = local.audition.replace(AudioFileAudition::new(source)) {
                    crate::graveyard::retire_audio_file(old.into_source());
                }
            }
            EngineCommand::StopAudition => {
                if let Some(old) = local.audition.take() {
                    crate::graveyard::retire_audio_file(old.into_source());
                }
            }
            EngineCommand::StartTransport => {
                let pos = shared.position_samples.load(Ordering::Relaxed);
                let active_clips = runtime.active_clip_count_at_sample(pos);
                if command_debug_enabled() {
                    eprintln!(
                        "[DAUx] StartTransport: pos={}sa ({:.3}s), active={}, scheduled={}",
                        pos,
                        pos as f64 / output_sample_rate as f64,
                        active_clips,
                        runtime.clips.len(),
                    );
                }
                if transport_freeze_debug_enabled() {
                    eprintln!("[play-debug callback] StartTransport command applied");
                    POST_PLAY_CALLBACK_LOGS.store(5, Ordering::Relaxed);
                }
                local.playing_local = true;
                local.stop_tail_samples = 0;
                shared.playing.store(true, Ordering::Relaxed);
                let old_state = crate::engine::AudioEngineState::from_u8(shared.engine_state.swap(
                    crate::engine::AudioEngineState::Running as u8,
                    Ordering::Relaxed,
                ));
                if callback_debug_enabled() || command_debug_enabled() {
                    eprintln!(
                        "[AudioEngineState] old={old_state:?} new=Running source=StartTransport"
                    );
                }
                runtime.reset_midi_playback(pos);
                // Clear stale PDC delay-line audio so the compensated tracks start
                // settled and stay aligned with plugin/VSTi-latency tracks from the
                // first audible block — parity with offline export's fresh-runtime
                // + warmup start. Realtime-safe zero-fill; runs only on Start.
                runtime.reset_pdc_delay_lines();
                local.reset_metronome_schedule(pos, output_sample_rate);
                if pdc_debug_enabled() {
                    runtime.dump_latency_compensation_graph("StartTransport");
                    eprintln!(
                        "[metronome-sync] context=StartTransport metronome_enabled={} \
                         raw_transport_sample_position={} graph_max_latency_samples={} \
                         metronome_compensation_delay_samples={}",
                        local.metronome_enabled,
                        pos,
                        metronome_graph_max_latency_samples(runtime),
                        metronome_compensation_delay_samples(runtime),
                    );
                }
            }
            EngineCommand::StopTransport => {
                if command_debug_enabled() {
                    eprintln!("[DAUx] StopTransport");
                }
                local.playing_local = false;
                shared.playing.store(false, Ordering::Relaxed);
                local.stop_tail_samples = post_stop_tail_samples(runtime.sample_rate);
                let old_state = crate::engine::AudioEngineState::from_u8(shared.engine_state.swap(
                    crate::engine::AudioEngineState::Paused as u8,
                    Ordering::Relaxed,
                ));
                if callback_debug_enabled() || command_debug_enabled() {
                    eprintln!(
                        "[AudioEngineState] old={old_state:?} new=Paused source=StopTransport"
                    );
                }
                runtime.all_notes_off("stop");
                // Drop latency-compensated pre-stop audio immediately so Stop
                // cuts at the command. Plugin release / reverb still ring out
                // via `stop_tail_samples`; only the PDC rings are cleared.
                runtime.reset_pdc_delay_lines();
            }
            EngineCommand::Seek { position_seconds } => {
                let sr = shared.sample_rate.load(Ordering::Relaxed) as f64;
                let pos = (position_seconds * sr) as u64;
                if command_debug_enabled() {
                    eprintln!("[DAUx] Seek -> {:.3}s ({}sa)", position_seconds, pos);
                }
                shared.position_samples.store(pos, Ordering::Relaxed);
                local.reset_metronome_schedule(pos, output_sample_rate);
                runtime.reset_midi_playback(pos);
                // A seek repositions the playhead; the PDC delay lines still hold
                // audio from the pre-seek position. Clear them so the compensated
                // tracks refill from the new position and stay aligned (spec:
                // "Seeking must reset and refill latency compensation buffers").
                runtime.reset_pdc_delay_lines();
                if pdc_debug_enabled() {
                    runtime.dump_latency_compensation_graph("Seek");
                    eprintln!(
                        "[metronome-sync] context=Seek metronome_enabled={} \
                         raw_transport_sample_position={} graph_max_latency_samples={} \
                         metronome_compensation_delay_samples={}",
                        local.metronome_enabled,
                        pos,
                        metronome_graph_max_latency_samples(runtime),
                        metronome_compensation_delay_samples(runtime),
                    );
                }
            }
            EngineCommand::SetMetronomeEnabled(enabled) => {
                let pos = shared.position_samples.load(Ordering::Relaxed);
                shared.metronome_enabled.store(enabled, Ordering::Relaxed);
                local.set_metronome_enabled(enabled, pos, output_sample_rate);
            }
            EngineCommand::SetMetronomeSuspended(suspended) => {
                local.set_metronome_suspended(suspended);
            }
            EngineCommand::SetBpm(bpm) => {
                let pos = shared.position_samples.load(Ordering::Relaxed);
                transport::store_f64_bits(&shared.bpm_bits, bpm);
                let map = crate::tempo_map::RuntimeTempoMapSnapshot::static_tempo(bpm);
                let next_pos = runtime.apply_tempo_map(map, pos);
                shared.position_samples.store(next_pos, Ordering::Relaxed);
                local.set_tempo_map(runtime.tempo_map.clone(), next_pos, output_sample_rate);
            }
            EngineCommand::SetTempoMap(map) => {
                let pos = shared.position_samples.load(Ordering::Relaxed);
                let next_pos = runtime.apply_tempo_map(map, pos);
                shared.position_samples.store(next_pos, Ordering::Relaxed);
                local.set_tempo_map(runtime.tempo_map.clone(), next_pos, output_sample_rate);
            }
            EngineCommand::SetTimeSignature(num, den) => {
                let pos = shared.position_samples.load(Ordering::Relaxed);
                shared.time_sig_num.store(num.max(1), Ordering::Relaxed);
                shared.time_sig_den.store(den.max(1), Ordering::Relaxed);
                local.set_time_signature(num, den, pos, output_sample_rate);
            }
            EngineCommand::SetTimeSignatureMap(map) => {
                let pos = shared.position_samples.load(Ordering::Relaxed);
                if let Some(pt) = map.points().first() {
                    shared
                        .time_sig_num
                        .store(pt.numerator.max(1) as u32, Ordering::Relaxed);
                    shared
                        .time_sig_den
                        .store(pt.denominator.max(1) as u32, Ordering::Relaxed);
                }
                local.set_time_signature_map(map, pos, output_sample_rate);
            }
            EngineCommand::SetLoop {
                enabled,
                start_seconds,
                end_seconds,
            } => {
                let sr = shared.sample_rate.load(Ordering::Relaxed) as f64;
                let start = (start_seconds.max(0.0) * sr) as u64;
                let end = (end_seconds.max(0.0) * sr) as u64;
                shared.loop_enabled.store(enabled, Ordering::Relaxed);
                shared.loop_start_samples.store(start, Ordering::Relaxed);
                shared.loop_end_samples.store(end, Ordering::Relaxed);
            }
            EngineCommand::SetMasterVolume { value } => {
                shared
                    .master_volume
                    .store(f32_store(value), Ordering::Relaxed);
            }
            EngineCommand::SetPluginBridgeSink { insert_id, sink } => {
                match sink {
                    Some(sink) => {
                        runtime.plugin_bridge_sinks.insert(insert_id, sink);
                    }
                    None => {
                        runtime.plugin_bridge_sinks.remove(&insert_id);
                    }
                }
                // Re-cache per-insert sink handles for the block path.
                runtime.resolve_bridge_sinks();
            }
            EngineCommand::CommandBarrier { ack } => {
                // Wait-free ack: every command sent before this one has now
                // been applied to the callback's runtime.
                ack.store(true, Ordering::Release);
            }
            EngineCommand::SetBridgeEditorActive { track_id, active } => {
                runtime.set_bridge_editor_active(&track_id, active);
                if !active {
                    // UI/control path command consumed on the realtime callback.
                    // The plugin editor's own VSTi keyboard is internal to the
                    // bridged host, so closing the editor does not show up as an
                    // engine MIDI preview. Keep the graph/bridge handshake alive
                    // long enough for the host to drain note-off and release tails.
                    local.preview_tail_samples = local
                        .preview_tail_samples
                        .max(post_stop_tail_samples(runtime.sample_rate));
                }
            }
            EngineCommand::SetTrackVolume { track_id, value } => {
                runtime.update_track_volume(&track_id, value);
            }
            EngineCommand::SetTrackPan { track_id, value } => {
                runtime.update_track_pan(&track_id, value);
            }
            EngineCommand::SetTrackMute { track_id, muted } => {
                if callback_debug_enabled() {
                    eprintln!("[DAUx] SetTrackMute track={track_id} muted={muted}");
                }
                // Scoped note-off (mirrors the cpal path): only newly-inaudible
                // tracks release notes; other tracks keep sounding.
                runtime.update_track_mute(&track_id, muted);
                runtime.notes_off_for_inaudible_tracks("track_mute");
            }
            EngineCommand::SetTrackSolo { track_id, solo } => {
                runtime.update_track_solo(&track_id, solo);
                runtime.notes_off_for_inaudible_tracks("track_solo");
            }
            EngineCommand::SetTrackInputState {
                track_index,
                record_armed,
                monitor_enabled,
                input_source,
            } => runtime.update_track_input_state(
                track_index,
                record_armed,
                monitor_enabled,
                input_source,
            ),
            EngineCommand::SetTrackPreviewMode { track_id, value } => {
                runtime.update_track_preview_mode(&track_id, RuntimePreviewMode::from_code(value));
            }
            EngineCommand::SetInsertParam {
                track_id,
                insert_id,
                param_id,
                value,
            } => {
                runtime.update_insert_param(&track_id, &insert_id, &param_id, value);
            }
            EngineCommand::MidiPreviewNoteOn {
                track_id,
                channel,
                pitch,
                velocity,
            } => {
                runtime.midi_preview_note_on(&track_id, channel, pitch, velocity);
            }
            EngineCommand::MidiPreviewNoteOff {
                track_id,
                channel,
                pitch,
            } => {
                runtime.midi_preview_note_off(&track_id, channel, pitch);
            }
            EngineCommand::MidiPreviewControlChange {
                track_id,
                channel,
                controller,
                value,
            } => {
                runtime.midi_preview_control_change(&track_id, channel, controller, value);
            }
            EngineCommand::MidiPreviewAllNotesOff { track_id } => {
                runtime.midi_preview_all_notes_off(&track_id);
            }
            EngineCommand::PluginPreviewNoteOn {
                track_id,
                plugin_instance_id,
                channel,
                pitch,
                velocity,
            } => {
                if crate::forensic_trace::engine_midi_verbose_enabled() {
                    eprintln!(
                        "[midi-preview-audio] dequeue note_on instance={plugin_instance_id} pitch={pitch}"
                    );
                }
                runtime.bridge_preview_note_on(
                    &track_id,
                    &plugin_instance_id,
                    channel,
                    pitch,
                    velocity,
                );
            }
            EngineCommand::PluginPreviewNoteOff {
                track_id,
                plugin_instance_id,
                channel,
                pitch,
            } => {
                if crate::forensic_trace::engine_midi_verbose_enabled() {
                    eprintln!(
                        "[midi-preview-audio] dequeue note_off instance={plugin_instance_id} pitch={pitch}"
                    );
                }
                runtime.bridge_preview_note_off(&track_id, &plugin_instance_id, channel, pitch);
            }
            EngineCommand::PluginPreviewControlChange {
                track_id,
                plugin_instance_id,
                channel,
                controller,
                value,
            } => {
                runtime.bridge_preview_control_change(
                    &track_id,
                    &plugin_instance_id,
                    channel,
                    controller,
                    value,
                );
            }
            EngineCommand::PluginPreviewAllNotesOff {
                track_id,
                plugin_instance_id,
            } => {
                runtime.bridge_preview_all_notes_off(&track_id, &plugin_instance_id);
            }
        }
    }
    false
}

// ── Core f32 stereo render ────────────────────────────────────────────────────

/// Output-callback block of the last slow-block log (throttle: one watchdog
/// log per ~200 callbacks so a sustained stall cannot flood stderr).
static SLOW_CALLBACK_LAST_LOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Fill interleaved f32 output data (stereo, `channels` wide).
///
/// Returns the number of frames written.
/// Realtime-safe — no allocation, no locking.
///
/// Wraps the render kernel with the callback-duration watchdog (audio-hang
/// spec §12): publishes last/max duration to [`SharedState`] and emits a
/// throttled warning when a block exceeds the realtime budget.
pub fn fill_output_f32(
    data: &mut [f32],
    channels: usize,
    runtime: &mut RuntimeProject,
    shared: &Arc<SharedState>,
    local: &mut LocalAudioState,
) -> u64 {
    let started = std::time::Instant::now();
    let frames = fill_output_f32_inner(data, channels, runtime, shared, local);
    let elapsed_us = started.elapsed().as_micros().min(u32::MAX as u128) as u32;
    // Publish last/max/deadline + classify dropout-risk against the active
    // protection mode (shared with the legacy callback so both paths agree).
    let block_frames = data.len().checked_div(channels).unwrap_or(0);
    crate::engine::record_output_callback_timing(
        shared,
        elapsed_us,
        block_frames,
        shared.sample_rate.load(Ordering::Relaxed),
    );
    if elapsed_us >= 5_000 {
        let cb = shared.output_cb_count.load(Ordering::Relaxed);
        let last = SLOW_CALLBACK_LAST_LOG.load(Ordering::Relaxed);
        if cb.wrapping_sub(last) > 200 {
            SLOW_CALLBACK_LAST_LOG.store(cb, Ordering::Relaxed);
            let state = crate::engine::AudioEngineState::from_u8(
                shared.engine_state.load(Ordering::Relaxed),
            );
            let severity = if elapsed_us >= 10_000 {
                "error"
            } else {
                "warning"
            };
            eprintln!(
                "[AudioCallback] slow block severity={severity} duration_us={elapsed_us} state={} frames={frames}",
                state.as_str()
            );
        }
    }
    frames
}

fn fill_output_f32_inner(
    data: &mut [f32],
    channels: usize,
    runtime: &mut RuntimeProject,
    shared: &Arc<SharedState>,
    local: &mut LocalAudioState,
) -> u64 {
    shared.output_cb_count.fetch_add(1, Ordering::Relaxed);
    let engine_state =
        crate::engine::AudioEngineState::from_u8(shared.engine_state.load(Ordering::Relaxed));
    // The transport only advances in Running — a stale `playing_local` left
    // over from a graph swap must never drive rendering while Paused.
    let transport_playing =
        local.playing_local && matches!(engine_state, crate::engine::AudioEngineState::Running);
    let software_monitoring = shared.monitor_enabled_any.load(Ordering::Relaxed)
        && shared.live_input_active.load(Ordering::Relaxed)
        && shared.input_ring.is_active();
    if engine_state.outputs_silence() {
        // Paused still services MIDI preview, the post-panic bridge flush, and
        // open plugin editors: those need the block/handshake loop alive so
        // bridged VSTi hosts keep draining MIDI and producing audio while the
        // transport is stopped. Every other non-Running state (loading /
        // closing / device switch / suspended) is hard silence.
        let preview_wake = matches!(engine_state, crate::engine::AudioEngineState::Paused)
            && (runtime.has_active_midi_preview()
                || runtime.bridge_panic_flush_samples > 0
                || runtime.bridge_preview_tail_samples > 0
                || runtime.has_bridge_editor_active()
                || local.preview_tail_samples > 0
                || local.stop_tail_samples > 0
                || software_monitoring
                || runtime
                    .tracks
                    .iter()
                    .any(|t| !t.midi_block_events.is_empty()));
        // When preview_wake holds, fall through to the normal body with the
        // transport treated as stopped — only preview/flush processing runs.
        if !preview_wake {
            for sample in data.iter_mut() {
                *sample = 0.0;
            }
            local.preview_tail_samples = 0;
            local.stop_tail_samples = 0;
            local.prev_peak_l = 0.0;
            local.prev_peak_r = 0.0;
            shared
                .peak_l
                .store(crate::engine::f32_store(0.0), Ordering::Relaxed);
            shared
                .peak_r
                .store(crate::engine::f32_store(0.0), Ordering::Relaxed);
            shared
                .rms_l
                .store(crate::engine::f32_store(0.0), Ordering::Relaxed);
            shared
                .rms_r
                .store(crate::engine::f32_store(0.0), Ordering::Relaxed);
            runtime.end_meter_block(0);
            let frames = data.len() / channels.max(1);
            if callback_debug_enabled() && shared.output_cb_count.load(Ordering::Relaxed) % 400 == 1
            {
                eprintln!(
                    "[AudioEngine] callback silence reason={} frames={frames}",
                    engine_state.as_str()
                );
                eprintln!("[AudioEngine] output cleared");
            }
            return frames as u64;
        }
    }
    if transport_playing {
        log_post_play_callback("block entered");
    }
    // Sync oscillator from atomics (set from control thread between blocks).
    let tone_on = shared.test_tone_enabled.load(Ordering::Relaxed);
    let tone_freq = f32_load(shared.test_tone_freq.load(Ordering::Relaxed));
    if tone_freq != local.osc_freq {
        local.osc_freq = tone_freq;
        local.osc_l.set_frequency(tone_freq as f64);
        local.osc_r.set_frequency(tone_freq as f64);
    }
    let gen_tone = tone_on || local.osc_on;
    let master_vol = f32_load(shared.master_volume.load(Ordering::Relaxed));
    let loop_bounds = if transport_playing {
        transport::active_loop_bounds(shared)
    } else {
        None
    };
    let raw_base_sample = shared.position_samples.load(Ordering::Relaxed);
    let base_sample = transport::normalize_loop_position(raw_base_sample, loop_bounds);
    if base_sample != raw_base_sample {
        shared
            .position_samples
            .store(base_sample, Ordering::Relaxed);
        runtime.reset_midi_playback(base_sample);
        local.reset_metronome_schedule(base_sample, runtime.sample_rate);
    }

    let mut frames = 0u64;
    runtime.begin_meter_block();

    let mut end_loop_midi_reset = None;
    if transport_playing {
        let frames_needed = data.len().checked_div(channels).unwrap_or(0) as u64;
        if frames_needed > 0 {
            end_loop_midi_reset = crate::engine::schedule_midi_render_block(
                runtime,
                base_sample,
                frames_needed,
                loop_bounds,
            );
        }
    }

    let pending_midi = channels > 0
        && runtime
            .tracks
            .iter()
            .any(|t| !t.midi_block_events.is_empty());
    let frames_in_block = data.len().checked_div(channels).unwrap_or(0) as u64;
    let has_preview = runtime.has_active_midi_preview();
    if transport_playing {
        // Transport drives processing while playing; don't carry a stale tail.
        local.preview_tail_samples = 0;
        local.stop_tail_samples = 0;
        // Playing blocks request/drain the bridge anyway — flush is implicit.
        runtime.bridge_panic_flush_samples = 0;
    } else if has_preview || pending_midi {
        // A preview note is held (or its on/off just queued) — keep enough tail
        // queued to render the instrument's release after the eventual note-off.
        local.preview_tail_samples = post_stop_tail_samples(runtime.sample_rate);
    }
    // Post-panic flush: keep requesting bridge blocks until the host has had
    // time to drain the panic CCs (stop/seek/mute) — counted down per block.
    let panic_flush = runtime.bridge_panic_flush_samples > 0;
    if !transport_playing && panic_flush {
        runtime.bridge_panic_flush_samples = runtime
            .bridge_panic_flush_samples
            .saturating_sub(frames_in_block);
    }
    let bridge_preview_tail = runtime.bridge_preview_tail_samples > 0;
    if !transport_playing && bridge_preview_tail {
        runtime.bridge_preview_tail_samples = runtime
            .bridge_preview_tail_samples
            .saturating_sub(frames_in_block);
    }
    // An open external plugin editor keeps the graph rendering while stopped
    // so the plugin's own UI keyboard stays audible (parity with the legacy
    // callback path).
    let bridge_editor_wakeup = runtime.has_bridge_editor_active();
    let audition_active = local.audition.is_some();
    let preview_render_active = has_preview
        || pending_midi
        || panic_flush
        || bridge_preview_tail
        || bridge_editor_wakeup
        || local.preview_tail_samples > 0
        || local.stop_tail_samples > 0
        || audition_active
        || software_monitoring;
    if preview_render_active
        && !transport_playing
        && (has_preview || pending_midi || local.preview_tail_samples > 0)
    {
        let active_notes: usize = runtime
            .midi_tracks
            .iter()
            .map(|mt| mt.preview_active.len())
            .sum();
        let active_u32 = active_notes as u32;
        let changed = active_u32 != local.prev_logged_preview_notes;
        if changed {
            if callback_debug_enabled() {
                eprintln!(
                    "[PreviewRenderWake] active_preview_notes changed {} -> {} tail_samples={}",
                    local.prev_logged_preview_notes, active_u32, local.preview_tail_samples
                );
            }
            local.prev_logged_preview_notes = active_u32;
            local.preview_wake_log_cooldown = 0;
        } else if active_notes > 0 && callback_debug_enabled() {
            local.preview_wake_log_cooldown = local.preview_wake_log_cooldown.saturating_add(1);
            let sr = runtime.sample_rate.max(1);
            let log_interval_blocks = (sr / frames_in_block.max(1) as u32).max(1);
            if local.preview_wake_log_cooldown >= log_interval_blocks {
                local.preview_wake_log_cooldown = 0;
                eprintln!(
                    "[PreviewRenderWake] active_preview_notes={} tail_samples={} rendering_while_stopped=true",
                    active_notes, local.preview_tail_samples
                );
            }
        }
        // Once no note is held and nothing is queued, the remaining tail is pure
        // decay — count it down so processing eventually stops.
        if !has_preview && !pending_midi {
            local.preview_tail_samples = local.preview_tail_samples.saturating_sub(frames_in_block);
            if local.preview_tail_samples == 0 {
                local.prev_logged_preview_notes = u32::MAX;
            }
        }
    }
    if !transport_playing && local.stop_tail_samples > 0 {
        local.stop_tail_samples = local.stop_tail_samples.saturating_sub(frames_in_block);
    }

    let monitor_input_ready = if shared.live_input_active.load(Ordering::Relaxed) {
        // Per-track input meters from the latest captured sample (Layer 6).
        let input_l = f32_load(shared.live_input_l.load(Ordering::Relaxed));
        let input_r = f32_load(shared.live_input_r.load(Ordering::Relaxed));
        let source_pair = shared.monitor_source_pair();
        runtime.accumulate_live_input_meters(input_l, input_r, source_pair);
        read_monitor_input(frames_in_block as usize, shared, local)
    } else {
        clear_input_bus_meter(shared, local);
        false
    };

    if channels >= 2 && (transport_playing || preview_render_active) {
        frames = if software_monitoring && monitor_input_ready {
            render_project_block_interleaved_with_live_input(
                runtime,
                base_sample,
                master_vol,
                data,
                channels,
                transport_playing,
                shared.time_sig_num.load(Ordering::Relaxed),
                shared.time_sig_den.load(Ordering::Relaxed),
                loop_bounds,
                &local.monitor_input_l[..frames_in_block as usize],
                &local.monitor_input_r[..frames_in_block as usize],
            )
        } else {
            render_project_block_interleaved(
                runtime,
                base_sample,
                master_vol,
                data,
                channels,
                transport_playing,
                shared.time_sig_num.load(Ordering::Relaxed),
                shared.time_sig_den.load(Ordering::Relaxed),
                loop_bounds,
            )
        };
        let audition_finished = local
            .audition
            .as_mut()
            .map(|audition| audition.mix_into(data, channels, runtime.sample_rate))
            .unwrap_or(false);
        if audition_finished {
            if let Some(audition) = local.audition.take() {
                crate::graveyard::retire_audio_file(audition.into_source());
            }
        }
        if !local.render_path_logged {
            local.render_path_logged = true;
            if callback_debug_enabled() {
                eprintln!(
                    "[SphereAudio callback] renderPath=daux-block frames={} channels={} tracks={}",
                    frames,
                    channels,
                    runtime.tracks.len()
                );
            }
        }
        let metronome_graph_max_samples = metronome_graph_max_latency_samples(runtime);
        let metronome_delay_samples = metronome_compensation_delay_samples(runtime);
        if gen_tone {
            for frame in data.chunks_mut(channels) {
                let tone_l = local.osc_l.next_sample() * TEST_TONE_AMPLITUDE * master_vol;
                let tone_r = local.osc_r.next_sample() * TEST_TONE_AMPLITUDE * master_vol;
                frame[0] = (frame[0] + tone_l).clamp(-1.0, 1.0);
                frame[1] = (frame[1] + tone_r).clamp(-1.0, 1.0);
            }
        }
        let mut segment_sample = base_sample;
        let mut callback_offset = 0usize;
        let mut remaining = frames;
        while remaining > 0 {
            let segment_frames =
                transport::segment_frames_until_loop_wrap(segment_sample, remaining, loop_bounds);
            for i in 0..segment_frames as usize {
                let frame = &mut data
                    [(callback_offset + i) * channels..(callback_offset + i) * channels + channels];
                let click = local.metronome_sample(
                    segment_sample + i as u64,
                    (callback_offset + i) as u64,
                    runtime.sample_rate,
                    transport_playing,
                    metronome_graph_max_samples,
                    metronome_delay_samples,
                );
                if click != 0.0 {
                    frame[0] = (frame[0] + click * master_vol).clamp(-1.0, 1.0);
                    frame[1] = (frame[1] + click * master_vol).clamp(-1.0, 1.0);
                }
            }
            callback_offset += segment_frames as usize;
            remaining -= segment_frames;
            if remaining == 0 {
                break;
            }
            let (next_sample, wrapped) =
                transport::advance_loop_position(segment_sample, segment_frames, loop_bounds);
            if wrapped {
                local.reset_metronome_schedule(next_sample, runtime.sample_rate);
            }
            segment_sample = next_sample;
        }
        // Live monitoring is mixed below via the input ring (single, clean
        // path) — the old per-block sample-and-hold monitor was removed because
        // it held one input sample across the whole output block (warble).

        if !transport_playing
            && runtime.bridge_preview_tail_samples > 0
            && data.iter().any(|sample| sample.abs() > 0.00001)
        {
            runtime.bridge_preview_tail_samples = post_stop_tail_samples(runtime.sample_rate);
        }
    } else if channels >= 2 {
        let metronome_graph_max_samples = metronome_graph_max_latency_samples(runtime);
        let metronome_delay_samples = metronome_compensation_delay_samples(runtime);
        for frame in data.chunks_mut(channels) {
            let (tone_l, tone_r) = if gen_tone {
                (
                    local.osc_l.next_sample() * TEST_TONE_AMPLITUDE * master_vol,
                    local.osc_r.next_sample() * TEST_TONE_AMPLITUDE * master_vol,
                )
            } else {
                (0.0, 0.0)
            };
            let (proj_l, proj_r) = if transport_playing {
                render_project_sample(runtime, base_sample + frames, master_vol)
            } else {
                (0.0, 0.0)
            };
            let click = local.metronome_sample(
                base_sample + frames,
                frames,
                runtime.sample_rate,
                transport_playing,
                metronome_graph_max_samples,
                metronome_delay_samples,
            ) * master_vol;
            let l = (tone_l + proj_l + click).clamp(-1.0, 1.0);
            let r = (tone_r + proj_r + click).clamp(-1.0, 1.0);
            // Live monitor is added afterwards from the input ring (see below).
            frame[0] = l;
            frame[1] = r;
            for extra in frame.iter_mut().skip(2) {
                *extra = 0.0;
            }
            frames += 1;
        }
    } else if channels == 1 {
        let metronome_graph_max_samples = metronome_graph_max_latency_samples(runtime);
        let metronome_delay_samples = metronome_compensation_delay_samples(runtime);
        for sample in data.iter_mut() {
            let tone = if gen_tone {
                local.osc_l.next_sample() * TEST_TONE_AMPLITUDE * master_vol
            } else {
                0.0
            };
            let (proj_l, proj_r) = if transport_playing {
                render_project_sample(runtime, base_sample + frames, master_vol)
            } else {
                (0.0, 0.0)
            };
            let click = local.metronome_sample(
                base_sample + frames,
                frames,
                runtime.sample_rate,
                transport_playing,
                metronome_graph_max_samples,
                metronome_delay_samples,
            ) * master_vol;
            let v = (tone + (proj_l + proj_r) * 0.5 + click).clamp(-1.0, 1.0);
            *sample = v;
            frames += 1;
        }
    }

    // Legacy master-bus bridge fallback (disabled by default — per-track routing
    // through external-bridge-plugin inserts is the normal path).
    if plugin_bridge_master_fallback_enabled() {
        let _ = mix_plugin_bridge(data, channels, runtime, master_vol);
    }

    // Meter the final output after playback, software monitoring, and bridge
    // contributions have all been summed. This avoids under-reporting monitor
    // gain and catches clipping caused by the actual final mix.
    let mut peak_l = 0.0f32;
    let mut peak_r = 0.0f32;
    let mut sum_sq_l = 0.0f32;
    let mut sum_sq_r = 0.0f32;
    frames = (data.len() / channels.max(1)) as u64;
    for frame in data.chunks(channels.max(1)) {
        let l = frame.first().copied().unwrap_or(0.0);
        let r = frame.get(1).copied().unwrap_or(l);
        peak_l = peak_l.max(l.abs());
        peak_r = peak_r.max(r.abs());
        sum_sq_l += l * l;
        sum_sq_r += r * r;
    }

    // Update meters.
    let rms_l = if frames > 0 {
        (sum_sq_l / frames as f32).sqrt()
    } else {
        0.0
    };
    let (pk_r, rms_r) = if channels >= 2 {
        (
            peak_r,
            if frames > 0 {
                (sum_sq_r / frames as f32).sqrt()
            } else {
                0.0
            },
        )
    } else {
        (peak_l, rms_l)
    };
    runtime.end_meter_block(frames);

    local.prev_peak_l = smooth_peak(local.prev_peak_l, peak_l, PEAK_DECAY);
    local.prev_peak_r = smooth_peak(local.prev_peak_r, pk_r, PEAK_DECAY);

    shared
        .peak_l
        .store(f32_store(local.prev_peak_l), Ordering::Relaxed);
    shared
        .peak_r
        .store(f32_store(local.prev_peak_r), Ordering::Relaxed);
    shared.rms_l.store(f32_store(rms_l), Ordering::Relaxed);
    shared.rms_r.store(f32_store(rms_r), Ordering::Relaxed);

    // Advance transport position.
    if transport_playing && channels > 0 {
        let (next_position, _) = transport::advance_loop_position(base_sample, frames, loop_bounds);
        shared
            .position_samples
            .store(next_position, Ordering::Relaxed);
        if let Some(reset_sample) = end_loop_midi_reset {
            runtime.reset_midi_playback(reset_sample);
            local.reset_metronome_schedule(reset_sample, runtime.sample_rate);
        }
    }

    // Consumed for this block — clear AFTER render so drain_commands preview
    // events queued earlier in the same callback survive until apply_insert.
    for track in &mut runtime.tracks {
        track.midi_block_events.clear();
    }

    frames
}

/// Largest block the bridge mix reads in one callback (stack scratch bound).
const BRIDGE_MAX_FRAMES: usize = 2048;

/// Whether the legacy master-bus bridge mix fallback is enabled. Bridge DSP is
/// normally routed per-track through `external-bridge-plugin` inserts; set
/// `FUTUREBOARD_PLUGIN_BRIDGE_AUDIO=0` to disable the master fallback.
fn plugin_bridge_master_fallback_enabled() -> bool {
    use std::sync::OnceLock;
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("FUTUREBOARD_PLUGIN_BRIDGE_AUDIO")
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                !matches!(v.as_str(), "0" | "false" | "no" | "off")
            })
            .unwrap_or(false)
    })
}

/// Stage 3b: read the external plugin host's previously produced block from the
/// shared region and mix it into the master output `data`, returning the mixed
/// peak so the caller can fold it into the master meter. Then request the next
/// block (one-block latency — never blocks the audio thread).
///
/// Realtime-safe: fixed stack scratch, atomics + arithmetic only, no allocation
/// or locking. No-op unless the bridge audio path is enabled and a sink is set.
fn mix_plugin_bridge(
    data: &mut [f32],
    channels: usize,
    runtime: &RuntimeProject,
    master_vol: f32,
) -> (f32, f32) {
    if runtime.plugin_bridge_sinks.is_empty() {
        return (0.0, 0.0);
    }
    let ch = channels.max(1);
    let frames = data.len() / ch;
    if frames == 0 {
        return (0.0, 0.0);
    }
    let n = frames.min(BRIDGE_MAX_FRAMES);
    let mut scratch_l = [0.0f32; BRIDGE_MAX_FRAMES];
    let mut scratch_r = [0.0f32; BRIDGE_MAX_FRAMES];
    let mut peak_l = 0.0f32;
    let mut peak_r = 0.0f32;
    // Mix every registered track's bridged plugin output into the master.
    // (Per-track routing through each track's fader/mute/solo is a later,
    // runtime-validated step; this sums them onto the master bus for now.)
    for sink in runtime.plugin_bridge_sinks.values() {
        let got = sink.read_output(&mut scratch_l[..n], &mut scratch_r[..n], n);
        for i in 0..got {
            let l = scratch_l[i] * master_vol;
            let r = scratch_r[i] * master_vol;
            let base = i * ch;
            data[base] += l;
            if ch > 1 {
                data[base + 1] += r;
            }
            peak_l = peak_l.max(l.abs());
            peak_r = peak_r.max(r.abs());
        }
        // Request the next block (the host fills it asynchronously for next time).
        sink.request_block(frames as u32);
    }
    (peak_l, peak_r)
}

#[inline]
fn monitor_resync_target_frames(
    output_block_frames: usize,
    sample_rate: u32,
    shared_clock: bool,
) -> u64 {
    let output_block_frames = output_block_frames as u64;
    if shared_clock {
        output_block_frames
    } else {
        ((sample_rate.max(1) as u64 * 15) / 1000).max(output_block_frames.saturating_mul(2))
    }
}

#[inline]
fn monitor_resync_limit_frames(target: u64, output_block_frames: u64, shared_clock: bool) -> u64 {
    if shared_clock {
        target
    } else {
        target.saturating_add(output_block_frames)
    }
}

/// Drain the shared input ring into the preallocated monitor-input block
/// (Layers 4 + 7).
///
/// Always advances the read cursor — even when monitoring is off — so the
/// input-bus peak stays live for diagnostics and the monitor path never
/// replays stale audio when it is toggled on. The staged block is injected
/// into the monitored tracks' buffers before the normal graph pass, so plugin
/// state, PDC, sends, and master DSP all apply exactly once.
///
/// Returns true when a full block of post-gain input is staged in
/// `local.monitor_input_l/r` (underruns are padded with silence, never stale
/// samples).
///
/// Realtime-safe: atomics + arithmetic only, no allocation or locking.
fn read_monitor_input(
    frames: usize,
    shared: &Arc<SharedState>,
    local: &mut LocalAudioState,
) -> bool {
    let ring = &shared.input_ring;
    if !ring.is_active() || frames == 0 {
        return false;
    }
    // The staging buffers are preallocated by the backend; never grow them on
    // the callback. A backend that did not size them cannot stage monitoring.
    if local.monitor_input_l.len() < frames || local.monitor_input_r.len() < frames {
        return false;
    }
    let head = ring.write_head();
    if head == 0 {
        return false;
    }
    let frames64 = frames as u64;

    // Hold a small, stable monitoring latency behind the producer. Separate
    // WASAPI clients retain the existing ≈15 ms / two-block target because
    // their callback sizes and scheduling differ. ASIO input/output callbacks
    // share one device clock, so one output block is sufficient resync backlog.
    let cap = ring.capacity_frames();
    let shared_clock = shared.monitor_shared_clock.load(Ordering::Relaxed);
    let target = monitor_resync_target_frames(
        frames,
        shared.sample_rate.load(Ordering::Relaxed),
        shared_clock,
    );
    let resync_limit = monitor_resync_limit_frames(target, frames64, shared_clock);

    // Resync on gross overrun (cursor lapped) or if the cursor is ahead of the
    // producer (should not happen): jump to `target` frames behind the head.
    if local.input_read_frames > head || head.saturating_sub(local.input_read_frames) > cap {
        local.input_read_frames = head.saturating_sub(target);
        shared.monitor_ring_overruns.fetch_add(1, Ordering::Relaxed);
    }
    // Latency crept too high (input outran output): skip forward to `target`.
    if head.saturating_sub(local.input_read_frames) > resync_limit {
        local.input_read_frames = head.saturating_sub(target);
        shared.monitor_ring_overruns.fetch_add(1, Ordering::Relaxed);
    }

    let available = head.saturating_sub(local.input_read_frames);
    if available < frames64 {
        // Not enough buffered to fill the block — count an underrun. We still
        // read what's there and pad the remainder with silence (never replay
        // stale samples).
        shared
            .monitor_ring_underruns
            .fetch_add(1, Ordering::Relaxed);
        shared.output_xruns.fetch_add(1, Ordering::Relaxed);
    }

    let monitor_on = shared.monitor_enabled_any.load(Ordering::Relaxed);
    let mon_gain = f32_load(shared.monitor_gain.load(Ordering::Relaxed));

    let mut bus_peak_l = 0.0f32;
    let mut bus_peak_r = 0.0f32;
    let mut staged_peak = 0.0f32;
    let mut read = local.input_read_frames;
    let mut consumed = 0u64;

    for frame_index in 0..frames {
        let (in_l, in_r) = if read < head {
            let s = ring.read_frame(read);
            read += 1;
            consumed += 1;
            s
        } else {
            // Underrun: emit silence rather than repeating the last block.
            (0.0, 0.0)
        };
        bus_peak_l = bus_peak_l.max(in_l.abs());
        bus_peak_r = bus_peak_r.max(in_r.abs());
        let staged_l = in_l * mon_gain;
        let staged_r = in_r * mon_gain;
        local.monitor_input_l[frame_index] = staged_l;
        local.monitor_input_r[frame_index] = staged_r;
        staged_peak = staged_peak.max(staged_l.abs()).max(staged_r.abs());
    }
    local.input_read_frames = read;
    shared
        .monitor_frames_consumed
        .fetch_add(consumed, Ordering::Relaxed);

    // Smooth + publish the input-bus peak (pre-gain) and the staged monitor
    // level (post-gain, pre-fader — the graph applies the rest) for
    // diagnostics.
    local.prev_input_bus_l = smooth_peak(local.prev_input_bus_l, bus_peak_l, PEAK_DECAY);
    local.prev_input_bus_r = smooth_peak(local.prev_input_bus_r, bus_peak_r, PEAK_DECAY);
    shared
        .input_bus_peak_l
        .store(f32_store(local.prev_input_bus_l), Ordering::Relaxed);
    shared
        .input_bus_peak_r
        .store(f32_store(local.prev_input_bus_r), Ordering::Relaxed);
    shared.monitor_output_peak.store(
        f32_store(if monitor_on { staged_peak } else { 0.0 }),
        Ordering::Relaxed,
    );

    true
}

/// No live input — clear the input-bus peak so diagnostics decay to 0.
fn clear_input_bus_meter(shared: &Arc<SharedState>, local: &mut LocalAudioState) {
    shared
        .input_bus_peak_l
        .store(f32_store(0.0), Ordering::Relaxed);
    shared
        .input_bus_peak_r
        .store(f32_store(0.0), Ordering::Relaxed);
    local.prev_input_bus_l = 0.0;
    local.prev_input_bus_r = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_clock_monitor_target_is_one_output_block() {
        assert_eq!(monitor_resync_target_frames(256, 48_000, true), 256);
        assert_eq!(monitor_resync_target_frames(512, 96_000, true), 512);
    }

    #[test]
    fn independent_clock_monitor_target_preserves_wasapi_backlog() {
        assert_eq!(monitor_resync_target_frames(256, 48_000, false), 720);
        assert_eq!(monitor_resync_target_frames(512, 48_000, false), 1024);
    }

    #[test]
    fn shared_clock_resyncs_as_soon_as_backlog_exceeds_one_block() {
        assert_eq!(monitor_resync_limit_frames(256, 256, true), 256);
        assert_eq!(monitor_resync_limit_frames(720, 256, false), 976);
    }

    #[test]
    fn metronome_click_waits_for_compensation_delay() {
        let mut local = LocalAudioState::new(48_000.0);
        local.set_metronome_enabled(true, 0, 48_000);

        for sample in 0..512 {
            let click = local.metronome_sample(sample, sample, 48_000, true, 512, 512);
            assert_eq!(
                click, 0.0,
                "click leaked before compensated sample {sample}"
            );
            assert_eq!(local.metronome_click_remaining, 0);
        }

        let first = local.metronome_sample(512, 512, 48_000, true, 512, 512);
        assert_eq!(
            first, 0.0,
            "first click oscillator sample starts at phase zero"
        );
        assert!(
            local.metronome_click_remaining > 0,
            "click should arm exactly at raw click sample plus compensation delay"
        );
    }

    #[test]
    fn metronome_click_without_compensation_arms_on_raw_beat() {
        let mut local = LocalAudioState::new(48_000.0);
        local.set_metronome_enabled(true, 0, 48_000);

        let first = local.metronome_sample(0, 0, 48_000, true, 0, 0);
        assert_eq!(
            first, 0.0,
            "first click oscillator sample starts at phase zero"
        );
        assert!(
            local.metronome_click_remaining > 0,
            "uncompensated metronome should arm at the raw beat sample"
        );
    }
}
