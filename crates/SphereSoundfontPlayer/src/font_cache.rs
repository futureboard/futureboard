//! Process-wide reuse of parsed SoundFonts.
//!
//! A parsed [`SoundFont`] is immutable, shareable, and expensive: a General
//! MIDI bank costs tens of megabytes of file read plus a full RIFF walk. A
//! [`Synthesizer`](rustysynth::Synthesizer), by contrast, must be rebuilt every
//! time polyphony, reverb/chorus, or the sample rate changes, and the runtime
//! audio graph clones its players whenever the engine swaps graphs. Without a
//! cache each of those rebuilds re-reads the file on the control thread.
//!
//! Entries are held weakly, so the cache costs nothing once the last player
//! using a font is dropped, and a file that changed on disk is re-read rather
//! than served stale.
//!
//! Control/offline only: [`load`] performs filesystem I/O and must never be
//! called from an audio callback.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::SystemTime;

use rustysynth::SoundFont;

use crate::SoundfontPlayerError;

/// Identity of a cached font. Length and modification time are checked so an
/// edited or replaced file is re-read instead of served from the cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontKey {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

static CACHE: OnceLock<Mutex<HashMap<FontKey, Weak<SoundFont>>>> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static Mutex<HashMap<FontKey, Weak<SoundFont>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn font_key(path: &Path) -> Result<FontKey, SoundfontPlayerError> {
    let metadata = std::fs::metadata(path)?;
    Ok(FontKey {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Returns the parsed font for `path`, reading and parsing it only if no live
/// copy of that exact file is already loaded.
pub fn load(path: &Path) -> Result<Arc<SoundFont>, SoundfontPlayerError> {
    let key = font_key(path)?;
    if let Some(font) = cache()
        .lock()
        .ok()
        .and_then(|entries| entries.get(&key).and_then(Weak::upgrade))
    {
        HITS.fetch_add(1, Ordering::Relaxed);
        return Ok(font);
    }

    let mut file = File::open(path)?;
    let font = Arc::new(SoundFont::new(&mut file)?);
    MISSES.fetch_add(1, Ordering::Relaxed);

    if let Ok(mut entries) = cache().lock() {
        entries.retain(|_, weak| weak.strong_count() > 0);
        entries.insert(key, Arc::downgrade(&font));
    }
    Ok(font)
}

/// Cache counters for diagnostics: `(live_entries, hits, misses)`.
pub fn stats() -> (usize, u64, u64) {
    let live = cache()
        .lock()
        .map(|entries| {
            entries
                .values()
                .filter(|weak| weak.strong_count() > 0)
                .count()
        })
        .unwrap_or(0);
    (
        live,
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reports_io_error() {
        let error = load(Path::new("/definitely/not/a/soundfont.sf2")).unwrap_err();
        assert!(matches!(error, SoundfontPlayerError::Io(_)));
    }
}
