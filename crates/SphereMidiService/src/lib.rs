//! Futureboard MIDI service layer.
//!
//! This crate owns MIDI data types, MIDI device enumeration, preference merge,
//! and UI/control-path MIDI event primitives. UI crates should depend on this
//! crate instead of carrying MIDI service state internally.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MidiDeviceDirection {
    Input,
    Output,
    InputOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MidiDeviceSetting {
    pub id: String,
    pub name: String,
    pub direction: MidiDeviceDirection,
    pub enabled: bool,
    pub connected: bool,
    #[serde(default)]
    pub clock_enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MidiHardwareSettings {
    #[serde(default)]
    pub devices: Vec<MidiDeviceSetting>,
    pub clock_sync: bool,
    /// Legacy — migrated into [`devices`] on load.
    #[serde(default, skip_serializing)]
    pub enabled_inputs: Vec<String>,
    #[serde(default, skip_serializing)]
    pub enabled_outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedMidiDevice {
    pub id: String,
    pub name: String,
    pub direction: MidiDeviceDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiInputSource {
    Hardware,
    PianoRollPreview,
    VirtualKeyboard,
    DawRemote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualKeyboardEvent {
    NoteOn { note: u8, velocity: u8, channel: u8 },
    NoteOff { note: u8, channel: u8 },
    Sustain { down: bool, channel: u8 },
    Panic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiInputEvent {
    NoteOn {
        note: u8,
        velocity: u8,
        channel: u8,
    },
    NoteOff {
        note: u8,
        channel: u8,
    },
    ControlChange {
        controller: u8,
        value: u8,
        channel: u8,
    },
    AllNotesOff,
    Panic,
}

impl From<VirtualKeyboardEvent> for MidiInputEvent {
    fn from(event: VirtualKeyboardEvent) -> Self {
        match event {
            VirtualKeyboardEvent::NoteOn {
                note,
                velocity,
                channel,
            } => Self::NoteOn {
                note,
                velocity,
                channel,
            },
            VirtualKeyboardEvent::NoteOff { note, channel } => Self::NoteOff { note, channel },
            VirtualKeyboardEvent::Sustain { down, channel } => Self::ControlChange {
                controller: 64,
                value: if down { 127 } else { 0 },
                channel,
            },
            VirtualKeyboardEvent::Panic => Self::Panic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiInputTarget {
    pub track_id: String,
    pub plugin_instance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiInputRouteStatus {
    Routed,
    NoTarget,
    EngineUnavailable,
    DispatchFailed(String),
}

pub struct MidiInputRouter;

impl MidiInputRouter {
    pub fn sanitize_channel(channel: u8) -> u8 {
        channel.min(15)
    }

    pub fn sanitize_note(note: u8) -> u8 {
        note.min(127)
    }

    pub fn sanitize_velocity(velocity: u8) -> u8 {
        velocity.clamp(1, 127)
    }
}

pub fn midi_settings_debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_MIDI_SETTINGS_DEBUG").is_some())
}

fn stable_id(direction: MidiDeviceDirection, name: &str) -> String {
    let slug = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let prefix = match direction {
        MidiDeviceDirection::Input => "midi-in",
        MidiDeviceDirection::Output => "midi-out",
        MidiDeviceDirection::InputOutput => "midi-io",
    };
    format!("{prefix}-{slug}")
}

/// Real MIDI port scan via `midir` (WinMM on Windows, CoreMIDI on macOS, ALSA on
/// Linux). Enumeration only reads port names — it never opens the hardware.
/// Wrapped in `catch_unwind` so a misbehaving backend yields an empty list and a
/// warning rather than taking down the UI thread.
pub fn scan_midi_ports() -> Vec<DetectedMidiDevice> {
    match std::panic::catch_unwind(real_scan_midi_ports) {
        Ok(devices) => {
            if midi_settings_debug_enabled() {
                eprintln!("[MIDI settings] detected devices ({})", devices.len());
                for device in &devices {
                    eprintln!(
                        "  - {} ({:?}) id={}",
                        device.name, device.direction, device.id
                    );
                }
            }
            devices
        }
        Err(_) => {
            eprintln!("[MidiDeviceScan] enumeration panicked — returning empty list");
            Vec::new()
        }
    }
}

/// macOS placeholder: midir's CoreMIDI backend currently conflicts with gpui's
/// pinned `core-foundation` version, so the macOS port scan is unavailable. We
/// return an empty list (no mock data) until that dependency pin is reconciled.
#[cfg(target_os = "macos")]
fn real_scan_midi_ports() -> Vec<DetectedMidiDevice> {
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
fn real_scan_midi_ports() -> Vec<DetectedMidiDevice> {
    use midir::{MidiInput, MidiOutput};

    let mut devices = Vec::new();
    match MidiInput::new("Futureboard MIDI scan (in)") {
        Ok(input) => {
            for port in input.ports() {
                if let Ok(name) = input.port_name(&port) {
                    devices.push(DetectedMidiDevice {
                        id: stable_id(MidiDeviceDirection::Input, &name),
                        name,
                        direction: MidiDeviceDirection::Input,
                    });
                }
            }
        }
        Err(e) => eprintln!("[MidiDeviceScan] MIDI input backend unavailable: {e}"),
    }
    match MidiOutput::new("Futureboard MIDI scan (out)") {
        Ok(output) => {
            for port in output.ports() {
                if let Ok(name) = output.port_name(&port) {
                    devices.push(DetectedMidiDevice {
                        id: stable_id(MidiDeviceDirection::Output, &name),
                        name,
                        direction: MidiDeviceDirection::Output,
                    });
                }
            }
        }
        Err(e) => eprintln!("[MidiDeviceScan] MIDI output backend unavailable: {e}"),
    }
    coalesce_detected_midi_devices(devices)
}

/// Merge same-name input and output ports into a single InputOutput entry so a
/// bidirectional controller does not appear twice in Preferences / routing lists.
fn coalesce_detected_midi_devices(devices: Vec<DetectedMidiDevice>) -> Vec<DetectedMidiDevice> {
    let mut by_name: HashMap<String, DetectedMidiDevice> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for device in devices {
        match by_name.get_mut(&device.name) {
            Some(existing) => {
                if existing.direction != device.direction
                    && existing.direction != MidiDeviceDirection::InputOutput
                {
                    existing.direction = MidiDeviceDirection::InputOutput;
                    existing.id = stable_id(MidiDeviceDirection::InputOutput, &existing.name);
                }
            }
            None => {
                order.push(device.name.clone());
                by_name.insert(device.name.clone(), device);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect()
}

/// Merge saved preferences with freshly detected devices. Saved-only entries stay visible as missing.
pub fn resolve_midi_devices(
    saved: &[MidiDeviceSetting],
    detected: &[DetectedMidiDevice],
) -> Vec<MidiDeviceSetting> {
    let detected = coalesce_detected_midi_devices(detected.to_vec());
    if midi_settings_debug_enabled() {
        eprintln!("[MIDI settings] saved preferences ({})", saved.len());
        for device in saved {
            eprintln!(
                "  - {} enabled={} connected={} clock={}",
                device.name, device.enabled, device.connected, device.clock_enabled
            );
        }
    }

    let saved_by_id: HashMap<&str, &MidiDeviceSetting> =
        saved.iter().map(|d| (d.id.as_str(), d)).collect();
    let saved_by_name: HashMap<&str, &MidiDeviceSetting> =
        saved.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut resolved = Vec::new();
    let mut resolved_names = std::collections::HashSet::new();

    for det in &detected {
        let saved = saved_by_id
            .get(det.id.as_str())
            .copied()
            .or_else(|| saved_by_name.get(det.name.as_str()).copied());
        resolved_names.insert(det.name.clone());
        resolved.push(MidiDeviceSetting {
            id: det.id.clone(),
            name: det.name.clone(),
            direction: det.direction,
            // New devices default to enabled so real-time MIDI input works
            // without a Preferences visit; users can still disable them.
            enabled: saved.map(|s| s.enabled).unwrap_or(true),
            connected: true,
            clock_enabled: saved.map(|s| s.clock_enabled).unwrap_or(false),
        });
    }

    for saved_device in saved {
        if detected.iter().any(|d| d.id == saved_device.id)
            || resolved_names.contains(&saved_device.name)
        {
            continue;
        }
        if midi_settings_debug_enabled() {
            eprintln!(
                "[MIDI settings] missing saved device: {} ({})",
                saved_device.name, saved_device.id
            );
        }
        resolved.push(MidiDeviceSetting {
            id: saved_device.id.clone(),
            name: saved_device.name.clone(),
            direction: saved_device.direction,
            enabled: saved_device.enabled,
            connected: false,
            clock_enabled: saved_device.clock_enabled,
        });
    }

    resolved
}

pub fn upsert_midi_device(midi: &mut MidiHardwareSettings, device: MidiDeviceSetting) {
    if let Some(existing) = midi.devices.iter_mut().find(|d| d.id == device.id) {
        *existing = device;
    } else {
        midi.devices.push(device);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HardwareMidiEvent {
    /// MIDI output device id or display name. The GPUI routing UI currently
    /// stores the display name; matching accepts either for migration safety.
    pub device_id: String,
    /// Legacy/diagnostic relative time. New playback scheduling uses
    /// [`absolute_sample`] against the audio transport timeline.
    pub delay_seconds: f64,
    /// Musical position used for diagnostics and rebuilds after tempo changes.
    pub beat: f64,
    /// Absolute sample position on the same timeline as audio playback.
    pub absolute_sample: u64,
    /// Raw MIDI bytes, e.g. [0x90 | channel, pitch, velocity].
    pub message: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct HardwareMidiPlaybackConfig {
    pub start_sample: u64,
    pub sample_rate: u32,
    pub lookahead: Duration,
}

impl HardwareMidiPlaybackConfig {
    pub fn new(start_sample: u64, sample_rate: u32) -> Self {
        Self {
            start_sample,
            sample_rate: sample_rate.max(1),
            lookahead: Duration::from_millis(10),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HardwareMidiProfilerSnapshot {
    /// Platform whose scheduler produced this measurement.
    pub platform: &'static str,
    pub events_per_second: u32,
    pub max_jitter_us: u32,
}

pub struct HardwareMidiPlayback {
    cancel_tx: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
    profiler: std::sync::Arc<HardwareMidiProfiler>,
}

impl Default for HardwareMidiPlayback {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareMidiPlayback {
    pub fn new() -> Self {
        Self {
            cancel_tx: None,
            handle: None,
            profiler: std::sync::Arc::new(HardwareMidiProfiler::default()),
        }
    }

    /// Legacy entry point retained for callers that only know relative seconds.
    /// New transport playback should call [`Self::start_at_sample`] so events are
    /// aligned to the same sample timeline as audio.
    pub fn start(&mut self, mut events: Vec<HardwareMidiEvent>) {
        for event in &mut events {
            if event.absolute_sample == 0 && event.delay_seconds > 0.0 {
                event.absolute_sample = seconds_to_samples(event.delay_seconds, 48_000);
            }
        }
        self.start_with_config(events, HardwareMidiPlaybackConfig::new(0, 48_000));
    }

    pub fn start_at_sample(
        &mut self,
        events: Vec<HardwareMidiEvent>,
        start_sample: u64,
        sample_rate: u32,
    ) {
        self.start_with_config(
            events,
            HardwareMidiPlaybackConfig::new(start_sample, sample_rate),
        );
    }

    pub fn start_with_config(
        &mut self,
        mut events: Vec<HardwareMidiEvent>,
        config: HardwareMidiPlaybackConfig,
    ) {
        self.stop();
        self.profiler.reset();
        if events.is_empty() {
            return;
        }
        sort_hardware_midi_events(&mut events);
        events = coalesce_hardware_midi_events(events, config.sample_rate);
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
        self.cancel_tx = Some(cancel_tx);
        self.handle = Some(spawn_hardware_midi_thread(
            events,
            config,
            cancel_rx,
            self.profiler.clone(),
        ));
    }

    pub fn profiler_snapshot(&self) -> HardwareMidiProfilerSnapshot {
        self.profiler.snapshot()
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HardwareMidiPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One decoded MIDI message received from an enabled hardware input port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareMidiInputMessage {
    pub device_id: String,
    pub device_name: String,
    pub event: MidiInputEvent,
}

/// Control-thread hardware MIDI input listener. Opens midir connections for
/// enabled input / input-output devices and pushes decoded events onto a
/// bounded queue drained by the UI poll loop.
pub struct HardwareMidiInput {
    event_rx: Option<std::sync::mpsc::Receiver<HardwareMidiInputMessage>>,
    /// Cancel + join handles for the active input connections.
    connections: Vec<HardwareMidiInputConnection>,
    /// Last synced set of enabled device ids (so we can no-op when unchanged).
    enabled_ids: Vec<String>,
}

struct HardwareMidiInputConnection {
    #[allow(dead_code)]
    device_id: String,
    /// Dropping the connection closes the port. Kept alive for the session.
    #[cfg(not(target_os = "macos"))]
    _connection: midir::MidiInputConnection<()>,
    #[cfg(target_os = "macos")]
    _connection: (),
}

impl Default for HardwareMidiInput {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareMidiInput {
    pub fn new() -> Self {
        Self {
            event_rx: None,
            connections: Vec::new(),
            enabled_ids: Vec::new(),
        }
    }

    /// Open / refresh connections for every enabled input-capable device.
    /// Safe to call repeatedly from the UI poll; reconnects only when the
    /// enabled set changes.
    pub fn sync_enabled_devices(&mut self, devices: &[MidiDeviceSetting]) {
        let mut enabled: Vec<(String, String)> = devices
            .iter()
            .filter(|d| {
                d.enabled
                    && d.connected
                    && matches!(
                        d.direction,
                        MidiDeviceDirection::Input | MidiDeviceDirection::InputOutput
                    )
            })
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect();
        enabled.sort_by(|a, b| a.0.cmp(&b.0));
        let enabled_ids: Vec<String> = enabled.iter().map(|(id, _)| id.clone()).collect();
        if enabled_ids == self.enabled_ids {
            return;
        }
        self.stop();
        self.enabled_ids = enabled_ids;
        if enabled.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.event_rx = Some(rx);
        self.connections = open_hardware_midi_inputs(enabled, tx);
    }

    /// Drain pending hardware MIDI messages (non-blocking). Bounded by caller.
    pub fn drain(&mut self, max: usize) -> Vec<HardwareMidiInputMessage> {
        let Some(rx) = self.event_rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while out.len() < max {
            match rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(_) => break,
            }
        }
        out
    }

    pub fn stop(&mut self) {
        self.connections.clear();
        self.event_rx = None;
        self.enabled_ids.clear();
    }
}

impl Drop for HardwareMidiInput {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Only the midir input path feeds this; macOS opens no hardware input yet
/// (see `open_hardware_midi_inputs`), so nothing decodes bytes there.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn decode_midi_bytes(bytes: &[u8]) -> Option<MidiInputEvent> {
    if bytes.is_empty() {
        return None;
    }
    let status = bytes[0];
    let kind = status & 0xF0;
    let channel = status & 0x0F;
    match kind {
        0x90 => {
            let note = *bytes.get(1)?;
            let velocity = *bytes.get(2)?;
            if velocity == 0 {
                Some(MidiInputEvent::NoteOff { note, channel })
            } else {
                Some(MidiInputEvent::NoteOn {
                    note,
                    velocity,
                    channel,
                })
            }
        }
        0x80 => {
            let note = *bytes.get(1)?;
            Some(MidiInputEvent::NoteOff { note, channel })
        }
        0xB0 => {
            let controller = *bytes.get(1)?;
            let value = *bytes.get(2)?;
            if controller == 123 {
                Some(MidiInputEvent::AllNotesOff)
            } else {
                Some(MidiInputEvent::ControlChange {
                    controller,
                    value,
                    channel,
                })
            }
        }
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn open_hardware_midi_inputs(
    enabled: Vec<(String, String)>,
    tx: std::sync::mpsc::Sender<HardwareMidiInputMessage>,
) -> Vec<HardwareMidiInputConnection> {
    use midir::MidiInput;

    let mut connections = Vec::new();
    let Ok(scanner) = MidiInput::new("Futureboard MIDI input scan") else {
        eprintln!("[MIDI input] backend unavailable — cannot open hardware ports");
        return connections;
    };
    let ports = scanner.ports();
    for (device_id, device_name) in enabled {
        let Some(port) = ports.iter().find(|port| {
            scanner
                .port_name(port)
                .ok()
                .is_some_and(|name| name == device_name)
        }) else {
            eprintln!("[MIDI input] port not found for enabled device '{device_name}'");
            continue;
        };
        // midir requires a fresh MidiInput per connection.
        let Ok(input) = MidiInput::new(&format!("Futureboard MIDI in ({device_name})")) else {
            continue;
        };
        let tx = tx.clone();
        let id = device_id.clone();
        let name = device_name.clone();
        match input.connect(
            port,
            &format!("Futureboard listen ({device_name})"),
            move |_stamp, message, _| {
                if let Some(event) = decode_midi_bytes(message) {
                    let _ = tx.send(HardwareMidiInputMessage {
                        device_id: id.clone(),
                        device_name: name.clone(),
                        event,
                    });
                }
            },
            (),
        ) {
            Ok(connection) => {
                if midi_settings_debug_enabled() {
                    eprintln!("[MIDI input] connected '{device_name}' ({device_id})");
                }
                connections.push(HardwareMidiInputConnection {
                    device_id,
                    _connection: connection,
                });
            }
            Err(error) => {
                eprintln!("[MIDI input] failed to open '{device_name}': {error}");
            }
        }
    }
    connections
}

#[cfg(target_os = "macos")]
fn open_hardware_midi_inputs(
    _enabled: Vec<(String, String)>,
    _tx: std::sync::mpsc::Sender<HardwareMidiInputMessage>,
) -> Vec<HardwareMidiInputConnection> {
    // midir's CoreMIDI backend currently conflicts with gpui's pinned
    // core-foundation version (same limitation as port scan / output).
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
fn spawn_hardware_midi_thread(
    events: Vec<HardwareMidiEvent>,
    config: HardwareMidiPlaybackConfig,
    cancel_rx: std::sync::mpsc::Receiver<()>,
    profiler: std::sync::Arc<HardwareMidiProfiler>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("Futureboard MIDI output".to_string())
        .spawn(move || run_hardware_midi_thread(events, config, cancel_rx, profiler))
        .expect("spawn Futureboard MIDI output thread")
}

#[cfg(target_os = "macos")]
fn spawn_hardware_midi_thread(
    _events: Vec<HardwareMidiEvent>,
    _config: HardwareMidiPlaybackConfig,
    _cancel_rx: std::sync::mpsc::Receiver<()>,
    _profiler: std::sync::Arc<HardwareMidiProfiler>,
) -> JoinHandle<()> {
    std::thread::spawn(|| {})
}

#[cfg(not(target_os = "macos"))]
fn run_hardware_midi_thread(
    events: Vec<HardwareMidiEvent>,
    config: HardwareMidiPlaybackConfig,
    cancel_rx: std::sync::mpsc::Receiver<()>,
    profiler: std::sync::Arc<HardwareMidiProfiler>,
) {
    use midir::MidiOutputConnection;

    let _thread_scope = MidiThreadScope::enter();
    let debug = midi_output_debug_enabled();
    let lateness_warnings = midi_lateness_warnings_enabled();
    let sample_rate = config.sample_rate.max(1) as f64;
    let start_sample = config.start_sample;
    let wall_start = Instant::now();
    let lookahead_samples = seconds_to_samples(config.lookahead.as_secs_f64(), config.sample_rate);
    let mut connections: HashMap<String, MidiOutputConnection> = HashMap::new();
    let mut cursor = events.partition_point(|ev| ev.absolute_sample < start_sample);

    // Open enabled target devices once on the MIDI thread. Avoiding open/close
    // during playback prevents WinMM/midir from introducing UI-sized stalls.
    for device_id in unique_event_devices(&events[cursor..]) {
        if let Some(conn) = open_midi_output(&device_id) {
            connections.insert(device_id, conn);
        }
    }

    while cursor < events.len() {
        if cancel_rx.try_recv().is_ok() {
            send_all_notes_off(&mut connections);
            return;
        }

        let timeline_sample = start_sample.saturating_add(seconds_to_samples(
            wall_start.elapsed().as_secs_f64(),
            config.sample_rate,
        ));
        let horizon = timeline_sample.saturating_add(lookahead_samples.max(1));

        if events[cursor].absolute_sample > horizon {
            wait_for_midi_tick(Duration::from_millis(1));
            continue;
        }

        let event = &events[cursor];
        let scheduled_wall = wall_start
            + samples_to_duration(
                event.absolute_sample.saturating_sub(start_sample),
                sample_rate,
            );
        while Instant::now() < scheduled_wall {
            if cancel_rx.try_recv().is_ok() {
                send_all_notes_off(&mut connections);
                return;
            }
            let remaining = scheduled_wall.saturating_duration_since(Instant::now());
            wait_for_midi_tick(remaining.min(Duration::from_millis(1)));
        }

        let actual = Instant::now();
        let lateness = actual.saturating_duration_since(scheduled_wall);
        profiler.record(lateness);
        if let Some(conn) = connections.get_mut(&event.device_id) {
            let _ = conn.send(&event.message);
        }
        log_midi_dispatch(
            event,
            scheduled_wall,
            actual,
            lateness,
            debug,
            lateness_warnings,
        );
        cursor += 1;
    }
}

#[cfg(not(target_os = "macos"))]
fn open_midi_output(device_id_or_name: &str) -> Option<midir::MidiOutputConnection> {
    let midi_out = midir::MidiOutput::new("Futureboard MIDI playback").ok()?;
    for port in midi_out.ports() {
        let Ok(name) = midi_out.port_name(&port) else {
            continue;
        };
        let stable = stable_id(MidiDeviceDirection::Output, &name);
        if name == device_id_or_name || stable == device_id_or_name {
            return midi_out.connect(&port, "Futureboard MIDI Out").ok();
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn send_all_notes_off(connections: &mut HashMap<String, midir::MidiOutputConnection>) {
    for conn in connections.values_mut() {
        for channel in 0..16u8 {
            for note in 0..128u8 {
                let _ = conn.send(&[0x80 | channel, note, 0]);
            }
            let _ = conn.send(&[0xb0 | channel, 64, 0]);
            let _ = conn.send(&[0xb0 | channel, 123, 0]);
            let _ = conn.send(&[0xb0 | channel, 120, 0]);
        }
    }
}

fn seconds_to_samples(seconds: f64, sample_rate: u32) -> u64 {
    (seconds.max(0.0) * sample_rate.max(1) as f64).round() as u64
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn samples_to_duration(samples: u64, sample_rate: f64) -> Duration {
    Duration::from_secs_f64(samples as f64 / sample_rate.max(1.0))
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn midi_output_debug_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_MIDI_OUTPUT_DEBUG").is_some())
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn midi_lateness_warnings_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_MIDI_OUTPUT_LATENESS_WARN").is_some())
}

fn sort_hardware_midi_events(events: &mut [HardwareMidiEvent]) {
    events.sort_by(|a, b| {
        a.absolute_sample
            .cmp(&b.absolute_sample)
            .then_with(|| midi_order_key(a).cmp(&midi_order_key(b)))
            .then_with(|| a.device_id.cmp(&b.device_id))
            .then_with(|| a.message.cmp(&b.message))
    });
}

fn midi_order_key(event: &HardwareMidiEvent) -> (u8, u8, u8, u8) {
    let status = event.message.first().copied().unwrap_or(0);
    let kind = status & 0xf0;
    let channel = status & 0x0f;
    let data1 = event.message.get(1).copied().unwrap_or(0);
    let data2 = event.message.get(2).copied().unwrap_or(0);
    let group = match kind {
        0x80 => 0,
        0x90 if data2 == 0 => 0,
        0xb0 if data1 == 64 && data2 == 0 => 1,
        0xb0 if data1 == 120 || data1 == 123 => 1,
        0xc0 => 2,
        0xb0 if data1 == 0 || data1 == 32 => 3,
        0xb0 => 4,
        0xe0 => 5,
        0xa0 | 0xd0 => 6,
        0x90 => 7,
        0xf0 => 8,
        _ => 9,
    };
    (group, channel, data1, data2)
}

fn coalesce_hardware_midi_events(
    events: Vec<HardwareMidiEvent>,
    sample_rate: u32,
) -> Vec<HardwareMidiEvent> {
    let close_window = seconds_to_samples(0.002, sample_rate).max(1);
    let dense_window = seconds_to_samples(0.005, sample_rate).max(1);
    let mut out: Vec<HardwareMidiEvent> = Vec::with_capacity(events.len());
    let mut last_cc: HashMap<(String, u8, u8), (u8, u64)> = HashMap::new();
    let mut last_pb: HashMap<(String, u8), (u16, u64)> = HashMap::new();

    for event in events {
        let Some(status) = event.message.first().copied() else {
            out.push(event);
            continue;
        };
        let kind = status & 0xf0;
        let channel = status & 0x0f;
        match kind {
            0xb0 => {
                let controller = event.message.get(1).copied().unwrap_or(0);
                let value = event.message.get(2).copied().unwrap_or(0);
                // Preserve bank select, sustain pedal transitions, and panic CCs.
                if matches!(controller, 0 | 32 | 64 | 120 | 123) {
                    out.push(event);
                    continue;
                }
                let key = (event.device_id.clone(), channel, controller);
                if let Some((prev_value, prev_sample)) = last_cc.get(&key).copied() {
                    let delta = event.absolute_sample.saturating_sub(prev_sample);
                    if prev_value == value && delta <= close_window {
                        continue;
                    }
                    if delta <= dense_window {
                        if let Some(last) = out.iter_mut().rev().find(|candidate| {
                            candidate.device_id == event.device_id
                                && candidate.message.first().copied().unwrap_or(0) & 0xf0 == 0xb0
                                && candidate.message.first().copied().unwrap_or(0) & 0x0f == channel
                                && candidate.message.get(1).copied().unwrap_or(255) == controller
                        }) {
                            *last = event.clone();
                        } else {
                            out.push(event.clone());
                        }
                        last_cc.insert(key, (value, event.absolute_sample));
                        continue;
                    }
                }
                last_cc.insert(key, (value, event.absolute_sample));
                out.push(event);
            }
            0xe0 => {
                let lsb = event.message.get(1).copied().unwrap_or(0) as u16;
                let msb = event.message.get(2).copied().unwrap_or(0) as u16;
                let value = (msb << 7) | lsb;
                let key = (event.device_id.clone(), channel);
                if let Some((prev_value, prev_sample)) = last_pb.get(&key).copied() {
                    let delta = event.absolute_sample.saturating_sub(prev_sample);
                    if prev_value == value && delta <= close_window {
                        continue;
                    }
                    if delta <= dense_window {
                        if let Some(last) = out.iter_mut().rev().find(|candidate| {
                            candidate.device_id == event.device_id
                                && candidate.message.first().copied().unwrap_or(0) & 0xf0 == 0xe0
                                && candidate.message.first().copied().unwrap_or(0) & 0x0f == channel
                        }) {
                            *last = event.clone();
                        } else {
                            out.push(event.clone());
                        }
                        last_pb.insert(key, (value, event.absolute_sample));
                        continue;
                    }
                }
                last_pb.insert(key, (value, event.absolute_sample));
                out.push(event);
            }
            _ => out.push(event),
        }
    }

    sort_hardware_midi_events(&mut out);
    out
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn unique_event_devices(events: &[HardwareMidiEvent]) -> Vec<String> {
    let mut devices = Vec::new();
    for event in events {
        if !devices.iter().any(|device| device == &event.device_id) {
            devices.push(event.device_id.clone());
        }
    }
    devices
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn log_midi_dispatch(
    event: &HardwareMidiEvent,
    scheduled_wall: Instant,
    actual: Instant,
    lateness: Duration,
    debug: bool,
    warnings: bool,
) {
    let lateness_ms = lateness.as_secs_f64() * 1000.0;
    if debug {
        let (kind, ch, d1, d2) = describe_midi_message(&event.message);
        eprintln!(
            "[midi-output] send type={kind} ch={ch} data1={d1} data2={d2} beat={:.6} sample={} scheduled={scheduled_wall:?} actual={actual:?} late_ms={lateness_ms:.3}",
            event.beat, event.absolute_sample,
        );
    }
    if warnings && lateness_ms >= 2.0 {
        let threshold = if lateness_ms >= 20.0 {
            20
        } else if lateness_ms >= 10.0 {
            10
        } else if lateness_ms >= 5.0 {
            5
        } else {
            2
        };
        eprintln!(
            "[midi-output] WARNING lateness>{threshold}ms actual={lateness_ms:.3} sample={} beat={:.6}",
            event.absolute_sample, event.beat
        );
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn describe_midi_message(message: &[u8]) -> (&'static str, u8, u8, u8) {
    let status = message.first().copied().unwrap_or(0);
    let channel = status & 0x0f;
    let d1 = message.get(1).copied().unwrap_or(0);
    let d2 = message.get(2).copied().unwrap_or(0);
    let kind = match status & 0xf0 {
        0x80 => "note_off",
        0x90 if d2 == 0 => "note_off",
        0x90 => "note_on",
        0xa0 => "poly_aftertouch",
        0xb0 => "cc",
        0xc0 => "program_change",
        0xd0 => "channel_aftertouch",
        0xe0 => "pitch_bend",
        0xf0 => "sysex/system",
        _ => "unknown",
    };
    (kind, channel, d1, d2)
}

#[derive(Default)]
struct HardwareMidiProfiler {
    events_total: std::sync::atomic::AtomicU64,
    max_jitter_us: std::sync::atomic::AtomicU32,
    started: OnceLock<Instant>,
}

impl HardwareMidiProfiler {
    fn reset(&self) {
        self.events_total
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.max_jitter_us
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn record(&self, lateness: Duration) {
        let _ = self.started.get_or_init(Instant::now);
        self.events_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let us = lateness.as_micros().min(u32::MAX as u128) as u32;
        let mut prev = self
            .max_jitter_us
            .load(std::sync::atomic::Ordering::Relaxed);
        while us > prev {
            match self.max_jitter_us.compare_exchange_weak(
                prev,
                us,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => prev = next,
            }
        }
    }

    fn snapshot(&self) -> HardwareMidiProfilerSnapshot {
        let elapsed = self
            .started
            .get()
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or(0.0)
            .max(0.001);
        HardwareMidiProfilerSnapshot {
            platform: std::env::consts::OS,
            events_per_second: (self.events_total.load(std::sync::atomic::Ordering::Relaxed) as f64
                / elapsed)
                .round()
                .min(u32::MAX as f64) as u32,
            max_jitter_us: self
                .max_jitter_us
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

#[cfg(target_os = "windows")]
struct MidiThreadScope;

#[cfg(target_os = "windows")]
impl MidiThreadScope {
    fn enter() -> Self {
        unsafe {
            let _ = timeBeginPeriod(1);
            set_current_thread_priority_high();
        }
        Self
    }
}

#[cfg(target_os = "windows")]
impl Drop for MidiThreadScope {
    fn drop(&mut self) {
        unsafe {
            let _ = timeEndPeriod(1);
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct MidiThreadScope;

#[cfg(not(target_os = "windows"))]
#[cfg_attr(target_os = "macos", allow(dead_code))]
impl MidiThreadScope {
    fn enter() -> Self {
        #[cfg(target_os = "linux")]
        promote_linux_midi_thread();
        Self
    }
}

#[cfg(target_os = "windows")]
fn wait_for_midi_tick(duration: Duration) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, SetWaitableTimer,
        TIMER_ALL_ACCESS, WaitForSingleObject,
    };

    struct HighResolutionTimer(windows::Win32::Foundation::HANDLE);

    impl Drop for HighResolutionTimer {
        fn drop(&mut self) {
            // SAFETY: this wrapper uniquely owns the handle.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    thread_local! {
        // One timer per MIDI scheduler thread. Reusing it avoids a kernel
        // handle create/close pair on every 1 ms scheduling slice.
        static TIMER: Option<HighResolutionTimer> = unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .ok()
            .map(HighResolutionTimer)
        };
    }

    let due_100ns = -(duration.as_nanos().min(i64::MAX as u128 / 100) as i64 / 100).max(-1);
    let waited = TIMER.with(|timer| {
        let Some(timer) = timer else {
            return false;
        };
        let mut due = due_100ns;
        // SAFETY: the timer remains alive for the entire TLS closure and the
        // due-time pointer is valid for the call.
        unsafe {
            SetWaitableTimer(timer.0, &mut due, 0, None, None, false).is_ok()
                && WaitForSingleObject(timer.0, u32::MAX) == WAIT_OBJECT_0
        }
    });
    if !waited {
        // Rare old-Windows / resource-exhaustion fallback. `park_timeout`
        // avoids returning to `thread::sleep` while retaining cancellation
        // checks at the caller's bounded 1 ms cadence.
        std::thread::park_timeout(duration);
    }
}

#[cfg(unix)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn wait_for_midi_tick(duration: Duration) {
    // POSIX nanosleep is backed by the platform's high-resolution monotonic
    // timer and preserves the remaining interval across signal interruption.
    // The caller recomputes the musical deadline after every bounded wait, so
    // it cannot accumulate drift.
    let mut request = libc::timespec {
        tv_sec: duration.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: duration.subsec_nanos() as libc::c_long,
    };
    loop {
        let mut remaining = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: both pointers reference initialized timespec values.
        if unsafe { libc::nanosleep(&request, &mut remaining) } == 0 {
            break;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            break;
        }
        request = remaining;
    }
}

#[cfg(not(any(target_os = "windows", unix)))]
fn wait_for_midi_tick(duration: Duration) {
    std::thread::park_timeout(duration);
}

#[cfg(target_os = "linux")]
fn promote_linux_midi_thread() {
    const FIFO_PRIORITIES: [libc::c_int; 3] = [60, 20, 5];
    const FALLBACK_NICE: libc::c_int = -8;
    for priority in FIFO_PRIORITIES {
        // SAFETY: the structure is fully initialized and pid 0 is the calling
        // thread on Linux.
        let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
        param.sched_priority = priority;
        if unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) } == 0 {
            eprintln!("[midi-output] scheduler=SCHED_FIFO priority={priority}");
            return;
        }
    }
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, FALLBACK_NICE) } == 0 {
        eprintln!(
            "[midi-output] realtime permission denied; scheduler=SCHED_OTHER nice={FALLBACK_NICE}"
        );
    } else {
        eprintln!(
            "[midi-output] realtime permission denied and nice fallback unavailable: {}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(target_os = "windows")]
unsafe fn set_current_thread_priority_high() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
    }
}

#[cfg(target_os = "windows")]
#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(uperiod: u32) -> u32;
    fn timeEndPeriod(uperiod: u32) -> u32;
}

pub fn migrate_legacy_midi_settings(midi: &mut MidiHardwareSettings) {
    if !midi.devices.is_empty() {
        midi.enabled_inputs.clear();
        midi.enabled_outputs.clear();
        return;
    }

    let mut devices = Vec::new();
    for name in &midi.enabled_inputs {
        devices.push(MidiDeviceSetting {
            id: stable_id(MidiDeviceDirection::Input, name),
            name: name.clone(),
            direction: MidiDeviceDirection::Input,
            enabled: true,
            connected: true,
            clock_enabled: false,
        });
    }
    for name in &midi.enabled_outputs {
        devices.push(MidiDeviceSetting {
            id: stable_id(MidiDeviceDirection::Output, name),
            name: name.clone(),
            direction: MidiDeviceDirection::Output,
            enabled: true,
            connected: true,
            clock_enabled: midi.clock_sync,
        });
    }
    if !devices.is_empty() {
        midi.devices = devices;
    }
    midi.enabled_inputs.clear();
    midi.enabled_outputs.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_keeps_missing_saved_devices() {
        let saved = vec![MidiDeviceSetting {
            id: "midi-in-old".to_string(),
            name: "Old Controller".to_string(),
            direction: MidiDeviceDirection::Input,
            enabled: true,
            connected: true,
            clock_enabled: false,
        }];
        let detected = vec![DetectedMidiDevice {
            id: "midi-in-new".to_string(),
            name: "New Controller".to_string(),
            direction: MidiDeviceDirection::Input,
        }];
        let resolved = resolve_midi_devices(&saved, &detected);
        assert_eq!(resolved.len(), 2);
        assert!(
            resolved
                .iter()
                .any(|d| d.id == "midi-in-old" && !d.connected)
        );
        assert!(
            resolved
                .iter()
                .any(|d| d.id == "midi-in-new" && d.connected && d.enabled)
        );
    }

    #[test]
    fn coalesce_merges_same_name_input_and_output() {
        let detected = vec![
            DetectedMidiDevice {
                id: "midi-in-pad".to_string(),
                name: "Pad Controller".to_string(),
                direction: MidiDeviceDirection::Input,
            },
            DetectedMidiDevice {
                id: "midi-out-pad".to_string(),
                name: "Pad Controller".to_string(),
                direction: MidiDeviceDirection::Output,
            },
            DetectedMidiDevice {
                id: "midi-in-keys".to_string(),
                name: "Keys".to_string(),
                direction: MidiDeviceDirection::Input,
            },
        ];
        let coalesced = coalesce_detected_midi_devices(detected);
        assert_eq!(coalesced.len(), 2);
        let pad = coalesced
            .iter()
            .find(|d| d.name == "Pad Controller")
            .expect("pad");
        assert_eq!(pad.direction, MidiDeviceDirection::InputOutput);
        assert!(pad.id.starts_with("midi-io-"));
        let keys = coalesced.iter().find(|d| d.name == "Keys").expect("keys");
        assert_eq!(keys.direction, MidiDeviceDirection::Input);
    }

    #[test]
    fn resolve_dedupes_saved_clone_when_live_name_matches() {
        let saved = vec![MidiDeviceSetting {
            id: "midi-in-pad".to_string(),
            name: "Pad Controller".to_string(),
            direction: MidiDeviceDirection::Input,
            enabled: true,
            connected: false,
            clock_enabled: false,
        }];
        let detected = vec![DetectedMidiDevice {
            id: "midi-io-pad-controller".to_string(),
            name: "Pad Controller".to_string(),
            direction: MidiDeviceDirection::InputOutput,
        }];
        let resolved = resolve_midi_devices(&saved, &detected);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "midi-io-pad-controller");
        assert!(resolved[0].enabled);
        assert!(resolved[0].connected);
    }

    #[test]
    fn migrate_legacy_inputs_outputs() {
        let mut midi = MidiHardwareSettings {
            devices: Vec::new(),
            clock_sync: true,
            enabled_inputs: vec!["Keyboard Controller".to_string()],
            enabled_outputs: vec!["Interface".to_string()],
        };
        migrate_legacy_midi_settings(&mut midi);
        assert_eq!(midi.devices.len(), 2);
        assert!(midi.enabled_inputs.is_empty());
        assert!(midi.enabled_outputs.is_empty());
    }

    fn hw_event(sample: u64, message: &[u8]) -> HardwareMidiEvent {
        HardwareMidiEvent {
            device_id: "midi-out-test".to_string(),
            delay_seconds: sample as f64 / 48_000.0,
            beat: sample as f64 / 24_000.0,
            absolute_sample: sample,
            message: message.to_vec(),
        }
    }

    #[test]
    fn hardware_sort_sends_note_off_before_note_on_at_same_sample() {
        let mut events = vec![
            hw_event(100, &[0x90, 60, 100]),
            hw_event(100, &[0xb0, 1, 64]),
            hw_event(100, &[0x80, 60, 0]),
        ];
        sort_hardware_midi_events(&mut events);
        assert_eq!(events[0].message, vec![0x80, 60, 0]);
        assert_eq!(events[1].message, vec![0xb0, 1, 64]);
        assert_eq!(events[2].message, vec![0x90, 60, 100]);
    }

    #[test]
    fn hardware_coalescing_drops_dense_cc_and_pitch_bend_but_keeps_notes() {
        let events = vec![
            hw_event(100, &[0x90, 60, 100]),
            hw_event(101, &[0xb0, 1, 10]),
            hw_event(102, &[0xb0, 1, 10]),
            hw_event(103, &[0xb0, 1, 20]),
            hw_event(104, &[0xe0, 0, 64]),
            hw_event(105, &[0xe0, 0, 64]),
            hw_event(106, &[0x80, 60, 0]),
        ];
        let coalesced = coalesce_hardware_midi_events(events, 48_000);
        assert!(
            coalesced
                .iter()
                .any(|event| event.message == vec![0x90, 60, 100])
        );
        assert!(
            coalesced
                .iter()
                .any(|event| event.message == vec![0x80, 60, 0])
        );
        assert_eq!(
            coalesced
                .iter()
                .filter(|event| event.message.first().copied().unwrap_or(0) & 0xf0 == 0xb0)
                .count(),
            1
        );
        assert_eq!(
            coalesced
                .iter()
                .filter(|event| event.message.first().copied().unwrap_or(0) & 0xf0 == 0xe0)
                .count(),
            1
        );
    }
}
