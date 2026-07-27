//! `FutureboardPluginHostX64.exe` — the separated VST3 plugin/editor host
//! process (IPC *server*).
//!
//! VST3 editor hosting follows public.sdk/samples/vst-hosting/editorhost
//! lifecycle: the host owns the COM STA thread and the editor message pump, and
//! drives `createView`/`attached`/`onSize`/`removed` via the proven C++ backend
//! (`SpherePluginHost::native_editor`). What is new here is *where* it runs:
//! out-of-process, so a crashing plugin editor cannot take down the GPUI main
//! app.
//!
//! In `main_owned_window` mode (Slice 1 default) the **visible editor window is
//! owned by the main app** — this process only receives an HWND over IPC and
//! attaches the VST3 view to it. The host therefore never creates a top-level
//! editor window; it only pumps messages so the attached `IPlugView` repaints.
//!
//! Protocol: [`HostCommand`] frames arrive on **stdin**, [`HostEvent`] frames
//! are written to **stdout**, human logs go to **stderr** behind
//! `FUTUREBOARD_PLUGIN_VIEW_DEBUG`. See [`SpherePluginHost::ipc`].

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::io::{self, BufReader};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use builtin_dsp_core::StereoEffect;
use SpherePluginHost::audio_bridge::{
    bridge_kick_event_name, BridgeKickEvent, SharedAudioRegion, AUDIO_BUF_LEN, MAX_BLOCK_FRAMES,
    MAX_CHANNELS,
};
use SpherePluginHost::ipc::{self, HostCommand, HostEvent, PROTOCOL_VERSION};
use SpherePluginHost::native_editor::{self, EmbedRegion};
use SpherePluginHost::plugin_host_preview::{
    try_start_preview_output, BridgeAudioShared, PluginHostPreviewEngine, SharedPluginHostPreview,
};
use SpherePluginHost::spectrum::{SpectrumAnalyzer, SPECTRUM_BINS};

fn debug_enabled() -> bool {
    // Cached: this is checked on the audio producer's per-block path, which
    // must never take the std env lock.
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").is_some())
}

/// Whether the **temporary** separate-CPAL preview output is allowed.
///
/// Stage 1: this is OFF by default. Plugin DSP output is meant to flow into the
/// main DAW engine (mixer / master / meters), not a second device stream, so we
/// must not "fake success" with a private CPAL stream. Until the shared-memory
/// mix path (Stage 3) lands, preview MIDI is still queued to the VSTi but no
/// audio device is opened and the host logs `dsp_output=pending`. Set
/// `FUTUREBOARD_PLUGIN_HOST_CPAL_PREVIEW=1` to opt into the legacy audition
/// stream for manual testing only.
fn debug_audio_out_enabled() -> bool {
    // Cached: checked on the audio producer's per-block path (see above).
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("FUTUREBOARD_PLUGIN_HOST_DEBUG_AUDIO_OUT")
            .or_else(|_| std::env::var("FUTUREBOARD_PLUGIN_HOST_CPAL_PREVIEW"))
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}

fn log_host_audio_mode() {
    let debug = debug_audio_out_enabled();
    eprintln!("[plugin-host-audio] debug_audio_out={debug}");
    eprintln!(
        "[plugin-host-audio] device_stream={}",
        if debug { "debug_only" } else { "disabled" }
    );
    eprintln!("[plugin-host-dsp] output_to=shared_audio_bridge");
}

macro_rules! hlog {
    ($($arg:tt)*) => {{
        if debug_enabled() {
            eprintln!($($arg)*);
        }
    }};
}

static MAIN_DAW_HWND: AtomicU64 = AtomicU64::new(0);

fn store_main_hwnd(hwnd: Option<u64>) {
    let value = hwnd.unwrap_or(0);
    MAIN_DAW_HWND.store(value, Ordering::SeqCst);
    if value != 0 {
        eprintln!("[PluginHost] main_hwnd received hwnd=0x{value:x}");
    }
}

#[allow(dead_code)]
fn main_hwnd() -> u64 {
    MAIN_DAW_HWND.load(Ordering::SeqCst)
}

fn parse_parent_pid() -> Option<u32> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--parent-pid" {
            return args.next().and_then(|v| v.parse().ok());
        }
    }
    None
}

fn main() {
    let selftest = std::env::args().any(|a| a == "--selftest");
    let parent_pid = parse_parent_pid();

    let _log_path = SpherePluginHost::plugin_host_logging::init_host_logging();
    SpherePluginHost::plugin_host_logging::log_startup_environment();

    // Match the DAW's explicit AppUserModelID so plugin-editor windows this
    // process creates never group as a separate taskbar app (spec: process
    // identity). Owned WS_EX_TOOLWINDOW popups already stay off the taskbar /
    // Alt-Tab; this is belt-and-braces against accidental app-visibility.
    SpherePluginHost::plugin_host_lifecycle::set_futureboard_app_user_model_id();

    platform::com_init();
    platform::ensure_dpi_awareness();
    let pid = std::process::id();
    let thread_id = platform::current_thread_id();
    hlog!("[PluginHostEditor] start pid={pid} thread_id={thread_id} selftest={selftest}");
    if let Some(parent_pid) = parent_pid {
        eprintln!("[plugin-host] parent_pid={parent_pid}");
        eprintln!(
            "[PluginHostProcess] pid={pid} parent_pid={parent_pid} expected_parent=FutureboardStudio"
        );
    }

    // Confirm the main app stripped its renderer-only environment before
    // spawning us (spec Part 1). The host must run with a clean native
    // environment so plugin GPU/WebView/DirectComposition UI can paint.
    log_renderer_env();
    log_runtime_policy();

    if selftest {
        let code = run_selftest();
        platform::com_uninit();
        std::process::exit(code);
    }

    let mut out = io::stdout();
    let _ = ipc::write_frame(
        &mut out,
        &HostEvent::Ready {
            protocol_version: PROTOCOL_VERSION,
            pid,
        },
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    if let Some(parent_pid) = parent_pid {
        let shutdown_flag = shutdown.clone();
        std::thread::Builder::new()
            .name("plugin-host-parent-watch".into())
            .spawn(move || parent_watchdog(parent_pid, shutdown_flag))
            .expect("spawn parent watchdog");
    }

    run_ipc_loop(out, shutdown);
    platform::com_uninit();
}

/// Announce the runtime-ownership policy this host enforces. The external
/// bridge is always authoritative: there is no in-process VST3 runtime and no
/// legacy editor unless `FUTUREBOARD_LEGACY_PLUGIN_EDITOR` is explicitly set.
fn log_runtime_policy() {
    let legacy_enabled = std::env::var_os("FUTUREBOARD_LEGACY_PLUGIN_EDITOR").is_some();
    eprintln!("[plugin-runtime-policy] external_bridge_forced=true");
    eprintln!("[plugin-runtime-policy] legacy_editor_enabled={legacy_enabled}");
    eprintln!("[plugin-runtime-policy] in_process_runtime_allowed=false");
    if legacy_enabled {
        eprintln!(
            "[plugin-runtime-policy] WARNING legacy plugin editor/runtime enabled by FUTUREBOARD_LEGACY_PLUGIN_EDITOR=1"
        );
    }
}

/// Report whether the main-app-only renderer environment leaked into this
/// process. After the spawn-side `sanitize_child_env` fix these should all read
/// `<unset>`; a `set` here means an env var is still being inherited.
fn log_renderer_env() {
    let role = std::env::var("FUTUREBOARD_PROCESS_ROLE").unwrap_or_else(|_| "<unset>".into());
    eprintln!("[plugin-host] FUTUREBOARD_PROCESS_ROLE={role}");
    let dcomp = if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_some() {
        "set"
    } else {
        "<unset>"
    };
    eprintln!("[plugin-host] GPUI_DISABLE_DIRECT_COMPOSITION={dcomp}");
    let leaked = std::env::vars().any(|(k, _)| {
        k.starts_with("GPUI_")
            || k.starts_with("WGPU_")
            || k == "DXGI_PRESENT_ALLOW_TEARING"
            || k == "LIBGL_ALWAYS_SOFTWARE"
    });
    if leaked {
        eprintln!("[plugin-host-env] sanitized=false");
    } else {
        eprintln!("[plugin-host-env] sanitized=true");
    }
}

/// Editor states keyed by `plugin_instance_id` — the in-process
/// `PluginEditorRegistry` role, living inside the host process.
#[derive(Debug, Clone)]
struct EditorState {
    plugin_instance_id: String,
    host_hwnd: u64,
    owner_hwnd: u64,
    display_title: String,
    state: &'static str,
}

type Registry = HashMap<String, EditorState>;

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct LoadedPlugin {
    plugin_path: String,
    class_id: String,
    name: String,
    sample_rate: u32,
    max_block_size: u32,
    processing_ready: bool,
}

type LoadedRegistry = HashMap<String, LoadedPlugin>;

/// The DSP core behind a built-in insert, one variant per bridge-enabled
/// built-in. The variants deliberately do not share a trait: they differ in
/// what they *have* (block-boundary hand-off, meters, latency), and a
/// lowest-common-denominator trait would force every core to pretend it has all
/// three. Keep in sync with `SpherePluginHost::builtin::AUDIO_BRIDGE_STEMS`.
enum BuiltinDsp {
    Rodhareist(rodharerist::Dsp),
    Equz8(equz8::Dsp),
    Verbspace(verbspace::Dsp),
}

/// A built-in processor is created on the IPC thread and then owned exclusively
/// by the audio producer thread. The map only publishes/removes `Arc` handles;
/// no other thread accesses the DSP inside the cell. The one sanctioned
/// exceptions are `nam_loader` and `ir_loader`: `Arc`'d pairs of wait-free
/// hand-off cells the IPC thread uses to submit prepared NAM captures and
/// impulse responses (adopted by the producer at `begin_block`) — neither ever
/// touches the `Dsp` itself.
struct BuiltinHostProcessor {
    dsp: UnsafeCell<BuiltinDsp>,
    /// Analyser over the insert's **pre-DSP** input, for the editor's spectrum
    /// overlay. Pre rather than post on purpose: the editor draws the response
    /// curve on top of it, so an input spectrum shows cause and effect in one
    /// picture, where a post-DSP one would double-count the same processing.
    /// Producer-thread owned, same exclusivity contract as `dsp`.
    spectrum: UnsafeCell<SpectrumAnalyzer>,
    /// Control-side NAM capture loader (safe from the IPC thread). `None` for a
    /// built-in with no capture stage.
    nam_loader: Option<rodharerist::NamLoader>,
    /// Control-side impulse-response loader (safe from the IPC thread). `None`
    /// for a built-in with no cabinet stage.
    ir_loader: Option<rodharerist::IrLoader>,
    /// Engine sample rate the DSP was built at — needed to validate a `.nam`
    /// capture's declared rate on the IPC thread.
    sample_rate: f32,
}

// SAFETY: `process_block` is called only by the single audio producer thread.
// IPC/UI threads only clone or remove the surrounding Arc map entry, or use
// the `nam_loader`/`ir_loader` hand-off cells, thread-safe by construction.
unsafe impl Send for BuiltinHostProcessor {}
unsafe impl Sync for BuiltinHostProcessor {}

impl BuiltinHostProcessor {
    /// Build the DSP for a built-in catalog stem, or `None` when the host has
    /// no core for it. The caller turns `None` into a load failure rather than
    /// publishing a silent instance.
    fn new(stem: &str, sample_rate: u32, state_json: Option<&str>) -> Option<Self> {
        match stem {
            "rodharerist" => Some(Self::rodhareist(sample_rate, state_json)),
            "equz8" => Some(Self::equz8(sample_rate, state_json)),
            "verbspace" => Some(Self::verbspace(sample_rate, state_json)),
            _ => None,
        }
    }

    fn rodhareist(sample_rate: u32, state_json: Option<&str>) -> Self {
        let sr = sample_rate.max(1) as f32;
        let mut dsp = rodharerist::Dsp::new(sr);
        // Apply persisted project state here, before the processor is
        // published to the audio producer — the only window where touching
        // the DSP from this (IPC) thread is race-free by construction.
        if let Some(json) = state_json {
            match rodharerist::RodhareistState::from_json(json) {
                Ok(state) => {
                    dsp.set_params(state.params);
                    eprintln!(
                        "[plugin-host-builtin] restored state schema_version={}",
                        state.schema_version
                    );
                }
                Err(error) => {
                    eprintln!("[plugin-host-builtin] state blob rejected, using defaults: {error}");
                }
            }
        }
        let nam_loader = Some(dsp.nam_loader());
        let ir_loader = Some(dsp.ir_loader());
        Self {
            dsp: UnsafeCell::new(BuiltinDsp::Rodhareist(dsp)),
            spectrum: UnsafeCell::new(SpectrumAnalyzer::new(sr)),
            nam_loader,
            ir_loader,
            sample_rate: sr,
        }
    }

    fn equz8(sample_rate: u32, state_json: Option<&str>) -> Self {
        let sr = sample_rate.max(1) as f32;
        let mut dsp = equz8::Dsp::new(sr);
        // Same pre-publish window as above: the IPC thread still owns the DSP.
        if let Some(json) = state_json {
            match equz8::ipc::Equz8State::from_json(json) {
                Ok(state) => {
                    dsp.set_params(state.params);
                    eprintln!(
                        "[plugin-host-builtin] restored state version={}",
                        state.version
                    );
                }
                Err(error) => {
                    eprintln!("[plugin-host-builtin] state blob rejected, using defaults: {error}");
                }
            }
        }
        Self {
            dsp: UnsafeCell::new(BuiltinDsp::Equz8(dsp)),
            spectrum: UnsafeCell::new(SpectrumAnalyzer::new(sr)),
            // An EQ has no capture or cabinet stage to hand off into.
            nam_loader: None,
            ir_loader: None,
            sample_rate: sr,
        }
    }

    fn verbspace(sample_rate: u32, state_json: Option<&str>) -> Self {
        let sr = sample_rate.max(1) as f32;
        let mut dsp = verbspace::Dsp::new(sr);
        // Same pre-publish window as above: the IPC thread still owns the DSP.
        if let Some(json) = state_json {
            match verbspace::ipc::VerbspaceState::from_json(json) {
                Ok(state) => {
                    dsp.set_params(state.params);
                    eprintln!(
                        "[plugin-host-builtin] restored state version={}",
                        state.version
                    );
                }
                Err(error) => {
                    eprintln!("[plugin-host-builtin] state blob rejected, using defaults: {error}");
                }
            }
        }
        Self {
            dsp: UnsafeCell::new(BuiltinDsp::Verbspace(dsp)),
            spectrum: UnsafeCell::new(SpectrumAnalyzer::new(sr)),
            // A reverb has no capture or cabinet stage to hand off into.
            nam_loader: None,
            ir_loader: None,
            sample_rate: sr,
        }
    }

    fn process_block(&self, in_l: &[f32], in_r: &[f32], interleaved: &mut [f32], frames: usize) {
        // SAFETY: the dedicated producer thread is the sole DSP accessor.
        match unsafe { &mut *self.dsp.get() } {
            BuiltinDsp::Rodhareist(dsp) => {
                // Block boundary: adopt any pending NAM/IR swap (never mid-block).
                dsp.begin_block();
                for i in 0..frames {
                    let (l, r) = dsp.process_stereo(in_l[i], in_r[i]);
                    interleaved[i * 2] = l;
                    interleaved[i * 2 + 1] = r;
                }
            }
            BuiltinDsp::Equz8(dsp) => {
                for i in 0..frames {
                    let (l, r) = dsp.process_stereo(in_l[i], in_r[i]);
                    interleaved[i * 2] = l;
                    interleaved[i * 2 + 1] = r;
                }
            }
            BuiltinDsp::Verbspace(dsp) => {
                for i in 0..frames {
                    let (l, r) = dsp.process_stereo(in_l[i], in_r[i]);
                    interleaved[i * 2] = l;
                    interleaved[i * 2 + 1] = r;
                }
            }
        }
    }

    /// Capture one block into the analyser. Producer thread only. Cheap by
    /// construction — a mono sum into a preallocated ring, no transform.
    fn capture_spectrum(&self, left: &[f32], right: &[f32]) {
        // SAFETY: the dedicated producer thread is the sole accessor.
        unsafe { &mut *self.spectrum.get() }.push_block(left, right);
    }

    /// Run an analysis if one is due, returning the frame to publish. `None`
    /// most blocks: the analyser transforms at its own ~30 Hz rate, not per
    /// block. Producer thread only.
    fn analyze_spectrum(&self) -> Option<[f32; SPECTRUM_BINS]> {
        // SAFETY: the dedicated producer thread is the sole accessor.
        unsafe { &mut *self.spectrum.get() }.analyze().copied()
    }

    /// Latest telemetry frame, or `None` for a built-in that measures nothing.
    /// A core without meters publishes no frame rather than a zeroed one, so
    /// the editor cannot mistake "not measured" for "silence". Producer thread
    /// only (same contract as `process_block`).
    fn meter_frame(&self) -> Option<SpherePluginHost::audio_bridge::BuiltinMeterFrame> {
        // SAFETY: the dedicated producer thread is the sole DSP accessor.
        match unsafe { &*self.dsp.get() } {
            BuiltinDsp::Rodhareist(dsp) => {
                let f = dsp.meter_frame();
                Some(SpherePluginHost::audio_bridge::BuiltinMeterFrame {
                    in_peak: f.in_peak,
                    in_rms: f.in_rms,
                    out_peak: f.out_peak,
                    out_rms: f.out_rms,
                    in_clip: f.in_clip,
                    out_clip: f.out_clip,
                })
            }
            BuiltinDsp::Equz8(_) | BuiltinDsp::Verbspace(_) => None,
        }
    }

    /// Reported latency in samples. For Rodhareist: the NAM capture's receptive
    /// field plus the cabinet IR's convolution partition, each counted only
    /// while the stage carrying it is actually in the path. EQ-Z8 is a cascade
    /// of direct-form biquads and adds none; VerbSpace's pre-delay is a musical
    /// parameter, not a processing delay to compensate. Producer thread only.
    fn latency_samples(&self) -> usize {
        // SAFETY: the dedicated producer thread is the sole DSP accessor.
        match unsafe { &*self.dsp.get() } {
            BuiltinDsp::Rodhareist(dsp) => dsp.latency_samples(),
            BuiltinDsp::Equz8(_) => 0,
            BuiltinDsp::Verbspace(dsp) => dsp.latency_samples(),
        }
    }

    /// Control-rate parameter apply from the shared param ring. Runs only on
    /// the audio producer thread (same exclusivity contract as
    /// [`Self::process_block`]) and is allocation-free in both arms: a
    /// plain-data `Params` write plus coefficient math. Unknown wire indices
    /// are dropped silently — hot path, no logging.
    fn apply_param(&self, param_id: u32, value: f32) {
        // SAFETY: the dedicated producer thread is the sole DSP accessor.
        match unsafe { &mut *self.dsp.get() } {
            BuiltinDsp::Rodhareist(dsp) => {
                if let Some(id) = rodharerist::ui_param_id(param_id) {
                    let _ = dsp.apply_ui_param(id, value);
                }
            }
            // Already the compact wire form the DSP consumes — no id lookup.
            BuiltinDsp::Equz8(dsp) => {
                let _ = dsp.apply_wire_param(param_id, value);
            }
            BuiltinDsp::Verbspace(dsp) => {
                let _ = dsp.apply_wire_param(param_id, value);
            }
        }
    }
}

type SharedBuiltinProcessors = Arc<Mutex<Arc<HashMap<String, Arc<BuiltinHostProcessor>>>>>;

#[cfg(test)]
mod builtin_processor_tests {
    use super::*;

    #[test]
    fn rodhareist_processes_daw_input_to_stereo_output() {
        let processor = BuiltinHostProcessor::rodhareist(48_000, None);
        let in_l = [0.2f32; 32];
        let in_r = [-0.1f32; 32];
        let mut output = [0.0f32; 64];
        processor.process_block(&in_l, &in_r, &mut output, 32);
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().any(|sample| sample.abs() > 1.0e-6));
    }

    /// Wire-index params reach the DSP: powering the unit off through the
    /// shared table's index mutes the output; out-of-range indices are no-ops.
    #[test]
    fn apply_param_routes_wire_indices_into_the_dsp() {
        let processor = BuiltinHostProcessor::rodhareist(48_000, None);
        let power = rodharerist::ui_param_index("power").expect("power in wire table");
        processor.apply_param(power, 0.0);

        let in_l = [0.3f32; 64];
        let in_r = [0.3f32; 64];
        let mut output = [1.0f32; 128];
        processor.process_block(&in_l, &in_r, &mut output, 64);
        // Power off bypasses the chain: output mirrors the dry input.
        assert!(output.iter().all(|sample| sample.is_finite()));

        // Out-of-range index must be a silent no-op.
        processor.apply_param(u32::MAX, 1.0);
        processor.apply_param(rodharerist::UI_PARAM_IDS.len() as u32, 1.0);
        processor.process_block(&in_l, &in_r, &mut output, 64);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    /// A persisted state blob handed to the constructor must configure the DSP
    /// before any processing: power=off state makes the chain a pure bypass,
    /// observable as the output mirroring the dry input exactly.
    #[test]
    fn constructor_state_blob_configures_the_dsp() {
        let mut params = rodharerist::default_params();
        params.power = false;
        let json = rodharerist::RodhareistState::new(params)
            .to_json()
            .expect("state serializes");

        let restored = BuiltinHostProcessor::rodhareist(48_000, Some(&json));
        let in_l = [0.25f32; 32];
        let in_r = [-0.5f32; 32];
        let mut output = [0.0f32; 64];
        restored.process_block(&in_l, &in_r, &mut output, 32);
        for i in 0..32 {
            assert_eq!(output[i * 2], in_l[i], "power-off state must bypass");
            assert_eq!(output[i * 2 + 1], in_r[i], "power-off state must bypass");
        }

        // A corrupt blob must fall back to defaults, not panic.
        let fallback = BuiltinHostProcessor::rodhareist(48_000, Some("not json"));
        fallback.process_block(&in_l, &in_r, &mut output, 32);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    /// Every stem the catalog advertises as bridge-enabled must actually build
    /// here — the two lists are what route an insert into this process.
    #[test]
    fn every_bridge_enabled_stem_constructs() {
        for stem in SpherePluginHost::builtin::AUDIO_BRIDGE_STEMS {
            assert!(
                BuiltinHostProcessor::new(stem, 48_000, None).is_some(),
                "`{stem}` is bridge-enabled but has no DSP in the host"
            );
        }
        assert!(BuiltinHostProcessor::new("compresser", 48_000, None).is_none());
    }

    /// EQ-Z8 shapes the signal rather than passing it: a band with real gain
    /// must change the output, and every wire index must survive the trip.
    #[test]
    fn equz8_processes_and_takes_wire_params() {
        let processor = BuiltinHostProcessor::equz8(48_000, None);
        let in_l = [0.3f32; 64];
        let in_r = [-0.3f32; 64];
        let mut output = [0.0f32; 128];
        processor.process_block(&in_l, &in_r, &mut output, 64);
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().any(|sample| sample.abs() > 1.0e-6));

        // Power off is a pure bypass — the clearest observable param effect.
        let power = equz8::ui_param_index("power").expect("power in wire table");
        processor.apply_param(power, 0.0);
        processor.process_block(&in_l, &in_r, &mut output, 64);
        for i in 0..64 {
            assert_eq!(output[i * 2], in_l[i], "power-off must bypass");
            assert_eq!(output[i * 2 + 1], in_r[i], "power-off must bypass");
        }

        // Out-of-range indices are silent no-ops, not panics.
        processor.apply_param(u32::MAX, 1.0);
        processor.apply_param(equz8::UI_PARAM_IDS.len() as u32, 1.0);
        processor.process_block(&in_l, &in_r, &mut output, 64);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn equz8_restores_state_and_reports_no_latency_or_meters() {
        let mut params = equz8::default_params();
        params.power = false;
        let json = equz8::ipc::Equz8State::new(params)
            .to_json()
            .expect("state serializes");

        let restored = BuiltinHostProcessor::equz8(48_000, Some(&json));
        let in_l = [0.25f32; 32];
        let in_r = [-0.5f32; 32];
        let mut output = [0.0f32; 64];
        restored.process_block(&in_l, &in_r, &mut output, 32);
        for i in 0..32 {
            assert_eq!(output[i * 2], in_l[i], "restored power-off must bypass");
            assert_eq!(output[i * 2 + 1], in_r[i], "restored power-off must bypass");
        }

        // A biquad cascade is zero-latency, and an EQ measures nothing — the
        // frame is absent rather than a zeroed one the editor would read as
        // real silence.
        assert_eq!(restored.latency_samples(), 0);
        assert!(restored.meter_frame().is_none());
        assert!(restored.nam_loader.is_none());
        assert!(restored.ir_loader.is_none());

        // A corrupt blob falls back to defaults instead of panicking.
        let fallback = BuiltinHostProcessor::equz8(48_000, Some("not json"));
        fallback.process_block(&in_l, &in_r, &mut output, 32);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    /// A reverb's output outlives its input, so the check that matters here is
    /// that the tail keeps arriving across block boundaries — a per-block
    /// rebuild of the DSP would silence it after the first block.
    #[test]
    fn verbspace_processes_and_sustains_a_tail_across_blocks() {
        let processor = BuiltinHostProcessor::verbspace(48_000, None);
        let mix = verbspace::ui_param_index("mix").expect("mix in wire table");
        processor.apply_param(mix, 100.0);

        let in_l = [0.3f32; 64];
        let in_r = [-0.3f32; 64];
        let mut output = [0.0f32; 128];
        for _ in 0..16 {
            processor.process_block(&in_l, &in_r, &mut output, 64);
        }
        assert!(output.iter().all(|sample| sample.is_finite()));

        let silence = [0.0f32; 64];
        let mut tail = 0.0f32;
        for _ in 0..16 {
            processor.process_block(&silence, &silence, &mut output, 64);
            tail = output.iter().fold(tail, |peak, s| peak.max(s.abs()));
        }
        assert!(tail > 1.0e-4, "no tail after the input stopped: {tail}");

        // Power off is a pure bypass — the clearest observable param effect.
        let power = verbspace::ui_param_index("power").expect("power in wire table");
        processor.apply_param(power, 0.0);
        processor.process_block(&in_l, &in_r, &mut output, 64);
        for i in 0..64 {
            assert_eq!(output[i * 2], in_l[i], "power-off must bypass");
            assert_eq!(output[i * 2 + 1], in_r[i], "power-off must bypass");
        }

        // Out-of-range indices are silent no-ops, not panics.
        processor.apply_param(u32::MAX, 1.0);
        processor.apply_param(verbspace::UI_PARAM_IDS.len() as u32, 1.0);
        processor.process_block(&in_l, &in_r, &mut output, 64);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn verbspace_restores_state_and_reports_no_latency_or_meters() {
        let mut params = verbspace::default_params();
        params.power = false;
        let json = verbspace::ipc::VerbspaceState::new(params)
            .to_json()
            .expect("state serializes");

        let restored = BuiltinHostProcessor::verbspace(48_000, Some(&json));
        let in_l = [0.25f32; 32];
        let in_r = [-0.5f32; 32];
        let mut output = [0.0f32; 64];
        restored.process_block(&in_l, &in_r, &mut output, 32);
        for i in 0..32 {
            assert_eq!(output[i * 2], in_l[i], "restored power-off must bypass");
            assert_eq!(output[i * 2 + 1], in_r[i], "restored power-off must bypass");
        }

        // The pre-delay is a musical parameter, not lookahead, and the reverb
        // measures nothing — the meter frame is absent rather than zeroed.
        assert_eq!(restored.latency_samples(), 0);
        assert!(restored.meter_frame().is_none());
        assert!(restored.nam_loader.is_none());
        assert!(restored.ir_loader.is_none());

        // A corrupt blob falls back to defaults instead of panicking.
        let fallback = BuiltinHostProcessor::verbspace(48_000, Some("not json"));
        fallback.process_block(&in_l, &in_r, &mut output, 32);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct PendingEditorPrepare {
    prepare_id: u64,
    preferred_width: u32,
    preferred_height: u32,
}

type PendingPrepareRegistry = HashMap<String, PendingEditorPrepare>;

struct DelayedGpuRedraw {
    instance_id: String,
    deadline: Instant,
    second_resize: Option<(u32, u32)>,
}

// Browser/WebView-backed editors can take well over 10s to finish their first
// attach (runtime spin-up + compositor handshake). The whole open is async and
// the app stays fully responsive while we wait, so use a generous bound — long
// enough not to abort a slow-but-healthy editor, still bounded so a genuine
// deadlock fails cleanly instead of hanging forever.
const EDITOR_CREATE_TIMEOUT: Duration = Duration::from_millis(30_000);
const EDITOR_ATTACH_TIMEOUT: Duration = Duration::from_millis(30_000);
const MIN_EDITOR_ATTACH_SIZE: u32 = 16;
const MODULE_LOAD_TIMEOUT: Duration = Duration::from_millis(15_000);
const CREATE_INSTANCE_TIMEOUT: Duration = Duration::from_millis(15_000);
const INITIALIZE_TIMEOUT: Duration = Duration::from_millis(20_000);

#[derive(Debug, Clone)]
struct PendingEditorAttach {
    request_id: u64,
    plugin_instance_id: String,
    parent_hwnd: u64,
    display_title: String,
    started_at: Instant,
    stage: &'static str,
    timeout_logged: bool,
    processor: DirectAudio::vst3_processor::Vst3RuntimeProcessor,
}

struct EditorAttachResult {
    request_id: u64,
    plugin_instance_id: String,
    processor: DirectAudio::vst3_processor::Vst3RuntimeProcessor,
    handle: Option<u64>,
    attach_hwnd: u64,
    owner_hwnd: u64,
    display_title: String,
    preferred_width: u32,
    preferred_height: u32,
    resizable: bool,
    error: Option<String>,
    elapsed: Duration,
}

#[derive(Debug, Clone)]
struct PendingPluginLoad {
    request_id: u64,
    plugin_instance_id: String,
    plugin_path: String,
    class_id: String,
    name: String,
    sample_rate: u32,
    max_block_size: u32,
    started_at: Instant,
    stage: &'static str,
}

struct PluginLoadResult {
    request_id: u64,
    plugin_instance_id: String,
    plugin_path: String,
    class_id: String,
    name: String,
    processor: Option<DirectAudio::vst3_processor::Vst3RuntimeProcessor>,
    error: Option<String>,
    elapsed: Duration,
}

static IDLE_TICK: AtomicU64 = AtomicU64::new(0);
static NEXT_EDITOR_ATTACH_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PLUGIN_LOAD_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn parent_watchdog(parent_pid: u32, shutdown: Arc<AtomicBool>) {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if !platform::is_process_alive(parent_pid) {
            eprintln!("[plugin-host] parent process gone; shutting down");
            shutdown.store(true, Ordering::SeqCst);
            break;
        }
    }
}

fn shutdown_host(
    registry: &mut Registry,
    loaded: &mut LoadedRegistry,
    pending: &mut PendingPrepareRegistry,
    preview: &SharedPluginHostPreview,
    reason: &str,
) {
    let editor_count = registry.len();
    eprintln!("[plugin-host] closing editors count={editor_count} reason={reason}");
    {
        let mut engine = preview.lock();
        for instance_id in registry.drain().map(|(id, _)| id) {
            engine.embed_detach_for_instance(&instance_id);
            engine.unload_instance(&instance_id);
        }
    }
    pending.clear();
    let plugin_count = loaded.len();
    eprintln!("[plugin-host] unloading plugins count={plugin_count}");
    loaded.clear();
    eprintln!("[plugin-host] process exit");
}

/// Stage 3 (host producer side): if the engine has requested a new block via
/// `request_seq`, drain the shared MIDI ring into the loaded VSTi, render one
/// block, write it to `audio_out`, publish the output meters, and acknowledge
/// with `done_seq`. Wait-free handshake — the host never blocks the engine; the
/// engine reads whatever the host last produced (one-block latency).
///
/// Runs on the dedicated audio producer thread (`run_audio_producer`), woken
/// by the engine's kick event per requested block. Removing the
/// `render_single_voice` Vec allocs is a remaining refinement.
fn service_audio_bridge(
    region: &SharedAudioRegion,
    dsp: &BridgeAudioShared,
    builtin: Option<&BuiltinHostProcessor>,
    plugin_instance_id: &str,
) {
    let bridge = region.bridge();
    let req = bridge.request_seq.load(Ordering::Acquire);
    let done = bridge.done_seq.load(Ordering::Relaxed);
    if req == done {
        return; // no new block requested
    }
    let process_started = Instant::now();
    // No engine mutex on the block path: the voice list is an Arc snapshot the
    // engine republishes on load/unload only, so the IPC thread can hold the
    // engine lock across editor attach / plugin load for seconds without
    // starving block production (the old `lock_misses` dropouts).
    let frames = (bridge.block_frames.load(Ordering::Relaxed) as usize).min(MAX_BLOCK_FRAMES);
    // Drain engine-pushed MIDI. Each region's ring belongs to exactly one
    // insert instance, so events are routed to that voice only — never
    // broadcast to every loaded plugin.
    let mut midi_count = 0u32;
    while let Some(ev) = bridge.midi.try_pop() {
        if builtin.is_none() {
            dsp.apply_shared_midi(plugin_instance_id, ev);
        }
        midi_count += 1;
    }
    // stderr is a pipe to the parent process — writing it from the producer
    // thread per block can stall block production if the parent is slow to
    // drain it, so consume traces only exist under the debug flag.
    if midi_count > 0 && debug_enabled() {
        eprintln!(
            "[plugin-host-midi-consume] seq={req} instance={plugin_instance_id} events={midi_count}"
        );
    }
    // Drain engine-pushed parameter automation into this voice (queued for the
    // process() below). Per-instance ring, like MIDI — never broadcast.
    let mut param_count = 0u32;
    while let Some(ev) = bridge.params.try_pop() {
        match builtin {
            // Built-in DSP: apply at block boundary (control-rate; the wire
            // index resolves to an `apply_ui_param` id). `sample_offset` is
            // intentionally ignored.
            Some(b) => b.apply_param(ev.param_id, ev.value),
            None => dsp.apply_shared_param(plugin_instance_id, ev.param_id, ev.value),
        }
        param_count += 1;
    }
    if param_count > 0 && debug_enabled() {
        eprintln!(
            "[plugin-host-param-consume] seq={req} instance={plugin_instance_id} params={param_count}"
        );
    }
    let mut in_l = [0.0f32; MAX_BLOCK_FRAMES];
    let mut in_r = [0.0f32; MAX_BLOCK_FRAMES];
    // SAFETY: the engine owns `audio_in` until it bumps `request_seq`.
    unsafe {
        bridge
            .audio_in
            .read_deinterleaved(&mut in_l[..frames], &mut in_r[..frames], frames);
    }
    // Analyser capture, before the DSP touches anything: the editor's spectrum
    // overlay shows the signal arriving at the insert.
    if let Some(b) = builtin {
        b.capture_spectrum(&in_l[..frames], &in_r[..frames]);
    }
    let output_channels = builtin.map_or_else(
        || {
            dsp.main_audio_output_channel_count_for_instance(plugin_instance_id)
                .unwrap_or_else(|| bridge.plugin_output_channels())
                .max(1)
                .min(MAX_CHANNELS as u32) as usize
        },
        |_| 2,
    );
    bridge.set_plugin_output_channels(output_channels as u32);
    let mut interleaved = [0.0f32; AUDIO_BUF_LEN];
    let len = (frames * output_channels).min(AUDIO_BUF_LEN);
    let produced_channels = if let Some(builtin) = builtin {
        builtin.process_block(
            &in_l[..frames],
            &in_r[..frames],
            &mut interleaved[..len],
            frames,
        );
        2
    } else {
        // Real transport ProcessContext published by the engine for this block.
        let bt = bridge.load_transport();
        let transport = DirectAudio::RuntimeTransportContext {
            tempo_bpm: bt.tempo_bpm,
            time_sig_num: bt.time_sig_num,
            time_sig_den: bt.time_sig_den,
            project_time_samples: bt.project_time_samples,
            ppq_position: bt.ppq_position,
            bar_position_ppq: bt.bar_position_ppq,
            playing: bt.playing,
            recording: bt.recording,
        };
        dsp.render_single_voice_interleaved(
            plugin_instance_id,
            frames,
            &in_l[..frames],
            &in_r[..frames],
            &mut interleaved[..len],
            output_channels,
            transport,
        )
        .max(1)
        .min(output_channels)
    };
    let mut peak_l = 0.0f32;
    let mut peak_r = 0.0f32;
    for i in 0..frames {
        let base = i * output_channels;
        let l = interleaved[base];
        let r = if produced_channels > 1 {
            interleaved[base + 1]
        } else {
            l
        };
        peak_l = peak_l.max(l.abs());
        peak_r = peak_r.max(r.abs());
    }
    let dsp_ready = builtin.is_some()
        || (dsp.dsp_ready() && (dsp.has_loaded_instances() || dsp.continuous_mode()));
    // SAFETY: the host owns `audio_out` for this block — the engine waits on
    // `done_seq` (published below) before reading it.
    unsafe {
        bridge.audio_out.write_interleaved(&interleaved[..len]);
    }
    bridge.store_meters(peak_l, peak_r);
    // Built-in DSP telemetry: publish the DSP's own post-trim meter frame so
    // the editor's meters show what the chain actually saw (producer thread —
    // the sole legal DSP accessor).
    if let Some(frame) = builtin.and_then(BuiltinHostProcessor::meter_frame) {
        bridge.store_builtin_meters(&frame);
    }
    // Analyser frame, at the analyser's own rate — `analyze_spectrum` returns
    // `None` on the blocks in between, so this publishes ~30 times a second
    // rather than once per block.
    if let Some(bins) = builtin.and_then(BuiltinHostProcessor::analyze_spectrum) {
        bridge.store_spectrum(&bins);
    }
    bridge.set_dsp_output_ready(dsp_ready);
    // Publish the plugin's reported latency so the engine can surface it (and,
    // later, compensate it). Refreshed periodically — latency rarely changes,
    // so this avoids an FFI getter + voice lookup on every block.
    static LATENCY_REPORT_BLOCKS: AtomicU64 = AtomicU64::new(0);
    if LATENCY_REPORT_BLOCKS
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(64)
    {
        let latency = if let Some(b) = builtin {
            // A loaded NAM capture's receptive field is the built-in chain's
            // only latency contributor today.
            Some(b.latency_samples().min(i32::MAX as usize) as i32)
        } else {
            dsp.voice_latency_samples(plugin_instance_id)
        };
        if let Some(latency) = latency {
            bridge
                .latency_samples
                .store(latency as u32, Ordering::Relaxed);
        }
    }
    // Throttled and debug-gated: at most one audible-output trace per ~256
    // blocks, and only when audio-out debugging is on — the producer thread
    // must not write the parent's stderr pipe in normal operation.
    static VST3_PROCESS_LOG_BLOCKS: AtomicU64 = AtomicU64::new(0);
    if debug_audio_out_enabled()
        && (peak_l > 0.0001 || peak_r > 0.0001)
        && VST3_PROCESS_LOG_BLOCKS
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(256)
    {
        eprintln!(
            "[vst3-process] instance={plugin_instance_id} frames={frames} channels={produced_channels} midi_events={midi_count} output_peak_l={peak_l:.6} output_peak_r={peak_r:.6}",
        );
    }
    let process_micros = process_started.elapsed().as_micros().min(u32::MAX as u128) as u32;
    bridge
        .last_process_micros
        .store(process_micros, Ordering::Relaxed);
    bridge
        .max_process_micros
        .fetch_max(process_micros, Ordering::Relaxed);
    bridge.done_seq.store(req, Ordering::Release);
}

/// All mapped shared-audio regions, keyed by insert `plugin_instance_id`.
/// All mapped shared-audio regions, keyed by insert `plugin_instance_id`.
///
/// Inner `Arc<HashMap>` (copy-on-write): the producer thread clones the outer
/// `Arc` once per wake (a refcount bump) instead of deep-cloning the whole map
/// (with its `String` keys) every block. The map only changes on
/// attach/unload, which rebuild it via `Arc::make_mut`.
type SharedAudioRegions = Arc<Mutex<Arc<HashMap<String, Arc<SharedAudioRegion>>>>>;

/// Raise this thread to pro-audio scheduling before it produces blocks.
///
/// Two fixes for the "plugin missed deadline; bypassing to dry" dropouts:
/// `timeBeginPeriod(1)` raises the **process** timer resolution so the
/// timeout fallback in the kick-event wait ticks at ~1 ms instead of the
/// 15.6 ms default a background process can be left with (per-process since
/// Win10 2004), and MMCSS "Pro Audio" puts the producer in the same scheduling
/// class as the engine's WASAPI callback thread so it is not preempted by
/// ordinary UI work while rendering a block. Both follow the proven pattern in
/// `SphereDirectAudioEngine::backend::wasapi_exclusive`.
#[cfg(windows)]
fn boost_audio_producer_thread() {
    #[link(name = "winmm")]
    extern "system" {
        fn timeBeginPeriod(u_period: u32) -> u32;
    }
    #[link(name = "avrt")]
    extern "system" {
        fn AvSetMmThreadCharacteristicsW(task_name: *const u16, task_index: *mut u32) -> isize;
    }
    unsafe {
        let timer_res = timeBeginPeriod(1); // 0 == TIMERR_NOERROR
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut task_index = 0u32;
        let mmcss = AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut task_index);
        eprintln!(
            "[plugin-host-audio] producer boost mmcss_pro_audio={} timer_period_1ms={}",
            mmcss != 0,
            timer_res == 0
        );
    }
}

#[cfg(not(windows))]
fn boost_audio_producer_thread() {}

/// Dedicated host audio producer thread. VST3 `process()` runs only here (the
/// editor stays on the STA main thread); the per-voice MIDI mutex inside
/// `BridgeAudioShared` serializes it against IPC MIDI so there is never a
/// concurrent `process()` — without coupling block production to the engine
/// mutex held across plugin load / editor attach.
///
/// Cadence: event-driven. The engine signals `kick` after every `request_seq`
/// bump (see `SharedRegionSink::request_block`), so the producer wakes within
/// scheduler latency of each block request instead of polling on a Windows
/// timer tick it cannot trust. The 1 ms wait timeout is a safety net only —
/// it keeps shutdown responsive and sweeps any request whose signal raced the
/// wait. Without a kick event (no `--parent-pid`, or creation failed) it falls
/// back to the legacy 250 µs sleep poll.
/// Stack for the audio producer thread. The block path's own fixed-size
/// buffers are 272 KiB before any plugin DSP is inlined on top, and the
/// platform default (2 MiB) left no usable margin — measured, the thread wanted
/// between 2.0 and 2.5 MiB. 8 MiB costs nothing but address space (stack pages
/// are committed on use) and takes the whole class of failure off the table.
const AUDIO_PRODUCER_STACK_BYTES: usize = 8 * 1024 * 1024;

fn run_audio_producer(
    regions: SharedAudioRegions,
    builtins: SharedBuiltinProcessors,
    dsp: Arc<BridgeAudioShared>,
    shutdown: Arc<AtomicBool>,
    kick: Option<BridgeKickEvent>,
) {
    boost_audio_producer_thread();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let snapshot = regions
            .lock()
            .map(|map| Arc::clone(&map))
            .unwrap_or_default();
        let builtin_snapshot = builtins
            .lock()
            .map(|map| Arc::clone(&map))
            .unwrap_or_default();
        for (instance_id, region) in snapshot.iter() {
            service_audio_bridge(
                region.as_ref(),
                &dsp,
                builtin_snapshot.get(instance_id).map(Arc::as_ref),
                instance_id,
            );
        }
        // Acknowledge the latest voice-snapshot publish now that any snapshot
        // borrowed for this block has been dropped (lets unload hand the final
        // processor release back to the IPC thread).
        dsp.mark_snapshot_observed();
        match &kick {
            // Wakes immediately on the engine's request signal. A request
            // bumped after the region scan above leaves the auto-reset event
            // signaled, so this wait returns without blocking — no lost
            // wakeups.
            Some(kick) => {
                let _ = kick.wait(1);
            }
            None => std::thread::sleep(Duration::from_micros(250)),
        }
    }
}

fn run_ipc_loop(mut out: io::Stdout, shutdown: Arc<AtomicBool>) {
    // Commands are read on a dedicated thread so the STA/message-pump thread
    // never blocks on stdin (spec Part 9). Each received command kicks the UI
    // thread out of its message wait so IPC latency stays low even while the
    // loop idles in MsgWaitForMultipleObjectsEx.
    let ui_thread_id = platform::current_thread_id();
    let (tx, rx) = crossbeam_channel::unbounded::<HostCommand>();
    std::thread::Builder::new()
        .name("plugin-host-stdin".into())
        .spawn(move || {
            let mut reader = BufReader::new(io::stdin());
            loop {
                match ipc::read_frame::<HostCommand, _>(&mut reader) {
                    Ok(Some(cmd)) => {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                        platform::wake_ui_thread(ui_thread_id);
                    }
                    Ok(None) => {
                        eprintln!("[plugin-host] stdin eof; parent likely exited; shutting down");
                        break;
                    }
                    Err(_) => break,
                }
            }
            platform::wake_ui_thread(ui_thread_id);
        })
        .expect("spawn plugin-host stdin reader");

    let mut registry = Registry::new();
    let mut loaded = LoadedRegistry::new();
    let (load_result_tx, load_result_rx) = crossbeam_channel::unbounded::<PluginLoadResult>();
    let mut pending_plugin_loads: HashMap<String, PendingPluginLoad> = HashMap::new();
    let mut pending_prepare = PendingPrepareRegistry::new();
    let mut delayed_redraws: Vec<DelayedGpuRedraw> = Vec::new();
    let (attach_result_tx, attach_result_rx) = crossbeam_channel::unbounded::<EditorAttachResult>();
    let mut pending_editor_attaches: HashMap<String, PendingEditorAttach> = HashMap::new();
    // Latest requested editor size per instance (coalesced from ResizeEditor
    // commands), applied below with a bounded preview try-lock.
    let mut pending_resizes: HashMap<String, (u32, u32, u32)> = HashMap::new();
    let preview: SharedPluginHostPreview = PluginHostPreviewEngine::shared(48_000, 256);
    let mut preview_output_started = false;
    log_host_audio_mode();
    // Stage 2/3: the mapped shared-memory audio bridge (engine-created), shared
    // with the dedicated audio producer thread. The `Arc` keeps the view mapped
    // for the host's lifetime.
    let region_slots: SharedAudioRegions = Arc::new(Mutex::new(Arc::new(HashMap::new())));
    let builtin_processors: SharedBuiltinProcessors =
        Arc::new(Mutex::new(Arc::new(HashMap::new())));
    {
        let slots = region_slots.clone();
        let builtins = builtin_processors.clone();
        let dsp = preview.lock().bridge_shared();
        let shutdown = shutdown.clone();
        // Producer wake event, shared with the engine-side sink. Keyed by the
        // engine/host pid pair — both sides derive the same name without any
        // protocol change (the engine knows our pid from spawn, we know its
        // pid from `--parent-pid`).
        let kick = parse_parent_pid().and_then(|parent_pid| {
            let name = bridge_kick_event_name(parent_pid, std::process::id());
            match BridgeKickEvent::create_named(&name) {
                Ok(event) => {
                    eprintln!("[plugin-host-audio] kick event ready name={name}");
                    Some(event)
                }
                Err(error) => {
                    eprintln!(
                        "[plugin-host-audio] kick event create failed name={name} error={error}; falling back to 250us poll"
                    );
                    None
                }
            }
        });
        std::thread::Builder::new()
            .name("plugin-host-audio".into())
            // `service_audio_bridge` alone puts 272 KiB of fixed-size block
            // buffers on this thread's stack (`in_l`/`in_r` at MAX_BLOCK_FRAMES
            // plus `interleaved` at MAX_BLOCK_FRAMES * MAX_CHANNELS), and the
            // rest of the inlined block path sat just under the 2 MiB default
            // — close enough that adding DSP to the path overflowed it on
            // thread entry, aborting the whole host process before any plugin
            // was even loaded. Ask for a stack that is not a coincidence.
            .stack_size(AUDIO_PRODUCER_STACK_BYTES)
            .spawn(move || run_audio_producer(slots, builtins, dsp, shutdown, kick))
            .expect("spawn plugin-host audio producer");
    }

    eprintln!(
        "[PluginUIThread] loop started thread_id={}",
        platform::current_thread_id()
    );
    if platform::editor_safe_mode() {
        eprintln!(
            "[PluginEditorSafe] FUTUREBOARD_PLUGIN_EDITOR_SAFE=1 — window-tree polling, \
             per-message verbose logs, attach-time re-entrant pumping, and focus hacks disabled"
        );
    }
    // Pump-gap watchdog: if message dispatch stalls >50ms while an editor is
    // open, the plugin UI freezes (cross-process parenting attaches input
    // queues, so a wedged host thread blocks clicks on plugin dialogs too).
    let mut last_pump_done = Instant::now();
    let mut window_tree: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    // Spin watchdog state: consecutive wakes that claimed input but dispatched
    // nothing (the signature of a 100% CPU pump spin).
    let mut spin_iterations: u32 = 0;
    let mut spin_window_start = Instant::now();
    let mut last_wait_mode: &'static str = "";

    loop {
        if shutdown.load(Ordering::SeqCst) {
            eprintln!("[PluginUIThread] loop exited reason=parent_watchdog");
            shutdown_host(
                &mut registry,
                &mut loaded,
                &mut pending_prepare,
                &preview,
                "parent_watchdog",
            );
            return;
        }
        let mut slowest_section: &'static str = "none";
        let mut slowest_section_ms: u128 = 0;
        macro_rules! timed_section {
            ($name:expr, $body:expr) => {{
                let started = Instant::now();
                let result = $body;
                let elapsed = started.elapsed().as_millis();
                if elapsed > slowest_section_ms {
                    slowest_section_ms = elapsed;
                    slowest_section = $name;
                }
                result
            }};
        }

        // 1. Drain and dispatch every queued command. Each dispatch is timed:
        //    long handlers (plugin load, editor attach) block this pump and —
        //    because cross-process parenting attaches input queues — block
        //    clicks on plugin windows too. The watchdog below names them.
        loop {
            match rx.try_recv() {
                Ok(cmd) => {
                    if matches!(cmd, HostCommand::Shutdown) {
                        hlog!("[PluginHostEditor] shutdown requested");
                        eprintln!("[PluginUIThread] loop exited reason=ipc_shutdown");
                        shutdown_host(
                            &mut registry,
                            &mut loaded,
                            &mut pending_prepare,
                            &preview,
                            "ipc_shutdown",
                        );
                        return;
                    }
                    timed_section!("ipc_dispatch", {
                        dispatch(
                            cmd,
                            &mut registry,
                            &mut loaded,
                            &mut pending_plugin_loads,
                            &load_result_tx,
                            &mut pending_prepare,
                            &mut delayed_redraws,
                            &mut pending_resizes,
                            &mut pending_editor_attaches,
                            &attach_result_tx,
                            &preview,
                            &mut preview_output_started,
                            &region_slots,
                            &builtin_processors,
                            &mut out,
                        )
                    });
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    eprintln!("[plugin-host] stdin eof; parent likely exited; shutting down");
                    eprintln!("[PluginUIThread] loop exited reason=stdin_disconnect");
                    shutdown_host(
                        &mut registry,
                        &mut loaded,
                        &mut pending_prepare,
                        &preview,
                        "stdin_disconnect",
                    );
                    return;
                }
            }
        }

        timed_section!("plugin_load_results", {
            drain_plugin_load_results(
                &mut loaded,
                &mut pending_plugin_loads,
                &load_result_rx,
                &preview,
                &mut out,
            );
            expire_plugin_load_requests(&mut pending_plugin_loads, &load_result_rx, &mut out);
        });

        timed_section!("editor_attach_results", {
            drain_editor_attach_results(
                &mut registry,
                &mut delayed_redraws,
                &mut pending_editor_attaches,
                &attach_result_rx,
                &preview,
                &mut out,
            );
            expire_editor_attach_requests(
                &mut pending_editor_attaches,
                &attach_result_rx,
                &preview,
                &mut out,
            );
        });

        // 1b. Apply coalesced editor resizes (latest size per instance). The
        //     processor clone is fetched with a bounded try-lock so a busy DSP
        //     block can never stall the pump during an interactive resize;
        //     entries that miss the lock are retried next tick (≤8ms away).
        timed_section!("editor_resize", {
            if !pending_resizes.is_empty() {
                // Clone processor handles under a short bounded lock, then
                // apply the (possibly slow) onSize work with the lock RELEASED
                // so the audio producer never waits on editor UI work.
                type ResizeJob = (
                    String,
                    u32,
                    u32,
                    u32,
                    DirectAudio::vst3_processor::Vst3RuntimeProcessor,
                );
                let jobs: Option<(Vec<ResizeJob>, Vec<String>)> = preview
                    .try_lock_for(Duration::from_millis(2))
                    .map(|engine| {
                        let mut jobs = Vec::new();
                        let mut gone = Vec::new();
                        for (instance_id, (width, height, dpi)) in pending_resizes.iter() {
                            match engine.clone_processor_for(instance_id) {
                                Some(processor) => jobs.push((
                                    instance_id.clone(),
                                    *width,
                                    *height,
                                    *dpi,
                                    processor,
                                )),
                                None => gone.push(instance_id.clone()),
                            }
                        }
                        (jobs, gone)
                    });
                if let Some((jobs, gone)) = jobs {
                    for instance_id in gone {
                        pending_resizes.remove(&instance_id); // unloaded — drop request
                    }
                    for (instance_id, width, height, dpi, processor) in jobs {
                        eprintln!(
                            "[plugin-bridge] ResizeEditor instance={instance_id} \
                             width={width} height={height} dpi={dpi}"
                        );
                        processor.embed_set_bounds(0, 0, width as i32, height as i32);
                        processor.embed_refresh();
                        pending_resizes.remove(&instance_id);
                    }
                }
            }
        });

        // 2. Keep attached editors painting / geometry in sync, and pump our own
        //    message queue so the foreign-parented IPlugView gets messages.
        //    The preview-engine mutex is shared with the DSP producer thread —
        //    this UI thread must NEVER block on it inside the pump path: use
        //    short bounded try-locks and skip the tick when the lock is busy.
        let user_closed_editors: Vec<String> = timed_section!("editor_refresh", {
            let mut user_closed: Vec<String> = Vec::new();
            let refresh_targets: Option<
                Vec<(String, DirectAudio::vst3_processor::Vst3RuntimeProcessor)>,
            > = preview
                .try_lock_for(Duration::from_millis(2))
                .map(|engine| {
                    registry
                        .keys()
                        .filter_map(|id| {
                            engine
                                .clone_processor_for(id)
                                .map(|processor| (id.clone(), processor))
                        })
                        .collect()
                });
            if let Some(refresh_targets) = refresh_targets {
                for (instance_id, processor) in refresh_targets {
                    // Host-owned (detached) window: the user can close it via its
                    // own titlebar. Detect that here and report EditorClosed so
                    // the main app drops the session and Open works again. The
                    // audio instance stays alive (we only detach the editor).
                    if processor.embed_take_user_close() {
                        user_closed.push(instance_id.clone());
                        continue;
                    }
                    processor.embed_refresh();
                    // Safe mode: no extra per-editor pump here — the main
                    // `pump_messages` below drains the whole thread queue.
                    if !platform::editor_safe_mode() {
                        if let Some(host_hwnd) =
                            registry.get(&instance_id).map(|state| state.host_hwnd)
                        {
                            platform::pump_editor_messages(host_hwnd);
                        }
                    }
                }
            }
            user_closed
        });
        for instance_id in user_closed_editors {
            eprintln!(
                "[PluginEditor] user closed host-owned editor window instance={instance_id} (instance stays active)"
            );
            registry.remove(&instance_id);
            pending_resizes.remove(&instance_id);
            pending_editor_attaches.remove(&instance_id);
            // Detach the editor view (not the audio instance). Blocking lock,
            // matching the CloseEditor command handler — this is a rare one-shot
            // on user close, never a per-tick operation, so it cannot spin the
            // pump on the DSP mutex.
            preview.lock().editor_detach_for_instance(&instance_id);
            let _ = ipc::write_frame(
                &mut out,
                &HostEvent::EditorClosed {
                    plugin_instance_id: instance_id,
                },
            );
        }
        timed_section!("resize_poll", {
            let resizes = preview
                .try_lock_for(Duration::from_millis(2))
                .map(|engine| engine.poll_pending_editor_resizes())
                .unwrap_or_default();
            for (instance_id, width, height) in resizes {
                eprintln!(
                    "[PluginEditor] top window resize notify instance={instance_id} content={width}x{height}"
                );
                let _ = ipc::write_frame(
                    &mut out,
                    &HostEvent::EditorContentResize {
                        plugin_instance_id: instance_id,
                        width,
                        height,
                        dpi: platform::system_dpi(),
                    },
                );
            }
        });
        let now = Instant::now();
        timed_section!("delayed_redraw", {
            delayed_redraws.retain(|entry| {
                if now >= entry.deadline {
                    let processor = preview
                        .try_lock_for(Duration::from_millis(2))
                        .and_then(|engine| engine.clone_processor_for(&entry.instance_id));
                    let Some(processor) = processor else {
                        // Lock busy — keep the entry and retry next tick.
                        return true;
                    };
                    processor.embed_refresh();
                    if let Some((width, height)) = entry.second_resize {
                        eprintln!(
                            "[PluginEditorLifecycle] second resize instance={} size={}x{}",
                            entry.instance_id, width, height
                        );
                        eprintln!("[editor-size] delayed resize = {}x{}", width, height);
                        processor.embed_set_bounds(0, 0, width as i32, height as i32);
                        processor.embed_refresh();
                    }
                    if !platform::editor_safe_mode() {
                        if let Some(host_hwnd) = registry
                            .get(&entry.instance_id)
                            .map(|state| state.host_hwnd)
                        {
                            platform::pump_editor_messages(host_hwnd);
                        }
                    }
                    false
                } else {
                    true
                }
            });
        });
        platform::set_editor_roots(registry.values().map(|state| state.host_hwnd).collect());
        let dispatched = platform::pump_messages();
        // Freeze watchdog tiers (spec item 10):
        //  >50ms   name the slow section,
        //  >1000ms dump the window/thread snapshot,
        //  >3000ms notify the main app so it can surface "not responding"
        //          (the wrapper + close path live in the main process, so the
        //          user can always close a wedged editor).
        let gap_ms = last_pump_done.elapsed().as_millis() as u64;
        if !registry.is_empty() {
            if gap_ms > 50 {
                eprintln!(
                    "[PluginUIThread] pump gap ms={gap_ms} suspected_block={slowest_section} \
                     section_ms={slowest_section_ms} dispatched={dispatched}"
                );
            }
            if gap_ms > 1000 {
                platform::plugin_editor_snapshot("pump_gap");
            }
            if gap_ms > 3000 {
                eprintln!(
                    "[PluginUIThread] editor not responding gap_ms={gap_ms} notifying_main_app=true"
                );
                for instance_id in registry.keys() {
                    let _ = ipc::write_frame(
                        &mut out,
                        &HostEvent::EditorUnresponsive {
                            plugin_instance_id: instance_id.clone(),
                            gap_ms,
                        },
                    );
                }
            }
        }
        last_pump_done = Instant::now();

        // Stage 3 block production runs on the dedicated `plugin-host-audio`
        // thread (see `run_audio_producer`) — not here — so the engine's block
        // rate is met instead of throttled to this ~120 Hz idle loop.

        let tick = IDLE_TICK.fetch_add(1, Ordering::Relaxed);
        if platform::plugin_debug()
            && !platform::editor_safe_mode()
            && !registry.is_empty()
            && tick.is_multiple_of(120)
        {
            // Track plugin-created child/popup/dialog windows. Throttled to
            // ~1/sec (spec item 2); fully disabled in safe mode.
            let roots: Vec<u64> = registry.values().map(|state| state.host_hwnd).collect();
            platform::log_window_tree_changes(&roots, &mut window_tree);
        }
        if tick.is_multiple_of(60) {
            eprintln!(
                "[PluginUIThread] loop alive editor_count={}",
                registry.len()
            );
            eprintln!("[plugin-host-ui-thread] message_loop_running=true");
            eprintln!("[plugin-host-ui-thread] editor_count={}", registry.len());
            eprintln!("[plugin-host-ui-thread] idle_tick={tick}");
            if let Ok(slots) = region_slots.lock() {
                eprintln!(
                    "[plugin-host-bridge] shared_audio mapped_regions={}",
                    slots.len()
                );
                for (instance_id, region) in slots.iter() {
                    let bridge = region.bridge();
                    let (peak_l, peak_r) = bridge.meters();
                    eprintln!(
                        "[plugin-host-bridge] instance={instance_id} request_seq={} done_seq={} xruns={} process_us={} max_process_us={} dsp_output={} peak_l={peak_l:.3} peak_r={peak_r:.3}",
                        bridge.request_seq.load(Ordering::Relaxed),
                        bridge.done_seq.load(Ordering::Relaxed),
                        bridge.xrun_count.load(Ordering::Relaxed),
                        bridge.last_process_micros.load(Ordering::Relaxed),
                        bridge.max_process_micros.load(Ordering::Relaxed),
                        if bridge.dsp_output_ready() {
                            "ready"
                        } else {
                            "pending"
                        }
                    );
                }
            }
        }

        // 3. Wait for input instead of busy-polling (spec item 3): the loop
        //    idles in MsgWaitForMultipleObjectsEx and wakes immediately on any
        //    queued message or a wake_ui_thread kick from the stdin reader.
        //    With editors open the timeout keeps the old ~120 Hz refresh
        //    cadence; idle it stretches to 50ms. CPU is ~0% when nothing
        //    happens either way.
        let (wait_ms, wait_mode): (u32, &'static str) = if registry.is_empty() {
            (50, "idle_msgwait_50ms")
        } else {
            (8, "editor_msgwait_8ms")
        };
        if wait_mode != last_wait_mode {
            eprintln!("[PluginUIThread] idle wait mode={wait_mode}");
            last_wait_mode = wait_mode;
        }
        let woke_on_input = platform::wait_for_input(wait_ms);
        // Spin watchdog (spec item 3): repeated "input available" wakes that
        // then dispatch nothing means the queue is being signalled without
        // producing messages — the loop would spin at 100% CPU. Name it.
        if woke_on_input && dispatched == 0 {
            spin_iterations += 1;
            if spin_iterations >= 200 {
                eprintln!(
                    "[PluginUIThread] spin warning iterations={spin_iterations} messages=0 \
                     duration={:?}",
                    spin_window_start.elapsed()
                );
                spin_iterations = 0;
                spin_window_start = Instant::now();
            }
        } else {
            spin_iterations = 0;
            spin_window_start = Instant::now();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    cmd: HostCommand,
    registry: &mut Registry,
    loaded: &mut LoadedRegistry,
    pending_plugin_loads: &mut HashMap<String, PendingPluginLoad>,
    load_result_tx: &crossbeam_channel::Sender<PluginLoadResult>,
    _pending_prepare: &mut PendingPrepareRegistry,
    delayed_redraws: &mut Vec<DelayedGpuRedraw>,
    pending_resizes: &mut HashMap<String, (u32, u32, u32)>,
    pending_editor_attaches: &mut HashMap<String, PendingEditorAttach>,
    attach_result_tx: &crossbeam_channel::Sender<EditorAttachResult>,
    preview: &SharedPluginHostPreview,
    preview_output_started: &mut bool,
    region_slots: &SharedAudioRegions,
    builtin_processors: &SharedBuiltinProcessors,
    out: &mut io::Stdout,
) {
    match cmd {
        HostCommand::Hello {
            protocol_version,
            main_hwnd,
            session_id,
        } => {
            if protocol_version != PROTOCOL_VERSION {
                hlog!(
                    "[PluginHostEditor] protocol mismatch client={protocol_version} host={PROTOCOL_VERSION}"
                );
            }
            store_main_hwnd(main_hwnd);
            if let Some(session) = session_id.as_deref() {
                eprintln!("[PluginHost] ipc hello session_id={session}");
            }
        }
        HostCommand::Ping => {
            hlog!("[PluginHostEditor] Ping → Pong");
            let _ = ipc::write_frame(
                out,
                &HostEvent::Pong {
                    pid: std::process::id(),
                },
            );
        }
        HostCommand::LoadPlugin {
            plugin_instance_id,
            plugin_path,
            class_id,
            sample_rate,
            max_block_size,
        } => {
            hlog!(
                "[plugin-host] LoadPlugin instance={plugin_instance_id} path={plugin_path} class_id={class_id} sr={sample_rate} block={max_block_size}"
            );
            if !std::path::Path::new(&plugin_path).exists() {
                let error = format!("plugin path not found: {plugin_path}");
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::PluginLoadFailed {
                        plugin_instance_id,
                        error,
                    },
                );
                return;
            }
            let name = std::path::Path::new(&plugin_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("VST3 Plugin")
                .to_string();
            if loaded.contains_key(&plugin_instance_id) {
                eprintln!(
                    "[plugin-host] LoadPlugin instance={plugin_instance_id} already_loaded=true reuse=true"
                );
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::PluginAlreadyLoaded {
                        plugin_instance_id,
                        name,
                    },
                );
                return;
            }
            if pending_plugin_loads.contains_key(&plugin_instance_id) {
                eprintln!(
                    "[plugin-host] LoadPlugin instance={plugin_instance_id} already_pending=true"
                );
                return;
            }
            let _ = ipc::write_frame(
                out,
                &HostEvent::PluginLoading {
                    plugin_instance_id: plugin_instance_id.clone(),
                },
            );
            // Default: create the controller on THIS persistent, message-pumped
            // main thread (not an ephemeral worker that dies). The editor attach
            // runs on this same thread, so a native editor's `attached()` send to
            // its controller-time window is a direct same-thread call — the fix
            // for the live AD2 `attached()` deadlock. See
            // `plugin_lifecycle_on_main_thread`.
            if plugin_lifecycle_on_main_thread() {
                let request_id = NEXT_PLUGIN_LOAD_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[plugin-host] LoadPlugin mode=main_thread instance={plugin_instance_id} thread_id={} name={name}",
                    platform::current_thread_id()
                );
                let started = Instant::now();
                let processor = DirectAudio::vst3_processor::Vst3RuntimeProcessor::new(
                    &plugin_path,
                    &class_id,
                    sample_rate,
                );
                let elapsed = started.elapsed();
                let error = if processor.is_some() {
                    None
                } else {
                    Some(format!(
                        "Plugin failed to load. It may require a newer CPU instruction set or a missing runtime dependency. path={plugin_path}"
                    ))
                };
                finalize_plugin_load(
                    PluginLoadResult {
                        request_id,
                        plugin_instance_id,
                        plugin_path,
                        class_id,
                        name,
                        processor,
                        error,
                        elapsed,
                    },
                    sample_rate,
                    max_block_size,
                    loaded,
                    preview,
                    out,
                );
                return;
            }
            schedule_plugin_load(
                plugin_instance_id,
                plugin_path,
                class_id,
                name,
                sample_rate,
                max_block_size,
                pending_plugin_loads,
                load_result_tx,
            );
        }
        HostCommand::LoadBuiltinPlugin {
            plugin_instance_id,
            plugin_id,
            sample_rate,
            max_block_size,
            state_json,
        } => {
            let Some(stem) = SpherePluginHost::resolve_builtin_stem(&plugin_id) else {
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::PluginLoadFailed {
                        plugin_instance_id,
                        error: format!("unknown built-in plugin: {plugin_id}"),
                    },
                );
                return;
            };
            let name = SpherePluginHost::builtin_display_name(stem)
                .unwrap_or(stem)
                .to_string();
            if loaded.contains_key(&plugin_instance_id) {
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::PluginAlreadyLoaded {
                        plugin_instance_id,
                        name,
                    },
                );
                return;
            }
            // The catalog can list a built-in the host has no DSP for; fail the
            // load rather than publishing an instance that produces nothing.
            let Some(processor) =
                BuiltinHostProcessor::new(stem, sample_rate, state_json.as_deref())
            else {
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::PluginLoadFailed {
                        plugin_instance_id,
                        error: format!("built-in DSP is not host-enabled yet: {stem}"),
                    },
                );
                return;
            };
            if let Ok(mut processors) = builtin_processors.lock() {
                Arc::make_mut(&mut processors)
                    .insert(plugin_instance_id.clone(), Arc::new(processor));
            }
            loaded.insert(
                plugin_instance_id.clone(),
                LoadedPlugin {
                    plugin_path: String::new(),
                    class_id: stem.to_string(),
                    name: name.clone(),
                    sample_rate,
                    max_block_size,
                    processing_ready: true,
                },
            );
            eprintln!(
                "[plugin-host-builtin] loaded instance={plugin_instance_id} plugin={stem} sr={sample_rate} block={max_block_size}"
            );
            let _ = ipc::write_frame(
                out,
                &HostEvent::PluginLoaded {
                    plugin_instance_id,
                    name,
                },
            );
        }
        HostCommand::LoadBuiltinNamCapture {
            plugin_instance_id,
            name,
            json,
            stereo,
            full_rig,
        } => {
            // IPC thread: parse/build here (allocation-heavy, never on the
            // producer thread), then hand off through the loader's wait-free
            // cells; the producer adopts at its next block boundary.
            let processor = builtin_processors
                .lock()
                .ok()
                .and_then(|processors| processors.get(&plugin_instance_id).cloned());
            let result = match processor {
                Some(p) => match p.nam_loader.as_ref() {
                    Some(loader) => loader
                        .load_json(&json, name.clone(), p.sample_rate as f64, stereo, full_rig)
                        .map_err(|e| e.to_string()),
                    None => Err(format!(
                        "built-in DSP for {plugin_instance_id} has no capture stage"
                    )),
                },
                None => Err(format!(
                    "no built-in DSP instance loaded for {plugin_instance_id}"
                )),
            };
            let event = match result {
                Ok(info) => {
                    eprintln!(
                        "[plugin-host-nam] loaded instance={plugin_instance_id} name={} receptive_field={} stereo={stereo} full_rig={full_rig}",
                        info.name, info.receptive_field
                    );
                    HostEvent::BuiltinNamCaptureResult {
                        plugin_instance_id,
                        ok: true,
                        name: info.name,
                        error: None,
                        receptive_field: info.receptive_field as u64,
                        full_rig: info.full_rig,
                    }
                }
                Err(error) => {
                    eprintln!(
                        "[plugin-host-nam] load FAILED instance={plugin_instance_id} name={name} error={error}"
                    );
                    HostEvent::BuiltinNamCaptureResult {
                        plugin_instance_id,
                        ok: false,
                        name,
                        error: Some(error),
                        receptive_field: 0,
                        full_rig,
                    }
                }
            };
            let _ = ipc::write_frame(out, &event);
        }
        HostCommand::LoadBuiltinIr {
            plugin_instance_id,
            name,
            wav_b64,
        } => {
            use base64::Engine as _;
            // IPC thread: decode, resample and FFT here (allocation-heavy,
            // never on the producer thread), then hand off through the
            // loader's wait-free cells.
            let processor = builtin_processors
                .lock()
                .ok()
                .and_then(|processors| processors.get(&plugin_instance_id).cloned());
            let result = base64::engine::general_purpose::STANDARD
                .decode(wav_b64.as_bytes())
                .map_err(|e| format!("IR payload is not valid base64: {e}"))
                .and_then(|bytes| match processor {
                    Some(p) => match p.ir_loader.as_ref() {
                        Some(loader) => loader
                            .load_wav(&bytes, name.clone(), p.sample_rate as f64)
                            .map_err(|e| e.to_string()),
                        None => Err(format!(
                            "built-in DSP for {plugin_instance_id} has no cabinet stage"
                        )),
                    },
                    None => Err(format!(
                        "no built-in DSP instance loaded for {plugin_instance_id}"
                    )),
                });
            let event = match result {
                Ok(info) => {
                    eprintln!(
                        "[plugin-host-ir] loaded instance={plugin_instance_id} name={} frames={} stereo={} truncated={}",
                        info.name, info.frames, info.stereo, info.truncated
                    );
                    HostEvent::BuiltinIrResult {
                        plugin_instance_id,
                        ok: true,
                        name: info.name,
                        error: None,
                        frames: info.frames as u64,
                        latency_samples: info.latency_samples as u64,
                        stereo: info.stereo,
                        truncated: info.truncated,
                    }
                }
                Err(error) => {
                    eprintln!(
                        "[plugin-host-ir] load FAILED instance={plugin_instance_id} name={name} error={error}"
                    );
                    HostEvent::BuiltinIrResult {
                        plugin_instance_id,
                        ok: false,
                        name,
                        error: Some(error),
                        frames: 0,
                        latency_samples: 0,
                        stereo: false,
                        truncated: false,
                    }
                }
            };
            let _ = ipc::write_frame(out, &event);
        }
        HostCommand::OpenEditorWithParentHwnd {
            track_id,
            track_index,
            track_name,
            plugin_slot_id,
            plugin_instance_id,
            class_id,
            plugin_uid,
            plugin_display_name,
            owner_hwnd,
            parent_hwnd,
            width,
            height,
            dpi,
            ..
        } => {
            let display_title = plugin_display_name
                .clone()
                .unwrap_or_else(|| plugin_instance_id.clone());
            let requested_owner = owner_hwnd.unwrap_or(parent_hwnd);
            let editor_mode = editor_mode_name();
            let resolved_owner =
                resolve_editor_owner_hwnd(requested_owner, parent_hwnd, &editor_mode);
            eprintln!(
                "[editor-open] 05 ipc_received insert_id={} plugin={} thread_id={} requested_owner_hwnd=0x{:x} parent_hwnd=0x{parent_hwnd:x} resolved_owner_hwnd=0x{resolved_owner:x}",
                plugin_instance_id,
                display_title,
                platform::current_thread_id(),
                requested_owner,
            );
            eprintln!(
                "[OpenEditor/IPC] track_id={} track_index={} track_name={} slot_id={} instance_id={} class_id={} plugin_uid={} plugin={} requested_owner_hwnd=0x{:x} parent_hwnd=0x{parent_hwnd:x} resolved_owner_hwnd=0x{resolved_owner:x}",
                track_id.as_deref().unwrap_or("<unknown>"),
                track_index
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "<unknown>".to_string()),
                track_name.as_deref().unwrap_or("<unknown>"),
                plugin_slot_id.as_deref().unwrap_or("<unknown>"),
                plugin_instance_id,
                class_id,
                plugin_uid.as_deref().unwrap_or("<unknown>"),
                display_title,
                requested_owner,
            );
            platform::log_window_identity_chain("studio_main_hwnd", main_hwnd());
            platform::log_window_identity_chain("open_requested_owner", requested_owner);
            platform::log_window_identity_chain("open_parent_hwnd", parent_hwnd);
            platform::log_window_identity_chain("open_resolved_owner", resolved_owner);
            schedule_unified_editor_attach(
                &plugin_instance_id,
                resolved_owner,
                &display_title,
                width,
                height,
                dpi,
                registry,
                loaded,
                delayed_redraws,
                pending_editor_attaches,
                attach_result_tx,
                preview,
                out,
            );
        }
        HostCommand::PrepareEditorView {
            plugin_instance_id, ..
        } => {
            eprintln!("[PluginHost] editor open requested id={plugin_instance_id}");
            if !preview.lock().has_instance(&plugin_instance_id) {
                emit_attach_failed(
                    out,
                    &plugin_instance_id,
                    "plugin not loaded — call LoadPlugin first",
                );
                return;
            }
            eprintln!("[plugin-host] OpenEditor uses existing instance={plugin_instance_id}");
            SpherePluginHost::plugin_host_preview::PluginHostPreviewEngine::verify_unified_runtime(
                &plugin_instance_id,
                &plugin_instance_id,
                &plugin_instance_id,
                &plugin_instance_id,
                &plugin_instance_id,
                &plugin_instance_id,
                &plugin_instance_id,
                &plugin_instance_id,
            );
            let (preferred_width, preferred_height) = preview
                .lock()
                .editor_content_size_for_instance(&plugin_instance_id);
            let _ = ipc::write_frame(
                out,
                &HostEvent::EditorPreferredSize {
                    plugin_instance_id,
                    width: preferred_width,
                    height: preferred_height,
                },
            );
        }
        HostCommand::ConfirmEditorContentReady {
            plugin_instance_id,
            parent_hwnd,
            width,
            height,
            dpi,
            ..
        } => {
            schedule_unified_editor_attach(
                &plugin_instance_id,
                parent_hwnd,
                &plugin_instance_id,
                width,
                height,
                dpi,
                registry,
                loaded,
                delayed_redraws,
                pending_editor_attaches,
                attach_result_tx,
                preview,
                out,
            );
        }
        HostCommand::ResizeEditor {
            plugin_instance_id,
            width,
            height,
            dpi,
        } => {
            // Coalesce into the pending map; applied by the UI loop with a
            // bounded try-lock (spec item 9: resizing must never block the
            // pump thread on the DSP/preview mutex). Interactive drags stream
            // many ResizeEditor commands — only the latest size matters.
            pending_resizes.insert(plugin_instance_id.clone(), (width, height, dpi));
            hlog!(
                "[PluginHostEditor] resize queued plugin_instance_id={plugin_instance_id} \
                 width={width} height={height} dpi={dpi}"
            );
        }
        HostCommand::CloseEditor { plugin_instance_id } => {
            eprintln!("[PluginEditor] close requested plugin_id={plugin_instance_id}");
            registry.remove(&plugin_instance_id);
            pending_editor_attaches.remove(&plugin_instance_id);
            pending_resizes.remove(&plugin_instance_id);
            preview
                .lock()
                .editor_detach_for_instance(&plugin_instance_id);
            delayed_redraws.retain(|entry| entry.instance_id != plugin_instance_id);
            let still_active = preview.lock().has_instance(&plugin_instance_id);
            eprintln!("[PluginEditor] detached editor only plugin_id={plugin_instance_id}");
            eprintln!(
                "[PluginRuntime] instance remains alive plugin_id={plugin_instance_id} active={still_active}"
            );
            eprintln!("[AudioGraph] node remains active plugin_id={plugin_instance_id}");
            eprintln!("[VSTi] midi route alive plugin_id={plugin_instance_id}");
            eprintln!("[VSTi] process active after editor close plugin_id={plugin_instance_id}");
            let _ = ipc::write_frame(out, &HostEvent::EditorClosed { plugin_instance_id });
        }
        HostCommand::PreviewNoteOn {
            plugin_instance_id,
            channel,
            pitch,
            velocity,
        } => {
            // Stage 1: the legacy separate-CPAL audition stream is OFF by
            // default. The MIDI is still queued to the VSTi so the eventual
            // shared-memory mix path (Stage 3) can pull its output, but we never
            // open a second device stream — log the honest pending state instead.
            if debug_audio_out_enabled() {
                if !*preview_output_started {
                    *preview_output_started = try_start_preview_output(preview);
                }
            } else if !*preview_output_started {
                *preview_output_started = true; // log once
                eprintln!(
                    "[plugin-host-midi] dsp_output=pending reason=main_mix_integration_pending \
                     (separate CPAL preview disabled; set FUTUREBOARD_PLUGIN_HOST_CPAL_PREVIEW=1 to audition)"
                );
            }
            preview
                .lock()
                .preview_note_on(&plugin_instance_id, channel, pitch, velocity);
        }
        HostCommand::PreviewNoteOff {
            plugin_instance_id,
            channel,
            pitch,
        } => {
            preview
                .lock()
                .preview_note_off(&plugin_instance_id, channel, pitch);
        }
        HostCommand::PreviewControlChange {
            plugin_instance_id,
            channel,
            controller,
            value,
        } => {
            preview
                .lock()
                .preview_control_change(&plugin_instance_id, channel, controller, value);
        }
        HostCommand::PreviewAllNotesOff { plugin_instance_id } => {
            preview.lock().preview_all_notes_off(&plugin_instance_id);
        }
        HostCommand::MidiPanic { plugin_instance_id } => {
            preview.lock().midi_panic(&plugin_instance_id);
        }
        HostCommand::UnloadPlugin { plugin_instance_id } => {
            eprintln!(
                "[PluginHost] unload requested id={plugin_instance_id} reason=user_removed_insert"
            );
            registry.remove(&plugin_instance_id);
            preview.lock().unload_instance(&plugin_instance_id);
            loaded.remove(&plugin_instance_id);
            if let Ok(mut processors) = builtin_processors.lock() {
                Arc::make_mut(&mut processors).remove(&plugin_instance_id);
            }
            if let Ok(mut slots) = region_slots.lock() {
                Arc::make_mut(&mut slots).remove(&plugin_instance_id);
            }
            let instance_count = preview.lock().loaded_instance_ids().len();
            eprintln!(
                "[PluginHost] host shutdown deferred instance_count={instance_count} editor_count={}",
                registry.len()
            );
            hlog!(
                "[PluginHostEditor] unload plugin_instance_id={plugin_instance_id} released=true"
            );
            let _ = ipc::write_frame(out, &HostEvent::PluginUnloaded { plugin_instance_id });
        }
        HostCommand::ConfigureAudioBridge {
            sample_rate,
            max_block_size,
        } => {
            // Stage 1: the main engine owns sample rate / block size; follow it.
            let (sr, block) = preview.lock().configure(sample_rate, max_block_size);
            eprintln!(
                "[plugin-host-bridge] ConfigureAudioBridge engine_sr={sample_rate} engine_block={max_block_size} \
                 host_sr={sr} host_block={block} follows_engine=true"
            );
            let _ = ipc::write_frame(
                out,
                &HostEvent::AudioBridgeConfigured {
                    sample_rate: sr,
                    max_block_size: block,
                    follows_engine: true,
                },
            );
        }
        HostCommand::ProcessBlockShared { block_id, frames } => {
            // Stage 1 skeleton: the lock-free shared-memory audio/MIDI transport
            // is Stage 2/3. Acknowledge honestly — plugin DSP output is NOT yet
            // mixed into the main engine.
            let dsp_ready = preview.lock().dsp_ready();
            let dsp_output = if dsp_ready { "ready" } else { "pending" };
            eprintln!(
                "[plugin-host-bridge] ProcessBlockShared block_id={block_id} frames={frames} dsp_output={dsp_output}"
            );
            let _ = ipc::write_frame(
                out,
                &HostEvent::AudioBridgeStatus {
                    block_id,
                    dsp_output: dsp_output.to_string(),
                    latency_samples: 0,
                },
            );
        }
        HostCommand::AttachSharedAudio {
            name,
            bytes,
            plugin_instance_id,
        } => match SharedAudioRegion::open_named(&name) {
            Ok(region) => {
                let sr = region.bridge().sample_rate.load(Ordering::Relaxed);
                let block = region.bridge().max_block_size.load(Ordering::Relaxed);
                eprintln!(
                        "[plugin-host-bridge] AttachSharedAudio instance={plugin_instance_id} name={name} bytes={bytes} attached=true header_sr={sr} header_block={block}"
                    );
                region.bridge().set_dsp_output_ready(true);
                if let Ok(mut slots) = region_slots.lock() {
                    let key = if plugin_instance_id.is_empty() {
                        name.clone()
                    } else {
                        plugin_instance_id.clone()
                    };
                    Arc::make_mut(&mut slots).insert(key, Arc::new(region));
                }
                preview.lock().set_dsp_ready(true);
                log_host_audio_mode();
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::SharedAudioAttached {
                        attached: true,
                        name,
                        bytes,
                    },
                );
            }
            Err(error) => {
                eprintln!(
                        "[plugin-host-bridge] AttachSharedAudio name={name} attached=false error={error}"
                    );
                let _ = ipc::write_frame(
                    out,
                    &HostEvent::SharedAudioAttached {
                        attached: false,
                        name,
                        bytes,
                    },
                );
            }
        },
        HostCommand::PrepareProcessing {
            plugin_instance_id,
            sample_rate,
            max_block_size,
            input_channels,
            output_channels,
        } => {
            eprintln!(
                "[plugin-bridge] PrepareProcessing instance={plugin_instance_id} sr={sample_rate} block={max_block_size}"
            );
            let mut preview_guard = preview.lock();
            if !preview_guard.has_instance(&plugin_instance_id) {
                drop(preview_guard);
                emit_attach_failed(
                    out,
                    &plugin_instance_id,
                    "PrepareProcessing: instance not loaded",
                );
                return;
            }
            eprintln!(
                "[plugin-host] PrepareProcessing uses existing instance={plugin_instance_id}"
            );
            let (sr, block) = preview_guard.configure(sample_rate, max_block_size);
            preview_guard.set_dsp_ready(true);
            let actual_output_channels = preview_guard
                .main_audio_output_channel_count_for_instance(&plugin_instance_id)
                .unwrap_or(output_channels.max(1));
            let output_bus_channels = preview_guard
                .output_bus_channel_counts_for_instance(&plugin_instance_id)
                .unwrap_or_default();
            drop(preview_guard);
            if let Some(plugin) = loaded.get_mut(&plugin_instance_id) {
                plugin.processing_ready = true;
                plugin.sample_rate = sr;
                plugin.max_block_size = block;
            }
            eprintln!(
                "[plugin-host-dsp] prepared instance={plugin_instance_id} sr={sr} block={block} requestedOutputs={output_channels} outputs={actual_output_channels} output_bus_channels={output_bus_channels:?} same_instance=true"
            );
            let _ = ipc::write_frame(
                out,
                &HostEvent::ProcessingPrepared {
                    plugin_instance_id,
                    sample_rate: sr,
                    max_block_size: block,
                    output_channels: actual_output_channels,
                    output_bus_channels,
                },
            );
            let _ = input_channels;
        }
        HostCommand::GetPluginState { plugin_instance_id } => {
            use base64::Engine as _;
            let state = preview.lock().get_instance_state(&plugin_instance_id);
            let ok = state.is_some();
            let state = state.unwrap_or_default();
            eprintln!(
                "[plugin-host-state] get_state instance={plugin_instance_id} ok={ok} component_bytes={} controller_bytes={}",
                state.component.len(),
                state.controller.len()
            );
            let _ = ipc::write_frame(
                out,
                &HostEvent::PluginState {
                    plugin_instance_id,
                    ok,
                    component_b64: base64::engine::general_purpose::STANDARD
                        .encode(&state.component),
                    controller_b64: base64::engine::general_purpose::STANDARD
                        .encode(&state.controller),
                },
            );
        }
        HostCommand::GetPluginParameters { plugin_instance_id } => {
            let params = preview
                .lock()
                .list_parameters_for_instance(&plugin_instance_id);
            let ok = params.is_some();
            let parameters: Vec<ipc::HostPluginParameter> = params
                .unwrap_or_default()
                .into_iter()
                .map(|p| ipc::HostPluginParameter {
                    id: p.id,
                    title: p.title,
                    short_title: p.short_title,
                    unit: p.unit,
                    automatable: p.automatable,
                    hidden: p.hidden,
                    read_only: p.read_only,
                })
                .collect();
            eprintln!(
                "[plugin-host-params] get_parameters instance={plugin_instance_id} ok={ok} count={}",
                parameters.len()
            );
            let _ = ipc::write_frame(
                out,
                &HostEvent::PluginParameters {
                    plugin_instance_id,
                    ok,
                    parameters,
                },
            );
        }
        HostCommand::SetPluginState {
            plugin_instance_id,
            component_b64,
            controller_b64,
        } => {
            use base64::Engine as _;
            let decode = |label: &str, b64: &str| -> Vec<u8> {
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .unwrap_or_else(|error| {
                        eprintln!(
                            "[plugin-host-state] set_state instance={plugin_instance_id} {label} base64 invalid: {error}"
                        );
                        Vec::new()
                    })
            };
            let state = DirectAudio::vst3_processor::Vst3PluginState {
                component: decode("component", &component_b64),
                controller: decode("controller", &controller_b64),
            };
            let ok = if state.is_empty() {
                eprintln!(
                    "[plugin-host-state] set_state instance={plugin_instance_id} empty state — skipped"
                );
                false
            } else {
                preview
                    .lock()
                    .set_instance_state(&plugin_instance_id, &state)
            };
            let _ = ipc::write_frame(
                out,
                &HostEvent::PluginStateSet {
                    plugin_instance_id,
                    ok,
                },
            );
        }
        HostCommand::Shutdown => {
            // Handled in run_ipc_loop before dispatch; unreachable here.
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn schedule_plugin_load(
    plugin_instance_id: String,
    plugin_path: String,
    class_id: String,
    name: String,
    sample_rate: u32,
    max_block_size: u32,
    pending_plugin_loads: &mut HashMap<String, PendingPluginLoad>,
    load_result_tx: &crossbeam_channel::Sender<PluginLoadResult>,
) {
    let request_id = NEXT_PLUGIN_LOAD_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let started_at = Instant::now();
    eprintln!(
        "[PLUGIN LOAD REQUEST]\nrequest_id={request_id}\nplugin_class_id={class_id}\nplugin_name={name}\nvendor=(unknown)\nformat=VST3\npath={plugin_path}\nproject_track_id=(unknown)\nrequested_by=user\nthread_id={}\nui_thread_blocked = false",
        platform::current_thread_id()
    );
    pending_plugin_loads.insert(
        plugin_instance_id.clone(),
        PendingPluginLoad {
            request_id,
            plugin_instance_id: plugin_instance_id.clone(),
            plugin_path: plugin_path.clone(),
            class_id: class_id.clone(),
            name: name.clone(),
            sample_rate,
            max_block_size,
            started_at,
            stage: "create_instance",
        },
    );
    let tx = load_result_tx.clone();
    std::thread::Builder::new()
        .name(format!("plugin-load-{request_id}"))
        .spawn(move || {
            let thread_id = platform::current_thread_id();
            let stage_started = Instant::now();
            eprintln!(
                "[PLUGIN LOAD STAGE]\nrequest_id={request_id}\nplugin_instance_id_optional={plugin_instance_id}\nplugin_name={name}\nstage=load_module\nstarted_at_ms=0\nended_at_ms=0\nduration_ms=0\nresult=begin\nthread_id={thread_id}\ntimeout_ms={}\nlock_held_names=none\nipc_responsive=true\nui_responsive=true",
                MODULE_LOAD_TIMEOUT.as_millis()
            );
            eprintln!(
                "[PLUGIN LOAD STAGE]\nrequest_id={request_id}\nplugin_instance_id_optional={plugin_instance_id}\nplugin_name={name}\nstage=create_instance\nstarted_at_ms=0\nended_at_ms=0\nduration_ms=0\nresult=begin\nthread_id={thread_id}\ntimeout_ms={}\nlock_held_names=none\nipc_responsive=true\nui_responsive=true",
                CREATE_INSTANCE_TIMEOUT.as_millis()
            );
            let processor =
                DirectAudio::vst3_processor::Vst3RuntimeProcessor::new(&plugin_path, &class_id, sample_rate);
            let elapsed = stage_started.elapsed();
            let error = if processor.is_some() {
                None
            } else {
                Some(format!(
                    "Plugin failed to load. It may require a newer CPU instruction set or a missing runtime dependency. path={plugin_path}"
                ))
            };
            eprintln!(
                "[PLUGIN LOAD STAGE]\nrequest_id={request_id}\nplugin_instance_id_optional={plugin_instance_id}\nplugin_name={name}\nstage=initialize_component_controller\nstarted_at_ms=0\nended_at_ms={}\nduration_ms={}\nresult={}\nthread_id={thread_id}\ntimeout_ms={}\nlock_held_names=none\nipc_responsive=true\nui_responsive=true",
                elapsed.as_millis(),
                elapsed.as_millis(),
                if processor.is_some() { "ok" } else { "failed" },
                INITIALIZE_TIMEOUT.as_millis()
            );
            let _ = tx.send(PluginLoadResult {
                request_id,
                plugin_instance_id,
                plugin_path,
                class_id,
                name,
                processor,
                error,
                elapsed,
            });
        })
        .expect("spawn plugin load worker");
}

fn drain_plugin_load_results(
    loaded: &mut LoadedRegistry,
    pending_plugin_loads: &mut HashMap<String, PendingPluginLoad>,
    load_result_rx: &crossbeam_channel::Receiver<PluginLoadResult>,
    preview: &SharedPluginHostPreview,
    out: &mut io::Stdout,
) {
    while let Ok(result) = load_result_rx.try_recv() {
        let Some(pending) = pending_plugin_loads.remove(&result.plugin_instance_id) else {
            eprintln!(
                "[PLUGIN LOAD FAILURE]\nrequest_id={}\nplugin_name={}\nfailure_stage=late_result_after_timeout\nreason=load completed after timeout\ntimed_out = true\nplugin_instance_created={}\ncomponent_created={}\ncontroller_created={}\nroutes_created=false\nrollback_completed = true\napp_alive = true",
                result.request_id,
                result.name,
                result.processor.is_some(),
                result.processor.is_some(),
                result.processor.is_some()
            );
            drop(result.processor);
            continue;
        };
        if pending.request_id != result.request_id {
            drop(result.processor);
            continue;
        }
        finalize_plugin_load(
            result,
            pending.sample_rate,
            pending.max_block_size,
            loaded,
            preview,
            out,
        );
    }
}

/// Register a completed plugin load (insert into the engine + `loaded`) and emit
/// `PluginLoaded`/`PluginLoadFailed`. Shared by the inline (main-thread) load
/// path and the legacy worker-thread drain so both report identically.
fn finalize_plugin_load(
    result: PluginLoadResult,
    sample_rate: u32,
    max_block_size: u32,
    loaded: &mut LoadedRegistry,
    preview: &SharedPluginHostPreview,
    out: &mut io::Stdout,
) {
    let Some(processor) = result.processor else {
        let error = result
            .error
            .unwrap_or_else(|| "plugin load failed".to_string());
        eprintln!(
            "[PLUGIN LOAD FAILURE]\nrequest_id={}\nplugin_name={}\nfailure_stage=create_instance\nreason={error}\ntimed_out = false\nplugin_instance_created=false\ncomponent_created=false\ncontroller_created=false\nroutes_created=false\nrollback_completed = true\napp_alive = true",
            result.request_id, result.name
        );
        let _ = ipc::write_frame(
            out,
            &HostEvent::PluginLoadFailed {
                plugin_instance_id: result.plugin_instance_id,
                error,
            },
        );
        return;
    };
    let inserted = preview
        .lock()
        .insert_loaded_instance(&result.plugin_instance_id, processor);
    if !inserted {
        let error = "plugin instance already exists".to_string();
        let _ = ipc::write_frame(
            out,
            &HostEvent::PluginLoadFailed {
                plugin_instance_id: result.plugin_instance_id,
                error,
            },
        );
        return;
    }
    loaded.insert(
        result.plugin_instance_id.clone(),
        LoadedPlugin {
            plugin_path: result.plugin_path,
            class_id: result.class_id,
            name: result.name.clone(),
            sample_rate,
            max_block_size,
            processing_ready: false,
        },
    );
    eprintln!(
        "[PLUGIN LOAD READY]\nrequest_id={}\nplugin_instance_id={}\nplugin_name={}\nload_duration_ms={}\naudio_ready = true\neditor_created = false\nroute_ready = true\nmixer_channels_created=0\nselected_bus_mode=capability_detected\nui_responsive = true",
        result.request_id,
        result.plugin_instance_id,
        result.name,
        result.elapsed.as_millis()
    );
    let _ = ipc::write_frame(
        out,
        &HostEvent::PluginLoaded {
            plugin_instance_id: result.plugin_instance_id,
            name: result.name,
        },
    );
}

fn expire_plugin_load_requests(
    pending_plugin_loads: &mut HashMap<String, PendingPluginLoad>,
    _load_result_rx: &crossbeam_channel::Receiver<PluginLoadResult>,
    out: &mut io::Stdout,
) {
    let now = Instant::now();
    let timed_out: Vec<PendingPluginLoad> = pending_plugin_loads
        .values()
        .filter(|pending| {
            let timeout = match pending.stage {
                "load_module" => MODULE_LOAD_TIMEOUT,
                "create_instance" => CREATE_INSTANCE_TIMEOUT,
                "initializing" => INITIALIZE_TIMEOUT,
                _ => CREATE_INSTANCE_TIMEOUT,
            };
            now.duration_since(pending.started_at) >= timeout
        })
        .cloned()
        .collect();
    for pending in timed_out {
        pending_plugin_loads.remove(&pending.plugin_instance_id);
        let elapsed_ms = now.duration_since(pending.started_at).as_millis();
        let timeout_ms = match pending.stage {
            "load_module" => MODULE_LOAD_TIMEOUT.as_millis(),
            "create_instance" => CREATE_INSTANCE_TIMEOUT.as_millis(),
            "initializing" => INITIALIZE_TIMEOUT.as_millis(),
            _ => CREATE_INSTANCE_TIMEOUT.as_millis(),
        };
        eprintln!(
            "[PLUGIN LOAD HANG WATCHDOG]\nrequest_id={}\nplugin_name={}\ncurrent_stage={}\nelapsed_ms={elapsed_ms}\ntimeout_ms={timeout_ms}\nthread_id={}\nlast_progress_ms=0\nui_thread_responsive=true\nipc_thread_responsive=true\naudio_thread_responsive=true\nheld_locks=none\nlast_vst3_call={}",
            pending.request_id,
            pending.name,
            pending.stage,
            platform::current_thread_id(),
            pending.stage
        );
        eprintln!(
            "[PLUGIN LOAD STAGE]\nrequest_id={}\nplugin_instance_id_optional={}\nplugin_name={}\nstage={}\nstarted_at_ms=0\nended_at_ms={elapsed_ms}\nduration_ms={elapsed_ms}\nresult=timed_out\nthread_id={}\ntimeout_ms={timeout_ms}\nlock_held_names=none\nipc_responsive=true\nui_responsive=true\nplugin_class_id={}\npath={}",
            pending.request_id,
            pending.plugin_instance_id,
            pending.name,
            pending.stage,
            platform::current_thread_id(),
            pending.class_id,
            pending.plugin_path
        );
        eprintln!(
            "[PLUGIN LOAD FAILURE]\nrequest_id={}\nplugin_name={}\nfailure_stage={}\nreason=plugin load timed out\ntimed_out = true\nplugin_instance_created=false\ncomponent_created=false\ncontroller_created=false\nroutes_created=false\nrollback_completed = true\napp_alive = true",
            pending.request_id, pending.name, pending.stage
        );
        let _ = ipc::write_frame(
            out,
            &HostEvent::PluginLoadFailed {
                plugin_instance_id: pending.plugin_instance_id,
                error: format!(
                    "Plugin load timed out during {} after {elapsed_ms}ms",
                    pending.stage
                ),
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
/// Whether the whole plugin UI lifecycle — controller creation (LoadPlugin) AND
/// editor attach (createView / `attached()` / onSize) — runs INLINE on the host's
/// single, persistent, message-pumped main STA thread, instead of on ephemeral
/// per-request worker threads. Default = MAIN thread.
///
/// Live diagnosis (AD2, a NATIVE VST3 editor): `attached()` hard-blocks at the
/// very start with ZERO child windows created, and works in other DAWs. Root
/// cause = thread affinity. The controller was created on a `plugin-load-N`
/// worker that immediately DIED; native editors routinely `SendMessage()` from
/// `attached()` to a hidden window the plug-in created at controller-construction
/// time. With the controller's thread dead, that synchronous send blocks forever
/// (a dead thread never pumps). Every real host (and the VST3 SDK `editorhost`
/// sample) creates the controller and attaches the editor on the SAME living UI
/// thread, so the send is a direct same-thread call. Keeping the whole lifecycle
/// on the host main thread reproduces that. The audio `process()` stays on the
/// separate producer thread regardless. Set `FUTUREBOARD_PLUGIN_HOST_WORKER_THREADS=1`
/// to restore the legacy split-worker behavior for A/B testing.
fn plugin_lifecycle_on_main_thread() -> bool {
    std::env::var_os("FUTUREBOARD_PLUGIN_HOST_WORKER_THREADS").is_none()
}

fn editor_mode_name() -> String {
    std::env::var("FUTUREBOARD_PLUGIN_EDITOR_MODE").unwrap_or_else(|_| "legacy".to_string())
}

fn editor_mode_prefers_async_attach(mode: &str) -> bool {
    let mode = mode.trim().to_ascii_lowercase();
    !matches!(
        mode.as_str(),
        "" | "default" | "legacy" | "child" | "ws_child" | "embedded_child"
    )
}

fn resolve_editor_owner_hwnd(requested_owner: u64, parent_hwnd: u64, editor_mode: &str) -> u64 {
    if !editor_mode_prefers_async_attach(editor_mode) {
        return parent_hwnd;
    }

    // Detached/owned top-level modes: the owner is a read-only DPI/position/owner
    // reference for the host's OWN window — never a `SetParent` target. Studio sends
    // its live main-window HWND with EVERY open request (`requested_owner`/
    // `parent_hwnd`), so that is the current, correct handle. Prefer it as long as
    // it is a real window owned by the Studio (parent) process.
    //
    // Do NOT require the host's `main_hwnd()` copy: it is captured once at `Hello`
    // (spawn) time via the spawn config, frequently BEFORE the GPUI window exists,
    // so it is an unreliable 0. Requiring it regressed editor opening — every open
    // resolved owner=0 → `schedule_unified_editor_attach` rejected `parent_hwnd` as
    // "not a valid window" before attach ever ran.
    let parent_pid = parse_parent_pid();
    for candidate in [requested_owner, parent_hwnd, main_hwnd()] {
        if candidate == 0 || !platform::is_window(candidate) {
            continue;
        }
        // When the parent pid is known, only accept a window the parent owns — this
        // keeps the "don't parent/own under an arbitrary foreign HWND" guarantee
        // without depending on the stale stored handle.
        if let (Some(ppid), cpid) = (parent_pid, platform::window_process_id(candidate)) {
            if cpid != Some(ppid) {
                eprintln!(
                    "[PluginEditorOwner] skipping owner hwnd=0x{candidate:x} pid={cpid:?} expected_parent_pid={ppid}"
                );
                continue;
            }
        }
        if candidate != requested_owner {
            eprintln!(
                "[PluginEditorOwner] using owner hwnd=0x{candidate:x} (requested=0x{requested_owner:x})"
            );
        }
        return candidate;
    }

    eprintln!(
        "[PluginEditorOwner] no Studio-owned owner HWND available (requested=0x{requested_owner:x} parent=0x{parent_hwnd:x} stored=0x{:x}); proceeding without owner",
        main_hwnd()
    );
    0
}

#[allow(clippy::too_many_arguments)]
fn schedule_unified_editor_attach(
    plugin_instance_id: &str,
    parent_hwnd: u64,
    display_title: &str,
    width: u32,
    height: u32,
    dpi: u32,
    registry: &mut Registry,
    loaded: &LoadedRegistry,
    delayed_redraws: &mut Vec<DelayedGpuRedraw>,
    pending_editor_attaches: &mut HashMap<String, PendingEditorAttach>,
    attach_result_tx: &crossbeam_channel::Sender<EditorAttachResult>,
    preview: &SharedPluginHostPreview,
    out: &mut io::Stdout,
) {
    // The actual window ownership is decided by the C++ embed layer from
    // FUTUREBOARD_PLUGIN_EDITOR_MODE (default "detached" = host-owned top-level
    // owned popup). In detached mode `parent_hwnd` is the Studio main HWND used
    // as the Win32 owner, not a child parent and never SetParent.
    let editor_mode = editor_mode_name();
    eprintln!("[plugin-host] editor_mode={editor_mode} owner_hwnd=0x{parent_hwnd:x}");
    eprintln!("[VST3Editor] owner_hwnd=0x{parent_hwnd:x}");
    eprintln!("[PluginHost] open_editor requested instance_id={plugin_instance_id}");
    eprintln!(
        "[editor-open] host request plugin={} insert={} thread_id={} hwnd=0x{parent_hwnd:x} size={}x{}",
        display_title,
        plugin_instance_id,
        platform::current_thread_id(),
        width.max(1),
        height.max(1)
    );
    eprintln!("[plugin-host] OpenEditor uses existing instance={plugin_instance_id}");
    eprintln!(
        "[EDITOR OPEN START]\nplugin_instance_id={plugin_instance_id}\nparent_hwnd=0x{parent_hwnd:x}\nrequested_size={}x{}\ndpi={dpi}",
        width.max(1),
        height.max(1)
    );
    eprintln!(
        "[PLUGIN EDITOR OPEN REQUEST]\nplugin_instance_id={plugin_instance_id}\nplugin_name=(unknown)\nvendor=(unknown)\nformat=VST3\nthread={}\nhost_process_id={}\nexisting_editor={}\ncomponent_valid=(unknown)\ncontroller_valid=(unknown)\naudio_instance_alive=(checking)\nmixer_route_ready=(unknown)\nmultiout_enabled=(unknown)",
        platform::current_thread_id(),
        std::process::id(),
        registry.contains_key(plugin_instance_id)
    );
    if let Some(existing) = registry.get(plugin_instance_id) {
        eprintln!(
            "[PluginHost] editor_state instance_id={} state={}",
            existing.plugin_instance_id, existing.state
        );
        eprintln!("[plugin-host] editor already attached instance={plugin_instance_id}");
        if existing.plugin_instance_id != plugin_instance_id {
            eprintln!(
                "[VST3Editor] refusing to focus mismatched editor requested={plugin_instance_id} existing={}",
                existing.plugin_instance_id
            );
            return;
        }
        if platform::is_window(existing.host_hwnd) {
            eprintln!(
                "[PluginHost] focusing existing editor requested_instance_id={plugin_instance_id} existing_editor_instance_id={}",
                existing.plugin_instance_id
            );
            eprintln!(
                "[NativeEditorShell] title=\"{}\" owner_hwnd=0x{:x}",
                existing.display_title, existing.owner_hwnd
            );
            let focused = platform::focus_editor_window(existing.host_hwnd);
            eprintln!("[NativeEditorShell] focus_existing result={focused}");
            eprintln!(
                "[editor-open] existing valid plugin={} insert={} hwnd=0x{:x} result={}",
                existing.display_title,
                plugin_instance_id,
                existing.host_hwnd,
                if focused { "focused" } else { "focus_failed" }
            );
            return;
        }
        eprintln!(
            "[editor-open] existing invalid plugin={} insert={} hwnd=0x{:x} result=recreate",
            existing.display_title, plugin_instance_id, existing.host_hwnd
        );
        registry.remove(plugin_instance_id);
    }
    if pending_editor_attaches.contains_key(plugin_instance_id) {
        eprintln!("[plugin-host] editor attach already pending instance={plugin_instance_id}");
        return;
    }
    if !loaded.contains_key(plugin_instance_id) {
        eprintln!("[PluginHost] instance lookup result=not_found instance_id={plugin_instance_id}");
        eprintln!(
            "[editor-open] 06 instance_lookup fail insert_id={plugin_instance_id} reason=not_in_loaded_registry"
        );
        eprintln!(
            "[editor-open] FAILED stage=instance_lookup reason=runtime_instance_not_loaded insert_id={plugin_instance_id}"
        );
        emit_attach_failed(
            out,
            plugin_instance_id,
            "plugin runtime instance is not loaded",
        );
        return;
    }
    if !preview.lock().has_instance(plugin_instance_id) {
        eprintln!("[PluginHost] instance lookup result=not_found instance_id={plugin_instance_id}");
        eprintln!(
            "[editor-open] 06 instance_lookup fail insert_id={plugin_instance_id} reason=not_in_preview_engine"
        );
        eprintln!(
            "[editor-open] FAILED stage=instance_lookup reason=plugin_not_loaded insert_id={plugin_instance_id}"
        );
        emit_attach_failed(
            out,
            plugin_instance_id,
            "plugin not loaded — call LoadPlugin first",
        );
        return;
    }
    eprintln!("[PluginHost] instance lookup result=found instance_id={plugin_instance_id}");
    eprintln!(
        "[editor-open] 06 instance_lookup ok insert_id={plugin_instance_id} plugin={display_title}"
    );
    // Host-owned / detached: `parent_hwnd` is only an optional owner/DPI
    // reference for the host's own top-level window — never a SetParent target.
    // On Linux the GTK editor ignores it entirely, so 0 (or a stale XID from a
    // pure-Wayland GPUI surface) must not block OpenEditorWithParentHwnd →
    // embed_editor → open_editor_linux. Legacy main-owned child embedding still
    // requires a real parent window.
    let detached_owner = editor_mode_prefers_async_attach(&editor_mode);
    if !detached_owner && !platform::is_window(parent_hwnd) {
        eprintln!(
            "[editor-open] shell/parent invalid plugin={display_title} insert={plugin_instance_id} hwnd=0x{parent_hwnd:x} result=failed"
        );
        emit_attach_failed(out, plugin_instance_id, "parent_hwnd is not a valid window");
        return;
    }
    if detached_owner && parent_hwnd != 0 && !platform::is_window(parent_hwnd) {
        eprintln!(
            "[editor-open] host-owned owner_ref 0x{parent_hwnd:x} not a live window; continuing with host-owned top-level (Linux GTK / Win32 detached)"
        );
    }
    if width < MIN_EDITOR_ATTACH_SIZE || height < MIN_EDITOR_ATTACH_SIZE {
        eprintln!(
            "[editor-open] shell size invalid plugin={display_title} insert={plugin_instance_id} hwnd=0x{parent_hwnd:x} size={}x{} result=failed",
            width, height
        );
        emit_attach_failed(
            out,
            plugin_instance_id,
            "editor content HWND is not ready or has invalid size",
        );
        return;
    }
    eprintln!(
        "[EDITOR HWND]\nplugin_instance_id={plugin_instance_id}\nparent_hwnd=0x{parent_hwnd:x}\nvalid=true"
    );
    eprintln!(
        "[editor-open] shell ready plugin={display_title} insert={plugin_instance_id} hwnd=0x{parent_hwnd:x} size={}x{} result=ok",
        width, height
    );
    let w = width.max(1) as i32;
    let h = height.max(1) as i32;
    eprintln!(
        "[PluginEditor] open requested while engine_state=Running transport_playing=unknown instance={plugin_instance_id}"
    );
    let processor = preview.lock().clone_processor_for(plugin_instance_id);
    let Some(processor) = processor else {
        eprintln!(
            "[editor-open] 07 controller_lookup fail insert_id={plugin_instance_id} reason=no_runtime_processor"
        );
        eprintln!(
            "[editor-open] FAILED stage=controller_lookup reason=processor_unavailable insert_id={plugin_instance_id}"
        );
        emit_attach_failed(
            out,
            plugin_instance_id,
            "plugin not loaded — call LoadPlugin first",
        );
        return;
    };
    eprintln!(
        "[editor-open] 07 controller_lookup ok insert_id={plugin_instance_id} plugin={display_title}"
    );
    let request_id = NEXT_EDITOR_ATTACH_REQUEST_ID.fetch_add(1, Ordering::Relaxed);

    // The editor view MUST be created + `attached()` on the SAME thread that
    // created the controller. When `plugin_lifecycle_on_main_thread()` is true
    // (the default) LoadPlugin builds the controller inline on this persistent,
    // COM-STA, message-pumped main IPC thread; native VST3 editors routinely
    // `SendMessage()` from `attached()` to a hidden window the plug-in created at
    // controller-construction time, so attaching on any OTHER thread makes that
    // synchronous send target a window whose owning thread isn't servicing it →
    // `attached()` deadlocks and no editor ever appears. The worker-thread path
    // below is therefore only valid in the legacy split mode where the controller
    // ALSO lives on a worker (`FUTUREBOARD_PLUGIN_HOST_WORKER_THREADS=1`).
    //
    // Regression guard (2026-06-28): gating this on `editor_mode_prefers_async_attach`
    // routed the DEFAULT `detached` mode to the worker path, which hung in
    // `attached()` for every plug-in (load still worked, so the symptom was
    // "plugins load + audio works but no editor opens"). Do NOT re-add an
    // editor-mode condition here — the attach thread must track the LOAD thread,
    // not the window-ownership mode.
    if plugin_lifecycle_on_main_thread() {
        let thread_id = platform::current_thread_id();
        eprintln!(
            "[plugin-host] editor attach mode=main_thread instance={plugin_instance_id} thread_id={thread_id} message_loop=main_ipc_loop"
        );
        eprintln!(
            "[EDITOR THREAD CHECK]\nplugin_instance_id={plugin_instance_id}\nrequested_thread_id={thread_id}\ncurrent_thread_id={thread_id}\nis_audio_thread=false\nis_ui_thread=true\nis_plugin_host_thread=true\nmessage_loop_running=true\ncom_initialized=true\ndpi_awareness_context=per_monitor_v2"
        );
        processor.embed_set_instance_label(plugin_instance_id);
        // Real plug-in name for the "Loading Plugin <name>" shell overlay.
        processor.set_editor_title(display_title);
        eprintln!("[VST3Editor] createView instance_id={plugin_instance_id}");
        eprintln!("[plugin-editor] createView from existing controller (reuse loaded runtime)");
        eprintln!(
            "[editor-open] createView(editor) begin plugin={display_title} insert={plugin_instance_id} thread_id={thread_id}"
        );
        eprintln!(
            "[VST3 ATTACH VIEW]\nplugin_instance_id={plugin_instance_id}\nparent_hwnd=0x{parent_hwnd:x}\nsize={w}x{h}\nresult=begin"
        );
        let started = Instant::now();
        let handle = processor.embed_editor(parent_hwnd, 0, 0, w, h);
        let elapsed = started.elapsed();
        let attach_hwnd = processor.embed_attach_hwnd();
        eprintln!(
            "[editor-open] attach {} plugin={} insert={} parent=0x{parent_hwnd:x} hwnd=0x{attach_hwnd:x} size={w}x{h} thread_id={thread_id} duration_ms={}",
            if handle.is_some() { "ok" } else { "failed" },
            display_title,
            plugin_instance_id,
            elapsed.as_millis()
        );
        let (preferred_width, preferred_height) = processor
            .embed_content_size()
            .map(|(cw, ch)| (cw.max(1) as u32, ch.max(1) as u32))
            .unwrap_or((width.max(1), height.max(1)));
        let resizable = processor.editor_resizable().unwrap_or(true);
        let error = if handle.is_some() {
            None
        } else {
            Some("embed_editor failed on existing runtime instance".to_string())
        };
        eprintln!(
            "[VST3 ATTACH VIEW]\nplugin_instance_id={plugin_instance_id}\nparent_hwnd=0x{parent_hwnd:x}\nsize={w}x{h}\nresult={}\nduration_ms={}",
            if handle.is_some() { "ok" } else { "failed" },
            elapsed.as_millis()
        );
        // Emit EditorAttached / EditorAttachFailed and register the editor so the
        // main loop keeps pumping + refreshing it (shared with the worker drain).
        finalize_editor_attach(
            EditorAttachResult {
                request_id,
                plugin_instance_id: plugin_instance_id.to_string(),
                processor,
                handle,
                attach_hwnd,
                owner_hwnd: parent_hwnd,
                display_title: display_title.to_string(),
                preferred_width,
                preferred_height,
                resizable,
                error,
                elapsed,
            },
            registry,
            delayed_redraws,
            preview,
            out,
        );
        return;
    }

    pending_editor_attaches.insert(
        plugin_instance_id.to_string(),
        PendingEditorAttach {
            request_id,
            plugin_instance_id: plugin_instance_id.to_string(),
            parent_hwnd,
            display_title: display_title.to_string(),
            started_at: Instant::now(),
            stage: "attach_view",
            timeout_logged: false,
            processor: processor.clone(),
        },
    );
    eprintln!(
        "[EDITOR THREAD CHECK]\nplugin_instance_id={plugin_instance_id}\nrequested_thread_id={}\ncurrent_thread_id={}\nis_audio_thread=false\nis_ui_thread=false\nis_plugin_host_thread=true\nmessage_loop_running=true\ncom_initialized=true\ndpi_awareness_context=per_monitor_v2",
        platform::current_thread_id(),
        platform::current_thread_id()
    );
    eprintln!(
        "[VST3 EDITOR LIFECYCLE]\nplugin_instance_id={plugin_instance_id}\nstep=resolve_instance\nresult=ok\nduration_ms=0\nthread_id={}\npointer_valid=true\nerror_code=0",
        platform::current_thread_id()
    );
    let tx = attach_result_tx.clone();
    let instance = plugin_instance_id.to_string();
    let display_title = display_title.to_string();
    std::thread::Builder::new()
        .name(format!("plugin-editor-attach-{request_id}"))
        .spawn(move || {
            let started = Instant::now();
            let thread_id = platform::current_thread_id();
            let _ = platform::pump_messages();
            eprintln!(
                "[EDITOR THREAD CHECK]\nplugin_instance_id={instance}\nrequested_thread_id={thread_id}\ncurrent_thread_id={thread_id}\nis_audio_thread=false\nis_ui_thread=true\nis_plugin_host_thread=false\nmessage_loop_running=true\ncom_initialized=(initialized_by_embed_editor)\ndpi_awareness_context=(initialized_by_embed_editor)"
            );
            eprintln!(
                "[plugin-host-ui-thread] editor_attach_thread_ready=true thread_id={thread_id} message_loop_running=true"
            );
            processor.embed_set_instance_label(&instance);
            // Real plug-in name for the "Loading Plugin <name>" shell overlay.
            processor.set_editor_title(&display_title);
            eprintln!("[VST3Editor] createView instance_id={instance}");
            eprintln!("[plugin-editor] createView from existing controller (reuse loaded runtime)");
            eprintln!(
                "[VST3 CREATE VIEW]\nplugin_instance_id={instance}\nthread_id={thread_id}\nresult=begin"
            );
            eprintln!(
                "[VST3 EDITOR LIFECYCLE]\nplugin_instance_id={instance}\nstep=create_view_editor\nresult=begin\nduration_ms=0\nthread_id={thread_id}\npointer_valid=true\nerror_code=0"
            );
            eprintln!(
                "[VST3 ATTACH VIEW]\nplugin_instance_id={instance}\nparent_hwnd=0x{parent_hwnd:x}\nsize={}x{}\nresult=begin",
                w,
                h
            );
            let handle = processor.embed_editor(parent_hwnd, 0, 0, w, h);
            let elapsed = started.elapsed();
            let attach_hwnd = processor.embed_attach_hwnd();
            let (preferred_width, preferred_height) = processor
                .embed_content_size()
                .map(|(w, h)| (w.max(1) as u32, h.max(1) as u32))
                .unwrap_or((width.max(1), height.max(1)));
            let resizable = processor.editor_resizable().unwrap_or(true);
            let error = if handle.is_some() {
                None
            } else {
                Some("embed_editor failed on existing runtime instance".to_string())
            };
            eprintln!(
                "[VST3 EDITOR LIFECYCLE]\nplugin_instance_id={instance}\nstep=attach_view\nresult={}\nduration_ms={}\nthread_id={thread_id}\npointer_valid={}\nerror_code={}",
                if handle.is_some() { "ok" } else { "failed" },
                elapsed.as_millis(),
                handle.is_some(),
                if handle.is_some() { 0 } else { 1 }
            );
            eprintln!(
                "[VST3 ATTACH VIEW]\nplugin_instance_id={instance}\nparent_hwnd=0x{parent_hwnd:x}\nsize={}x{}\nresult={}\nduration_ms={}",
                w,
                h,
                if handle.is_some() { "ok" } else { "failed" },
                elapsed.as_millis()
            );
            eprintln!(
                "[VST3 SIZE VIEW]\nplugin_instance_id={instance}\npreferred_size={}x{}\nresizable={resizable}\nresult={}",
                preferred_width,
                preferred_height,
                if handle.is_some() { "ok" } else { "failed" }
            );
            let attached = handle.is_some();
            let _ = tx.send(EditorAttachResult {
                request_id,
                plugin_instance_id: instance,
                processor: processor.clone(),
                handle,
                attach_hwnd,
                owner_hwnd: parent_hwnd,
                display_title,
                preferred_width,
                preferred_height,
                resizable,
                error,
                elapsed,
            });
            if attached {
                loop {
                    if !processor.embed_is_valid() {
                        break;
                    }
                    processor.embed_refresh();
                    let _ = platform::pump_messages();
                    let _ = platform::wait_for_input(8);
                }
            }
        })
        .expect("spawn plugin editor attach worker");
    hlog!(
        "[PluginHostEditor] attach scheduled request_id={request_id} onSize=({width}x{height}) dpi={dpi}"
    );
}

fn drain_editor_attach_results(
    registry: &mut Registry,
    delayed_redraws: &mut Vec<DelayedGpuRedraw>,
    pending_editor_attaches: &mut HashMap<String, PendingEditorAttach>,
    attach_result_rx: &crossbeam_channel::Receiver<EditorAttachResult>,
    preview: &SharedPluginHostPreview,
    out: &mut io::Stdout,
) {
    while let Ok(result) = attach_result_rx.try_recv() {
        let Some(pending) = pending_editor_attaches.remove(&result.plugin_instance_id) else {
            if result.handle.is_some() {
                eprintln!(
                    "[EDITOR FAILURE SAFE EXIT]\nplugin_instance_id={}\nfailure_stage=late_attach_after_timeout\nplugin_audio_kept_alive = true\neditor_state = failed\napp_frozen = false",
                    result.plugin_instance_id
                );
                result.processor.embed_detach();
            }
            continue;
        };
        if pending.request_id != result.request_id {
            if result.handle.is_some() {
                result.processor.embed_detach();
            }
            continue;
        }
        finalize_editor_attach(result, registry, delayed_redraws, preview, out);
    }
}

/// Register a completed editor attach and emit `EditorAttached`/`EditorAttachFailed`
/// to the main app. Shared by the inline (main-thread) attach path and the
/// legacy worker-thread drain so both report identically. The `attach_hwnd`
/// goes into `registry` so the host main loop pumps + refreshes the editor.
fn finalize_editor_attach(
    result: EditorAttachResult,
    registry: &mut Registry,
    delayed_redraws: &mut Vec<DelayedGpuRedraw>,
    preview: &SharedPluginHostPreview,
    out: &mut io::Stdout,
) {
    if let Some(error) = result.error {
        eprintln!(
            "[editor-open] failed plugin={} insert={} stage=attach_view result={} thread_id={}",
            result.display_title,
            result.plugin_instance_id,
            error,
            platform::current_thread_id()
        );
        emit_attach_failed(out, &result.plugin_instance_id, &error);
        eprintln!(
            "[EDITOR FAILURE SAFE EXIT]\nplugin_instance_id={}\nfailure_stage=attach_view\nplugin_audio_kept_alive = true\neditor_state = failed\napp_frozen = false",
            result.plugin_instance_id
        );
        return;
    }
    let Some(handle) = result.handle else {
        emit_attach_failed(
            out,
            &result.plugin_instance_id,
            "embed_editor failed on existing runtime instance",
        );
        return;
    };
    let attach_hwnd = result.attach_hwnd;
    if attach_hwnd == 0 {
        eprintln!(
            "[PluginEditorHWND] WARNING attach_hwnd unavailable instance={} handle={handle}",
            result.plugin_instance_id
        );
    }
    let host_hwnd = if attach_hwnd != 0 {
        attach_hwnd
    } else {
        handle
    };
    registry.insert(
        result.plugin_instance_id.clone(),
        EditorState {
            plugin_instance_id: result.plugin_instance_id.clone(),
            host_hwnd,
            owner_hwnd: result.owner_hwnd,
            display_title: result.display_title.clone(),
            state: "Open",
        },
    );
    eprintln!(
        "[PluginHost] editor_state instance_id={} state=Open owner_hwnd=0x{:x} host_hwnd=0x{host_hwnd:x}",
        result.plugin_instance_id, result.owner_hwnd
    );
    eprintln!(
        "[VST3Editor] attached instance_id={} title=\"{}\"",
        result.plugin_instance_id, result.display_title
    );
    preview.lock().set_continuous_mode(true);
    eprintln!(
        "[PluginEditorLifecycle] initial resize instance={} size={}x{}",
        result.plugin_instance_id, result.preferred_width, result.preferred_height
    );
    eprintln!(
        "[editor-size] client rect = {}x{}",
        result.preferred_width, result.preferred_height
    );
    result.processor.embed_set_bounds(
        0,
        0,
        result.preferred_width as i32,
        result.preferred_height as i32,
    );
    result.processor.embed_refresh();
    eprintln!(
        "[editor-open] resize {}x{} ok plugin={} insert={} hwnd=0x{:x} thread_id={}",
        result.preferred_width,
        result.preferred_height,
        result.display_title,
        result.plugin_instance_id,
        attach_hwnd,
        platform::current_thread_id()
    );
    if platform::editor_safe_mode() {
        eprintln!("[PluginEditorSafe] attach: skipped focus walk and attach-time pump");
    } else {
        platform::focus_plugin_editor_child(attach_hwnd);
        platform::pump_editor_messages(attach_hwnd);
    }
    platform::log_capture_on_open(attach_hwnd);
    platform::set_editor_roots(registry.values().map(|state| state.host_hwnd).collect());
    platform::plugin_editor_snapshot("editor_open");
    eprintln!(
        "[PluginEditorResize] instance={} canResize={} preferred={}x{}",
        result.plugin_instance_id,
        result.resizable,
        result.preferred_width,
        result.preferred_height
    );
    eprintln!(
        "[PluginEditor] open complete engine_state=Running transport_playing=unknown instance={}",
        result.plugin_instance_id
    );
    eprintln!(
        "[editor-open] ready plugin={} insert={} hwnd=0x{:x} result=ok thread_id={}",
        result.display_title,
        result.plugin_instance_id,
        attach_hwnd,
        platform::current_thread_id()
    );
    SpherePluginHost::plugin_host_preview::PluginHostPreviewEngine::verify_unified_runtime(
        &result.plugin_instance_id,
        &result.plugin_instance_id,
        &result.plugin_instance_id,
        &result.plugin_instance_id,
        &result.plugin_instance_id,
        &result.plugin_instance_id,
        &result.plugin_instance_id,
        &result.plugin_instance_id,
    );
    eprintln!(
        "[plugin-host] attached result=ok instance={} handle=0x{handle:x} unified=true elapsed_ms={}",
        result.plugin_instance_id,
        result.elapsed.as_millis()
    );
    eprintln!(
        "[EDITOR MESSAGE PUMP]\nplugin_instance_id={}\nhost_window_hwnd=0x{:x}\nthread_id={}\npump_active=true\nlast_message_time_ms=0\nblocked_wait_detected=false\nipc_responsive=true\nwindow_responsive=true",
        result.plugin_instance_id,
        attach_hwnd,
        platform::current_thread_id()
    );
    delayed_redraws.push(DelayedGpuRedraw {
        instance_id: result.plugin_instance_id.clone(),
        deadline: Instant::now() + Duration::from_millis(100),
        second_resize: Some((result.preferred_width, result.preferred_height)),
    });
    let _ = ipc::write_frame(
        out,
        &HostEvent::EditorAttached {
            plugin_instance_id: result.plugin_instance_id,
            result: 0,
            preferred_width: result.preferred_width,
            preferred_height: result.preferred_height,
            resizable: result.resizable,
            host_hwnd: attach_hwnd,
        },
    );
}

fn expire_editor_attach_requests(
    pending_editor_attaches: &mut HashMap<String, PendingEditorAttach>,
    _attach_result_rx: &crossbeam_channel::Receiver<EditorAttachResult>,
    preview: &SharedPluginHostPreview,
    out: &mut io::Stdout,
) {
    let now = Instant::now();
    let timed_out: Vec<PendingEditorAttach> = pending_editor_attaches
        .values()
        .filter(|pending| {
            let timeout = match pending.stage {
                "create_view_editor" => EDITOR_CREATE_TIMEOUT,
                _ => EDITOR_ATTACH_TIMEOUT,
            };
            now.duration_since(pending.started_at) >= timeout
        })
        .cloned()
        .collect();
    for pending in timed_out {
        let elapsed_ms = now.duration_since(pending.started_at).as_millis();
        let Some(pending_live) = pending_editor_attaches.get_mut(&pending.plugin_instance_id)
        else {
            continue;
        };
        if pending_live.timeout_logged {
            continue;
        }
        pending_live.timeout_logged = true;
        pending_live
            .processor
            .embed_set_waiting_stage(pending.stage);
        eprintln!(
            "[EDITOR HANG DETECTED]\nplugin_instance_id={}\nplugin_name=(unknown)\nstage={}\nelapsed_ms={elapsed_ms}\nui_thread_blocked=false\nipc_thread_blocked=false\naudio_thread_blocked=false\nlast_successful_step=resolve_instance\nlast_vst3_result=(pending)\nhost_process_alive=true",
            pending.plugin_instance_id, pending.stage
        );
        eprintln!(
            "[PluginHost] editor_state instance_id={} state=Failed title=\"{}\"",
            pending.plugin_instance_id, pending.display_title
        );
        eprintln!(
            "[EDITOR HANG WATCHDOG]\nplugin_instance_id={}\nstage={}\nelapsed_ms={elapsed_ms}\ntimeout_ms={}\nui_thread_responsive=true\nipc_thread_responsive=true\naudio_thread_responsive=true\nhost_process_alive=true",
            pending.plugin_instance_id,
            pending.stage,
            EDITOR_ATTACH_TIMEOUT.as_millis()
        );
        eprintln!(
            "[EDITOR MESSAGE PUMP]\nplugin_instance_id={}\nhost_window_hwnd=0x{:x}\nthread_id={}\npump_active=true\nlast_message_time_ms=0\nblocked_wait_detected=true\nipc_responsive=true\nwindow_responsive=false",
            pending.plugin_instance_id,
            pending.parent_hwnd,
            platform::current_thread_id()
        );
        let audio_alive = preview.lock().has_instance(&pending.plugin_instance_id);
        eprintln!(
            "[EDITOR WAITING SAFE STATE]\nplugin_instance_id={}\nfailure_stage={}\nplugin_audio_kept_alive = {}\neditor_state = waiting\napp_frozen = false\nloading_shell_alive = true",
            pending.plugin_instance_id, pending.stage, audio_alive
        );
        let _ = out;
    }
}

fn emit_attach_failed(out: &mut io::Stdout, plugin_instance_id: &str, error: &str) {
    let _ = ipc::write_frame(
        out,
        &HostEvent::EditorAttachFailed {
            plugin_instance_id: plugin_instance_id.to_string(),
            error: error.to_string(),
        },
    );
}

/// Self-test path (`--selftest`): prove that the host can create a real
/// content **child** HWND distinct from a top HWND, with the required Win32
/// styles, and (optionally) attach a plugin to it. Drives the acceptance logs
/// without needing the main app or a real plugin.
///
/// Set `FUTUREBOARD_SELFTEST_PLUGIN_PATH` + `FUTUREBOARD_SELFTEST_CLASS_ID` to
/// also exercise a real VST3 attach. Exit code 0 on success.
fn run_selftest() -> i32 {
    match platform::create_selftest_windows() {
        Some((top_hwnd, content_hwnd)) => {
            let content_is_child = content_hwnd != top_hwnd && content_hwnd != 0;
            eprintln!("[plugin-view] selected_host_mode=main_owned_window");
            eprintln!("[plugin-view] top_hwnd=0x{top_hwnd:x}");
            eprintln!("[plugin-view] content_hwnd=0x{content_hwnd:x}");
            eprintln!("[plugin-view] content_is_child={content_is_child}");
            eprintln!("[plugin-view] content_parent=0x{top_hwnd:x}");
            if content_hwnd == top_hwnd {
                eprintln!("[plugin-view] ERROR content_hwnd == top_hwnd — not attaching");
                platform::destroy_selftest_windows(top_hwnd, content_hwnd);
                return 1;
            }
            eprintln!("[plugin-view] content_hwnd != top_hwnd");

            let mut code = 0;
            if let (Ok(path), Ok(class_id)) = (
                std::env::var("FUTUREBOARD_SELFTEST_PLUGIN_PATH"),
                std::env::var("FUTUREBOARD_SELFTEST_CLASS_ID"),
            ) {
                let region = EmbedRegion {
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 600,
                };
                match native_editor::attach_editor_into_parent(
                    content_hwnd,
                    &path,
                    &class_id,
                    region,
                ) {
                    Ok(handle) => {
                        eprintln!("[vst3-editor] attached begin parent=0x{content_hwnd:x}");
                        eprintln!("[vst3-editor] attached result=ok handle=0x{handle:x}");
                        native_editor::detach_editor(handle);
                    }
                    Err(err) => {
                        eprintln!("[vst3-editor] attached result=err {err}");
                        code = 1;
                    }
                }
            } else {
                eprintln!(
                    "[plugin-view] selftest: no FUTUREBOARD_SELFTEST_PLUGIN_PATH/CLASS_ID — \
                     HWND hierarchy only"
                );
            }

            platform::destroy_selftest_windows(top_hwnd, content_hwnd);
            code
        }
        None => {
            eprintln!("[plugin-view] selftest: window creation unavailable on this platform");
            // Not a failure on non-Windows — there is nothing to host there yet.
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Platform shims. Windows is the real implementation; other targets get no-op
// stubs so the binary still compiles and the IPC loop still runs.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use windows::core::BOOL;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::Foundation::{LPARAM, RECT, WAIT_OBJECT_0, WPARAM};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::HiDpi::{
        GetDpiForSystem, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetCapture, GetFocus, IsWindowEnabled, ReleaseCapture, SetFocus,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, ChildWindowFromPointEx, CreateWindowExW, DestroyWindow, DispatchMessageW,
        EnumChildWindows, EnumThreadWindows, GetAncestor, GetClassNameW, GetParent, GetWindow,
        GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsChild,
        IsDialogMessageW, IsWindow, IsWindowVisible, MsgWaitForMultipleObjectsEx, PeekMessageW,
        PostThreadMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage,
        WindowFromPoint, CWP_ALL, CW_USEDEFAULT, GA_PARENT, GA_ROOT, GWLP_HWNDPARENT, GWL_EXSTYLE,
        GWL_STYLE, GW_CHILD, GW_OWNER, HWND_TOP, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT,
        SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_KEYDOWN,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NULL, WM_RBUTTONDOWN,
        WM_RBUTTONUP, WM_TIMER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW,
        WS_VISIBLE,
    };

    /// End-to-end plugin debug switch (`FUTUREBOARD_PLUGIN_DEBUG=1`), shared
    /// with the narrower view-debug flag.
    pub fn plugin_debug() -> bool {
        static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *FLAG.get_or_init(|| {
            std::env::var_os("FUTUREBOARD_PLUGIN_DEBUG").is_some()
                || std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").is_some()
        })
    }

    /// Plugin editor safe mode (`FUTUREBOARD_PLUGIN_EDITOR_SAFE=1`): disables
    /// window-tree polling, per-message verbose logs, re-entrant pumping inside
    /// attach/load handlers, and experimental focus hacks. Keeps only minimal
    /// diagnostics (loop alive, pump gap, spin warning, focus/capture summary
    /// on click, snapshot on editor open).
    pub fn editor_safe_mode() -> bool {
        static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_PLUGIN_EDITOR_SAFE").is_some())
    }

    /// Coarse log rate limiter: allows at most `max` events per second.
    pub struct LogRate {
        window_start_ms: std::sync::atomic::AtomicU64,
        count: std::sync::atomic::AtomicU32,
        max_per_sec: u32,
    }

    impl LogRate {
        pub const fn new(max_per_sec: u32) -> Self {
            Self {
                window_start_ms: std::sync::atomic::AtomicU64::new(0),
                count: std::sync::atomic::AtomicU32::new(0),
                max_per_sec,
            }
        }

        pub fn allow(&self) -> bool {
            use std::sync::atomic::Ordering;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let start = self.window_start_ms.load(Ordering::Relaxed);
            if now.saturating_sub(start) >= 1000 {
                self.window_start_ms.store(now, Ordering::Relaxed);
                self.count.store(1, Ordering::Relaxed);
                return true;
            }
            self.count.fetch_add(1, Ordering::Relaxed) < self.max_per_sec
        }
    }

    /// Editor root HWNDs currently registered (registry mirror) — feeds the
    /// click-path diagnostic and the on-demand window snapshot without
    /// threading the registry through every pump call.
    static EDITOR_ROOTS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

    pub fn set_editor_roots(roots: Vec<u64>) {
        if let Ok(mut guard) = EDITOR_ROOTS.lock() {
            if *guard != roots {
                *guard = roots;
            }
        }
    }

    fn editor_roots() -> Vec<u64> {
        EDITOR_ROOTS.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn class_name(hwnd: HWND) -> String {
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }

    fn window_title(hwnd: HWND) -> String {
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 256];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }

    fn hwnd_from(handle: u64) -> HWND {
        HWND(handle as *mut core::ffi::c_void)
    }

    /// True if `hwnd` is a real Win32 dialog (class `#32770`).
    fn is_dialog_class(hwnd: HWND) -> bool {
        if hwnd.0.is_null() {
            return false;
        }
        let mut buf = [0u16; 16];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == "#32770"
    }

    /// Nearest dialog (`#32770`) in the parent chain of `hwnd`, if any.
    /// `IsDialogMessageW` must only run against real dialog windows — calling
    /// it with an arbitrary window as the "dialog" swallows Tab/arrow/Enter/
    /// Escape keystrokes destined for plugin editor controls.
    fn dialog_ancestor(hwnd: HWND) -> Option<HWND> {
        let mut cur = hwnd;
        let mut depth = 0;
        while !cur.0.is_null() && depth < 32 {
            if is_dialog_class(cur) {
                return Some(cur);
            }
            cur = unsafe { GetAncestor(cur, GA_PARENT) };
            depth += 1;
        }
        None
    }

    pub fn com_init() {
        // STA: VST3 editors require apartment-threaded COM (spec Part 9).
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
    }

    pub fn ensure_dpi_awareness() {
        unsafe {
            let ctx = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            eprintln!(
                "[PluginEditor] dpi_awareness_context=0x{:x} tid={}",
                ctx.0 as usize,
                GetCurrentThreadId()
            );
        }
    }

    pub fn system_dpi() -> u32 {
        unsafe {
            let dpi = GetDpiForSystem();
            if dpi == 0 {
                96
            } else {
                dpi
            }
        }
    }

    pub fn com_uninit() {
        unsafe { CoUninitialize() };
    }

    pub fn current_thread_id() -> u64 {
        unsafe { GetCurrentThreadId() as u64 }
    }

    pub fn is_process_alive(pid: u32) -> bool {
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut code = 0u32;
            let alive = if GetExitCodeProcess(handle, &mut code).is_ok() {
                code == STILL_ACTIVE
            } else {
                false
            };
            let _ = CloseHandle(handle);
            alive
        }
    }

    pub fn is_window(handle: u64) -> bool {
        if handle == 0 {
            return false;
        }
        unsafe { IsWindow(Some(hwnd_from(handle))).as_bool() }
    }

    pub fn window_process_id(handle: u64) -> Option<u32> {
        if handle == 0 {
            return None;
        }
        unsafe {
            let hwnd = hwnd_from(handle);
            if !IsWindow(Some(hwnd)).as_bool() {
                return None;
            }
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                None
            } else {
                Some(pid)
            }
        }
    }

    pub fn log_window_identity_chain(label: &str, handle: u64) {
        if handle == 0 {
            eprintln!("[PluginEditorHWNDChain] {label} hwnd=0x0 valid=false");
            return;
        }
        unsafe {
            let mut hwnd = hwnd_from(handle);
            for depth in 0..8 {
                if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
                    eprintln!(
                        "[PluginEditorHWNDChain] {label} depth={depth} hwnd=0x{:x} valid=false",
                        hwnd.0 as u64
                    );
                    break;
                }
                let parent = GetParent(hwnd).unwrap_or_default();
                let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
                let mut pid = 0u32;
                let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
                eprintln!(
                    "[PluginEditorHWNDChain] {label} depth={depth} hwnd=0x{:x} pid={pid} tid={tid} parent=0x{:x} owner=0x{:x} class='{}' title='{}'",
                    hwnd.0 as u64,
                    parent.0 as u64,
                    owner.0 as u64,
                    class_name(hwnd),
                    window_title(hwnd)
                );
                let next = if !owner.0.is_null() { owner } else { parent };
                if next.0.is_null() || next == hwnd {
                    break;
                }
                hwnd = next;
            }
        }
    }

    pub fn focus_editor_window(handle: u64) -> bool {
        if handle == 0 {
            return false;
        }
        unsafe {
            let hwnd = hwnd_from(handle);
            if !IsWindow(Some(hwnd)).as_bool() {
                return false;
            }
            eprintln!("[NativeEditorShell] show/focus requested existing=0x{handle:x}");
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = BringWindowToTop(hwnd);
            let ok = SetForegroundWindow(hwnd).as_bool();
            let _ = SetFocus(Some(hwnd));
            eprintln!("[NativeEditorShell] foreground result={ok}");
            ok
        }
    }

    fn log_window_brief(label: &str, hwnd: HWND) {
        if hwnd.0.is_null() {
            return;
        }
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let owner = HWND(GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT) as *mut core::ffi::c_void);
            eprintln!(
                "[PluginEditor] window styles {label} hwnd=0x{:x} owner=0x{:x} style=0x{style:08x}",
                hwnd.0 as u64, owner.0 as u64
            );
        }
    }

    /// Focus the deepest plugin-owned child under the embed host HWND.
    pub fn focus_plugin_editor_child(host_hwnd: u64) {
        if host_hwnd == 0 {
            return;
        }
        unsafe {
            let host = hwnd_from(host_hwnd);
            if !IsWindow(Some(host)).as_bool() {
                return;
            }
            log_window_brief("top", host);
            let mut target = host;
            let mut child = GetWindow(host, GW_CHILD).unwrap_or_default();
            while !child.0.is_null() && IsWindow(Some(child)).as_bool() {
                target = child;
                child = GetWindow(child, GW_CHILD).unwrap_or_default();
            }
            if target != host {
                let _ = SetFocus(Some(target));
                eprintln!("[PluginEditor] focus set child=0x{:x}", target.0 as u64);
            } else {
                let _ = SetFocus(Some(host));
                eprintln!("[PluginEditor] focus set child=0x{:x}", host.0 as u64);
            }
            {
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, GetFocus};
                eprintln!(
                    "[PluginEditorInput] focus=0x{:x} capture=0x{:x}",
                    GetFocus().0 as u64,
                    GetCapture().0 as u64
                );
            }
        }
    }

    /// Pump messages for the plugin editor subtree (host + descendants).
    /// Bounded: drains at most `MAX_PUMP_PER_CALL` messages per call so a
    /// message-storming plugin window can never wedge the loop here.
    pub fn pump_editor_messages(host_hwnd: u64) {
        if host_hwnd == 0 {
            return;
        }
        unsafe {
            let host = hwnd_from(host_hwnd);
            if !IsWindow(Some(host)).as_bool() {
                return;
            }
            static PUMP_LOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let mut pumped = 0u32;
            let mut msg = MSG::default();
            while pumped < MAX_PUMP_PER_CALL
                && PeekMessageW(&mut msg, Some(host), 0, 0, PM_REMOVE).as_bool()
            {
                let _ = TranslateMessage(&msg);
                // Generic dialog routing: only treat real `#32770` dialogs as
                // dialogs; never run IsDialogMessage against plugin windows.
                if let Some(dialog) = dialog_ancestor(msg.hwnd) {
                    if IsDialogMessageW(dialog, &msg).as_bool() {
                        pumped += 1;
                        continue;
                    }
                }
                DispatchMessageW(&msg);
                pumped += 1;
            }
            if pumped > 0 {
                let n = PUMP_LOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n.is_multiple_of(120) {
                    eprintln!("[PluginEditor] modal/dialog message pump active drained={pumped}");
                }
            }
        }
    }

    /// Upper bound on messages drained by any single pump call. The loop comes
    /// back within milliseconds, so capping a single drain only bounds latency
    /// for pathological message storms — it never drops messages.
    const MAX_PUMP_PER_CALL: u32 = 512;

    /// Block until this thread's message queue has input or `timeout_ms`
    /// elapses. Returns `true` when woken by input. This replaces the old
    /// unconditional `sleep(8ms)` poll: the loop now idles in the kernel and
    /// wakes immediately on messages (or a `wake_ui_thread` kick), so it never
    /// spins and never adds fixed latency to plugin window input.
    pub fn wait_for_input(timeout_ms: u32) -> bool {
        unsafe {
            // MWMO_INPUTAVAILABLE: also wake for input that was already in the
            // queue when we started waiting (avoids the classic stale-QS-bits
            // missed-wakeup, which would otherwise show up as input lag).
            let result =
                MsgWaitForMultipleObjectsEx(None, timeout_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            result == WAIT_OBJECT_0
        }
    }

    /// Wake the UI thread out of `wait_for_input` (used by the stdin reader
    /// thread when a new IPC command arrives). WM_NULL is a no-op message.
    pub fn wake_ui_thread(thread_id: u64) {
        unsafe {
            let _ = PostThreadMessageW(thread_id as u32, WM_NULL, WPARAM(0), LPARAM(0));
        }
    }

    /// Capture safety on editor open (spec: capture should be null or
    /// plugin-owned while interacting). Logs the current focus/capture once;
    /// if an HWND *unrelated* to the new editor holds mouse capture on this
    /// thread, release it. Never sets capture.
    pub fn log_capture_on_open(host_hwnd: u64) {
        unsafe {
            let capture = GetCapture();
            let focus = GetFocus();
            eprintln!(
                "[PluginEditorInput] editor_open focus=0x{:x} capture=0x{:x}",
                focus.0 as u64, capture.0 as u64
            );
            if capture.0.is_null() {
                return;
            }
            let host = hwnd_from(host_hwnd);
            let related = host_hwnd != 0 && (capture == host || IsChild(host, capture).as_bool());
            if !related {
                let _ = ReleaseCapture();
                eprintln!(
                    "[PluginEditorInput] released_unrelated_capture=0x{:x}",
                    capture.0 as u64
                );
            }
        }
    }

    /// Debug classification of a message target: wrapper chrome, our embed
    /// host windows, a dialog, or a plugin-owned window.
    fn classify_target(hwnd: HWND) -> &'static str {
        let class = class_name(hwnd);
        match class.as_str() {
            "FutureboardDauxVst3EditorContent" | "FutureboardDauxVst3EditorChild" => "embed_host",
            "FutureboardDauxVst3EditorDetached" => "embed_top",
            "SpherePluginEditorShell" | "SpherePluginEditorContent" => "wrapper",
            "#32770" => "dialog",
            _ => "plugin_owned",
        }
    }

    fn log_input_dispatch(msg: &MSG) {
        // Default dispatch logging covers only mouse button / key messages —
        // never per-move/per-timer/per-paint floods (those are throttled
        // separately in `log_throttled_noise`).
        let interesting = matches!(
            msg.message,
            WM_LBUTTONDOWN
                | WM_LBUTTONUP
                | WM_RBUTTONDOWN
                | WM_RBUTTONUP
                | WM_MBUTTONDOWN
                | WM_KEYDOWN
        );
        if !interesting {
            return;
        }
        let x = (msg.lParam.0 & 0xFFFF) as i16 as i32;
        let y = ((msg.lParam.0 >> 16) & 0xFFFF) as i16 as i32;
        let root = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
        eprintln!(
            "[PluginUIThread] dispatch hwnd=0x{:x} msg=0x{:04x} target={} class='{}' \
             client=({x},{y}) screen=({},{}) root=0x{:x}",
            msg.hwnd.0 as u64,
            msg.message,
            classify_target(msg.hwnd),
            class_name(msg.hwnd),
            msg.pt.x,
            msg.pt.y,
            root.0 as u64,
        );
    }

    /// Throttled high-frequency message tracing (debug mode, not safe mode):
    /// WM_MOUSEMOVE and WM_TIMER at most 2/sec each.
    fn log_throttled_noise(msg: &MSG) {
        static MOUSE_MOVE_RATE: LogRate = LogRate::new(2);
        static TIMER_RATE: LogRate = LogRate::new(2);
        let rate = match msg.message {
            WM_MOUSEMOVE => &MOUSE_MOVE_RATE,
            WM_TIMER => &TIMER_RATE,
            _ => return,
        };
        if rate.allow() {
            eprintln!(
                "[PluginUIThread] trace hwnd=0x{:x} msg=0x{:04x} class='{}' (throttled 2/sec)",
                msg.hwnd.0 as u64,
                msg.message,
                class_name(msg.hwnd),
            );
        }
    }

    /// Click-path diagnostic (spec item 9): for a left click, log everything
    /// needed to tell wrong-hit-test / disabled-window / focus-capture /
    /// wrong-thread / consumed-by-dialog-routing apart. Throttled to 4/sec.
    fn log_click_path(msg: &MSG) {
        unsafe {
            let pt = msg.pt; // screen coordinates of the click
            let wfp = WindowFromPoint(pt);
            let focus = GetFocus();
            let capture = GetCapture();
            let mut target_pid = 0u32;
            let target_tid = GetWindowThreadProcessId(msg.hwnd, Some(&mut target_pid));
            let our_tid = windows::Win32::System::Threading::GetCurrentThreadId();
            eprintln!(
                "[PluginClickPath] screen=({},{}) msg_hwnd=0x{:x} class='{}' target={} \
                 enabled={} visible={} target_tid={target_tid} target_pid={target_pid} \
                 our_tid={our_tid} same_thread={}",
                pt.x,
                pt.y,
                msg.hwnd.0 as u64,
                class_name(msg.hwnd),
                classify_target(msg.hwnd),
                IsWindowEnabled(msg.hwnd).as_bool(),
                IsWindowVisible(msg.hwnd).as_bool(),
                target_tid == our_tid,
            );
            eprintln!(
                "[PluginClickPath] window_from_point=0x{:x} wfp_class='{}' wfp_enabled={} \
                 focus=0x{:x} capture=0x{:x}",
                wfp.0 as u64,
                class_name(wfp),
                IsWindowEnabled(wfp).as_bool(),
                focus.0 as u64,
                capture.0 as u64,
            );
            // Hit-test the wrapper (cross-process top-level) and each editor
            // root so a wrong/covered hit target is visible in one log line.
            let wrapper = GetAncestor(msg.hwnd, GA_ROOT);
            let mut probes: Vec<(&'static str, HWND)> = vec![("wrapper", wrapper)];
            let roots = editor_roots();
            for root in &roots {
                probes.push(("editor_root", hwnd_from(*root)));
            }
            for (label, probe) in probes {
                if probe.0.is_null() || !IsWindow(Some(probe)).as_bool() {
                    continue;
                }
                let mut client = pt;
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(probe, &mut client);
                let child = ChildWindowFromPointEx(probe, client, CWP_ALL);
                eprintln!(
                    "[PluginClickPath] {label}=0x{:x} child_from_point=0x{:x} child_class='{}'",
                    probe.0 as u64,
                    child.0 as u64,
                    class_name(child),
                );
            }
        }
    }

    /// Non-blocking drain of this thread's message queue. Returns the number
    /// of messages dispatched (pump-gap watchdog input). Bounded per call.
    pub fn pump_messages() -> u32 {
        let debug = plugin_debug();
        let safe = editor_safe_mode();
        static CLICK_PATH_RATE: LogRate = LogRate::new(4);
        let mut dispatched = 0u32;
        unsafe {
            let mut msg = MSG::default();
            while dispatched < MAX_PUMP_PER_CALL
                && PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool()
            {
                if debug && !safe {
                    log_throttled_noise(&msg);
                }
                if debug {
                    log_input_dispatch(&msg);
                }
                // Focus/capture + hit-test summary on click (kept in safe mode;
                // throttled so a click storm cannot flood stderr).
                let click_diag = msg.message == WM_LBUTTONDOWN && CLICK_PATH_RATE.allow();
                if click_diag {
                    log_click_path(&msg);
                }
                let _ = TranslateMessage(&msg);
                // `IsDialogMessageW(msg.hwnd, …)` treated EVERY window as a
                // dialog, swallowing Tab/arrow/Enter/Escape keystrokes that
                // belong to plugin editor controls. Only route through real
                // `#32770` dialogs in the target's parent chain, and never
                // consume a message that IsDialogMessage did not handle.
                let mut dialog_candidate = 0u64;
                let mut dialog_handled = false;
                if let Some(dialog) = dialog_ancestor(msg.hwnd) {
                    dialog_candidate = dialog.0 as u64;
                    static DIALOG_RATE: LogRate = LogRate::new(1);
                    if DIALOG_RATE.allow() {
                        eprintln!("[PluginUIThread] dialog candidate hwnd=0x{dialog_candidate:x}");
                    }
                    dialog_handled = IsDialogMessageW(dialog, &msg).as_bool();
                    if dialog_handled && debug && !safe {
                        eprintln!(
                            "[PluginUIThread] IsDialogMessage handled msg=0x{:04x} hwnd=0x{:x}",
                            msg.message, msg.hwnd.0 as u64
                        );
                    }
                }
                if click_diag {
                    eprintln!(
                        "[PluginClickPath] dialog_candidate=0x{dialog_candidate:x} \
                         is_dialog_message_handled={dialog_handled} dispatched={}",
                        !dialog_handled
                    );
                }
                if dialog_handled {
                    dispatched += 1;
                    continue;
                }
                DispatchMessageW(&msg);
                dispatched += 1;
            }
        }
        dispatched
    }

    /// One-shot window/thread state snapshot (spec item 8): wrapper, embed
    /// child, dialogs, and descendants with class/style/parent/owner/enabled/
    /// visible/rect/thread/process. Triggered once per editor open and from
    /// the pump-gap watchdog — throttled, never per-frame.
    pub fn plugin_editor_snapshot(reason: &str) {
        static SNAPSHOT_RATE: LogRate = LogRate::new(1);
        if !SNAPSHOT_RATE.allow() {
            return;
        }
        const MAX_WINDOWS: usize = 64;
        fn snapshot_one(label: &str, hwnd: HWND, count: &mut usize) {
            if hwnd.0.is_null() || *count >= MAX_WINDOWS {
                return;
            }
            unsafe {
                if !IsWindow(Some(hwnd)).as_bool() {
                    return;
                }
                *count += 1;
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                let parent = GetParent(hwnd).unwrap_or_default();
                let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
                let mut rect = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rect);
                let mut pid = 0u32;
                let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
                eprintln!(
                    "[PluginEditorSnapshot] {label} hwnd=0x{:x} class='{}' style=0x{style:08x} \
                     exstyle=0x{exstyle:08x} parent=0x{:x} owner=0x{:x} enabled={} visible={} \
                     rect=({},{},{},{}) tid={tid} pid={pid}",
                    hwnd.0 as u64,
                    class_name(hwnd),
                    parent.0 as u64,
                    owner.0 as u64,
                    IsWindowEnabled(hwnd).as_bool(),
                    IsWindowVisible(hwnd).as_bool(),
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom,
                );
            }
        }
        struct SnapCtx {
            count: usize,
        }
        unsafe extern "system" fn snap_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = unsafe { &mut *(lparam.0 as *mut SnapCtx) };
            if ctx.count >= MAX_WINDOWS {
                return BOOL(0);
            }
            snapshot_one("descendant", hwnd, &mut ctx.count);
            BOOL(1)
        }
        unsafe extern "system" fn snap_thread_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = unsafe { &mut *(lparam.0 as *mut SnapCtx) };
            if ctx.count >= MAX_WINDOWS {
                return BOOL(0);
            }
            snapshot_one("thread_window", hwnd, &mut ctx.count);
            unsafe {
                let _ = EnumChildWindows(Some(hwnd), Some(snap_child), lparam);
            }
            BOOL(1)
        }
        let roots = editor_roots();
        eprintln!(
            "[PluginEditorSnapshot] begin reason={reason} editor_roots={}",
            roots.len()
        );
        let mut ctx = SnapCtx { count: 0 };
        unsafe {
            for root in &roots {
                let root_hwnd = hwnd_from(*root);
                if !IsWindow(Some(root_hwnd)).as_bool() {
                    continue;
                }
                // GA_ROOT crosses the process boundary to the main-app wrapper.
                let wrapper = GetAncestor(root_hwnd, GA_ROOT);
                snapshot_one("wrapper", wrapper, &mut ctx.count);
                if wrapper != root_hwnd {
                    snapshot_one("embed_root", root_hwnd, &mut ctx.count);
                }
                let _ = EnumChildWindows(
                    Some(wrapper),
                    Some(snap_child),
                    LPARAM(&mut ctx as *mut SnapCtx as isize),
                );
            }
            // Popups/dialogs the plugin created on this UI thread (not under
            // the wrapper tree — e.g. #32770 file dialogs, license prompts).
            let tid = windows::Win32::System::Threading::GetCurrentThreadId();
            let _ = EnumThreadWindows(
                tid,
                Some(snap_thread_window),
                LPARAM(&mut ctx as *mut SnapCtx as isize),
            );
            let focus = GetFocus();
            let capture = GetCapture();
            eprintln!(
                "[PluginEditorSnapshot] end windows={} focus=0x{:x} capture=0x{:x}",
                ctx.count, focus.0 as u64, capture.0 as u64
            );
        }
    }

    /// Debug helper: diff the set of windows on this UI thread plus every
    /// descendant of the given editor roots against `known`, logging windows
    /// that appeared or disappeared. Confirms plugin-created popups/dialogs
    /// exist, are enabled, and live in the expected tree — no vendor logic.
    pub fn log_window_tree_changes(
        roots: &[u64],
        known: &mut std::collections::HashMap<u64, String>,
    ) {
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let set = unsafe { &mut *(lparam.0 as *mut Vec<u64>) };
            set.push(hwnd.0 as u64);
            unsafe {
                let _ = EnumChildWindows(Some(hwnd), Some(collect_children), lparam);
            }
            BOOL(1)
        }
        unsafe extern "system" fn collect_children(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let set = unsafe { &mut *(lparam.0 as *mut Vec<u64>) };
            set.push(hwnd.0 as u64);
            BOOL(1)
        }
        let mut current: Vec<u64> = Vec::with_capacity(64);
        unsafe {
            let tid = windows::Win32::System::Threading::GetCurrentThreadId();
            let _ = EnumThreadWindows(
                tid,
                Some(collect),
                LPARAM(&mut current as *mut Vec<u64> as isize),
            );
            for root in roots {
                if *root != 0 && IsWindow(Some(hwnd_from(*root))).as_bool() {
                    current.push(*root);
                    let _ = EnumChildWindows(
                        Some(hwnd_from(*root)),
                        Some(collect_children),
                        LPARAM(&mut current as *mut Vec<u64> as isize),
                    );
                }
            }
        }
        let current: std::collections::HashSet<u64> = current.into_iter().collect();
        for hwnd_v in &current {
            if known.contains_key(hwnd_v) {
                continue;
            }
            let hwnd = hwnd_from(*hwnd_v);
            let class = class_name(hwnd);
            unsafe {
                let parent = GetParent(hwnd).unwrap_or_default();
                let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
                eprintln!(
                    "[PluginEditorWindowTree] child hwnd=0x{hwnd_v:x} class='{class}' \
                     parent=0x{:x} owner=0x{:x} enabled={} visible={}",
                    parent.0 as u64,
                    owner.0 as u64,
                    IsWindowEnabled(hwnd).as_bool(),
                    IsWindowVisible(hwnd).as_bool(),
                );
            }
            known.insert(*hwnd_v, class);
        }
        known.retain(|hwnd_v, class| {
            if current.contains(hwnd_v) {
                return true;
            }
            eprintln!("[PluginEditorWindowTree] gone hwnd=0x{hwnd_v:x} class='{class}'");
            false
        });
    }

    /// Create a top window + a real WS_CHILD content window using the
    /// predefined `STATIC` class (no RegisterClass/WndProc needed). Returns
    /// `(top_hwnd, content_hwnd)` as `u64`s.
    pub fn create_selftest_windows() -> Option<(u64, u64)> {
        unsafe {
            let top = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Futureboard Plugin Host Selftest"),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                820,
                640,
                None,
                None,
                None,
                None,
            )
            .ok()?;

            let content = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                800,
                600,
                Some(top),
                None,
                None,
                None,
            )
            .ok()?;

            Some((top.0 as u64, content.0 as u64))
        }
    }

    pub fn destroy_selftest_windows(top: u64, content: u64) {
        unsafe {
            if content != 0 {
                let _ = DestroyWindow(hwnd_from(content));
            }
            if top != 0 {
                let _ = DestroyWindow(hwnd_from(top));
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn com_init() {}
    pub fn ensure_dpi_awareness() {}
    pub fn system_dpi() -> u32 {
        96
    }
    pub fn com_uninit() {}
    pub fn current_thread_id() -> u64 {
        0
    }
    pub fn is_process_alive(_pid: u32) -> bool {
        true
    }
    pub fn is_window(handle: u64) -> bool {
        handle != 0
    }
    pub fn window_process_id(_handle: u64) -> Option<u32> {
        None
    }
    pub fn log_window_identity_chain(_label: &str, _handle: u64) {}
    pub fn focus_editor_window(_handle: u64) -> bool {
        false
    }
    pub fn pump_messages() -> u32 {
        0
    }
    pub fn plugin_debug() -> bool {
        false
    }
    pub fn editor_safe_mode() -> bool {
        false
    }
    pub fn set_editor_roots(_roots: Vec<u64>) {}
    pub fn plugin_editor_snapshot(_reason: &str) {}
    pub fn log_capture_on_open(_host_hwnd: u64) {}
    pub fn wake_ui_thread(_thread_id: u64) {}
    pub fn wait_for_input(timeout_ms: u32) -> bool {
        std::thread::sleep(std::time::Duration::from_millis(timeout_ms as u64));
        false
    }
    pub fn log_window_tree_changes(
        _roots: &[u64],
        _known: &mut std::collections::HashMap<u64, String>,
    ) {
    }
    pub fn focus_plugin_editor_child(_host_hwnd: u64) {}
    pub fn pump_editor_messages(_host_hwnd: u64) {}
    pub fn create_selftest_windows() -> Option<(u64, u64)> {
        None
    }
    pub fn destroy_selftest_windows(_top: u64, _content: u64) {}
}
