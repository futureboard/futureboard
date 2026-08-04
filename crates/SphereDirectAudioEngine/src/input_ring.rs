//! Lock-free stereo input bridge (Layer 4 of the audio-input pipeline).
//!
//! A single-producer / single-consumer ring buffer that carries live input
//! samples from the capture stream's realtime callback (producer) to the
//! output render callback (consumer).
//!
//! # Why a ring
//!
//! The input and output devices run on **separate** realtime threads with
//! independent block sizes (and, on shared-mode WASAPI, independent wake-ups).
//! A ring buffer decouples the two: the input callback appends frames as they
//! arrive; the output callback drains the freshest frames each block. Neither
//! side allocates or locks — backing storage is preallocated once and indices
//! are plain atomics.
//!
//! # Realtime safety
//!
//! * Producer (`write_stereo`) and consumer (`read_frame` / `write_head`) touch
//!   only atomics — no allocation, no locking, no syscalls.
//! * The producer publishes its write index with `Release`; the consumer reads
//!   it with `Acquire`, so any sample stored before the index bump is visible.
//! * Exactly one producer and one consumer are assumed (SPSC). Two readers or
//!   two writers would race the cursor — but the engine only ever has one of
//!   each (one live-input stream, one output stream).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Ring capacity in frames. Power of two so index wrap is a mask.
/// 16384 frames ≈ 341 ms at 48 kHz — far larger than any sane input/output
/// block pair, so the consumer never overruns under normal scheduling.
const CAPACITY_FRAMES: usize = 1 << 14;
const MASK: usize = CAPACITY_FRAMES - 1;

/// Stereo, lock-free input bridge. Stored inside `SharedState` (already behind
/// an `Arc`), so both callbacks reach it through their `Arc<SharedState>`.
pub struct InputRing {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    /// Total frames written since process start (monotonic). The low bits index
    /// the backing arrays via `MASK`.
    write_frames: AtomicU64,
    /// `true` while a live-input stream is feeding the ring.
    active: AtomicBool,
    /// Source channel count / sample rate of the feeding stream (diagnostics).
    channels: AtomicU32,
    sample_rate: AtomicU32,
}

impl std::fmt::Debug for InputRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputRing")
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("write_frames", &self.write_frames.load(Ordering::Relaxed))
            .field("channels", &self.channels.load(Ordering::Relaxed))
            .field("sample_rate", &self.sample_rate.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for InputRing {
    fn default() -> Self {
        let make = || {
            (0..CAPACITY_FRAMES)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice()
        };
        Self {
            left: make(),
            right: make(),
            write_frames: AtomicU64::new(0),
            active: AtomicBool::new(false),
            channels: AtomicU32::new(0),
            sample_rate: AtomicU32::new(0),
        }
    }
}

impl InputRing {
    #[inline]
    pub fn capacity_frames(&self) -> u64 {
        CAPACITY_FRAMES as u64
    }

    /// Producer: append one stereo frame. Realtime-safe (atomics only).
    #[inline]
    pub fn write_stereo(&self, l: f32, r: f32) {
        let w = self.write_frames.load(Ordering::Relaxed);
        let idx = (w as usize) & MASK;
        self.left[idx].store(l.to_bits(), Ordering::Relaxed);
        self.right[idx].store(r.to_bits(), Ordering::Relaxed);
        // Publish the slot *after* the samples are stored.
        self.write_frames
            .store(w.wrapping_add(1), Ordering::Release);
    }

    /// Consumer: total frames written so far (monotonic). Read with `Acquire`
    /// so samples stored before the matching `write_stereo` index bump are
    /// visible.
    #[inline]
    pub fn write_head(&self) -> u64 {
        self.write_frames.load(Ordering::Acquire)
    }

    /// Consumer: read the stereo frame at absolute index `frame`. The caller is
    /// responsible for keeping `frame < write_head()` and within one capacity
    /// window of the head (see `read_block_into`).
    #[inline]
    pub fn read_frame(&self, frame: u64) -> (f32, f32) {
        let idx = (frame as usize) & MASK;
        (
            f32::from_bits(self.left[idx].load(Ordering::Relaxed)),
            f32::from_bits(self.right[idx].load(Ordering::Relaxed)),
        )
    }

    /// Mark the ring as fed (or not) by a live-input stream, recording the
    /// source format for diagnostics.
    pub fn set_active(&self, active: bool, channels: u32, sample_rate: u32) {
        self.channels.store(channels, Ordering::Relaxed);
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
        self.active.store(active, Ordering::Relaxed);
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn channels(&self) -> u32 {
        self.channels.load(Ordering::Relaxed)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }
}

// ── Recording waveform preview ring (Part 1) ───────────────────────────────────

/// One waveform preview bin: min/max/rms of the samples in one preview window.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaveformPeak {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

/// Max preview bins retained. 1<<16 ≈ 65 k bins ≈ 7 min at 150 bins/s — plenty
/// for one take; older bins wrap (the UI drains far faster than that).
const PREVIEW_CAPACITY: usize = 1 << 16;
const PREVIEW_MASK: usize = PREVIEW_CAPACITY - 1;

/// Max simultaneously-armed tracks whose live recording waveform can be
/// previewed at once. Chosen generously for typical multitrack sessions;
/// tracks armed beyond this cap still record to disk correctly (the writer
/// path is unrelated), they just don't get a live waveform preview.
pub(crate) const MAX_RECORDING_PREVIEW_TRACKS: usize = 32;

/// Lock-free ring of finalized preview peaks. The recording input callback
/// (producer) pushes one bin every `samples_per_bin` frames; the control thread
/// (consumer) drains completed bins for the UI. SPSC, atomics only.
pub struct PreviewPeakRing {
    min: Box<[AtomicU32]>,
    max: Box<[AtomicU32]>,
    rms: Box<[AtomicU32]>,
    write_bins: AtomicU64,
}

impl std::fmt::Debug for PreviewPeakRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewPeakRing")
            .field("write_bins", &self.write_bins.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for PreviewPeakRing {
    fn default() -> Self {
        let make = || {
            (0..PREVIEW_CAPACITY)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice()
        };
        Self {
            min: make(),
            max: make(),
            rms: make(),
            write_bins: AtomicU64::new(0),
        }
    }
}

impl PreviewPeakRing {
    /// Producer: append one finalized preview bin. Realtime-safe.
    #[inline]
    pub fn push(&self, peak: WaveformPeak) {
        let w = self.write_bins.load(Ordering::Relaxed);
        let idx = (w as usize) & PREVIEW_MASK;
        self.min[idx].store(peak.min.to_bits(), Ordering::Relaxed);
        self.max[idx].store(peak.max.to_bits(), Ordering::Relaxed);
        self.rms[idx].store(peak.rms.to_bits(), Ordering::Relaxed);
        self.write_bins.store(w.wrapping_add(1), Ordering::Release);
    }

    /// Consumer: total bins written since the last [`reset`](Self::reset).
    #[inline]
    pub fn head(&self) -> u64 {
        self.write_bins.load(Ordering::Acquire)
    }

    /// Consumer: read the bin at absolute index `i` (`i < head()`).
    #[inline]
    pub fn read(&self, i: u64) -> WaveformPeak {
        let idx = (i as usize) & PREVIEW_MASK;
        WaveformPeak {
            min: f32::from_bits(self.min[idx].load(Ordering::Relaxed)),
            max: f32::from_bits(self.max[idx].load(Ordering::Relaxed)),
            rms: f32::from_bits(self.rms[idx].load(Ordering::Relaxed)),
        }
    }

    /// Reset the bin counter — called on the control thread before a take so
    /// the consumer's indices line up with the new recording.
    pub fn reset(&self) {
        self.write_bins.store(0, Ordering::Release);
    }

    /// Retained bin window — drains older than this are clamped.
    pub fn default_capacity() -> u64 {
        PREVIEW_CAPACITY as u64
    }
}
