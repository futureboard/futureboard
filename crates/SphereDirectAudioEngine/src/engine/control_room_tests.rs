//! Control Room / Listen Bus coverage.
//!
//! These tests drive the *real* graph and the *real* Control Room stage
//! (`backend::render::run_control_room`) rather than a stand-in, so they check
//! the actual signal flow:
//!
//! ```txt
//! tracks/instruments/aux/returns/groups -> master -> router -> listen
//!     -> monitor inserts -> monitor control -> monitoring output pair
//! ```

use super::{
    render_project_block_interleaved, render_project_block_interleaved_with_live_input,
    render_project_block_interleaved_with_taps,
};
use crate::backend::render::run_control_room;
use crate::monitor::{ListenMode, MonitorControl, MonitorOutputTarget, MonitorSource};
use crate::runtime::{RuntimeInsert, RuntimeProject};
use crate::types::{
    EngineInsertSnapshot, EngineProjectSnapshot, EngineRoutingSnapshot, EngineSendSnapshot,
    EngineTrackInputSourceSnapshot, EngineTrackSnapshot,
};
use crate::vst3_processor::RuntimeTransportContext;
use std::collections::HashMap;

const FRAMES: usize = 64;
const CHANNELS: usize = 2;
const SAMPLE_RATE: u32 = 48_000;

fn transport() -> RuntimeTransportContext {
    RuntimeTransportContext {
        tempo_bpm: 120.0,
        time_sig_num: 4,
        time_sig_den: 4,
        project_time_samples: 0,
        ppq_position: 0.0,
        bar_position_ppq: 0.0,
        playing: true,
        recording: false,
    }
}

fn track(id: &str, track_type: &str) -> EngineTrackSnapshot {
    // Audio tracks are input-monitored so the live-input injection path can act
    // as a deterministic signal generator through the whole graph.
    let is_audio = track_type == "audio";
    EngineTrackSnapshot {
        id: id.to_string(),
        track_type: track_type.to_string(),
        volume: 1.0,
        pan: 0.0,
        muted: false,
        solo: false,
        armed: false,
        input_monitor: is_audio,
        input_source: if is_audio {
            EngineTrackInputSourceSnapshot {
                device_id: Some("asio:test".to_string()),
                channels: vec![0, 1],
            }
        } else {
            Default::default()
        },
        preview_mode: "stereo".to_string(),
        output_track_id: None,
        inserts: Vec::new(),
        sends: Vec::new(),
        automation_lanes: Vec::new(),
        builtin_soundfont_player: false,
        soundfont_path: None,
        soundfont_preset_bank: None,
        soundfont_preset_patch: None,
        soundfont_volume: 1.0,
        soundfont_reverb_chorus: true,
        soundfont_polyphony: 64,
        soundfont_envelope: Default::default(),
        soundfont_quality: Default::default(),
    }
}

fn gain_insert(id: &str, gain_db: f32) -> EngineInsertSnapshot {
    let mut params = HashMap::new();
    params.insert("gainDb".to_string(), serde_json::json!(gain_db as f64));
    EngineInsertSnapshot {
        id: id.to_string(),
        kind: "gain".to_string(),
        enabled: true,
        params,
        state: None,
    }
}

fn build(tracks: Vec<EngineTrackSnapshot>) -> RuntimeProject {
    let snapshot = EngineProjectSnapshot {
        project_id: "control-room".to_string(),
        project_root: None,
        preferred_input_device: None,
        bpm: 120.0,
        tempo_points: Vec::new(),
        time_signature: [4, 4],
        sample_rate: SAMPLE_RATE,
        tracks,
        clips: Vec::new(),
        midi_clips: Vec::new(),
        pdc_enabled: true,
        latency_graph_version: 1,
        routing: EngineRoutingSnapshot {
            master_output_device: None,
            sample_rate: SAMPLE_RATE,
            buffer_size: 256,
        },
    };
    RuntimeProject::build(&snapshot, SAMPLE_RATE, &mut HashMap::new(), None, true)
        .expect("control room runtime")
}

/// Build a standalone insert chain for the Control Room by borrowing the
/// runtime's normal insert construction (there is no separate builder for
/// monitor inserts — they are ordinary inserts on a monitor-owned chain).
fn monitor_insert_chain(gain_db: f32) -> Vec<RuntimeInsert> {
    let mut host = track("master", "master");
    host.inserts = vec![gain_insert("monitor-gain", gain_db)];
    build(vec![host]).tracks.remove(0).inserts
}

fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// Render one block through the graph with a constant live-input signal,
/// returning the interleaved master-bus feed exactly as the device callback
/// would see it before the Control Room runs.
fn render_master(runtime: &mut RuntimeProject, level: f32) -> Vec<f32> {
    let input = vec![level; FRAMES];
    let mut data = vec![0.0f32; FRAMES * CHANNELS];
    render_project_block_interleaved_with_live_input(
        runtime, 0, 1.0, &mut data, CHANNELS, true, 4, 4, None, &input, &input,
    );
    data
}

/// Render the graph, then run the real Control Room stage over the result.
/// Returns `(master_feed_before, monitored_after)`.
fn render_and_monitor(runtime: &mut RuntimeProject, level: f32) -> (Vec<f32>, Vec<f32>) {
    let master = render_master(runtime, level);
    let mut data = master.clone();
    run_control_room(&mut data, CHANNELS, runtime, None, transport());
    (master, data)
}

// ── The complete internal mix reaches the Control Room ──────────────────────

#[test]
fn a_normal_audio_track_is_audible_through_monitor() {
    let mut runtime = build(vec![track("audio-1", "audio"), track("master", "master")]);
    assert_eq!(runtime.monitor.source, MonitorSource::MasterBus);

    let (master, monitored) = render_and_monitor(&mut runtime, 0.5);
    assert!(peak(&master) > 0.01, "graph produced no audio to monitor");
    assert_eq!(
        monitored, master,
        "with the default master-bus source and unity monitor gain the Control \
         Room must pass the mix through untouched"
    );
}

#[test]
fn a_virtual_instrument_is_audible_through_monitor() {
    use sphere_soundfont_player::test_font;

    let dir = std::env::temp_dir().join(format!(
        "futureboard-control-room-{}-instrument",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let font_path = dir.join("test.sf2");
    test_font::write_sf2(&font_path).expect("write test soundfont");

    let mut instrument = track("inst-1", "instrument");
    instrument.builtin_soundfont_player = true;
    instrument.soundfont_path = Some(font_path.to_string_lossy().into_owned());
    instrument.soundfont_preset_bank = Some(0);
    instrument.soundfont_preset_patch = Some(0);
    let mut runtime = build(vec![instrument, track("master", "master")]);

    // Hold a note so the instrument actually renders.
    runtime.midi_preview_note_on("inst-1", 0, 64, 100);

    let mut data = vec![0.0f32; FRAMES * CHANNELS];
    let mut instrument_peak = 0.0f32;
    // Give the synth a few blocks to get past its attack.
    for block in 0..8 {
        data.iter_mut().for_each(|s| *s = 0.0);
        render_project_block_interleaved(
            &mut runtime,
            block * FRAMES as u64,
            1.0,
            &mut data,
            CHANNELS,
            true,
            4,
            4,
            None,
        );
        instrument_peak = instrument_peak.max(peak(&data));
    }
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        instrument_peak > 0.0,
        "built-in instrument produced no audio; cannot verify monitoring"
    );
    let master = data.clone();
    run_control_room(&mut data, CHANNELS, &mut runtime, None, transport());
    assert_eq!(
        data, master,
        "instrument audio reaching the master bus must reach the Control Room"
    );
}

#[test]
fn aux_send_and_return_signals_are_audible_through_monitor() {
    let mut source = track("audio-1", "audio");
    // Route the dry path into a muted group so the *only* way signal can reach
    // the monitor is through the send -> return -> master path.
    source.output_track_id = Some("dead-group".to_string());
    source.sends = vec![EngineSendSnapshot {
        id: "send-1".to_string(),
        return_track_id: "return-1".to_string(),
        level: 1.0,
        enabled: true,
        pre_fader: false,
    }];
    let mut dead_group = track("dead-group", "group");
    dead_group.muted = true;

    let mut runtime = build(vec![
        source,
        dead_group,
        track("return-1", "return"),
        track("master", "master"),
    ]);

    let (master, monitored) = render_and_monitor(&mut runtime, 0.5);
    assert!(
        peak(&master) > 0.01,
        "aux/return path produced no audio at the master bus"
    );
    assert_eq!(
        monitored, master,
        "return-channel audio must be audible in the Control Room"
    );
}

#[test]
fn a_group_bus_is_audible_through_monitor() {
    let mut source = track("audio-1", "audio");
    source.output_track_id = Some("group-1".to_string());
    let mut runtime = build(vec![
        source,
        track("group-1", "group"),
        track("master", "master"),
    ]);

    let (master, monitored) = render_and_monitor(&mut runtime, 0.5);
    assert!(peak(&master) > 0.01, "group bus produced no audio");
    assert_eq!(monitored, master);
}

#[test]
fn master_inserts_are_audible_in_monitor() {
    let build_with_master_gain = |gain_db: f32| {
        let mut master = track("master", "master");
        master.inserts = vec![gain_insert("master-gain", gain_db)];
        build(vec![track("audio-1", "audio"), master])
    };

    let mut unity = build_with_master_gain(0.0);
    let mut cut = build_with_master_gain(-12.0);
    let (_, monitored_unity) = render_and_monitor(&mut unity, 0.5);
    let (_, monitored_cut) = render_and_monitor(&mut cut, 0.5);

    let unity_peak = peak(&monitored_unity);
    let cut_peak = peak(&monitored_cut);
    assert!(unity_peak > 0.01, "no signal to compare");
    assert!(
        cut_peak < unity_peak * 0.5,
        "a -12 dB master insert must be heard in the Control Room \
         (unity={unity_peak}, cut={cut_peak})"
    );
}

// ── Monitoring never leaks into export ──────────────────────────────────────

/// The export entry point, rendered with whatever monitor settings are in
/// place. Monitoring must make no difference to the result.
fn render_for_export(runtime: &mut RuntimeProject) -> Vec<f32> {
    let mut data = vec![0.0f32; FRAMES * CHANNELS];
    render_project_block_interleaved_with_taps(
        runtime, 0, 1.0, &mut data, CHANNELS, true, 4, 4, None, None,
    );
    data
}

#[test]
fn monitor_gain_does_not_change_exported_audio() {
    let tracks = || {
        let mut master = track("master", "master");
        master.inserts = vec![gain_insert("master-gain", 0.0)];
        vec![track("audio-1", "audio"), master]
    };
    let mut plain = build(tracks());
    let mut loud = build(tracks());
    loud.monitor.control = MonitorControl {
        gain: 4.0,
        mute: false,
        dim: true,
        mono: true,
    };

    let exported_plain = render_for_export(&mut plain);
    let exported_loud = render_for_export(&mut loud);
    assert_eq!(
        exported_plain, exported_loud,
        "monitor gain/dim/mono must never reach exported audio"
    );

    // And a fully muted Control Room must not silence the export.
    let mut muted = build(tracks());
    muted.monitor.control = MonitorControl {
        gain: 1.0,
        mute: true,
        dim: false,
        mono: false,
    };
    assert_eq!(render_for_export(&mut muted), exported_plain);
}

#[test]
fn monitor_inserts_do_not_appear_in_exported_audio() {
    let tracks = || vec![track("audio-1", "audio"), track("master", "master")];
    let mut plain = build(tracks());
    let mut with_monitor_fx = build(tracks());
    with_monitor_fx.monitor.inserts = monitor_insert_chain(-40.0);
    assert_eq!(with_monitor_fx.monitor.inserts.len(), 1);

    assert_eq!(
        render_for_export(&mut plain),
        render_for_export(&mut with_monitor_fx),
        "monitor inserts must never reach exported audio"
    );

    // Sanity: the same insert *is* audible on the monitoring path, so the
    // equality above is isolation and not an inert insert.
    let (_, monitored) = render_and_monitor(&mut with_monitor_fx, 0.5);
    let (_, monitored_plain) = render_and_monitor(&mut plain, 0.5);
    assert!(
        peak(&monitored) < peak(&monitored_plain) * 0.5,
        "monitor insert had no audible effect on the monitoring path"
    );
}

// ── PFL / AFL ───────────────────────────────────────────────────────────────

#[test]
fn pfl_is_independent_of_the_track_fader() {
    let monitored_at_fader = |volume: f32| {
        let mut source = track("audio-1", "audio");
        source.volume = volume;
        let mut runtime = build(vec![source, track("master", "master")]);
        runtime.tracks[0].listen = ListenMode::Pfl;
        let (_, monitored) = render_and_monitor(&mut runtime, 0.5);
        peak(&monitored)
    };

    let open = monitored_at_fader(1.0);
    let closed = monitored_at_fader(0.0);
    assert!(open > 0.01, "PFL produced no signal");
    assert!(
        (open - closed).abs() < 1.0e-5,
        "PFL must tap before the fader (open={open}, closed={closed})"
    );
}

#[test]
fn afl_follows_the_track_fader() {
    let monitored_at_fader = |volume: f32| {
        let mut source = track("audio-1", "audio");
        source.volume = volume;
        let mut runtime = build(vec![source, track("master", "master")]);
        runtime.tracks[0].listen = ListenMode::Afl;
        let (_, monitored) = render_and_monitor(&mut runtime, 0.5);
        peak(&monitored)
    };

    let open = monitored_at_fader(1.0);
    let half = monitored_at_fader(0.5);
    let closed = monitored_at_fader(0.0);
    assert!(open > 0.01, "AFL produced no signal");
    assert!(
        (half - open * 0.5).abs() < 1.0e-3,
        "AFL must scale with the fader (open={open}, half={half})"
    );
    assert!(closed < 1.0e-6, "a closed fader must silence AFL");
}

#[test]
fn disabling_all_listen_returns_to_master_bus_monitoring() {
    let mut source = track("audio-1", "audio");
    source.volume = 0.0; // silent at the master bus, but not at PFL
    let mut runtime = build(vec![source, track("master", "master")]);

    runtime.tracks[0].listen = ListenMode::Pfl;
    let (_, listening) = render_and_monitor(&mut runtime, 0.5);
    assert!(
        peak(&listening) > 0.01,
        "PFL should be audible even with the fader closed"
    );

    runtime.tracks[0].listen = ListenMode::Off;
    let (master, monitored) = render_and_monitor(&mut runtime, 0.5);
    assert_eq!(
        monitored, master,
        "clearing every Listen must fall back to the selected source, which is \
         the master bus"
    );
    assert!(
        peak(&monitored) < 1.0e-6,
        "with the fader closed the master bus — and so the monitor — is silent"
    );
}

// ── Hardware input is opt-in only ───────────────────────────────────────────

#[test]
fn microphone_input_is_not_activated_unless_explicitly_selected() {
    let mut runtime = build(vec![track("audio-1", "audio"), track("master", "master")]);
    assert_eq!(runtime.monitor.source, MonitorSource::MasterBus);
    assert!(
        !runtime.monitor.source.needs_hardware_input(),
        "the default Control Room source must not request a capture device"
    );

    // Offer a hardware block anyway. With a master-bus source the router must
    // ignore it entirely rather than monitoring the microphone.
    let master = render_master(&mut runtime, 0.5);
    let mut data = master.clone();
    let mic = vec![0.9f32; FRAMES];
    run_control_room(
        &mut data,
        CHANNELS,
        &mut runtime,
        Some((&mic, &mic)),
        transport(),
    );
    assert_eq!(
        data, master,
        "an unselected hardware input must never enter the monitor path"
    );

    // Only after an explicit selection does the input reach the monitor.
    runtime.monitor.source = MonitorSource::HardwareInput("in-1".to_string());
    runtime.resolve_indices();
    let mut data = master.clone();
    run_control_room(
        &mut data,
        CHANNELS,
        &mut runtime,
        Some((&mic, &mic)),
        transport(),
    );
    assert!(
        (peak(&data) - 0.9).abs() < 1.0e-5,
        "an explicitly selected hardware input should be monitored"
    );
}

#[test]
fn a_hardware_input_source_replaces_the_mix_and_cannot_feed_back() {
    let mut runtime = build(vec![track("audio-1", "audio"), track("master", "master")]);
    runtime.monitor.source = MonitorSource::HardwareInput("in-1".to_string());
    runtime.resolve_indices();

    let master = render_master(&mut runtime, 0.5);
    assert!(peak(&master) > 0.01);
    let mut data = master.clone();
    let mic = vec![0.25f32; FRAMES];
    run_control_room(
        &mut data,
        CHANNELS,
        &mut runtime,
        Some((&mic, &mic)),
        transport(),
    );
    // The monitored block is exactly the input — the master mix was replaced,
    // not summed, so the monitored input cannot re-enter the mix and build a
    // feedback loop.
    for frame in data.chunks(CHANNELS) {
        assert!((frame[0] - 0.25).abs() < 1.0e-5);
        assert!((frame[1] - 0.25).abs() < 1.0e-5);
    }
}

// ── Output routing ──────────────────────────────────────────────────────────

#[test]
fn monitoring_an_alternate_pair_leaves_the_main_outs_carrying_the_master_feed() {
    let mut runtime = build(vec![track("audio-1", "audio"), track("master", "master")]);
    runtime.monitor.output = MonitorOutputTarget::new("Out 3-4", 2);
    runtime.monitor.control.gain = 0.5;

    let channels = 4usize;
    let input = vec![0.5f32; FRAMES];
    let mut data = vec![0.0f32; FRAMES * channels];
    render_project_block_interleaved_with_live_input(
        &mut runtime,
        0,
        1.0,
        &mut data,
        channels,
        true,
        4,
        4,
        None,
        &input,
        &input,
    );
    let main_before: Vec<f32> = data.chunks(channels).map(|f| f[0]).collect();
    run_control_room(&mut data, channels, &mut runtime, None, transport());

    let main_after: Vec<f32> = data.chunks(channels).map(|f| f[0]).collect();
    let monitor_out: Vec<f32> = data.chunks(channels).map(|f| f[2]).collect();
    assert_eq!(
        main_after, main_before,
        "monitoring Out 3-4 must not disturb the main outs"
    );
    assert!(peak(&monitor_out) > 0.0, "monitor pair received no signal");
    assert!(
        (peak(&monitor_out) - peak(&main_before) * 0.5).abs() < 1.0e-4,
        "the monitor pair should carry the level-controlled monitor feed"
    );
}

#[test]
fn monitoring_the_main_pair_replaces_it_instead_of_summing_into_it() {
    let mut runtime = build(vec![track("audio-1", "audio"), track("master", "master")]);
    runtime.monitor.control.gain = 0.5;

    let (master, monitored) = render_and_monitor(&mut runtime, 0.5);
    let master_peak = peak(&master);
    let monitored_peak = peak(&monitored);
    assert!(master_peak > 0.01);
    assert!(
        (monitored_peak - master_peak * 0.5).abs() < 1.0e-4,
        "half-gain monitoring of the main pair must replace the master feed \
         (master={master_peak}, monitored={monitored_peak}); a summed path \
         would read ~1.5x instead"
    );
}
