//! CLI test harness for SphereDirectAudioEngine.
//!
//! Usage:
//!   sphere-audio-test list-devices
//!   sphere-audio-test test-tone [--seconds <N>] [--freq <Hz>] [--device <name>]
//!   sphere-audio-test input-test [--seconds <N>] [--device <name>]
//!   sphere-audio-test status
//!
//! Build:
//!   cd crates/SphereDirectAudioEngine
//!   cargo build --bin sphere-audio-test
//!
//! Run (from workspace root):
//!   ./target/debug/sphere-audio-test list-devices

use std::thread;
use std::time::Duration;

use DirectAudio::{device, engine::EngineInner, types::JsDeviceOpenConfig};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("list-devices") => cmd_list_devices(),
        Some("test-tone") => cmd_test_tone(&args[2..]),
        Some("input-test") => cmd_input_test(&args[2..]),
        Some("status") => cmd_status(),
        Some("ks-probe") => cmd_ks_probe(),
        Some(unknown) => {
            eprintln!("Unknown command: {unknown}");
            print_usage();
            std::process::exit(1);
        }
        None => {
            print_usage();
            std::process::exit(1);
        }
    }
}

fn cmd_input_test(args: &[String]) {
    let mut seconds = 3u64;
    let mut device = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seconds" | "-s" => {
                i += 1;
                seconds = args
                    .get(i)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(3);
            }
            "--device" | "-d" => {
                i += 1;
                device = args.get(i).cloned();
            }
            flag => eprintln!("Warning: unknown flag '{flag}' — ignoring"),
        }
        i += 1;
    }

    let engine = EngineInner::new();
    if let Err(error) = engine.start_input_test(device.clone()) {
        eprintln!("Failed to open input: {error}");
        std::process::exit(1);
    }
    println!(
        "Testing input '{}' for {seconds}s…",
        device.as_deref().unwrap_or("system default")
    );
    for _ in 0..seconds * 10 {
        thread::sleep(Duration::from_millis(100));
        let peak = engine.get_input_test_level();
        let bar = "#".repeat((peak * 40.0) as usize);
        eprint!("\r  IN {bar:<40} {peak:.5}");
    }
    eprintln!();
    engine.stop_input_test();
}

// ── Commands ──────────────────────────────────────────────────────────────────

fn cmd_list_devices() {
    println!("=== Output Devices ===");
    let outputs = device::list_output_devices();
    if outputs.is_empty() {
        println!("  (none found)");
    }
    for dev in &outputs {
        let default_tag = if dev.is_default { " [default]" } else { "" };
        println!(
            "  [{backend}] {name}{default_tag}  — {ch}ch @ {sr}Hz",
            backend = dev.backend,
            name = dev.name,
            ch = dev.channels,
            sr = dev.default_sample_rate,
        );
    }

    println!("\n=== Input Devices ===");
    let inputs = device::list_input_devices();
    if inputs.is_empty() {
        println!("  (none found)");
    }
    for dev in &inputs {
        let default_tag = if dev.is_default { " [default]" } else { "" };
        println!(
            "  [{backend}] {name}{default_tag}  — {ch}ch @ {sr}Hz",
            backend = dev.backend,
            name = dev.name,
            ch = dev.channels,
            sr = dev.default_sample_rate,
        );
    }
}

fn cmd_test_tone(args: &[String]) {
    // Parse flags: --seconds N, --freq Hz, --device NAME
    let mut seconds: u64 = 3;
    let mut freq: f32 = 440.0;
    let mut device: Option<String> = None;
    let mut sample_rate: Option<u32> = None;
    let mut buffer_size: Option<u32> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seconds" | "-s" => {
                i += 1;
                seconds = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(3);
            }
            "--freq" | "-f" => {
                i += 1;
                freq = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(440.0);
            }
            "--device" | "-d" => {
                i += 1;
                device = args.get(i).cloned();
            }
            "--rate" | "-r" => {
                i += 1;
                sample_rate = args.get(i).and_then(|v| v.parse().ok());
            }
            "--buffer" | "-b" => {
                i += 1;
                buffer_size = args.get(i).and_then(|v| v.parse().ok());
            }
            flag => {
                eprintln!("Warning: unknown flag '{flag}' — ignoring");
            }
        }
        i += 1;
    }

    println!(
        "Test tone: {freq} Hz for {seconds}s  [device={dev}  rate={sample_rate}  buf={buffer_size}]",
        dev = device.as_deref().unwrap_or("(default)"),
        sample_rate = sample_rate
            .map(|v| format!("{v}Hz"))
            .unwrap_or_else(|| "auto".to_string()),
        buffer_size = buffer_size
            .map(|v| v.to_string())
            .unwrap_or_else(|| "auto".to_string()),
    );

    let engine = EngineInner::new();

    let config = JsDeviceOpenConfig {
        output_device_id: device,
        input_device_id: None,
        sample_rate,
        buffer_size,
    };

    if let Err(e) = engine.open_device(config) {
        eprintln!("Failed to open device: {e}");
        std::process::exit(1);
    }

    if let Err(e) = engine.start() {
        eprintln!("Failed to start stream: {e}");
        std::process::exit(1);
    }

    // Enable test tone.
    engine.set_test_tone(true, freq);

    println!("Playing…  (Ctrl-C to abort early)");

    // Poll meters on the main thread at 10 Hz for the requested duration.
    let total_ticks = seconds * 10;
    for _ in 0..total_ticks {
        thread::sleep(Duration::from_millis(100));
        let m = engine.get_meters();
        // Simple ASCII VU bar (40 chars wide)
        let bar_len = (m.master_peak_l * 40.0) as usize;
        let bar: String = "#".repeat(bar_len.min(40));
        eprint!(
            "\r  L {bar:<40} {pk:.3}  RMS {rms:.3}",
            pk = m.master_peak_l,
            rms = m.master_rms_l,
        );
    }
    eprintln!(); // newline after meter bar

    // Shut down cleanly.
    engine.set_test_tone(false, freq);
    engine.stop();
    engine.close_device();

    println!("Done.");
}

fn cmd_status() {
    let engine = EngineInner::new();
    let st = engine.get_status();
    println!("SphereDirectAudioEngine v{}", st.version);
    println!("  backend:       {}", st.backend_name);
    println!("  stream_open:   {}", st.stream_open);
    println!("  running:       {}", st.running);
    println!("  sample_rate:   {}", st.sample_rate);
    println!("  buffer_size:   {}", st.buffer_size);
    println!(
        "  output_device: {}",
        st.output_device.as_deref().unwrap_or("(none)")
    );
    println!(
        "  input_device:  {}",
        st.input_device.as_deref().unwrap_or("(none)")
    );
    if let Some(err) = &st.last_error {
        println!("  last_error:    {err}");
    }

    println!("\nDefault output devices:");
    for d in device::list_output_devices() {
        if d.is_default {
            println!(
                "  → {} ({} ch @ {} Hz)",
                d.name, d.channels, d.default_sample_rate
            );
        }
    }
}

fn cmd_ks_probe() {
    #[cfg(target_os = "windows")]
    {
        println!("=== WDM-KS interface probe ===");
        print!("{}", DirectAudio::backend::wdm_ks::diagnose_report());
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("ks-probe is Windows-only.");
    }
}

fn print_usage() {
    println!(
        r#"sphere-audio-test — SphereDirectAudioEngine CLI test harness

USAGE:
    sphere-audio-test <COMMAND> [OPTIONS]

COMMANDS:
    list-devices
        List all available audio input and output devices.

    test-tone [OPTIONS]
        Play a sine test tone through the default (or named) output device.

        Options:
          --seconds, -s <N>       Duration in seconds  (default: 3)
          --freq,    -f <Hz>      Frequency in Hz      (default: 440)
          --device,  -d <name>    Output device name   (default: system default)
          --rate,    -r <rate>    Sample rate           (default: device default)
          --buffer,  -b <frames>  Buffer size in frames (default: device default)

    input-test [OPTIONS]
        Open a CoreAudio input stream and display its live peak meter.

        Options:
          --seconds, -s <N>       Duration in seconds  (default: 3)
          --device,  -d <name>    Input device name    (default: system default)

    status
        Print engine version, backend info, and default device list.

    ks-probe   (Windows only)
        Enumerate every WDM-KS audio/render interface and report whether its
        KS pin property set is reachable. Useful for diagnosing why WDM-KS
        cannot open a device on a given machine.

EXAMPLES:
    sphere-audio-test list-devices
    sphere-audio-test test-tone --seconds 5 --freq 880
    sphere-audio-test test-tone --device "Headphones (Realtek Audio)" --seconds 2
    sphere-audio-test status
"#
    );
}
