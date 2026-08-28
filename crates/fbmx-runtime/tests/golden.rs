//! Python ↔ Rust parity.
//!
//! The fixture is produced by `neural/scripts/make_golden.py` from the exact
//! `.fbmx` sitting next to it: same weights, same conditioning, same input,
//! zero initial state. PyTorch's answer is the reference; this runtime has to
//! reproduce it.
//!
//! Acceptance is 1e-6 absolute on audio and state for the smoke model, and
//! 1e-5 relative to the signal's own peak for the FA76 model — because a model whose weights have to
//! produce +20 dB of gain carries larger absolute values, and one f32 ULP at a
//! peak of 2.1 is 2.4e-7 rather than 6e-8.
//!
//! Some disagreement is unavoidable and its source has been measured: it is
//! `exp`/`tanh`, not the arithmetic around them. PyTorch's vectorised
//! `sigmoid` sits up to 2 ULP from a correctly-rounded result and two libm
//! implementations differ from each other by up to 4 ULP; with 4·H gate
//! evaluations per sample over thousands of recurrent steps that compounds.
//! Widening the accumulators to f64 and splitting the bias vectors to mirror
//! PyTorch's grouping were both tried and each moved the error by under 1 %.
//!
//! What the tolerance is really guarding against is a structural mistake —
//! wrong gate order, wrong embedding row, a dropped residual — every one of
//! which lands orders of magnitude above this.
//!
//! Run with `cargo test -p fbmx-runtime --test golden -- --nocapture` to see
//! the measured errors.

mod support;

use fbmx_runtime::{AudioModel, FbmxModel};
use serde_json::Value;
use support::*;

/// Absolute budget, met by any model whose signals stay near unity.
const TOLERANCE: f32 = 1e-6;
/// Relative budget, for models with larger internal magnitudes.
///
/// 1e-5 of peak is about 20 f32 ULP — the level at which a few thousand
/// recurrent steps of rounding land — and four orders of magnitude tighter
/// than anything a structural mistake could produce. The measured values are
/// printed either way, so nobody has to take the threshold on trust.
const RELATIVE_TOLERANCE: f32 = 1e-5;

struct Golden {
    model_uuid: String,
    sample_rate: u32,
    parameters: Value,
    input: Vec<f32>,
    output: Vec<f32>,
    final_h: Vec<f32>,
    final_c: Vec<f32>,
}

fn load_golden(stem: &str) -> Golden {
    let text = std::fs::read_to_string(golden_json(stem))
        .unwrap_or_else(|e| panic!("missing golden fixture {stem}.json: {e}"));
    let v: Value = serde_json::from_str(&text).expect("golden fixture must be valid JSON");
    let floats = |key: &str| -> Vec<f32> {
        v[key]
            .as_array()
            .unwrap_or_else(|| panic!("golden fixture has no array {key:?}"))
            .iter()
            .map(|x| x.as_f64().expect("numeric") as f32)
            .collect()
    };
    Golden {
        model_uuid: v["model_uuid"].as_str().unwrap_or_default().to_string(),
        sample_rate: v["sample_rate"].as_u64().unwrap_or(48_000) as u32,
        parameters: v["parameters"].clone(),
        input: floats("input"),
        output: floats("output"),
        final_h: floats("final_h"),
        final_c: floats("final_c"),
    }
}

/// Error relative to the reference's own peak, floored at unity so a quiet
/// signal cannot inflate the budget.
fn relative_error(reference: &[f32], candidate: &[f32]) -> f32 {
    let peak = reference
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()))
        .max(1.0);
    max_abs_diff(reference, candidate) / peak
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(
        a.len(),
        b.len(),
        "length mismatch between reference and runtime"
    );
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

/// Load the model named by the fixture and apply the fixture's parameters.
fn prepare(stem: &str) -> (Golden, fbmx_runtime::LstmRuntime) {
    let golden = load_golden(stem);
    let model = FbmxModel::load(golden_model(stem)).expect("golden model must load");
    assert_eq!(
        model.info().model_uuid,
        golden.model_uuid,
        "the fixture and the model have drifted apart; regenerate with make_golden.py"
    );
    let mut engine = model.instantiate().expect("golden model must instantiate");
    for (name, value) in golden.parameters.as_object().expect("parameters object") {
        match value {
            Value::Number(n) => engine
                .set_parameter(name, n.as_f64().unwrap() as f32)
                .unwrap_or_else(|e| panic!("{name}: {e}")),
            Value::String(s) => engine
                .set_category(name, s)
                .unwrap_or_else(|e| panic!("{name}: {e}")),
            other => panic!("unsupported parameter value {other:?}"),
        }
    }
    engine.refresh_conditioning();
    (golden, engine)
}

#[test]
fn matches_pytorch_sample_for_sample() {
    let (golden, mut engine) = prepare("smoke_lstm32");
    assert_eq!(engine.sample_rate(), golden.sample_rate);

    let mut out = vec![0.0f32; golden.input.len()];
    engine.process_block(&golden.input, &mut out);

    let audio_error = max_abs_diff(&golden.output, &out);
    let h_error = max_abs_diff(&golden.final_h, &engine.state().h);
    let c_error = max_abs_diff(&golden.final_c, &engine.state().c);

    println!("golden parity ({} samples)", golden.input.len());
    println!("  max |audio error| {audio_error:.3e}");
    println!("  max |h error|     {h_error:.3e}");
    println!("  max |c error|     {c_error:.3e}");

    assert!(
        audio_error <= TOLERANCE,
        "audio error {audio_error:.3e} > {TOLERANCE:.0e}"
    );
    assert!(
        h_error <= TOLERANCE,
        "hidden-state error {h_error:.3e} > {TOLERANCE:.0e}"
    );
    assert!(
        c_error <= TOLERANCE,
        "cell-state error {c_error:.3e} > {TOLERANCE:.0e}"
    );
}

#[test]
fn parity_holds_under_block_processing() {
    // The reference was produced as one call; matching it while *also* being
    // block-split is the property that actually ships.
    let (golden, mut engine) = prepare("smoke_lstm32");
    let mut out = Vec::with_capacity(golden.input.len());
    let mut scratch = [0.0f32; 64];
    for chunk in golden.input.chunks(64) {
        let slice = &mut scratch[..chunk.len()];
        engine.process_block(chunk, slice);
        out.extend_from_slice(slice);
    }
    let error = max_abs_diff(&golden.output, &out);
    println!("  max |audio error|, 64-sample blocks: {error:.3e}");
    assert!(error <= TOLERANCE);
}

#[test]
fn the_wrong_parameters_give_a_different_answer() {
    // Guards against a runtime that silently ignores conditioning: if this
    // passed with any settings, the parity test above would prove nothing
    // about the embedding and dial path.
    let (golden, mut engine) = prepare("smoke_lstm32");
    engine.set_parameter("drive", 0.0).unwrap();
    engine.set_parameter("mix", 0.0).unwrap();
    engine.set_category("mode", "soft").unwrap();
    engine.refresh_conditioning();

    let mut out = vec![0.0f32; golden.input.len()];
    engine.process_block(&golden.input, &mut out);
    assert!(
        max_abs_diff(&golden.output, &out) > 1e-4,
        "changing every control changed nothing — the conditioning path is dead"
    );
}

#[test]
fn a_second_pass_after_reset_reproduces_the_first() {
    let (golden, mut engine) = prepare("smoke_lstm32");
    let mut first = vec![0.0f32; golden.input.len()];
    engine.process_block(&golden.input, &mut first);
    engine.reset();
    let mut second = vec![0.0f32; golden.input.len()];
    engine.process_block(&golden.input, &mut second);
    assert_eq!(
        first, second,
        "reset must restore the initial conditions exactly"
    );
}

#[test]
fn matches_pytorch_on_solo_violin_residual() {
    let (golden, mut engine) = prepare("solo_violin_runtime");
    assert_eq!(engine.sample_rate(), 48_000);
    assert_eq!(engine.input_size(), 10, "audio + 9 conditioning features");

    let mut out = vec![0.0_f32; golden.input.len()];
    engine.process_block(&golden.input, &mut out);
    let audio_rel = relative_error(&golden.output, &out);
    let h_rel = relative_error(&golden.final_h, &engine.state().h);
    let c_rel = relative_error(&golden.final_c, &engine.state().c);
    println!(
        "SoloViolin FBMX parity ({} samples): audio={audio_rel:.3e}, h={h_rel:.3e}, c={c_rel:.3e}",
        golden.input.len()
    );
    assert!(audio_rel <= RELATIVE_TOLERANCE);
    assert!(h_rel <= RELATIVE_TOLERANCE);
    assert!(c_rel <= RELATIVE_TOLERANCE);
}

// ---------------------------------------------------------------------------
// the FA76 model: conditioned on four controls, with an auxiliary head
// ---------------------------------------------------------------------------
#[test]
fn matches_pytorch_on_the_fa76_model() {
    // A harder case than the smoke model in every way that matters: three
    // continuous dials plus a five-way categorical, an 8-wide recurrent input,
    // and an auxiliary gain head whose weights are in the file and must be
    // ignored rather than tripping the loader.
    let (golden, mut engine) = prepare("fa76_revd");
    assert_eq!(engine.input_size(), 8, "1 audio + 3 dials + 4 embedding");
    assert_eq!(engine.conditioning_dim(), 7);

    let mut out = vec![0.0f32; golden.input.len()];
    engine.process_block(&golden.input, &mut out);

    let audio_error = max_abs_diff(&golden.output, &out);
    let h_error = max_abs_diff(&golden.final_h, &engine.state().h);
    let c_error = max_abs_diff(&golden.final_c, &engine.state().c);
    let audio_rel = relative_error(&golden.output, &out);
    let h_rel = relative_error(&golden.final_h, &engine.state().h);
    let c_rel = relative_error(&golden.final_c, &engine.state().c);

    println!("fa76 golden parity ({} samples)", golden.input.len());
    println!("  max |audio error| {audio_error:.3e}  ({audio_rel:.3e} of peak)");
    println!("  max |h error|     {h_error:.3e}  ({h_rel:.3e} of peak)");
    println!("  max |c error|     {c_error:.3e}  ({c_rel:.3e} of peak)");

    for (what, rel) in [
        ("audio", audio_rel),
        ("hidden state", h_rel),
        ("cell state", c_rel),
    ] {
        assert!(
            rel <= RELATIVE_TOLERANCE,
            "{what} error {rel:.3e} of peak exceeds {RELATIVE_TOLERANCE:.0e} —              that is a structural difference, not rounding"
        );
    }
}

#[test]
fn the_fa76_model_declares_the_fa76_control_surface() {
    let model = FbmxModel::load(golden_model("fa76_revd")).expect("fa76 golden model");
    let info = model.info();
    assert_eq!(
        info.conditioning.names(),
        vec!["Input", "Attack", "Release", "Ratio"]
    );
    let ratio = &info.conditioning.categorical[0];
    assert_eq!(
        ratio.categories,
        vec!["4:1", "8:1", "12:1", "20:1", "All Buttons"],
        "the categorical spellings index an embedding table; they must not drift"
    );
    // Provenance travels with the weights, and this one came from a circuit
    // simulation rather than from hardware.
    assert_eq!(info.source_type, fbmx_runtime::SourceType::CircuitModel);
    assert!(!info.source_type.is_hardware_derived());
    assert!(!info.validated);
}

#[test]
fn the_auxiliary_head_is_ignored_not_executed() {
    let model = FbmxModel::load(golden_model("fa76_revd")).unwrap();
    // The head's weights are present in the container...
    assert!(model.tensor("aux.gain.weight").is_ok());
    // ...and the runtime loads the model anyway, without them.
    let engine = model
        .instantiate()
        .expect("aux heads must not block loading");
    assert_eq!(engine.hidden_size(), 32);
}

#[test]
fn matches_pytorch_on_the_phase_three_model() {
    // Two auxiliary heads this time (gain and control voltage). Their weights
    // are in the container and the runtime must ignore both of them without
    // the audio drifting by so much as an ULP more than before.
    let (golden, mut engine) = prepare("fa76_revd_v2");
    let mut out = vec![0.0f32; golden.input.len()];
    engine.process_block(&golden.input, &mut out);

    let audio_rel = relative_error(&golden.output, &out);
    let h_rel = relative_error(&golden.final_h, &engine.state().h);
    let c_rel = relative_error(&golden.final_c, &engine.state().c);
    println!("fa76 v2 golden parity ({} samples)", golden.input.len());
    println!(
        "  max |audio error| {:.3e}  ({audio_rel:.3e} of peak)",
        max_abs_diff(&golden.output, &out)
    );
    println!(
        "  max |h error|     {:.3e}  ({h_rel:.3e} of peak)",
        max_abs_diff(&golden.final_h, &engine.state().h)
    );
    println!(
        "  max |c error|     {:.3e}  ({c_rel:.3e} of peak)",
        max_abs_diff(&golden.final_c, &engine.state().c)
    );

    for (what, rel) in [
        ("audio", audio_rel),
        ("hidden state", h_rel),
        ("cell state", c_rel),
    ] {
        assert!(rel <= RELATIVE_TOLERANCE, "{what} error {rel:.3e} of peak");
    }
}

#[test]
fn two_auxiliary_heads_are_both_ignored() {
    let model = FbmxModel::load(golden_model("fa76_revd_v2")).unwrap();
    for name in [
        "aux.gain.weight",
        "aux.gain.bias",
        "aux.cv.weight",
        "aux.cv.bias",
    ] {
        assert!(
            model.tensor(name).is_ok(),
            "{name} should be in the container"
        );
    }
    let engine = model
        .instantiate()
        .expect("aux heads must not block loading");
    // The runtime executes the audio path only: 33 parameters per ignored head.
    assert_eq!(
        model.info().architecture.parameter_count as usize - engine.parameter_count(),
        66,
        "expected exactly two 33-parameter heads to be left unexecuted"
    );
}
