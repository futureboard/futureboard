//! The Audio Jam bridge inside the engine: lock-free rings between the network
//! and the realtime callback.
//!
//! ```txt
//! jam receive thread ──▶ JamInputSlot ──▶ audio callback ──▶ track block
//! audio callback ─────▶ JamPublishSlot ──▶ jam publish thread ──▶ network
//! ```
//!
//! # Why the engine owns this and not the jam client
//!
//! Because the engine owns the device. A jam that opened its own WASAPI /
//! CoreAudio / ALSA client would contend with the DAW for the same hardware and
//! run on a second, unrelated clock. Instead the jam is a producer and a
//! consumer of buffers the existing engine already fills and drains:
//!
//! ```txt
//! hardware ─▶ Futureboard Audio Engine ─▶ jam publish
//! jam receive ─▶ Futureboard Audio Engine ─▶ track / master
//! ```
//!
//! # Realtime contract
//!
//! Every slot's backing storage is allocated once, at construction. The audio
//! callback only loads and stores atomics: no allocation, no locking, no
//! syscall, no network. A slot with nothing in it reads as silence rather than
//! waiting — a jam must never be able to stall the audio device.
//!
//! # Timing
//!
//! A slot carries a **capture base** rather than a per-frame timestamp: the
//! capture position that corresponds to absolute ring frame zero. While a
//! stream is continuous, one value describes every frame in it
//! (`position = base + frame`), so the audio callback reads a single atomic
//! instead of a pair that could tear against each other. A gap or a publisher
//! reconnect re-bases it, which is exactly where the timeline really does move.
//!
//! The unit is **this ring's own sample rate**, not the jam's 48 kHz session
//! tick. The bridge converts session ticks into ring frames before writing, so
//! what the engine reads out is already in project samples and a recorder never
//! has to know what rate the publisher ran at. What survives that conversion is
//! the publisher's capture instant — never the moment the packet arrived.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;

/// Ring capacity per slot, in frames. Power of two so the wrap is a mask.
/// 16384 frames is 341 ms at 48 kHz — far more than any jitter buffer plus
/// callback pair, so the consumer never overruns under normal scheduling.
const CAPACITY_FRAMES: usize = 1 << 14;
const MASK: usize = CAPACITY_FRAMES - 1;

/// How many remote streams can feed tracks at once.
///
/// Fixed rather than grown on demand: the audio callback indexes this table and
/// must never wait on a reallocation. Thirty-two is well past the server's own
/// participant ceiling for one jam.
pub const MAX_JAM_INPUT_SLOTS: usize = 32;

/// How many local sources can be published at once.
pub const MAX_JAM_PUBLISH_SLOTS: usize = 8;

/// The prefix that marks an Audio Connections port as a jam stream rather than
/// a hardware one.
///
/// Routing a remote performer to a track reuses the whole existing Audio
/// Connections layer — logical bus, stable id, non-destructive device loss —
/// instead of growing a parallel routing model beside it. What makes a port a
/// jam port is this prefix on its device id, and it is the only place the two
/// worlds meet.
pub const JAM_DEVICE_PREFIX: &str = "jam:";

/// Build the Audio Connections device id for one remote stream.
pub fn jam_device_id(stream_id: &str) -> String {
    format!("{JAM_DEVICE_PREFIX}{stream_id}")
}

/// Publish-slot key for the master bus.
///
/// Publish sources are keyed by what they tap rather than by an index, so the
/// realtime callback and the jam bridge agree on which ring is which without
/// sharing a counter.
pub const PUBLISH_KEY_MASTER: &str = "master";

/// Publish-slot key for one track or bus.
pub fn publish_key_track(track_id: &str) -> String {
    format!("track:{track_id}")
}

/// Publish-slot key for a hardware input, named by its Audio Connection.
pub fn publish_key_hardware_input(connection_id: &str) -> String {
    format!("input:{connection_id}")
}

/// The stream id inside a jam device id, or `None` for a hardware device.
pub fn jam_stream_id(device_id: &str) -> Option<&str> {
    device_id.strip_prefix(JAM_DEVICE_PREFIX)
}

/// Whether a device id names a jam stream.
pub fn is_jam_device(device_id: &str) -> bool {
    device_id.starts_with(JAM_DEVICE_PREFIX)
}

/// How a slot's two channels reach a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JamChannelMode {
    /// Channel 0 to the left, channel 1 to the right.
    Stereo,
    /// Channel 1 to the left, channel 0 to the right.
    ///
    /// Reachable because Audio Connections lets any stream channel be bound to
    /// any logical channel. Without this variant a deliberately swapped route
    /// would play unswapped, which is the worst kind of routing bug: everything
    /// works and the image is wrong.
    StereoSwapped,
    /// Both sides from channel 0.
    Left,
    /// Both sides from channel 1.
    Right,
    /// Both sides from the halved sum.
    Mono,
}

impl JamChannelMode {
    /// Resolve the mode from the logical channel indices an Audio Connection
    /// bound, which is the same shape hardware routes use.
    pub fn from_channels(channels: &[u32]) -> Self {
        match channels {
            [] => Self::Mono,
            [0] => Self::Left,
            [1] => Self::Right,
            // A single channel beyond the pair the ring holds. There is nothing
            // to pick, so it folds rather than reading a channel that is not
            // there.
            [_] => Self::Mono,
            // The same channel twice is a fold-down, not a stereo pair.
            [left, right, ..] if left == right => Self::Mono,
            [left, right, ..] if left > right => Self::StereoSwapped,
            [_, _, ..] => Self::Stereo,
        }
    }

    #[inline]
    fn apply(self, left: f32, right: f32) -> (f32, f32) {
        match self {
            Self::Stereo => (left, right),
            Self::StereoSwapped => (right, left),
            Self::Left => (left, left),
            Self::Right => (right, right),
            Self::Mono => {
                let sum = (left + right) * 0.5;
                (sum, sum)
            }
        }
    }
}

/// One remote stream's audio, written by the jam receive thread and drained by
/// the audio callback.
pub struct JamInputSlot {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    /// Total frames written since this slot was claimed. The low bits index the
    /// backing arrays through `MASK`.
    write_frames: AtomicU64,
    /// The consumer's position. Owned by the audio callback.
    read_frames: AtomicU64,
    /// `true` while a stream is bound to this slot.
    active: AtomicBool,
    /// Sample rate of the arriving stream, for diagnostics and for the caller
    /// that has to decide whether a conversion is needed.
    sample_rate: AtomicU32,
    channels: AtomicU32,
    /// Capture position, in this ring's sample rate, of absolute ring frame
    /// zero. `position(frame) = capture_base + frame` while the stream is
    /// continuous.
    capture_base: AtomicI64,
    /// Whether `capture_base` means anything yet. Before the first packet it
    /// does not, and a caller must be able to tell that from position zero.
    capture_known: AtomicBool,
    /// Max-hold peak since the last read, for the meter.
    peak: AtomicU32,
    /// Blocks the callback wanted and the network had not delivered.
    underruns: AtomicU64,
    /// Frames the producer overwrote before the callback read them.
    overruns: AtomicU64,
}

impl std::fmt::Debug for JamInputSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JamInputSlot")
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("write_frames", &self.write_frames.load(Ordering::Relaxed))
            .field("read_frames", &self.read_frames.load(Ordering::Relaxed))
            .field("sample_rate", &self.sample_rate.load(Ordering::Relaxed))
            .field("underruns", &self.underruns.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for JamInputSlot {
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
            read_frames: AtomicU64::new(0),
            active: AtomicBool::new(false),
            sample_rate: AtomicU32::new(0),
            channels: AtomicU32::new(0),
            capture_base: AtomicI64::new(0),
            capture_known: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
        }
    }
}

impl JamInputSlot {
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    pub fn channels(&self) -> u32 {
        self.channels.load(Ordering::Relaxed)
    }

    /// Frames written but not yet read.
    pub fn available(&self) -> u64 {
        self.write_frames
            .load(Ordering::Acquire)
            .saturating_sub(self.read_frames.load(Ordering::Relaxed))
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Read and reset the max-hold peak. The UI polls this at meter rate.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    /// The capture position of the next frame the callback will read, in this
    /// ring's sample rate, or `None` before any packet has arrived.
    ///
    /// This is the value a recorder aligns a remote take against. It is derived
    /// from the publisher's capture timestamp, never from when the packet
    /// happened to arrive, so audio that took forty milliseconds to cross the
    /// internet still lands where it was played.
    pub fn next_capture_position(&self) -> Option<i64> {
        if !self.capture_known.load(Ordering::Acquire) {
            return None;
        }
        let base = self.capture_base.load(Ordering::Acquire);
        Some(base.saturating_add(self.read_frames.load(Ordering::Relaxed) as i64))
    }

    /// Producer: append one block of interleaved samples.
    ///
    /// `capture_position` is the capture instant of the first frame in
    /// `interleaved`, already converted into this ring's sample rate by the
    /// bridge. Called from the jam receive thread, never from the audio
    /// callback.
    pub fn write_interleaved(
        &self,
        interleaved: &[f32],
        channels: usize,
        capture_position: u64,
        sample_rate: u32,
    ) {
        if channels == 0 || interleaved.is_empty() {
            return;
        }
        let frames = interleaved.len() / channels;
        if frames == 0 {
            return;
        }
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
        self.channels.store(channels as u32, Ordering::Relaxed);

        let write = self.write_frames.load(Ordering::Relaxed);
        let read = self.read_frames.load(Ordering::Acquire);

        // Re-base the capture clock whenever the ring is empty or the arriving
        // position does not continue where the last block left off. That covers
        // a fresh stream, a loss gap, and a publisher reconnect — every case
        // where the timeline genuinely moved.
        let expected = self
            .capture_base
            .load(Ordering::Relaxed)
            .saturating_add(write as i64);
        if !self.capture_known.load(Ordering::Relaxed) || expected != capture_position as i64 {
            self.capture_base
                .store(capture_position as i64 - write as i64, Ordering::Relaxed);
            self.capture_known.store(true, Ordering::Release);
        }

        // The consumer is a whole ring behind: it has stalled or the network is
        // ahead of the device. Overwriting is the realtime answer — the old
        // frames are past their moment — but it is counted so it is visible.
        if write.saturating_sub(read) + frames as u64 > CAPACITY_FRAMES as u64 {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }

        let mut peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        for frame in 0..frames {
            let at = frame * channels;
            let left = interleaved[at];
            let right = if channels >= 2 {
                interleaved[at + 1]
            } else {
                left
            };
            let index = ((write as usize).wrapping_add(frame)) & MASK;
            self.left[index].store(left.to_bits(), Ordering::Relaxed);
            self.right[index].store(right.to_bits(), Ordering::Relaxed);
            peak = peak.max(left.abs()).max(right.abs());
        }
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
        // Publish the frames only after they are all stored.
        self.write_frames
            .store(write.wrapping_add(frames as u64), Ordering::Release);
    }

    /// Consumer: mix `frames` frames into `out_l` / `out_r`.
    ///
    /// Returns how many frames actually came from the ring. A shortfall is left
    /// as whatever the caller had — silence on a freshly cleared block — rather
    /// than waited for. Realtime-safe: atomics only.
    #[inline]
    pub fn mix_into(
        &self,
        mode: JamChannelMode,
        out_l: &mut [f32],
        out_r: &mut [f32],
        frames: usize,
    ) -> usize {
        if !self.active.load(Ordering::Acquire) {
            return 0;
        }
        let write = self.write_frames.load(Ordering::Acquire);
        let mut read = self.read_frames.load(Ordering::Relaxed);

        // Never read a frame the producer has already lapped: the samples under
        // it belong to a much later moment.
        let lag = write.saturating_sub(read);
        if lag > CAPACITY_FRAMES as u64 {
            read = write - CAPACITY_FRAMES as u64;
        }

        let ready = write.saturating_sub(read).min(frames as u64) as usize;
        let usable = ready.min(out_l.len()).min(out_r.len());
        for offset in 0..usable {
            let index = ((read as usize).wrapping_add(offset)) & MASK;
            let left = f32::from_bits(self.left[index].load(Ordering::Relaxed));
            let right = f32::from_bits(self.right[index].load(Ordering::Relaxed));
            let (l, r) = mode.apply(left, right);
            out_l[offset] += l;
            out_r[offset] += r;
        }
        self.read_frames
            .store(read.wrapping_add(usable as u64), Ordering::Relaxed);
        if usable < frames {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        usable
    }

    fn claim(&self) {
        self.write_frames.store(0, Ordering::Relaxed);
        self.read_frames.store(0, Ordering::Relaxed);
        self.capture_base.store(0, Ordering::Relaxed);
        self.capture_known.store(false, Ordering::Relaxed);
        self.peak.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.overruns.store(0, Ordering::Relaxed);
        self.sample_rate.store(0, Ordering::Relaxed);
        self.channels.store(0, Ordering::Relaxed);
        self.active.store(true, Ordering::Release);
    }

    fn release(&self) {
        self.active.store(false, Ordering::Release);
    }
}

/// One local source being published, written by the audio callback and drained
/// by the jam publish thread.
pub struct JamPublishSlot {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    write_frames: AtomicU64,
    read_frames: AtomicU64,
    active: AtomicBool,
    sample_rate: AtomicU32,
    /// Session tick corresponding to absolute ring frame zero, published by the
    /// bridge once the jam clock has locked. Unlike the input side this one is
    /// in session ticks: it goes straight into an outgoing packet header, which
    /// the protocol defines in the jam's own tick domain.
    capture_base: AtomicI64,
    capture_known: AtomicBool,
    /// Frames the callback overwrote before the publish thread drained them.
    overruns: AtomicU64,
}

impl std::fmt::Debug for JamPublishSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JamPublishSlot")
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("write_frames", &self.write_frames.load(Ordering::Relaxed))
            .field("read_frames", &self.read_frames.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for JamPublishSlot {
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
            read_frames: AtomicU64::new(0),
            active: AtomicBool::new(false),
            sample_rate: AtomicU32::new(0),
            capture_base: AtomicI64::new(0),
            capture_known: AtomicBool::new(false),
            overruns: AtomicU64::new(0),
        }
    }
}

impl JamPublishSlot {
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Producer: append one interleaved block from the audio callback.
    ///
    /// Realtime-safe: atomics only, no allocation, no lock. When nothing is
    /// subscribed the slot is inactive and this returns immediately, so an
    /// unpublished engine pays one atomic load per block.
    #[inline]
    pub fn write_interleaved(&self, interleaved: &[f32], channels: usize, sample_rate: u32) {
        if !self.active.load(Ordering::Acquire) || channels == 0 || interleaved.is_empty() {
            return;
        }
        let frames = interleaved.len() / channels;
        if frames == 0 {
            return;
        }
        self.sample_rate.store(sample_rate, Ordering::Relaxed);

        let write = self.write_frames.load(Ordering::Relaxed);
        let read = self.read_frames.load(Ordering::Acquire);
        if write.saturating_sub(read) + frames as u64 > CAPACITY_FRAMES as u64 {
            // The publish thread is not keeping up. Dropping the oldest frames
            // is the realtime policy: the callback must not wait, and a jam
            // listener would rather lose a moment than hear everything late.
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
        for frame in 0..frames {
            let at = frame * channels;
            let left = interleaved[at];
            let right = if channels >= 2 {
                interleaved[at + 1]
            } else {
                left
            };
            let index = ((write as usize).wrapping_add(frame)) & MASK;
            self.left[index].store(left.to_bits(), Ordering::Relaxed);
            self.right[index].store(right.to_bits(), Ordering::Relaxed);
        }
        self.write_frames
            .store(write.wrapping_add(frames as u64), Ordering::Release);
    }

    /// Consumer: take up to `max_frames` interleaved stereo frames.
    ///
    /// Returns the number of frames written into `out` and the session tick of
    /// the first of them, when the clock base is known.
    pub fn read_interleaved(
        &self,
        out: &mut Vec<f32>,
        max_frames: usize,
    ) -> Option<(usize, Option<i64>)> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let write = self.write_frames.load(Ordering::Acquire);
        let mut read = self.read_frames.load(Ordering::Relaxed);
        let lag = write.saturating_sub(read);
        if lag > CAPACITY_FRAMES as u64 {
            read = write - CAPACITY_FRAMES as u64;
        }
        let ready = write.saturating_sub(read).min(max_frames as u64) as usize;
        if ready == 0 {
            return None;
        }
        out.clear();
        out.reserve(ready * 2);
        for offset in 0..ready {
            let index = ((read as usize).wrapping_add(offset)) & MASK;
            out.push(f32::from_bits(self.left[index].load(Ordering::Relaxed)));
            out.push(f32::from_bits(self.right[index].load(Ordering::Relaxed)));
        }
        let tick = if self.capture_known.load(Ordering::Acquire) {
            Some(
                self.capture_base
                    .load(Ordering::Acquire)
                    .saturating_add(read as i64),
            )
        } else {
            None
        };
        self.read_frames
            .store(read.wrapping_add(ready as u64), Ordering::Relaxed);
        Some((ready, tick))
    }

    /// Publish the mapping from ring frames to session ticks.
    ///
    /// Called by the bridge once the jam clock has locked, and again whenever
    /// it re-anchors. Until then a published packet carries no usable capture
    /// timestamp, which receivers are told by the tick being absent rather than
    /// by a guessed value.
    pub fn set_capture_base(&self, base: i64) {
        self.capture_base.store(base, Ordering::Release);
        self.capture_known.store(true, Ordering::Release);
    }

    /// Frames written since this slot was claimed, so the bridge can compute a
    /// capture base against the current head.
    pub fn write_head(&self) -> u64 {
        self.write_frames.load(Ordering::Acquire)
    }

    fn claim(&self) {
        self.write_frames.store(0, Ordering::Relaxed);
        self.read_frames.store(0, Ordering::Relaxed);
        self.capture_base.store(0, Ordering::Relaxed);
        self.capture_known.store(false, Ordering::Relaxed);
        self.overruns.store(0, Ordering::Relaxed);
        self.active.store(true, Ordering::Release);
    }

    fn release(&self) {
        self.active.store(false, Ordering::Release);
    }
}

/// The slot tables, shared between the engine and the jam bridge.
///
/// One instance lives in the engine's `SharedState`, so the audio callback
/// reaches it through the `Arc` it already holds.
pub struct JamAudioBus {
    inputs: Box<[JamInputSlot]>,
    publishes: Box<[JamPublishSlot]>,
    /// Stream id to input slot. Written by the control thread only; the audio
    /// callback never touches it, because it works from a resolved index that
    /// was baked into the runtime snapshot.
    input_keys: RwLock<HashMap<String, usize>>,
    publish_keys: RwLock<HashMap<String, usize>>,
    /// `true` while at least one input slot is bound. Lets the render pass skip
    /// the whole jam branch with one relaxed load.
    any_input: AtomicBool,
    any_publish: AtomicBool,
}

impl std::fmt::Debug for JamAudioBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JamAudioBus")
            .field("inputs_bound", &self.bound_input_count())
            .field("publishes_bound", &self.bound_publish_count())
            .finish()
    }
}

impl Default for JamAudioBus {
    fn default() -> Self {
        Self {
            inputs: (0..MAX_JAM_INPUT_SLOTS)
                .map(|_| JamInputSlot::default())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            publishes: (0..MAX_JAM_PUBLISH_SLOTS)
                .map(|_| JamPublishSlot::default())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            input_keys: RwLock::new(HashMap::new()),
            publish_keys: RwLock::new(HashMap::new()),
            any_input: AtomicBool::new(false),
            any_publish: AtomicBool::new(false),
        }
    }
}

impl JamAudioBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any remote stream is bound. One relaxed load, so the render pass
    /// can skip the jam branch entirely in a project that is not in a jam.
    #[inline]
    pub fn has_inputs(&self) -> bool {
        self.any_input.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn has_publishes(&self) -> bool {
        self.any_publish.load(Ordering::Relaxed)
    }

    /// The slot at `index`, or `None` when the index is out of range.
    #[inline]
    pub fn input(&self, index: usize) -> Option<&JamInputSlot> {
        self.inputs.get(index)
    }

    #[inline]
    pub fn publish(&self, index: usize) -> Option<&JamPublishSlot> {
        self.publishes.get(index)
    }

    /// Bind a remote stream to a slot, or return the slot it already holds.
    ///
    /// Control thread only. `None` means every slot is taken, which the caller
    /// must report rather than silently route somewhere else.
    pub fn bind_input(&self, stream_id: &str) -> Option<usize> {
        let mut keys = self.input_keys.write().ok()?;
        if let Some(index) = keys.get(stream_id) {
            return Some(*index);
        }
        let index = self
            .inputs
            .iter()
            .position(|slot| !slot.active.load(Ordering::Acquire))?;
        self.inputs[index].claim();
        keys.insert(stream_id.to_string(), index);
        self.any_input.store(true, Ordering::Release);
        Some(index)
    }

    /// The slot a stream is bound to, if any.
    pub fn input_slot_for(&self, stream_id: &str) -> Option<usize> {
        self.input_keys.read().ok()?.get(stream_id).copied()
    }

    /// Release a stream's slot. The audio callback keeps reading the index it
    /// was given, and reads silence, until the next runtime snapshot removes
    /// the binding — which is what makes a performer leaving mid-block safe.
    pub fn release_input(&self, stream_id: &str) {
        let Ok(mut keys) = self.input_keys.write() else {
            return;
        };
        if let Some(index) = keys.remove(stream_id) {
            self.inputs[index].release();
        }
        self.any_input.store(!keys.is_empty(), Ordering::Release);
    }

    /// Bind a publish source keyed by its own identity, e.g. `master` or
    /// `track:trk_1`.
    pub fn bind_publish(&self, key: &str) -> Option<usize> {
        let mut keys = self.publish_keys.write().ok()?;
        if let Some(index) = keys.get(key) {
            return Some(*index);
        }
        let index = self
            .publishes
            .iter()
            .position(|slot| !slot.active.load(Ordering::Acquire))?;
        self.publishes[index].claim();
        keys.insert(key.to_string(), index);
        self.any_publish.store(true, Ordering::Release);
        Some(index)
    }

    pub fn publish_slot_for(&self, key: &str) -> Option<usize> {
        self.publish_keys.read().ok()?.get(key).copied()
    }

    pub fn release_publish(&self, key: &str) {
        let Ok(mut keys) = self.publish_keys.write() else {
            return;
        };
        if let Some(index) = keys.remove(key) {
            self.publishes[index].release();
        }
        self.any_publish.store(!keys.is_empty(), Ordering::Release);
    }

    /// Drop every binding. Used when a jam session ends, so a project left open
    /// afterwards has no stale routes claiming slots.
    pub fn release_all(&self) {
        if let Ok(mut keys) = self.input_keys.write() {
            for index in keys.values() {
                self.inputs[*index].release();
            }
            keys.clear();
        }
        if let Ok(mut keys) = self.publish_keys.write() {
            for index in keys.values() {
                self.publishes[*index].release();
            }
            keys.clear();
        }
        self.any_input.store(false, Ordering::Release);
        self.any_publish.store(false, Ordering::Release);
    }

    pub fn bound_input_count(&self) -> usize {
        self.input_keys.read().map(|keys| keys.len()).unwrap_or(0)
    }

    pub fn bound_publish_count(&self) -> usize {
        self.publish_keys.read().map(|keys| keys.len()).unwrap_or(0)
    }

    /// Every bound stream id with its slot, for diagnostics and for the panel.
    pub fn bound_inputs(&self) -> Vec<(String, usize)> {
        self.input_keys
            .read()
            .map(|keys| {
                let mut out: Vec<(String, usize)> = keys
                    .iter()
                    .map(|(id, index)| (id.clone(), *index))
                    .collect();
                out.sort();
                out
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jam_device_ids_round_trip_and_do_not_claim_hardware() {
        let id = jam_device_id("str_01K4S8");
        assert_eq!(id, "jam:str_01K4S8");
        assert!(is_jam_device(&id));
        assert_eq!(jam_stream_id(&id), Some("str_01K4S8"));

        assert!(!is_jam_device("Focusrite USB ASIO"));
        assert_eq!(jam_stream_id("Focusrite USB ASIO"), None);
    }

    #[test]
    fn a_written_block_comes_back_through_the_callback_path() {
        let slot = JamInputSlot::default();
        slot.claim();
        // Two stereo frames: (0.25, -0.25), (0.5, -0.5).
        slot.write_interleaved(&[0.25, -0.25, 0.5, -0.5], 2, 1_000, 48_000);

        let mut left = vec![0.0; 2];
        let mut right = vec![0.0; 2];
        let read = slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 2);
        assert_eq!(read, 2);
        assert_eq!(left, vec![0.25, 0.5]);
        assert_eq!(right, vec![-0.25, -0.5]);
        assert_eq!(slot.sample_rate(), 48_000);
    }

    #[test]
    fn the_callback_mixes_rather_than_replaces_so_a_track_can_carry_other_material() {
        let slot = JamInputSlot::default();
        slot.claim();
        slot.write_interleaved(&[0.25, 0.25], 2, 0, 48_000);

        let mut left = vec![0.5];
        let mut right = vec![0.5];
        slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 1);
        assert_eq!(left, vec![0.75]);
        assert_eq!(right, vec![0.75]);
    }

    #[test]
    fn an_empty_slot_reads_as_silence_and_never_blocks() {
        let slot = JamInputSlot::default();
        slot.claim();
        let mut left = vec![0.0; 4];
        let mut right = vec![0.0; 4];
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 4),
            0
        );
        assert_eq!(left, vec![0.0; 4]);
        assert_eq!(slot.underruns(), 1);
    }

    #[test]
    fn an_inactive_slot_produces_nothing_at_all() {
        let slot = JamInputSlot::default();
        let mut left = vec![0.0; 4];
        let mut right = vec![0.0; 4];
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 4),
            0
        );
        // Not even an underrun: nothing is expected from a slot nobody bound.
        assert_eq!(slot.underruns(), 0);
    }

    #[test]
    fn a_partial_block_fills_what_it_can_and_counts_the_shortfall() {
        let slot = JamInputSlot::default();
        slot.claim();
        slot.write_interleaved(&[1.0, 1.0], 2, 0, 48_000);

        let mut left = vec![0.0; 4];
        let mut right = vec![0.0; 4];
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 4),
            1
        );
        assert_eq!(left, vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(slot.underruns(), 1);
    }

    #[test]
    fn channel_modes_fold_a_stereo_stream_the_documented_way() {
        for (mode, expect) in [
            (JamChannelMode::Stereo, (1.0f32, 0.0f32)),
            (JamChannelMode::Left, (1.0, 1.0)),
            (JamChannelMode::Right, (0.0, 0.0)),
            (JamChannelMode::Mono, (0.5, 0.5)),
        ] {
            let slot = JamInputSlot::default();
            slot.claim();
            slot.write_interleaved(&[1.0, 0.0], 2, 0, 48_000);
            let mut left = vec![0.0];
            let mut right = vec![0.0];
            slot.mix_into(mode, &mut left, &mut right, 1);
            assert_eq!((left[0], right[0]), expect, "{mode:?}");
        }
    }

    #[test]
    fn a_mono_stream_reaches_both_sides() {
        let slot = JamInputSlot::default();
        slot.claim();
        slot.write_interleaved(&[0.75, 0.5], 1, 0, 48_000);
        let mut left = vec![0.0; 2];
        let mut right = vec![0.0; 2];
        slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 2);
        assert_eq!(left, vec![0.75, 0.5]);
        assert_eq!(right, vec![0.75, 0.5]);
    }

    #[test]
    fn the_capture_position_follows_the_publisher_not_the_arrival() {
        let slot = JamInputSlot::default();
        slot.claim();
        assert_eq!(
            slot.next_capture_position(),
            None,
            "unknown before any packet"
        );

        // The publisher captured this at position 20 000 000.
        slot.write_interleaved(&[0.0; 8], 2, 20_000_000, 48_000);
        assert_eq!(slot.next_capture_position(), Some(20_000_000));

        // After the callback consumes two frames the next one is two ticks on.
        let mut left = vec![0.0; 2];
        let mut right = vec![0.0; 2];
        slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 2);
        assert_eq!(slot.next_capture_position(), Some(20_000_002));
    }

    #[test]
    fn a_gap_re_bases_the_capture_clock_instead_of_drifting() {
        let slot = JamInputSlot::default();
        slot.claim();
        slot.write_interleaved(&[0.0; 4], 2, 1_000, 48_000);
        // The next block should have been tick 1002; it is 5000, because
        // packets were lost. The mapping follows the publisher.
        slot.write_interleaved(&[0.0; 4], 2, 5_000, 48_000);

        let mut left = vec![0.0; 2];
        let mut right = vec![0.0; 2];
        slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 2);
        assert_eq!(slot.next_capture_position(), Some(5_000 - 2 + 2));
    }

    #[test]
    fn a_publish_slot_round_trips_the_callbacks_output() {
        let slot = JamPublishSlot::default();
        slot.claim();
        slot.write_interleaved(&[0.1, 0.2, 0.3, 0.4], 2, 48_000);

        let mut out = Vec::new();
        let (frames, tick) = slot.read_interleaved(&mut out, 8).expect("frames ready");
        assert_eq!(frames, 2);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(tick, None, "the tick is absent until the jam clock locks");

        assert!(slot.read_interleaved(&mut out, 8).is_none());
    }

    #[test]
    fn an_unclaimed_publish_slot_costs_the_callback_one_load() {
        let slot = JamPublishSlot::default();
        slot.write_interleaved(&[1.0, 1.0], 2, 48_000);
        let mut out = Vec::new();
        assert!(slot.read_interleaved(&mut out, 8).is_none());
        assert_eq!(slot.write_head(), 0);
    }

    #[test]
    fn a_published_capture_base_reaches_the_reader() {
        let slot = JamPublishSlot::default();
        slot.claim();
        slot.write_interleaved(&[0.0; 4], 2, 48_000);
        slot.set_capture_base(20_000_000);
        let mut out = Vec::new();
        let (_, tick) = slot.read_interleaved(&mut out, 8).expect("frames ready");
        assert_eq!(tick, Some(20_000_000));
    }

    #[test]
    fn binding_is_idempotent_and_bounded() {
        let bus = JamAudioBus::new();
        assert!(!bus.has_inputs());

        let first = bus.bind_input("str_1").expect("a slot is free");
        assert_eq!(bus.bind_input("str_1"), Some(first), "rebinding is a no-op");
        assert!(bus.has_inputs());
        assert_eq!(bus.input_slot_for("str_1"), Some(first));

        for index in 0..MAX_JAM_INPUT_SLOTS {
            bus.bind_input(&format!("filler_{index}"));
        }
        // Every slot is taken; the next stream is refused rather than routed
        // somewhere arbitrary.
        assert_eq!(bus.bind_input("one_too_many"), None);
    }

    #[test]
    fn releasing_a_stream_frees_its_slot_and_silences_it() {
        let bus = JamAudioBus::new();
        let index = bus.bind_input("str_1").expect("bound");
        bus.input(index)
            .expect("in range")
            .write_interleaved(&[1.0, 1.0], 2, 0, 48_000);

        bus.release_input("str_1");
        assert!(!bus.has_inputs());
        assert_eq!(bus.input_slot_for("str_1"), None);

        // The audio callback may still hold the old index for one block.
        let mut left = vec![0.0];
        let mut right = vec![0.0];
        assert_eq!(
            bus.input(index).expect("in range").mix_into(
                JamChannelMode::Stereo,
                &mut left,
                &mut right,
                1
            ),
            0
        );
        assert_eq!(left, vec![0.0]);
    }

    #[test]
    fn releasing_everything_leaves_no_slot_claimed() {
        let bus = JamAudioBus::new();
        bus.bind_input("str_1");
        bus.bind_input("str_2");
        bus.bind_publish("master");
        bus.release_all();
        assert_eq!(bus.bound_input_count(), 0);
        assert_eq!(bus.bound_publish_count(), 0);
        assert!(!bus.has_inputs());
        assert!(!bus.has_publishes());
    }

    #[test]
    fn channel_modes_come_from_the_same_index_shape_hardware_routes_use() {
        assert_eq!(JamChannelMode::from_channels(&[]), JamChannelMode::Mono);
        assert_eq!(JamChannelMode::from_channels(&[0]), JamChannelMode::Left);
        assert_eq!(JamChannelMode::from_channels(&[1]), JamChannelMode::Right);
        assert_eq!(
            JamChannelMode::from_channels(&[0, 1]),
            JamChannelMode::Stereo
        );
        assert_eq!(
            JamChannelMode::from_channels(&[1, 0]),
            JamChannelMode::StereoSwapped,
            "a route bound right-to-left really is swapped"
        );
        assert_eq!(
            JamChannelMode::from_channels(&[0, 0]),
            JamChannelMode::Mono,
            "a route that names one channel twice is a fold-down, not a stereo pair"
        );
    }

    #[test]
    fn a_swapped_route_swaps_the_sides() {
        let slot = JamInputSlot::default();
        slot.claim();
        slot.write_interleaved(&[1.0, -1.0], 2, 0, 48_000);
        let mut left = vec![0.0];
        let mut right = vec![0.0];
        slot.mix_into(JamChannelMode::StereoSwapped, &mut left, &mut right, 1);
        assert_eq!((left[0], right[0]), (-1.0, 1.0));
    }

    #[test]
    fn a_lapped_consumer_resynchronises_instead_of_reading_stale_audio() {
        let slot = JamInputSlot::default();
        slot.claim();
        // Write more than a whole ring without reading any of it.
        let block = vec![0.0f32; 2 * 1024];
        for _ in 0..(CAPACITY_FRAMES / 1024 + 2) {
            slot.write_interleaved(&block, 2, 0, 48_000);
        }
        assert!(slot.overruns() > 0);

        let mut left = vec![0.0; 64];
        let mut right = vec![0.0; 64];
        let read = slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 64);
        assert_eq!(read, 64, "the consumer catches up to the live window");
    }
}
