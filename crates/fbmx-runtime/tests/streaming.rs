//! Block-size independence and state discipline.
//!
//! A host hands the plugin whatever buffer it feels like, and changes it when
//! the user switches audio device. If any temporal state resets at a block
//! boundary the artefact is audible and buffer-size dependent, which is a
//! correctness bug and not a quality one.
//!
//! Here the claim is stronger than "close": `process_block` is a loop over
//! `process_sample` and nothing else, so equality is exact and is asserted as
//! such. A tolerance would let a future optimisation hide a real drift.

mod support;

use fbmx_runtime::{AudioModel, FbmxModel, LstmRuntime};
use support::*;

const BLOCK_SIZES: [usize; 7] = [16, 32, 64, 128, 256, 512, 1024];

fn engine() -> LstmRuntime {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).expect("golden model");
    let mut engine = model.instantiate().expect("instantiate");
    engine.set_parameter("drive", 0.7).unwrap();
    engine.set_category("mode", "hard").unwrap();
    engine.refresh_conditioning();
    engine
}

/// Deterministic, no dependencies: a signal with silence, a step, an impulse
/// and tone, i.e. the places where a state bug shows itself.
fn probe(n: usize) -> Vec<f32> {
    let mut state = 0x2545F491_4F6CDD1Du64;
    let mut rng = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 40) as f32 / 8_388_608.0) - 1.0
    };
    (0..n)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            match i * 4 / n {
                0 => 0.3 * (std::f32::consts::TAU * 50.0 * t).sin(),
                1 => 0.25 * rng(),
                2 => 0.0,
                _ => 0.8 * (std::f32::consts::TAU * 1000.0 * t).sin(),
            }
        })
        .collect()
}

fn process_whole(engine: &mut LstmRuntime, input: &[f32]) -> Vec<f32> {
    engine.reset();
    let mut out = vec![0.0; input.len()];
    engine.process_block(input, &mut out);
    out
}

fn process_in_blocks(engine: &mut LstmRuntime, input: &[f32], sizes: &[usize]) -> Vec<f32> {
    engine.reset();
    let mut out = Vec::with_capacity(input.len());
    let mut scratch = vec![0.0f32; *sizes.iter().max().unwrap()];
    let mut at = 0;
    let mut i = 0;
    while at < input.len() {
        let n = sizes[i % sizes.len()].min(input.len() - at);
        let slice = &mut scratch[..n];
        engine.process_block(&input[at..at + n], slice);
        out.extend_from_slice(slice);
        at += n;
        i += 1;
    }
    out
}

#[test]
fn every_block_size_matches_the_whole_sequence() {
    let mut engine = engine();
    let input = probe(6000);
    let reference = process_whole(&mut engine, &input);

    for size in BLOCK_SIZES {
        let blocked = process_in_blocks(&mut engine, &input, &[size]);
        assert_eq!(blocked.len(), reference.len());
        let error = reference
            .iter()
            .zip(&blocked)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert_eq!(
            error, 0.0,
            "block size {size} changed the output by {error:e}"
        );
    }
}

#[test]
fn randomly_varying_block_sizes_match_too() {
    // Hosts do change buffer size mid-session (freeze, bounce, device switch),
    // and a fixed-size test cannot catch a bug that depends on the sequence of
    // sizes. Includes 1 and a prime, which catch off-by-one buffer handling.
    let mut engine = engine();
    let input = probe(6000);
    let reference = process_whole(&mut engine, &input);
    let schedule = [64, 1, 333, 7, 128, 999, 16, 2, 512, 45];
    let blocked = process_in_blocks(&mut engine, &input, &schedule);
    assert_eq!(blocked, reference);
}

#[test]
fn in_place_processing_matches() {
    let mut engine = engine();
    let input = probe(2048);
    let reference = process_whole(&mut engine, &input);

    engine.reset();
    let mut buffer = input.clone();
    for chunk in buffer.chunks_mut(128) {
        engine.process_block_in_place(chunk);
    }
    assert_eq!(buffer, reference);
}

#[test]
fn state_is_carried_not_reset() {
    // The negative control for the test above: if the engine *did* reset per
    // block, this is the comparison that would differ. It must.
    let mut engine = engine();
    let input = probe(2048);
    let carried = process_in_blocks(&mut engine, &input, &[128]);

    engine.reset();
    let mut per_block_reset = Vec::new();
    let mut scratch = [0.0f32; 128];
    for chunk in input.chunks(128) {
        engine.reset();
        let slice = &mut scratch[..chunk.len()];
        engine.process_block(chunk, slice);
        per_block_reset.extend_from_slice(slice);
    }
    assert_ne!(
        carried, per_block_reset,
        "resetting per block made no difference — the model has no memory to lose"
    );
}

#[test]
fn reset_restores_the_initial_state() {
    let mut engine = engine();
    let input = probe(1024);
    let first = process_whole(&mut engine, &input);
    engine.process_block(&input, &mut vec![0.0; input.len()]); // dirty the state
    let second = process_whole(&mut engine, &input);
    assert_eq!(first, second);
    engine.reset();
    assert!(engine.state().h.iter().all(|v| *v == 0.0));
    assert!(engine.state().c.iter().all(|v| *v == 0.0));
}

#[test]
fn two_engines_do_not_share_state() {
    let model = FbmxModel::load(golden_model("smoke_lstm32")).unwrap();
    let mut a = model.instantiate().unwrap();
    let mut b = model.instantiate().unwrap();
    let input = probe(1024);

    let solo = process_whole(&mut b, &input);
    a.process_block(&vec![0.9; 4096], &mut vec![0.0; 4096]); // drive a somewhere else
    let still_solo = process_whole(&mut b, &input);
    assert_eq!(solo, still_solo);
}

#[test]
fn state_can_be_snapshotted_and_restored() {
    let mut engine = engine();
    let input = probe(2048);
    engine.reset();
    let mut half = vec![0.0; 1024];
    engine.process_block(&input[..1024], &mut half);
    let snapshot = engine.state().clone();

    let mut tail_a = vec![0.0; 1024];
    engine.process_block(&input[1024..], &mut tail_a);

    engine.set_state(snapshot).unwrap();
    let mut tail_b = vec![0.0; 1024];
    engine.process_block(&input[1024..], &mut tail_b);
    assert_eq!(tail_a, tail_b);
}

#[test]
fn non_finite_input_does_not_poison_the_state() {
    // A NaN reaching h and c would silence the model permanently. One bad
    // sample must cost one sample.
    let mut engine = engine();
    let input = probe(512);
    let clean = process_whole(&mut engine, &input);

    engine.reset();
    let mut poisoned = input.clone();
    poisoned[10] = f32::NAN;
    poisoned[11] = f32::INFINITY;
    let mut out = vec![0.0; poisoned.len()];
    engine.process_block(&poisoned, &mut out);

    assert!(
        out.iter().all(|v| v.is_finite()),
        "NaN escaped to the output"
    );
    assert!(engine.state().h.iter().all(|v| v.is_finite()));
    // and it recovers: the tail is close to the clean run again
    let tail_error = clean[400..]
        .iter()
        .zip(&out[400..])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        tail_error < 1e-3,
        "did not recover from a bad sample: {tail_error:e}"
    );
}

#[test]
fn a_parameter_change_lands_on_the_block_boundary() {
    let mut engine = engine();
    let input = probe(1024);
    engine.reset();
    let mut a = vec![0.0; 512];
    engine.process_block(&input[..512], &mut a);

    engine.set_parameter("drive", 0.0).unwrap();
    let mut b = vec![0.0; 512];
    engine.process_block(&input[512..], &mut b);

    // ...and the same run without the change differs, i.e. the new value was
    // actually picked up without an explicit refresh call.
    engine.reset();
    engine.set_parameter("drive", 0.7).unwrap();
    let reference = process_whole(&mut engine, &input);
    let changed: Vec<f32> = a.iter().chain(&b).copied().collect();
    assert_ne!(changed[512..], reference[512..]);
}

#[test]
fn silence_in_gives_a_bounded_output() {
    let mut engine = engine();
    let mut out = vec![0.0; 48_000];
    engine.process_block(&vec![0.0; 48_000], &mut out);
    assert!(out.iter().all(|v| v.is_finite()));
    assert!(
        out.iter().all(|v| v.abs() < 1.0),
        "an untrained-on silence input produced a large output"
    );
}

// ---------------------------------------------------------------------------
// idle compensation
// ---------------------------------------------------------------------------
// A model distilled from a circuit that is silent at rest is not silent at
// rest: its state settles onto a conditioning-dependent fixed point and the
// readout turns that into a constant. On the FA76 models it reached -28 dBFS
// at INPUT 2 against a circuit that puts out exactly zero, and it is what a
// plugin analyser draws as a notch and a peak below 60 Hz while a stepped sine
// sweep of the same model reads flat. See `LstmRuntime::set_idle_compensation`.

fn settle(engine: &mut LstmRuntime, seconds: f64) -> f32 {
    let n = (seconds * engine.sample_rate() as f64) as usize;
    let mut last = 0.0;
    for _ in 0..n {
        last = engine.process_sample(0.0);
    }
    last
}

#[test]
fn without_compensation_the_model_is_not_silent_at_rest() {
    // The defect itself, asserted so that a future model that happens not to
    // have it does not quietly turn this file into a tautology.
    let mut engine = engine();
    assert!(!engine.idle_compensation());
    let idle = settle(&mut engine, 1.0);
    assert!(
        idle.abs() > 1e-7,
        "this golden model is silent at rest ({idle:e}); the compensation          tests below are no longer testing anything"
    );
}

#[test]
fn with_compensation_silence_in_is_silence_out() {
    let mut engine = engine();
    engine.set_idle_compensation(true);
    let idle = settle(&mut engine, 1.0);
    assert!(idle.abs() < 1e-9, "still {idle:e} out with nothing in");
}

#[test]
fn it_follows_the_conditioning_rather_than_holding_one_constant() {
    // The offset is a family of constants, not one: it moves with the
    // controls, which is why a single measured trim would not do.
    let mut engine = engine();
    engine.set_idle_compensation(true);
    for setting in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        engine.set_parameter_at(0, setting);
        engine.refresh_conditioning();
        let idle = settle(&mut engine, 1.0);
        assert!(idle.abs() < 1e-9, "{idle:e} out at setting {setting}");
    }
}

#[test]
fn it_subtracts_a_constant_and_not_a_filter() {
    // The whole reason to do it this way rather than with a high-pass: the twin
    // sees only zeros, so whatever it puts out is a constant, and the
    // difference between compensated and uncompensated output must therefore
    // be exactly that constant at every sample — no frequency dependence, no
    // corner, nothing to argue about in a response measurement.
    let signal: Vec<f32> = (0..4096)
        .map(|i| 0.3 * (i as f32 * 0.05).sin() + 0.1 * (i as f32 * 0.9).sin())
        .collect();

    let mut plain = engine();
    let mut compensated = engine();
    compensated.set_idle_compensation(true);
    // Let both twins reach the fixed point first, so the constant is settled.
    settle(&mut compensated, 1.0);
    settle(&mut plain, 1.0);

    let offset = compensated.idle_output();
    for (i, &x) in signal.iter().enumerate() {
        let a = plain.process_sample(x);
        let b = compensated.process_sample(x);
        assert!(
            (a - b - offset).abs() < 1e-6,
            "sample {i}: difference {} is not the idle constant {offset}",
            a - b
        );
    }
}

#[test]
fn it_can_be_turned_off_again_and_leaves_no_trace() {
    let mut engine = engine();
    engine.set_idle_compensation(true);
    settle(&mut engine, 0.5);
    engine.set_idle_compensation(false);
    assert!(!engine.idle_compensation());
    assert_eq!(engine.idle_output(), 0.0);

    let mut reference = engine_after_reset();
    engine.reset();
    let signal: Vec<f32> = (0..1024).map(|i| 0.2 * (i as f32 * 0.07).sin()).collect();
    for &x in &signal {
        assert_eq!(engine.process_sample(x), reference.process_sample(x));
    }
}

fn engine_after_reset() -> LstmRuntime {
    let mut e = engine();
    e.reset();
    e
}
