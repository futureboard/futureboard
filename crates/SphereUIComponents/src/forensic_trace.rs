//! Forensic MIDI/plugin trace instrumentation.
//!
//! Enable the full hop chain with `FUTUREBOARD_FORENSIC_TRACE=1`, or enable
//! individual areas via `FUTUREBOARD_MIDI_DEBUG`, `FUTUREBOARD_PLUGIN_DEBUG`, etc.

use crate::components::timeline::timeline_state::{
    midi_debug_enabled, ClipType, PluginRuntimeState, TimelineState, TrackType,
};

/// Master switch for the forensic hop chain (all areas).
pub fn forensic_trace_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_FORENSIC_TRACE").is_some())
}

pub fn plugin_trace_enabled() -> bool {
    forensic_trace_enabled()
        || std::env::var_os("FUTUREBOARD_PLUGIN_DEBUG").is_some()
        || std::env::var_os("FUTUREBOARD_PLUGIN_INSERT_DEBUG").is_some()
}

pub fn midi_model_trace_enabled() -> bool {
    forensic_trace_enabled() || midi_debug_enabled()
}

pub fn shell_layout_trace_enabled() -> bool {
    forensic_trace_enabled() || std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").is_some()
}

/// Plugin editor safe mode (`FUTUREBOARD_PLUGIN_EDITOR_SAFE=1`): disables
/// experimental focus hacks and verbose per-message logging in the native
/// editor shell. Mirrors the same flag in the plugin host process.
pub fn plugin_editor_safe_mode() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_PLUGIN_EDITOR_SAFE").is_some())
}

/// Coarse log rate limiter: allows at most `max_per_sec` events per second.
/// Used to keep per-message diagnostics (hit-test, mouse-move traces) bounded.
pub struct LogRateLimiter {
    window_start_ms: std::sync::atomic::AtomicU64,
    count: std::sync::atomic::AtomicU32,
    max_per_sec: u32,
}

impl LogRateLimiter {
    pub const fn new(max_per_sec: u32) -> Self {
        Self {
            window_start_ms: std::sync::atomic::AtomicU64::new(0),
            count: std::sync::atomic::AtomicU32::new(0),
            max_per_sec,
        }
    }

    pub fn allow(&self) -> bool {
        use std::sync::atomic::Ordering;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let start = self.window_start_ms.load(Ordering::Relaxed);
        if now.saturating_sub(start) >= 1000 {
            self.window_start_ms.store(now, Ordering::Relaxed);
            self.count.store(1, Ordering::Relaxed);
            return true;
        }
        self.count.fetch_add(1, Ordering::Relaxed) < self.max_per_sec
    }
}

pub fn preview_perf_trace_enabled() -> bool {
    forensic_trace_enabled() || std::env::var_os("FUTUREBOARD_MIDI_VERBOSE").is_some()
}

/// Stable editor window id: `track_id::insert_id`.
pub fn editor_window_id(track_id: &str, insert_id: &str) -> String {
    format!("{track_id}::{insert_id}")
}

/// Log the required per-instance trace label (Phase 0).
pub fn log_trace_plugin(track_id: &str, insert_id: &str) {
    if !plugin_trace_enabled() {
        return;
    }
    let editor_window_id = editor_window_id(track_id, insert_id);
    eprintln!(
        "[trace-plugin] track={track_id} insert={insert_id} \
         plugin_instance_id={insert_id} editor_window_id={editor_window_id}"
    );
}

fn track_type_label(ty: TrackType) -> &'static str {
    match ty {
        TrackType::Audio => "audio",
        TrackType::Midi => "midi",
        TrackType::Instrument => "instrument",
        TrackType::Bus => "bus",
        TrackType::Return => "return",
        TrackType::Group => "group",
        TrackType::Master => "master",
        TrackType::Video => "video",
    }
}

/// Hop 1: dump the UI/project model before engine sync (e.g. on Play).
pub fn dump_midi_model(state: &TimelineState) {
    if !midi_model_trace_enabled() {
        return;
    }
    eprintln!("[midi-model-dump] tracks={}", state.tracks.len());
    for track in &state.tracks {
        let ty = track_type_label(track.track_type);
        let midi_clip_count = track
            .clips
            .iter()
            .filter(|c| matches!(c.clip_type, ClipType::Midi { .. }))
            .count();
        eprintln!(
            "[midi-model-dump] track={} type={ty} clips={midi_clip_count}",
            track.id
        );
        for clip in &track.clips {
            let ClipType::Midi { notes, .. } = &clip.clip_type else {
                continue;
            };
            eprintln!("[midi-model-dump] clip={} notes={}", clip.id, notes.len());
            for note in notes.iter().filter(|n| !n.muted) {
                let end = note.start + note.duration;
                eprintln!(
                    "[midi-model-dump] note pitch={} start={:.3} end={:.3}",
                    note.pitch, note.start, end
                );
            }
        }
    }
}

/// Main-app plugin insert registry audit.
pub fn log_plugin_main_registry(state: &TimelineState) {
    if !plugin_trace_enabled() {
        return;
    }
    let mut inserts: Vec<(String, String, String, String)> = Vec::new();
    for track in &state.tracks {
        for slot in &track.inserts {
            let backend = slot.runtime_backend.label().to_string();
            let state_tag = match &slot.runtime_state {
                PluginRuntimeState::NotLoaded => "not_loaded",
                PluginRuntimeState::Loading => "loading",
                PluginRuntimeState::Loaded => "loaded",
                PluginRuntimeState::Active => "active",
                PluginRuntimeState::Ready => "ready",
                PluginRuntimeState::EditorOpening => "editor_opening",
                PluginRuntimeState::EditorOpen => "editor_open",
                PluginRuntimeState::EditorClosed => "editor_closed",
                PluginRuntimeState::Bypassed => "bypassed",
                PluginRuntimeState::Missing(_) => "missing",
                PluginRuntimeState::Failed(_) => "failed",
                PluginRuntimeState::Crashed => "crashed",
                PluginRuntimeState::Unloaded => "unloaded",
            };
            inserts.push((
                track.id.clone(),
                slot.id.clone(),
                backend,
                state_tag.to_string(),
            ));
        }
    }
    eprintln!("[plugin-main-registry] inserts={}", inserts.len());
    for (track_id, instance, backend, state_tag) in &inserts {
        eprintln!(
            "[plugin-main-registry] instance={instance} track={track_id} backend={backend} state={state_tag}"
        );
    }
}
