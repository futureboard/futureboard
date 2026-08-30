//! On-demand high-resolution waveform peaks for deep zoom.
//!
//! The peak file that ships with every imported asset stops at
//! [`waveform_cache::PEAK_FINE_SPP`] — one min/max pair per 256 source frames.
//! At 48 kHz that is 187 peaks a second, which is more than enough until the
//! arrangement is zoomed past roughly 190 px/s. Beyond that the drawing runs
//! out of data before it runs out of pixels: every pixel inside a 256-frame
//! bucket reads the same min/max, so the waveform turns into a staircase of
//! flat-topped blocks exactly when the user has zoomed in to look at detail.
//!
//! Generating the finer ladder up front is not the answer. Halving
//! samples-per-peak doubles the peak count, and the finest level dominates the
//! total, so carrying peaks down to 8 frames would cost about 32× the peak file
//! for detail almost no session ever looks at.
//!
//! So it is built on demand, for the window on screen only:
//!
//! 1. the render notes what resolution it wanted and for which source frames
//!    ([`note_needed`]) — a bounded channel send, never any I/O;
//! 2. one long-lived worker decodes just those frames and installs them into the
//!    existing chunk store under their own samples-per-peak key;
//! 3. installing bumps the file entry's revision, the geometry cache drops its
//!    stale bars, and the next frame draws at full detail.
//!
//! Nothing here blocks the render. Until a chunk lands the drawing keeps using
//! the coarsest data it has, which is what it did before — the picture only ever
//! gets better, never later.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

use super::waveform_cache::{self, WaveformPeak, CHUNK_PEAKS, PEAK_FINE_SPP};
use DirectAudio::{open_clip_audio_source, read_frame_stereo};

/// Samples-per-peak levels below the shipped ladder, coarsest first.
///
/// Stops at 8. A pixel that covers fewer than 8 frames means the zoom is past
/// 6 kpx/s, where the useful drawing is the sample points themselves rather
/// than a min/max envelope — that is a different view, not a finer peak.
pub const DETAIL_LOD_LEVELS: [usize; 5] = [128, 64, 32, 16, 8];

/// How many detail chunks stay resident. Each is `CHUNK_PEAKS` pairs — 32 KiB —
/// so this is a ~16 MiB ceiling for the whole session, and it is an LRU: the
/// windows the user is actually looking at stay, the ones they scrolled past
/// are dropped and rebuilt if they come back.
const MAX_RESIDENT_CHUNKS: usize = 512;

/// Bounded so a render that asks faster than the worker can decode drops
/// requests instead of growing a queue. A dropped request costs one frame of
/// coarser drawing and is re-sent on the next frame anyway.
const REQUEST_QUEUE_DEPTH: usize = 64;

/// Identity of one detail chunk. Keyed by the *cache* key (the asset id the
/// peak store is indexed by), not by the file path — the same asset can be
/// re-keyed when a project is saved, and two clips of one asset must share the
/// chunk rather than each decode it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChunkKey {
    cache_key: String,
    samples_per_peak: usize,
    chunk_index: u32,
}

/// A chunk to build, plus the file to read it from. The path is carried beside
/// the key rather than inside it so re-keying an asset does not orphan work
/// already done for it.
#[derive(Debug, Clone)]
struct DetailRequest {
    key: ChunkKey,
    path: String,
}

struct Registry {
    /// Requested or resident — either way, do not ask again.
    known: HashSet<ChunkKey>,
    /// Resident chunks in install order, for eviction.
    resident: VecDeque<ChunkKey>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            known: HashSet::new(),
            resident: VecDeque::new(),
        })
    })
}

fn sender() -> &'static SyncSender<DetailRequest> {
    static SENDER: OnceLock<SyncSender<DetailRequest>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = sync_channel::<DetailRequest>(REQUEST_QUEUE_DEPTH);
        std::thread::Builder::new()
            .name("waveform-detail".to_string())
            .spawn(move || {
                // One source stays open across consecutive chunks of the same
                // file, which is the common case while zooming: reopening per
                // chunk would re-read the header and, for a streaming source,
                // restart its decoder.
                let mut open: Option<(String, DirectAudio::ClipAudioSource)> = None;
                while let Ok(request) = rx.recv() {
                    if open
                        .as_ref()
                        .map(|(p, _)| p != &request.path)
                        .unwrap_or(true)
                    {
                        open = open_clip_audio_source(&request.path)
                            .ok()
                            .map(|source| (request.path.clone(), source));
                    }
                    let Some((_, source)) = open.as_ref() else {
                        // Unreadable: forget the request so a later retry (after
                        // the asset is relinked) can happen, but do not spin.
                        forget(&request.key);
                        continue;
                    };
                    match build_chunk(
                        source,
                        request.key.samples_per_peak,
                        request.key.chunk_index,
                    ) {
                        Some(peaks) => install(request.key, peaks),
                        None => forget(&request.key),
                    }
                }
            })
            .ok();
        tx
    })
}

/// Peaks for one chunk, or `None` when the window is past the end of the file.
fn build_chunk(
    source: &DirectAudio::ClipAudioSource,
    samples_per_peak: usize,
    chunk_index: u32,
) -> Option<Vec<WaveformPeak>> {
    let total_frames = source.frames();
    let spp = samples_per_peak.max(1);
    let first_frame = chunk_index as usize * CHUNK_PEAKS * spp;
    if first_frame >= total_frames {
        return None;
    }
    let mut peaks = Vec::with_capacity(CHUNK_PEAKS);
    for peak in 0..CHUNK_PEAKS {
        let start = first_frame + peak * spp;
        if start >= total_frames {
            break;
        }
        let end = (start + spp).min(total_frames);
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for frame in start..end {
            let (l, r) = read_frame_stereo(source, frame);
            // The shipped ladder is built from the mono mix (the mean of the
            // channels), so the detail level has to be too — otherwise the
            // envelope would visibly change height at the zoom where the two
            // meet.
            let mono = ((l + r) * 0.5).clamp(-1.0, 1.0);
            if mono < min {
                min = mono;
            }
            if mono > max {
                max = mono;
            }
        }
        if min > max {
            break;
        }
        peaks.push(WaveformPeak { min, max });
    }
    (!peaks.is_empty()).then_some(peaks)
}

fn install(key: ChunkKey, peaks: Vec<WaveformPeak>) {
    waveform_cache::install_detail_chunk(
        &key.cache_key,
        key.samples_per_peak as u32,
        key.chunk_index,
        Arc::new(peaks),
    );
    let evicted = {
        let Ok(mut reg) = registry().lock() else {
            return;
        };
        reg.resident.push_back(key);
        let mut evicted = Vec::new();
        while reg.resident.len() > MAX_RESIDENT_CHUNKS {
            if let Some(old) = reg.resident.pop_front() {
                reg.known.remove(&old);
                evicted.push(old);
            }
        }
        evicted
    };
    for old in evicted {
        waveform_cache::remove_chunk(&old.cache_key, old.samples_per_peak as u32, old.chunk_index);
    }
}

fn forget(key: &ChunkKey) {
    if let Ok(mut reg) = registry().lock() {
        reg.known.remove(key);
    }
}

/// The finest samples-per-peak worth building for `pixels_per_second`.
///
/// `None` when the shipped ladder already resolves the zoom — which is the
/// common case, and the cheap early exit for every clip on screen.
pub fn detail_level_for_zoom(pixels_per_second: f32, sample_rate: u32) -> Option<usize> {
    if sample_rate == 0 {
        return None;
    }
    // Frames behind one pixel. The shipped ladder stops being enough exactly
    // when one 256-frame peak no longer covers a whole pixel column — below
    // that, neighbouring pixels read the same min/max and the envelope becomes
    // a staircase. Above it, the coarse ladder still has a peak per pixel and
    // decoding finer data would buy nothing that can be drawn.
    let frames_per_pixel = (sample_rate as f32 / pixels_per_second.max(1.0)).max(1.0);
    if frames_per_pixel >= PEAK_FINE_SPP as f32 {
        return None;
    }
    // Once engaged, aim for about two peaks per pixel so a column's min/max is
    // a real envelope rather than one sample pair.
    let target = (frames_per_pixel / 2.0).max(1.0) as usize;
    // Coarsest detail level that still resolves the zoom, so a mild zoom past
    // the ladder does not decode 32× more than it can show.
    DETAIL_LOD_LEVELS
        .iter()
        .copied()
        .find(|&spp| spp <= target)
        .or_else(|| DETAIL_LOD_LEVELS.last().copied())
}

/// Ask for the detail covering source frames `[first_frame, last_frame)`.
///
/// Called from the render path, so it does no I/O and never blocks: it takes a
/// lock long enough to test a set, and pushes onto a bounded channel that drops
/// when the worker is behind.
pub fn note_needed(
    cache_key: &str,
    source_path: &str,
    samples_per_peak: usize,
    first_frame: u64,
    last_frame: u64,
    total_frames: u64,
) {
    if source_path.trim().is_empty() {
        return;
    }
    let spp = samples_per_peak.max(1);
    if last_frame <= first_frame || total_frames == 0 {
        return;
    }
    let frames_per_chunk = (CHUNK_PEAKS * spp) as u64;
    let first_chunk = first_frame / frames_per_chunk;
    let last_chunk = (last_frame.min(total_frames).saturating_sub(1)) / frames_per_chunk;
    // A visible window that spans more chunks than this is not a deep zoom, it
    // is a mis-computed range; asking for all of it would evict everything the
    // user can actually see.
    const MAX_CHUNKS_PER_REQUEST: u64 = 8;
    let last_chunk = last_chunk.min(first_chunk + MAX_CHUNKS_PER_REQUEST - 1);

    for chunk_index in first_chunk..=last_chunk {
        let key = ChunkKey {
            cache_key: cache_key.to_string(),
            samples_per_peak: spp,
            chunk_index: chunk_index as u32,
        };
        {
            let Ok(mut reg) = registry().lock() else {
                return;
            };
            if !reg.known.insert(key.clone()) {
                continue;
            }
        }
        let request = DetailRequest {
            key: key.clone(),
            path: source_path.to_string(),
        };
        if sender().try_send(request).is_err() {
            // Queue full — drop it and let the next frame ask again.
            forget(&key);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped ladder already resolves ordinary zoom levels; asking for
    /// detail there would decode the whole session for nothing.
    #[test]
    fn ordinary_zoom_needs_no_detail() {
        assert_eq!(detail_level_for_zoom(20.0, 48_000), None);
        assert_eq!(detail_level_for_zoom(100.0, 48_000), None);
        // 48000 / 256 = 187.5 px/s is exactly where one shipped peak still
        // covers a pixel; detail starts past it, not at it.
        assert_eq!(detail_level_for_zoom(187.0, 48_000), None);
        assert!(detail_level_for_zoom(190.0, 48_000).is_some());
    }

    /// Past the ladder, the level tracks the zoom rather than jumping straight
    /// to the finest: a mild overshoot must not decode 16× what it can show.
    #[test]
    fn detail_tracks_the_zoom_depth() {
        let mild = detail_level_for_zoom(400.0, 48_000).expect("past the ladder");
        let deep = detail_level_for_zoom(4000.0, 48_000).expect("far past the ladder");
        assert!(
            mild < PEAK_FINE_SPP,
            "mild {mild} must beat the shipped LOD"
        );
        assert!(
            deep < mild,
            "deeper zoom needs finer peaks: {deep} should be under {mild}"
        );
    }

    /// However far it is zoomed, the level stays on the ladder — an off-ladder
    /// value would never match a stored chunk.
    #[test]
    fn every_level_is_on_the_ladder() {
        for pps in [200.0_f32, 500.0, 1_000.0, 5_000.0, 50_000.0, 500_000.0] {
            let spp = detail_level_for_zoom(pps, 48_000).expect("past the ladder");
            assert!(
                DETAIL_LOD_LEVELS.contains(&spp),
                "{pps} px/s produced off-ladder spp {spp}"
            );
        }
    }

    /// A sample rate of zero is a file whose header has not been read yet.
    #[test]
    fn an_unknown_sample_rate_asks_for_nothing() {
        assert_eq!(detail_level_for_zoom(10_000.0, 0), None);
    }
}
