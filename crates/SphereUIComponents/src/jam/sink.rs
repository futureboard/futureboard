//! Remote audio into the engine.
//!
//! ```txt
//! jam receive thread ──▶ JamEngineSink ──▶ SRC ──▶ JamAudioBus slot ──▶ track
//! ```
//!
//! Everything in this file runs on the jam receive thread. Nothing here is
//! called from the audio callback, which is why it is allowed to hold a lock
//! and run a sinc filter: the only thing it hands the realtime side is atomic
//! stores into a preallocated ring.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use sphere_jam_client::bridge::{JamAudioFrame, JamAudioSink};
use sphere_jam_client::clock::SessionClock;
use sphere_jam_client::ids::StreamId;
use DirectAudio::engine::SharedState;

use super::resample::JamResampler;

/// Per-stream conversion state. One of these exists for each stream this client
/// is actually receiving.
struct StreamBridge {
    slot: usize,
    resampler: JamResampler,
    /// Interleaved stereo scratch, reused so the steady state allocates
    /// nothing.
    scratch: Vec<f32>,
    converted: Vec<f32>,
    /// The last drift the resampler was told about, so the ratio is only
    /// touched when the estimate actually moves.
    drift_ppm: f64,
}

/// Writes decoded remote audio into the engine's jam bus.
pub struct JamEngineSink {
    /// The engine's shared state, which carries the jam bus the audio callback
    /// already reaches through its own `Arc`.
    shared: Arc<SharedState>,
    /// The jam's session clock, for converting a publisher's capture tick into
    /// a position on this engine's own sample clock.
    clock: Arc<Mutex<SessionClock>>,
    streams: Mutex<HashMap<StreamId, StreamBridge>>,
}

impl JamEngineSink {
    pub fn new(shared: Arc<SharedState>, clock: Arc<Mutex<SessionClock>>) -> Self {
        Self {
            shared,
            clock,
            streams: Mutex::new(HashMap::new()),
        }
    }

    /// The engine's current device rate. Remote audio is converted into it; the
    /// project rate never follows the jam.
    fn engine_rate(&self) -> u32 {
        let rate = self.shared.sample_rate.load(Ordering::Relaxed);
        if rate == 0 {
            48_000
        } else {
            rate
        }
    }

    /// Release everything. Called when a jam ends, so a project left open
    /// afterwards holds no slots and no filter state.
    pub fn clear(&self) {
        if let Ok(mut streams) = self.streams.lock() {
            for (id, _) in streams.drain() {
                self.shared.jam_bus.release_input(id.as_str());
            }
        }
    }

    /// Which bus slot a stream is using, for the panel and for diagnostics.
    pub fn slot_for(&self, stream: &StreamId) -> Option<usize> {
        self.shared.jam_bus.input_slot_for(stream.as_str())
    }
}

impl JamAudioSink for JamEngineSink {
    fn deliver(&self, stream: &StreamId, frame: JamAudioFrame<'_>) {
        if frame.frames == 0 || frame.channels == 0 {
            return;
        }
        let engine_rate = self.engine_rate();
        let Ok(mut streams) = self.streams.lock() else {
            return;
        };

        // Bind on first audio. The route may have claimed the slot already, in
        // which case this finds it rather than taking a second one.
        let bridge = match streams.get_mut(stream) {
            Some(bridge) => bridge,
            None => {
                let Some(slot) = self.shared.jam_bus.bind_input(stream.as_str()) else {
                    // Every slot is taken. Dropping the audio is honest; the
                    // panel shows the stream as unrouted.
                    return;
                };
                streams.insert(
                    stream.clone(),
                    StreamBridge {
                        slot,
                        resampler: JamResampler::new(frame.sample_rate, engine_rate),
                        scratch: Vec::with_capacity(frame.frames * 2),
                        converted: Vec::with_capacity(frame.frames * 4),
                        drift_ppm: 0.0,
                    },
                );
                streams.get_mut(stream).expect("just inserted")
            }
        };

        // A rate change means the publisher republished at a different format.
        // Rebuilding is correct — the filter's state belongs to the old rate.
        if bridge.resampler.source_rate() != frame.sample_rate
            || bridge.resampler.target_rate() != engine_rate
        {
            bridge.resampler = JamResampler::new(frame.sample_rate, engine_rate);
        }

        // Fold the arriving layout to stereo once, here, so the ring and the
        // audio callback only ever deal with two channels.
        bridge.scratch.clear();
        bridge.scratch.reserve(frame.frames * 2);
        for index in 0..frame.frames {
            let at = index * frame.channels;
            let left = frame.samples.get(at).copied().unwrap_or(0.0);
            let right = if frame.channels >= 2 {
                frame.samples.get(at + 1).copied().unwrap_or(left)
            } else {
                left
            };
            bridge.scratch.push(left);
            bridge.scratch.push(right);
        }

        // Clock drift and the tick-to-sample mapping both come from the jam
        // clock. Until it locks, audio still plays — it just has no position on
        // the project timeline yet, which the slot reports as unknown rather
        // than as zero.
        let (clock_rate, drift_ppm, locked) = match self.clock.lock() {
            Ok(clock) => (clock.rate(), clock.drift_ppm(), clock.locked()),
            Err(_) => (48_000, 0.0, false),
        };
        if locked && (drift_ppm - bridge.drift_ppm).abs() > 0.5 {
            bridge.drift_ppm = drift_ppm;
            bridge.resampler.set_drift_ppm(drift_ppm);
        }

        let converted = {
            let StreamBridge {
                resampler,
                scratch,
                converted,
                ..
            } = bridge;
            resampler.process(scratch, converted);
            converted
        };
        if converted.is_empty() {
            return;
        }

        // The capture instant, converted from the jam's tick domain into this
        // engine's sample clock. This is what a recorder aligns against, and it
        // is the publisher's own timestamp — never when the packet arrived.
        let capture_position = if clock_rate == 0 {
            frame.capture_timestamp
        } else {
            (frame.capture_timestamp as u128 * engine_rate as u128 / clock_rate as u128) as u64
        };

        if let Some(slot) = self.shared.jam_bus.input(bridge.slot) {
            slot.write_interleaved(converted, 2, capture_position, engine_rate);
        }
    }

    fn stream_ended(&self, stream: &StreamId) {
        if let Ok(mut streams) = self.streams.lock() {
            streams.remove(stream);
        }
        self.shared.jam_bus.release_input(stream.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sphere_jam_client::clock::{measure, SessionClock};
    use sphere_jam_client::protocol::LatencyMetadata;

    fn sink() -> (Arc<SharedState>, Arc<Mutex<SessionClock>>, JamEngineSink) {
        let shared = Arc::new(SharedState::default());
        shared.sample_rate.store(48_000, Ordering::Relaxed);
        let clock = Arc::new(Mutex::new(SessionClock::new(48_000)));
        let sink = JamEngineSink::new(Arc::clone(&shared), Arc::clone(&clock));
        (shared, clock, sink)
    }

    fn frame<'a>(samples: &'a [f32], rate: u32, capture: u64) -> JamAudioFrame<'a> {
        JamAudioFrame {
            capture_timestamp: capture,
            presentation_timestamp: capture,
            sequence: 0,
            frames: samples.len() / 2,
            channels: 2,
            sample_rate: rate,
            flags: 0,
            samples,
            latency: LatencyMetadata::default(),
        }
    }

    #[test]
    fn delivered_audio_reaches_the_engine_bus() {
        let (shared, _clock, sink) = sink();
        let id = StreamId::new("str_1");
        let samples: Vec<f32> = (0..128).flat_map(|_| [0.5f32, -0.5]).collect();

        sink.deliver(&id, frame(&samples, 48_000, 1_000));

        let slot_index = sink.slot_for(&id).expect("bound on first audio");
        let slot = shared.jam_bus.input(slot_index).expect("in range");
        assert!(slot.is_active());
        assert_eq!(slot.available(), 128);
        assert_eq!(slot.sample_rate(), 48_000);
    }

    #[test]
    fn the_capture_position_is_converted_into_the_engines_own_sample_clock() {
        let (shared, _clock, sink) = sink();
        shared.sample_rate.store(96_000, Ordering::Relaxed);
        let id = StreamId::new("str_1");
        let samples: Vec<f32> = (0..512).flat_map(|_| [0.25f32, 0.25]).collect();

        // Captured at session tick 48 000, which is one second in. At a 96 kHz
        // engine rate that is sample 96 000.
        sink.deliver(&id, frame(&samples, 48_000, 48_000));

        let slot_index = sink.slot_for(&id).expect("bound");
        let slot = shared.jam_bus.input(slot_index).expect("in range");
        assert_eq!(slot.next_capture_position(), Some(96_000));
    }

    #[test]
    fn a_stream_that_ends_frees_its_slot() {
        let (shared, _clock, sink) = sink();
        let id = StreamId::new("str_1");
        let samples = [0.5f32; 256];
        sink.deliver(&id, frame(&samples, 48_000, 0));
        assert!(sink.slot_for(&id).is_some());

        sink.stream_ended(&id);
        assert!(sink.slot_for(&id).is_none());
        assert!(!shared.jam_bus.has_inputs());
    }

    #[test]
    fn a_mono_stream_is_widened_before_it_reaches_the_ring() {
        let (shared, _clock, sink) = sink();
        let id = StreamId::new("str_mono");
        let samples = vec![0.75f32; 128];
        let mut mono = frame(&samples, 48_000, 0);
        mono.channels = 1;
        mono.frames = samples.len();

        sink.deliver(&id, mono);

        let slot_index = sink.slot_for(&id).expect("bound");
        let slot = shared.jam_bus.input(slot_index).expect("in range");
        let mut left = vec![0.0; 4];
        let mut right = vec![0.0; 4];
        slot.mix_into(
            DirectAudio::JamChannelMode::Stereo,
            &mut left,
            &mut right,
            4,
        );
        assert_eq!(left, vec![0.75; 4]);
        assert_eq!(right, vec![0.75; 4]);
    }

    #[test]
    fn a_rate_mismatch_is_converted_rather_than_played_at_the_wrong_speed() {
        let (shared, _clock, sink) = sink();
        let id = StreamId::new("str_1");
        // 96 kHz publisher into a 48 kHz engine: half as many frames come out.
        let samples: Vec<f32> = (0..2048).flat_map(|_| [0.1f32, 0.1]).collect();

        sink.deliver(&id, frame(&samples, 96_000, 0));

        let slot_index = sink.slot_for(&id).expect("bound");
        let slot = shared.jam_bus.input(slot_index).expect("in range");
        let produced = slot.available();
        assert!(
            (1024i64 - produced as i64).abs() < 300,
            "produced {produced} frames from 2048 at half the rate"
        );
        assert_eq!(slot.sample_rate(), 48_000);
    }

    #[test]
    fn a_locked_clock_moves_the_resampler_ratio_rather_than_dropping_samples() {
        let (_shared, clock, sink) = sink();
        {
            let mut clock = clock.lock().expect("clock");
            // Ten exchanges with the server pulling steadily ahead: a real
            // drift, not a single bad sample.
            for i in 0..10i64 {
                clock.apply(
                    measure(0, i * 100_000, i * 100_000, 0),
                    0,
                    i * 1_000_000_000,
                );
            }
            assert!(clock.locked());
            assert!(clock.drift_ppm().abs() > 1.0);
        }

        let id = StreamId::new("str_1");
        let samples: Vec<f32> = (0..1024).flat_map(|_| [0.1f32, 0.1]).collect();
        // A converting stream, so drift correction is meaningful.
        sink.deliver(&id, frame(&samples, 96_000, 0));

        let streams = sink.streams.lock().expect("streams");
        let bridge = streams.get(&id).expect("bridged");
        assert!(
            bridge.resampler.drift_ppm().abs() > 0.0,
            "the measured drift never reached the resampler"
        );
    }

    #[test]
    fn clearing_the_sink_releases_every_slot() {
        let (shared, _clock, sink) = sink();
        let samples = [0.5f32; 128];
        sink.deliver(&StreamId::new("str_1"), frame(&samples, 48_000, 0));
        sink.deliver(&StreamId::new("str_2"), frame(&samples, 48_000, 0));
        assert_eq!(shared.jam_bus.bound_input_count(), 2);

        sink.clear();
        assert_eq!(shared.jam_bus.bound_input_count(), 0);
    }
}
