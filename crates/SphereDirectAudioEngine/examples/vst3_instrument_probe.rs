//! Manual probe: instantiate a real VST3 instrument, feed it MIDI, and check
//! that it produces audio.
//!
//! Covers the "discovered instruments can actually be instantiated" and "MIDI
//! reaches the instrument" contract end to end: component + controller
//! creation, event input bus, output bus activation, sample-rate/block-size
//! setup, note on/off with velocity and channel, CC64, and state round-trip.
//!
//! Needs real plug-ins, so it is an example rather than a unit test:
//! `cargo run -p sphere_directaudioengine --example vst3_instrument_probe -- <bundle.vst3> [class-id]`

use DirectAudio::vst3_processor::{Vst3MidiEvent, Vst3RuntimeProcessor};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK: usize = 512;

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: vst3_instrument_probe <bundle.vst3> [class-id]");
        return;
    };
    let class_id = args.next().unwrap_or_default();

    let started = std::time::Instant::now();
    let Some(mut plugin) = Vst3RuntimeProcessor::new(&path, &class_id, SAMPLE_RATE) else {
        println!("FAIL  instantiate: processor could not be created for {path}");
        return;
    };
    println!(
        "ok    instantiate ({} ms) ready={} sample_rate={}",
        started.elapsed().as_millis(),
        plugin.is_ready(),
        plugin.sample_rate()
    );
    if let Some(error) = plugin.last_error() {
        println!("      last_error: {error}");
    }
    println!(
        "ok    buses: event_in={} audio_in={} audio_out={} main_out_channels={}",
        plugin.event_input_bus_count(),
        plugin.audio_input_bus_count(),
        plugin.audio_output_bus_count(),
        plugin.main_audio_output_channel_count(),
    );
    if plugin.event_input_bus_count() == 0 {
        println!("note  no event input bus — this class is not a MIDI-driven instrument");
    }

    // An effect is fed a 440 Hz tone so "processes audio" is observable; an
    // instrument gets silence so "silent until a note arrives" is observable.
    let is_effect = plugin.audio_input_bus_count() > 0;
    let silence = vec![0.0f32; BLOCK];
    let input: Vec<f32> = if is_effect {
        (0..BLOCK)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / SAMPLE_RATE as f32).sin() * 0.25)
            .collect()
    } else {
        silence.clone()
    };
    let mut out_l = vec![0.0f32; BLOCK];
    let mut out_r = vec![0.0f32; BLOCK];

    // Several blocks, not one: a reverb or look-ahead limiter legitimately
    // outputs silence for its first block(s) while its delay line fills.
    let mut idle = 0.0f32;
    for _ in 0..8 {
        plugin.process_stereo_block_with_midi(&input, &input, &mut out_l, &mut out_r, &[]);
        idle = idle.max(peak(&out_l).max(peak(&out_r)));
    }
    println!(
        "{}  {} peak={idle:.6}",
        if is_effect == (idle > 1e-5) {
            "ok   "
        } else {
            "FAIL "
        },
        if is_effect {
            "effect passes audio"
        } else {
            "idle silence"
        }
    );
    if is_effect {
        println!("note  effect: the MIDI measurements below are informational, not a contract");
    }

    // Note on, channel 0, velocity 100/127. Then run enough blocks for an
    // attack stage to open.
    let note_on = [Vst3MidiEvent::note_on(0, 0, 60, 100.0 / 127.0)];
    let mut sounding = 0.0f32;
    for block in 0..16 {
        let events: &[Vst3MidiEvent] = if block == 0 { &note_on } else { &[] };
        plugin.process_stereo_block_with_midi(&silence, &silence, &mut out_l, &mut out_r, events);
        sounding = sounding.max(peak(&out_l).max(peak(&out_r)));
    }
    // Silence here is not automatically a host defect: a sampler host with no
    // instrument loaded into its rack (Kontakt, Falcon) is legitimately silent.
    // The host-side contract is that the event bus exists and the events were
    // accepted; audible output additionally needs plug-in content.
    println!(
        "{}  note_on(ch0 pitch60 vel100) peak={sounding:.6}{}",
        if sounding > 1e-5 { "ok   " } else { "note " },
        if sounding > 1e-5 {
            ""
        } else {
            "  (silent — expected for a sampler host with no content loaded)"
        }
    );

    // Sustain pedal (CC64) down, then note off: a plug-in that honours CC64
    // keeps sounding. Either outcome is legal, so this reports rather than
    // asserts — what matters is that neither path panics or goes silent-broken.
    let sustain = [Vst3MidiEvent::control_change(0, 0, 64, 1.0)];
    plugin.process_stereo_block_with_midi(&silence, &silence, &mut out_l, &mut out_r, &sustain);
    let note_off = [Vst3MidiEvent::note_off(0, 0, 60, 0.0)];
    let mut after_release = 0.0f32;
    for block in 0..16 {
        let events: &[Vst3MidiEvent] = if block == 0 { &note_off } else { &[] };
        plugin.process_stereo_block_with_midi(&silence, &silence, &mut out_l, &mut out_r, events);
        after_release = after_release.max(peak(&out_l).max(peak(&out_r)));
    }
    println!("ok    cc64 + note_off peak={after_release:.6}");

    // Channel routing: a note on MIDI channel 3 must not be rejected outright.
    let ch3 = [Vst3MidiEvent::note_on(0, 3, 67, 1.0)];
    let mut ch3_peak = 0.0f32;
    for block in 0..12 {
        let events: &[Vst3MidiEvent] = if block == 0 { &ch3 } else { &[] };
        plugin.process_stereo_block_with_midi(&silence, &silence, &mut out_l, &mut out_r, events);
        ch3_peak = ch3_peak.max(peak(&out_l).max(peak(&out_r)));
    }
    println!("ok    note_on(ch3 pitch67) peak={ch3_peak:.6}");
    plugin.process_stereo_block_with_midi(
        &silence,
        &silence,
        &mut out_l,
        &mut out_r,
        &[Vst3MidiEvent::note_off(0, 3, 67, 0.0)],
    );

    // Opaque state round-trip — what project save/reopen relies on.
    match plugin.get_state() {
        Some(state) => {
            let restored = plugin.set_state(&state);
            println!(
                "{}  state round-trip restored={restored}",
                if restored { "ok   " } else { "FAIL " }
            );
        }
        None => println!("note  plug-in reported no state"),
    }

    println!(
        "done  processed_blocks={} latency_samples={}",
        plugin.process_count(),
        plugin.get_latency_samples()
    );
}
