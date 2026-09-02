//! Studio's jam bridge, end to end through a live Futureboard Jam Server.
//!
//! The jam client has its own end-to-end test for the protocol. This one checks
//! the half that belongs to Studio: that audio arriving from the network lands
//! in the audio engine's jam bus at the right level and the right position, and
//! that audio written by the engine's callback reaches a listener on the other
//! side of the server.
//!
//! ```txt
//! engine publish slot ─▶ JamEngineSource ─▶ jamd ─▶ JamEngineSink ─▶ engine input slot
//! ```
//!
//! Opt-in, because it needs a server:
//!
//! ```sh
//! FUTUREBOARD_JAM_E2E=1 cargo test -p sphere_ui_components --test jam_engine_e2e -- --nocapture
//! ```

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sphere_jam_client::api::{CreateInviteRequest, CreateJamRequest, JamApiClient};
use sphere_jam_client::bridge::{JamAudioSink, JamPublishRequest, JamPublishSourceKind};
use sphere_jam_client::clock::SessionClock;
use sphere_jam_client::config::{EnvSource, JamConfig};
use sphere_jam_client::credentials::{SharedCredentials, StaticToken};
use sphere_jam_client::ids::{JamId, StreamId};
use sphere_jam_client::session::{JamSession, JamSessionOptions, JamState};
use sphere_ui_components::jam::{JamEngineSink, JamEngineSource};
use DirectAudio::engine::SharedState;
use DirectAudio::jam_bus::PUBLISH_KEY_MASTER;
use DirectAudio::JamChannelMode;

const API: &str = "http://127.0.0.1:8090";
const WS: &str = "ws://127.0.0.1:8090/v1/realtime";
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
const ENGINE_RATE: u32 = 48_000;
const TONE_HZ: f32 = 440.0;
const TONE_AMPLITUDE: f32 = 0.5;

fn enabled() -> bool {
    std::env::var("FUTUREBOARD_JAM_E2E")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn config() -> JamConfig {
    JamConfig::from_source(&EnvSource::from_pairs([
        ("FUTUREBOARD_ENV", "development"),
        ("FUTUREBOARD_JAM_API_URL", API),
        ("FUTUREBOARD_JAM_WS_URL", WS),
        ("FUTUREBOARD_JAM_RECONNECT", "false"),
    ]))
    .expect("the development configuration is valid")
}

fn credentials(account: &str) -> SharedCredentials {
    Arc::new(StaticToken::new(format!("dev:{account}")))
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + STEP_TIMEOUT;
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

/// Fill an engine publish slot the way the realtime callback does: interleaved
/// stereo blocks at the engine's rate, written straight into the ring.
fn feed_master(shared: &SharedState, slot_index: usize, phase: &mut f32, frames: usize) {
    let step = std::f32::consts::TAU * TONE_HZ / ENGINE_RATE as f32;
    let mut block = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        let sample = phase.sin() * TONE_AMPLITUDE;
        *phase += step;
        if *phase > std::f32::consts::TAU {
            *phase -= std::f32::consts::TAU;
        }
        block.push(sample);
        block.push(sample);
    }
    shared
        .jam_bus
        .publish(slot_index)
        .expect("in range")
        .write_interleaved(&block, 2, ENGINE_RATE);
}

/// Where a remote take belongs on the timeline, across two different project
/// rates.
///
/// This is the claim recording alignment rests on, and it is the one that is
/// easy to get subtly wrong: the jam counts time in 48 kHz session ticks, the
/// receiving project counts it in its own samples, and the number that has to
/// survive the trip is *when the publisher played the note* — not when the
/// packet turned up. A 96 kHz session must place a 48 kHz performer at twice
/// the sample index, and network delay must not move it.
#[test]
fn a_remote_take_lands_where_it_was_played_even_at_a_different_project_rate() {
    if !enabled() {
        eprintln!("skipping the alignment test: set FUTUREBOARD_JAM_E2E=1");
        return;
    }

    const RECEIVER_RATE: u32 = 96_000;
    let config = config();

    let publisher_engine = Arc::new(SharedState::default());
    publisher_engine
        .sample_rate
        .store(ENGINE_RATE, Ordering::Relaxed);
    let publisher_clock = Arc::new(Mutex::new(SessionClock::default()));
    let publisher_sink = Arc::new(JamEngineSink::new(
        Arc::clone(&publisher_engine),
        Arc::clone(&publisher_clock),
    ));

    let api = JamApiClient::new(config.clone(), credentials("hachi224")).expect("api client");
    let created = api
        .create_jam(&CreateJamRequest {
            name: "Alignment".to_string(),
            max_participants: 4,
            ..Default::default()
        })
        .expect("a jam is created");
    let jam_id = JamId::new(created.jam.id.clone());

    let mut options = JamSessionOptions::new(
        "align-a",
        Arc::clone(&publisher_sink) as Arc<dyn JamAudioSink>,
    );
    options.publish_sample_rate = ENGINE_RATE as i32;
    let publisher = JamSession::spawn_with_clock(
        config.clone(),
        credentials("hachi224"),
        options,
        Arc::clone(&publisher_clock),
    )
    .expect("the publisher worker starts");
    publisher
        .join(jam_id.clone(), "")
        .expect("the publisher joins");
    wait_for("the publisher to connect", || {
        publisher.state() == JamState::Connected
    });

    let slot_index = publisher_engine
        .jam_bus
        .bind_publish(PUBLISH_KEY_MASTER)
        .expect("a publish slot is free");
    publisher
        .publish(
            JamPublishRequest::stereo("Guitar", JamPublishSourceKind::Master),
            Arc::new(JamEngineSource::new(
                Arc::clone(&publisher_engine),
                Arc::clone(&publisher_clock),
                PUBLISH_KEY_MASTER,
                ENGINE_RATE,
            )),
        )
        .expect("the publish is queued");
    wait_for("the stream to be published", || {
        !publisher.snapshot().streams.is_empty()
    });
    let published = publisher.snapshot().streams[0].clone();

    // The receiving Studio runs at twice the rate. Nothing about the jam is
    // allowed to change that, and nothing about that is allowed to change where
    // the remote performance lands.
    let invite = api
        .create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: "performer".to_string(),
                max_uses: 4,
                ..Default::default()
            },
        )
        .expect("an invite is minted");
    let guest_api = JamApiClient::new(config.clone(), credentials("mina")).expect("api client");
    let admitted = guest_api
        .exchange_invite(&invite.secret, &created.jam.public_id)
        .expect("the invite is exchanged");

    let receiver_engine = Arc::new(SharedState::default());
    receiver_engine
        .sample_rate
        .store(RECEIVER_RATE, Ordering::Relaxed);
    let receiver_clock = Arc::new(Mutex::new(SessionClock::default()));
    let receiver_sink = Arc::new(JamEngineSink::new(
        Arc::clone(&receiver_engine),
        Arc::clone(&receiver_clock),
    ));
    let guest_options = JamSessionOptions::new(
        "align-b",
        Arc::clone(&receiver_sink) as Arc<dyn JamAudioSink>,
    );
    let receiver = JamSession::spawn_with_clock(
        config.clone(),
        credentials("mina"),
        guest_options,
        Arc::clone(&receiver_clock),
    )
    .expect("the receiver worker starts");
    receiver
        .join(jam_id.clone(), admitted.access_token.clone())
        .expect("the receiver joins");
    wait_for("the receiver to connect", || {
        receiver.state() == JamState::Connected
    });
    wait_for("the receiver to be given a format", || {
        !receiver.snapshot().formats.is_empty()
    });

    let stream_id = StreamId::new(published.id.clone());
    let mut phase = 0.0f32;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bound = None;
    while Instant::now() < deadline {
        feed_master(&publisher_engine, slot_index, &mut phase, 256);
        std::thread::sleep(Duration::from_millis(5));
        if bound.is_none() {
            bound = receiver_sink.slot_for(&stream_id);
        }
        if let Some(index) = bound {
            let slot = receiver_engine.jam_bus.input(index).expect("in range");
            // Wait for the receiving clock to lock too: before it does, the
            // slot correctly reports no position rather than a guess.
            if slot.available() >= 9_600 && slot.next_capture_position().is_some() {
                break;
            }
        }
    }

    let index = bound.expect("the receiving engine bound a slot");
    let slot = receiver_engine.jam_bus.input(index).expect("in range");
    let start = slot
        .next_capture_position()
        .expect("the take has a position on the receiving timeline");

    // 1. The position is in the receiving project's own samples, and it advances
    //    one per frame consumed — so a recorder can write frames straight down
    //    from it.
    const DRAIN: usize = 4_096;
    let mut left = vec![0.0f32; DRAIN];
    let mut right = vec![0.0f32; DRAIN];
    let read = slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, DRAIN);
    assert_eq!(read, DRAIN, "the callback drained a full block");
    let after = slot
        .next_capture_position()
        .expect("the position survives a drain");
    assert_eq!(
        after - start,
        DRAIN as i64,
        "the capture position must advance exactly one per frame at the project rate"
    );

    // 2. The position is in the right units and the right epoch. Converted back
    //    into session ticks it has to sit close to the jam's own clock — within
    //    a couple of seconds, which is loose enough not to be flaky and tight
    //    enough to catch a rate factor of two or a wrong epoch.
    let (now_ticks, rate) = {
        let clock = receiver_clock.lock().expect("clock");
        (
            clock
                .session_ticks_at(sphere_jam_client::clock::client_nanos())
                .expect("the receiving clock is locked"),
            clock.rate(),
        )
    };
    let ticks_at_start = start * rate as i64 / RECEIVER_RATE as i64;
    let behind_ms = (now_ticks - ticks_at_start) as f64 * 1000.0 / rate as f64;
    eprintln!(
        "receiver at {RECEIVER_RATE} Hz: take starts at sample {start} \
         ({ticks_at_start} ticks), {behind_ms:.0} ms behind the session clock"
    );
    assert!(
        behind_ms.abs() < 2_000.0,
        "the take is {behind_ms:.0} ms from the session clock — wrong epoch or wrong rate"
    );
    // It must be *behind* rather than ahead: audio that has arrived was played
    // in the past. A position in the future would mean the mapping is inverted.
    assert!(
        behind_ms > -50.0,
        "the take claims to have been captured in the future"
    );

    receiver.leave().expect("the receiver leaves");
    publisher.leave().expect("the publisher leaves");
    api.close_jam(jam_id.as_str()).expect("the jam is closed");
}

#[test]
fn studio_publishes_its_master_and_hears_a_remote_stream_on_its_own_bus() {
    if !enabled() {
        eprintln!(
            "skipping the live Studio jam test: set FUTUREBOARD_JAM_E2E=1 with a jamd on {API}"
        );
        return;
    }

    let config = config();

    // ── Studio A: the engine that publishes its master bus ──────────────────
    let publisher_engine = Arc::new(SharedState::default());
    publisher_engine
        .sample_rate
        .store(ENGINE_RATE, Ordering::Relaxed);
    let publisher_clock = Arc::new(Mutex::new(SessionClock::default()));
    let publisher_sink = Arc::new(JamEngineSink::new(
        Arc::clone(&publisher_engine),
        Arc::clone(&publisher_clock),
    ));

    let api = JamApiClient::new(config.clone(), credentials("hachi224")).expect("api client");
    let created = api
        .create_jam(&CreateJamRequest {
            name: "Studio Jam".to_string(),
            max_participants: 4,
            ..Default::default()
        })
        .expect("a jam is created");
    let jam_id = JamId::new(created.jam.id.clone());
    eprintln!("jam {} ({})", created.jam.id, created.jam.public_id);

    let mut options = JamSessionOptions::new(
        "studio-a",
        Arc::clone(&publisher_sink) as Arc<dyn JamAudioSink>,
    );
    options.device_name = "Studio A".to_string();
    options.publish_sample_rate = ENGINE_RATE as i32;
    let publisher = JamSession::spawn_with_clock(
        config.clone(),
        credentials("hachi224"),
        options,
        Arc::clone(&publisher_clock),
    )
    .expect("the publisher worker starts");
    publisher.join(jam_id.clone(), "").expect("Studio A joins");
    wait_for("Studio A to connect", || {
        publisher.state() == JamState::Connected
    });

    // Claim the master publish slot, exactly as `JamController::publish_master`
    // does, then hand the jam a source that reads it.
    let slot_index = publisher_engine
        .jam_bus
        .bind_publish(PUBLISH_KEY_MASTER)
        .expect("a publish slot is free");
    let source = Arc::new(JamEngineSource::new(
        Arc::clone(&publisher_engine),
        Arc::clone(&publisher_clock),
        PUBLISH_KEY_MASTER,
        ENGINE_RATE,
    ));
    publisher
        .publish(
            JamPublishRequest::stereo("Studio Master", JamPublishSourceKind::Master),
            source,
        )
        .expect("the publish is queued");
    wait_for("the master stream to be published", || {
        !publisher.snapshot().streams.is_empty()
    });
    let published = publisher.snapshot().streams[0].clone();
    eprintln!(
        "Studio A published {} ({} {} Hz {}ch)",
        published.name,
        published.codec.as_str(),
        published.sample_rate,
        published.channels
    );

    // ── Studio B: the engine that receives it ───────────────────────────────
    let invite = api
        .create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: "performer".to_string(),
                max_uses: 4,
                ..Default::default()
            },
        )
        .expect("an invite is minted");
    let guest_api = JamApiClient::new(config.clone(), credentials("mina")).expect("api client");
    let admitted = guest_api
        .exchange_invite(&invite.secret, &created.jam.public_id)
        .expect("the invite is exchanged");

    let receiver_engine = Arc::new(SharedState::default());
    receiver_engine
        .sample_rate
        .store(ENGINE_RATE, Ordering::Relaxed);
    let receiver_clock = Arc::new(Mutex::new(SessionClock::default()));
    let receiver_sink = Arc::new(JamEngineSink::new(
        Arc::clone(&receiver_engine),
        Arc::clone(&receiver_clock),
    ));

    let mut guest_options = JamSessionOptions::new(
        "studio-b",
        Arc::clone(&receiver_sink) as Arc<dyn JamAudioSink>,
    );
    guest_options.device_name = "Studio B".to_string();
    let receiver = JamSession::spawn_with_clock(
        config.clone(),
        credentials("mina"),
        guest_options,
        Arc::clone(&receiver_clock),
    )
    .expect("the receiver worker starts");
    receiver
        .join(jam_id.clone(), admitted.access_token.clone())
        .expect("Studio B joins");
    wait_for("Studio B to connect", || {
        receiver.state() == JamState::Connected
    });
    wait_for("Studio B to be given a format", || {
        !receiver.snapshot().formats.is_empty()
    });

    // ── Play: Studio A's callback fills the master ring ─────────────────────
    let stream_id = StreamId::new(published.id.clone());
    let mut phase = 0.0f32;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bound_slot = None;
    while Instant::now() < deadline {
        feed_master(&publisher_engine, slot_index, &mut phase, 256);
        std::thread::sleep(Duration::from_millis(5));

        if bound_slot.is_none() {
            bound_slot = receiver_sink.slot_for(&stream_id);
        }
        if let Some(index) = bound_slot {
            let slot = receiver_engine.jam_bus.input(index).expect("in range");
            if slot.available() >= 4_800 {
                break;
            }
        }
    }

    // ── Assert: the tone is on Studio B's own bus ───────────────────────────
    let index = bound_slot.expect("the receiving engine bound a slot for the stream");
    let slot = receiver_engine.jam_bus.input(index).expect("in range");
    eprintln!(
        "Studio B bus slot {index}: {} frames ready, rate {}, underruns {}, overruns {}",
        slot.available(),
        slot.sample_rate(),
        slot.underruns(),
        slot.overruns()
    );

    assert!(
        slot.available() >= 4_800,
        "only {} frames reached the receiving engine",
        slot.available()
    );
    assert_eq!(
        slot.sample_rate(),
        ENGINE_RATE,
        "audio is converted into the receiving engine's own rate"
    );
    assert!(
        slot.next_capture_position().is_some(),
        "the bus knows where this audio belongs on the timeline"
    );

    // Drain the way the audio callback does, and check the level.
    let mut left = vec![0.0f32; 2_048];
    let mut right = vec![0.0f32; 2_048];
    let read = slot.mix_into(JamChannelMode::Stereo, &mut left, &mut right, 2_048);
    assert_eq!(read, 2_048, "the callback drained a full block");

    let peak = left
        .iter()
        .chain(right.iter())
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    eprintln!("Studio B heard a peak of {peak:.3}");
    assert!(
        (peak - TONE_AMPLITUDE).abs() < 0.05,
        "the tone arrived at {peak:.3}, expected about {TONE_AMPLITUDE}"
    );

    // A publisher is never echoed its own audio, so its own bus stays empty.
    assert_eq!(
        publisher_engine.jam_bus.bound_input_count(),
        0,
        "the publisher must not receive its own stream"
    );

    receiver.leave().expect("Studio B leaves");
    publisher.leave().expect("Studio A leaves");
    api.close_jam(jam_id.as_str()).expect("the jam is closed");
}
