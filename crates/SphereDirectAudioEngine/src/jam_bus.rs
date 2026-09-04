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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;

/// Ring capacity per slot, in frames. Power of two so the wrap is a mask.
/// 16384 frames is 341 ms at 48 kHz — far more than any jitter buffer plus
/// callback pair, so the consumer never overruns under normal scheduling.
const CAPACITY_FRAMES: usize = 1 << 14;
const MASK: usize = CAPACITY_FRAMES - 1;

/// Backlog a receiving slot holds before it starts handing blocks to the
/// callback, in milliseconds of its own ring rate.
///
/// The client's jitter buffer reorders packets; it does not absorb the phase
/// difference between a network that delivers in bursts and a device that asks
/// for exactly one block on a hard clock. Without a cushion here the ring runs
/// at zero backlog by construction: every block where the next packet has not
/// landed yet is a short read, and a short read is a hole in the audio. That is
/// the crackle. Thirty milliseconds is the same order as the hardware monitor
/// ring's own target and costs the room nothing a network path had not already
/// spent.
const TARGET_MS: u64 = 30;

/// The backlog a slot aims to hold, in frames.
///
/// Never less than two callback blocks: a cushion thinner than the thing it is
/// cushioning is not one.
#[inline]
fn target_frames(sample_rate: u32, block_frames: usize) -> u64 {
    let by_time = (sample_rate.max(1) as u64 * TARGET_MS) / 1000;
    by_time.max((block_frames as u64).saturating_mul(2)).min(
        // Never more than a quarter of the ring, or a resync could not recover.
        (CAPACITY_FRAMES / 4) as u64,
    )
}

/// How many remote streams can feed tracks at once.
///
/// Fixed rather than grown on demand: the audio callback indexes this table and
/// must never wait on a reallocation. Thirty-two is well past the server's own
/// participant ceiling for one jam.
pub const MAX_JAM_INPUT_SLOTS: usize = 32;

/// How many local sources can be published at once.
///
/// These are the stereo slots: the master mix, and one per individually shared
/// track. The multitrack slot is separate and is not counted here — see
/// [`MULTITRACK_PUBLISH_SLOT`].
pub const MAX_JAM_PUBLISH_SLOTS: usize = 8;

/// Widest layout the multitrack publish slot can carry, in channels.
///
/// Eight stereo pairs. The ceiling is the jam server's own
/// `protocol.MaxStreamChannels`, and beyond it the datagram budget makes an
/// uncompressed stream unsendable anyway: sixteen channels of 16-bit audio is
/// 32 bytes a frame, which fits about thirty-seven frames in one packet.
pub const MAX_MULTITRACK_CHANNELS: usize = 16;

/// How many track pairs the multitrack slot carries.
pub const MAX_MULTITRACK_PAIRS: usize = MAX_MULTITRACK_CHANNELS / 2;

/// Index of the multitrack publish slot.
///
/// It sits after the stereo slots in the same table so the realtime path
/// addresses every publish slot the same way — one bounds-checked index — while
/// only this one pays for the wide ring storage.
pub const MULTITRACK_PUBLISH_SLOT: usize = MAX_JAM_PUBLISH_SLOTS;

/// Total publish slots, stereo plus the one wide slot.
pub const TOTAL_JAM_PUBLISH_SLOTS: usize = MAX_JAM_PUBLISH_SLOTS + 1;

/// The multitrack pair-assignment entry for "nobody fills this pair".
///
/// A sentinel rather than an `Option<u32>` so the assignment stays a plain
/// `Copy` array the audio callback can apply without touching the heap.
pub const NO_JAM_PAIR: u32 = u32::MAX;

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

/// Publish-slot key for the engine's live hardware input pair.
///
/// The one capture path the callback already stages — the pair the Control
/// Room monitors — tapped before it reaches any track. It is how a performer
/// sends an instrument into a jam without arming a track for it, and it is
/// keyed by a constant rather than by connection because the engine has one
/// live input pair, not one per Audio Connection; see
/// [`publish_key_hardware_input`] for the per-connection form that shape will
/// take when it exists.
pub const PUBLISH_KEY_LIVE_INPUT: &str = "live-input";

/// Publish-slot key for the Control Room / Monitor output.
///
/// Distinct from [`PUBLISH_KEY_MASTER`], and the difference is the reason it
/// exists: master is the mix *before* the Control Room, which is what an export
/// gets. This is the signal after the monitor insert chain and the control
/// processor — what actually leaves for the monitoring output, which is what
/// someone means by "send what I am hearing".
pub const PUBLISH_KEY_MONITOR: &str = "monitor";

/// Device id of the Audio Jam's *output* ports in Audio Connections.
///
/// Where [`JAM_DEVICE_PREFIX`] makes a remote stream selectable as a track
/// input, this makes the jam selectable as an output: an enabled output bus
/// bound to these ports is a jam send, and its name is the stream the room
/// sees. It deliberately does not carry the input prefix, so nothing that
/// strips a stream id off a device id can mistake a send for a stream.
pub const JAM_SEND_DEVICE_ID: &str = "jam-send";

/// Port index of the master tap's left channel on [`JAM_SEND_DEVICE_ID`].
pub const JAM_SEND_PORT_MASTER: u32 = 0;
/// Port index of the live-input tap's left channel on [`JAM_SEND_DEVICE_ID`].
pub const JAM_SEND_PORT_LIVE_INPUT: u32 = 2;
/// Port index of the Control Room / Monitor tap's left channel.
pub const JAM_SEND_PORT_MONITOR: u32 = 4;
/// Ports on the send device: two taps, a stereo pair each.
pub const JAM_SEND_PORTS: u32 = 6;

/// Whether a device id names the jam send device.
pub fn is_jam_send_device(device_id: &str) -> bool {
    device_id == JAM_SEND_DEVICE_ID
}

/// Publish-slot key for the multitrack stream.
///
/// One key, one slot, one stream: every shared track occupies a channel pair
/// inside it rather than a stream of its own, so a receiving Studio gets one
/// clock, one sequence and one capture base to align the whole arrangement
/// against instead of N independently jittered ones.
pub const PUBLISH_KEY_MULTITRACK: &str = "multitrack";

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

    /// Resolve a source frame to the destination pair.
    ///
    /// Public because track loopback maps its source the same way a jam stream
    /// does — the mode describes a stereo route, not anything jam-specific —
    /// and a second identical enum would be one more thing to keep in step.
    #[inline]
    pub fn apply(self, left: f32, right: f32) -> (f32, f32) {
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
    /// `true` while the ring is filling its cushion and contributing silence.
    ///
    /// Set when the slot is claimed and again on every underrun, so a stream
    /// that falls behind rebuilds its backlog once instead of tearing a hole in
    /// every block from then on.
    priming: AtomicBool,
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
            priming: AtomicBool::new(true),
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
    /// Returns how many frames came from the ring — `frames` or nothing.
    ///
    /// **A block is whole or it is silence.** Handing back a partial block puts
    /// a hole in the middle of the audio, and because the ring would then be
    /// empty again next callback, it puts one in every block after it: that is
    /// the crackle a jam listener hears. Instead the slot holds a cushion
    /// ([`target_frames`]), starts consuming only once it has one, and on an
    /// underrun goes back to filling rather than tearing. The listener gets a
    /// short silence while the cushion rebuilds and then clean audio, instead
    /// of ripped audio indefinitely.
    ///
    /// Realtime-safe: atomics only, no allocation, never a wait.
    #[inline]
    pub fn mix_into(
        &self,
        mode: JamChannelMode,
        out_l: &mut [f32],
        out_r: &mut [f32],
        frames: usize,
    ) -> usize {
        if !self.active.load(Ordering::Acquire) || frames == 0 {
            return 0;
        }
        let want = frames.min(out_l.len()).min(out_r.len());
        if want == 0 {
            return 0;
        }
        let write = self.write_frames.load(Ordering::Acquire);
        let mut read = self.read_frames.load(Ordering::Relaxed);

        // Never read a frame the producer has already lapped: the samples under
        // it belong to a much later moment.
        if write.saturating_sub(read) > CAPACITY_FRAMES as u64 {
            read = write.saturating_sub(CAPACITY_FRAMES as u64);
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }

        let target = target_frames(self.sample_rate.load(Ordering::Relaxed), want);
        let available = write.saturating_sub(read);

        if self.priming.load(Ordering::Relaxed) {
            // Still filling the cushion. Contribute nothing and leave the read
            // cursor where it is; the frames are not late, they are early.
            if available < target {
                self.read_frames.store(read, Ordering::Relaxed);
                return 0;
            }
            self.priming.store(false, Ordering::Relaxed);
        }

        if available < want as u64 {
            // The cushion is spent. One counted underrun and back to priming,
            // rather than a torn block now and another one next callback.
            self.underruns.fetch_add(1, Ordering::Relaxed);
            self.priming.store(true, Ordering::Relaxed);
            self.read_frames.store(read, Ordering::Relaxed);
            return 0;
        }

        // Latency crept above the cushion — the network ran ahead of the device,
        // or the callback stalled. Skip forward instead of playing further and
        // further behind; the frames skipped are already past their moment.
        if available > target.saturating_add(want as u64) {
            read = write.saturating_sub(target);
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }

        for offset in 0..want {
            let index = ((read as usize).wrapping_add(offset)) & MASK;
            let left = f32::from_bits(self.left[index].load(Ordering::Relaxed));
            let right = f32::from_bits(self.right[index].load(Ordering::Relaxed));
            let (l, r) = mode.apply(left, right);
            out_l[offset] += l;
            out_r[offset] += r;
        }
        self.read_frames
            .store(read.wrapping_add(want as u64), Ordering::Relaxed);
        want
    }

    /// Whether this slot is filling its cushion rather than playing. Diagnostic
    /// and UI only — the callback reads the flag directly.
    pub fn is_priming(&self) -> bool {
        self.priming.load(Ordering::Relaxed)
    }

    /// Test seam: pretend the cushion is already full.
    ///
    /// A test about channel folding or capture positions is not a test about
    /// the cushion, and should not have to write thirty milliseconds of warm-up
    /// to ask its own question. The cushion has its own tests.
    #[cfg(test)]
    pub(crate) fn assume_primed(&self) {
        self.priming.store(false, Ordering::Relaxed);
    }

    fn claim(&self) {
        self.write_frames.store(0, Ordering::Relaxed);
        self.read_frames.store(0, Ordering::Relaxed);
        self.priming.store(true, Ordering::Relaxed);
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
///
/// The ring is planar: one preallocated channel ring per channel the slot can
/// carry. A stereo slot has two and a multitrack slot has
/// [`MAX_MULTITRACK_CHANNELS`], and neither ever grows — the widest layout a
/// slot will ever hold is decided when the bus is built, because the audio
/// callback cannot wait for a reallocation.
pub struct JamPublishSlot {
    planes: Box<[Box<[AtomicU32]>]>,
    /// Channels in use for the current claim. Never above `planes.len()`, and
    /// fixed for the life of a claim so a reader that asks once per block
    /// cannot observe it changing mid-read.
    channels: AtomicU32,
    /// Which pairs a multitrack producer has staged into the block being
    /// assembled. Cleared by [`JamPublishSlot::commit`], which zeroes whatever
    /// was not staged — so a track that stops sharing mid-stream leaves silence
    /// in its pair rather than one block repeated forever.
    staged_pairs: AtomicU32,
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
    /// Max-hold peak of what was written since the last read, for the panel.
    peak: AtomicU32,
}

impl std::fmt::Debug for JamPublishSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JamPublishSlot")
            .field("active", &self.active.load(Ordering::Relaxed))
            .field("channels", &self.channels.load(Ordering::Relaxed))
            .field("write_frames", &self.write_frames.load(Ordering::Relaxed))
            .field("read_frames", &self.read_frames.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for JamPublishSlot {
    fn default() -> Self {
        Self::with_capacity_channels(2)
    }
}

impl JamPublishSlot {
    /// Allocate a slot that can carry up to `channels` channels.
    ///
    /// Every ring is allocated here, once, because the producer is the audio
    /// callback. A slot claimed for a narrower layout simply leaves the rest of
    /// its rings untouched.
    pub fn with_capacity_channels(channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_MULTITRACK_CHANNELS);
        Self {
            planes: (0..channels)
                .map(|_| {
                    (0..CAPACITY_FRAMES)
                        .map(|_| AtomicU32::new(0))
                        .collect::<Vec<_>>()
                        .into_boxed_slice()
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            channels: AtomicU32::new(2.min(channels) as u32),
            staged_pairs: AtomicU32::new(0),
            write_frames: AtomicU64::new(0),
            read_frames: AtomicU64::new(0),
            active: AtomicBool::new(false),
            sample_rate: AtomicU32::new(0),
            capture_base: AtomicI64::new(0),
            capture_known: AtomicBool::new(false),
            overruns: AtomicU64::new(0),
            peak: AtomicU32::new(0),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    /// Read and reset the max-hold peak of what actually left this slot.
    ///
    /// The difference between "a stream is announced" and "the room can hear
    /// it" is exactly this number, and without it a send that is bound to a tap
    /// nothing feeds looks identical on screen to one that is working.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    /// Channels this slot is currently carrying.
    pub fn channels(&self) -> usize {
        self.channels.load(Ordering::Acquire) as usize
    }

    /// The widest layout this slot could ever carry.
    pub fn capacity_channels(&self) -> usize {
        self.planes.len()
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
        if channels == 0 || interleaved.is_empty() {
            return;
        }
        let frames = interleaved.len() / channels;
        self.write_frames_with(frames, sample_rate, |frame| {
            let at = frame * channels;
            let left = interleaved[at];
            let right = if channels >= 2 {
                interleaved[at + 1]
            } else {
                left
            };
            (left, right)
        });
    }

    /// The same, from the separate left and right buffers the mixer works in.
    ///
    /// The engine renders planar, so an interleaving pass on the way out would
    /// be a copy the callback does not otherwise need, into a scratch buffer
    /// every track would have to carry.
    #[inline]
    pub fn write_planar(&self, left: &[f32], right: &[f32], frames: usize, sample_rate: u32) {
        let frames = frames.min(left.len()).min(right.len());
        self.write_frames_with(frames, sample_rate, |frame| (left[frame], right[frame]));
    }

    #[inline]
    fn write_frames_with(
        &self,
        frames: usize,
        sample_rate: u32,
        mut sample: impl FnMut(usize) -> (f32, f32),
    ) {
        if !self.active.load(Ordering::Acquire) || frames == 0 || self.planes.len() < 2 {
            return;
        }
        let write = self.write_frames.load(Ordering::Relaxed);
        let mut peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        for frame in 0..frames {
            let (left, right) = sample(frame);
            let index = ((write as usize).wrapping_add(frame)) & MASK;
            self.planes[0][index].store(left.to_bits(), Ordering::Relaxed);
            self.planes[1][index].store(right.to_bits(), Ordering::Relaxed);
            peak = peak.max(left.abs()).max(right.abs());
        }
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
        self.channels.store(2, Ordering::Release);
        self.advance(write, frames, sample_rate);
    }

    /// Producer: write one channel pair of the block being assembled, leaving
    /// the head where it is.
    ///
    /// A multitrack block is written by several callers — one per shared track,
    /// each from its own point in the render pass — so the head cannot advance
    /// with the first of them. [`JamPublishSlot::commit`] closes the block once
    /// every track has had its turn.
    ///
    /// Realtime-safe: bounds-checked indexing and relaxed stores, nothing else.
    #[inline]
    pub fn stage_pair(&self, pair: usize, left: &[f32], right: &[f32], frames: usize) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        let (first, second) = (pair * 2, pair * 2 + 1);
        if second >= self.channels.load(Ordering::Acquire) as usize || second >= self.planes.len() {
            return;
        }
        let frames = frames.min(left.len()).min(right.len());
        if frames == 0 {
            return;
        }
        let write = self.write_frames.load(Ordering::Relaxed) as usize;
        let mut peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        for frame in 0..frames {
            let index = write.wrapping_add(frame) & MASK;
            self.planes[first][index].store(left[frame].to_bits(), Ordering::Relaxed);
            self.planes[second][index].store(right[frame].to_bits(), Ordering::Relaxed);
            peak = peak.max(left[frame].abs()).max(right[frame].abs());
        }
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
        self.staged_pairs.fetch_or(1 << pair, Ordering::Release);
    }

    /// Producer: close the staged block and publish it.
    ///
    /// Every pair nobody staged is zeroed first. Without that a track that
    /// stopped sharing would leave its last block looping in the stream, which
    /// sounds exactly like a stuck buffer and is far harder to diagnose than
    /// silence.
    #[inline]
    pub fn commit(&self, frames: usize, sample_rate: u32) {
        if !self.active.load(Ordering::Acquire) || frames == 0 {
            return;
        }
        let staged = self.staged_pairs.swap(0, Ordering::AcqRel);
        let write = self.write_frames.load(Ordering::Relaxed);
        let channels = (self.channels.load(Ordering::Acquire) as usize).min(self.planes.len());
        for pair in 0..channels / 2 {
            if staged & (1 << pair) != 0 {
                continue;
            }
            for channel in [pair * 2, pair * 2 + 1] {
                for frame in 0..frames {
                    let index = ((write as usize).wrapping_add(frame)) & MASK;
                    self.planes[channel][index].store(0, Ordering::Relaxed);
                }
            }
        }
        self.advance(write, frames, sample_rate);
    }

    /// Publish `frames` written at `write`, accounting for a consumer that fell
    /// behind.
    #[inline]
    fn advance(&self, write: u64, frames: usize, sample_rate: u32) {
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
        let read = self.read_frames.load(Ordering::Acquire);
        if write.saturating_sub(read) + frames as u64 > CAPACITY_FRAMES as u64 {
            // The publish thread is not keeping up. Dropping the oldest frames
            // is the realtime policy: the callback must not wait, and a jam
            // listener would rather lose a moment than hear everything late.
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
        self.write_frames
            .store(write.wrapping_add(frames as u64), Ordering::Release);
    }

    /// Consumer: take up to `max_frames` interleaved frames.
    ///
    /// Returns how many frames were written into `out`, how many channels each
    /// carries, and the session tick of the first of them when the clock base
    /// is known. The channel count is returned rather than read separately so a
    /// caller can never pair a frame count with the wrong layout.
    pub fn read_interleaved(
        &self,
        out: &mut Vec<f32>,
        max_frames: usize,
    ) -> Option<(usize, usize, Option<i64>)> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let channels = (self.channels.load(Ordering::Acquire) as usize)
            .clamp(1, self.planes.len())
            .max(1);
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
        out.reserve(ready * channels);
        for offset in 0..ready {
            let index = ((read as usize).wrapping_add(offset)) & MASK;
            for plane in self.planes.iter().take(channels) {
                out.push(f32::from_bits(plane[index].load(Ordering::Relaxed)));
            }
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
        Some((ready, channels, tick))
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

    fn claim(&self, channels: usize) {
        self.peak.store(0, Ordering::Relaxed);
        self.write_frames.store(0, Ordering::Relaxed);
        self.read_frames.store(0, Ordering::Relaxed);
        self.capture_base.store(0, Ordering::Relaxed);
        self.capture_known.store(false, Ordering::Relaxed);
        self.overruns.store(0, Ordering::Relaxed);
        self.staged_pairs.store(0, Ordering::Relaxed);
        // Even channel counts only: every producer here writes in pairs, and a
        // stream with a dangling channel has no source to fill it.
        let channels = (channels & !1).clamp(2, self.planes.len().max(2));
        self.channels
            .store(channels.min(self.planes.len()) as u32, Ordering::Relaxed);
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
    /// Whether the metronome click is part of the published master mix.
    ///
    /// It is by default, and that is what almost every room wants: a jam runs
    /// to a count, and a guest who cannot hear the click is playing to a mix
    /// that appears to have no pulse. It is a toggle rather than a constant
    /// because the same tap also serves an audience — someone listening to the
    /// room over the link is not playing along, and a click over the mix is the
    /// last thing they want.
    ///
    /// Read once per block on the realtime path, so it is an atomic rather than
    /// anything that would need a lock.
    master_click: AtomicBool,

    /// The master bus's publish slot, or `-1`.
    ///
    /// Resolved here rather than looked up by key in the callback: the key map
    /// is a `RwLock<HashMap<String, _>>`, and taking a lock and hashing a string
    /// once per block is exactly the kind of work the realtime path must not do.
    master_publish: AtomicI32,
    /// The live input pair's publish slot, or `-1`. Same reasoning as
    /// `master_publish`: resolved once on the control thread so the callback
    /// pays one load, never a key lookup.
    live_input_publish: AtomicI32,
    /// The Control Room / Monitor output's publish slot, or `-1`. Same
    /// reasoning again: one load on the callback, never a key lookup.
    monitor_publish: AtomicI32,
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
            // The stereo slots, then the one wide slot the multitrack stream
            // uses. Only the last carries the extra ring storage, so a Studio
            // that never shares an arrangement pays nothing for it beyond the
            // allocation itself.
            publishes: (0..TOTAL_JAM_PUBLISH_SLOTS)
                .map(|index| {
                    JamPublishSlot::with_capacity_channels(if index == MULTITRACK_PUBLISH_SLOT {
                        MAX_MULTITRACK_CHANNELS
                    } else {
                        2
                    })
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            input_keys: RwLock::new(HashMap::new()),
            publish_keys: RwLock::new(HashMap::new()),
            any_input: AtomicBool::new(false),
            any_publish: AtomicBool::new(false),
            master_click: AtomicBool::new(true),
            master_publish: AtomicI32::new(-1),
            live_input_publish: AtomicI32::new(-1),
            monitor_publish: AtomicI32::new(-1),
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

    /// The master bus's publish slot, if one is bound.
    ///
    /// One relaxed load, so the render callback can reach it without touching
    /// the key map.
    #[inline]
    pub fn master_publish(&self) -> Option<&JamPublishSlot> {
        let index = self.master_publish.load(Ordering::Acquire);
        if index < 0 {
            return None;
        }
        self.publishes.get(index as usize)
    }

    /// The live input pair's publish slot, if one is bound. One load.
    #[inline]
    pub fn live_input_publish(&self) -> Option<&JamPublishSlot> {
        let index = self.live_input_publish.load(Ordering::Acquire);
        if index < 0 {
            return None;
        }
        self.publishes.get(index as usize)
    }

    /// The Control Room / Monitor output's publish slot, if one is bound.
    ///
    /// One load, because the render callback asks after every block it has
    /// finished routing through the Control Room.
    #[inline]
    pub fn monitor_publish(&self) -> Option<&JamPublishSlot> {
        let index = self.monitor_publish.load(Ordering::Acquire);
        if index < 0 {
            return None;
        }
        self.publishes.get(index as usize)
    }

    /// Whether the published master mix carries the metronome click.
    ///
    /// One relaxed load per block on the render path.
    #[inline]
    pub fn master_click_published(&self) -> bool {
        self.master_click.load(Ordering::Relaxed)
    }

    /// Include or exclude the metronome click in the published master mix.
    /// Control thread; takes effect on the next block.
    pub fn set_master_click_published(&self, included: bool) {
        self.master_click.store(included, Ordering::Relaxed);
    }

    /// The multitrack publish slot, if the arrangement is being shared.
    ///
    /// Its index is a constant, so this costs one atomic load on the `active`
    /// flag and no key lookup at all.
    #[inline]
    pub fn multitrack_publish(&self) -> Option<&JamPublishSlot> {
        self.publishes
            .get(MULTITRACK_PUBLISH_SLOT)
            .filter(|slot| slot.is_active())
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
        if key == PUBLISH_KEY_MULTITRACK {
            return self.bind_multitrack_publish(2);
        }
        let mut keys = self.publish_keys.write().ok()?;
        if let Some(index) = keys.get(key) {
            return Some(*index);
        }
        // Only the stereo slots: the wide one is claimed by key, never handed
        // out to whatever asked for a slot first.
        let index = self.publishes[..MAX_JAM_PUBLISH_SLOTS]
            .iter()
            .position(|slot| !slot.active.load(Ordering::Acquire))?;
        self.publishes[index].claim(2);
        keys.insert(key.to_string(), index);
        if key == PUBLISH_KEY_MASTER {
            self.master_publish.store(index as i32, Ordering::Release);
        }
        if key == PUBLISH_KEY_LIVE_INPUT {
            self.live_input_publish
                .store(index as i32, Ordering::Release);
        }
        if key == PUBLISH_KEY_MONITOR {
            self.monitor_publish.store(index as i32, Ordering::Release);
        }
        self.any_publish.store(true, Ordering::Release);
        Some(index)
    }

    /// Claim the multitrack slot for `channels` channels.
    ///
    /// Control thread only, and the layout is fixed for the life of the claim:
    /// the channel count goes into the stream the server announces, so changing
    /// it mid-stream would mean every receiver decoding the new width with the
    /// old one's layout. Sharing a different set of tracks is a republish.
    ///
    /// Returns `None` when the slot is already claimed, so a caller cannot
    /// silently take over a stream that is live.
    pub fn bind_multitrack_publish(&self, channels: usize) -> Option<usize> {
        let mut keys = self.publish_keys.write().ok()?;
        if keys.contains_key(PUBLISH_KEY_MULTITRACK) {
            return None;
        }
        let slot = self.publishes.get(MULTITRACK_PUBLISH_SLOT)?;
        slot.claim(channels);
        keys.insert(PUBLISH_KEY_MULTITRACK.to_string(), MULTITRACK_PUBLISH_SLOT);
        self.any_publish.store(true, Ordering::Release);
        Some(MULTITRACK_PUBLISH_SLOT)
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
        if key == PUBLISH_KEY_MASTER {
            self.master_publish.store(-1, Ordering::Release);
        }
        if key == PUBLISH_KEY_LIVE_INPUT {
            self.live_input_publish.store(-1, Ordering::Release);
        }
        if key == PUBLISH_KEY_MONITOR {
            self.monitor_publish.store(-1, Ordering::Release);
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
        self.master_publish.store(-1, Ordering::Release);
        self.live_input_publish.store(-1, Ordering::Release);
        self.monitor_publish.store(-1, Ordering::Release);
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
        slot.assume_primed();
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
        slot.assume_primed();
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
        slot.assume_primed();
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

    /// The crackle, pinned. A block the network only half-filled used to be
    /// handed over half-full, which is a hole in the audio — and because the
    /// ring was then empty again, another hole in the next block, and the next.
    /// It is silence and a re-prime instead: one gap while the cushion rebuilds,
    /// then clean audio.
    #[test]
    fn a_partial_block_is_silence_and_a_re_prime_rather_than_a_hole() {
        let slot = JamInputSlot::default();
        slot.claim();
        slot.assume_primed();
        slot.write_interleaved(&[1.0, 1.0], 2, 0, 48_000);

        let mut left = vec![0.0; 4];
        let mut right = vec![0.0; 4];
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 4),
            0
        );
        assert_eq!(
            left,
            vec![0.0; 4],
            "a torn block is worse than a silent one"
        );
        assert_eq!(slot.underruns(), 1);
        assert!(
            slot.is_priming(),
            "an underrun must rebuild the cushion, not keep tearing"
        );
        assert_eq!(
            slot.available(),
            1,
            "the frame that did arrive is kept, not thrown away"
        );
    }

    /// Frames per call in the cushion tests, and a block of that many stereo
    /// frames of `value`.
    const BLOCK: usize = 128;

    fn stereo_block(frames: usize, value: f32) -> Vec<f32> {
        vec![value; frames * 2]
    }

    /// Fill `slot` until it has at least a cushion, writing in `BLOCK` chunks
    /// the way the receive thread does.
    fn fill_cushion(slot: &JamInputSlot, sample_rate: u32) {
        let target = target_frames(sample_rate, BLOCK);
        let mut position = 0u64;
        while slot.available() < target {
            slot.write_interleaved(&stereo_block(BLOCK, 1.0), 2, position, sample_rate);
            position += BLOCK as u64;
        }
    }

    /// A jam that starts playing the instant the first packet lands has no
    /// absorption at all: it is empty again by the next callback. The slot holds
    /// a cushion first, and while it is holding one it contributes silence
    /// rather than a trickle.
    #[test]
    fn a_fresh_stream_holds_a_cushion_before_it_plays() {
        let slot = JamInputSlot::default();
        slot.claim();
        let mut left = vec![0.0; BLOCK];
        let mut right = vec![0.0; BLOCK];

        slot.write_interleaved(&stereo_block(BLOCK, 1.0), 2, 0, 48_000);
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, BLOCK),
            0,
            "one packet is not a cushion"
        );
        assert_eq!(
            slot.underruns(),
            0,
            "priming is not lateness — those frames are early, not missing"
        );
        assert!(slot.is_priming());

        fill_cushion(&slot, 48_000);
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, BLOCK),
            BLOCK,
            "a full cushion plays whole blocks"
        );
        assert!(!slot.is_priming());
        assert_eq!(left[0], 1.0);
    }

    /// Steady state: as long as the producer keeps up, every block is whole and
    /// nothing is counted against the stream.
    #[test]
    fn a_stream_that_keeps_up_plays_every_block_whole() {
        let slot = JamInputSlot::default();
        slot.claim();
        fill_cushion(&slot, 48_000);
        let mut position = 1 << 20;

        for _ in 0..64 {
            let mut left = vec![0.0; BLOCK];
            let mut right = vec![0.0; BLOCK];
            assert_eq!(
                slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, BLOCK),
                BLOCK
            );
            slot.write_interleaved(&stereo_block(BLOCK, 1.0), 2, position, 48_000);
            position += BLOCK as u64;
        }
        assert_eq!(slot.underruns(), 0);
        assert_eq!(slot.overruns(), 0);
    }

    /// The network ran ahead, or the callback stalled. Playing further and
    /// further behind is worse than skipping: the frames skipped are already
    /// past their moment.
    #[test]
    fn latency_creep_is_resynced_forward_to_the_cushion() {
        let slot = JamInputSlot::default();
        slot.claim();
        fill_cushion(&slot, 48_000);
        let target = target_frames(48_000, BLOCK);

        // Four cushions' worth queued and nobody reading.
        let mut position = 1 << 20;
        while slot.available() < target * 4 {
            slot.write_interleaved(&stereo_block(BLOCK, 1.0), 2, position, 48_000);
            position += BLOCK as u64;
        }

        let mut left = vec![0.0; BLOCK];
        let mut right = vec![0.0; BLOCK];
        assert_eq!(
            slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, BLOCK),
            BLOCK
        );
        assert!(slot.overruns() >= 1, "the skip has to be visible");
        assert!(
            slot.available() <= target,
            "backlog must come back down to the cushion, not stay four deep"
        );
    }

    /// A cushion thinner than the thing it is cushioning is not one.
    #[test]
    fn the_cushion_is_never_thinner_than_two_callback_blocks() {
        for (rate, block) in [(48_000u32, 1024usize), (44_100, 2048), (96_000, 64)] {
            let target = target_frames(rate, block);
            assert!(
                target >= (block as u64) * 2,
                "rate={rate} block={block} target={target}"
            );
            assert!(target <= (CAPACITY_FRAMES / 4) as u64);
        }
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
            slot.assume_primed();
            slot.assume_primed();
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
        slot.assume_primed();
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
        slot.assume_primed();
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
        slot.assume_primed();
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
        slot.claim(2);
        slot.write_interleaved(&[0.1, 0.2, 0.3, 0.4], 2, 48_000);

        let mut out = Vec::new();
        let (frames, _channels, tick) = slot.read_interleaved(&mut out, 8).expect("frames ready");
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
        slot.claim(2);
        slot.write_interleaved(&[0.0; 4], 2, 48_000);
        slot.set_capture_base(20_000_000);
        let mut out = Vec::new();
        let (_, _channels, tick) = slot.read_interleaved(&mut out, 8).expect("frames ready");
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
    fn the_live_input_publish_slot_is_resolved_and_released_like_the_masters() {
        let bus = JamAudioBus::new();
        assert!(bus.live_input_publish().is_none());
        let index = bus.bind_publish(PUBLISH_KEY_LIVE_INPUT).expect("bound");
        let slot = bus.live_input_publish().expect("resolved");
        assert!(std::ptr::eq(slot, bus.publish(index).expect("indexed")));
        // The two taps are distinct slots: a send of the live input must never
        // read the master's ring or the other way round.
        let master = bus.bind_publish(PUBLISH_KEY_MASTER).expect("bound");
        assert_ne!(master, index);
        bus.release_publish(PUBLISH_KEY_LIVE_INPUT);
        assert!(bus.live_input_publish().is_none());
        assert!(bus.master_publish().is_some());
        bus.release_all();
        assert!(bus.master_publish().is_none());
    }

    #[test]
    fn the_master_publish_slot_is_reachable_without_touching_the_key_map() {
        let bus = JamAudioBus::new();
        assert!(bus.master_publish().is_none());

        let index = bus.bind_publish(PUBLISH_KEY_MASTER).expect("bound");
        let slot = bus.master_publish().expect("resolved");
        slot.write_interleaved(&[0.5, -0.5], 2, 48_000);
        assert_eq!(bus.publish(index).expect("in range").write_head(), 1);

        bus.release_publish(PUBLISH_KEY_MASTER);
        assert!(bus.master_publish().is_none());
    }

    #[test]
    fn the_multitrack_slot_carries_a_pair_per_track_in_one_block() {
        let bus = JamAudioBus::new();
        assert!(bus.multitrack_publish().is_none());

        let index = bus
            .bind_multitrack_publish(6)
            .expect("the wide slot is free");
        assert_eq!(index, MULTITRACK_PUBLISH_SLOT);
        let slot = bus.multitrack_publish().expect("claimed");
        assert_eq!(slot.channels(), 6, "three pairs");

        // Three tracks stage their post-fader blocks; one block is published.
        slot.stage_pair(0, &[0.1, 0.1], &[0.2, 0.2], 2);
        slot.stage_pair(1, &[0.3, 0.3], &[0.4, 0.4], 2);
        slot.stage_pair(2, &[0.5, 0.5], &[0.6, 0.6], 2);
        assert_eq!(
            slot.write_head(),
            0,
            "staging a pair does not publish the block on its own"
        );
        slot.commit(2, 48_000);
        assert_eq!(slot.write_head(), 2, "one head advance for the whole block");

        let mut out = Vec::new();
        let (frames, channels, _) = slot.read_interleaved(&mut out, 8).expect("frames ready");
        assert_eq!((frames, channels), (2, 6));
        assert_eq!(
            out[..6],
            [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            "channels are interleaved in pair order"
        );

        bus.release_publish(PUBLISH_KEY_MULTITRACK);
        assert!(bus.multitrack_publish().is_none());
    }

    /// A track that stops sharing must leave silence in its pair, not its last
    /// block repeating — a stuck buffer sounds like a bug in the receiver.
    #[test]
    fn an_unstaged_pair_is_published_as_silence_rather_than_the_previous_block() {
        let bus = JamAudioBus::new();
        bus.bind_multitrack_publish(4).expect("free");
        let slot = bus.multitrack_publish().expect("claimed");

        slot.stage_pair(0, &[1.0], &[1.0], 1);
        slot.stage_pair(1, &[0.5], &[0.5], 1);
        slot.commit(1, 48_000);

        // The second track stops sharing: only pair 0 is staged this block.
        slot.stage_pair(0, &[1.0], &[1.0], 1);
        slot.commit(1, 48_000);

        let mut out = Vec::new();
        let (frames, channels, _) = slot.read_interleaved(&mut out, 8).expect("frames ready");
        assert_eq!((frames, channels), (2, 4));
        assert_eq!(out[2..4], [0.5, 0.5], "the first block is unchanged");
        assert_eq!(out[6..8], [0.0, 0.0], "the second is silent, not repeated");
    }

    #[test]
    fn the_wide_slot_is_never_handed_out_to_an_ordinary_publish() {
        let bus = JamAudioBus::new();
        // Claim every stereo slot; the wide one must still be free, because a
        // master or track publish writing stereo into it would announce a
        // sixteen-channel stream carrying two channels of audio.
        for index in 0..MAX_JAM_PUBLISH_SLOTS {
            assert!(bus.bind_publish(&format!("track:{index}")).is_some());
        }
        assert!(bus.bind_publish("track:overflow").is_none());
        assert!(bus.bind_multitrack_publish(4).is_some());

        // And a second claim is refused rather than taking over a live stream.
        assert!(bus.bind_multitrack_publish(8).is_none());
    }

    #[test]
    fn the_published_master_carries_the_click_unless_it_is_switched_off() {
        let bus = JamAudioBus::new();
        assert!(
            bus.master_click_published(),
            "a jam runs to a count, so the click is in the mix by default"
        );
        bus.set_master_click_published(false);
        assert!(!bus.master_click_published());
    }

    #[test]
    fn a_planar_write_produces_the_same_frames_as_an_interleaved_one() {
        let planar = JamPublishSlot::default();
        planar.claim(2);
        planar.write_planar(&[0.25, 0.5], &[-0.25, -0.5], 2, 48_000);

        let interleaved = JamPublishSlot::default();
        interleaved.claim(2);
        interleaved.write_interleaved(&[0.25, -0.25, 0.5, -0.5], 2, 48_000);

        let mut from_planar = Vec::new();
        let mut from_interleaved = Vec::new();
        planar
            .read_interleaved(&mut from_planar, 8)
            .expect("frames ready");
        interleaved
            .read_interleaved(&mut from_interleaved, 8)
            .expect("frames ready");
        assert_eq!(from_planar, from_interleaved);
    }

    #[test]
    fn a_planar_write_is_bounded_by_the_shorter_buffer() {
        let slot = JamPublishSlot::default();
        slot.claim(2);
        // A caller that passes a frame count longer than its buffers must not
        // read past them; the shorter side wins.
        slot.write_planar(&[0.25], &[0.25, 0.5], 8, 48_000);
        let mut out = Vec::new();
        let (frames, _channels, _) = slot.read_interleaved(&mut out, 8).expect("frames ready");
        assert_eq!(frames, 1);
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
        slot.assume_primed();
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
        slot.assume_primed();
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
