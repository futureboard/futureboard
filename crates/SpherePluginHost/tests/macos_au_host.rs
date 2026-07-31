//! Audio Unit hosting end to end against a real spawned plug-in host process.
//!
//! Unlike the VST3 editor test, this needs no third-party plug-in: Apple's stock
//! AUDelay (`aufx`/`dely`/`appl`) ships with every macOS install, so the whole
//! path can be exercised on any machine:
//!
//!   spawn host -> Ready -> AttachSharedAudio -> LoadAudioUnit -> PluginLoaded
//!               -> drive real blocks through the shared region
//!               -> GetPluginParameters / Get+SetPluginState -> UnloadPlugin
//!
//! Run with:
//!
//!   cargo test -p sphere-plugin-host --features plugin-host-bin \
//!     --test macos_au_host -- --nocapture
//!
//! The in-process unit tests in `au_host.rs` cover the native runtime itself.
//! What this test adds is the parts only a real host process has: the
//! `LoadAudioUnit` command, the AU arm of the block path, and the AU branches of
//! the instance-keyed commands.
#![cfg(all(target_os = "macos", feature = "plugin-host-bin"))]

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use SpherePluginHost::audio_bridge::{BridgeTransport, SharedAudioRegion};
use SpherePluginHost::ipc::HostEvent;
use SpherePluginHost::plugin_host_client::{ClientEvent, PluginHostClient};

const INSTANCE_ID: &str = "track1:insert1";
/// Apple AUDelay: `aufx`/`dely`/`appl` as the scanner's component id.
const APPLE_DELAY: &str = "au:61756678:64656c79:6170706c";
const SAMPLE_RATE: u32 = 48_000;
const BLOCK_FRAMES: u32 = 256;

fn wait_for<T>(
    client: &PluginHostClient,
    timeout: Duration,
    mut accept: impl FnMut(HostEvent) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match client.try_recv_event() {
            Some(ClientEvent::Host(event)) => {
                if let Some(value) = accept(event) {
                    return Some(value);
                }
            }
            Some(other) => eprintln!("[test] client event {other:?}"),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    None
}

/// Run one block through the host the way the engine does: write input, publish
/// the request, wait for the acknowledgement, read what came back.
fn render_block(region: &SharedAudioRegion, input: &[f32]) -> Option<Vec<f32>> {
    let bridge = region.bridge();
    let frames = input.len();
    bridge.block_frames.store(frames as u32, Ordering::Relaxed);
    // SAFETY: the engine side owns `audio_in` until it bumps `request_seq`.
    unsafe { bridge.audio_in.write_deinterleaved(input, input, frames) };

    let requested = bridge.request_seq.load(Ordering::Relaxed) + 1;
    bridge.request_seq.store(requested, Ordering::Release);

    // The host's producer wakes on its own 1 ms safety-net timeout when no kick
    // event is signalled, so polling is enough here.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if bridge.done_seq.load(Ordering::Acquire) >= requested {
            let channels = bridge.plugin_output_channels().max(1) as usize;
            let mut out = vec![0.0f32; frames * channels];
            // SAFETY: the host published `done_seq`, so `audio_out` is ours again.
            unsafe {
                bridge
                    .audio_out
                    .read_interleaved(&mut out, frames * channels)
            };
            return Some(out);
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    None
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
}

#[test]
fn a_stock_audio_unit_loads_renders_and_reports_its_state() {
    let mut client = PluginHostClient::spawn_bridge().expect("spawn plugin host");
    assert!(
        wait_for(&client, Duration::from_secs(15), |event| matches!(
            event,
            HostEvent::Ready { .. }
        )
        .then_some(()))
        .is_some(),
        "host never reported Ready"
    );

    // Engine side of the shared audio region.
    let region_name = format!("FutureboardAuHostTest-{}", std::process::id());
    let region = SharedAudioRegion::create_named(&region_name, SAMPLE_RATE, BLOCK_FRAMES, 2)
        .expect("create shared audio region");
    region.bridge().store_transport(&BridgeTransport {
        tempo_bpm: 120.0,
        time_sig_num: 4,
        time_sig_den: 4,
        project_time_samples: 0,
        ppq_position: 0.0,
        bar_position_ppq: 0.0,
        playing: true,
        recording: false,
    });
    client
        .attach_shared_audio(region_name.clone(), region.bytes(), INSTANCE_ID)
        .expect("send attach_shared_audio");
    let attached = wait_for(&client, Duration::from_secs(10), |event| match event {
        HostEvent::SharedAudioAttached { attached, .. } => Some(attached),
        _ => None,
    })
    .expect("host never answered AttachSharedAudio");
    assert!(attached, "host failed to map the shared audio region");

    client
        .load_au_plugin(INSTANCE_ID, APPLE_DELAY, SAMPLE_RATE, BLOCK_FRAMES, None)
        .expect("send load_au_plugin");
    let loaded = wait_for(&client, Duration::from_secs(30), |event| match event {
        HostEvent::PluginLoaded { name, .. } | HostEvent::PluginAlreadyLoaded { name, .. } => {
            Some(Ok(name))
        }
        HostEvent::PluginLoadFailed { error, .. } => Some(Err(error)),
        _ => None,
    })
    .expect("host never answered LoadAudioUnit");
    let loaded = loaded.unwrap_or_else(|error| panic!("audio unit load failed: {error}"));
    eprintln!("[test] loaded {loaded}");

    // Audio, through the real block path in the real host process.
    let frames = BLOCK_FRAMES as usize;
    let tone: Vec<f32> = (0..frames)
        .map(|frame| (frame as f32 * 0.05).sin() * 0.5)
        .collect();
    let mut best = 0.0f32;
    for block in 0..8 {
        let rendered = render_block(&region, &tone)
            .unwrap_or_else(|| panic!("host did not acknowledge block {block}"));
        best = best.max(peak(&rendered));
    }
    eprintln!(
        "[test] rendered 8 blocks output_peak={best:.4} channels={}",
        region.bridge().plugin_output_channels()
    );
    assert!(
        best > 0.01,
        "the audio unit produced silence through the bridge (peak {best})"
    );

    client
        .get_plugin_parameters(INSTANCE_ID)
        .expect("send get_plugin_parameters");
    let parameters = wait_for(&client, Duration::from_secs(10), |event| match event {
        HostEvent::PluginParameters { parameters, ok, .. } => Some((parameters, ok)),
        _ => None,
    })
    .expect("host never answered GetPluginParameters");
    assert!(
        parameters.1,
        "host reported no parameters for the audio unit"
    );
    assert!(
        !parameters.0.is_empty(),
        "AUDelay exposes global parameters, none came back"
    );
    eprintln!(
        "[test] parameters count={} first={:?}",
        parameters.0.len(),
        parameters.0.first().map(|p| (&p.title, &p.unit))
    );

    client
        .get_plugin_state(INSTANCE_ID)
        .expect("send get_plugin_state");
    let state = wait_for(&client, Duration::from_secs(10), |event| match event {
        HostEvent::PluginState {
            ok, component_b64, ..
        } => Some((ok, component_b64)),
        _ => None,
    })
    .expect("host never answered GetPluginState");
    assert!(state.0, "host reported no state for the audio unit");
    assert!(
        !state.1.is_empty(),
        "AU state must carry the ClassInfo plist in component_b64"
    );

    client
        .set_plugin_state(INSTANCE_ID, state.1, String::new())
        .expect("send set_plugin_state");
    let restored = wait_for(&client, Duration::from_secs(10), |event| match event {
        HostEvent::PluginStateSet { ok, .. } => Some(ok),
        _ => None,
    })
    .expect("host never answered SetPluginState");
    assert!(restored, "the unit rejected the state it just produced");

    // Still alive after the restore.
    let after_restore = render_block(&region, &tone).expect("host stopped acknowledging blocks");
    assert!(
        peak(&after_restore) > 0.01,
        "the audio unit went silent after a state restore"
    );

    client
        .unload_plugin(INSTANCE_ID)
        .expect("send unload_plugin");
    assert!(
        wait_for(&client, Duration::from_secs(10), |event| matches!(
            event,
            HostEvent::PluginUnloaded { .. }
        )
        .then_some(()))
        .is_some(),
        "host never confirmed the unload"
    );
    client.shutdown().ok();
}

/// Well-formed id (`oooo`/`oooo`/`oooo`), no such component installed — the
/// failure has to come from the lookup, not from parsing.
const UNINSTALLED_COMPONENT: &str = "au:6f6f6f6f:6f6f6f6f:6f6f6f6f";

#[test]
fn an_unknown_component_fails_the_load_with_a_reason() {
    let mut client = PluginHostClient::spawn_bridge().expect("spawn plugin host");
    assert!(
        wait_for(&client, Duration::from_secs(15), |event| matches!(
            event,
            HostEvent::Ready { .. }
        )
        .then_some(()))
        .is_some(),
        "host never reported Ready"
    );

    client
        .load_au_plugin(
            "track9:insert9",
            UNINSTALLED_COMPONENT,
            SAMPLE_RATE,
            BLOCK_FRAMES,
            None,
        )
        .expect("send load_au_plugin");
    let error = wait_for(&client, Duration::from_secs(20), |event| match event {
        HostEvent::PluginLoadFailed { error, .. } => Some(Some(error)),
        HostEvent::PluginLoaded { .. } => Some(None),
        _ => None,
    })
    .expect("host never answered a bad LoadAudioUnit");
    let error = error.expect("a nonexistent component must not report PluginLoaded");
    assert!(!error.trim().is_empty(), "failure must carry a reason");
    eprintln!("[test] load failure reported: {error}");
    client.shutdown().ok();
}
