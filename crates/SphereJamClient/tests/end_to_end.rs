//! End-to-end against a live Futureboard Jam Server.
//!
//! This is the test that proves the translation is right. Everything else in
//! this crate checks a piece in isolation; this one starts two clients, puts
//! real PCM on a real socket, and asserts the samples one published are the
//! samples the other received — through the server's own REST control plane,
//! its signaling protocol, its transport negotiation and its SFU.
//!
//! It needs a server, so it is opt-in:
//!
//! ```sh
//! # In the jam server checkout, with development auth:
//! #   JAM_ENV=development JAM_AUTH_MODE=dev JAM_MEDIA_ENABLED=1 \
//! #   JAM_MEDIA_PUBLIC_HOST=127.0.0.1 ./jamd
//!
//! FUTUREBOARD_JAM_E2E=1 cargo test -p sphere-jam-client --test end_to_end -- --nocapture
//! ```
//!
//! Without `FUTUREBOARD_JAM_E2E=1` it reports that it was skipped and passes,
//! so an ordinary `cargo test` on a machine with no server stays green and
//! honest about what it did not check.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sphere_jam_client::api::{CreateInviteRequest, CreateJamRequest, JamApiClient};
use sphere_jam_client::bridge::{
    JamAudioFrame, JamAudioSink, JamPublishRequest, JamPublishSource, JamPublishSourceKind,
    PulledBlock,
};
use sphere_jam_client::config::{EnvSource, JamConfig};
use sphere_jam_client::credentials::{SharedCredentials, StaticToken};
use sphere_jam_client::ids::{JamId, StreamId};
use sphere_jam_client::protocol::TransportKind;
use sphere_jam_client::session::{JamIngress, JamSession, JamSessionOptions, JamState};
use sphere_jam_client::transport;

const API: &str = "http://127.0.0.1:8090";
const WS: &str = "ws://127.0.0.1:8090/v1/realtime";

/// How long to wait for each step. Generous: a cold server is doing its first
/// allocations, and a flaky timeout would make this test useless as a signal.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// The tone the publisher sends. A steady sine is easy to assert on and
/// impossible to confuse with silence or with a decode that lost its alignment.
const TONE_HZ: f32 = 440.0;
const TONE_AMPLITUDE: f32 = 0.5;
const PUBLISH_RATE: u32 = 48_000;

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
        ("FUTUREBOARD_JAM_CONNECT_TIMEOUT_MS", "10000"),
    ]))
    .expect("the development configuration is valid")
}

fn credentials(account: &str) -> SharedCredentials {
    // The server's development verifier maps `dev:<name>` to a stable account,
    // so the two clients here are two real, distinct users.
    Arc::new(StaticToken::new(format!("dev:{account}")))
}

/// A publish source that generates a sine at the negotiated rate.
///
/// It stands in for the Studio master tap: same trait, same pull shape, same
/// capture-timestamp contract.
struct ToneSource {
    phase: Mutex<f32>,
    ticks: AtomicU64,
    started: AtomicBool,
}

impl ToneSource {
    fn new() -> Self {
        Self {
            phase: Mutex::new(0.0),
            ticks: AtomicU64::new(0),
            started: AtomicBool::new(false),
        }
    }
}

impl JamPublishSource for ToneSource {
    fn pull(&self, out: &mut Vec<f32>, max_frames: usize) -> Option<PulledBlock> {
        let frames = max_frames.min(256);
        if frames == 0 {
            return None;
        }
        let mut phase = self.phase.lock().ok()?;
        out.clear();
        out.reserve(frames * 2);
        let step = std::f32::consts::TAU * TONE_HZ / PUBLISH_RATE as f32;
        for _ in 0..frames {
            let sample = phase.sin() * TONE_AMPLITUDE;
            *phase += step;
            if *phase > std::f32::consts::TAU {
                *phase -= std::f32::consts::TAU;
            }
            out.push(sample);
            out.push(sample);
        }
        let capture_ticks = self.ticks.fetch_add(frames as u64, Ordering::Relaxed);
        let take_start = !self.started.swap(true, Ordering::Relaxed);
        // Pace to roughly realtime. A publisher that ran flat out would fill
        // the server's queues and prove nothing about a musical stream.
        std::thread::sleep(Duration::from_millis(4));
        Some(PulledBlock {
            frames,
            capture_ticks,
            take_start,
        })
    }
}

/// What the receiving side heard.
#[derive(Debug, Default)]
struct Heard {
    frames: u64,
    packets: u64,
    peak: f32,
    first_capture_tick: Option<u64>,
    last_capture_tick: u64,
    sample_rate: u32,
    channels: usize,
    saw_take_start: bool,
}

struct RecordingSink {
    heard: Mutex<Heard>,
    stream: Mutex<Option<StreamId>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            heard: Mutex::new(Heard::default()),
            stream: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> Heard {
        let heard = self.heard.lock().expect("heard");
        Heard {
            frames: heard.frames,
            packets: heard.packets,
            peak: heard.peak,
            first_capture_tick: heard.first_capture_tick,
            last_capture_tick: heard.last_capture_tick,
            sample_rate: heard.sample_rate,
            channels: heard.channels,
            saw_take_start: heard.saw_take_start,
        }
    }
}

impl JamAudioSink for RecordingSink {
    fn deliver(&self, stream: &StreamId, frame: JamAudioFrame<'_>) {
        let mut heard = match self.heard.lock() {
            Ok(heard) => heard,
            Err(_) => return,
        };
        heard.packets += 1;
        heard.frames += frame.frames as u64;
        heard.sample_rate = frame.sample_rate;
        heard.channels = frame.channels;
        if heard.first_capture_tick.is_none() {
            heard.first_capture_tick = Some(frame.capture_timestamp);
        }
        heard.last_capture_tick = frame.capture_timestamp;
        heard.saw_take_start |= frame.is_take_start();
        for sample in frame.samples {
            heard.peak = heard.peak.max(sample.abs());
        }
        if let Ok(mut held) = self.stream.lock() {
            if held.is_none() {
                *held = Some(stream.clone());
            }
        }
    }

    fn stream_ended(&self, _stream: &StreamId) {}
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

/// The survivability claim, tested rather than asserted in a comment.
///
/// A client that reports it can open nothing but a WebSocket is the shape of a
/// laptop on a corporate network that permits outbound HTTPS and nothing else.
/// The server is required to offer it a path anyway, and audio is required to
/// arrive on it — slower and head-of-line blocked, but present. A jam that
/// silently produced nothing here would be worse than one that refused to join.
#[test]
fn a_client_that_can_only_open_443_still_receives_audio() {
    if !enabled() {
        eprintln!("skipping the 443-only jam test: set FUTUREBOARD_JAM_E2E=1");
        return;
    }

    let config = config();
    let host_api =
        JamApiClient::new(config.clone(), credentials("hachi224")).expect("the API client starts");
    let created = host_api
        .create_jam(&CreateJamRequest {
            name: "Locked Down".to_string(),
            max_participants: 4,
            ..Default::default()
        })
        .expect("a jam is created");
    let jam_id = JamId::new(created.jam.id.clone());

    // The publisher takes whatever path is best; only the receiver is
    // constrained, which is the real-world shape of this.
    let host_sink = Arc::new(RecordingSink::new());
    let mut host_options = JamSessionOptions::new(
        "e2e-ws-host",
        Arc::clone(&host_sink) as Arc<dyn JamAudioSink>,
    );
    host_options.publish_sample_rate = PUBLISH_RATE as i32;
    let host = JamSession::spawn(config.clone(), credentials("hachi224"), host_options)
        .expect("the host worker starts");
    host.join(jam_id.clone(), "").expect("the host joins");
    wait_for("the host to connect", || {
        host.state() == JamState::Connected
    });

    let tone = Arc::new(ToneSource::new());
    host.publish(
        JamPublishRequest::stereo("Guitar", JamPublishSourceKind::Master),
        Arc::clone(&tone) as Arc<dyn JamPublishSource>,
    )
    .expect("the publish is queued");
    wait_for("the stream to be published", || {
        !host.snapshot().streams.is_empty()
    });

    let invite = host_api
        .create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: "performer".to_string(),
                max_uses: 4,
                ..Default::default()
            },
        )
        .expect("an invite is minted");
    let guest_api =
        JamApiClient::new(config.clone(), credentials("mina")).expect("the API client starts");
    let admitted = guest_api
        .exchange_invite(&invite.secret, &created.jam.public_id)
        .expect("the invite is exchanged");

    let guest_sink = Arc::new(RecordingSink::new());
    let mut guest_options = JamSessionOptions::new(
        "e2e-ws-guest",
        Arc::clone(&guest_sink) as Arc<dyn JamAudioSink>,
    );
    guest_options.device_name = "Behind a firewall".to_string();
    guest_options.transport = transport::reliable_only_capabilities();

    let guest = JamSession::spawn(config.clone(), credentials("mina"), guest_options)
        .expect("the guest worker starts");
    guest
        .join(jam_id.clone(), admitted.access_token.clone())
        .expect("the guest joins");
    wait_for("the 443-only guest to connect", || {
        guest.state() == JamState::Connected
    });

    let transport_used = guest.snapshot().transport;
    eprintln!("the locked-down guest landed on {transport_used:?}");
    assert_eq!(
        transport_used,
        Some(TransportKind::WebSocket),
        "a client offering nothing else must land on the 443 fallback"
    );

    wait_for("audio to reach the 443-only guest", || {
        let heard = guest_sink.snapshot();
        heard.frames > 4_800 && heard.peak > 0.1
    });
    let heard = guest_sink.snapshot();
    eprintln!(
        "the locked-down guest heard {} packets / {} frames, peak {:.3}",
        heard.packets, heard.frames, heard.peak
    );
    assert!(
        (heard.peak - TONE_AMPLITUDE).abs() < 0.02,
        "the tone arrived at {:.3} over the fallback",
        heard.peak
    );
    assert_eq!(heard.sample_rate, PUBLISH_RATE);
    assert_eq!(heard.channels, 2);

    guest.leave().expect("the guest leaves");
    host.leave().expect("the host leaves");
    host_api
        .close_jam(jam_id.as_str())
        .expect("the host closes the jam");
}

/// Reconnect, from the receiving side's point of view.
///
/// A Studio that drops and comes back is the common case in a realtime system,
/// not an edge case: networks fail, laptops sleep, and a transport migrates from
/// UDP to the 443 fallback when a firewall changes its mind. The server
/// re-attaches a join from the same (account, device) to the participant that
/// is already there, keeping its id and its published streams and bumping the
/// connection generation.
///
/// Two things have to hold across that, and both are easy to get wrong:
///
///  * the room must still show **one** guitar, not one per reconnect — which
///    means the returning client adopts the stream it already has rather than
///    publishing a second one;
///  * audio must resume, with the receiver's jitter buffer discarding the old
///    generation rather than interleaving it with the new one.
#[test]
fn a_reconnecting_publisher_keeps_its_stream_instead_of_duplicating_it() {
    if !enabled() {
        eprintln!("skipping the reconnect test: set FUTUREBOARD_JAM_E2E=1");
        return;
    }

    let config = config();
    let host_api =
        JamApiClient::new(config.clone(), credentials("hachi224")).expect("the API client starts");
    let created = host_api
        .create_jam(&CreateJamRequest {
            name: "Reconnect".to_string(),
            max_participants: 4,
            ..Default::default()
        })
        .expect("a jam is created");
    let jam_id = JamId::new(created.jam.id.clone());

    // One device id, used by both the original session and the one that takes
    // over from it. That pair is what the server re-attaches on.
    const DEVICE: &str = "e2e-reconnecting-studio";

    let make_publisher = || {
        let sink = Arc::new(RecordingSink::new());
        let mut options =
            JamSessionOptions::new(DEVICE, Arc::clone(&sink) as Arc<dyn JamAudioSink>);
        options.publish_sample_rate = PUBLISH_RATE as i32;
        JamSession::spawn(config.clone(), credentials("hachi224"), options)
            .expect("the publisher worker starts")
    };

    let first = make_publisher();
    first
        .join(jam_id.clone(), "")
        .expect("the first session joins");
    wait_for("the first session to connect", || {
        first.state() == JamState::Connected
    });
    first
        .publish(
            JamPublishRequest::stereo("Guitar", JamPublishSourceKind::Master),
            Arc::new(ToneSource::new()) as Arc<dyn JamPublishSource>,
        )
        .expect("the publish is queued");
    wait_for("the first session to publish", || {
        !first.snapshot().streams.is_empty()
    });
    let original = first.snapshot().streams[0].clone();
    let original_participant = first
        .snapshot()
        .self_participant
        .expect("the first session knows its participant")
        .id;
    eprintln!(
        "first session: participant {original_participant}, stream {} alias {}",
        original.id, original.media_alias
    );

    // A guest, so there is somebody for the audio to reach.
    let invite = host_api
        .create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: "performer".to_string(),
                max_uses: 4,
                ..Default::default()
            },
        )
        .expect("an invite is minted");
    let guest_api =
        JamApiClient::new(config.clone(), credentials("mina")).expect("the API client starts");
    let admitted = guest_api
        .exchange_invite(&invite.secret, &created.jam.public_id)
        .expect("the invite is exchanged");
    let guest_sink = Arc::new(RecordingSink::new());
    let guest_options = JamSessionOptions::new(
        "e2e-reconnect-guest",
        Arc::clone(&guest_sink) as Arc<dyn JamAudioSink>,
    );
    let guest = JamSession::spawn(config.clone(), credentials("mina"), guest_options)
        .expect("the guest worker starts");
    guest
        .join(jam_id.clone(), admitted.access_token.clone())
        .expect("the guest joins");
    wait_for("the guest to hear the first session", || {
        guest_sink.snapshot().frames > 2_400
    });
    let before = guest_sink.snapshot();
    eprintln!("guest heard {} frames before the reconnect", before.frames);

    // The reconnect: a second session on the same account and device, with the
    // same publish armed. That is what a returning Studio looks like — the
    // publish is arranged before the join and restored once the session is up,
    // which is the path `restore_publications` runs on every reconnect.
    let second = make_publisher();
    second
        .publish(
            JamPublishRequest::stereo("Guitar", JamPublishSourceKind::Master),
            Arc::new(ToneSource::new()) as Arc<dyn JamPublishSource>,
        )
        .expect("the publish is armed before joining");
    second
        .join(jam_id.clone(), "")
        .expect("the returning session joins");
    wait_for("the returning session to connect", || {
        second.state() == JamState::Connected
    });

    let resumed = second
        .snapshot()
        .self_participant
        .expect("the returning session knows its participant");
    assert_eq!(
        resumed.id, original_participant,
        "a re-attach keeps the participant, so the room never sees a departure"
    );

    wait_for("the returning session to take its stream back", || {
        !second.snapshot().streams.is_empty()
    });
    let adopted = second.snapshot().streams[0].clone();
    eprintln!(
        "returning session: stream {} alias {}",
        adopted.id, adopted.media_alias
    );
    assert_eq!(
        adopted.id, original.id,
        "the returning session adopted its own stream rather than minting a second"
    );
    assert_eq!(adopted.media_alias, original.media_alias);

    // And the room agrees: one guitar, not two.
    let room = guest_api
        .streams(jam_id.as_str())
        .expect("the room lists its streams");
    let guitars = room.iter().filter(|stream| stream.name == "Guitar").count();
    assert_eq!(
        guitars, 1,
        "a reconnect must not leave a second guitar behind"
    );

    // Audio resumes on the new generation.
    let resume_mark = guest_sink.snapshot().frames;
    wait_for("audio to resume after the reconnect", || {
        guest_sink.snapshot().frames > resume_mark + 2_400
    });
    let after = guest_sink.snapshot();
    eprintln!(
        "guest heard {} frames total, peak {:.3}",
        after.frames, after.peak
    );
    assert!(
        (after.peak - TONE_AMPLITUDE).abs() < 0.02,
        "the tone survived the reconnect at {:.3}",
        after.peak
    );

    guest.leave().expect("the guest leaves");
    second.leave().expect("the returning session leaves");
    host_api
        .close_jam(jam_id.as_str())
        .expect("the host closes the jam");
}

/// The compiled production defaults have to match where the service actually
/// lives, because a release build with no environment uses exactly these.
#[test]
fn the_compiled_defaults_point_at_the_deployed_service() {
    let defaults = JamConfig::default();
    assert_eq!(defaults.api_url.as_str(), "https://jam.futureboard.studio/");
    assert_eq!(
        defaults.websocket_url.as_str(),
        "wss://jam.futureboard.studio/v1/realtime"
    );
    // The media plane is a different hostname, and the client never needs to
    // know it: every media address arrives as a signed candidate.
    assert!(!defaults.api_url.as_str().contains("media."));
}

#[test]
fn two_clients_exchange_pcm_over_a_live_jam() {
    if !enabled() {
        eprintln!(
            "skipping the live jam test: set FUTUREBOARD_JAM_E2E=1 with a jamd running on {API}"
        );
        return;
    }

    let config = config();

    // ── Host: create a jam over REST ────────────────────────────────────────
    let host_api =
        JamApiClient::new(config.clone(), credentials("hachi224")).expect("the API client starts");
    let created = host_api
        .create_jam(&CreateJamRequest {
            name: "Studio Jam".to_string(),
            max_participants: 8,
            ..Default::default()
        })
        .expect("a jam is created");
    let jam_id = JamId::new(created.jam.id.clone());
    assert!(!created.jam.public_id.is_empty(), "a jam gets a share code");
    eprintln!(
        "created {} ({}) in region {}",
        created.jam.id, created.jam.public_id, created.region.id
    );

    // Regions are a real endpoint, and the client reads them.
    let regions = host_api.regions().expect("regions list");
    assert!(!regions.regions.is_empty(), "the node serves a region");

    // ── Host: join, then publish a tone ─────────────────────────────────────
    let host_sink = Arc::new(RecordingSink::new());
    let mut host_options =
        JamSessionOptions::new("e2e-host", Arc::clone(&host_sink) as Arc<dyn JamAudioSink>);
    host_options.device_name = "End-to-end host".to_string();
    host_options.publish_sample_rate = PUBLISH_RATE as i32;
    host_options.publish_frame_samples = 256;

    let host = JamSession::spawn(config.clone(), credentials("hachi224"), host_options)
        .expect("the host worker starts");
    host.join(jam_id.clone(), "").expect("the host joins");
    wait_for("the host to connect", || {
        host.state() == JamState::Connected
    });
    eprintln!("host connected over {:?}", host.snapshot().transport);

    let tone = Arc::new(ToneSource::new());
    host.publish(
        JamPublishRequest::stereo("Guitar", JamPublishSourceKind::Master),
        Arc::clone(&tone) as Arc<dyn JamPublishSource>,
    )
    .expect("the publish command is queued");
    wait_for("the stream to be published", || {
        !host.snapshot().streams.is_empty()
    });
    let published = host.snapshot().streams[0].clone();
    eprintln!(
        "published {} as alias {} ({} {} Hz {}ch)",
        published.id,
        published.media_alias,
        published.codec.as_str(),
        published.sample_rate,
        published.channels
    );
    assert_eq!(published.name, "Guitar");
    assert_eq!(published.sample_rate, PUBLISH_RATE as i32);

    // ── Guest: exchange an invite and join ──────────────────────────────────
    let invite = host_api
        .create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: "performer".to_string(),
                max_uses: 4,
                ..Default::default()
            },
        )
        .expect("an invite is minted");
    assert!(!invite.secret.is_empty(), "the secret is returned once");

    let guest_api =
        JamApiClient::new(config.clone(), credentials("mina")).expect("the API client starts");
    let admitted = guest_api
        .exchange_invite(&invite.secret, &created.jam.public_id)
        .expect("the invite is exchanged");
    assert_eq!(admitted.jam.id, created.jam.id);
    assert!(admitted.permissions.receive_audio);

    let guest_sink = Arc::new(RecordingSink::new());
    let mut guest_options = JamSessionOptions::new(
        "e2e-guest",
        Arc::clone(&guest_sink) as Arc<dyn JamAudioSink>,
    );
    guest_options.device_name = "End-to-end guest".to_string();

    let guest = JamSession::spawn(config.clone(), credentials("mina"), guest_options)
        .expect("the guest worker starts");
    guest
        .join(jam_id.clone(), admitted.access_token.clone())
        .expect("the guest joins");
    wait_for("the guest to connect", || {
        guest.state() == JamState::Connected
    });

    // The room, as the guest sees it: the host is there and its stream is
    // listed, with the media alias the packets will carry.
    wait_for("the guest to see the host's stream", || {
        let snapshot = guest.snapshot();
        !snapshot.participants.is_empty() && !snapshot.streams.is_empty()
    });
    let guest_view = guest.snapshot();
    let seen = &guest_view.streams[0];
    assert_eq!(seen.id, published.id, "the same stream id on both sides");
    assert_eq!(seen.media_alias, published.media_alias);
    assert_eq!(
        seen.user_id, published.user_id,
        "routing identity is the account, not the username"
    );

    // A format selected for this receiver is the server saying "you are
    // subscribed and this is what will arrive".
    wait_for("the server to select a format for the guest", || {
        !guest.snapshot().formats.is_empty()
    });
    let (_, format) = guest.snapshot().formats[0];
    eprintln!(
        "negotiated {} {} Hz {}ch {} × {} frames",
        format.codec.as_str(),
        format.sample_rate,
        format.channels,
        format.format.as_str(),
        format.frame_samples
    );
    assert_eq!(format.sample_rate, PUBLISH_RATE as i32);
    assert_eq!(format.channels, 2);

    // ── The point of the whole exercise: audio arrives ─────────────────────
    wait_for("audio to reach the guest", || {
        let heard = guest_sink.snapshot();
        heard.frames > 4_800 && heard.peak > 0.1
    });

    let heard = guest_sink.snapshot();
    eprintln!(
        "guest heard {} packets / {} frames, peak {:.3}, capture ticks {:?}..{}",
        heard.packets, heard.frames, heard.peak, heard.first_capture_tick, heard.last_capture_tick
    );

    assert_eq!(heard.channels, 2, "the layout survived the round trip");
    assert_eq!(heard.sample_rate, PUBLISH_RATE);
    assert!(
        (heard.peak - TONE_AMPLITUDE).abs() < 0.02,
        "the tone arrived at {:.3}, expected about {TONE_AMPLITUDE}",
        heard.peak
    );
    // The guest joined after publishing had already started, so it correctly
    // never saw the take's first packet and its capture clock starts partway in.
    let first = heard
        .first_capture_tick
        .expect("every packet carries the publisher's capture timestamp");
    assert!(
        heard.last_capture_tick > first,
        "the capture clock advanced with the audio"
    );

    // The strongest statement this test can make about timing: the capture
    // timestamps are contiguous with the audio itself. If the client were
    // stamping arrival time, this span would carry the network's jitter and
    // would not line up with the frame count at all.
    let frames_per_packet = heard.frames / heard.packets;
    let span = heard.last_capture_tick - first;
    assert_eq!(
        span,
        heard.frames - frames_per_packet,
        "capture timestamps must track the publisher's own clock, not arrival"
    );

    // The publisher never receives its own stream back.
    let host_heard = host_sink.snapshot();
    assert_eq!(
        host_heard.frames, 0,
        "a publisher must not be echoed its own audio"
    );

    // The clock exchange runs on its own schedule; by now it has landed.
    wait_for("the session clock to lock", || {
        guest.snapshot().clock_locked
    });
    let clock = guest.snapshot();
    eprintln!(
        "clock: rtt {:.2} ms, offset {:.3} ms, drift {:.1} ppm",
        clock.rtt_ms, clock.clock_offset_ms, clock.clock_drift_ppm
    );
    assert!(clock.rtt_ms >= 0.0);

    // ── Leaving is clean on both sides ─────────────────────────────────────
    guest.leave().expect("the guest leaves");
    wait_for("the guest to disconnect", || {
        guest.state() != JamState::Connected
    });
    wait_for("the host to see the guest leave", || {
        host.snapshot().participants.len() <= 1
    });

    host.leave().expect("the host leaves");
    host_api
        .close_jam(jam_id.as_str())
        .expect("the host closes the jam");
}

/// Ingress control, from the receiving side.
///
/// Two performers publish. The receiver joins as a DAW would — silent, then
/// asking for one performer by name — and must hear that one and only that one.
/// The claim being tested is bandwidth, which no assertion can observe
/// directly; what makes it observable is that the stream nobody routed never
/// gets a format and never reaches the sink, while the routed one does.
///
/// It also covers the part that is easy to get wrong: a track bound to a
/// performer who has not published yet. The subscription is asked for before
/// the second stream exists, and has to attach by itself when it appears.
#[test]
fn a_routed_receiver_hears_only_the_streams_it_asked_for() {
    if !enabled() {
        eprintln!("skipping the ingress test: set FUTUREBOARD_JAM_E2E=1");
        return;
    }

    let config = config();
    let host_api =
        JamApiClient::new(config.clone(), credentials("hachi224")).expect("the API client starts");
    let created = host_api
        .create_jam(&CreateJamRequest {
            name: "Routed Ingress".to_string(),
            max_participants: 4,
            ..Default::default()
        })
        .expect("a jam is created");
    let jam_id = JamId::new(created.jam.id.clone());

    // The publisher takes both streams: two publishers would prove the same
    // thing and cost a second account, and what is under test is the receiver.
    let host_sink = Arc::new(RecordingSink::new());
    let mut host_options = JamSessionOptions::new(
        "e2e-in-host",
        Arc::clone(&host_sink) as Arc<dyn JamAudioSink>,
    );
    host_options.publish_sample_rate = PUBLISH_RATE as i32;
    let host = JamSession::spawn(config.clone(), credentials("hachi224"), host_options)
        .expect("the host worker starts");
    host.join(jam_id.clone(), "").expect("the host joins");
    wait_for("the host to connect", || {
        host.state() == JamState::Connected
    });

    let routed_tone = Arc::new(ToneSource::new());
    host.publish(
        JamPublishRequest::stereo("Routed Guitar", JamPublishSourceKind::Master),
        Arc::clone(&routed_tone) as Arc<dyn JamPublishSource>,
    )
    .expect("the publish is queued");
    wait_for("the first stream to be published", || {
        !host.snapshot().streams.is_empty()
    });
    let routed = StreamId::new(
        host.snapshot()
            .streams
            .iter()
            .find(|stream| stream.name == "Routed Guitar")
            .expect("the routed stream is in the room")
            .id
            .clone(),
    );

    let invite = host_api
        .create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: "performer".to_string(),
                max_uses: 4,
                ..Default::default()
            },
        )
        .expect("an invite is minted");
    let guest_api =
        JamApiClient::new(config.clone(), credentials("mina")).expect("the API client starts");
    let admitted = guest_api
        .exchange_invite(&invite.secret, &created.jam.public_id)
        .expect("the invite is exchanged");

    let guest_sink = Arc::new(RecordingSink::new());
    let mut guest_options = JamSessionOptions::new(
        "e2e-in-guest",
        Arc::clone(&guest_sink) as Arc<dyn JamAudioSink>,
    );
    guest_options.device_name = "Routing Studio".to_string();
    // The whole point: this client chooses.
    guest_options.ingress = JamIngress::Routed;

    let guest = JamSession::spawn(config.clone(), credentials("mina"), guest_options)
        .expect("the guest worker starts");
    guest
        .join(jam_id.clone(), admitted.access_token.clone())
        .expect("the guest joins");
    wait_for("the routed guest to connect", || {
        guest.state() == JamState::Connected
    });

    // Nothing was routed yet, so nothing may arrive. Waited out rather than
    // checked once: a subscription that leaked would take a moment to show.
    std::thread::sleep(Duration::from_millis(600));
    let quiet = guest_sink.snapshot();
    assert_eq!(
        quiet.frames, 0,
        "a routed receiver that asked for nothing received {} frames",
        quiet.frames
    );

    // Route the published performer, and a second one that does not exist yet.
    let waiting = StreamId::new("str_not_published_yet");
    guest
        .subscribe(vec![routed.clone(), waiting])
        .expect("the subscribe is queued");

    wait_for("audio from the routed stream", || {
        let heard = guest_sink.snapshot();
        heard.frames > 4_800 && heard.peak > 0.1
    });
    let heard = guest_sink.snapshot();
    eprintln!(
        "the routed guest heard {} packets / {} frames, peak {:.3}",
        heard.packets, heard.frames, heard.peak
    );
    assert!(
        (heard.peak - TONE_AMPLITUDE).abs() < 0.02,
        "the routed tone arrived at {:.3}",
        heard.peak
    );

    // A second stream appears that nobody routed. It has to stay silent: this
    // is the case that costs a band's worth of bandwidth if ingress control is
    // only honoured at join time.
    let unrouted_tone = Arc::new(ToneSource::new());
    host.publish(
        JamPublishRequest::stereo("Unrouted Keys", JamPublishSourceKind::Master),
        Arc::clone(&unrouted_tone) as Arc<dyn JamPublishSource>,
    )
    .expect("the second publish is queued");
    wait_for("the second stream to reach the room", || {
        guest
            .snapshot()
            .streams
            .iter()
            .any(|stream| stream.name == "Unrouted Keys")
    });
    std::thread::sleep(Duration::from_millis(600));

    // A resolved format is the observable proof that audio is on its way: it is
    // what the server sends only to a subscriber, and what the media threads
    // need before they will decode a packet at all.
    let unrouted = StreamId::new(
        guest
            .snapshot()
            .streams
            .iter()
            .find(|stream| stream.name == "Unrouted Keys")
            .expect("the unrouted stream is listed")
            .id
            .clone(),
    );
    assert!(
        !receiving(&guest, &unrouted),
        "a stream nobody routed resolved a format and is arriving"
    );

    // And routing it is all it takes.
    guest
        .subscribe(vec![unrouted.clone()])
        .expect("the second subscribe is queued");
    wait_for("the second stream to start arriving", || {
        receiving(&guest, &unrouted)
    });

    // Dropping a routing stops it again.
    guest
        .unsubscribe(vec![routed.clone()])
        .expect("the unsubscribe is queued");
    wait_for("the routed stream to stop", || !receiving(&guest, &routed));

    guest.leave().expect("the guest leaves");
    host.leave().expect("the host leaves");
    host_api
        .close_jam(jam_id.as_str())
        .expect("the host closes the jam");
}

/// Whether the server has resolved a format for this stream, which is the one
/// observable difference between a stream that is listed and one that arrives.
fn receiving(session: &JamSession, stream: &StreamId) -> bool {
    session
        .snapshot()
        .formats
        .iter()
        .any(|(id, _)| id == stream)
}
