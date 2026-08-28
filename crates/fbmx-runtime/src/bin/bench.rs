//! Throughput of the scalar reference kernel.
//!
//! ```text
//! cargo run -p fbmx-runtime --release --bin fbmx-bench -- <model.fbmx> [--seconds 2.0]
//! ```
//!
//! Reports µs per block, µs per sample and realtime factor at the block sizes
//! a host actually uses. Numbers are for the scalar `f32` path — there is no
//! vectorised backend yet, and this is the baseline any later one has to beat
//! before the claim is worth making.
//!
//! Caveats worth keeping attached to the numbers: one thread, one core, no
//! attempt to pin affinity or disable frequency scaling, and the machine is
//! doing other things. It measures the right order of magnitude, not the
//! last 5 %.

use std::time::Instant;

use fbmx_runtime::{AudioModel, FbmxModel};

const BLOCK_SIZES: [usize; 7] = [16, 32, 64, 128, 256, 512, 1024];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.first() {
        Some(p) if !p.starts_with("--") => p.clone(),
        _ => {
            eprintln!("usage: fbmx-bench <model.fbmx> [--seconds N] [--rate HZ]");
            std::process::exit(2);
        }
    };
    let flag_value = |name: &str, default: f64| -> f64 {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let audio_seconds = flag_value("--seconds", 2.0);

    let model = match FbmxModel::load(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("could not load {path}: {e}");
            std::process::exit(1);
        }
    };
    let info = model.info().clone();
    let mut engine = match model.instantiate() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("could not instantiate: {e}");
            std::process::exit(1);
        }
    };
    let rate = flag_value("--rate", info.sample_rate as f64);

    println!("model        {path}");
    println!(
        "             {}, hidden {}, input {}, cond {}, {} parameters",
        info.model_type.as_str(),
        engine.hidden_size(),
        engine.input_size(),
        engine.conditioning_dim(),
        info.architecture.parameter_count
    );
    println!("rate         {rate} Hz");
    println!("build        scalar f32 reference kernel\n");
    println!(
        "{:>6}  {:>12}  {:>12}  {:>12}  {:>10}",
        "block", "us/block", "us/sample", "ns/sample", "realtime x"
    );

    // A steady tone rather than silence: denormals in a decayed state would
    // flatter the measurement on some machines.
    let signal: Vec<f32> = (0..8192).map(|i| 0.4 * ((i as f32) * 0.01).sin()).collect();

    for block in BLOCK_SIZES {
        let iterations = ((audio_seconds * rate) as usize / block).max(16);
        let mut buffer = vec![0.0f32; block];
        let mut out = vec![0.0f32; block];

        for _ in 0..8 {
            engine.process_block(&buffer, &mut out);
        }
        engine.reset();

        let start = Instant::now();
        for i in 0..iterations {
            let base = (i * block) % (signal.len() - block);
            buffer.copy_from_slice(&signal[base..base + block]);
            engine.process_block(&buffer, &mut out);
        }
        let elapsed = start.elapsed().as_secs_f64();
        std::hint::black_box(&out);

        let samples = (iterations * block) as f64;
        let us_per_block = elapsed * 1e6 / iterations as f64;
        let us_per_sample = elapsed * 1e6 / samples;
        let realtime = (samples / rate) / elapsed;
        println!(
            "{block:>6}  {us_per_block:>12.3}  {us_per_sample:>12.4}  {:>12.1}  {realtime:>10.1}",
            us_per_sample * 1000.0
        );
    }
}
