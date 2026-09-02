//! Per-stream packet reordering and jitter absorption.
//!
//! One of these sits between the media receive thread and the audio bridge for
//! every stream being received. It is deliberately conservative: hold a small
//! number of packets, release them in sequence order, and report a gap rather
//! than inventing samples to cover one. Anything cleverer — adaptive depth from
//! measured jitter, packet loss concealment — is a later change that this shape
//! does not close the door on.
//!
//! Two properties matter more than the algorithm:
//!
//! * **Bounded.** The queue has a hard packet ceiling and a hard byte ceiling.
//!   A publisher that floods, or a consumer that stalls, costs a fixed amount
//!   of memory and then drops, rather than growing until the process dies.
//! * **Allocation-free in the steady state.** Payload buffers are recycled
//!   through a pool, so a jam that has been running for an hour is doing the
//!   same work as one that just started.

use std::collections::VecDeque;

use crate::packet::AudioPacketHeader;

/// Default depth, in packets. At 48 kHz with 128-sample frames each packet is
/// 2.7 ms, so three packets is about 8 ms of absorption — the studio/LAN case.
pub const DEFAULT_TARGET_PACKETS: usize = 3;

/// Hard ceiling on queued packets per stream. At the same frame size this is
/// roughly 170 ms; past that a receiver is not late, it is disconnected.
pub const MAX_QUEUED_PACKETS: usize = 64;

/// Hard ceiling on queued payload bytes per stream, so a stream negotiated at
/// 192 kHz stereo cannot use sixteen times the memory of a mono one.
pub const MAX_QUEUED_BYTES: usize = 512 * 1024;

/// One packet held for release.
#[derive(Debug)]
pub struct QueuedPacket {
    pub header: AudioPacketHeader,
    pub payload: Vec<u8>,
}

/// What a stream's arrival pattern looks like. Read by the UI at a throttled
/// rate; never on the audio thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub received: u64,
    pub released: u64,
    /// Packets that arrived after their slot had already been released.
    pub late: u64,
    /// Packets whose sequence had already been seen.
    pub duplicates: u64,
    /// Sequence numbers that never arrived.
    pub lost: u64,
    /// Packets discarded because the queue was full.
    pub overflowed: u64,
    /// Packets that arrived out of order but early enough to be re-sequenced.
    pub reordered: u64,
    /// Current queue depth in packets.
    pub depth: usize,
}

/// A per-stream reorder queue.
#[derive(Debug)]
pub struct JitterBuffer {
    /// Held packets, kept sorted by sequence.
    queue: VecDeque<QueuedPacket>,
    /// Recycled payload buffers.
    pool: Vec<Vec<u8>>,
    /// The next sequence expected out. `None` until the first packet arrives,
    /// so a stream that starts at sequence 900 000 is not treated as a
    /// 900 000-packet gap.
    next_sequence: Option<u64>,
    /// Generation of the publisher connection currently accepted. A packet from
    /// an older generation belongs to a socket that has already been replaced.
    generation: u32,
    target_packets: usize,
    queued_bytes: usize,
    stats: JitterStats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_TARGET_PACKETS)
    }
}

impl JitterBuffer {
    pub fn new(target_packets: usize) -> Self {
        let target = target_packets.clamp(1, MAX_QUEUED_PACKETS);
        Self {
            queue: VecDeque::with_capacity(MAX_QUEUED_PACKETS),
            pool: Vec::new(),
            next_sequence: None,
            generation: 0,
            target_packets: target,
            queued_bytes: 0,
            stats: JitterStats::default(),
        }
    }

    pub fn stats(&self) -> JitterStats {
        let mut stats = self.stats;
        stats.depth = self.queue.len();
        stats
    }

    pub fn target_packets(&self) -> usize {
        self.target_packets
    }

    /// Change the target depth. Clamped to the hard ceiling; a caller that asks
    /// for a second of buffering gets the maximum, not an unbounded queue.
    pub fn set_target_packets(&mut self, packets: usize) {
        self.target_packets = packets.clamp(1, MAX_QUEUED_PACKETS);
    }

    /// Accept one packet.
    ///
    /// Returns false when the packet was not queued — a duplicate, a late
    /// arrival, a stale generation, or an overflow. The reason is in the stats
    /// rather than in the return type because every caller does the same thing
    /// with it: nothing.
    pub fn push(&mut self, header: AudioPacketHeader, payload: &[u8]) -> bool {
        self.stats.received = self.stats.received.saturating_add(1);

        if header.generation < self.generation {
            // A packet from a socket that has already been replaced. Dropping
            // it is what keeps a dying transport from interleaving audio with
            // the connection that took over.
            self.stats.late = self.stats.late.saturating_add(1);
            return false;
        }
        if header.generation > self.generation {
            // The publisher reconnected. Everything held belongs to the old
            // connection and its sequence numbering restarts.
            self.reset_to_generation(header.generation);
        }

        if let Some(next) = self.next_sequence {
            if header.sequence < next {
                self.stats.late = self.stats.late.saturating_add(1);
                return false;
            }
        }
        if self
            .queue
            .iter()
            .any(|held| held.header.sequence == header.sequence)
        {
            self.stats.duplicates = self.stats.duplicates.saturating_add(1);
            return false;
        }
        if self.queue.len() >= MAX_QUEUED_PACKETS
            || self.queued_bytes.saturating_add(payload.len()) > MAX_QUEUED_BYTES
        {
            // Realtime drop policy: shed the oldest, because it is the packet
            // most likely to be past its moment already. Growing instead would
            // trade a dropout now for an out-of-memory later.
            if let Some(dropped) = self.queue.pop_front() {
                self.queued_bytes = self.queued_bytes.saturating_sub(dropped.payload.len());
                self.next_sequence = Some(dropped.header.sequence + 1);
                self.recycle(dropped.payload);
            }
            self.stats.overflowed = self.stats.overflowed.saturating_add(1);
        }

        let mut buffer = self.pool.pop().unwrap_or_default();
        buffer.clear();
        buffer.extend_from_slice(payload);
        self.queued_bytes = self.queued_bytes.saturating_add(buffer.len());

        // Insert in sequence order. The queue is small and almost always
        // already sorted, so a linear scan from the back is cheaper than a heap
        // and keeps the packets contiguous for the release path.
        let position = self
            .queue
            .iter()
            .rposition(|held| held.header.sequence < header.sequence)
            .map(|index| index + 1)
            .unwrap_or(0);
        if position != self.queue.len() {
            self.stats.reordered = self.stats.reordered.saturating_add(1);
        }
        self.queue.insert(
            position,
            QueuedPacket {
                header,
                payload: buffer,
            },
        );
        true
    }

    /// Release the next packet, if one is due.
    ///
    /// Nothing is released until the queue has reached its target depth, which
    /// is what absorbs the jitter. Once it has, a missing sequence is skipped
    /// rather than waited for: the packet is not coming, and stalling would
    /// turn one lost datagram into a growing delay for everything behind it.
    pub fn pop(&mut self) -> Option<QueuedPacket> {
        if self.queue.len() < self.target_packets {
            return None;
        }
        let packet = self.queue.pop_front()?;
        self.queued_bytes = self.queued_bytes.saturating_sub(packet.payload.len());

        if let Some(expected) = self.next_sequence {
            if packet.header.sequence > expected {
                self.stats.lost = self
                    .stats
                    .lost
                    .saturating_add(packet.header.sequence - expected);
            }
        }
        self.next_sequence = Some(packet.header.sequence + 1);
        self.stats.released = self.stats.released.saturating_add(1);
        Some(packet)
    }

    /// Release everything held, in order, ignoring the target depth. Used when
    /// a stream ends so the tail is not left in the queue.
    pub fn drain(&mut self) -> Vec<QueuedPacket> {
        let mut out = Vec::with_capacity(self.queue.len());
        while let Some(packet) = self.queue.pop_front() {
            self.queued_bytes = self.queued_bytes.saturating_sub(packet.payload.len());
            self.next_sequence = Some(packet.header.sequence + 1);
            self.stats.released = self.stats.released.saturating_add(1);
            out.push(packet);
        }
        out
    }

    /// Hand a released packet's buffer back for reuse.
    pub fn recycle(&mut self, mut buffer: Vec<u8>) {
        if self.pool.len() < MAX_QUEUED_PACKETS {
            buffer.clear();
            self.pool.push(buffer);
        }
    }

    /// Forget everything and follow a new publisher generation.
    pub fn reset_to_generation(&mut self, generation: u32) {
        while let Some(packet) = self.queue.pop_front() {
            self.recycle(packet.payload);
        }
        self.queued_bytes = 0;
        self.next_sequence = None;
        self.generation = generation;
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AudioCodec;

    fn packet(sequence: u64) -> AudioPacketHeader {
        AudioPacketHeader {
            version: 1,
            codec: AudioCodec::Pcm,
            flags: 0,
            jam_alias: 1,
            stream_alias: 1,
            generation: 0,
            sequence,
            capture_timestamp: sequence * 128,
            frames: 128,
            channels: 2,
        }
    }

    fn push(buffer: &mut JitterBuffer, sequence: u64) -> bool {
        buffer.push(packet(sequence), &[0u8; 16])
    }

    #[test]
    fn nothing_is_released_until_the_target_depth_is_reached() {
        let mut buffer = JitterBuffer::new(3);
        push(&mut buffer, 0);
        push(&mut buffer, 1);
        assert!(buffer.pop().is_none());
        push(&mut buffer, 2);
        assert_eq!(buffer.pop().expect("due").header.sequence, 0);
    }

    #[test]
    fn out_of_order_arrivals_are_re_sequenced() {
        let mut buffer = JitterBuffer::new(2);
        push(&mut buffer, 0);
        push(&mut buffer, 2);
        // 1 overtook 2 on the way here and is still early enough to be placed.
        push(&mut buffer, 1);
        assert_eq!(buffer.pop().expect("due").header.sequence, 0);
        assert_eq!(buffer.pop().expect("due").header.sequence, 1);
        assert_eq!(buffer.stats().reordered, 1);
        assert_eq!(buffer.stats().lost, 0);
    }

    #[test]
    fn the_queue_only_releases_while_it_stays_at_the_target_depth() {
        // Releasing below the target would spend the absorption the buffer
        // exists to provide, so a burst of three at a target of three yields
        // exactly one packet until more arrive.
        let mut buffer = JitterBuffer::new(3);
        push(&mut buffer, 0);
        push(&mut buffer, 1);
        push(&mut buffer, 2);
        assert_eq!(buffer.pop().expect("due").header.sequence, 0);
        assert!(buffer.pop().is_none());
        push(&mut buffer, 3);
        assert_eq!(buffer.pop().expect("due").header.sequence, 1);
    }

    #[test]
    fn a_missing_sequence_is_counted_as_loss_and_does_not_stall_the_stream() {
        let mut buffer = JitterBuffer::new(2);
        push(&mut buffer, 0);
        push(&mut buffer, 1);
        assert_eq!(buffer.pop().expect("due").header.sequence, 0);
        // 2 never arrives.
        push(&mut buffer, 3);
        assert_eq!(buffer.pop().expect("due").header.sequence, 1);
        push(&mut buffer, 4);
        assert_eq!(buffer.pop().expect("due").header.sequence, 3);
        assert_eq!(buffer.stats().lost, 1);
    }

    #[test]
    fn a_packet_that_arrives_after_its_slot_is_dropped_not_played_late() {
        let mut buffer = JitterBuffer::new(1);
        push(&mut buffer, 0);
        push(&mut buffer, 1);
        assert_eq!(buffer.pop().expect("due").header.sequence, 0);
        assert_eq!(buffer.pop().expect("due").header.sequence, 1);
        // 0 turns up again from a redundant path, long after it mattered.
        assert!(!push(&mut buffer, 0));
        assert_eq!(buffer.stats().late, 1);
    }

    #[test]
    fn duplicates_are_recognised_while_still_queued() {
        let mut buffer = JitterBuffer::new(4);
        push(&mut buffer, 5);
        assert!(!push(&mut buffer, 5));
        assert_eq!(buffer.stats().duplicates, 1);
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn the_queue_is_bounded_and_sheds_the_oldest_packet() {
        let mut buffer = JitterBuffer::new(MAX_QUEUED_PACKETS);
        for sequence in 0..(MAX_QUEUED_PACKETS as u64 + 10) {
            push(&mut buffer, sequence);
        }
        assert!(buffer.len() <= MAX_QUEUED_PACKETS);
        assert!(buffer.stats().overflowed >= 10);
        // The packets kept are the newest ones.
        assert_eq!(buffer.pop().expect("due").header.sequence, 10);
    }

    #[test]
    fn a_large_payload_is_bounded_by_bytes_as_well_as_by_count() {
        let mut buffer = JitterBuffer::new(MAX_QUEUED_PACKETS);
        let big = vec![0u8; 64 * 1024];
        for sequence in 0..16u64 {
            buffer.push(packet(sequence), &big);
        }
        assert!(buffer.len() * big.len() <= MAX_QUEUED_BYTES + big.len());
        assert!(buffer.stats().overflowed > 0);
    }

    #[test]
    fn a_new_publisher_generation_discards_the_old_connections_audio() {
        let mut buffer = JitterBuffer::new(2);
        push(&mut buffer, 100);
        push(&mut buffer, 101);

        let mut header = packet(0);
        header.generation = 1;
        buffer.push(header, &[0u8; 16]);
        assert_eq!(buffer.generation(), 1);
        assert_eq!(buffer.len(), 1);

        // And a straggler from the old generation is refused.
        assert!(!push(&mut buffer, 102));
    }

    #[test]
    fn a_stream_that_starts_at_a_high_sequence_is_not_treated_as_a_huge_gap() {
        let mut buffer = JitterBuffer::new(1);
        push(&mut buffer, 900_000);
        buffer.pop().expect("due");
        assert_eq!(buffer.stats().lost, 0);
    }

    #[test]
    fn draining_releases_the_tail_in_order() {
        let mut buffer = JitterBuffer::new(8);
        push(&mut buffer, 1);
        push(&mut buffer, 0);
        let drained = buffer.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].header.sequence, 0);
        assert_eq!(drained[1].header.sequence, 1);
        assert!(buffer.is_empty());
    }

    #[test]
    fn recycled_buffers_are_reused_instead_of_reallocated() {
        let mut buffer = JitterBuffer::new(1);
        push(&mut buffer, 0);
        let packet = buffer.pop().expect("due");
        let capacity = packet.payload.capacity();
        buffer.recycle(packet.payload);
        push(&mut buffer, 1);
        let reused = buffer.pop().expect("due");
        assert!(reused.payload.capacity() >= capacity);
    }
}
