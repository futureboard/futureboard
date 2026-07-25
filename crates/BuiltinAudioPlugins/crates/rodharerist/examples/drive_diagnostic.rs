//! Offline drive/amp diagnostic — NOT part of the shipping plugin.
//!
//! Renders test signals through each drive model and prints peak / RMS / crest
//! factor / DC offset / harmonic residual, then sweeps the Drive and Amp Gain
//! knobs to show how level and saturation track them, prints the even/odd
//! harmonic fingerprint of each drive model, and finishes with a pairwise
//! distinction matrix.
//!
//! The sweep is the one to read first. Level must never fall as a gain knob
//! rises — a negative run in its change column is the "turn it down and it
//! gets louder" fault — and THD must keep climbing to the top of the knob, or
//! the last third of its travel is doing nothing. Run with:
//!
//! ```text
//! cargo run -p rodharerist --example drive_diagnostic --release
//! ```

use builtin_dsp_core::StereoEffect;
use rodharerist::{AmpModel, DriveModel, Dsp, PATH_SLOTS, StageKind, default_params};

const SR: f32 = 48_000.0;
const N: usize = 48_000;
const SETTLE: usize = 8_000;

fn drive_dsp(model: DriveModel, gain: f32) -> Dsp {
    let mut dsp = Dsp::new(SR);
    let mut p = default_params();
    p.stage_order = [None; PATH_SLOTS];
    p.stage_order[0] = Some(StageKind::Drive);
    p.drive_model = model;
    p.drive_gain = gain;
    dsp.set_params(p);
    dsp
}

fn amp_dsp(model: AmpModel, gain: f32) -> Dsp {
    let mut dsp = Dsp::new(SR);
    let mut p = default_params();
    p.stage_order = [None; PATH_SLOTS];
    p.stage_order[0] = Some(StageKind::Amp);
    p.amp_model = model;
    p.amp_gain = gain;
    p.amp_master = 5.0;
    dsp.set_params(p);
    dsp
}

fn lcg(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
}

fn render(model: DriveModel, gain: f32, signal: &dyn Fn(usize) -> f32) -> Vec<f32> {
    let mut dsp = drive_dsp(model, gain);
    (0..N)
        .map(|n| {
            let x = signal(n);
            dsp.process_stereo(x, x).0
        })
        .skip(SETTLE)
        .collect()
}

struct Stats {
    peak: f32,
    rms: f32,
    crest_db: f32,
    dc: f32,
}

fn stats(buf: &[f32]) -> Stats {
    let peak = buf.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    let rms = (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt();
    let dc = buf.iter().sum::<f32>() / buf.len() as f32;
    Stats {
        peak,
        rms,
        crest_db: 20.0 * (peak / rms.max(1.0e-9)).log10(),
        dc,
    }
}

/// Probe tone for the sweeps. Deliberately not lower: the tight models cut
/// everything below their coupling corner (Slate reaches 390 Hz wide open), so
/// a 300–400 Hz probe reads *their own tightening* as missing saturation —
/// Recto measures 55% THD at 375 Hz and 91% at 750 Hz for the same setting.
/// Completes exactly 128 cycles in the 8192-sample window, so there is no
/// spectral leakage.
const THD_HZ: f32 = 750.0;

/// Amplitude of one harmonic of [`THD_HZ`], by direct DFT bin.
fn harmonic_magnitude(output: &[f32], harmonic: usize) -> f32 {
    let window = &output[output.len() - 8_192..];
    let frequency = THD_HZ * harmonic as f32;
    let (mut real, mut imag) = (0.0f32, 0.0f32);
    for (n, &sample) in window.iter().enumerate() {
        let phase = n as f32 * frequency * std::f32::consts::TAU / SR;
        real += sample * phase.cos();
        imag -= sample * phase.sin();
    }
    (real * real + imag * imag).sqrt() / window.len() as f32 * 2.0
}

/// Harmonics 2..10 over the fundamental, in percent.
///
/// Unlike [`harmonic_residual`] this is phase-invariant, so filter phase shift
/// (which moves with the Gain knob, because the coupling corners move with it)
/// is not mistaken for saturation.
fn thd_percent(output: &[f32]) -> f32 {
    let fundamental = harmonic_magnitude(output, 1).max(1.0e-9);
    (2..=10).map(|h| harmonic_magnitude(output, h)).sum::<f32>() / fundamental * 100.0
}

/// Energy fraction NOT explained by a pure rescale of the input — a cheap
/// "how much waveshaping actually happened" number.
fn harmonic_residual(input: &[f32], output: &[f32]) -> f32 {
    let dot: f32 = input.iter().zip(output).map(|(a, b)| a * b).sum();
    let in_e: f32 = input.iter().map(|a| a * a).sum();
    let scale = dot / in_e.max(1.0e-9);
    let resid: f32 = input
        .iter()
        .zip(output)
        .map(|(a, b)| (b - a * scale).powi(2))
        .sum();
    let out_e: f32 = output.iter().map(|b| b * b).sum();
    resid / out_e.max(1.0e-9)
}

fn main() {
    let models = [
        DriveModel::DsOne,
        DriveModel::SuperDrive,
        DriveModel::MetalCore,
        DriveModel::TightRift,
        DriveModel::Screamer,
        DriveModel::Minotaur,
        DriveModel::Rat,
        DriveModel::Breaker,
        DriveModel::Fuzz,
        DriveModel::Centurion,
    ];
    let sines = [100.0f32, 440.0, 1_000.0];

    println!("== per-model signal stats (drive=8, level=default) ==");
    for model in models {
        println!("-- {model:?}");
        for &f in &sines {
            let sig = move |n: usize| (n as f32 * 2.0 * std::f32::consts::PI * f / SR).sin() * 0.5;
            let input: Vec<f32> = (0..N).map(sig).skip(SETTLE).collect();
            let out = render(model, 8.0, &sig);
            let s = stats(&out);
            println!(
                "   sine {f:>6.0} Hz  peak={:>6.3}  rms={:>6.3}  crest={:>5.1} dB  dc={:>+8.5}  harm_resid={:>5.1}%",
                s.peak,
                s.rms,
                s.crest_db,
                s.dc,
                harmonic_residual(&input, &out) * 100.0
            );
        }
        // Impulse train (transient behavior) and low-level noise.
        let imp = |n: usize| {
            if n.is_multiple_of(4_800) { 0.9 } else { 0.0 }
        };
        let s = stats(&render(model, 8.0, &imp));
        println!(
            "   impulses        peak={:>6.3}  rms={:>6.3}  crest={:>5.1} dB  dc={:>+8.5}",
            s.peak, s.rms, s.crest_db, s.dc
        );
        let noise_sig = |n: usize| {
            let mut st = 0x600D_F00Du32.wrapping_add(n as u32 * 2_654_435_761);
            lcg(&mut st) * 0.1
        };
        let s = stats(&render(model, 8.0, &noise_sig));
        println!(
            "   noise (-20 dB)  peak={:>6.3}  rms={:>6.3}  crest={:>5.1} dB  dc={:>+8.5}",
            s.peak, s.rms, s.crest_db, s.dc
        );
    }

    println!("\n== level & saturation vs Drive knob ({THD_HZ} Hz @ -6 dBFS) ==");
    println!("   per knob step: rms dBFS (change vs previous step) / THD h2..h10");
    println!(
        "   a negative run in the change column is the \"turn it down, it gets louder\" fault"
    );
    let sweep = [0.0f32, 2.0, 4.0, 6.0, 8.0, 10.0];
    let sig = |n: usize| (n as f32 * 2.0 * std::f32::consts::PI * THD_HZ / SR).sin() * 0.5;
    let sweep_row = |label: String, render_at: &dyn Fn(f32) -> Vec<f32>| {
        print!("-- {label:<11}");
        let mut previous_rms = f32::NAN;
        for &gain in &sweep {
            let out = render_at(gain);
            let rms_db = 20.0 * stats(&out).rms.max(1.0e-9).log10();
            let delta = rms_db - previous_rms;
            print!(
                "  g{gain:>4.1}:{rms_db:>6.1}{} {:>4.0}%",
                if delta.is_nan() {
                    String::from("       ")
                } else {
                    format!("({delta:>+5.1})")
                },
                thd_percent(&out)
            );
            previous_rms = rms_db;
        }
        println!();
    };

    for model in models {
        sweep_row(format!("{model:?}"), &|gain| render(model, gain, &sig));
    }

    // Even vs odd balance is the character fingerprint: odd-only reads as a
    // square/fuzz wall, a strong h2/h4 reads as tube-ish asymmetry.
    println!("\n== harmonic profile at Drive 10 (dB relative to the fundamental) ==");
    print!("   {:<13}", "model");
    for h in 1..=9 {
        print!("  h{h:<5}");
    }
    println!("   h1 abs");
    for model in models {
        let out = render(model, 10.0, &sig);
        let h1 = harmonic_magnitude(&out, 1).max(1.0e-9);
        print!("   {:<13}", format!("{model:?}"));
        for h in 1..=9 {
            print!(
                "{:>6.1} ",
                20.0 * (harmonic_magnitude(&out, h) / h1).max(1.0e-9).log10()
            );
        }
        println!("   {:>5.3}", h1);
    }

    println!("\n== level & saturation vs Amp Gain knob (master 5) ==");
    for model in AmpModel::ALL {
        sweep_row(format!("{model:?}"), &|gain| {
            let mut dsp = amp_dsp(*model, gain);
            (0..N)
                .map(|n| dsp.process_stereo(sig(n), sig(n)).0)
                .skip(SETTLE)
                .collect()
        });
    }

    println!("\n== pairwise distinction (rms of output difference, 440 Hz) ==");
    let outs: Vec<Vec<f32>> = models.iter().map(|&m| render(m, 7.0, &sig)).collect();
    for i in 0..models.len() {
        for j in (i + 1)..models.len() {
            let diff = (outs[i]
                .iter()
                .zip(&outs[j])
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f32>()
                / outs[i].len() as f32)
                .sqrt();
            println!("   {:?} vs {:?}: {:.4}", models[i], models[j], diff);
        }
    }
}
