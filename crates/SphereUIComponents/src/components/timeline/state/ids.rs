use super::*;

/// Raise a monotonic id counter so the next mint is strictly greater than `seen`.
fn observe_counter(counter: &std::sync::atomic::AtomicU64, seen: u64) {
    use std::sync::atomic::Ordering;
    if seen == 0 {
        return;
    }
    let mut current = counter.load(Ordering::Relaxed);
    while current <= seen {
        match counter.compare_exchange_weak(current, seen + 1, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

fn mint_counter(counter: &std::sync::atomic::AtomicU64) -> u64 {
    use std::sync::atomic::Ordering;
    counter.fetch_add(1, Ordering::Relaxed)
}

fn counter_midi_note() -> &'static std::sync::atomic::AtomicU64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    &COUNTER
}

fn counter_controller_point() -> &'static std::sync::atomic::AtomicU64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    &COUNTER
}

fn counter_pitch_point() -> &'static std::sync::atomic::AtomicU64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    &COUNTER
}

fn counter_automation_point() -> &'static std::sync::atomic::AtomicU64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    &COUNTER
}

/// Monotonic source of pitch-curve point identities. Persisted from project
/// format v38 onward so a pitch point keeps its identity across save/load,
/// undo, and note move/copy/split.
pub fn next_pitch_point_id() -> u64 {
    mint_counter(counter_pitch_point())
}

/// Ensure subsequent pitch-point mints do not collide with a loaded id.
pub fn observe_pitch_point_id(id: u64) {
    observe_counter(counter_pitch_point(), id);
}

/// Monotonic source of automation-point identities. Persisted from project
/// format v26 onward; older files mint fresh ids on load.
pub fn next_automation_point_id() -> u64 {
    mint_counter(counter_automation_point())
}

/// Ensure subsequent automation-point mints do not collide with a loaded id.
pub fn observe_automation_point_id(id: u64) {
    observe_counter(counter_automation_point(), id);
}

/// Monotonic source of MIDI note identities. Persisted from project format v26
/// onward so selection, undo, and clipboard targets survive save/load. Copies
/// and duplicates must call [`next_midi_note_id`] for a new identity; moves
/// keep the existing id.
pub fn next_midi_note_id() -> u64 {
    mint_counter(counter_midi_note())
}

/// Ensure subsequent note mints do not collide with a loaded id.
pub fn observe_midi_note_id(id: u64) {
    observe_counter(counter_midi_note(), id);
}

/// Monotonic source of controller-point identities. Persisted from project
/// format v26 onward (same lifecycle as note ids).
pub fn next_controller_point_id() -> u64 {
    mint_counter(counter_controller_point())
}

/// Ensure subsequent controller-point mints do not collide with a loaded id.
pub fn observe_controller_point_id(id: u64) {
    observe_counter(counter_controller_point(), id);
}

/// Monotonic source of stable tempo-point identities. Persisted in project
/// files so edits target a point by id even after the user drags it to a new
/// beat position.
pub fn next_tempo_point_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("tempo-{ts:x}-{seq:x}")
}

pub fn next_time_signature_point_id() -> TimeSignaturePointId {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("ts-{ts:x}-{seq:x}")
}

pub fn next_timeline_marker_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("marker-{ts:x}-{seq:x}")
}

pub fn next_timeline_region_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("region-{ts:x}-{seq:x}")
}

/// Stable project identity for chord, lyric, and section events. Timestamp plus
/// a process-local sequence avoids collisions with loaded projects without an
/// observe pass, while preserving IDs across save/load and undo/redo.
pub fn next_song_text_event_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("song-text-{ts:x}-{seq:x}")
}
