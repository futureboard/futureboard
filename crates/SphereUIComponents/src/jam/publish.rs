//! Studio audio out to the jam.
//!
//! ```txt
//! audio callback ──▶ JamPublishSlot ──▶ JamEngineSource ──▶ SRC ──▶ packets
//! ```
//!
//! The tap is on something the engine is already rendering — the master bus
//! today, and the same shape serves a track, a bus or a hardware input — so a
//! jam never opens a capture client of its own and never competes with the DAW
//! for the device.
//!
//! Two rates are in play and they stay separate. The project renders at
//! whatever rate the session was set up at; the jam publishes at the rate it
//! negotiated. The conversion lives here, on the jam branch, and the project
//! rate is never changed to suit the network.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use sphere_jam_client::bridge::{JamPublishSource, PulledBlock};
use sphere_jam_client::clock::{self, SessionClock};
use sphere_jam_client::protocol::LatencyMetadata;
use DirectAudio::engine::SharedState;

use super::resample::JamResampler;

/// Pulls from one engine publish slot and converts it to the jam's rate.
pub struct JamEngineSource {
    shared: Arc<SharedState>,
    clock: Arc<Mutex<SessionClock>>,
    /// The slot key, e.g. `master`. Resolved per pull, so a slot rebound after
    /// a reconnect is picked up without rebuilding the source.
    key: String,
    /// The rate this stream was published at.
    publish_rate: u32,
    state: Mutex<SourceState>,
}

struct SourceState {
    resampler: Option<JamResampler>,
    raw: Vec<f32>,
    converted: Vec<f32>,
    /// The engine rate the current resampler was built for.
    engine_rate: u32,
    /// The channel count it was built for. A slot's layout is fixed for the
    /// life of a claim, so this changes only when a stream is republished —
    /// at which point the converter has to be rebuilt, because `rubato` fixes
    /// its channel count at construction.
    channels: usize,
    /// Whether the next block starts a take.
    take_start: bool,
    drift_ppm: f64,
}

impl JamEngineSource {
    pub fn new(
        shared: Arc<SharedState>,
        clock: Arc<Mutex<SessionClock>>,
        key: impl Into<String>,
        publish_rate: u32,
    ) -> Self {
        Self {
            shared,
            clock,
            key: key.into(),
            publish_rate: publish_rate.max(1),
            state: Mutex::new(SourceState {
                resampler: None,
                raw: Vec::new(),
                converted: Vec::new(),
                engine_rate: 0,
                channels: 0,
                take_start: true,
                drift_ppm: 0.0,
            }),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    fn engine_rate(&self) -> u32 {
        let rate = self.shared.sample_rate.load(Ordering::Relaxed);
        if rate == 0 {
            48_000
        } else {
            rate
        }
    }

    /// Anchor the tap's ring to the jam's session clock.
    ///
    /// Run before the read, so the position the ring reports for the frames
    /// just taken is already in session ticks. Until the clock locks nothing is
    /// published — a guessed anchor would place a receiver's recorded take at
    /// an arbitrary point on its timeline, which is worse than the receiver
    /// knowing the position is unknown.
    fn anchor_clock(&self, slot: &DirectAudio::JamPublishSlot, engine_rate: u32) {
        let Ok(clock) = self.clock.lock() else {
            return;
        };
        if !clock.locked() {
            return;
        }
        let Some(now_ticks) = clock.session_ticks_at(clock::client_nanos()) else {
            return;
        };
        // The ring's write head is "now"; frame zero is that many frames
        // earlier, converted into the tick domain.
        let head = slot.write_head() as i64;
        let behind = clock.project_samples_to_ticks(head, engine_rate);
        slot.set_capture_base(now_ticks - behind);
    }
}

impl JamPublishSource for JamEngineSource {
    fn pull(&self, out: &mut Vec<f32>, max_frames: usize) -> Option<PulledBlock> {
        let slot_index = self.shared.jam_bus.publish_slot_for(&self.key)?;
        let slot = self.shared.jam_bus.publish(slot_index)?;
        let engine_rate = self.engine_rate();

        let channels = slot.channels().max(1);

        let mut state = self.state.lock().ok()?;
        if state.resampler.is_none()
            || state.engine_rate != engine_rate
            || state.channels != channels
        {
            state.resampler = Some(JamResampler::with_channels(
                engine_rate,
                self.publish_rate,
                channels,
            ));
            state.engine_rate = engine_rate;
            state.channels = channels;
            // A rebuilt converter is a new take: its history is gone, so the
            // first block after it is genuinely the start of one and marking it
            // is what tells a receiver to re-anchor rather than splice.
            state.take_start = true;
        }

        // Follow the measured drift, so a Studio that runs a few parts per
        // million fast does not slowly overrun every listener's buffer.
        if let Ok(clock) = self.clock.lock() {
            if clock.locked() {
                let drift = clock.drift_ppm();
                if (drift - state.drift_ppm).abs() > 0.5 {
                    state.drift_ppm = drift;
                    if let Some(resampler) = state.resampler.as_mut() {
                        resampler.set_drift_ppm(drift);
                    }
                }
            }
        }
        self.anchor_clock(slot, engine_rate);

        // Ask the ring for the number of engine frames that will produce about
        // `max_frames` after conversion. Overshooting would build a backlog the
        // resampler has to carry; undershooting is picked up on the next pull.
        let wanted =
            (max_frames as u128 * engine_rate as u128 / self.publish_rate as u128).max(1) as usize;

        let position = {
            let raw = &mut state.raw;
            let (frames, read_channels, position) = slot.read_interleaved(raw, wanted)?;
            if frames == 0 || read_channels != channels {
                // The layout changed between the two reads, which only happens
                // across a republish. Dropping this block is right: converting
                // it against the old layout would interleave the wrong channels
                // into every frame of it.
                return None;
            }
            position
        };

        let SourceState {
            resampler,
            raw,
            converted,
            ..
        } = &mut *state;
        let resampler = resampler.as_mut()?;
        resampler.process(raw, converted);
        if converted.is_empty() {
            return None;
        }

        out.clear();
        out.extend_from_slice(converted);
        let produced = converted.len() / channels;

        let take_start = state.take_start;
        state.take_start = false;

        // The ring reports the capture position of the frames just taken, in
        // session ticks. `None` means the jam clock has not locked yet; a zero
        // there would claim these samples were captured at the very start of
        // the session, so it is reported as tick zero only because the protocol
        // has no "unknown", and the accompanying latency metadata says the
        // clock offset is unmeasured.
        let capture_ticks = position.unwrap_or(0).max(0) as u64;

        Some(PulledBlock {
            frames: produced,
            capture_ticks,
            take_start,
        })
    }

    fn latency(&self) -> LatencyMetadata {
        let codec_delay = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.resampler.as_ref().map(|r| r.delay_samples()))
            .unwrap_or(0);
        let (offset_ticks, drift_ppm) = match self.clock.lock() {
            Ok(clock) if clock.locked() => (clock.offset_ticks(), clock.drift_ppm()),
            // Unmeasured, and reported as such. A guessed offset moves a
            // receiver's recorded waveform; a zero merely fails to move it.
            _ => (0, 0.0),
        };
        LatencyMetadata {
            // A master tap has no capture device in front of it, so there is no
            // driver-reported input latency and no capture buffer to declare.
            // Both stay zero rather than being filled with a plausible number
            // nobody measured.
            input_latency_samples: 0,
            capture_buffer_samples: 0,
            codec_delay_samples: codec_delay,
            clock_offset_ticks: offset_ticks,
            clock_drift_ppm: drift_ppm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DirectAudio::jam_bus::PUBLISH_KEY_MASTER;

    fn source(publish_rate: u32) -> (Arc<SharedState>, JamEngineSource) {
        let shared = Arc::new(SharedState::default());
        shared.sample_rate.store(48_000, Ordering::Relaxed);
        let clock = Arc::new(Mutex::new(SessionClock::new(48_000)));
        let source =
            JamEngineSource::new(Arc::clone(&shared), clock, PUBLISH_KEY_MASTER, publish_rate);
        (shared, source)
    }

    #[test]
    fn an_unbound_slot_produces_nothing_rather_than_silence() {
        let (_shared, source) = source(48_000);
        let mut out = Vec::new();
        assert!(source.pull(&mut out, 256).is_none());
        assert!(out.is_empty());
    }

    #[test]
    fn the_callbacks_master_output_comes_back_out_of_the_source() {
        let (shared, source) = source(48_000);
        let index = shared
            .jam_bus
            .bind_publish(PUBLISH_KEY_MASTER)
            .expect("a slot is free");
        let block: Vec<f32> = (0..256).flat_map(|_| [0.25f32, -0.25]).collect();
        shared
            .jam_bus
            .publish(index)
            .expect("in range")
            .write_interleaved(&block, 2, 48_000);

        let mut out = Vec::new();
        let pulled = source.pull(&mut out, 256).expect("frames ready");
        assert_eq!(pulled.frames, 256);
        assert!(pulled.take_start, "the first block of a take is marked");
        assert_eq!(out.len(), 512);
        assert!((out[0] - 0.25).abs() < 1e-6);
        assert!((out[1] + 0.25).abs() < 1e-6);

        shared
            .jam_bus
            .publish(index)
            .expect("in range")
            .write_interleaved(&block, 2, 48_000);
        let pulled = source.pull(&mut out, 256).expect("frames ready");
        assert!(!pulled.take_start);
    }

    #[test]
    fn publishing_at_a_different_rate_converts_rather_than_changing_the_project() {
        let (shared, source) = source(48_000);
        // A 96 kHz project publishing a 48 kHz jam stream.
        shared.sample_rate.store(96_000, Ordering::Relaxed);
        let index = shared
            .jam_bus
            .bind_publish(PUBLISH_KEY_MASTER)
            .expect("a slot is free");

        let mut produced = 0usize;
        let mut out = Vec::new();
        for _ in 0..16 {
            let block: Vec<f32> = (0..512)
                .flat_map(|i| {
                    let sample = (i as f32 * 0.01).sin() * 0.5;
                    [sample, sample]
                })
                .collect();
            shared
                .jam_bus
                .publish(index)
                .expect("in range")
                .write_interleaved(&block, 2, 96_000);
            if let Some(pulled) = source.pull(&mut out, 512) {
                produced += pulled.frames;
            }
        }
        // 16 × 512 frames at 96 kHz is 8192; at 48 kHz that is about 4096.
        assert!(
            (4096i64 - produced as i64).abs() < 700,
            "produced {produced} frames"
        );
        // And the project rate is untouched.
        assert_eq!(shared.sample_rate.load(Ordering::Relaxed), 96_000);
    }

    #[test]
    fn latency_metadata_reports_only_what_was_measured() {
        let (_shared, source) = source(48_000);
        let latency = source.latency();
        assert_eq!(
            latency.input_latency_samples, 0,
            "a master tap has no capture device in front of it"
        );
        assert_eq!(
            latency.clock_offset_ticks, 0,
            "an unlocked clock reports no offset rather than a guess"
        );
        assert_eq!(latency.clock_drift_ppm, 0.0);
    }

    #[test]
    fn a_locked_clock_anchors_the_tap_so_packets_carry_a_session_tick() {
        let (shared, source) = source(48_000);
        {
            let mut clock = source.clock.lock().expect("clock");
            clock.apply(
                clock::measure(0, 0, 0, 0),
                20_000_000,
                clock::client_nanos(),
            );
            assert!(clock.locked());
        }
        let index = shared
            .jam_bus
            .bind_publish(PUBLISH_KEY_MASTER)
            .expect("a slot is free");
        let block: Vec<f32> = (0..256).flat_map(|_| [0.1f32, 0.1]).collect();
        shared
            .jam_bus
            .publish(index)
            .expect("in range")
            .write_interleaved(&block, 2, 48_000);

        let mut out = Vec::new();
        let pulled = source.pull(&mut out, 256).expect("frames ready");
        assert!(
            pulled.capture_ticks > 19_000_000,
            "capture tick was {}",
            pulled.capture_ticks
        );
    }
}
