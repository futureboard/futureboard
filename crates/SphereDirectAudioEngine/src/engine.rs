//! Core engine logic.
//!
//! `EngineInner` is the real audio engine.  It is wrapped in `Arc` and shared
//! between the N-API class (on the JS/main thread) and the audio callback
//! (on cpal's realtime thread).
//!
//! Thread safety:
//!   - All control parameters are sent through a `crossbeam_channel` and
//!     consumed at the top of each audio block — no locking in the hot path.
//!   - Meter output uses `AtomicU32` (f32 bit-cast) — both sides access with
//!     `Relaxed` ordering, which is sufficient for meter display purposes.
//!   - Stream lifecycle (open/close) is guarded by `parking_lot::Mutex`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{BufferSize, FromSample, Sample, SampleFormat, SizedSample};
use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;
use sphere_audio_plugins::{canonical_plugin_id, process_stereo_sample};

use crate::audio_file::AudioFileAudition;
use crate::audio_graph::is_routing_track_type;
use crate::audio_source::{sample_source_stereo, ClipAudioSource};
#[cfg(target_os = "windows")]
use crate::backend::wasapi_exclusive::{self, WasapiExclusiveHandle};
#[cfg(target_os = "windows")]
use crate::backend::wdm_ks::{self, WdmKsHandle};
use crate::backend::{
    cpal_backend::{self, CpalStreamHandle},
    list_available_backends, BackendKind, DauxDeviceConfig,
};
use crate::command::EngineCommand;
use crate::device;
use crate::dsp::{meter::smooth_peak, oscillator::SineOscillator};
use crate::error::SphereAudioError;
use crate::graph::{MasterState, TrackState};
use crate::latency_graph::apply_pdc_delay_block;
use crate::recording::{self, RecordingSession};
use crate::runtime::{
    ClipDspProcessor, RuntimeInsert, RuntimePreviewMode, RuntimeProject, RuntimeTrack,
    RuntimeTrackInputSource,
};
use crate::tempo_map::{TempoMap, TempoPoint};
use crate::transport::{self, RuntimeTransportSnapshot};
use crate::types::{
    EngineProjectSnapshot, EngineStatus, EngineTrackInputSourceSnapshot, JsAudioDeviceInfo,
    JsDauxBackendInfo, JsDauxConfig, JsDauxStatus, JsDeviceOpenConfig, JsEngineDebugInfo,
    JsMeterSnapshot, JsPluginOutputMeterSnapshot, JsRecordingResult, JsRecordingStatus,
    JsSphereAudioStatus, JsStartRecordingConfig, JsTrackMeterSnapshot,
};
use crate::vst3_processor::RuntimeTransportContext;

// ── Version ───────────────────────────────────────────────────────────────────

pub const ENGINE_VERSION: &str = "0.1.0";

/// Keep a few sources that just disappeared from the arrangement so a normal
/// delete/undo cycle can reuse the open mapping or decoded buffer. Active
/// sources do not count toward this limit.
const MAX_INACTIVE_AUDIO_CACHE_ENTRIES: usize = 4;

fn command_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_AUDIO_COMMAND_DEBUG").is_some())
}

/// `FUTUREBOARD_INPUT_DEBUG=1` enables throttled raw-input-peak traces from the
/// control thread (see `log_input_debug_throttled`). Off by default.
fn input_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_INPUT_DEBUG").is_some())
}

/// `FUTUREBOARD_AUDIO_CALLBACK_DEBUG=1` enables the realtime callback's
/// occasional eprintln traces (graph swap, mute, render-path). Off by default
/// so the audio thread never formats strings or writes to stdio — see
/// `tasks/native/audio-system-spec.md` §1 and Phase A finding A.2.2.
fn callback_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_AUDIO_CALLBACK_DEBUG").is_some())
}

fn plugin_restore_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_PLUGIN_DEBUG").is_some())
}

/// `FUTUREBOARD_PLUGIN_BRIDGE_DEBUG=1` enables the throttled bridge
/// missed-deadline / recovered traces from the audio callback. Off by default —
/// stall accounting stays in `RuntimeInsert::bridge_missed_blocks` either way.
fn bridge_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_PLUGIN_BRIDGE_DEBUG").is_some())
}

fn log_sphere_audio_processor_diagnostics_once() {
    static LOGGED: OnceLock<()> = OnceLock::new();
    LOGGED.get_or_init(|| {
        eprintln!(
            "SphereAudioProcessor:\n- InternalRePitch: available\n- Signalsmith Stretch: {}",
            if SphereAudioProcessor::signalsmith_stretch_available() {
                "available"
            } else {
                "unavailable"
            }
        );
    });
}

// ── Realtime constants shared with render.rs ──────────────────────────────────

pub const TEST_TONE_AMPLITUDE: f32 = 0.125; // −18 dBFS  (safe default test level)
pub const PEAK_DECAY: f32 = 0.94; // per audio block, responsive UI peak decay

// ── Atomic helpers (pub for render.rs) ────────────────────────────────────────

#[inline]
pub fn f32_store(v: f32) -> u32 {
    v.to_bits()
}
#[inline]
pub fn f32_load(v: u32) -> f32 {
    f32::from_bits(v)
}

#[inline]
fn monitor_channel_pair(channels: &[u32]) -> Option<(u32, u32)> {
    channels
        .first()
        .copied()
        .map(|left| (left, channels.get(1).copied().unwrap_or(left)))
}

#[inline]
const fn pack_monitor_source_pair(left: u32, right: u32) -> u64 {
    left as u64 | ((right as u64) << 32)
}

#[inline]
const fn unpack_monitor_source_pair(pair: u64) -> (u32, u32) {
    (pair as u32, (pair >> 32) as u32)
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
fn routed_input_peaks(input_peaks: &[f32], channels: &[u32]) -> Option<(f32, f32)> {
    let (left, right) = monitor_channel_pair(channels)?;
    let peak_l = input_peaks.get(left as usize).copied().unwrap_or(0.0);
    let peak_r = input_peaks.get(right as usize).copied().unwrap_or(0.0);
    Some((peak_l, peak_r))
}

#[inline]
pub(crate) fn atomic_max_f32_bits(target: &AtomicU32, value: f32) {
    let value = value.max(0.0);
    let mut current = target.load(Ordering::Relaxed);
    loop {
        if value <= f32::from_bits(current) {
            break;
        }
        match target.compare_exchange_weak(
            current,
            value.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

// ── Audio engine lifecycle (control → audio, Relaxed reads in callback) ───────

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioEngineState {
    Running = 0,
    Paused = 1,
    LoadingProject = 2,
    ClosingProject = 3,
    DeviceSwitching = 4,
    Suspended = 5,
}

impl AudioEngineState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Paused,
            2 => Self::LoadingProject,
            3 => Self::ClosingProject,
            4 => Self::DeviceSwitching,
            5 => Self::Suspended,
            _ => Self::Running,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::LoadingProject => "LoadingProject",
            Self::ClosingProject => "ClosingProject",
            Self::DeviceSwitching => "DeviceSwitching",
            Self::Suspended => "Suspended",
        }
    }

    pub fn outputs_silence(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Dropout Protection mode (Settings → Playback). Controls how much internal
/// headroom the engine keeps against control/UI/plugin jitter, independent of
/// the device buffer size. In this slice the mode sets the dropout-detection
/// warn fraction (how early a block approaching its deadline is flagged) and is
/// delivered to the engine so the deferred render-ahead safety buffer can read
/// it. `Medium` is the recommended default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropoutProtectionMode {
    /// Lowest latency, minimal safety margin — flag only true overruns.
    Off = 0,
    /// Small safety margin / conservative scheduling.
    Light = 1,
    /// Default recommended mode — better protection during UI activity.
    Medium = 2,
    /// Maximum stability — widest margin (may add internal latency later).
    High = 3,
}

impl DropoutProtectionMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Off,
            1 => Self::Light,
            3 => Self::High,
            _ => Self::Medium,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Light => "Light",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    /// Fraction `(num, den)` of the per-block deadline at or above which a block
    /// is counted as a dropout-risk. Stricter modes tolerate less headroom
    /// erosion before flagging, so the UI surfaces trouble earlier and the user
    /// can react (or, in the deferred render-ahead slice, the buffer absorbs it).
    #[inline]
    pub fn dropout_threshold_ratio(self) -> (u64, u64) {
        match self {
            Self::Off => (100, 100),
            Self::Light => (90, 100),
            Self::Medium => (80, 100),
            Self::High => (70, 100),
        }
    }
}

/// Why the most recent dropout-risk block was flagged. Stored as a `u8` in
/// [`SharedState`] (audio → control); only `CallbackOverrun` is detected in this
/// slice — the rest are reserved for the plugin-watchdog / disk-streaming slices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropoutReason {
    None = 0,
    CallbackOverrun = 1,
    GraphSwapLate = 2,
    DiskCacheMiss = 3,
    PluginOverrun = 4,
}

impl DropoutReason {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::CallbackOverrun,
            2 => Self::GraphSwapLate,
            3 => Self::DiskCacheMiss,
            4 => Self::PluginOverrun,
            _ => Self::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CallbackOverrun => "callback-overrun",
            Self::GraphSwapLate => "graph-swap-late",
            Self::DiskCacheMiss => "disk-cache-miss",
            Self::PluginOverrun => "plugin-overrun",
        }
    }
}

/// Snapshot of dropout-protection diagnostics, read off the control thread.
#[derive(Clone, Copy, Debug)]
pub struct DropoutDiagnostics {
    pub protection_mode: DropoutProtectionMode,
    pub dropout_count: u64,
    pub last_reason: DropoutReason,
    pub callback_last_us: u32,
    pub callback_max_us: u32,
    pub callback_deadline_us: u32,
    pub slow_callback_count: u64,
}

/// Realtime-safe output-callback timing + dropout detection. Atomics only — no
/// allocation, lock, logging, or syscall — so it is safe to call at the end of
/// every audio callback. Shared by the legacy in-process callback and the DAUx
/// backend so both paths report identical diagnostics.
#[inline]
pub(crate) fn record_output_callback_timing(
    shared: &SharedState,
    elapsed_us: u32,
    block_frames: usize,
    sample_rate: u32,
) {
    shared.last_callback_us.store(elapsed_us, Ordering::Relaxed);
    if elapsed_us > shared.max_callback_us.load(Ordering::Relaxed) {
        shared.max_callback_us.store(elapsed_us, Ordering::Relaxed);
    }
    // Wall-clock budget for this block: it must render in less time than it takes
    // to play, or the device starves (xrun).
    let deadline_us = if sample_rate > 0 {
        ((block_frames as u64 * 1_000_000) / sample_rate as u64).min(u32::MAX as u64) as u32
    } else {
        0
    };
    shared
        .callback_deadline_us
        .store(deadline_us, Ordering::Relaxed);
    if elapsed_us >= 2_000 {
        shared.slow_callback_count.fetch_add(1, Ordering::Relaxed);
    }
    if deadline_us > 0 {
        let mode =
            DropoutProtectionMode::from_u8(shared.dropout_protection_mode.load(Ordering::Relaxed));
        let (num, den) = mode.dropout_threshold_ratio();
        let threshold = ((deadline_us as u64 * num) / den).min(u32::MAX as u64) as u32;
        if elapsed_us >= threshold {
            shared.dropout_count.fetch_add(1, Ordering::Relaxed);
            shared
                .dropout_last_reason
                .store(DropoutReason::CallbackOverrun as u8, Ordering::Relaxed);
        }
    }
}

// ── Shared state (accessed by both control and audio threads) ─────────────────

pub struct SharedState {
    // Meters (audio → control)
    pub peak_l: AtomicU32,
    pub peak_r: AtomicU32,
    pub rms_l: AtomicU32,
    pub rms_r: AtomicU32,

    // Control flags (control → audio, relaxed reads in callback)
    pub test_tone_enabled: AtomicBool,
    pub test_tone_freq: AtomicU32, // Hz as f32 bits
    pub master_volume: AtomicU32,  // linear f32 bits
    pub playing: AtomicBool,
    /// Lifecycle gate for realtime output (see [`AudioEngineState`]).
    pub engine_state: AtomicU8,
    pub position_samples: AtomicU64, // samples elapsed from start
    pub sample_rate: AtomicU32,

    // Transport clock (control ↔ audio via commands + Relaxed reads)
    pub bpm_bits: AtomicU64,
    pub time_sig_num: AtomicU32,
    pub time_sig_den: AtomicU32,
    pub loop_enabled: AtomicBool,
    pub loop_start_samples: AtomicU64,
    pub loop_end_samples: AtomicU64,
    pub metronome_enabled: AtomicBool,

    // Recording (Phase U): input monitor tap + session flag for realtime mix.
    pub recording_active: AtomicBool,
    pub recording_monitor_mix: AtomicBool,
    pub recording_monitor_l: AtomicU32,
    pub recording_monitor_r: AtomicU32,
    pub live_input_active: AtomicBool,
    pub live_input_l: AtomicU32,
    pub live_input_r: AtomicU32,
    pub live_input_peak_l: AtomicU32,
    pub live_input_peak_r: AtomicU32,

    // ── Live monitoring / input bus (Layers 4, 6, 7) ──────────────────────
    /// Packed source channel indices (`left | right << 32`) tapped by the
    /// monitor path. Publishing the pair in one atomic prevents a callback from
    /// observing half of an old route and half of a new one.
    monitor_src_pair: AtomicU64,
    /// `true` when input and output are driven by the same ASIO device clock.
    /// Backend/render code can use this to choose a lower monitor latency target.
    pub monitor_shared_clock: AtomicBool,
    /// Max-hold input peak across *all* capture channels (ASIO session), reset
    /// by the Settings input-test poll via `swap`.
    pub session_input_peak: AtomicU32,
    /// Lock-free stereo bridge from the input callback to the output render
    /// callback. Carries the actual monitored samples (not just a peak).
    pub input_ring: crate::input_ring::InputRing,
    /// `true` when any track has monitoring enabled — gates the output-side
    /// monitor mix so the render callback only taps the ring when needed.
    pub monitor_enabled_any: AtomicBool,
    /// Linear monitor gain applied to the input bus before it reaches master.
    pub monitor_gain: AtomicU32,
    /// Peak of the input bus *after* the render callback reads it from the ring
    /// (Layer 4 verification — distinct from the raw `live_input_peak_*`).
    pub input_bus_peak_l: AtomicU32,
    pub input_bus_peak_r: AtomicU32,

    // ── Realtime diagnostics counters (Part 4) ────────────────────────────
    pub input_cb_count: AtomicU64,
    pub output_cb_count: AtomicU64,
    pub input_frames_received: AtomicU64,
    pub monitor_frames_consumed: AtomicU64,
    pub monitor_ring_underruns: AtomicU64,
    pub monitor_ring_overruns: AtomicU64,
    pub record_ring_overruns: AtomicU64,
    pub output_xruns: AtomicU64,
    /// Peak of the record capture (pre-file) for diagnostics.
    pub record_peak: AtomicU32,
    /// Measured output-stream latency (callback → play-out) in seconds, stored
    /// as f32 bits. Published by the output render callback from the cpal
    /// timestamp; read at record-stop to auto-compensate the take position.
    pub output_latency_secs: AtomicU32,
    /// Measured recording-input latency (capture → callback) in seconds, f32
    /// bits. Published by the take's capture callback. Together with
    /// `output_latency_secs` this is the round-trip a recorded clip must shift
    /// earlier by so live overdubs line up with what the player heard.
    pub record_input_latency_secs: AtomicU32,
    /// Peak actually mixed to the monitor output for diagnostics.
    pub monitor_output_peak: AtomicU32,

    // ── Realtime recording waveform preview (Part 1) ──────────────────────
    /// Finalized preview bins pushed by the recording input callback.
    pub preview_ring: crate::input_ring::PreviewPeakRing,
    /// `true` while a take is feeding the preview ring.
    pub recording_preview_active: AtomicBool,
    /// Monotonic id for the current take (lets the UI discard stale chunks).
    pub recording_preview_id: AtomicU64,
    /// Transport sample at which the take started (preview clip origin).
    pub recording_preview_start_sample: AtomicU64,
    pub recording_preview_sample_rate: AtomicU32,
    pub recording_preview_channels: AtomicU32,
    pub recording_preview_peaks_per_sec: AtomicU32,

    // ── Callback watchdog (audio-hang spec §12; audio → control) ──────────
    /// Duration of the most recent output callback, microseconds.
    pub last_callback_us: AtomicU32,
    /// Worst output-callback duration seen since stream open, microseconds.
    pub max_callback_us: AtomicU32,
    /// Blocks that exceeded the 2 ms debug threshold.
    pub slow_callback_count: AtomicU64,

    // ── Dropout protection (Part 2; audio ↔ control) ──────────────────────
    /// Active [`DropoutProtectionMode`] as `u8` (control → audio). Default
    /// `Medium`. Read by [`record_output_callback_timing`] to pick the warn
    /// fraction; reserved for the deferred render-ahead safety buffer.
    pub dropout_protection_mode: AtomicU8,
    /// Blocks flagged as dropout-risk (elapsed ≥ mode fraction of the deadline).
    pub dropout_count: AtomicU64,
    /// [`DropoutReason`] of the most recent flagged block, as `u8`.
    pub dropout_last_reason: AtomicU8,
    /// Per-block wall-clock budget published each callback, microseconds.
    pub callback_deadline_us: AtomicU32,

    // DAUx diagnostics (incremented by audio thread, read by control thread)
    pub glitch_count: AtomicU64,
    pub mmcss_active: AtomicBool,
    /// Set by a backend when the audio device disappears mid-stream (USB
    /// unplugged, default device changed, exclusive-mode timeout). Read by the
    /// control thread to surface a DeviceLost state and trigger recovery.
    pub device_lost: AtomicBool,
    /// Playback plugin delay compensation (Phase W). Settings → Playback.
    pub pdc_enabled: AtomicBool,
    /// Monotonic generation of the latency-compensation graph (bumped on PDC
    /// toggle). Stamped into the offline-export snapshot for graph-version parity.
    pub latency_graph_version: AtomicU64,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            peak_l: AtomicU32::new(f32_store(0.0)),
            peak_r: AtomicU32::new(f32_store(0.0)),
            rms_l: AtomicU32::new(f32_store(0.0)),
            rms_r: AtomicU32::new(f32_store(0.0)),
            test_tone_enabled: AtomicBool::new(false),
            test_tone_freq: AtomicU32::new(f32_store(440.0)),
            master_volume: AtomicU32::new(f32_store(1.0)),
            playing: AtomicBool::new(false),
            engine_state: AtomicU8::new(AudioEngineState::Paused as u8),
            position_samples: AtomicU64::new(0),
            sample_rate: AtomicU32::new(44100),
            bpm_bits: AtomicU64::new(120.0_f64.to_bits()),
            time_sig_num: AtomicU32::new(4),
            time_sig_den: AtomicU32::new(4),
            loop_enabled: AtomicBool::new(false),
            loop_start_samples: AtomicU64::new(0),
            loop_end_samples: AtomicU64::new(0),
            metronome_enabled: AtomicBool::new(false),
            recording_active: AtomicBool::new(false),
            recording_monitor_mix: AtomicBool::new(false),
            recording_monitor_l: AtomicU32::new(f32_store(0.0)),
            recording_monitor_r: AtomicU32::new(f32_store(0.0)),
            live_input_active: AtomicBool::new(false),
            live_input_l: AtomicU32::new(f32_store(0.0)),
            live_input_r: AtomicU32::new(f32_store(0.0)),
            live_input_peak_l: AtomicU32::new(f32_store(0.0)),
            live_input_peak_r: AtomicU32::new(f32_store(0.0)),
            monitor_src_pair: AtomicU64::new(pack_monitor_source_pair(0, 1)),
            monitor_shared_clock: AtomicBool::new(false),
            session_input_peak: AtomicU32::new(f32_store(0.0)),
            input_ring: crate::input_ring::InputRing::default(),
            monitor_enabled_any: AtomicBool::new(false),
            monitor_gain: AtomicU32::new(f32_store(1.0)),
            input_bus_peak_l: AtomicU32::new(f32_store(0.0)),
            input_bus_peak_r: AtomicU32::new(f32_store(0.0)),
            input_cb_count: AtomicU64::new(0),
            output_cb_count: AtomicU64::new(0),
            input_frames_received: AtomicU64::new(0),
            monitor_frames_consumed: AtomicU64::new(0),
            monitor_ring_underruns: AtomicU64::new(0),
            monitor_ring_overruns: AtomicU64::new(0),
            record_ring_overruns: AtomicU64::new(0),
            output_xruns: AtomicU64::new(0),
            record_peak: AtomicU32::new(f32_store(0.0)),
            output_latency_secs: AtomicU32::new(f32_store(0.0)),
            record_input_latency_secs: AtomicU32::new(f32_store(0.0)),
            monitor_output_peak: AtomicU32::new(f32_store(0.0)),
            preview_ring: crate::input_ring::PreviewPeakRing::default(),
            recording_preview_active: AtomicBool::new(false),
            recording_preview_id: AtomicU64::new(0),
            recording_preview_start_sample: AtomicU64::new(0),
            recording_preview_sample_rate: AtomicU32::new(0),
            recording_preview_channels: AtomicU32::new(0),
            recording_preview_peaks_per_sec: AtomicU32::new(0),
            last_callback_us: AtomicU32::new(0),
            max_callback_us: AtomicU32::new(0),
            slow_callback_count: AtomicU64::new(0),
            dropout_protection_mode: AtomicU8::new(DropoutProtectionMode::Medium as u8),
            dropout_count: AtomicU64::new(0),
            dropout_last_reason: AtomicU8::new(DropoutReason::None as u8),
            callback_deadline_us: AtomicU32::new(0),
            glitch_count: AtomicU64::new(0),
            mmcss_active: AtomicBool::new(false),
            device_lost: AtomicBool::new(false),
            pdc_enabled: AtomicBool::new(true),
            latency_graph_version: AtomicU64::new(1),
        }
    }
}

impl SharedState {
    #[inline]
    pub(crate) fn set_monitor_source_pair(&self, left: u32, right: u32) {
        self.monitor_src_pair
            .store(pack_monitor_source_pair(left, right), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn monitor_source_pair(&self) -> (u32, u32) {
        unpack_monitor_source_pair(self.monitor_src_pair.load(Ordering::Relaxed))
    }
}

// ── Engine inner ───────────────────────────────────────────────────────────────

/// Active stream variant — cpal-backed, ASIO duplex session, or a raw
/// Windows backend thread.
enum ActiveStream {
    Cpal(CpalStreamHandle),
    #[cfg(all(target_os = "windows", feature = "asio"))]
    // Boxed: the duplex handle (with its per-channel meter bank) is much
    // larger than the other variants (clippy::large_enum_variant).
    AsioDuplex(Box<crate::backend::asio_session::AsioDuplexHandle>),
    #[cfg(target_os = "windows")]
    WasapiExclusive(WasapiExclusiveHandle),
    #[cfg(target_os = "windows")]
    WdmKs(WdmKsHandle),
}

impl ActiveStream {
    fn cmd_tx(&self) -> Option<&crossbeam_channel::Sender<EngineCommand>> {
        match self {
            ActiveStream::Cpal(h) => Some(&h.cmd_tx),
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(h) => Some(&h.cmd_tx),
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(h) => Some(&h.cmd_tx),
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(h) => Some(&h.cmd_tx),
        }
    }
    fn play(&self) -> Result<(), String> {
        match self {
            ActiveStream::Cpal(h) => h.play(),
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(h) => h.play(),
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(_) => Ok(()), // already playing from stream start
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(_) => Ok(()), // already playing from stream start
        }
    }
    fn pause(&self) -> Result<(), String> {
        match self {
            ActiveStream::Cpal(h) => h.pause(),
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(h) => h.pause(),
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(_) => Ok(()), // no pause in exclusive — caller mutes output
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(_) => Ok(()), // no pause in low-level backend — caller mutes output
        }
    }
    #[cfg(all(target_os = "windows", feature = "asio"))]
    fn as_asio_duplex(&self) -> Option<&crate::backend::asio_session::AsioDuplexHandle> {
        match self {
            ActiveStream::AsioDuplex(h) => Some(h.as_ref()),
            _ => None,
        }
    }
    #[allow(dead_code)]
    fn sample_rate(&self) -> u32 {
        match self {
            ActiveStream::Cpal(h) => h.sample_rate,
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(h) => h.sample_rate,
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(h) => h.sample_rate,
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(h) => h.sample_rate,
        }
    }
    #[allow(dead_code)]
    fn buffer_size(&self) -> u32 {
        match self {
            ActiveStream::Cpal(h) => h.buffer_size,
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(h) => h.buffer_size,
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(h) => h.buffer_size,
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(h) => h.buffer_size,
        }
    }
    #[allow(dead_code)]
    fn device_name(&self) -> &str {
        match self {
            ActiveStream::Cpal(h) => &h.device_name,
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(h) => &h.device_name,
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(h) => &h.device_name,
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(h) => &h.device_name,
        }
    }
    fn backend_name(&self) -> &str {
        match self {
            ActiveStream::Cpal(h) => &h.backend_name,
            #[cfg(all(target_os = "windows", feature = "asio"))]
            ActiveStream::AsioDuplex(_) => "DAUx ASIO",
            #[cfg(target_os = "windows")]
            ActiveStream::WasapiExclusive(_) => "DAUx WASAPI Exclusive",
            #[cfg(target_os = "windows")]
            ActiveStream::WdmKs(_) => "DAUx WDM-KS",
        }
    }
}

// Safety: same rationale as before — all access is on the JS/main thread under Mutex.
unsafe impl Send for ActiveStream {}
unsafe impl Sync for ActiveStream {}

/// Live input-level test stream (Settings "Test Input" button). Holds the
/// cpal input stream and a shared peak atomic the callback writes and the UI
/// polls. Stored only inside `EngineInner` and touched only on the control
/// thread, so it inherits `EngineInner`'s `Send`/`Sync` assertion.
struct InputTestHandle {
    _stream: cpal::Stream,
    peak: Arc<AtomicU32>,
}

struct LiveInputHandle {
    _stream: cpal::Stream,
    device_id: Option<String>,
}

pub struct EngineInner {
    // Shared atomic state
    pub shared: Arc<SharedState>,

    // Active stream (cpal or WASAPI exclusive).
    // Dropping this stops the stream.
    active_stream: Mutex<Option<ActiveStream>>,

    // Legacy cpal stream path kept for compatibility with open_device().
    stream: Mutex<Option<cpal::Stream>>,
    cmd_tx: Mutex<Option<Sender<EngineCommand>>>,

    // Non-realtime mutable status (device names, error strings, etc.)
    status: Mutex<EngineStatus>,

    // In-memory track graph — updated from project snapshots / param commands.
    tracks: Mutex<Vec<TrackState>>,
    #[allow(dead_code)]
    master: Mutex<MasterState>,

    // Last loaded project snapshot (optional, for reference/debugging).
    project: Mutex<Option<EngineProjectSnapshot>>,
    // Incremented only after an incremental arm/monitor/route transaction
    // commits. `load_project` uses this to merge a newer input edit that arrived
    // while its graph was being prepared, then serializes the command ordering
    // under `project` so stale graph loads cannot overwrite that edit.
    input_state_revision: AtomicU64,

    // Prepared render graph shared with new streams and pushed to callbacks.
    runtime: Mutex<RuntimeProject>,
    plugin_bridge_sinks: Mutex<crate::plugin_bridge::PluginBridgeSinkMap>,
    audio_cache: Mutex<HashMap<String, Arc<ClipAudioSource>>>,
    inactive_audio_cache_lru: Mutex<VecDeque<String>>,

    // DAUx config & glitch counter (shared with audio thread for diagnostics).
    glitch_counter: Arc<AtomicU64>,
    daux_config: Mutex<DauxDeviceConfig>,

    // Active recording session (None when not recording).
    recording: Mutex<Option<RecordingSession>>,

    // Live input-level test stream (None when not testing input).
    input_test: Mutex<Option<InputTestHandle>>,

    // Engine-owned live input stream for armed/monitored track meters.
    live_input: Mutex<Option<LiveInputHandle>>,

    // Capabilities of the open ASIO session (None otherwise). Set by
    // `open_daux`, cleared by `close_device_inner`.
    asio_caps: Mutex<Option<crate::backend::AsioSessionCaps>>,

    // Settings input test reads the ASIO session peak instead of owning a
    // stream (true only between start/stop_input_test on the ASIO backend).
    input_test_uses_session: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
struct OutputStreamCandidate {
    config: cpal::StreamConfig,
    sample_format: SampleFormat,
    label: &'static str,
}

// ── Thread-safety declarations ────────────────────────────────────────────────
//
// On Windows, `cpal::Stream` (WASAPI backend) carries a
// `NotSendSyncAcrossAllPlatforms(PhantomData<*mut ()>)` marker that makes it
// `!Send`.  This is conservative — the WASAPI COM interfaces *can* be used from
// another thread as long as the stream is not concurrently accessed without
// synchronisation.
//
// Safety contract we uphold:
//   1. `EngineInner::stream` is accessed ONLY from the JS / main thread (via
//      `parking_lot::Mutex`).  No code path sends the `Mutex` guard across a
//      thread boundary.
//   2. The cpal audio callback has its OWN state captures (oscillator, local
//      vars) and accesses shared data only through `Arc<SharedState>` (which
//      is genuinely `Send + Sync` — plain atomics).  It never touches the
//      `Stream` handle.
//   3. The `Sender<EngineCommand>` inside `cmd_tx` is `Send`; it is only
//      written from the JS thread and read inside the audio callback's
//      `try_recv()`.
//
// Given these invariants, treating `EngineInner` as `Send + Sync` does not
// introduce data races.
unsafe impl Send for EngineInner {}
unsafe impl Sync for EngineInner {}

impl Default for EngineInner {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineInner {
    pub fn new() -> Self {
        log_sphere_audio_processor_diagnostics_once();
        Self {
            shared: Arc::new(SharedState::default()),
            active_stream: Mutex::new(None),
            stream: Mutex::new(None),
            cmd_tx: Mutex::new(None),
            status: Mutex::new(EngineStatus::default()),
            tracks: Mutex::new(Vec::new()),
            master: Mutex::new(MasterState::default()),
            project: Mutex::new(None),
            input_state_revision: AtomicU64::new(0),
            runtime: Mutex::new(RuntimeProject::default()),
            plugin_bridge_sinks: Mutex::new(Default::default()),
            audio_cache: Mutex::new(HashMap::new()),
            inactive_audio_cache_lru: Mutex::new(VecDeque::new()),
            glitch_counter: Arc::new(AtomicU64::new(0)),
            daux_config: Mutex::new(DauxDeviceConfig::default()),
            recording: Mutex::new(None),
            input_test: Mutex::new(None),
            live_input: Mutex::new(None),
            asio_caps: Mutex::new(None),
            input_test_uses_session: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn pdc_enabled(&self) -> bool {
        if std::env::var_os("FUTUREBOARD_PDC").is_some_and(|v| v == "0" || v == "false") {
            return false;
        }
        self.shared.pdc_enabled.load(Ordering::Relaxed)
    }

    pub fn set_pdc_enabled(&self, enabled: bool) {
        let prev = self.shared.pdc_enabled.swap(enabled, Ordering::Relaxed);
        if prev != enabled {
            // Latency-compensated graph changed; bump the generation so offline
            // export can stamp/compare the version it rendered against.
            self.shared
                .latency_graph_version
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Monotonic generation of the realtime latency-compensation graph. Bumped
    /// whenever Global Latency Sync (PDC) is toggled. Stamped into the offline
    /// export snapshot so the exporter can report/verify it rendered against the
    /// same graph generation as realtime playback.
    #[inline]
    pub fn latency_graph_version(&self) -> u64 {
        self.shared.latency_graph_version.load(Ordering::Relaxed)
    }

    /// Snapshot the currently attached external-plugin DSP endpoints for an
    /// offline export worker. Cloning only increments the shared handles.
    pub fn plugin_bridge_sinks(&self) -> crate::plugin_bridge::PluginBridgeSinkMap {
        self.plugin_bridge_sinks.lock().clone()
    }

    /// Active Dropout Protection mode (Settings → Playback).
    pub fn dropout_protection_mode(&self) -> DropoutProtectionMode {
        DropoutProtectionMode::from_u8(self.shared.dropout_protection_mode.load(Ordering::Relaxed))
    }

    /// Set the Dropout Protection mode. Control → audio via a single relaxed
    /// store; the audio callback reads it when classifying each block.
    pub fn set_dropout_protection_mode(&self, mode: DropoutProtectionMode) {
        self.shared
            .dropout_protection_mode
            .store(mode as u8, Ordering::Relaxed);
    }

    /// Snapshot the realtime dropout-protection counters for the control thread
    /// (status bar / settings). Pure atomic reads — never blocks the callback.
    pub fn dropout_diagnostics(&self) -> DropoutDiagnostics {
        let s = &self.shared;
        DropoutDiagnostics {
            protection_mode: DropoutProtectionMode::from_u8(
                s.dropout_protection_mode.load(Ordering::Relaxed),
            ),
            dropout_count: s.dropout_count.load(Ordering::Relaxed),
            last_reason: DropoutReason::from_u8(s.dropout_last_reason.load(Ordering::Relaxed)),
            callback_last_us: s.last_callback_us.load(Ordering::Relaxed),
            callback_max_us: s.max_callback_us.load(Ordering::Relaxed),
            callback_deadline_us: s.callback_deadline_us.load(Ordering::Relaxed),
            slow_callback_count: s.slow_callback_count.load(Ordering::Relaxed),
        }
    }

    /// Current transport play flag (set/cleared only by Start/StopTransport).
    pub fn transport_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Current lifecycle gate of the realtime callback.
    pub fn engine_state(&self) -> AudioEngineState {
        AudioEngineState::from_u8(self.shared.engine_state.load(Ordering::Relaxed))
    }

    // ── Lifecycle ──────────────────────────────────────────────────────────

    pub fn get_version(&self) -> String {
        ENGINE_VERSION.to_string()
    }

    pub fn get_status(&self) -> JsSphereAudioStatus {
        let st = self.status.lock().clone();
        let sample_rate = self.shared.sample_rate.load(Ordering::Relaxed).max(1);
        let position_samples = self.shared.position_samples.load(Ordering::Relaxed);
        JsSphereAudioStatus {
            available: true,
            running: st.running,
            stream_open: st.stream_open,
            transport_playing: self.shared.playing.load(Ordering::Relaxed),
            position_seconds: position_samples as f64 / sample_rate as f64,
            version: ENGINE_VERSION.to_string(),
            backend_name: cpal::default_host().id().name().to_string(),
            sample_rate: st.sample_rate,
            buffer_size: st.buffer_size,
            input_device: st.input_device,
            output_device: st.output_device,
            last_error: st.last_error,
        }
    }

    pub fn list_input_devices(&self) -> Vec<JsAudioDeviceInfo> {
        device::list_input_devices()
    }

    pub fn list_output_devices(&self) -> Vec<JsAudioDeviceInfo> {
        device::list_output_devices()
    }

    /// Open (or re-open) an audio output stream.
    /// Closes any existing stream first.
    pub fn open_device(&self, config: JsDeviceOpenConfig) -> Result<(), SphereAudioError> {
        self.close_device_inner();

        let (dev, dev_name) = device::resolve_output_device(config.output_device_id.as_deref())
            .map_err(SphereAudioError::DeviceNotFound)?;

        let candidates =
            output_stream_candidates(&dev, &config).map_err(SphereAudioError::StreamOpenFailed)?;

        let mut last_error = None;
        let mut selected = None;

        for candidate in candidates {
            let (tx, rx) = bounded::<EngineCommand>(512);
            let shared_cb = Arc::clone(&self.shared);
            shared_cb
                .sample_rate
                .store(candidate.config.sample_rate.0, Ordering::Relaxed);
            let initial_runtime = self
                .project
                .lock()
                .as_ref()
                .map(|snapshot| {
                    let mut audio_cache = self.audio_cache.lock();
                    match RuntimeProject::build(
                        snapshot,
                        candidate.config.sample_rate.0,
                        &mut audio_cache,
                        None,
                        self.pdc_enabled(),
                    ) {
                        Ok(runtime) => runtime,
                        Err(e) => {
                            eprintln!(
                                "[SphereAudio] open_device: invalid routing graph ({e}), keeping previous runtime"
                            );
                            let mut runtime = self.runtime.lock().clone();
                            runtime.sample_rate = candidate.config.sample_rate.0;
                            runtime
                        }
                    }
                })
                .unwrap_or_else(|| {
                    let mut runtime = self.runtime.lock().clone();
                    runtime.sample_rate = candidate.config.sample_rate.0;
                    runtime
                });
            *self.runtime.lock() = initial_runtime.clone();

            match build_output_stream(
                &dev,
                &candidate.config,
                candidate.sample_format,
                rx,
                shared_cb,
                initial_runtime,
            ) {
                Ok(stream) => {
                    selected = Some((candidate, tx, stream));
                    break;
                }
                Err(e) => {
                    last_error = Some(format!("{} config failed: {e}", candidate.label));
                }
            }
        }

        let (selected_config, tx, stream) = selected.ok_or_else(|| {
            SphereAudioError::StreamOpenFailed(
                last_error.unwrap_or_else(|| "no stream config candidates available".to_string()),
            )
        })?;

        let sample_rate = selected_config.config.sample_rate.0;
        let buffer_size = reported_buffer_size(&selected_config.config);
        eprintln!("[audio-engine] active_sample_rate={sample_rate} block_size={buffer_size}");

        // Store the stream and sender.
        *self.stream.lock() = Some(stream);
        *self.cmd_tx.lock() = Some(tx);

        // Update status.
        let mut st = self.status.lock();
        st.stream_open = true;
        st.running = false;
        st.sample_rate = sample_rate;
        st.buffer_size = buffer_size;
        st.output_device = Some(dev_name);
        st.last_error = None;

        Ok(())
    }

    /// Stop and close the audio stream.
    pub fn close_device(&self) {
        self.close_device_inner();
    }

    fn close_device_inner(&self) {
        // Preserve the input ring's SPSC contract across backend switches: the
        // standalone capture callback must be gone before a duplex ASIO producer
        // can start writing into the same ring.
        self.stop_live_input_stream();
        // Drop active DAUx stream (stops WASAPI exclusive thread or cpal stream).
        *self.active_stream.lock() = None;
        // Drop legacy cpal stream path.
        *self.stream.lock() = None;
        *self.cmd_tx.lock() = None;
        *self.asio_caps.lock() = None;

        let mut st = self.status.lock();
        st.stream_open = false;
        st.running = false;
    }

    /// Start audio playback (calls `stream.play()`).
    pub fn start(&self) -> Result<(), SphereAudioError> {
        {
            let st = self.status.lock();
            if st.stream_open && st.running {
                return Ok(());
            }
        }

        // Try DAUx active stream first — drop `active_stream` lock before `play()`.
        let daux_play = {
            let guard = self.active_stream.lock();
            guard.as_ref().map(|stream| stream.play())
        };
        if let Some(result) = daux_play {
            result.map_err(SphereAudioError::StreamStartFailed)?;
            self.shared.playing.store(false, Ordering::Relaxed); // transport starts paused
            self.status.lock().running = true;
            return Ok(());
        }
        // Legacy cpal path.
        let play_result = {
            let guard = self.stream.lock();
            guard
                .as_ref()
                .ok_or(SphereAudioError::EngineNotOpen)?
                .play()
                .map_err(|e| SphereAudioError::StreamStartFailed(e.to_string()))
        };
        play_result?;
        self.shared.playing.store(false, Ordering::Relaxed); // transport starts paused
        self.status.lock().running = true;
        Ok(())
    }

    /// Stop audio playback (calls `stream.pause()`).
    pub fn stop(&self) {
        if let Some(stream) = self.active_stream.lock().as_ref() {
            let _ = stream.pause();
        } else if let Some(stream) = self.stream.lock().as_ref() {
            let _ = stream.pause();
        }
        self.shared.playing.store(false, Ordering::Relaxed);
        self.status.lock().running = false;
    }

    /// Explicit shutdown for application exit — do not rely on `Drop` alone.
    /// Stops transport, pauses the device stream, and releases realtime resources.
    pub fn shutdown(&self) {
        self.shared
            .engine_state
            .store(AudioEngineState::ClosingProject as u8, Ordering::Relaxed);
        eprintln!("[AudioEngine] state -> ClosingProject");
        self.shared.playing.store(false, Ordering::Relaxed);
        let _ = self.send_command(EngineCommand::StopTransport);
        self.stop();
        self.close_device_inner();
    }

    // ── Transport ──────────────────────────────────────────────────────────

    pub fn play(&self) -> Result<(), SphereAudioError> {
        if self.shared.playing.load(Ordering::Relaxed) {
            if transport_freeze_debug_enabled() {
                eprintln!("[play-debug engine] play() skipped — transport already playing");
            }
            return Ok(());
        }
        if transport_freeze_debug_enabled() {
            eprintln!("[play-debug engine] play() queuing StartTransport");
        }
        self.send_command(EngineCommand::StartTransport)
    }

    pub fn pause(&self) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::StopTransport)
    }

    pub fn seek(&self, position_seconds: f64) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::Seek { position_seconds })
    }

    pub fn set_metronome_suspended(&self, suspended: bool) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::SetMetronomeSuspended(suspended))
    }

    pub fn set_metronome_enabled(&self, enabled: bool) -> Result<(), SphereAudioError> {
        self.shared
            .metronome_enabled
            .store(enabled, Ordering::Relaxed);
        self.send_command(EngineCommand::SetMetronomeEnabled(enabled))
    }

    pub fn set_bpm(&self, bpm: f64) -> Result<(), SphereAudioError> {
        self.set_tempo_map(bpm, Vec::new())
    }

    /// Replace the authoritative tempo map used for playback, metronome, and
    /// beat/time/sample conversion. `points` empty = static tempo at `default_bpm`.
    pub fn set_tempo_map(
        &self,
        default_bpm: f64,
        points: Vec<crate::types::EngineTempoPointSnapshot>,
    ) -> Result<(), SphereAudioError> {
        let snapshot = crate::runtime::build_tempo_map_from_points(default_bpm, &points);
        transport::store_f64_bits(&self.shared.bpm_bits, default_bpm);
        if let Some(project) = self.project.lock().as_mut() {
            project.bpm = default_bpm;
            project.tempo_points = points;
        }
        self.send_command(EngineCommand::SetTempoMap(snapshot))
    }

    pub fn set_insert_param(
        &self,
        track_id: String,
        insert_id: String,
        param_id: String,
        value: f32,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::SetInsertParam {
            track_id,
            insert_id,
            param_id,
            value,
        })
    }

    /// Stage 3b: install (or clear, with `None`) the realtime sink for
    /// `track_id` — the audio callback mixes its external plugin-host DSP output
    /// into the master. Applied between blocks; no realtime allocation.
    pub fn set_plugin_bridge_sink(
        &self,
        insert_id: String,
        sink: Option<std::sync::Arc<dyn crate::plugin_bridge::PluginBridgeSink>>,
    ) -> Result<(), SphereAudioError> {
        if let Some(sink) = sink.as_ref() {
            self.plugin_bridge_sinks
                .lock()
                .insert(insert_id.clone(), sink.clone());
        } else {
            self.plugin_bridge_sinks.lock().remove(&insert_id);
        }
        self.send_command(EngineCommand::SetPluginBridgeSink { insert_id, sink })
    }

    pub fn set_bridge_editor_active(
        &self,
        track_id: String,
        active: bool,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::SetBridgeEditorActive { track_id, active })
    }

    /// Block the *calling control thread* (never the callback) until the audio
    /// callback has drained every command sent before this call, or `timeout`
    /// elapses. Returns `true` on a confirmed ack. `false` means the barrier was
    /// not confirmed — no stream is open, the callback is stalled, or the device
    /// is paused — and the caller should fall back to its own grace handling.
    ///
    /// Used by offline export to confirm `SetPluginBridgeSink(None)` handoffs
    /// have reached the realtime graph before the export worker starts driving
    /// the shared bridge, replacing a guessed fixed sleep.
    pub fn wait_for_command_barrier(&self, timeout: std::time::Duration) -> bool {
        let ack = Arc::new(AtomicBool::new(false));
        if self
            .send_command(EngineCommand::CommandBarrier { ack: ack.clone() })
            .is_err()
        {
            return false;
        }
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if ack.load(Ordering::Acquire) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        ack.load(Ordering::Acquire)
    }

    pub fn set_time_signature(
        &self,
        numerator: u32,
        denominator: u32,
    ) -> Result<(), SphereAudioError> {
        self.shared
            .time_sig_num
            .store(numerator.max(1), Ordering::Relaxed);
        self.shared
            .time_sig_den
            .store(denominator.max(1), Ordering::Relaxed);
        self.send_command(EngineCommand::SetTimeSignature(numerator, denominator))
    }

    pub fn set_time_signature_map(
        &self,
        points: Vec<crate::time_signature_map::RuntimeTimeSignaturePointSnapshot>,
    ) -> Result<(), SphereAudioError> {
        let snapshot =
            crate::time_signature_map::RuntimeTimeSignatureMapSnapshot::from_points(points);
        if let Some(pt) = snapshot.points().first() {
            self.shared
                .time_sig_num
                .store(pt.numerator.max(1) as u32, Ordering::Relaxed);
            self.shared
                .time_sig_den
                .store(pt.denominator.max(1) as u32, Ordering::Relaxed);
        }
        self.send_command(EngineCommand::SetTimeSignatureMap(snapshot))
    }

    pub fn set_loop(
        &self,
        enabled: bool,
        start_seconds: f64,
        end_seconds: f64,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::SetLoop {
            enabled,
            start_seconds,
            end_seconds,
        })
    }

    pub fn midi_preview_note_on(
        &self,
        track_id: String,
        channel: u8,
        pitch: u8,
        velocity: u8,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::MidiPreviewNoteOn {
            track_id,
            channel,
            pitch,
            velocity,
        })
    }

    pub fn midi_preview_note_off(
        &self,
        track_id: String,
        channel: u8,
        pitch: u8,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::MidiPreviewNoteOff {
            track_id,
            channel,
            pitch,
        })
    }

    pub fn midi_preview_control_change(
        &self,
        track_id: String,
        channel: u8,
        controller: u8,
        value: u8,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::MidiPreviewControlChange {
            track_id,
            channel,
            controller,
            value,
        })
    }

    pub fn midi_preview_all_notes_off(&self, track_id: String) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::MidiPreviewAllNotesOff { track_id })
    }

    pub fn plugin_preview_note_on(
        &self,
        track_id: String,
        plugin_instance_id: String,
        channel: u8,
        pitch: u8,
        velocity: u8,
    ) -> Result<(), SphereAudioError> {
        eprintln!(
            "[midi-preview-engine] queued note_on track={track_id} instance={plugin_instance_id} pitch={pitch}"
        );
        self.send_command(EngineCommand::PluginPreviewNoteOn {
            track_id,
            plugin_instance_id,
            channel,
            pitch,
            velocity,
        })
    }

    pub fn plugin_preview_note_off(
        &self,
        track_id: String,
        plugin_instance_id: String,
        channel: u8,
        pitch: u8,
    ) -> Result<(), SphereAudioError> {
        eprintln!(
            "[midi-preview-engine] queued note_off track={track_id} instance={plugin_instance_id} pitch={pitch}"
        );
        self.send_command(EngineCommand::PluginPreviewNoteOff {
            track_id,
            plugin_instance_id,
            channel,
            pitch,
        })
    }

    pub fn plugin_preview_control_change(
        &self,
        track_id: String,
        plugin_instance_id: String,
        channel: u8,
        controller: u8,
        value: u8,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::PluginPreviewControlChange {
            track_id,
            plugin_instance_id,
            channel,
            controller,
            value,
        })
    }

    pub fn plugin_preview_all_notes_off(
        &self,
        track_id: String,
        plugin_instance_id: String,
    ) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::PluginPreviewAllNotesOff {
            track_id,
            plugin_instance_id,
        })
    }

    /// Read the current transport/clock snapshot for UI polling.
    pub fn transport_snapshot(&self) -> RuntimeTransportSnapshot {
        RuntimeTransportSnapshot::from_shared(&self.shared, &self.tempo_map())
    }

    fn tempo_map(&self) -> TempoMap {
        self.project
            .lock()
            .as_ref()
            .map(tempo_map_from_project_snapshot)
            .unwrap_or_else(|| {
                TempoMap::static_tempo(transport::f64_from_bits(
                    self.shared.bpm_bits.load(Ordering::Relaxed),
                ))
            })
    }

    // ── Test tone ──────────────────────────────────────────────────────────

    pub fn set_test_tone(&self, enabled: bool, frequency: f32) {
        self.shared
            .test_tone_enabled
            .store(enabled, Ordering::Relaxed);
        self.shared
            .test_tone_freq
            .store(f32_store(frequency), Ordering::Relaxed);
        // Also send through command queue so the oscillator resets its freq.
        let _ = self.send_command(EngineCommand::SetTestTone { enabled, frequency });
    }

    // ── Meters ────────────────────────────────────────────────────────────

    pub fn get_meters(&self) -> JsMeterSnapshot {
        // `mut` is only exercised by the ASIO input-meter override below.
        #[cfg_attr(not(all(target_os = "windows", feature = "asio")), allow(unused_mut))]
        let mut tracks: Vec<_> = self
            .runtime
            .lock()
            .meter_snapshots()
            .into_iter()
            .map(|meter| JsTrackMeterSnapshot {
                track_id: meter.track_id,
                peak_l: meter.peak_l as f64,
                peak_r: meter.peak_r as f64,
                rms_l: meter.rms_l as f64,
                rms_r: meter.rms_r as f64,
            })
            .collect();

        #[cfg(all(target_os = "windows", feature = "asio"))]
        {
            let input_peaks = self
                .active_stream
                .lock()
                .as_ref()
                .and_then(|stream| stream.as_asio_duplex())
                .map(|handle| handle.take_input_channel_peaks());
            if let (Some(input_peaks), Some(project)) = (input_peaks, self.project.lock().as_ref())
            {
                for meter in &mut tracks {
                    let Some(track) = project.tracks.iter().find(|track| {
                        track.id == meter.track_id
                            && track.track_type == "audio"
                            && (track.armed || track.input_monitor)
                    }) else {
                        continue;
                    };
                    if let Some((peak_l, peak_r)) =
                        routed_input_peaks(&input_peaks, &track.input_source.channels)
                    {
                        meter.peak_l = peak_l as f64;
                        meter.peak_r = peak_r as f64;
                        meter.rms_l = 0.0;
                        meter.rms_r = 0.0;
                    }
                }
            }
        }
        let plugin_outputs = self
            .runtime
            .lock()
            .plugin_output_meter_snapshots()
            .into_iter()
            .map(|meter| JsPluginOutputMeterSnapshot {
                track_id: meter.track_id,
                insert_id: meter.insert_id,
                channel: meter.channel as u32,
                peak: meter.peak as f64,
            })
            .collect();

        JsMeterSnapshot {
            tracks,
            plugin_outputs,
            master_peak_l: f32_load(self.shared.peak_l.load(Ordering::Relaxed)) as f64,
            master_peak_r: f32_load(self.shared.peak_r.load(Ordering::Relaxed)) as f64,
            master_rms_l: f32_load(self.shared.rms_l.load(Ordering::Relaxed)) as f64,
            master_rms_r: f32_load(self.shared.rms_r.load(Ordering::Relaxed)) as f64,
            input_peak_l: f32_load(self.shared.live_input_peak_l.swap(0, Ordering::Relaxed)) as f64,
            input_peak_r: f32_load(self.shared.live_input_peak_r.swap(0, Ordering::Relaxed)) as f64,
        }
    }

    /// Full-pipeline diagnostics snapshot (Layer 10). Non-destructive — reads
    /// peaks without resetting them so it can be polled alongside `get_meters`.
    pub fn get_audio_diagnostics(&self) -> crate::types::JsAudioDiagnostics {
        use crate::types::{JsAudioDiagnostics, JsTrackInputDiagnostics};
        let st = self.status.lock().clone();
        let input_device_name = self
            .live_input
            .lock()
            .as_ref()
            .and_then(|h| h.device_id.clone());
        let input_running = self.live_input.lock().is_some() && self.shared.input_ring.is_active();

        let raw_input_peak = f32_load(self.shared.live_input_peak_l.load(Ordering::Relaxed)).max(
            f32_load(self.shared.live_input_peak_r.load(Ordering::Relaxed)),
        );
        let input_bus_peak = f32_load(self.shared.input_bus_peak_l.load(Ordering::Relaxed)).max(
            f32_load(self.shared.input_bus_peak_r.load(Ordering::Relaxed)),
        );
        let output_peak = f32_load(self.shared.peak_l.load(Ordering::Relaxed))
            .max(f32_load(self.shared.peak_r.load(Ordering::Relaxed)));

        let runtime = self.runtime.lock();
        let tracks = runtime
            .tracks
            .iter()
            .filter(|t| t.track_type == "audio")
            .map(|t| {
                let meter = t.meter.load(&t.id);
                JsTrackInputDiagnostics {
                    track_id: t.id.clone(),
                    record_armed: t.record_armed,
                    monitor_enabled: t.monitor_enabled,
                    input_source: format!("{:?}", t.input_source),
                    track_input_peak: meter.peak_l.max(meter.peak_r) as f64,
                    track_output_peak: meter.peak_l.max(meter.peak_r) as f64,
                }
            })
            .collect();

        JsAudioDiagnostics {
            backend: self.daux_config.lock().backend.display_name().to_string(),
            input_device_name,
            output_device_name: st.output_device,
            input_stream_running: input_running,
            output_stream_running: st.running,
            input_sample_rate: self.shared.input_ring.sample_rate(),
            input_channels: self.shared.input_ring.channels(),
            raw_input_peak: raw_input_peak as f64,
            input_bus_peak: input_bus_peak as f64,
            output_peak: output_peak as f64,
            tracks,
            input_callback_count: self.shared.input_cb_count.load(Ordering::Relaxed) as f64,
            output_callback_count: self.shared.output_cb_count.load(Ordering::Relaxed) as f64,
            input_frames_received: self.shared.input_frames_received.load(Ordering::Relaxed) as f64,
            monitor_frames_consumed: self.shared.monitor_frames_consumed.load(Ordering::Relaxed)
                as f64,
            monitor_ring_underruns: self.shared.monitor_ring_underruns.load(Ordering::Relaxed)
                as f64,
            monitor_ring_overruns: self.shared.monitor_ring_overruns.load(Ordering::Relaxed) as f64,
            record_ring_overruns: self.shared.record_ring_overruns.load(Ordering::Relaxed) as f64,
            output_xruns: self.shared.output_xruns.load(Ordering::Relaxed) as f64,
            monitor_output_peak: f32_load(self.shared.monitor_output_peak.load(Ordering::Relaxed))
                as f64,
            record_peak: f32_load(self.shared.record_peak.load(Ordering::Relaxed)) as f64,
        }
    }

    /// Metadata + current bin count for the in-progress recording preview.
    pub fn recording_preview_info(&self) -> crate::types::JsRecordingPreviewInfo {
        crate::types::JsRecordingPreviewInfo {
            active: self.shared.recording_preview_active.load(Ordering::Relaxed),
            recording_id: self.shared.recording_preview_id.load(Ordering::Relaxed) as f64,
            start_sample: self
                .shared
                .recording_preview_start_sample
                .load(Ordering::Relaxed) as f64,
            sample_rate: self
                .shared
                .recording_preview_sample_rate
                .load(Ordering::Relaxed),
            channels: self
                .shared
                .recording_preview_channels
                .load(Ordering::Relaxed),
            peaks_per_second: self
                .shared
                .recording_preview_peaks_per_sec
                .load(Ordering::Relaxed),
            peak_count: self.shared.preview_ring.head() as f64,
        }
    }

    /// Drain preview bins in `[from_index, head)`. Cheap clone on the control
    /// thread; never blocks the audio path. Returns an empty vec when there is
    /// nothing new.
    pub fn drain_recording_preview_peaks(
        &self,
        from_index: f64,
    ) -> Vec<crate::types::JsWaveformPeak> {
        let head = self.shared.preview_ring.head();
        let mut from = from_index.max(0.0) as u64;
        // If the consumer lagged past the ring window, clamp to what's retained.
        let cap = crate::input_ring::PreviewPeakRing::default_capacity();
        if head.saturating_sub(from) > cap {
            from = head.saturating_sub(cap);
        }
        let mut out = Vec::with_capacity((head.saturating_sub(from)) as usize);
        let mut i = from;
        while i < head {
            let p = self.shared.preview_ring.read(i);
            out.push(crate::types::JsWaveformPeak {
                min: p.min as f64,
                max: p.max as f64,
                rms: p.rms as f64,
            });
            i += 1;
        }
        out
    }

    /// Throttled (≈500 ms) raw-input-peak trace, gated by
    /// `FUTUREBOARD_INPUT_DEBUG`. Called from the control thread (never the
    /// audio callback) so the realtime path stays allocation/IO-free.
    pub fn log_input_debug_throttled(&self) {
        if !input_debug_enabled() {
            return;
        }
        static LAST: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
        let last = LAST.get_or_init(|| {
            Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1))
        });
        {
            let mut guard = last.lock();
            if guard.elapsed() < std::time::Duration::from_millis(500) {
                return;
            }
            *guard = std::time::Instant::now();
        }
        let s = &self.shared;
        let raw = f32_load(s.live_input_peak_l.load(Ordering::Relaxed))
            .max(f32_load(s.live_input_peak_r.load(Ordering::Relaxed)));
        let bus = f32_load(s.input_bus_peak_l.load(Ordering::Relaxed))
            .max(f32_load(s.input_bus_peak_r.load(Ordering::Relaxed)));
        let head = s.preview_ring.head();
        eprintln!(
            "[AudioRealtime] input_cb={} output_cb={} input_frames={} monitor_consumed={} \
             monitor_underruns={} monitor_overruns={} record_overruns={} output_xruns={} \
             raw_input_peak={raw:.4} monitor_output_peak={:.4} record_peak={:.4} input_stream={} monitor_any={}",
            s.input_cb_count.load(Ordering::Relaxed),
            s.output_cb_count.load(Ordering::Relaxed),
            s.input_frames_received.load(Ordering::Relaxed),
            s.monitor_frames_consumed.load(Ordering::Relaxed),
            s.monitor_ring_underruns.load(Ordering::Relaxed),
            s.monitor_ring_overruns.load(Ordering::Relaxed),
            s.record_ring_overruns.load(Ordering::Relaxed),
            s.output_xruns.load(Ordering::Relaxed),
            f32_load(s.monitor_output_peak.load(Ordering::Relaxed)),
            f32_load(s.record_peak.load(Ordering::Relaxed)),
            s.input_ring.is_active(),
            s.monitor_enabled_any.load(Ordering::Relaxed),
        );
        let _ = bus;
        if s.recording_preview_active.load(Ordering::Relaxed) {
            let sr = s
                .recording_preview_sample_rate
                .load(Ordering::Relaxed)
                .max(1);
            let pps = s
                .recording_preview_peaks_per_sec
                .load(Ordering::Relaxed)
                .max(1);
            eprintln!(
                "[RecordingPreview] recording_id={} peaks={} preview_duration_sec={:.2} sr={sr} pps={pps}",
                s.recording_preview_id.load(Ordering::Relaxed),
                head,
                head as f64 / pps as f64,
            );
        }
    }

    // ── Param updates ──────────────────────────────────────────────────────

    pub fn set_master_volume(&self, value: f32) -> Result<(), SphereAudioError> {
        self.shared
            .master_volume
            .store(f32_store(value.clamp(0.0, 2.0)), Ordering::Relaxed);
        Ok(())
    }

    pub fn update_track_input_state(
        &self,
        track_id: &str,
        record_armed: bool,
        monitor_enabled: bool,
        input_source: EngineTrackInputSourceSnapshot,
    ) -> Result<(), SphereAudioError> {
        self.update_track_input_state_inner(
            track_id,
            record_armed,
            monitor_enabled,
            Some(input_source),
        )
    }

    pub fn update_track_input_flags(
        &self,
        track_id: &str,
        record_armed: bool,
        monitor_enabled: bool,
    ) -> Result<(), SphereAudioError> {
        self.update_track_input_state_inner(track_id, record_armed, monitor_enabled, None)
    }

    fn update_track_input_state_inner(
        &self,
        track_id: &str,
        record_armed: bool,
        monitor_enabled: bool,
        explicit_input_source: Option<EngineTrackInputSourceSnapshot>,
    ) -> Result<(), SphereAudioError> {
        // Serialize with start/stop recording. A non-ASIO take owns the only
        // capture stream; route changes must never race it and open a second
        // client. ASIO uses the same fail-closed policy for one predictable UI
        // contract while a take is active.
        let recording_guard = self.recording.lock();
        if recording_guard.is_some() {
            return Err(SphereAudioError::InvalidConfig(
                "track input routing cannot change during an active recording".to_string(),
            ));
        }

        let mut project = self.project.lock();
        let snapshot = project
            .as_mut()
            .ok_or_else(|| SphereAudioError::InvalidConfig("no project is loaded".to_string()))?;
        let track_index = snapshot
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or_else(|| {
                SphereAudioError::InvalidConfig(format!("track '{track_id}' was not found"))
            })?;
        let input_source = explicit_input_source
            .unwrap_or_else(|| snapshot.tracks[track_index].input_source.clone());
        if input_source.channels.len() > 2 {
            return Err(SphereAudioError::InvalidConfig(format!(
                "track '{track_id}' input route has {} channels; only mono or stereo routes are supported",
                input_source.channels.len()
            )));
        }
        if matches!(input_source.channels.as_slice(), [left, right] if left == right) {
            return Err(SphereAudioError::InvalidConfig(format!(
                "track '{track_id}' stereo input route uses channel {} twice",
                input_source.channels[0].saturating_add(1)
            )));
        }
        #[cfg(all(target_os = "windows", feature = "asio"))]
        if let Some(caps) = self.asio_caps.lock().clone() {
            self.validate_asio_input_source(track_id, &input_source, &caps)?;
        }

        let old_armed = snapshot.tracks[track_index].armed;
        let old_monitor = snapshot.tracks[track_index].input_monitor;
        let old_source = snapshot.tracks[track_index].input_source.clone();
        snapshot.tracks[track_index].armed = record_armed;
        snapshot.tracks[track_index].input_monitor = monitor_enabled;
        snapshot.tracks[track_index].input_source = input_source.clone();

        if let Err(error) = self.sync_live_input_stream(snapshot) {
            snapshot.tracks[track_index].armed = old_armed;
            snapshot.tracks[track_index].input_monitor = old_monitor;
            snapshot.tracks[track_index].input_source = old_source;
            let _ = self.sync_live_input_stream(snapshot);
            return Err(error);
        }

        let next_runtime_source = RuntimeTrackInputSource::from_channels(&input_source.channels);
        self.runtime.lock().update_track_input_state(
            track_index,
            record_armed,
            monitor_enabled,
            next_runtime_source,
        );
        let command = EngineCommand::SetTrackInputState {
            track_index,
            record_armed,
            monitor_enabled,
            input_source: next_runtime_source,
        };
        let result = match self.send_command(command) {
            Ok(()) | Err(SphereAudioError::EngineNotOpen) => {
                self.input_state_revision.fetch_add(1, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                snapshot.tracks[track_index].armed = old_armed;
                snapshot.tracks[track_index].input_monitor = old_monitor;
                snapshot.tracks[track_index].input_source = old_source.clone();
                self.runtime.lock().update_track_input_state(
                    track_index,
                    old_armed,
                    old_monitor,
                    RuntimeTrackInputSource::from_channels(&old_source.channels),
                );
                let _ = self.sync_live_input_stream(snapshot);
                Err(error)
            }
        };
        drop(project);
        drop(recording_guard);
        result
    }

    pub fn update_track_param(
        &self,
        track_id: &str,
        param_id: &str,
        value: f64,
    ) -> Result<(), SphereAudioError> {
        if track_id == "__master__" && param_id == "volume" {
            return self.set_master_volume(value as f32);
        }

        match param_id {
            "volume" => self.send_command(EngineCommand::SetTrackVolume {
                track_id: track_id.into(),
                value: value as f32,
            }),
            "pan" => self.send_command(EngineCommand::SetTrackPan {
                track_id: track_id.into(),
                value: value as f32,
            }),
            "muted" => self.send_command(EngineCommand::SetTrackMute {
                track_id: track_id.into(),
                muted: value != 0.0,
            }),
            "solo" => self.send_command(EngineCommand::SetTrackSolo {
                track_id: track_id.into(),
                solo: value != 0.0,
            }),

            "previewMode" => self.send_command(EngineCommand::SetTrackPreviewMode {
                track_id: track_id.into(),
                value: value as f32,
            }),
            other => {
                // Unknown param — log but don't error (UI might send future params)
                eprintln!("[SphereAudio] Unknown track param: '{other}' (track={track_id})");
                Ok(())
            }
        }
    }

    pub fn update_insert_param(
        &self,
        track_id: &str,
        insert_id: &str,
        param_id: &str,
        value: f64,
    ) -> Result<(), SphereAudioError> {
        eprintln!(
            "[SphereAudio] queue insert param track={} insert={} param={} value={:.6}",
            track_id, insert_id, param_id, value
        );
        self.send_command(EngineCommand::SetInsertParam {
            track_id: track_id.into(),
            insert_id: insert_id.into(),
            param_id: param_id.into(),
            value: value as f32,
        })
    }

    pub fn open_insert_editor(
        &self,
        track_id: &str,
        insert_id: &str,
        window_id: &str,
        title: &str,
        width: i32,
        height: i32,
    ) -> Result<u64, SphereAudioError> {
        let mut runtime = self.runtime.lock();
        let Some(track) = runtime.tracks.iter_mut().find(|track| track.id == track_id) else {
            return Err(SphereAudioError::NativeError(format!(
                "track not found for insert editor: {track_id}"
            )));
        };
        let Some(insert) = track
            .inserts
            .iter_mut()
            .find(|insert| insert.id == insert_id)
        else {
            return Err(SphereAudioError::NativeError(format!(
                "insert not found for editor: track={track_id} insert={insert_id}"
            )));
        };
        let Some(vst3) = insert.vst3.as_mut() else {
            return Err(SphereAudioError::NativeError(format!(
                "insert has no ready VST3 processor: track={track_id} insert={insert_id}"
            )));
        };
        let handle = vst3
            .open_editor(window_id, title, width, height)
            .ok_or_else(|| {
                SphereAudioError::NativeError(format!(
                    "failed to open VST3 editor: track={track_id} insert={insert_id}"
                ))
            })?;
        eprintln!(
            "[SphereAudio] opened insert editor track={} insert={} handle={} processorHandle=0x{:x}",
            track_id,
            insert_id,
            handle,
            vst3.handle_value()
        );
        Ok(handle)
    }

    pub fn close_insert_editor(
        &self,
        track_id: &str,
        insert_id: &str,
    ) -> Result<(), SphereAudioError> {
        let mut runtime = self.runtime.lock();
        if let Some(vst3) = runtime
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .and_then(|track| {
                track
                    .inserts
                    .iter_mut()
                    .find(|insert| insert.id == insert_id)
            })
            .and_then(|insert| insert.vst3.as_mut())
        {
            vst3.close_editor();
            eprintln!(
                "[SphereAudio] closed insert editor track={} insert={}",
                track_id, insert_id
            );
        }
        Ok(())
    }

    /// Clone the live runtime VST3 processor handle for an insert, if it has a
    /// ready native plugin instance. The returned handle is `Arc`-backed and
    /// cheap to clone; holding it keeps the C++ instance alive even across a
    /// runtime rebuild or insert removal (the GUI editor relies on this so it
    /// can attach to / refresh the *existing* instance without re-locking the
    /// engine every frame). Used by the GPUI PluginView for the embedded editor.
    pub fn insert_processor(
        &self,
        track_id: &str,
        insert_id: &str,
    ) -> Option<crate::vst3_processor::Vst3RuntimeProcessor> {
        let mut runtime = self.runtime.lock();
        runtime
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .and_then(|track| {
                track
                    .inserts
                    .iter_mut()
                    .find(|insert| insert.id == insert_id)
            })
            .and_then(|insert| insert.vst3.as_ref().cloned())
    }

    pub fn focus_insert_editor(
        &self,
        track_id: &str,
        insert_id: &str,
    ) -> Result<bool, SphereAudioError> {
        let mut runtime = self.runtime.lock();
        if let Some(vst3) = runtime
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .and_then(|track| {
                track
                    .inserts
                    .iter_mut()
                    .find(|insert| insert.id == insert_id)
            })
            .and_then(|insert| insert.vst3.as_mut())
        {
            return Ok(vst3.focus_editor());
        }
        Ok(false)
    }

    // ── Project snapshot ───────────────────────────────────────────────────

    pub fn load_project(
        &self,
        mut snapshot: EngineProjectSnapshot,
    ) -> Result<(), SphereAudioError> {
        let input_state_revision_at_start = self.input_state_revision.load(Ordering::Acquire);
        let old_state = AudioEngineState::from_u8(
            self.shared
                .engine_state
                .swap(AudioEngineState::LoadingProject as u8, Ordering::Relaxed),
        );
        eprintln!(
            "[AudioEngineState] old={old_state:?} new=LoadingProject source=load_project playing={}",
            self.shared.playing.load(Ordering::Relaxed)
        );
        let output_sample_rate = self.shared.sample_rate.load(Ordering::Relaxed).max(1);
        let previously_active_paths: Vec<String> = self
            .project
            .lock()
            .as_ref()
            .into_iter()
            .flat_map(|project| project.clips.iter())
            .filter_map(|clip| clip.media_path.as_deref())
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();

        // Log how many clips have paths before building runtime.
        let clips_with_path = snapshot
            .clips
            .iter()
            .filter(|c| {
                c.media_path
                    .as_deref()
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
            })
            .count();
        eprintln!(
            "[SphereAudio] load_project: id='{}' tracks={} snapshot_clips={} clips_with_path={}",
            snapshot.project_id,
            snapshot.tracks.len(),
            snapshot.clips.len(),
            clips_with_path,
        );

        if clips_with_path == 0 && !snapshot.clips.is_empty() {
            eprintln!(
                "[SphereAudio] ⚠ WARNING: all {} clips have null/empty mediaPath — no audio will play!",
                snapshot.clips.len()
            );
        }

        // Extract existing VST3 processors from the current runtime so they can
        // be reused in the new build if the same insert ID + plugin path + class_id
        // + sample_rate still match.  This prevents spurious processor destruction
        // (and editor HWND invalidation) when just reloading the project.
        let mut existing_vst3: HashMap<String, crate::vst3_processor::Vst3RuntimeProcessor> = {
            let mut current = self.runtime.lock();
            let mut map = HashMap::new();
            for track in &mut current.tracks {
                for insert in &mut track.inserts {
                    if let Some(vst3) = insert.vst3.take() {
                        vst3.set_destroy_reason("replaced-by-load-project");
                        map.insert(insert.id.clone(), vst3);
                    }
                }
            }
            map
        };

        let mut runtime = {
            let mut audio_cache = self.audio_cache.lock();
            match RuntimeProject::build(
                &snapshot,
                output_sample_rate,
                &mut audio_cache,
                Some(&mut existing_vst3),
                self.pdc_enabled(),
            ) {
                Ok(project) => {
                    // Keep recently removed sources warm for delete/undo. Opening
                    // a large mapped or streaming source again can take long
                    // enough to make undo appear stuck. The inactive LRU remains
                    // bounded, while every source still used by the project is
                    // retained independently of that limit.
                    let active_paths: HashSet<String> = snapshot
                        .clips
                        .iter()
                        .filter_map(|c| c.media_path.as_deref())
                        .filter(|p| !p.is_empty())
                        .map(str::to_owned)
                        .collect();
                    let mut inactive_lru = self.inactive_audio_cache_lru.lock();
                    inactive_lru.retain(|path| !active_paths.contains(path));
                    for path in &previously_active_paths {
                        if !active_paths.contains(path)
                            && audio_cache.contains_key(path)
                            && !inactive_lru.contains(path)
                        {
                            inactive_lru.push_back(path.clone());
                        }
                    }
                    while inactive_lru.len() > MAX_INACTIVE_AUDIO_CACHE_ENTRIES {
                        if let Some(path) = inactive_lru.pop_front() {
                            if !active_paths.contains(&path) {
                                audio_cache.remove(&path);
                            }
                        }
                    }
                    project
                }
                Err(e) => {
                    let mut current = self.runtime.lock();
                    for track in &mut current.tracks {
                        for insert in &mut track.inserts {
                            if insert.vst3.is_none() {
                                if let Some(vst3) = existing_vst3.remove(&insert.id) {
                                    insert.vst3 = Some(vst3);
                                }
                            }
                        }
                    }
                    // Failed build must not leave the engine wedged in
                    // LoadingProject (permanent silence) — restore.
                    self.shared
                        .engine_state
                        .store(old_state as u8, Ordering::Relaxed);
                    eprintln!(
                        "[AudioEngineState] old=LoadingProject new={old_state:?} source=load_project_failed"
                    );
                    return Err(e.into());
                }
            }
        };
        // Processors left in existing_vst3 had no matching insert in the new
        // snapshot — they are dropped here with reason="replaced-by-load-project".
        drop(existing_vst3);

        eprintln!(
            "[SphereAudio] RuntimeProject built: {} runtime clips from {} snapshot clips (sr={}) graph_nodes={} pass2={} rejected_routes={}",
            runtime.clips.len(),
            snapshot.clips.len(),
            output_sample_rate,
            runtime.audio_graph.nodes.len(),
            runtime.audio_graph.pass2_routing_indices.len(),
            runtime.audio_graph.rejected_routes.len(),
        );

        // Build initial track states from snapshot.
        let mut tracks = self.tracks.lock();
        tracks.clear();
        for t in &snapshot.tracks {
            let mut ts = TrackState::new(&t.id);
            ts.volume = t.volume.clamp(0.0, 2.0);
            ts.pan = t.pan.clamp(-1.0, 1.0);
            ts.muted = t.muted;
            ts.solo = t.solo;
            tracks.push(ts);
        }
        drop(tracks);

        for t in &snapshot.tracks {
            let track_clips = runtime.clips.iter().filter(|c| c.track_id == t.id).count();
            eprintln!(
                "[SphereAudio] track '{}' type={} clips={} volume={:.2} pan={:.2} muted={} solo={}",
                t.id, t.track_type, track_clips, t.volume, t.pan, t.muted, t.solo
            );
        }
        // Initialise the graveyard drop-thread on this (control) thread so the
        // callback's first graph swap is a cheap atomic enqueue, never a
        // channel allocation + thread spawn on the audio thread.
        crate::graveyard::prime();

        // Serialize the project store and callback command with incremental
        // input edits. If one committed while this graph was being built, carry
        // its newer arm/monitor/route fields into both the snapshot and runtime
        // before `LoadProject` is enqueued.
        let mut current_project = self.project.lock();
        if self.input_state_revision.load(Ordering::Acquire) != input_state_revision_at_start {
            if let Some(current) = current_project
                .as_ref()
                .filter(|current| current.project_id == snapshot.project_id)
            {
                for (track_index, next_track) in snapshot.tracks.iter_mut().enumerate() {
                    let Some(current_track) = current
                        .tracks
                        .iter()
                        .find(|track| track.id == next_track.id)
                    else {
                        continue;
                    };
                    next_track.armed = current_track.armed;
                    next_track.input_monitor = current_track.input_monitor;
                    next_track.input_source = current_track.input_source.clone();
                    runtime.update_track_input_state(
                        track_index,
                        current_track.armed,
                        current_track.input_monitor,
                        RuntimeTrackInputSource::from_channels(
                            &current_track.input_source.channels,
                        ),
                    );
                }
            }
        }
        if let Err(error) = self.sync_live_input_stream(&snapshot) {
            let message = format!("Live input unavailable: {error}");
            eprintln!("[SphereAudio] {message}");
            self.status.lock().last_error = Some(message);
        }
        *self.runtime.lock() = runtime.clone();
        *current_project = Some(snapshot.clone());
        let load_result = self.send_command(EngineCommand::LoadProject(Box::new(runtime)));
        drop(current_project);

        match load_result {
            Ok(()) => eprintln!("[SphereAudio] LoadProject command sent to audio callback"),
            Err(SphereAudioError::EngineNotOpen) => {
                eprintln!(
                    "[SphereAudio] ⚠ WARNING: no audio stream open — runtime stored, \
                     will apply on next openDevice/openDaux"
                );
                // No callback will run the graph-swap transition; leave a
                // consistent Paused state instead of LoadingProject.
                self.shared
                    .engine_state
                    .store(AudioEngineState::Paused as u8, Ordering::Relaxed);
                eprintln!(
                    "[AudioEngineState] old=LoadingProject new=Paused source=load_project_no_stream"
                );
            }
            Err(e) => {
                self.shared
                    .engine_state
                    .store(old_state as u8, Ordering::Relaxed);
                eprintln!(
                    "[AudioEngineState] old=LoadingProject new={old_state:?} source=load_project_send_failed"
                );
                return Err(e);
            }
        }

        let _ = self.set_tempo_map(snapshot.bpm, snapshot.tempo_points.clone());
        let _ = self.set_time_signature(snapshot.time_signature[0], snapshot.time_signature[1]);

        Ok(())
    }

    // ── Debug info ─────────────────────────────────────────────────────────

    pub fn get_debug_info(&self) -> JsEngineDebugInfo {
        let runtime = self.runtime.lock();
        let project = self.project.lock();
        let sample_rate = self.shared.sample_rate.load(Ordering::Relaxed).max(1);

        let ready_clips = runtime
            .clips
            .iter()
            .filter(|c| c.source.frames() > 0)
            .count() as u32;
        let clip_summaries: Vec<String> = runtime
            .clips
            .iter()
            .map(|c| {
                format!(
                    "id={} track={} start={:.3}s dur={:.3}s frames={} gain={:.2}",
                    c.id,
                    c.track_id,
                    c.start_sample as f64 / sample_rate as f64,
                    c.duration_samples as f64 / sample_rate as f64,
                    c.source.frames(),
                    c.gain,
                )
            })
            .collect();
        let insert_summaries: Vec<String> = runtime
            .tracks
            .iter()
            .flat_map(|track| {
                track.inserts.iter().map(move |insert| {
                    let format = insert
                        .params
                        .get("format")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let path = insert
                        .params
                        .get("path")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let vst3_ready = insert
                        .vst3
                        .as_ref()
                        .map(|processor| processor.is_ready())
                        .unwrap_or(false);
                    let (process_count, input_peak, output_peak, diff_peak) = insert
                        .vst3
                        .as_ref()
                        .map(|processor| {
                            (
                                processor.process_count(),
                                processor.last_input_peak(),
                                processor.last_output_peak(),
                                processor.last_difference_peak(),
                            )
                        })
                        .unwrap_or((0, 0.0, 0.0, 0.0));
                    format!(
                        concat!(
                            "track={} insert={} kind={} format={} enabled={}",
                            "vst3Ready={} processCount={}",
                            "inPeak={:.4} outPeak={:.4} diffPeak={:.6} path={}"
                        ),
                        track.id,
                        insert.id,
                        insert.kind,
                        format,
                        insert.enabled,
                        vst3_ready,
                        process_count,
                        input_peak,
                        output_peak,
                        diff_peak,
                        path,
                    )
                })
            })
            .collect();

        let graph_rejected_route_summaries: Vec<String> = runtime
            .audio_graph
            .rejected_routes
            .iter()
            .map(|route| {
                format!(
                    "{} -> {} ({:?}): {}",
                    route.from_track_id, route.to_track_id, route.kind, route.reason
                )
            })
            .collect();

        let transport = self.transport_snapshot();
        let disk = crate::streaming_source::diagnostics();

        JsEngineDebugInfo {
            project_id: project.as_ref().map(|p| p.project_id.clone()),
            loaded_tracks: runtime.tracks.len() as u32,
            loaded_clips: runtime.clips.len() as u32,
            ready_clips,
            is_playing: transport.playing,
            position_seconds: transport.position_seconds,
            position_beats: transport.position_beats,
            loop_enabled: transport.loop_enabled,
            has_solo: runtime.has_solo,
            clip_summaries,
            insert_summaries,
            disk_underruns: disk.underruns as f64,
            disk_stream_active_sources: disk.active_sources as f64,
            disk_stream_cache_reads: disk.cache_reads as f64,
            disk_stream_cache_hits: disk.cache_hits as f64,
            disk_stream_cache_misses: disk.cache_misses as f64,
            disk_stream_cache_memory_used_mb: disk.cache_memory_used_bytes as f64
                / (1024.0 * 1024.0),
            disk_stream_cache_memory_budget_mb: disk.cache_memory_budget_bytes as f64
                / (1024.0 * 1024.0),
            disk_stream_blocks_decoded: disk.blocks_decoded as f64,
            disk_stream_frames_decoded: disk.frames_decoded as f64,
            graph_node_count: runtime.audio_graph.nodes.len() as u32,
            graph_pass1_count: runtime.audio_graph.pass1_source_indices.len() as u32,
            graph_pass2_count: runtime.audio_graph.pass2_routing_indices.len() as u32,
            graph_rejected_route_count: runtime.audio_graph.rejected_routes.len() as u32,
            graph_rejected_route_summaries,
        }
    }

    /// Structured per-insert instantiation status for UI readback (Phase 2b).
    ///
    /// Built-in DSP inserts are always `ready`. Native-plugin inserts are
    /// `ready` only when their `Vst3RuntimeProcessor` instantiated and is live;
    /// a native insert with `ready == false` is a definitive instantiation
    /// failure (the worker attempted it during `load_project` and got `None`).
    /// Used by the native shell to flip `InsertLoadStatus::Failed`.
    pub fn insert_statuses(&self) -> Vec<crate::native::EngineInsertStatus> {
        let runtime = self.runtime.lock();
        runtime
            .tracks
            .iter()
            .flat_map(|track| {
                let track_id = track.id.clone();
                track.inserts.iter().map(move |insert| {
                    let native = insert.kind.eq_ignore_ascii_case("native-plugin");
                    let ready = if native {
                        insert.vst3.as_ref().map(|p| p.is_ready()).unwrap_or(false)
                    } else {
                        true
                    };
                    crate::native::EngineInsertStatus {
                        track_id: track_id.clone(),
                        insert_id: insert.id.clone(),
                        native,
                        ready,
                    }
                })
            })
            .collect()
    }

    // ── Recording API ──────────────────────────────────────────────────────

    fn find_input_device_for_active_backend(
        &self,
        device_id: Option<&str>,
    ) -> Result<cpal::Device, SphereAudioError> {
        let backend = self.daux_config.lock().backend.clone();
        match backend {
            // An ASIO driver is one duplex session. Opening a second input
            // stream would re-prepare the shared buffer set and kill the
            // output side — capture always taps the session input callback.
            BackendKind::Asio => Err(SphereAudioError::NativeError(
                "ASIO input is captured by the active duplex session; \
                 no separate input device is ever opened"
                    .into(),
            )),
            _ => recording::find_input_device(device_id),
        }
    }

    /// Live capabilities of the open ASIO session (None for other backends or
    /// while no stream is open). Device enumeration overlays these onto the
    /// registry-derived driver list so only the *active* driver reports real
    /// channel counts — installed-but-idle drivers are never instantiated just
    /// to fill a dropdown.
    pub fn asio_session_caps(&self) -> Option<crate::backend::AsioSessionCaps> {
        self.asio_caps.lock().clone()
    }

    /// Begin writing armed tracks to disk. WASAPI-family backends open a
    /// dedicated capture stream; the ASIO backend installs a record sink into
    /// the already-running duplex session (the driver/stream is never touched).
    pub fn start_recording(&self, config: JsStartRecordingConfig) -> Result<(), SphereAudioError> {
        let mut guard = self.recording.lock();
        if guard.is_some() {
            return Err(SphereAudioError::NativeError(
                "A recording session is already active".to_string(),
            ));
        }
        let monitor_mix = config.monitor_mix;

        #[cfg(all(target_os = "windows", feature = "asio"))]
        if self.daux_config.lock().backend == BackendKind::Asio {
            return self.start_recording_asio(config, monitor_mix, &mut guard);
        }

        // Single-capture-stream invariant: the recording stream becomes the sole
        // input device client during a take (it feeds the monitor ring, preview
        // ring, and file writer). Stop the standalone monitor stream so two
        // WASAPI shared clients don't contend on the same endpoint — a likely
        // source of jitter while recording.
        self.stop_live_input_stream();
        let session = self
            .find_input_device_for_active_backend(config.input_device_id.as_deref())
            .and_then(|device| {
                recording::start_recording_with_device(
                    config,
                    Arc::clone(&self.shared),
                    monitor_mix,
                    device,
                )
            });
        match session {
            Ok(session) => {
                *guard = Some(session);
                drop(guard);
                // Ensure the output stream runs so monitoring is audible.
                self.warm_output_for_monitoring();
                Ok(())
            }
            Err(e) => {
                drop(guard);
                // Restore standalone monitoring from the last snapshot on failure.
                if let Some(snapshot) = self.project.lock().clone() {
                    let _ = self.sync_live_input_stream(&snapshot);
                }
                Err(e)
            }
        }
    }

    /// ASIO take: build the writer pipeline, then hand the record sink to the
    /// session's input callback over its bounded command queue. Monitoring,
    /// the stream, and the driver are untouched — recording never depends on
    /// monitoring being enabled, and starting a take never recreates streams.
    #[cfg(all(target_os = "windows", feature = "asio"))]
    fn start_recording_asio(
        &self,
        config: JsStartRecordingConfig,
        monitor_mix: bool,
        guard: &mut Option<RecordingSession>,
    ) -> Result<(), SphereAudioError> {
        use crate::backend::asio_session::AsioInputCommand;

        let caps = self.asio_caps.lock().clone().ok_or_else(|| {
            SphereAudioError::NativeError(
                "ASIO recording requires an open ASIO stream — apply the audio settings first"
                    .into(),
            )
        })?;
        if caps.input_channels == 0 {
            let detail = self
                .status
                .lock()
                .last_daux_error
                .clone()
                .unwrap_or_else(|| {
                    format!("ASIO driver '{}' has no usable input channels", caps.driver)
                });
            return Err(SphereAudioError::NativeError(detail));
        }

        let (session, sink) = recording::start_recording_asio_tap(
            config,
            Arc::clone(&self.shared),
            monitor_mix,
            caps.input_channels,
            caps.sample_rate,
            caps.buffer_size,
        )?;

        let install_result = {
            let stream_guard = self.active_stream.lock();
            match stream_guard.as_ref().and_then(|s| s.as_asio_duplex()) {
                None => Err(SphereAudioError::NativeError(
                    "ASIO session is not open".into(),
                )),
                Some(handle) => {
                    // Dispose sinks from a previous take before installing.
                    handle.drain_trashed_sinks();
                    handle
                        .input_cmd_tx
                        .try_send(AsioInputCommand::SetRecordSink(Box::new(sink)))
                        .map_err(|_| {
                            SphereAudioError::NativeError(
                                "ASIO input command queue is full; cannot start recording".into(),
                            )
                        })
                }
            }
        };

        if let Err(error) = install_result {
            // Unwind the already-armed writer pipeline (finalizes and reports;
            // results are discarded because the take never started).
            let _ = recording::stop_recording(session);
            return Err(error);
        }

        *guard = Some(session);
        self.warm_output_for_monitoring();
        Ok(())
    }

    /// Stop the active recording, finalize WAV files, and return per-track results.
    pub fn stop_recording(&self) -> Result<Vec<JsRecordingResult>, SphereAudioError> {
        let session = self.recording.lock().take().ok_or_else(|| {
            SphereAudioError::NativeError("No active recording session".to_string())
        })?;

        // ASIO tap: detach the record sink from the session input callback so
        // the callback releases its channel senders on the next block. The
        // writer additionally exits on the session's stop flag, so a stalled
        // driver still finalizes the take.
        #[cfg(all(target_os = "windows", feature = "asio"))]
        let asio_tap = session.is_asio_tap();
        #[cfg(all(target_os = "windows", feature = "asio"))]
        if asio_tap {
            if let Some(handle) = self
                .active_stream
                .lock()
                .as_ref()
                .and_then(|s| s.as_asio_duplex())
            {
                let _ = handle
                    .input_cmd_tx
                    .try_send(crate::backend::asio_session::AsioInputCommand::ClearRecordSink);
            }
        }

        let mut results = recording::stop_recording(session)?;

        #[cfg(all(target_os = "windows", feature = "asio"))]
        if asio_tap {
            if let Some(handle) = self
                .active_stream
                .lock()
                .as_ref()
                .and_then(|s| s.as_asio_duplex())
            {
                handle.drain_trashed_sinks();
            }
        }

        let runtime = self.runtime.lock();
        let buffer_frames = self.status.lock().buffer_size;
        let sample_rate = runtime.sample_rate.max(1) as f64;
        // Round-trip compensation: driver-reported latencies when the ASIO
        // session provides them, else the 2×buffer estimate.
        let asio_round_trip = self.asio_caps.lock().as_ref().map(|caps| {
            caps.input_latency_samples
                .saturating_add(caps.output_latency_samples)
        });
        let round_trip_buffer = match asio_round_trip {
            Some(latency) if latency > 0 => latency,
            _ => buffer_frames.saturating_mul(2),
        };
        for result in &mut results {
            let Some(track_idx) = runtime
                .tracks
                .iter()
                .position(|track| track.id == result.track_id)
            else {
                continue;
            };
            let path_samples = runtime
                .latency_graph
                .max_path_latency_samples
                .saturating_sub(
                    runtime
                        .latency_graph
                        .track_pdc_delay
                        .get(track_idx)
                        .copied()
                        .unwrap_or(0),
                );
            let offset_samples = round_trip_buffer.saturating_add(path_samples);
            let offset_beats = runtime
                .tempo_map
                .beat_at_samples(offset_samples as u64, sample_rate);
            result.start_beat = (result.start_beat - offset_beats).max(0.0);
        }
        drop(runtime);

        // Restore the standalone monitoring input stream from the last snapshot
        // (re-opens it if any track is still armed/monitored).
        if let Some(snapshot) = self.project.lock().clone() {
            let _ = self.sync_live_input_stream(&snapshot);
        }
        Ok(results)
    }

    /// Snapshot of current recording state (for UI status polling).
    pub fn get_recording_status(&self) -> JsRecordingStatus {
        match self.recording.lock().as_ref() {
            None => JsRecordingStatus::default(),
            Some(s) => JsRecordingStatus {
                active: true,
                duration_seconds: s.started_at.elapsed().as_secs_f64(),
                track_count: s.track_count as u32,
            },
        }
    }

    // ── DAUx backend API ───────────────────────────────────────────────────

    /// List all available DAUx backends on this platform.
    pub fn list_daux_backends(&self) -> Vec<JsDauxBackendInfo> {
        list_available_backends()
            .into_iter()
            .map(|b| JsDauxBackendInfo {
                id: b.id,
                name: b.name,
                available: b.available,
                is_default: b.is_default,
                description: b.description,
            })
            .collect()
    }

    /// Open (or re-open) a stream using the full DAUx config.
    /// This is the preferred method; `open_device()` is kept for backward compat.
    ///
    /// On failure the stream stays closed — the caller should call `open_daux`
    /// again with a safe fallback config.  Use `open_daux_safe` if you want the
    /// engine to handle the fallback automatically.
    pub fn open_daux(&self, config: JsDauxConfig) -> Result<(), SphereAudioError> {
        self.close_device_inner();

        // Sanitize before open so a persisted Exclusive ASIO id cannot reach
        // the ASIO arm on Community / unentitled builds — fall back to Auto
        // (WASAPI Shared on Windows) instead of failing closed with no stream.
        let requested = BackendKind::from_id(&config.backend_id);
        let backend = BackendKind::sanitize_for_current_platform(requested.clone());
        if backend != requested {
            eprintln!(
                "[DAUx] open_daux: backend {} unavailable; falling back to {}",
                requested.id(),
                backend.id()
            );
        }
        let daux_cfg = DauxDeviceConfig {
            backend: backend.clone(),
            output_device_id: config.output_device_id.filter(|s| !s.is_empty()),
            input_device_id: None,
            sample_rate: config.sample_rate.filter(|&v| v > 0),
            buffer_size: config.buffer_size.filter(|&v| v > 0),
            mmcss_priority: config.mmcss_priority,
            safe_mode: config.safe_mode,
        };

        *self.daux_config.lock() = daux_cfg.clone();
        self.glitch_counter.store(0, Ordering::Relaxed);

        let initial_runtime = self.get_initial_runtime(None);

        // Non-fatal open diagnostics (e.g. ASIO input degraded) applied after
        // the generic error-clearing below so they stay visible in Settings.
        #[allow(unused_mut)] // mutated only by the cfg-gated ASIO arm
        let mut deferred_open_warning: Option<String> = None;

        let stream = match backend {
            #[cfg(target_os = "windows")]
            BackendKind::WasapiExclusive => {
                let handle = wasapi_exclusive::open(
                    &daux_cfg,
                    Arc::clone(&self.shared),
                    initial_runtime,
                    Arc::clone(&self.glitch_counter),
                )?;
                let sr = handle.sample_rate;
                let bs = handle.buffer_size;
                let dev_name = handle.device_name.clone();
                let stream = ActiveStream::WasapiExclusive(handle);
                self.commit_stream_open(sr, bs, dev_name, "DAUx WASAPI Exclusive".into());
                stream
            }
            #[cfg(target_os = "windows")]
            BackendKind::WdmKs => {
                let handle = wdm_ks::open(
                    &daux_cfg,
                    Arc::clone(&self.shared),
                    initial_runtime,
                    Arc::clone(&self.glitch_counter),
                )?;
                let sr = handle.sample_rate;
                let bs = handle.buffer_size;
                let dev_name = handle.device_name.clone();
                let stream = ActiveStream::WdmKs(handle);
                self.commit_stream_open(sr, bs, dev_name, "DAUx WDM-KS".into());
                stream
            }
            #[cfg(all(target_os = "windows", feature = "asio"))]
            BackendKind::Asio => {
                let host = crate::backend::asio_host()?;
                let handle = crate::backend::asio_session::open_duplex(
                    host,
                    &daux_cfg,
                    Arc::clone(&self.shared),
                    initial_runtime,
                    Arc::clone(&self.glitch_counter),
                )?;
                let sr = handle.sample_rate;
                let bs = handle.buffer_size;
                let dev_name = handle.device_name.clone();
                deferred_open_warning = handle.input_warning.clone();
                *self.asio_caps.lock() = Some(handle.caps.clone());
                let stream = ActiveStream::AsioDuplex(Box::new(handle));
                self.commit_stream_open(sr, bs, dev_name, "DAUx ASIO".into());
                stream
            }
            #[cfg(not(all(target_os = "windows", feature = "asio")))]
            BackendKind::Asio => {
                return Err(SphereAudioError::BackendUnavailable(
                    "this build does not include the DAUx ASIO backend".into(),
                ));
            }
            #[cfg(not(target_os = "windows"))]
            BackendKind::WdmKs => {
                return Err(SphereAudioError::BackendUnavailable(
                    "DAUx WDM-KS is only available on Windows".into(),
                ));
            }
            _ => {
                let handle = cpal_backend::open(
                    &daux_cfg,
                    Arc::clone(&self.shared),
                    initial_runtime,
                    Arc::clone(&self.glitch_counter),
                )?;
                let sr = handle.sample_rate;
                let bs = handle.buffer_size;
                let dev_name = handle.device_name.clone();
                let backend_name = handle.backend_name.clone();
                let stream = ActiveStream::Cpal(handle);
                self.commit_stream_open(sr, bs, dev_name, backend_name);
                stream
            }
        };

        // Clear any previous error / device-lost flag on success.
        self.status.lock().last_daux_error = deferred_open_warning;
        self.shared.device_lost.store(false, Ordering::Relaxed);

        // Surface any divergence between the requested rate and the rate the
        // device actually opened at. Shared/Auto: allowed (logged + shown in
        // Settings). Exclusive: an implicit fallback happened — make it a
        // visible warning so the user isn't silently off-pitch.
        let active_sr = self.shared.sample_rate.load(Ordering::Relaxed);
        let requested_sr = daux_cfg.sample_rate.unwrap_or(0);
        if let Some(exclusive_warning) =
            log_sample_rate_decision(requested_sr, active_sr, &daux_cfg.backend)
        {
            self.status.lock().last_daux_error = Some(exclusive_warning);
        }

        *self.active_stream.lock() = Some(stream);

        // Re-derive input routing after every backend/device reopen. ASIO
        // restores atomic routing on its persistent callback; other backends
        // recreate the standalone capture stream when an armed/monitored track
        // needs one.
        if let Some(snapshot) = self.project.lock().clone() {
            if let Err(error) = self.sync_live_input_stream(&snapshot) {
                eprintln!("[DAUx] input routing restore failed: {error}");
            }
        }

        Ok(())
    }

    /// Re-open the audio device after a device-loss event using the last-known
    /// good DAUx config. Returns `Ok(true)` if a recovery was performed,
    /// `Ok(false)` if no recovery was needed (device not lost). On failure the
    /// `device_lost` flag stays set so the UI keeps showing DeviceLost.
    pub fn recover_daux(&self) -> Result<bool, SphereAudioError> {
        if !self.shared.device_lost.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let cfg = {
            let prev = self.daux_config.lock().clone();
            JsDauxConfig {
                backend_id: prev.backend.id().to_string(),
                output_device_id: prev.output_device_id.clone(),
                sample_rate: prev.sample_rate,
                buffer_size: prev.buffer_size,
                mmcss_priority: prev.mmcss_priority,
                safe_mode: prev.safe_mode,
            }
        };
        // `open_daux` clears `device_lost` on success.
        self.open_daux(cfg)?;
        Ok(true)
    }

    /// Whether applying `new` would require a controlled device restart versus
    /// the currently-open config. In this engine every device-shaping change
    /// (backend, device, sample rate, buffer size, MMCSS, safe mode) reopens the
    /// stream, so the UI should mark those settings "restart required".
    pub fn daux_requires_restart(&self, new: &JsDauxConfig) -> bool {
        let cur = self.daux_config.lock();
        let norm = |s: &Option<String>| s.clone().filter(|v| !v.is_empty());
        let norm_u32 = |v: Option<u32>| v.filter(|&n| n > 0);
        new.backend_id != cur.backend.id()
            || norm(&new.output_device_id) != cur.output_device_id
            || norm_u32(new.sample_rate) != cur.sample_rate
            || norm_u32(new.buffer_size) != cur.buffer_size
            || new.mmcss_priority != cur.mmcss_priority
            || new.safe_mode != cur.safe_mode
    }

    /// Safe variant: tries `new_config`, and on failure restores the previous
    /// working config.  Returns `Ok(())` if the new config succeeded, or
    /// `Err(message)` describing why exclusive failed (after restoring the
    /// previous backend).  The engine always ends up with an open stream.
    pub fn open_daux_safe(&self, new_config: JsDauxConfig) -> Result<(), SphereAudioError> {
        let attempted_backend_name = BackendKind::from_id(&new_config.backend_id).display_name();
        // Save the previous working config before closing.
        let previous_config = {
            let prev = self.daux_config.lock().clone();
            JsDauxConfig {
                backend_id: prev.backend.id().to_string(),
                output_device_id: prev.output_device_id.clone(),
                sample_rate: prev.sample_rate,
                buffer_size: prev.buffer_size,
                mmcss_priority: prev.mmcss_priority,
                safe_mode: prev.safe_mode,
            }
        };
        let had_previous_stream = self.active_stream.lock().is_some();

        // Stop transport so playback doesn't resume mid-switch.
        self.shared.playing.store(false, Ordering::Relaxed);

        match self.open_daux(new_config) {
            Ok(()) => Ok(()),
            Err(open_err) => {
                let err_msg = open_err.to_string();
                eprintln!("[DAUx] open_daux_safe: failed ({err_msg}), attempting fallback");

                // Store the error before trying to restore.
                self.status.lock().last_daux_error = Some(err_msg.clone());

                // Attempt to restore the previous working config.
                if had_previous_stream {
                    match self.open_daux(previous_config) {
                        Ok(()) => {
                            eprintln!("[DAUx] open_daux_safe: previous backend restored");
                            let restore_msg = format!(
                                "{attempted_backend_name} backend switch failed: {err_msg}. Reverted to previous backend."
                            );
                            self.status.lock().last_daux_error = Some(restore_msg.clone());
                            Err(SphereAudioError::StreamOpenFailed(restore_msg))
                        }
                        Err(restore_err) => {
                            eprintln!("[DAUx] open_daux_safe: fallback also failed: {restore_err}");
                            let combined = format!(
                                "{attempted_backend_name} backend switch failed: {err_msg}. Fallback also failed: {restore_err}"
                            );
                            self.status.lock().last_daux_error = Some(combined.clone());
                            Err(SphereAudioError::StreamOpenFailed(combined))
                        }
                    }
                } else {
                    Err(open_err)
                }
            }
        }
    }

    fn sync_live_input_stream(
        &self,
        snapshot: &EngineProjectSnapshot,
    ) -> Result<(), SphereAudioError> {
        let mut monitored_route: Option<(&str, &EngineTrackInputSourceSnapshot)> = None;
        for track in snapshot.tracks.iter().filter(|track| {
            track.track_type == "audio"
                && track.input_monitor
                && !track.input_source.channels.is_empty()
        }) {
            if let Some((other_track_id, other_source)) = monitored_route {
                if other_source.device_id != track.input_source.device_id
                    || other_source.channels != track.input_source.channels
                {
                    return Err(SphereAudioError::InvalidConfig(format!(
                        "software monitoring routes conflict: track '{}' and track '{}' use different hardware inputs",
                        other_track_id, track.id
                    )));
                }
            } else {
                monitored_route = Some((track.id.as_str(), &track.input_source));
            }
        }

        // ASIO: the duplex session's persistent input already feeds the ring.
        // Route changes are atomic stores — never a stream open/close, which
        // would dispose the shared buffer set and kill playback. During stream
        // startup/restart the backend is selected before session capabilities
        // are published; defer routing in that window. `open_daux` restores it
        // from the project as soon as the duplex session is installed.
        if self.daux_config.lock().backend == BackendKind::Asio {
            #[cfg(all(target_os = "windows", feature = "asio"))]
            if let Some(caps) = self.asio_caps.lock().clone() {
                return self.sync_asio_input_routing(snapshot, &caps);
            }

            self.stop_live_input_stream();
            return Ok(());
        }

        self.shared
            .monitor_shared_clock
            .store(false, Ordering::Relaxed);

        // The first armed/monitored audio track with a routable input source
        // drives which device + channels the shared capture stream uses.
        let desired_track = snapshot.tracks.iter().find(|track| {
            track.track_type == "audio"
                && (track.armed || track.input_monitor)
                && !track.input_source.channels.is_empty()
        });

        let wants_live_input = desired_track.is_some();

        // Any track that explicitly wants monitoring (not just record-arm) —
        // gates whether the render callback mixes the input bus to the output.
        let monitor_any = snapshot.tracks.iter().any(|track| {
            track.track_type == "audio"
                && track.input_monitor
                && !track.input_source.channels.is_empty()
        });
        self.shared
            .monitor_enabled_any
            .store(monitor_any, Ordering::Relaxed);

        if !wants_live_input {
            self.stop_live_input_stream();
            return Ok(());
        }

        // Device resolution order: the track's own pinned device, then the
        // global Preferences input device, then the system default.
        let desired_device = desired_track
            .and_then(|track| track.input_source.device_id.clone())
            .filter(|device_id| !device_id.trim().is_empty())
            .or_else(|| {
                snapshot
                    .preferred_input_device
                    .clone()
                    .filter(|d| !d.trim().is_empty())
            });

        // Which device channels feed the L/R input bus.
        let (mon_l_ch, mon_r_ch) = desired_track
            .and_then(|track| monitor_channel_pair(&track.input_source.channels))
            .unwrap_or((0, 1));
        self.shared.set_monitor_source_pair(mon_l_ch, mon_r_ch);

        eprintln!(
            "[Engine] applied input device = {:?} (preferred={:?}) monitor_any={}",
            desired_device, snapshot.preferred_input_device, monitor_any
        );

        // Reuse the existing stream when the device is unchanged — only the
        // monitor flag/channels may have moved (already stored above).
        if self
            .live_input
            .lock()
            .as_ref()
            .is_some_and(|handle| handle.device_id == desired_device)
        {
            self.shared.live_input_active.store(true, Ordering::Relaxed);
            self.warm_output_for_monitoring();
            return Ok(());
        }

        self.stop_live_input_stream();
        let device = self.find_input_device_for_active_backend(desired_device.as_deref())?;
        let default_cfg = device.default_input_config().map_err(|e| {
            SphereAudioError::NativeError(format!("Input device config error: {e}"))
        })?;
        let stream_config = cpal::StreamConfig {
            channels: default_cfg.channels(),
            sample_rate: default_cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let channel_count = stream_config.channels.max(1) as usize;
        let input_sr = stream_config.sample_rate.0;
        let device_label = device
            .name()
            .unwrap_or_else(|_| desired_device.clone().unwrap_or_else(|| "default".into()));

        eprintln!(
            "[AudioDevice] opening input stream: device='{device_label}' sample_rate={input_sr} \
             channels={channel_count} buffer=default monitor_channels=({mon_l_ch},{mon_r_ch})"
        );
        eprintln!(
            "[WASAPI] input sample_rate={input_sr} channels={channel_count} format={:?}",
            default_cfg.sample_format()
        );
        let output_sr = self.shared.sample_rate.load(Ordering::Relaxed);
        if output_sr != 0 && input_sr != 0 && output_sr != input_sr {
            eprintln!(
                "[WASAPI] ⚠ input sample_rate ({input_sr}) != output sample_rate ({output_sr}); \
                 monitored audio will be pitch-shifted until resampling is added (Layer 9 TODO)"
            );
        }

        // Reset and arm the input ring for this stream's format.
        self.shared
            .input_ring
            .set_active(true, channel_count as u32, input_sr);

        let shared = Arc::clone(&self.shared);
        let stream = device
            .build_input_stream::<f32, _, _>(
                &stream_config,
                move |data: &[f32], _info| {
                    let frames = data.len() / channel_count.max(1);
                    shared.input_cb_count.fetch_add(1, Ordering::Relaxed);
                    shared
                        .input_frames_received
                        .fetch_add(frames as u64, Ordering::Relaxed);
                    let (mon_l_ch, mon_r_ch) = shared.monitor_source_pair();
                    let mon_l_ch = mon_l_ch as usize;
                    let mon_r_ch = mon_r_ch as usize;
                    let mut peak_l = 0.0f32;
                    let mut peak_r = 0.0f32;
                    let mut last_l = 0.0f32;
                    let mut last_r = 0.0f32;
                    for frame in data.chunks(channel_count) {
                        // Pick this block's monitor channels; fall back to the
                        // first sample when a channel index is out of range.
                        let first = frame.first().copied().unwrap_or(0.0);
                        let l = frame
                            .get(mon_l_ch)
                            .copied()
                            .unwrap_or(first)
                            .clamp(-1.0, 1.0);
                        let r = frame.get(mon_r_ch).copied().unwrap_or(l).clamp(-1.0, 1.0);
                        last_l = l;
                        last_r = r;
                        peak_l = peak_l.max(l.abs());
                        peak_r = peak_r.max(r.abs());
                        // Layer 4: publish the actual samples to the output side.
                        shared.input_ring.write_stereo(l, r);
                    }
                    shared
                        .live_input_l
                        .store(f32_store(last_l), Ordering::Relaxed);
                    shared
                        .live_input_r
                        .store(f32_store(last_r), Ordering::Relaxed);
                    atomic_max_f32_bits(&shared.live_input_peak_l, peak_l);
                    atomic_max_f32_bits(&shared.live_input_peak_r, peak_r);
                    shared.live_input_active.store(true, Ordering::Relaxed);
                },
                |err| eprintln!("[SphereAudio] Live input stream error: {err}"),
                None,
            )
            .map_err(|e| {
                self.shared.input_ring.set_active(false, 0, 0);
                SphereAudioError::NativeError(format!("Cannot open live input stream: {e}"))
            })?;
        stream
            .play()
            .map_err(|e| SphereAudioError::NativeError(format!("Live input play failed: {e}")))?;
        eprintln!("[AudioDevice] input stream started: device='{device_label}'");

        *self.live_input.lock() = Some(LiveInputHandle {
            _stream: stream,
            device_id: desired_device,
        });
        self.shared.live_input_active.store(true, Ordering::Relaxed);

        // Make sure the output render callback is actually running so per-track
        // input meters and the monitor mix update even before transport play.
        self.warm_output_for_monitoring();
        Ok(())
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    fn validate_asio_input_source(
        &self,
        track_id: &str,
        input_source: &EngineTrackInputSourceSnapshot,
        caps: &crate::backend::AsioSessionCaps,
    ) -> Result<(), SphereAudioError> {
        if input_source.channels.is_empty() {
            return Ok(());
        }
        let failure = if caps.input_channels == 0 {
            Some(format!(
                "ASIO driver '{}' has no usable input channels",
                caps.driver
            ))
        } else if let Some(device_id) = input_source
            .device_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .filter(|id| *id != caps.driver)
        {
            Some(format!(
                "track '{track_id}' input device '{device_id}' is not the active ASIO driver '{}'",
                caps.driver
            ))
        } else {
            input_source
                .channels
                .iter()
                .copied()
                .find(|channel| *channel >= caps.input_channels)
                .map(|channel| {
                    format!(
                        "track '{track_id}' input channel {} is unavailable on ASIO driver '{}' ({} input channel(s))",
                        channel.saturating_add(1),
                        caps.driver,
                        caps.input_channels
                    )
                })
        };
        if let Some(message) = failure {
            eprintln!("[DAUx ASIO] {message}");
            self.status.lock().last_error = Some(message.clone());
            return Err(SphereAudioError::InvalidConfig(message));
        }
        Ok(())
    }

    /// Apply track input routing to the persistent ASIO session.
    ///
    /// Everything here is an atomic store into `SharedState` — the session's
    /// input callback re-reads the monitor channel indices every block and the
    /// ring's active flag gates the output-side monitor mix. No streams are
    /// created, no buffers touched, no project reload issued.
    #[cfg(all(target_os = "windows", feature = "asio"))]
    fn sync_asio_input_routing(
        &self,
        snapshot: &EngineProjectSnapshot,
        caps: &crate::backend::AsioSessionCaps,
    ) -> Result<(), SphereAudioError> {
        let active_tracks = || {
            snapshot.tracks.iter().filter(|track| {
                track.track_type == "audio"
                    && (track.armed || track.input_monitor)
                    && !track.input_source.channels.is_empty()
            })
        };
        for track in active_tracks() {
            if let Err(error) =
                self.validate_asio_input_source(&track.id, &track.input_source, caps)
            {
                self.stop_live_input_stream();
                return Err(error);
            }
        }
        // The shared monitor ring currently carries one pair. A monitored route
        // must win over an armed-only route so record arm never changes what the
        // user hears while per-track monitor buses are prepared separately.
        let desired_track = active_tracks()
            .find(|track| track.input_monitor)
            .or_else(|| active_tracks().next());
        let monitor_any = snapshot.tracks.iter().any(|track| {
            track.track_type == "audio"
                && track.input_monitor
                && !track.input_source.channels.is_empty()
        });
        self.shared
            .monitor_enabled_any
            .store(monitor_any, Ordering::Relaxed);

        let Some(track) = desired_track else {
            // Nothing armed/monitored: deactivate the ring (input keeps
            // running for meters) and clear the monitor meter state.
            self.stop_live_input_stream();
            return Ok(());
        };

        let (left, right) = monitor_channel_pair(&track.input_source.channels).unwrap_or((0, 0));

        self.shared.set_monitor_source_pair(left, right);
        self.shared
            .monitor_enabled_any
            .store(monitor_any, Ordering::Relaxed);
        self.shared
            .input_ring
            .set_active(true, caps.input_channels, caps.sample_rate);
        self.shared
            .monitor_shared_clock
            .store(true, Ordering::Relaxed);
        self.shared.live_input_active.store(true, Ordering::Relaxed);
        self.warm_output_for_monitoring();
        Ok(())
    }

    /// Resume the (already-open) output stream so `fill_output_f32` runs while
    /// monitoring/armed, without advancing the transport. No-op when no stream
    /// is open yet — the next explicit `start()` will pick it up.
    fn warm_output_for_monitoring(&self) {
        let already_running = self.status.lock().running;
        if already_running {
            return;
        }
        match self.start() {
            Ok(()) => eprintln!("[AudioDevice] output stream warmed for monitoring"),
            Err(SphereAudioError::EngineNotOpen) => {}
            Err(e) => eprintln!("[AudioDevice] could not warm output for monitoring: {e}"),
        }
    }

    fn stop_live_input_stream(&self) {
        *self.live_input.lock() = None;
        self.shared
            .live_input_active
            .store(false, Ordering::Relaxed);
        self.shared
            .monitor_shared_clock
            .store(false, Ordering::Relaxed);
        self.shared.input_ring.set_active(false, 0, 0);
        self.shared
            .monitor_enabled_any
            .store(false, Ordering::Relaxed);
        self.shared
            .live_input_l
            .store(f32_store(0.0), Ordering::Relaxed);
        self.shared
            .live_input_r
            .store(f32_store(0.0), Ordering::Relaxed);
        self.shared
            .live_input_peak_l
            .store(f32_store(0.0), Ordering::Relaxed);
        self.shared
            .live_input_peak_r
            .store(f32_store(0.0), Ordering::Relaxed);
        self.shared
            .input_bus_peak_l
            .store(f32_store(0.0), Ordering::Relaxed);
        self.shared
            .input_bus_peak_r
            .store(f32_store(0.0), Ordering::Relaxed);
    }

    /// Start an input-level test on `device_id` (or the default input). Opens a
    /// capture stream whose callback tracks the running peak; poll it with
    /// [`Self::get_input_test_level`] and stop with [`Self::stop_input_test`].
    /// Independent of recording and of the output device.
    pub fn start_input_test(&self, device_id: Option<String>) -> Result<(), SphereAudioError> {
        // Replace any existing test stream.
        *self.input_test.lock() = None;
        self.input_test_uses_session
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // ASIO: the session input is already running — the "test" is just a
        // read of the session-wide peak. Never open a second stream.
        if self.daux_config.lock().backend == BackendKind::Asio {
            let caps = self.asio_caps.lock().clone();
            return match caps {
                Some(caps) if caps.input_channels > 0 => {
                    self.shared
                        .session_input_peak
                        .store(f32_store(0.0), Ordering::Relaxed);
                    self.input_test_uses_session
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                }
                Some(caps) => Err(SphereAudioError::NativeError(format!(
                    "ASIO driver '{}' has no usable input channels",
                    caps.driver
                ))),
                None => Err(SphereAudioError::NativeError(
                    "open the ASIO stream before testing its input".into(),
                )),
            };
        }

        let device = self.find_input_device_for_active_backend(device_id.as_deref())?;
        let default_cfg = device.default_input_config().map_err(|e| {
            SphereAudioError::NativeError(format!("Input device config error: {e}"))
        })?;
        let stream_config = cpal::StreamConfig {
            channels: default_cfg.channels(),
            sample_rate: default_cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let peak = Arc::new(AtomicU32::new(0));
        let cb_peak = Arc::clone(&peak);
        let stream = device
            .build_input_stream::<f32, _, _>(
                &stream_config,
                move |data: &[f32], _info| {
                    let mut block_peak = 0.0f32;
                    for &s in data {
                        let a = s.abs();
                        if a > block_peak {
                            block_peak = a;
                        }
                    }
                    // Keep the running max until the UI polls (swap-resets it).
                    let mut cur = cb_peak.load(Ordering::Relaxed);
                    loop {
                        if block_peak <= f32::from_bits(cur) {
                            break;
                        }
                        match cb_peak.compare_exchange_weak(
                            cur,
                            block_peak.to_bits(),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(c) => cur = c,
                        }
                    }
                },
                |err| eprintln!("[SphereAudio] Input test stream error: {err}"),
                None,
            )
            .map_err(|e| {
                SphereAudioError::NativeError(format!("Cannot open input test stream: {e}"))
            })?;
        stream
            .play()
            .map_err(|e| SphereAudioError::NativeError(format!("Input test play failed: {e}")))?;

        *self.input_test.lock() = Some(InputTestHandle {
            _stream: stream,
            peak,
        });
        Ok(())
    }

    /// Read (and reset) the peak input level since the last poll, `0.0..=1.0`.
    /// Returns `0.0` when no input test is active.
    pub fn get_input_test_level(&self) -> f32 {
        if self
            .input_test_uses_session
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return f32::from_bits(self.shared.session_input_peak.swap(0, Ordering::Relaxed));
        }
        self.input_test
            .lock()
            .as_ref()
            .map(|h| f32::from_bits(h.peak.swap(0, Ordering::Relaxed)))
            .unwrap_or(0.0)
    }

    /// Stop and release the input-level test stream.
    pub fn stop_input_test(&self) {
        *self.input_test.lock() = None;
        self.input_test_uses_session
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Aggregate latency report (Phase V — reporting only).
    ///
    /// Sums each track's enabled native-plugin insert latencies (queried from
    /// the live VST3 processors on the control-side runtime copy) plus the
    /// device buffer latency. Full plug-in delay compensation is Phase W; this
    /// is the data the UI displays and the basis (`max_track_samples`) PDC will
    /// later use.
    pub fn get_latency_info(&self) -> crate::types::JsLatencyInfo {
        use crate::latency_graph::strip_plugin_latency_samples;
        use crate::types::{JsLatencyInfo, JsTrackLatency};
        let runtime = self.runtime.lock();
        let (sample_rate, buffer_frames) = {
            let st = self.status.lock();
            (st.sample_rate.max(1), st.buffer_size)
        };
        let to_ms = |samples: u32| samples as f64 / sample_rate as f64 * 1000.0;
        let pdc_enabled = self.pdc_enabled();

        let max_path_samples = runtime.latency_graph.max_path_latency_samples;
        let mut tracks = Vec::new();
        let mut master_samples = 0u32;
        let mut max_track_plugin_samples = 0u32;
        for (idx, track) in runtime.tracks.iter().enumerate() {
            let mut samples: i64 = 0;
            for insert in &track.inserts {
                if !insert.enabled {
                    continue;
                }
                if let Some(vst3) = insert.vst3.as_ref() {
                    if vst3.is_ready() {
                        samples += vst3.get_latency_samples().max(0) as i64;
                    }
                } else if insert.kind.eq_ignore_ascii_case("external-bridge-plugin") {
                    // Bridged plugin: the host-reported plugin latency plus the
                    // one-block bridge handshake (the engine reads each block one
                    // late). Reporting only — PDC compensation of this is a
                    // dedicated follow-up (needs a graph rebuild on report).
                    if let Some(sink) = runtime.plugin_bridge_sinks.get(&insert.id) {
                        samples += sink.reported_latency_samples() as i64 + buffer_frames as i64;
                    }
                }
            }
            let plugin_samples = if samples > 0 {
                samples as u32
            } else {
                strip_plugin_latency_samples(track)
            };
            let path_samples = max_path_samples.saturating_sub(
                runtime
                    .latency_graph
                    .track_pdc_delay
                    .get(idx)
                    .copied()
                    .unwrap_or(0),
            );
            let pdc_delay_samples = runtime
                .latency_graph
                .track_pdc_delay
                .get(idx)
                .copied()
                .unwrap_or(0);
            if track.track_type == "master" {
                master_samples = plugin_samples;
            } else {
                max_track_plugin_samples = max_track_plugin_samples.max(plugin_samples);
                tracks.push(JsTrackLatency {
                    track_id: track.id.clone(),
                    plugin_samples,
                    plugin_ms: to_ms(plugin_samples),
                    path_samples,
                    path_ms: to_ms(path_samples),
                    pdc_delay_samples,
                    pdc_delay_ms: to_ms(pdc_delay_samples),
                });
            }
        }

        JsLatencyInfo {
            sample_rate,
            buffer_frames,
            buffer_ms: to_ms(buffer_frames),
            tracks,
            master_samples,
            master_ms: to_ms(master_samples),
            max_path_samples,
            max_path_ms: to_ms(max_path_samples),
            pdc_enabled,
            max_track_samples: max_track_plugin_samples,
        }
    }

    /// Return the current DAUx status (backend, device, latency, glitches).
    pub fn get_daux_status(&self) -> JsDauxStatus {
        let st = self.status.lock().clone();
        let daux_cfg = self.daux_config.lock().clone();
        let glitch_count = self.glitch_counter.load(Ordering::Relaxed) as f64;
        let mmcss_active = self.shared.mmcss_active.load(Ordering::Relaxed);
        let device_lost = self.shared.device_lost.load(Ordering::Relaxed);
        let running = self.shared.playing.load(Ordering::Relaxed);
        let device_state = if device_lost {
            "DeviceLost"
        } else if !st.stream_open {
            "Closed"
        } else if running {
            "Running"
        } else {
            "Ready"
        }
        .to_string();

        let backend_id = daux_cfg.backend.id().to_string();
        let backend_name = if let Some(stream) = self.active_stream.lock().as_ref() {
            stream.backend_name().to_string()
        } else {
            daux_cfg.backend.display_name().to_string()
        };

        let estimated_latency_ms = if st.sample_rate > 0 {
            st.buffer_size as f64 / st.sample_rate as f64 * 1000.0
        } else {
            0.0
        };

        JsDauxStatus {
            backend_id,
            backend_name,
            output_device: st.output_device,
            sample_rate: st.sample_rate,
            requested_sample_rate: daux_cfg.sample_rate.unwrap_or(0),
            buffer_size: st.buffer_size,
            estimated_latency_ms,
            glitch_count,
            mmcss_active,
            last_error: st.last_daux_error.clone(),
            device_lost,
            device_state,
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    fn get_initial_runtime(&self, sample_rate_override: Option<u32>) -> RuntimeProject {
        let sr = sample_rate_override
            .unwrap_or_else(|| self.shared.sample_rate.load(Ordering::Relaxed).max(44100));

        // Prefer the live runtime built by `load_project`. Rebuilding from the
        // snapshot here re-decodes media on the caller thread and has frozen
        // the native UI when Play re-opened the device after a sync.
        {
            let cached = self.runtime.lock().clone();
            if !cached.tracks.is_empty() {
                let mut runtime = cached;
                runtime.sample_rate = sr;
                return runtime;
            }
        }

        self.project
            .lock()
            .as_ref()
            .map(|snapshot| {
                let mut audio_cache = self.audio_cache.lock();
                match RuntimeProject::build(snapshot, sr, &mut audio_cache, None, self.pdc_enabled()) {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        eprintln!(
                            "[SphereAudio] get_initial_runtime: invalid routing graph ({e}), keeping previous runtime"
                        );
                        let mut runtime = self.runtime.lock().clone();
                        runtime.sample_rate = sr;
                        runtime
                    }
                }
            })
            .unwrap_or_else(|| {
                let mut runtime = self.runtime.lock().clone();
                runtime.sample_rate = sr;
                runtime
            })
    }

    fn commit_stream_open(&self, sr: u32, bs: u32, device_name: String, backend_name: String) {
        self.shared.sample_rate.store(sr, Ordering::Relaxed);
        let mut st = self.status.lock();
        st.stream_open = true;
        st.running = false;
        st.sample_rate = sr;
        st.buffer_size = bs;
        st.output_device = Some(device_name);
        st.last_error = None;
        eprintln!("[audio-engine] active_sample_rate={sr} block_size={bs}");
        eprintln!("[DAUx] Stream committed: backend={backend_name} sr={sr} buf={bs}");
    }

    fn cmd_sender(&self) -> Option<Sender<EngineCommand>> {
        if let Some(stream) = self.active_stream.lock().as_ref() {
            if let Some(tx) = stream.cmd_tx() {
                return Some(tx.clone());
            }
        }
        self.cmd_tx.lock().as_ref().cloned()
    }

    fn send_command(&self, cmd: EngineCommand) -> Result<(), SphereAudioError> {
        if transport_freeze_debug_enabled() {
            let label = match &cmd {
                EngineCommand::LoadProject(_) => "LoadProject",
                EngineCommand::SetTestTone { .. } => "SetTestTone",
                EngineCommand::SetMasterVolume { .. } => "SetMasterVolume",
                EngineCommand::SetTrackVolume { .. } => "SetTrackVolume",
                EngineCommand::SetTrackPan { .. } => "SetTrackPan",
                EngineCommand::SetTrackMute { .. } => "SetTrackMute",
                EngineCommand::SetTrackSolo { .. } => "SetTrackSolo",
                EngineCommand::SetTrackInputState { .. } => "SetTrackInputState",
                EngineCommand::SetTrackPreviewMode { .. } => "SetTrackPreviewMode",
                EngineCommand::SetInsertParam { .. } => "SetInsertParam",
                EngineCommand::MidiPreviewNoteOn { .. } => "MidiPreviewNoteOn",
                EngineCommand::MidiPreviewNoteOff { .. } => "MidiPreviewNoteOff",
                EngineCommand::MidiPreviewControlChange { .. } => "MidiPreviewControlChange",
                EngineCommand::MidiPreviewAllNotesOff { .. } => "MidiPreviewAllNotesOff",
                EngineCommand::PluginPreviewNoteOn { .. } => "PluginPreviewNoteOn",
                EngineCommand::PluginPreviewNoteOff { .. } => "PluginPreviewNoteOff",
                EngineCommand::PluginPreviewControlChange { .. } => "PluginPreviewControlChange",
                EngineCommand::PluginPreviewAllNotesOff { .. } => "PluginPreviewAllNotesOff",
                EngineCommand::StartTransport => "StartTransport",
                EngineCommand::StopTransport => "StopTransport",
                EngineCommand::Seek { .. } => "Seek",
                EngineCommand::SetMetronomeEnabled(_) => "SetMetronomeEnabled",
                EngineCommand::SetMetronomeSuspended(_) => "SetMetronomeSuspended",
                EngineCommand::SetBpm(_) => "SetBpm",
                EngineCommand::SetTempoMap(_) => "SetTempoMap",
                EngineCommand::SetTimeSignature(_, _) => "SetTimeSignature",
                EngineCommand::SetTimeSignatureMap(_) => "SetTimeSignatureMap",
                EngineCommand::SetLoop { .. } => "SetLoop",
                EngineCommand::SetPluginBridgeSink { .. } => "SetPluginBridgeSink",
                EngineCommand::CommandBarrier { .. } => "CommandBarrier",
                EngineCommand::SetBridgeEditorActive { .. } => "SetBridgeEditorActive",
                EngineCommand::StartAudition { .. } => "StartAudition",
                EngineCommand::StopAudition => "StopAudition",
            };
            eprintln!("[play-debug engine] send_command {label}");
        }
        let tx = self.cmd_sender().ok_or(SphereAudioError::EngineNotOpen)?;
        tx.try_send(cmd)
            .map_err(|e| SphereAudioError::NativeError(e.to_string()))
    }

    pub(crate) fn audition_file_async(
        self: &Arc<Self>,
        path: String,
    ) -> Result<(), SphereAudioError> {
        self.cmd_sender().ok_or(SphereAudioError::EngineNotOpen)?;
        let engine = Arc::clone(self);
        std::thread::Builder::new()
            .name("browser-audition-decode".to_string())
            .spawn(move || match crate::audio_file::load_audio_file(&path) {
                Ok(source) => {
                    if let Err(error) = engine.send_command(EngineCommand::StartAudition {
                        source: Box::new(source),
                    }) {
                        eprintln!("[audition] could not start preview for {path}: {error}");
                    }
                }
                Err(error) => eprintln!("[audition] could not decode {path}: {error}"),
            })
            .map_err(|error| SphereAudioError::NativeError(error.to_string()))?;
        Ok(())
    }

    pub(crate) fn stop_audition(&self) -> Result<(), SphereAudioError> {
        self.send_command(EngineCommand::StopAudition)
    }
}

fn transport_freeze_debug_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FUTUREBOARD_TRANSPORT_FREEZE_DEBUG").is_some())
}

// ── Audio callback builder ────────────────────────────────────────────────────

fn output_stream_candidates(
    device: &cpal::Device,
    requested: &JsDeviceOpenConfig,
) -> Result<Vec<OutputStreamCandidate>, String> {
    let default_supported = device.default_output_config().map_err(|e| e.to_string())?;
    let default_config = default_supported.config();
    let sample_format = default_supported.sample_format();

    let mut candidates = Vec::new();

    if requested.sample_rate.is_some() || requested.buffer_size.is_some() {
        let mut requested_config = default_config.clone();

        if let Some(sample_rate) = requested.sample_rate {
            requested_config.sample_rate = cpal::SampleRate(sample_rate);
        }
        if let Some(buffer_size) = requested.buffer_size {
            requested_config.buffer_size = BufferSize::Fixed(buffer_size);
        }

        candidates.push(OutputStreamCandidate {
            config: requested_config,
            sample_format,
            label: "requested",
        });
    }

    candidates.push(OutputStreamCandidate {
        config: default_config,
        sample_format,
        label: "default",
    });

    Ok(candidates)
}

fn reported_buffer_size(config: &cpal::StreamConfig) -> u32 {
    match config.buffer_size {
        BufferSize::Fixed(frames) => frames,
        BufferSize::Default => 0,
    }
}

/// Compact, log-friendly mode token for the audio-device sample-rate decision.
fn sample_rate_mode_label(backend: &BackendKind) -> &'static str {
    match backend {
        BackendKind::Auto => "AUTO",
        BackendKind::WasapiShared => "WASAPI_SHARED",
        BackendKind::WasapiExclusive => "WASAPI_EXCLUSIVE",
        BackendKind::WdmKs => "WDM_KS",
        BackendKind::Asio => "ASIO",
        BackendKind::CoreAudio => "COREAUDIO",
        BackendKind::Alsa => "ALSA",
        BackendKind::MmeFallback => "MME",
    }
}

/// Returns `Some((requested, active))` when a *specific* rate was requested
/// (`requested > 0`) and the opened stream runs at a different rate. `None`
/// otherwise (no specific request, or the rates agree). Control-thread only —
/// the realtime callback must never call this.
pub(crate) fn sample_rate_mismatch(requested: u32, active: u32) -> Option<(u32, u32)> {
    if requested > 0 && active > 0 && requested != active {
        Some((requested, active))
    } else {
        None
    }
}

/// Emit the visible sample-rate decision after a stream opens (control thread).
/// Shared/Auto/WDM-KS mismatch is *allowed* (Windows resamples) but must be
/// visible. Exclusive-mode mismatch means an implicit fallback happened and is
/// surfaced as a warning string the caller can store in `last_daux_error`.
fn log_sample_rate_decision(requested: u32, active: u32, backend: &BackendKind) -> Option<String> {
    let (req, act) = sample_rate_mismatch(requested, active)?;
    let mode = sample_rate_mode_label(backend);
    eprintln!("[audio-device] requested_sample_rate={req} active_sample_rate={act} mode={mode}");
    eprintln!("[audio-device] sample rate mismatch: using active device rate for timing");
    if matches!(backend, BackendKind::WasapiExclusive) {
        // Exclusive mode could not honor the exact rate and fell back to the
        // native device rate — a real degradation the user should see.
        let msg = format!(
            "Exclusive mode could not open at {req} Hz; the device is running at {act} Hz. \
             Timing uses the active device rate."
        );
        eprintln!("[audio-device] WARNING: {msg}");
        Some(msg)
    } else {
        None
    }
}

// ── DSP / render kernel (see `engine/render.rs`) ──────────────────────────────
//
// The per-block render path was split into the `render` submodule so this file
// stays focused on device lifecycle, command dispatch, and the public engine
// API. Pure relocation — re-exported here so existing `crate::engine::*` call
// sites and the in-file controller keep resolving unchanged.
mod render;
pub use render::*;

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: SampleFormat,
    cmd_rx: Receiver<EngineCommand>,
    shared: Arc<SharedState>,
    initial_runtime: RuntimeProject,
) -> Result<cpal::Stream, String> {
    match sample_format {
        SampleFormat::I8 => {
            build_output_stream_typed::<i8>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::I16 => {
            build_output_stream_typed::<i16>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::I32 => {
            build_output_stream_typed::<i32>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::I64 => {
            build_output_stream_typed::<i64>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::U8 => {
            build_output_stream_typed::<u8>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::U16 => {
            build_output_stream_typed::<u16>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::U32 => {
            build_output_stream_typed::<u32>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::U64 => {
            build_output_stream_typed::<u64>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::F32 => {
            build_output_stream_typed::<f32>(device, config, cmd_rx, shared, initial_runtime)
        }
        SampleFormat::F64 => {
            build_output_stream_typed::<f64>(device, config, cmd_rx, shared, initial_runtime)
        }
        format => Err(format!("unsupported output sample format: {format}")),
    }
}

fn build_output_stream_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    cmd_rx: Receiver<EngineCommand>,
    shared: Arc<SharedState>,
    initial_runtime: RuntimeProject,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Sample + FromSample<f32>,
{
    let output_sample_rate = config.sample_rate.0;
    let sr = output_sample_rate as f64;

    // Oscillator state — local to the audio callback (no lock needed).
    let mut osc_l = SineOscillator::new(440.0, sr);
    let mut osc_r = SineOscillator::new(440.0, sr);
    let mut osc_freq = 440.0f32;
    let mut osc_on = false;

    // Local playback state.
    let mut playing_local = false;
    let mut render_path_logged = false;
    let mut block_scratch: Vec<f32> = Vec::new();
    let mut metronome = crate::backend::render::LocalAudioState::new(sr);
    let mut audition: Option<AudioFileAudition> = None;

    // Meter state with peak hold.
    let mut prev_peak_l = 0.0f32;
    let mut prev_peak_r = 0.0f32;

    let ch = config.channels as usize;
    let mut runtime = initial_runtime;
    runtime.sample_rate = output_sample_rate;

    let stream = device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                // Dropout watchdog: time the whole callback (atomics only). The
                // legacy in-process callback previously had no timing — only the
                // DAUx backend did — so realtime dropouts went undetected here.
                let cb_started = std::time::Instant::now();

                // Publish the output play-out latency (callback → DAC) so a live
                // recording can shift its take earlier by the real round-trip.
                // cpal already computes these instants; this is one atomic store.
                let out_ts = info.timestamp();
                if let Some(delay) = out_ts.playback.duration_since(&out_ts.callback) {
                    shared
                        .output_latency_secs
                        .store(f32_store(delay.as_secs_f32()), Ordering::Relaxed);
                }
                if ch > 0 {
                    for track in &mut runtime.tracks {
                        track.midi_block_events.clear();
                    }
                }

                // ── 1. Drain command queue ───────────────────────────────────
                // Runs first so commands take effect from the start of this block.
                let frames_for_flush = if ch > 0 {
                    data.len().checked_div(ch).unwrap_or(0)
                } else {
                    0
                };
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        EngineCommand::LoadProject(next_runtime) => {
                            if callback_debug_enabled() {
                                eprintln!(
                                    "[SphereAudio callback] LoadProject: {} tracks, {} clips (sr={})",
                                    next_runtime.tracks.len(),
                                    next_runtime.clips.len(),
                                    output_sample_rate,
                                );
                            }
                            // Release notes on the current graph before swapping
                            // it out, otherwise pending note-offs would be
                            // dropped with the retired VST3 processors.
                            runtime.all_notes_off("project_load");
                            runtime.flush_vst3_midi_inserts(frames_for_flush);
                            // Swap in the new graph and retire the old one off
                            // the realtime thread — its destructor frees
                            // buffers / munmaps sources / destroys VST3 handles
                            // and must not run here. See `crate::graveyard`.
                            let old = std::mem::replace(&mut runtime, *next_runtime);
                            runtime.sample_rate = output_sample_rate;
                            // Preserve the plugin-bridge sinks across reloads (Stage 3b).
                            runtime.plugin_bridge_sinks = old.plugin_bridge_sinks.clone();
                            // Re-cache the per-insert sink handles on the fresh
                            // graph (the block path reads insert.bridge_sink).
                            runtime.resolve_bridge_sinks();
                            runtime.bridge_editor_active = old.bridge_editor_active.clone();
                            // The panic pushed into the preserved sinks above
                            // still needs flushing through the new graph.
                            runtime.bridge_panic_flush_samples = old.bridge_panic_flush_samples;
                            runtime.bridge_preview_tail_samples = old.bridge_preview_tail_samples;
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            metronome.set_tempo_map(
                                runtime.tempo_map.clone(),
                                pos,
                                output_sample_rate,
                            );
                            crate::graveyard::retire(old);
                            if crate::runtime::midi_engine_debug_enabled() {
                                let notes: usize =
                                    runtime.midi_clips.iter().map(|c| c.events.len() / 2).sum();
                                eprintln!(
                                    "[DAUx MIDI] snapshot clips={} notes={}",
                                    runtime.midi_clips.len(),
                                    notes
                                );
                            }
                            // Position MIDI cursors for the current transport
                            // position so a load mid-playback stays in sync.
                            let midi_pos = shared.position_samples.load(Ordering::Relaxed);
                            runtime.reset_midi_playback(midi_pos);
                        }
                        EngineCommand::SetTestTone { enabled, frequency } => {
                            osc_on = enabled;
                            osc_freq = frequency;
                            osc_l.set_frequency(frequency as f64);
                            osc_r.set_frequency(frequency as f64);
                        }
                        EngineCommand::StartAudition { source } => {
                            if let Some(old) = audition.replace(AudioFileAudition::new(source)) {
                                crate::graveyard::retire_audio_file(old.into_source());
                            }
                        }
                        EngineCommand::StopAudition => {
                            if let Some(old) = audition.take() {
                                crate::graveyard::retire_audio_file(old.into_source());
                            }
                        }
                        EngineCommand::StartTransport => {
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            let active_clips = runtime.active_clip_count_at_sample(pos);
                            if command_debug_enabled() {
                                eprintln!(
                                    "[SphereAudio callback] StartTransport: position={}sa ({:.3}s), active_clip_count={}, scheduled_clip_count={}",
                                    pos,
                                    pos as f64 / output_sample_rate as f64,
                                    active_clips,
                                    runtime.clips.len(),
                                );
                            }
                            playing_local = true;
                            metronome.stop_tail_samples = 0;
                            shared.playing.store(true, Ordering::Relaxed);
                            // Position MIDI cursors at the start beat and clear
                            // any stale active notes so play-from is clean.
                            runtime.reset_midi_playback(pos);
                            // Clear stale PDC delay-line audio so the compensated
                            // (lower-latency / audio) tracks start settled and stay
                            // aligned with plugin/VSTi-latency tracks from the first
                            // audible block — parity with the DAUx backend and with
                            // offline export's fresh-runtime + warmup start. Without
                            // this, the legacy realtime callback replays pre-stop
                            // audio out of the delay rings, desyncing audio vs VSTi
                            // tracks at the start of every play (export was fine
                            // because it builds a fresh zeroed runtime). Realtime-safe
                            // zero-fill; runs only on Start.
                            runtime.reset_pdc_delay_lines();
                        }
                        EngineCommand::StopTransport => {
                            if command_debug_enabled() {
                                eprintln!("[SphereAudio callback] StopTransport");
                            }
                            playing_local = false;
                            metronome.stop_tail_samples =
                                crate::backend::render::post_stop_tail_samples(runtime.sample_rate);
                            shared.playing.store(false, Ordering::Relaxed);
                            // Release held notes so nothing is left stuck.
                            runtime.all_notes_off("stop");
                        }
                        EngineCommand::Seek { position_seconds } => {
                            let sr_local = shared.sample_rate.load(Ordering::Relaxed) as f64;
                            let pos = (position_seconds * sr_local) as u64;
                            if command_debug_enabled() {
                                eprintln!(
                                    "[SphereAudio callback] Seek -> {:.3}s ({}sa)",
                                    position_seconds, pos
                                );
                            }
                            shared.position_samples.store(pos, Ordering::Relaxed);
                            metronome.reset_metronome_schedule(pos, output_sample_rate);
                            // Re-seek MIDI cursors + flush held notes.
                            runtime.reset_midi_playback(pos);
                            // A seek repositions the playhead; the PDC delay rings
                            // still hold audio from the pre-seek position. Clear them
                            // so the compensated tracks refill from the new position
                            // and stay aligned with plugin/VSTi-latency tracks (same
                            // reset the DAUx backend and export already do).
                            runtime.reset_pdc_delay_lines();
                        }
                        EngineCommand::SetMetronomeEnabled(enabled) => {
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            shared
                                .metronome_enabled
                                .store(enabled, Ordering::Relaxed);
                            metronome.set_metronome_enabled(enabled, pos, output_sample_rate);
                        }
                        EngineCommand::SetMetronomeSuspended(suspended) => {
                            metronome.set_metronome_suspended(suspended);
                        }
                        EngineCommand::SetBpm(bpm) => {
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            transport::store_f64_bits(&shared.bpm_bits, bpm);
                            let map = crate::tempo_map::RuntimeTempoMapSnapshot::static_tempo(bpm);
                            metronome.set_tempo_map(map.clone(), pos, output_sample_rate);
                            let next_pos = runtime.apply_tempo_map(map, pos);
                            shared.position_samples.store(next_pos, Ordering::Relaxed);
                        }
                        EngineCommand::SetTempoMap(map) => {
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            metronome.set_tempo_map(map.clone(), pos, output_sample_rate);
                            let next_pos = runtime.apply_tempo_map(map, pos);
                            shared.position_samples.store(next_pos, Ordering::Relaxed);
                        }
                        EngineCommand::SetTimeSignature(num, den) => {
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            shared.time_sig_num.store(num.max(1), Ordering::Relaxed);
                            shared.time_sig_den.store(den.max(1), Ordering::Relaxed);
                            metronome.set_time_signature(num, den, pos, output_sample_rate);
                        }
                        EngineCommand::SetTimeSignatureMap(map) => {
                            let pos = shared.position_samples.load(Ordering::Relaxed);
                            if let Some(pt) = map.points().first() {
                                shared
                                    .time_sig_num
                                    .store(pt.numerator.max(1) as u32, Ordering::Relaxed);
                                shared
                                    .time_sig_den
                                    .store(pt.denominator.max(1) as u32, Ordering::Relaxed);
                            }
                            metronome.set_time_signature_map(map, pos, output_sample_rate);
                        }
                        EngineCommand::SetLoop {
                            enabled,
                            start_seconds,
                            end_seconds,
                        } => {
                            let sr_local = shared.sample_rate.load(Ordering::Relaxed) as f64;
                            let start = (start_seconds.max(0.0) * sr_local) as u64;
                            let end = (end_seconds.max(0.0) * sr_local) as u64;
                            shared.loop_enabled.store(enabled, Ordering::Relaxed);
                            shared
                                .loop_start_samples
                                .store(start, Ordering::Relaxed);
                            shared.loop_end_samples.store(end, Ordering::Relaxed);
                        }
                        EngineCommand::SetMasterVolume { value } => {
                            shared
                                .master_volume
                                .store(f32_store(value), Ordering::Relaxed);
                        }
                        EngineCommand::SetPluginBridgeSink { insert_id, sink } => {
                            match sink {
                                Some(sink) => {
                                    runtime.plugin_bridge_sinks.insert(insert_id, sink);
                                }
                                None => {
                                    runtime.plugin_bridge_sinks.remove(&insert_id);
                                }
                            }
                            // Re-cache per-insert sink handles for the block path.
                            runtime.resolve_bridge_sinks();
                        }
                        EngineCommand::CommandBarrier { ack } => {
                            // Wait-free ack: every command sent before this one
                            // has now been applied to the callback's runtime.
                            ack.store(true, Ordering::Release);
                        }
                        EngineCommand::SetBridgeEditorActive { track_id, active } => {
                            runtime.set_bridge_editor_active(&track_id, active);
                            if !active {
                                // The plugin editor's VSTi keyboard lives inside the bridged host,
                                // not the engine preview tracker. Closing the editor must keep the
                                // stopped-transport graph alive long enough to drain note-off/release.
                                metronome.preview_tail_samples = metronome
                                    .preview_tail_samples
                                    .max(crate::backend::render::post_stop_tail_samples(
                                        runtime.sample_rate,
                                    ));
                            }
                        }
                        EngineCommand::SetTrackVolume { track_id, value } => {
                            runtime.update_track_volume(&track_id, value);
                        }
                        EngineCommand::SetTrackPan { track_id, value } => {
                            runtime.update_track_pan(&track_id, value);
                        }
                        EngineCommand::SetTrackMute { track_id, muted } => {
                            if callback_debug_enabled() {
                                eprintln!("[SphereAudio callback] SetTrackMute track={track_id} muted={muted}");
                            }
                            // Scoped note-off: only tracks that are inaudible
                            // after the toggle release their notes. The old
                            // global all_notes_off cut every sounding voice on
                            // any mute toggle — an audible stutter by itself.
                            runtime.update_track_mute(&track_id, muted);
                            runtime.notes_off_for_inaudible_tracks("track_mute");
                        }
                        EngineCommand::SetTrackSolo { track_id, solo } => {
                            runtime.update_track_solo(&track_id, solo);
                            runtime.notes_off_for_inaudible_tracks("track_solo");
                        }
                        EngineCommand::SetTrackInputState {
                            track_index,
                            record_armed,
                            monitor_enabled,
                            input_source,
                        } => runtime.update_track_input_state(
                            track_index,
                            record_armed,
                            monitor_enabled,
                            input_source,
                        ),
                        EngineCommand::SetTrackPreviewMode { track_id, value } => {
                            runtime.update_track_preview_mode(&track_id, RuntimePreviewMode::from_code(value));
                        }
                        EngineCommand::SetInsertParam { track_id, insert_id, param_id, value } => {
                            runtime.update_insert_param(&track_id, &insert_id, &param_id, value);
                        }
                        EngineCommand::MidiPreviewNoteOn {
                            track_id,
                            channel,
                            pitch,
                            velocity,
                        } => {
                            runtime.midi_preview_note_on(&track_id, channel, pitch, velocity);
                        }
                        EngineCommand::MidiPreviewNoteOff {
                            track_id,
                            channel,
                            pitch,
                        } => {
                            runtime.midi_preview_note_off(&track_id, channel, pitch);
                        }
                        EngineCommand::MidiPreviewControlChange {
                            track_id,
                            channel,
                            controller,
                            value,
                        } => {
                            runtime.midi_preview_control_change(
                                &track_id,
                                channel,
                                controller,
                                value,
                            );
                        }
                        EngineCommand::MidiPreviewAllNotesOff { track_id } => {
                            runtime.midi_preview_all_notes_off(&track_id);
                        }
                        EngineCommand::PluginPreviewNoteOn {
                            track_id,
                            plugin_instance_id,
                            channel,
                            pitch,
                            velocity,
                        } => {
                            runtime.bridge_preview_note_on(
                                &track_id,
                                &plugin_instance_id,
                                channel,
                                pitch,
                                velocity,
                            );
                        }
                        EngineCommand::PluginPreviewNoteOff {
                            track_id,
                            plugin_instance_id,
                            channel,
                            pitch,
                        } => {
                            runtime.bridge_preview_note_off(
                                &track_id,
                                &plugin_instance_id,
                                channel,
                                pitch,
                            );
                        }
                        EngineCommand::PluginPreviewControlChange {
                            track_id,
                            plugin_instance_id,
                            channel,
                            controller,
                            value,
                        } => {
                            runtime.bridge_preview_control_change(
                                &track_id,
                                &plugin_instance_id,
                                channel,
                                controller,
                                value,
                            );
                        }
                        EngineCommand::PluginPreviewAllNotesOff {
                            track_id,
                            plugin_instance_id,
                        } => {
                            runtime.bridge_preview_all_notes_off(&track_id, &plugin_instance_id);
                        }
                    }
                }

                // ── 2. Read shared control state (Relaxed OK for audio) ──────
                let master_vol = f32_load(shared.master_volume.load(Ordering::Relaxed));
                // Re-sync local osc flags from atomics in case they changed externally.
                let tone_on = shared.test_tone_enabled.load(Ordering::Relaxed);
                let tone_freq = f32_load(shared.test_tone_freq.load(Ordering::Relaxed));
                if tone_freq != osc_freq {
                    osc_freq = tone_freq;
                    osc_l.set_frequency(tone_freq as f64);
                    osc_r.set_frequency(tone_freq as f64);
                }
                let gen_tone = tone_on || osc_on;

                // ── 3. Fill output buffer ────────────────────────────────────
                let mut peak_l = 0.0f32;
                let mut peak_r = 0.0f32;
                let mut sum_sq_l = 0.0f32;
                let mut sum_sq_r = 0.0f32;
                let mut frames = 0u64;
                let loop_bounds = if playing_local {
                    transport::active_loop_bounds(&shared)
                } else {
                    None
                };
                let raw_base_sample = shared.position_samples.load(Ordering::Relaxed);
                let base_sample = transport::normalize_loop_position(raw_base_sample, loop_bounds);
                if base_sample != raw_base_sample {
                    shared.position_samples.store(base_sample, Ordering::Relaxed);
                    runtime.reset_midi_playback(base_sample);
                    metronome.reset_metronome_schedule(base_sample, output_sample_rate);
                }
                runtime.begin_meter_block();

                // MIDI scheduling — once per block when playing.
                let mut end_loop_midi_reset = None;
                if playing_local && ch > 0 {
                    let frames_needed = data.len().checked_div(ch).unwrap_or(0) as u64;
                    end_loop_midi_reset = schedule_midi_render_block(
                        &mut runtime,
                        base_sample,
                        frames_needed,
                        loop_bounds,
                    );
                }

                let pending_midi = ch > 0
                    && runtime
                        .tracks
                        .iter()
                        .any(|t| !t.midi_block_events.is_empty());
                let frames_in_block = data.len().checked_div(ch).unwrap_or(0) as u64;
                let has_preview = runtime.has_active_midi_preview();
                if playing_local {
                    metronome.preview_tail_samples = 0;
                    metronome.stop_tail_samples = 0;
                    // Playing blocks drive the bridge anyway — flush is implicit.
                    runtime.bridge_panic_flush_samples = 0;
                } else if has_preview || pending_midi {
                    // Keep release-tail processing queued past the note-off so a
                    // stopped-transport preview doesn't cut the instrument dead.
                    metronome.preview_tail_samples =
                        crate::backend::render::post_stop_tail_samples(runtime.sample_rate);
                }
                // Post-panic flush: keep the bridge handshake alive after a
                // stop/seek/mute panic so the host drains the panic CCs instead
                // of leaving VSTi voices stuck until the next play.
                let panic_flush = runtime.bridge_panic_flush_samples > 0;
                if !playing_local && panic_flush {
                    runtime.bridge_panic_flush_samples = runtime
                        .bridge_panic_flush_samples
                        .saturating_sub(frames_in_block);
                }
                let bridge_preview_tail = runtime.bridge_preview_tail_samples > 0;
                if !playing_local && bridge_preview_tail {
                    runtime.bridge_preview_tail_samples = runtime
                        .bridge_preview_tail_samples
                        .saturating_sub(frames_in_block);
                }
                let bridge_editor_wakeup = runtime.has_bridge_editor_active();
                let audition_active = audition.is_some();
                let preview_render_active = has_preview
                    || pending_midi
                    || panic_flush
                    || bridge_preview_tail
                    || metronome.preview_tail_samples > 0
                    || metronome.stop_tail_samples > 0
                    || bridge_editor_wakeup
                    || audition_active;
                if preview_render_active
                    && !playing_local
                    && (has_preview || pending_midi || metronome.preview_tail_samples > 0)
                {
                    let active_notes: usize = runtime
                        .midi_tracks
                        .iter()
                        .map(|mt| mt.preview_active.len())
                        .sum();
                    let active_u32 = active_notes as u32;
                    let changed = active_u32 != metronome.prev_logged_preview_notes;
                    if changed {
                        eprintln!(
                            "[PreviewRenderWake] active_preview_notes changed {} -> {} tail_samples={}",
                            metronome.prev_logged_preview_notes,
                            active_u32,
                            metronome.preview_tail_samples
                        );
                        metronome.prev_logged_preview_notes = active_u32;
                        metronome.preview_wake_log_cooldown = 0;
                    } else if active_notes > 0 {
                        metronome.preview_wake_log_cooldown =
                            metronome.preview_wake_log_cooldown.saturating_add(1);
                        let sr = runtime.sample_rate.max(1);
                        let log_interval_blocks =
                            (sr / frames_in_block.max(1) as u32).max(1);
                        if metronome.preview_wake_log_cooldown >= log_interval_blocks {
                            metronome.preview_wake_log_cooldown = 0;
                            eprintln!(
                                "[PreviewRenderWake] active_preview_notes={} tail_samples={} rendering_while_stopped=true",
                                active_notes, metronome.preview_tail_samples
                            );
                        }
                    }
                    if !has_preview && !pending_midi {
                        metronome.preview_tail_samples =
                            metronome.preview_tail_samples.saturating_sub(frames_in_block);
                        if metronome.preview_tail_samples == 0 {
                            metronome.prev_logged_preview_notes = u32::MAX;
                        }
                    }
                }
                if !playing_local && metronome.stop_tail_samples > 0 {
                    metronome.stop_tail_samples =
                        metronome.stop_tail_samples.saturating_sub(frames_in_block);
                }

                if ch >= 2 && (playing_local || preview_render_active) {
                    let frames_needed = data.len() / ch;
                    let scratch_len = frames_needed * ch;
                    if block_scratch.len() < scratch_len {
                        block_scratch.resize(scratch_len, 0.0);
                    }
                    let scratch = &mut block_scratch[..scratch_len];
                    frames = render_project_block_interleaved(
                        &mut runtime,
                        base_sample,
                        master_vol,
                        scratch,
                        ch,
                        playing_local,
                        shared.time_sig_num.load(Ordering::Relaxed),
                        shared.time_sig_den.load(Ordering::Relaxed),
                        loop_bounds,
                    );
                    let audition_finished = audition
                        .as_mut()
                        .map(|audition| audition.mix_into(scratch, ch, runtime.sample_rate))
                        .unwrap_or(false);
                    if audition_finished {
                        if let Some(audition) = audition.take() {
                            crate::graveyard::retire_audio_file(audition.into_source());
                        }
                    }
                    if !render_path_logged {
                        render_path_logged = true;
                        if callback_debug_enabled() {
                            eprintln!(
                                "[SphereAudio callback] renderPath=legacy-block frames={} channels={} tracks={}",
                                frames,
                                ch,
                                runtime.tracks.len()
                            );
                        }
                    }
                    if gen_tone {
                        for frame in scratch.chunks_mut(ch) {
                            let tone_l = osc_l.next_sample() * TEST_TONE_AMPLITUDE * master_vol;
                            let tone_r = osc_r.next_sample() * TEST_TONE_AMPLITUDE * master_vol;
                            frame[0] = (frame[0] + tone_l).clamp(-1.0, 1.0);
                            frame[1] = (frame[1] + tone_r).clamp(-1.0, 1.0);
                        }
                    }
                    let metronome_graph_max_samples =
                        crate::backend::render::metronome_graph_max_latency_samples(&runtime);
                    let metronome_delay_samples =
                        crate::backend::render::metronome_compensation_delay_samples(&runtime);
                    let mut segment_sample = base_sample;
                    let mut callback_offset = 0usize;
                    let mut remaining = frames;
                    while remaining > 0 {
                        let segment_frames = transport::segment_frames_until_loop_wrap(
                            segment_sample,
                            remaining,
                            loop_bounds,
                        );
                        for i in 0..segment_frames as usize {
                            let frame = &mut scratch[(callback_offset + i) * ch
                                ..(callback_offset + i) * ch + ch];
                            let click = metronome.metronome_sample(
                                segment_sample + i as u64,
                                (callback_offset + i) as u64,
                                output_sample_rate,
                                playing_local,
                                metronome_graph_max_samples,
                                metronome_delay_samples,
                            );
                            if click != 0.0 {
                                frame[0] = (frame[0] + click * master_vol).clamp(-1.0, 1.0);
                                frame[1] = (frame[1] + click * master_vol).clamp(-1.0, 1.0);
                            }
                        }
                        callback_offset += segment_frames as usize;
                        remaining -= segment_frames;
                        if remaining == 0 {
                            break;
                        }
                        let (next_sample, wrapped) = transport::advance_loop_position(
                            segment_sample,
                            segment_frames,
                            loop_bounds,
                        );
                        if wrapped {
                            metronome.reset_metronome_schedule(next_sample, output_sample_rate);
                        }
                        segment_sample = next_sample;
                    }
                    // (Legacy NAPI path) Software monitoring is handled by the
                    // DAUx/cpal render kernel via the input ring; the old
                    // sample-and-hold monitor was removed (warble).
                    for (out_frame, frame) in data.chunks_mut(ch).zip(scratch.chunks(ch)) {
                        let l = frame[0].clamp(-1.0, 1.0);
                        let r = frame[1].clamp(-1.0, 1.0);
                        out_frame[0] = T::from_sample(l);
                        out_frame[1] = T::from_sample(r);
                        for extra in out_frame.iter_mut().skip(2) {
                            *extra = T::from_sample(0.0);
                        }
                        peak_l = peak_l.max(l.abs());
                        peak_r = peak_r.max(r.abs());
                        sum_sq_l += l * l;
                        sum_sq_r += r * r;
                    }
                    if !playing_local
                        && runtime.bridge_preview_tail_samples > 0
                        && peak_l.max(peak_r) > 0.00001
                    {
                        runtime.bridge_preview_tail_samples =
                            crate::backend::render::post_stop_tail_samples(runtime.sample_rate);
                    }
                } else if ch >= 2 {
                    let metronome_graph_max_samples =
                        crate::backend::render::metronome_graph_max_latency_samples(&runtime);
                    let metronome_delay_samples =
                        crate::backend::render::metronome_compensation_delay_samples(&runtime);
                    for frame in data.chunks_mut(ch) {
                        let (tone_l, tone_r) = if gen_tone {
                            (
                                osc_l.next_sample() * TEST_TONE_AMPLITUDE * master_vol,
                                osc_r.next_sample() * TEST_TONE_AMPLITUDE * master_vol,
                            )
                        } else {
                            (0.0, 0.0)
                        };
                        let (project_l, project_r) = if playing_local {
                            render_project_sample(&mut runtime, base_sample + frames, master_vol)
                        } else {
                            (0.0, 0.0)
                        };
                        let click = metronome.metronome_sample(
                            base_sample + frames,
                            frames,
                            output_sample_rate,
                            playing_local,
                            metronome_graph_max_samples,
                            metronome_delay_samples,
                        ) * master_vol;
                        let l = (tone_l + project_l + click).clamp(-1.0, 1.0);
                        let r = (tone_r + project_r + click).clamp(-1.0, 1.0);
                        frame[0] = T::from_sample(l);
                        frame[1] = T::from_sample(r);
                        // Extra channels get silence.
                        for extra in frame.iter_mut().skip(2) {
                            *extra = T::from_sample(0.0);
                        }
                        peak_l = peak_l.max(l.abs());
                        peak_r = peak_r.max(r.abs());
                        sum_sq_l += l * l;
                        sum_sq_r += r * r;
                        frames += 1;
                    }
                } else if ch == 1 {
                    let metronome_graph_max_samples =
                        crate::backend::render::metronome_graph_max_latency_samples(&runtime);
                    let metronome_delay_samples =
                        crate::backend::render::metronome_compensation_delay_samples(&runtime);
                    for sample in data.iter_mut() {
                        let tone = if gen_tone {
                            osc_l.next_sample() * TEST_TONE_AMPLITUDE * master_vol
                        } else {
                            0.0
                        };
                        let (project_l, project_r) = if playing_local {
                            render_project_sample(&mut runtime, base_sample + frames, master_vol)
                        } else {
                            (0.0, 0.0)
                        };
                        let click = metronome.metronome_sample(
                            base_sample + frames,
                            frames,
                            output_sample_rate,
                            playing_local,
                            metronome_graph_max_samples,
                            metronome_delay_samples,
                        ) * master_vol;
                        let value =
                            (tone + (project_l + project_r) * 0.5 + click).clamp(-1.0, 1.0);
                        *sample = T::from_sample(value);
                        peak_l = peak_l.max(value.abs());
                        sum_sq_l += value * value;
                        frames += 1;
                    }
                }

                if shared.live_input_active.load(Ordering::Relaxed) {
                    let input_l = f32_load(shared.live_input_l.load(Ordering::Relaxed));
                    let input_r = f32_load(shared.live_input_r.load(Ordering::Relaxed));
                    let source_pair = shared.monitor_source_pair();
                    runtime.accumulate_live_input_meters(input_l, input_r, source_pair);
                }

                // ── 4. Compute meters (no allocation) ────────────────────────
                let rms_l = if frames > 0 {
                    (sum_sq_l / frames as f32).sqrt()
                } else {
                    0.0
                };
                let (pk_r, rms_r) = if ch >= 2 {
                    (
                        peak_r,
                        if frames > 0 {
                            (sum_sq_r / frames as f32).sqrt()
                        } else {
                            0.0
                        },
                    )
                } else {
                    (peak_l, rms_l)
                };
                runtime.end_meter_block(frames);

                prev_peak_l = smooth_peak(prev_peak_l, peak_l, PEAK_DECAY);
                prev_peak_r = smooth_peak(prev_peak_r, pk_r, PEAK_DECAY);

                shared
                    .peak_l
                    .store(f32_store(prev_peak_l), Ordering::Relaxed);
                shared
                    .peak_r
                    .store(f32_store(prev_peak_r), Ordering::Relaxed);
                shared.rms_l.store(f32_store(rms_l), Ordering::Relaxed);
                shared.rms_r.store(f32_store(rms_r), Ordering::Relaxed);

                // Advance position counter.
                if playing_local && ch > 0 {
                    let (next_position, _) =
                        transport::advance_loop_position(base_sample, frames, loop_bounds);
                    shared.position_samples.store(next_position, Ordering::Relaxed);
                    if let Some(reset_sample) = end_loop_midi_reset {
                        runtime.reset_midi_playback(reset_sample);
                        metronome.reset_metronome_schedule(reset_sample, output_sample_rate);
                    }
                }

                // ── Dropout watchdog: publish timing + classify this block ────
                shared.output_cb_count.fetch_add(1, Ordering::Relaxed);
                let elapsed_us = cb_started.elapsed().as_micros().min(u32::MAX as u128) as u32;
                let block_frames = data.len().checked_div(ch).unwrap_or(0);
                crate::engine::record_output_callback_timing(
                    &shared,
                    elapsed_us,
                    block_frames,
                    output_sample_rate,
                );
            },
            move |err| {
                eprintln!("[SphereAudio] Stream error: {err}");
            },
            None,
        )
        .map_err(|e| e.to_string())?;

    Ok(stream)
}

#[cfg(test)]
mod input_route_tests {
    use super::{monitor_channel_pair, routed_input_peaks, EngineInner, SharedState};
    use std::sync::atomic::Ordering;

    #[test]
    fn mono_monitor_route_uses_one_channel_for_both_sides() {
        assert_eq!(monitor_channel_pair(&[5]), Some((5, 5)));
    }

    #[test]
    fn mono_input_meter_duplicates_selected_channel_peak() {
        assert_eq!(routed_input_peaks(&[0.1, 0.4, 0.2], &[1]), Some((0.4, 0.4)));
    }

    #[test]
    fn monitor_source_pair_publishes_both_channels_together() {
        let shared = SharedState::default();
        shared.set_monitor_source_pair(7, 3);
        assert_eq!(shared.monitor_source_pair(), (7, 3));
    }

    #[test]
    fn monitor_shared_clock_defaults_false() {
        assert!(!SharedState::default()
            .monitor_shared_clock
            .load(Ordering::Relaxed));
    }

    #[test]
    fn stopping_live_input_clears_shared_clock() {
        let engine = EngineInner::new();
        engine
            .shared
            .monitor_shared_clock
            .store(true, Ordering::Relaxed);

        engine.stop_live_input_stream();

        assert!(!engine.shared.monitor_shared_clock.load(Ordering::Relaxed));
    }
}

#[cfg(test)]
mod live_input_tests {
    use super::*;
    use crate::types::{EngineRoutingSnapshot, EngineTrackSnapshot};

    fn monitored_audio_snapshot() -> EngineProjectSnapshot {
        EngineProjectSnapshot {
            project_id: "asio-routing-test".into(),
            project_root: None,
            preferred_input_device: None,
            bpm: 120.0,
            tempo_points: Vec::new(),
            time_signature: [4, 4],
            sample_rate: 48_000,
            tracks: vec![EngineTrackSnapshot {
                id: "audio-1".into(),
                track_type: "audio".into(),
                volume: 1.0,
                pan: 0.0,
                muted: false,
                solo: false,
                armed: true,
                input_monitor: true,
                input_source: EngineTrackInputSourceSnapshot {
                    device_id: Some("test-asio-driver".into()),
                    channels: vec![0, 1],
                },
                preview_mode: "stereo".into(),
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
            }],
            clips: Vec::new(),
            midi_clips: Vec::new(),
            pdc_enabled: true,
            latency_graph_version: 0,
            routing: EngineRoutingSnapshot {
                master_output_device: None,
                sample_rate: 48_000,
                buffer_size: 256,
            },
        }
    }

    #[test]
    fn asio_input_routing_waits_for_duplex_session() {
        let engine = EngineInner::new();
        engine.daux_config.lock().backend = BackendKind::Asio;

        let result = engine.sync_live_input_stream(&monitored_audio_snapshot());

        assert!(result.is_ok());
        assert!(engine.live_input.lock().is_none());
        assert!(!engine.shared.input_ring.is_active());
    }
}

#[cfg(test)]
mod clip_fade_tests {
    use super::clip_fade_gain;

    #[test]
    fn no_fades_is_unity() {
        assert_eq!(clip_fade_gain(0, 1000, 0, 0), 1.0);
        assert_eq!(clip_fade_gain(500, 1000, 0, 0), 1.0);
        assert_eq!(clip_fade_gain(999, 1000, 0, 0), 1.0);
    }

    #[test]
    fn fade_in_ramps_zero_to_one() {
        // 100-sample fade-in over a 1000-sample clip.
        assert_eq!(clip_fade_gain(0, 1000, 100, 0), 0.0);
        assert!((clip_fade_gain(50, 1000, 100, 0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        // At/after the fade-in length it is full gain.
        assert_eq!(clip_fade_gain(100, 1000, 100, 0), 1.0);
        assert_eq!(clip_fade_gain(900, 1000, 100, 0), 1.0);
    }

    #[test]
    fn fade_out_ramps_one_to_zero() {
        // 100-sample fade-out: starts at sample 900 (duration - fade_out).
        assert_eq!(clip_fade_gain(899, 1000, 0, 100), 1.0);
        assert!((clip_fade_gain(900, 1000, 0, 100) - 1.0).abs() < 1e-6);
        assert!((clip_fade_gain(950, 1000, 0, 100) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!(clip_fade_gain(1000, 1000, 0, 100) <= 0.0);
    }

    #[test]
    fn fade_in_and_out_combine() {
        // In the flat middle region both fades are unity.
        assert!((clip_fade_gain(500, 1000, 100, 100) - 1.0).abs() < 1e-6);
        // Inside the fade-in region only the fade-in shapes the gain.
        let expected = (0.25f32 * std::f32::consts::FRAC_PI_2).sin();
        assert!((clip_fade_gain(25, 1000, 100, 100) - expected).abs() < 1e-6);
    }
}

#[cfg(test)]
mod sample_rate_mismatch_tests {
    use super::{log_sample_rate_decision, sample_rate_mismatch, sample_rate_mode_label};
    use crate::backend::BackendKind;

    #[test]
    fn shared_mode_requested_48k_active_96k_is_a_mismatch() {
        // The exact reported scenario: Preferences requested 48000, but the
        // device opened at 96000 (WASAPI shared / device default).
        assert_eq!(sample_rate_mismatch(48_000, 96_000), Some((48_000, 96_000)));
    }

    #[test]
    fn matching_rates_report_no_mismatch() {
        assert_eq!(sample_rate_mismatch(48_000, 48_000), None);
        assert_eq!(sample_rate_mismatch(96_000, 96_000), None);
    }

    #[test]
    fn zero_requested_means_device_default_no_mismatch() {
        // requested == 0 ("use device default") is never a mismatch, whatever
        // the device opened at.
        assert_eq!(sample_rate_mismatch(0, 96_000), None);
        assert_eq!(sample_rate_mismatch(48_000, 0), None);
    }

    #[test]
    fn shared_mismatch_is_visible_but_not_an_error() {
        // Shared/Auto mismatch is allowed (Windows resamples) — logged, but no
        // error string is returned for the UI's error channel.
        assert_eq!(
            log_sample_rate_decision(48_000, 96_000, &BackendKind::WasapiShared),
            None
        );
        assert_eq!(
            log_sample_rate_decision(48_000, 96_000, &BackendKind::Auto),
            None
        );
    }

    #[test]
    fn exclusive_mismatch_returns_a_visible_warning() {
        // Exclusive mode could not honor the rate and fell back — surfaced as a
        // warning string the caller stores in last_daux_error.
        let warning = log_sample_rate_decision(48_000, 96_000, &BackendKind::WasapiExclusive)
            .expect("exclusive fallback must surface a warning");
        assert!(warning.contains("48000"));
        assert!(warning.contains("96000"));
    }

    #[test]
    fn exclusive_without_mismatch_has_no_warning() {
        assert_eq!(
            log_sample_rate_decision(96_000, 96_000, &BackendKind::WasapiExclusive),
            None
        );
    }

    #[test]
    fn mode_labels_are_stable() {
        assert_eq!(
            sample_rate_mode_label(&BackendKind::WasapiShared),
            "WASAPI_SHARED"
        );
        assert_eq!(
            sample_rate_mode_label(&BackendKind::WasapiExclusive),
            "WASAPI_EXCLUSIVE"
        );
    }
}

#[cfg(test)]
mod clip_stretch_dsp_tests {
    use super::{clip_source_pos_seconds, sample_clip_processor_stereo, signalsmith_input_span};
    use crate::audio_file::AudioFileBuffer;
    use crate::audio_source::ClipAudioSource;
    use crate::runtime::{resolve_clip_processor, ClipDspProcessor};
    use std::sync::Arc;

    /// Source sample index a given in-clip output offset reads from, with the
    /// source/output sample rates matched at 48 kHz.
    fn src_sample(rel: u64, duration: u64, speed: f32, reverse: bool) -> f64 {
        clip_source_pos_seconds(0.0, rel, duration, 48_000, speed, reverse) * 48_000.0
    }

    #[test]
    fn manual_stretch_maps_output_to_source_samples() {
        // ratio 2.0 → speed 0.5; a 96k-output clip consumes the 48k source.
        assert!((src_sample(0, 96_000, 0.5, false) - 0.0).abs() < 1e-6);
        assert!((src_sample(24_000, 96_000, 0.5, false) - 12_000.0).abs() < 1e-6);
        assert!((src_sample(48_000, 96_000, 0.5, false) - 24_000.0).abs() < 1e-6);
        assert!((src_sample(95_999, 96_000, 0.5, false) - 47_999.5).abs() < 1e-3);
    }

    #[test]
    fn resample_mode_changes_source_read_rate() {
        // speed 1.0 advances 1:1; 2.0 twice as fast; 0.5 half as fast.
        assert!((src_sample(100, 48_000, 1.0, false) - 100.0).abs() < 1e-6);
        assert!((src_sample(100, 48_000, 2.0, false) - 200.0).abs() < 1e-6);
        assert!((src_sample(100, 48_000, 0.5, false) - 50.0).abs() < 1e-6);
    }

    #[test]
    fn reverse_mapping_reads_from_source_end_backwards() {
        // speed 1.0, 48k clip: output 0 → last source frame; last output → start.
        assert!((src_sample(0, 48_000, 1.0, true) - 47_999.0).abs() < 1e-6);
        assert!((src_sample(47_999, 48_000, 1.0, true) - 0.0).abs() < 1e-6);
        // Reverse reads strictly decreasing source positions.
        assert!(src_sample(0, 48_000, 1.0, true) > src_sample(1, 48_000, 1.0, true));
    }

    #[test]
    fn processor_selection_routes_by_mode_and_preserve() {
        assert_eq!(
            resolve_clip_processor("off", false),
            ClipDspProcessor::NoStretch
        );
        assert_eq!(
            resolve_clip_processor("resample", true),
            ClipDspProcessor::Resample
        );
        assert_eq!(
            resolve_clip_processor("manual", false),
            ClipDspProcessor::Resample
        );
        assert_eq!(
            resolve_clip_processor("manual", true),
            ClipDspProcessor::PhaseVocoderBasic
        );
        assert_eq!(
            resolve_clip_processor("temposync", true),
            ClipDspProcessor::PhaseVocoderBasic
        );
        assert_eq!(
            resolve_clip_processor("warp", false),
            ClipDspProcessor::PhaseVocoderBasic
        );
    }

    #[test]
    fn signalsmith_feed_tiles_source_without_gap_or_overlap() {
        // Across the whole clip, consecutive block input spans must tile the
        // source contiguously and total to floor(duration / ratio) — i.e. the
        // source length — so the stretcher never over-reads or backs up.
        for &(duration, ratio) in &[
            (96_000u64, 2.0_f64), // slow down 2× (source 48k)
            (24_000u64, 0.5_f64), // speed up 2× (source 48k)
            (50_000u64, 1.37_f64),
            (50_000u64, 0.73_f64),
        ] {
            let frames = 512usize;
            let mut rel = 0u64;
            let mut prev_end: Option<i64> = None;
            let mut total: i64 = 0;
            while rel < duration {
                let block = frames.min((duration - rel) as usize);
                let (in_start, input_frames) = signalsmith_input_span(rel, block, ratio);
                if let Some(end) = prev_end {
                    assert_eq!(in_start, end, "blocks must tile with no gap/overlap");
                }
                prev_end = Some(in_start + input_frames as i64);
                total += input_frames as i64;
                rel += block as u64;
            }
            let expected = (duration as f64 / ratio).floor() as i64;
            // Allow the final partial block ±1 rounding, but never over-read.
            assert!(
                (total - expected).abs() <= 1,
                "consumed {total} source frames, expected ~{expected} (ratio {ratio})"
            );
        }
    }

    #[test]
    fn preserve_pitch_processor_output_differs_from_resample() {
        let mut samples = Vec::new();
        for i in 0..4096 {
            let v = ((i as f32 * 0.017).sin() * (i as f32 * 0.003).cos()).clamp(-1.0, 1.0);
            samples.push(v);
            samples.push(v);
        }
        let source = ClipAudioSource::InMemory(Arc::new(AudioFileBuffer {
            sample_rate: 48_000,
            channels: 2,
            frames: 4096,
            samples,
        }));
        let resample =
            sample_clip_processor_stereo(&source, 512.0, 256.0, 2.0, ClipDspProcessor::Resample);
        let pv = sample_clip_processor_stereo(
            &source,
            512.0,
            256.0,
            2.0,
            ClipDspProcessor::PhaseVocoderBasic,
        );
        assert!(
            (resample.0 - pv.0).abs() > 1e-5 || (resample.1 - pv.1).abs() > 1e-5,
            "PhaseVocoderBasic should not be identical to Resample"
        );
    }
}

#[cfg(test)]
mod bridge_insert_tests {
    use super::*;
    use crate::plugin_bridge::PluginBridgeSink;
    use crate::runtime::{InsertDspState, RuntimeInsert, RuntimePreviewMode, RuntimeTrack};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[derive(Debug)]
    struct WetEffectSink {
        done_seq: AtomicU64,
        requests: AtomicU64,
        latency_samples: u32,
    }

    impl PluginBridgeSink for WetEffectSink {
        fn dsp_ready(&self) -> bool {
            true
        }

        fn read_output(&self, out_l: &mut [f32], out_r: &mut [f32], frames: usize) -> usize {
            if self.done_seq.load(Ordering::Acquire) == 0 {
                return 0;
            }
            for i in 0..frames.min(out_l.len()).min(out_r.len()) {
                out_l[i] = 0.25;
                out_r[i] = 0.25;
            }
            frames
        }

        fn push_midi(&self, _: u8, _: u8, _: u8, _: u32) {}

        fn write_input(&self, _: &[f32], _: &[f32], _: usize) {}

        fn request_block(&self, _: u32) {
            self.requests.fetch_add(1, Ordering::Relaxed);
            self.done_seq.store(1, Ordering::Release);
        }

        fn reported_latency_samples(&self) -> u32 {
            self.latency_samples
        }
    }

    fn bridge_effect_track(block: f32) -> RuntimeTrack {
        let mut params = HashMap::new();
        params.insert("role".to_string(), serde_json::json!("effect"));
        RuntimeTrack {
            id: "track-1".to_string(),
            track_type: "audio".to_string(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            record_armed: false,
            monitor_enabled: false,
            input_source: crate::runtime::RuntimeTrackInputSource::None,
            preview_mode: RuntimePreviewMode::Stereo,
            output_track_id: None,
            output_track_index: None,
            inserts: vec![RuntimeInsert {
                id: "insert-1".to_string(),
                kind: "external-bridge-plugin".to_string(),
                kind_tag: crate::runtime::RuntimeInsertKind::ExternalBridge,
                enabled: true,
                params,
                bridge_is_effect: true,
                bridge_is_builtin: false,
                bridge_enabled_output_channels: Vec::new(),
                bridge_sink: None,
                dsp: InsertDspState::default(),
                vst3: None,
                callback_process_log_done: false,
                silent_process_blocks: 0,
                bridge_missed_blocks: 0,
                scratch_l: vec![0.0; 8],
                scratch_r: vec![0.0; 8],
                vsti_output_children: Vec::new(),
                scratch_multi: Vec::new(),
            }],
            sends: Vec::new(),
            automation_lanes: Vec::new(),
            plugin_param_automation: Vec::new(),
            meter: Arc::new(Default::default()),
            meter_peak_l: 0.0,
            meter_peak_r: 0.0,
            meter_sum_sq_l: 0.0,
            meter_sum_sq_r: 0.0,
            callback_insert_log_done: false,
            callback_clip_route_log_done: false,
            block_l: vec![block; 8],
            block_r: vec![block; 8],
            recv_l: vec![0.0; 8],
            recv_r: vec![0.0; 8],
            soundfont_l: vec![0.0; 8],
            soundfont_r: vec![0.0; 8],
            midi_block_events: Vec::new(),
            midi_instrument_insert_ix: None,
            soundfont_player: None,
            plugin_latency_samples: 0,
            pdc_delay_l: Vec::new(),
            pdc_delay_r: Vec::new(),
            pdc_write_pos: 0,
            smoothed_gain_l: 1.0,
            smoothed_gain_r: 1.0,
        }
    }

    #[test]
    fn bridged_vst3_and_builtin_effects_process_on_master_when_sink_is_bound() {
        for bridge_is_builtin in [false, true] {
            let mut track = bridge_effect_track(1.0);
            track.track_type = "master".to_string();
            track.inserts[0].bridge_is_builtin = bridge_is_builtin;
            let sink = WetEffectSink {
                done_seq: AtomicU64::new(1),
                requests: AtomicU64::new(0),
                latency_samples: 0,
            };
            track.inserts[0].bridge_sink = Some(std::sync::Arc::new(sink));
            apply_track_chain_block(&mut track, 4, RuntimeTransportContext::default());
            assert!(
                (track.block_l[0] - 0.25).abs() < 1e-6,
                "master bridge format={} left={}",
                if bridge_is_builtin { "BuiltIn" } else { "VST3" },
                track.block_l[0]
            );
            assert!(
                (track.block_r[0] - 0.25).abs() < 1e-6,
                "master bridge format={} right={}",
                if bridge_is_builtin { "BuiltIn" } else { "VST3" },
                track.block_r[0]
            );
        }
    }

    #[derive(Debug, Default)]
    struct ParamCaptureSink {
        last_param_id: AtomicU64,
        last_value_bits: AtomicU64,
        pushes: AtomicU64,
    }

    impl PluginBridgeSink for ParamCaptureSink {
        fn dsp_ready(&self) -> bool {
            true
        }
        fn read_output(&self, _l: &mut [f32], _r: &mut [f32], _frames: usize) -> usize {
            0
        }
        fn push_midi(&self, _: u8, _: u8, _: u8, _: u32) {}
        fn push_param(&self, param_id: u32, value: f32, _sample_offset: u32) {
            self.last_param_id.store(param_id as u64, Ordering::Release);
            self.last_value_bits
                .store(value.to_bits() as u64, Ordering::Release);
            self.pushes.fetch_add(1, Ordering::Release);
        }
        fn write_input(&self, _: &[f32], _: &[f32], _: usize) {}
        fn request_block(&self, _: u32) {}
    }

    fn plugin_param_lane(insert_id: &str, param_id: &str) -> crate::runtime::RuntimeAutomationLane {
        crate::runtime::RuntimeAutomationLane {
            id: "lane-1".to_string(),
            name: "Cutoff".to_string(),
            target: crate::runtime::RuntimeAutomationTarget::PluginParameter {
                insert_id: insert_id.to_string(),
                parameter_id: param_id.to_string(),
            },
            enabled: true,
            points: vec![
                crate::runtime::RuntimeAutomationPoint {
                    beat: 0.0,
                    value: 0.0,
                    curve: crate::runtime::RuntimeAutomationCurve::Linear,
                    tension: 0.0,
                },
                crate::runtime::RuntimeAutomationPoint {
                    beat: 4.0,
                    value: 1.0,
                    curve: crate::runtime::RuntimeAutomationCurve::Linear,
                    tension: 0.0,
                },
            ],
        }
    }

    #[test]
    fn plugin_param_automation_pushes_value_to_bridge_sink_during_playback() {
        let mut track = bridge_effect_track(0.0);
        // Bridge plugin param automation: lane targets insert-1 / param 42.
        track.automation_lanes = vec![plugin_param_lane("insert-1", "42")];
        track.plugin_param_automation = vec![crate::runtime::RuntimePluginParamBinding {
            insert_ix: 0,
            lane_ix: 0,
            param_id: 42,
            last_value: f32::NAN,
        }];
        let sink = std::sync::Arc::new(ParamCaptureSink::default());
        track.inserts[0].bridge_sink = Some(sink.clone());

        // Block at beat 2.0 → linear midpoint between 0.0 and 1.0 = 0.5.
        let transport = RuntimeTransportContext {
            playing: true,
            ppq_position: 2.0,
            ..RuntimeTransportContext::default()
        };
        apply_track_chain_block(&mut track, 4, transport);

        assert_eq!(sink.pushes.load(Ordering::Acquire), 1);
        assert_eq!(sink.last_param_id.load(Ordering::Acquire), 42);
        let pushed = f32::from_bits(sink.last_value_bits.load(Ordering::Acquire) as u32);
        assert!((pushed - 0.5).abs() < 1e-4, "pushed={pushed}");
    }

    #[test]
    fn plugin_param_automation_dedupes_unchanged_value_and_is_idle_when_stopped() {
        let mut track = bridge_effect_track(0.0);
        track.automation_lanes = vec![plugin_param_lane("insert-1", "42")];
        track.plugin_param_automation = vec![crate::runtime::RuntimePluginParamBinding {
            insert_ix: 0,
            lane_ix: 0,
            param_id: 42,
            last_value: f32::NAN,
        }];
        let sink = std::sync::Arc::new(ParamCaptureSink::default());
        track.inserts[0].bridge_sink = Some(sink.clone());

        let transport = RuntimeTransportContext {
            playing: true,
            ppq_position: 2.0,
            ..RuntimeTransportContext::default()
        };
        // Same beat twice → second block must not re-push the identical value.
        apply_track_chain_block(&mut track, 4, transport);
        apply_track_chain_block(&mut track, 4, transport);
        assert_eq!(sink.pushes.load(Ordering::Acquire), 1);

        // Stopped transport must not drive parameter automation at all.
        let stopped = RuntimeTransportContext {
            playing: false,
            ppq_position: 3.5,
            ..RuntimeTransportContext::default()
        };
        apply_track_chain_block(&mut track, 4, stopped);
        assert_eq!(sink.pushes.load(Ordering::Acquire), 1);
    }

    #[test]
    fn bridge_latency_refresh_updates_pdc_delays() {
        let frames = 16usize;
        let mut fast = bridge_effect_track(1.0);
        fast.id = "fast".to_string();
        fast.inserts.clear();
        let mut slow = bridge_effect_track(1.0);
        slow.id = "slow".to_string();
        slow.inserts[0].id = "slow-insert".to_string();
        let tracks = vec![fast, slow];
        let audio_graph = crate::audio_graph::plan_runtime_audio_graph(&tracks).unwrap();
        let mut p = RuntimeProject {
            tracks,
            audio_graph,
            pdc_enabled: true,
            ..Default::default()
        };
        let sink = WetEffectSink {
            done_seq: AtomicU64::new(1),
            requests: AtomicU64::new(0),
            latency_samples: 16,
        };
        p.plugin_bridge_sinks
            .insert("slow-insert".to_string(), Arc::new(sink));
        // The block path reads the cached per-insert sink, exactly like the
        // SetPluginBridgeSink command handler.
        p.resolve_bridge_sinks();

        assert!(p.refresh_runtime_latency_graph(frames as u32));
        assert_eq!(p.latency_graph.track_plugin_latency[0], 0);
        assert_eq!(p.latency_graph.track_plugin_latency[1], 32);
        assert_eq!(p.latency_graph.track_pdc_delay[0], 32);
        assert_eq!(p.latency_graph.track_pdc_delay[1], 0);
        assert!(!p.refresh_runtime_latency_graph(frames as u32));
    }

    #[test]
    fn external_bridge_effect_stays_dry_without_sink() {
        let mut track = bridge_effect_track(1.0);
        apply_track_chain_block(&mut track, 4, RuntimeTransportContext::default());
        assert!((track.block_l[0] - 1.0).abs() < 1e-6);
        assert!((track.block_r[0] - 1.0).abs() < 1e-6);
    }

    fn bridge_effect_track_with_id(id: &str) -> RuntimeInsert {
        let mut params = HashMap::new();
        params.insert("role".to_string(), serde_json::json!("effect"));
        RuntimeInsert {
            id: id.to_string(),
            kind: "external-bridge-plugin".to_string(),
            kind_tag: crate::runtime::RuntimeInsertKind::ExternalBridge,
            enabled: true,
            params,
            bridge_is_effect: true,
            bridge_is_builtin: false,
            bridge_enabled_output_channels: Vec::new(),
            bridge_sink: None,
            dsp: InsertDspState::default(),
            vst3: None,
            callback_process_log_done: false,
            silent_process_blocks: 0,
            bridge_missed_blocks: 0,
            scratch_l: vec![0.0; 8],
            scratch_r: vec![0.0; 8],
            vsti_output_children: Vec::new(),
            scratch_multi: Vec::new(),
        }
    }

    #[test]
    fn serial_bridge_effect_chain_applies_both_gains() {
        #[derive(Debug)]
        struct MultSink {
            mult: f32,
            done: AtomicU64,
            buf_l: std::sync::Mutex<Vec<f32>>,
            buf_r: std::sync::Mutex<Vec<f32>>,
        }

        impl PluginBridgeSink for MultSink {
            fn dsp_ready(&self) -> bool {
                true
            }
            fn read_output(&self, out_l: &mut [f32], out_r: &mut [f32], frames: usize) -> usize {
                let done = self.done.load(Ordering::Acquire);
                if done == 0 {
                    return 0;
                }
                self.done.store(0, Ordering::Release);
                let bl = self.buf_l.lock().unwrap();
                let br = self.buf_r.lock().unwrap();
                let n = frames.min(out_l.len()).min(out_r.len()).min(bl.len());
                for i in 0..n {
                    out_l[i] = bl[i] * self.mult;
                    out_r[i] = br[i] * self.mult;
                }
                n
            }
            fn push_midi(&self, _: u8, _: u8, _: u8, _: u32) {}
            fn write_input(&self, in_l: &[f32], in_r: &[f32], frames: usize) {
                let n = frames.min(in_l.len()).min(in_r.len());
                *self.buf_l.lock().unwrap() = in_l[..n].to_vec();
                *self.buf_r.lock().unwrap() = in_r[..n].to_vec();
            }
            fn request_block(&self, _: u32) {
                self.done.store(1, Ordering::Release);
            }
        }

        let mut track = RuntimeTrack {
            id: "track-1".to_string(),
            track_type: "audio".to_string(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            record_armed: false,
            monitor_enabled: false,
            input_source: crate::runtime::RuntimeTrackInputSource::None,
            preview_mode: RuntimePreviewMode::Stereo,
            output_track_id: None,
            output_track_index: None,
            inserts: vec![
                bridge_effect_track_with_id("insert-a"),
                bridge_effect_track_with_id("insert-b"),
            ],
            sends: Vec::new(),
            automation_lanes: Vec::new(),
            plugin_param_automation: Vec::new(),
            meter: Arc::new(Default::default()),
            meter_peak_l: 0.0,
            meter_peak_r: 0.0,
            meter_sum_sq_l: 0.0,
            meter_sum_sq_r: 0.0,
            callback_insert_log_done: false,
            callback_clip_route_log_done: false,
            block_l: vec![1.0; 8],
            block_r: vec![1.0; 8],
            recv_l: vec![0.0; 8],
            recv_r: vec![0.0; 8],
            soundfont_l: vec![0.0; 8],
            soundfont_r: vec![0.0; 8],
            midi_block_events: Vec::new(),
            midi_instrument_insert_ix: None,
            soundfont_player: None,
            plugin_latency_samples: 0,
            pdc_delay_l: Vec::new(),
            pdc_delay_r: Vec::new(),
            pdc_write_pos: 0,
            smoothed_gain_l: 1.0,
            smoothed_gain_r: 1.0,
        };
        let sink_a = Arc::new(MultSink {
            mult: 2.0,
            done: AtomicU64::new(1),
            buf_l: std::sync::Mutex::new(vec![0.0; 8]),
            buf_r: std::sync::Mutex::new(vec![0.0; 8]),
        });
        let sink_b = Arc::new(MultSink {
            mult: 3.0,
            done: AtomicU64::new(1),
            buf_l: std::sync::Mutex::new(vec![0.0; 8]),
            buf_r: std::sync::Mutex::new(vec![0.0; 8]),
        });
        track.inserts[0].bridge_sink = Some(sink_a as Arc<dyn PluginBridgeSink>);
        track.inserts[1].bridge_sink = Some(sink_b as Arc<dyn PluginBridgeSink>);
        apply_track_chain_block(&mut track, 4, RuntimeTransportContext::default());
        assert!(
            (track.block_l[0] - 6.0).abs() < 1e-4,
            "serial chain expected 1*2*3=6 got {}",
            track.block_l[0]
        );
    }

    #[test]
    fn serial_bridge_effect_chain_bypasses_only_missed_middle_insert() {
        #[derive(Debug)]
        struct MultSink {
            mult: f32,
            done: AtomicU64,
            buf_l: std::sync::Mutex<Vec<f32>>,
            buf_r: std::sync::Mutex<Vec<f32>>,
        }

        impl PluginBridgeSink for MultSink {
            fn dsp_ready(&self) -> bool {
                true
            }
            fn read_output(&self, out_l: &mut [f32], out_r: &mut [f32], frames: usize) -> usize {
                let done = self.done.load(Ordering::Acquire);
                if done == 0 {
                    return 0;
                }
                self.done.store(0, Ordering::Release);
                let bl = self.buf_l.lock().unwrap();
                let br = self.buf_r.lock().unwrap();
                let n = frames.min(out_l.len()).min(out_r.len()).min(bl.len());
                for i in 0..n {
                    out_l[i] = bl[i] * self.mult;
                    out_r[i] = br[i] * self.mult;
                }
                n
            }
            fn push_midi(&self, _: u8, _: u8, _: u8, _: u32) {}
            fn write_input(&self, in_l: &[f32], in_r: &[f32], frames: usize) {
                let n = frames.min(in_l.len()).min(in_r.len());
                *self.buf_l.lock().unwrap() = in_l[..n].to_vec();
                *self.buf_r.lock().unwrap() = in_r[..n].to_vec();
            }
            fn request_block(&self, _: u32) {
                self.done.store(1, Ordering::Release);
            }
        }

        #[derive(Debug)]
        struct MissSink;

        impl PluginBridgeSink for MissSink {
            fn dsp_ready(&self) -> bool {
                true
            }
            fn read_output(&self, _: &mut [f32], _: &mut [f32], _: usize) -> usize {
                0
            }
            fn push_midi(&self, _: u8, _: u8, _: u8, _: u32) {}
            fn write_input(&self, _: &[f32], _: &[f32], _: usize) {}
            fn request_block(&self, _: u32) {}
        }

        let mut track = RuntimeTrack {
            id: "track-1".to_string(),
            track_type: "audio".to_string(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            record_armed: false,
            monitor_enabled: false,
            input_source: crate::runtime::RuntimeTrackInputSource::None,
            preview_mode: RuntimePreviewMode::Stereo,
            output_track_id: None,
            output_track_index: None,
            inserts: vec![
                bridge_effect_track_with_id("insert-a"),
                bridge_effect_track_with_id("insert-b"),
                bridge_effect_track_with_id("insert-c"),
            ],
            sends: Vec::new(),
            automation_lanes: Vec::new(),
            plugin_param_automation: Vec::new(),
            meter: Arc::new(Default::default()),
            meter_peak_l: 0.0,
            meter_peak_r: 0.0,
            meter_sum_sq_l: 0.0,
            meter_sum_sq_r: 0.0,
            callback_insert_log_done: false,
            callback_clip_route_log_done: false,
            block_l: vec![1.0; 8],
            block_r: vec![1.0; 8],
            recv_l: vec![0.0; 8],
            recv_r: vec![0.0; 8],
            soundfont_l: vec![0.0; 8],
            soundfont_r: vec![0.0; 8],
            midi_block_events: Vec::new(),
            midi_instrument_insert_ix: None,
            soundfont_player: None,
            plugin_latency_samples: 0,
            pdc_delay_l: Vec::new(),
            pdc_delay_r: Vec::new(),
            pdc_write_pos: 0,
            smoothed_gain_l: 1.0,
            smoothed_gain_r: 1.0,
        };
        let sink_a = Arc::new(MultSink {
            mult: 2.0,
            done: AtomicU64::new(1),
            buf_l: std::sync::Mutex::new(vec![0.0; 8]),
            buf_r: std::sync::Mutex::new(vec![0.0; 8]),
        });
        let sink_b = Arc::new(MissSink);
        let sink_c = Arc::new(MultSink {
            mult: 5.0,
            done: AtomicU64::new(1),
            buf_l: std::sync::Mutex::new(vec![0.0; 8]),
            buf_r: std::sync::Mutex::new(vec![0.0; 8]),
        });
        track.inserts[0].bridge_sink = Some(sink_a as Arc<dyn PluginBridgeSink>);
        track.inserts[1].bridge_sink = Some(sink_b as Arc<dyn PluginBridgeSink>);
        track.inserts[2].bridge_sink = Some(sink_c as Arc<dyn PluginBridgeSink>);
        apply_track_chain_block(&mut track, 4, RuntimeTransportContext::default());
        assert!(
            (track.block_l[0] - 10.0).abs() < 1e-4,
            "A x2, B missed, C x5 expected 1*2*5=10 got {}",
            track.block_l[0]
        );
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::audio_file::AudioFileBuffer;
    use crate::audio_source::ClipAudioSource;
    use crate::runtime::{
        volume_db_to_norm, RuntimeAutomationCurve, RuntimeAutomationLane, RuntimeAutomationPoint,
        RuntimeAutomationTarget, RuntimeClip, RuntimePreviewMode, RuntimeProject, RuntimeSend,
        RuntimeTrack,
    };
    use std::sync::Arc;

    fn track(id: &str, ty: &str, sends: Vec<RuntimeSend>) -> RuntimeTrack {
        let cap = 8;
        RuntimeTrack {
            id: id.to_string(),
            track_type: ty.to_string(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            record_armed: false,
            monitor_enabled: false,
            input_source: crate::runtime::RuntimeTrackInputSource::None,
            preview_mode: RuntimePreviewMode::Stereo,
            output_track_id: None,
            output_track_index: None,
            inserts: Vec::new(),
            sends,
            automation_lanes: Vec::new(),
            plugin_param_automation: Vec::new(),
            meter: Arc::new(Default::default()),
            meter_peak_l: 0.0,
            meter_peak_r: 0.0,
            meter_sum_sq_l: 0.0,
            meter_sum_sq_r: 0.0,
            callback_insert_log_done: false,
            callback_clip_route_log_done: false,
            block_l: vec![0.0; cap],
            block_r: vec![0.0; cap],
            recv_l: vec![0.0; cap],
            recv_r: vec![0.0; cap],
            soundfont_l: vec![0.0; cap],
            soundfont_r: vec![0.0; cap],
            midi_block_events: Vec::new(),
            midi_instrument_insert_ix: None,
            soundfont_player: None,
            plugin_latency_samples: 0,
            pdc_delay_l: Vec::new(),
            pdc_delay_r: Vec::new(),
            pdc_write_pos: 0,
            smoothed_gain_l: 1.0,
            smoothed_gain_r: 1.0,
        }
    }

    fn send(target: &str, level: f32) -> RuntimeSend {
        RuntimeSend {
            id: format!("send-{target}"),
            return_track_id: target.to_string(),
            return_track_index: None,
            level,
            enabled: true,
            pre_fader: false,
        }
    }

    fn automation_lane(
        target: RuntimeAutomationTarget,
        value: f32,
        enabled: bool,
    ) -> RuntimeAutomationLane {
        RuntimeAutomationLane {
            id: "auto-1".to_string(),
            name: "Automation".to_string(),
            target,
            enabled,
            points: vec![RuntimeAutomationPoint {
                beat: 0.0,
                value,
                curve: RuntimeAutomationCurve::Linear,
                tension: 0.0,
            }],
        }
    }

    #[test]
    fn block_render_wraps_clip_material_inside_callback() {
        let frames = 5usize;
        let channels = 2usize;
        let mut audio_track = track("audio", "audio", vec![]);
        audio_track.pan = -1.0;
        let tracks = vec![audio_track];
        let audio_graph = crate::audio_graph::plan_runtime_audio_graph(&tracks).unwrap();
        let mut samples = Vec::new();
        for i in 0..8 {
            let v = i as f32 / 10.0;
            samples.push(v);
            samples.push(v);
        }
        let source = Arc::new(ClipAudioSource::InMemory(Arc::new(AudioFileBuffer {
            sample_rate: 48_000,
            channels: 2,
            frames: 8,
            samples,
        })));
        let mut p = RuntimeProject {
            sample_rate: 48_000,
            tracks,
            clips: vec![RuntimeClip {
                id: "clip-1".to_string(),
                track_id: "audio".to_string(),
                track_index: None,
                start_beat: 0.0,
                duration_beats: 8.0,
                start_sample: 0,
                duration_samples: 8,
                offset_seconds: 0.0,
                gain: 1.0,
                stretch: SphereAudioProcessor::StretchParams::default(),
                speed_ratio: 1.0,
                source_read_rate: 1.0,
                effective_time_ratio: 1.0,
                pitch_ratio: 1.0,
                stretch_backend: SphereAudioProcessor::StretchBackend::InternalRePitch,
                source_start_samples: 0,
                source_end_samples: 8,
                warp_markers: Vec::new(),
                processor: ClipDspProcessor::Resample,
                reverse: false,
                muted: false,
                fade_in_samples: 0,
                fade_out_samples: 0,
                source,
                stretch_processor: None,
                stretch_input_l: Vec::new(),
                stretch_input_r: Vec::new(),
                stretch_output_l: Vec::new(),
                stretch_output_r: Vec::new(),
                stretch_prime_l: Vec::new(),
                stretch_prime_r: Vec::new(),
                stretch_next_project_sample: None,
            }],
            audio_graph,
            ..Default::default()
        };
        p.resolve_indices();

        let mut output = vec![0.0f32; frames * channels];
        let rendered = render_project_block_interleaved(
            &mut p,
            3,
            1.0,
            &mut output,
            channels,
            true,
            4,
            4,
            Some(crate::transport::LoopBounds { start: 2, end: 5 }),
        );

        assert_eq!(rendered, frames as u64);
        let left: Vec<f32> = output.chunks(channels).map(|frame| frame[0]).collect();
        assert_eq!(left, vec![0.3, 0.4, 0.2, 0.3, 0.4]);
    }

    /// End-to-end render check that `reverse` and `speed_ratio` actually change
    /// the audio (not just the snapshot). This is the same block renderer the
    /// offline exporter drives, so it also covers "export uses the clip DSP path".
    #[test]
    fn reverse_and_speed_change_clip_render_output() {
        let frames = 8usize;
        let channels = 2usize;
        // Distinct, soft-limit-safe per-frame values: left[i] = i / 10.
        let samples: Vec<f32> = (0..8)
            .flat_map(|i| {
                let v = i as f32 / 10.0;
                [v, v]
            })
            .collect();

        let render = |reverse: bool, speed: f32| -> Vec<f32> {
            let mut audio_track = track("audio", "audio", vec![]);
            audio_track.pan = -1.0; // equal-power hard-left → left == source
            let tracks = vec![audio_track];
            let audio_graph = crate::audio_graph::plan_runtime_audio_graph(&tracks).unwrap();
            let source = Arc::new(ClipAudioSource::InMemory(Arc::new(AudioFileBuffer {
                sample_rate: 48_000,
                channels: 2,
                frames: 8,
                samples: samples.clone(),
            })));
            let mut p = RuntimeProject {
                sample_rate: 48_000,
                tracks,
                clips: vec![RuntimeClip {
                    id: "c".to_string(),
                    track_id: "audio".to_string(),
                    track_index: None,
                    start_beat: 0.0,
                    duration_beats: 8.0,
                    start_sample: 0,
                    duration_samples: 8,
                    offset_seconds: 0.0,
                    gain: 1.0,
                    stretch: SphereAudioProcessor::StretchParams::default(),
                    speed_ratio: speed,
                    source_read_rate: speed,
                    effective_time_ratio: if speed > 0.0 { 1.0 / speed } else { 1.0 },
                    pitch_ratio: 1.0,
                    stretch_backend: SphereAudioProcessor::StretchBackend::InternalRePitch,
                    source_start_samples: 0,
                    source_end_samples: 8,
                    warp_markers: Vec::new(),
                    processor: ClipDspProcessor::Resample,
                    reverse,
                    muted: false,
                    fade_in_samples: 0,
                    fade_out_samples: 0,
                    source,
                    stretch_processor: None,
                    stretch_input_l: Vec::new(),
                    stretch_input_r: Vec::new(),
                    stretch_output_l: Vec::new(),
                    stretch_output_r: Vec::new(),
                    stretch_prime_l: Vec::new(),
                    stretch_prime_r: Vec::new(),
                    stretch_next_project_sample: None,
                }],
                audio_graph,
                ..Default::default()
            };
            p.resolve_indices();
            let mut output = vec![0.0f32; frames * channels];
            render_project_block_interleaved(
                &mut p,
                0,
                1.0,
                &mut output,
                channels,
                true,
                4,
                4,
                None,
            );
            output.chunks(channels).map(|f| f[0]).collect()
        };

        let close = |a: &[f32], b: &[f32]| {
            assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
            for (x, y) in a.iter().zip(b) {
                assert!((x - y).abs() < 1e-4, "{a:?} vs {b:?}");
            }
        };

        // Forward reads source frames 0..7 straight.
        close(
            &render(false, 1.0),
            &[0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
        );
        // Reverse reads frames 7..0.
        close(
            &render(true, 1.0),
            &[0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0],
        );
        // Half read-rate (speed 0.5) consumes the source half as fast, linearly
        // interpolating between frames.
        close(
            &render(false, 0.5),
            &[0.0, 0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35],
        );
    }

    #[test]
    fn track_volume_and_pan_automation_affect_fader_output() {
        let mut t = track("audio", "audio", vec![]);
        t.volume = 0.25;
        t.pan = 0.0;
        t.automation_lanes = vec![
            automation_lane(
                RuntimeAutomationTarget::TrackVolume,
                volume_db_to_norm(0.0),
                true,
            ),
            automation_lane(RuntimeAutomationTarget::TrackPan, 1.0, true),
        ];

        let (l, r) = apply_track_chain_at_beat(1.0, 1.0, &mut t, 0.0);

        assert!(l.abs() < 1e-6, "full-right pan should mute left");
        assert!(
            (r - 1.0).abs() < 1e-6,
            "0 dB automation should override base volume"
        );
    }

    #[test]
    fn track_mute_automation_overrides_base_mute_state() {
        let mut t = track("audio", "audio", vec![]);
        assert!(!effective_track_muted(&t, 0.0));

        t.automation_lanes = vec![automation_lane(
            RuntimeAutomationTarget::TrackMute,
            1.0,
            true,
        )];
        assert!(effective_track_muted(&t, 0.0));

        t.automation_lanes[0].enabled = false;
        assert!(!effective_track_muted(&t, 0.0));
    }

    #[test]
    fn send_to_return_accumulates_scaled() {
        let frames = 4;
        let mut p = RuntimeProject {
            tracks: vec![
                track("audio", "audio", vec![send("ret", 0.5)]),
                track("ret", "return", vec![]),
            ],
            ..Default::default()
        };
        p.resolve_indices();
        // Source post-fader signal (accumulate_sends reads block_*).
        p.tracks[0].block_l[..frames].fill(1.0);
        p.tracks[0].block_r[..frames].fill(-2.0);

        accumulate_sends(&mut p, 0, frames, false);

        assert!(p.tracks[1].recv_l[..frames]
            .iter()
            .all(|&v| (v - 0.5).abs() < 1e-6));
        assert!(p.tracks[1].recv_r[..frames]
            .iter()
            .all(|&v| (v + 1.0).abs() < 1e-6));
    }

    #[test]
    fn pre_fader_filter_only_routes_matching_phase() {
        let frames = 4;
        let mut pre = send("ret", 1.0);
        pre.pre_fader = true;
        let mut p = RuntimeProject {
            tracks: vec![
                track("audio", "audio", vec![pre]),
                track("ret", "return", vec![]),
            ],
            ..Default::default()
        };
        p.resolve_indices();
        p.tracks[0].block_l[..frames].fill(1.0);

        // Post-fader phase: the pre-fader send must NOT route.
        accumulate_sends(&mut p, 0, frames, false);
        assert!(p.tracks[1].recv_l[..frames].iter().all(|&v| v == 0.0));

        // Pre-fader phase: now it routes.
        accumulate_sends(&mut p, 0, frames, true);
        assert!(p.tracks[1].recv_l[..frames]
            .iter()
            .all(|&v| (v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn send_to_non_routing_target_is_rejected() {
        let frames = 4;
        let mut p = RuntimeProject {
            tracks: vec![
                track("a", "audio", vec![send("b", 1.0)]),
                track("b", "audio", vec![]),
            ],
            ..Default::default()
        };
        p.resolve_indices();
        p.tracks[0].block_l[..frames].fill(1.0);
        accumulate_sends(&mut p, 0, frames, false);
        // Target is a normal audio track → not a valid send destination.
        assert!(p.tracks[1].recv_l[..frames].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn routing_to_earlier_routing_is_rejected_as_cycle_unsafe() {
        let frames = 4;
        // bus "early" at index 0 sends to "late" at index 1 (forward → OK),
        // and "late" sends back to "early" (backward → rejected).
        let mut p = RuntimeProject {
            tracks: vec![
                track("early", "bus", vec![send("late", 1.0)]),
                track("late", "return", vec![send("early", 1.0)]),
            ],
            ..Default::default()
        };
        p.resolve_indices();
        p.tracks[0].block_l[..frames].fill(1.0);
        p.tracks[1].block_l[..frames].fill(1.0);

        accumulate_sends(&mut p, 0, frames, false); // early → late: forward, accepted
        accumulate_sends(&mut p, 1, frames, false); // late → early: backward, rejected

        assert!(p.tracks[1].recv_l[..frames]
            .iter()
            .all(|&v| (v - 1.0).abs() < 1e-6));
        assert!(p.tracks[0].recv_l[..frames].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn main_output_to_bus_routes_into_bus_receive_not_master() {
        let frames = 4;
        let channels = 2;
        let mut a = track("a", "audio", vec![]);
        a.output_track_id = Some("bus".to_string());
        let mut p = RuntimeProject {
            tracks: vec![a, track("bus", "bus", vec![])],
            ..Default::default()
        };
        p.resolve_indices();
        p.tracks[0].block_l[..frames].fill(0.8);
        p.tracks[0].block_r[..frames].fill(-0.4);

        let mut output = vec![0.0f32; frames * channels];
        route_main_output(&mut p, 0, frames, &mut output, channels);

        // The post-fader block went into the bus receive buffers …
        assert!(p.tracks[1].recv_l[..frames]
            .iter()
            .all(|&v| (v - 0.8).abs() < 1e-6));
        assert!(p.tracks[1].recv_r[..frames]
            .iter()
            .all(|&v| (v + 0.4).abs() < 1e-6));
        // … and NOT into the master output.
        assert!(output.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn main_output_to_master_sums_into_output() {
        let frames = 4;
        let channels = 2;
        let mut p = RuntimeProject {
            tracks: vec![track("a", "audio", vec![])], // output_track_id = None → master
            ..Default::default()
        };
        p.tracks[0].block_l[..frames].fill(0.5);
        p.tracks[0].block_r[..frames].fill(0.25);

        let mut output = vec![0.0f32; frames * channels];
        route_main_output(&mut p, 0, frames, &mut output, channels);

        for f in 0..frames {
            assert!((output[f * channels] - 0.5).abs() < 1e-6);
            assert!((output[f * channels + 1] - 0.25).abs() < 1e-6);
        }
    }

    #[test]
    fn main_output_to_non_routing_track_falls_back_to_master() {
        let frames = 4;
        let channels = 2;
        let mut a = track("a", "audio", vec![]);
        a.output_track_id = Some("b".to_string()); // "b" is a plain audio track
        let mut p = RuntimeProject {
            tracks: vec![a, track("b", "audio", vec![])],
            ..Default::default()
        };
        p.resolve_indices();
        p.tracks[0].block_l[..frames].fill(1.0);
        p.tracks[0].block_r[..frames].fill(1.0);

        let mut output = vec![0.0f32; frames * channels];
        route_main_output(&mut p, 0, frames, &mut output, channels);

        // Not a routing target → summed to master, "b" untouched.
        assert!(output.iter().all(|&v| (v - 1.0).abs() < 1e-6));
        assert!(p.tracks[1].recv_l[..frames].iter().all(|&v| v == 0.0));
    }
}

#[cfg(test)]
mod dropout_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    // 512 frames @ 48 kHz: per-block budget = 512 * 1e6 / 48000 ≈ 10_666 µs.
    const FRAMES: usize = 512;
    const SR: u32 = 48_000;

    #[test]
    fn threshold_ratios_are_ordered() {
        // Stricter modes flag at a smaller fraction of the deadline.
        let frac = |m: DropoutProtectionMode| {
            let (n, d) = m.dropout_threshold_ratio();
            n as f64 / d as f64
        };
        assert!(frac(DropoutProtectionMode::High) < frac(DropoutProtectionMode::Medium));
        assert!(frac(DropoutProtectionMode::Medium) < frac(DropoutProtectionMode::Light));
        assert!(frac(DropoutProtectionMode::Light) < frac(DropoutProtectionMode::Off));
        assert_eq!(frac(DropoutProtectionMode::Off), 1.0);
    }

    #[test]
    fn fast_block_never_flags_dropout() {
        let shared = SharedState::default();
        shared
            .dropout_protection_mode
            .store(DropoutProtectionMode::Medium as u8, Ordering::Relaxed);
        record_output_callback_timing(&shared, 1_000, FRAMES, SR);
        assert_eq!(shared.dropout_count.load(Ordering::Relaxed), 0);
        assert_eq!(
            shared.dropout_last_reason.load(Ordering::Relaxed),
            DropoutReason::None as u8
        );
        // Deadline is published regardless.
        assert!(shared.callback_deadline_us.load(Ordering::Relaxed) > 10_000);
    }

    #[test]
    fn medium_flags_block_over_eighty_percent_of_deadline() {
        let shared = SharedState::default();
        shared
            .dropout_protection_mode
            .store(DropoutProtectionMode::Medium as u8, Ordering::Relaxed);
        // 9_000 µs > 80% of 10_666 µs (≈ 8_533) → dropout under Medium.
        record_output_callback_timing(&shared, 9_000, FRAMES, SR);
        assert_eq!(shared.dropout_count.load(Ordering::Relaxed), 1);
        assert_eq!(
            shared.dropout_last_reason.load(Ordering::Relaxed),
            DropoutReason::CallbackOverrun as u8
        );
    }

    #[test]
    fn off_only_flags_true_overruns() {
        let shared = SharedState::default();
        shared
            .dropout_protection_mode
            .store(DropoutProtectionMode::Off as u8, Ordering::Relaxed);
        // Same 9_000 µs is < the full 10_666 µs deadline → no dropout under Off.
        record_output_callback_timing(&shared, 9_000, FRAMES, SR);
        assert_eq!(shared.dropout_count.load(Ordering::Relaxed), 0);
        // A genuine overrun past the deadline does flag, even under Off.
        record_output_callback_timing(&shared, 12_000, FRAMES, SR);
        assert_eq!(shared.dropout_count.load(Ordering::Relaxed), 1);
    }
}
