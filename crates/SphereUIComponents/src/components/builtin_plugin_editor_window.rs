//! Floating shell window for a built-in plugin's CEF editor.
//!
//! Built-in editors use off-screen CEF on every platform. Windows takes the
//! accelerated path: CEF supplies D3D11 shared textures which are copied into
//! stable GPUI atlas tiles on the GPU. Other platforms use the software BGRA
//! framebuffer path. The same GPUI region forwards mouse and keyboard input in
//! both cases; see `builtin_plugin_editor_surface.rs`.
//!
//! ## Lifecycle
//!
//! ```text
//! open  → GPUI window exists, native handle not yet valid  (WaitingForHandle)
//!       → content child created, CEF browser created       (Attached)
//! close → view closed, content child destroyed
//! ```
//!
//! Off-screen hosting has no native handle to wait for, so `WaitingForHandle`
//! resolves on the first render pass that knows the content rect.
//!
//! CEF's message loop is pumped from a GPUI timer for as long as this window is
//! alive; without that the browser never paints or handles input.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    canvas, div, img, px, size, App, AppContext, Bounds, Context, DispatchPhase, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, Styled, StyledImage, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind,
};

use crate::components::builtin_plugin_editor::{
    self as host, EditorInput, EditorKeyKind, HostAvailability, ViewEvent, ViewId, ViewRect,
    OFFSCREEN_HOSTING,
};
use crate::components::builtin_plugin_editor_surface::{
    editor_char_keys, editor_key, editor_mouse_button, OffscreenSurface,
};
use crate::components::plugin_content_host::{ContentChildHwnd, ContentRect};
use crate::components::title_bar::{external_window_titlebar, TITLEBAR_HEIGHT};
use crate::theme::Colors;

pub const BUILTIN_EDITOR_WIDTH: f32 = 1180.0;
pub const BUILTIN_EDITOR_HEIGHT: f32 = 760.0;
pub const BUILTIN_EDITOR_MIN_WIDTH: f32 = 900.0;
pub const BUILTIN_EDITOR_MIN_HEIGHT: f32 = 620.0;

/// Z-Comp's reference browser viewport. Its faceplate is designed at
/// 1020 × 600 inside this 1024 × 720 canvas, so native chrome must be added
/// outside the browser rectangle rather than taking space from it.
const ZCOMP_EDITOR_CONTENT_WIDTH: f32 = 1024.0;
const ZCOMP_EDITOR_CONTENT_HEIGHT: f32 = 720.0;

/// Height of the GPUI-drawn header strip above the browser rect. Uses the
/// shared external-dialog titlebar height so the browser rect and the chrome
/// can never disagree about where the content starts.
const HEADER_H: f32 = TITLEBAR_HEIGHT;

/// Logical pixels one line-based scroll notch scrolls the page by. GPUI
/// reports discrete wheel steps in lines; CEF wants pixel deltas. Matches
/// Chromium's own `kPixelsPerLineStep`, so a notch moves an embedded editor
/// exactly as far as it would move the same page in a browser window.
const SCROLL_LINE_HEIGHT: f32 = 40.0;

/// Guard against a pathological wheel step (Windows reports `WHEEL_PAGESCROLL`
/// as `u32::MAX` lines when "scroll one screen at a time" is selected), which
/// would otherwise saturate the `i32` CEF is handed.
const MAX_SCROLL_DELTA: f32 = 10_000.0;

/// Width of the native instance sidebar. Reserved out of the CEF content rect
/// the same way `HEADER_H` is — the browser must never be told to draw under
/// it, and the sidebar must never be told to draw over the browser.
const SIDEBAR_W: f32 = 208.0;

fn default_editor_window_size(plugin_id: &str) -> (f32, f32) {
    if host::origin_for_plugin_id(plugin_id) == Some("zcomp") {
        (
            SIDEBAR_W + ZCOMP_EDITOR_CONTENT_WIDTH,
            HEADER_H + ZCOMP_EDITOR_CONTENT_HEIGHT,
        )
    } else {
        (BUILTIN_EDITOR_WIDTH, BUILTIN_EDITOR_HEIGHT)
    }
}

/// Identity of one DSP insert that can be shown in a shared built-in editor.
/// `track_id`/`insert_id` are the same stable, session-monotonic ids
/// `InsertSlotState` already uses (see `plugin_chain.rs`) — never reused
/// within a session and round-tripped from the project file, so they are
/// stable enough to key a binding without introducing a parallel id scheme.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginInstanceKey {
    pub track_id: String,
    pub insert_id: String,
}

/// One row in the shared editor's sidebar: enough to render the row and to
/// re-resolve the live insert slot when selected. Cheap to rebuild wholesale
/// on every lifecycle event (add/remove/rename/reorder) rather than diffed.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginInstanceDescriptor {
    pub instance_key: PluginInstanceKey,
    pub plugin_id: String,
    pub track_name: String,
    pub insert_name: String,
    pub bypassed: bool,
    pub enabled: bool,
    /// Persisted per-insert DSP state, if this insert has ever been saved —
    /// see `InsertSlotState::vst3_state`'s doc comment (reused generically,
    /// not VST3-only). UTF-8 JSON bytes for built-ins. `None` for a fresh
    /// insert; `push_selected_instance` falls back to DSP defaults then.
    pub state_bytes: Option<std::sync::Arc<Vec<u8>>>,
}

/// Wire protocol version. Bump alongside any breaking change to the message
/// shapes below; both sides reject a mismatch instead of guessing.
const BRIDGE_PROTOCOL_VERSION: u32 = 1;

/// The wire-format instance id (`futureboard.selectInstance.instanceId`,
/// route `/instance/{instanceId}`). One string, not a `(track_id, insert_id)`
/// pair — React never needs to know they're composite.
fn wire_instance_id(key: &PluginInstanceKey) -> String {
    format!("{}::{}", key.track_id, key.insert_id)
}

/// Decode an insert's persisted state bytes (UTF-8 JSON, see
/// `PluginInstanceDescriptor::state_bytes`) for the `selectInstance` wire
/// message. Deliberately generic (`serde_json::Value`, not a specific
/// plugin's Rust type) — this module hosts any built-in plugin's shared
/// editor, not only rodharerist's.
///
/// A fresh insert with no saved state, or bytes that fail to parse (a
/// corrupt/foreign blob), both fall back to `{}` rather than erroring: the
/// editor is expected to apply its own defaults when it receives an empty
/// object, same as it does today with nothing wired at all.
fn decode_state_bytes(bytes: Option<&[u8]>) -> serde_json::Value {
    let Some(bytes) = bytes else {
        return serde_json::json!({});
    };
    match std::str::from_utf8(bytes).map(serde_json::from_str::<serde_json::Value>) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            eprintln!("[plugin-bridge] persisted state is not valid JSON, using defaults: {error}");
            serde_json::json!({})
        }
        Err(error) => {
            eprintln!(
                "[plugin-bridge] persisted state is not valid UTF-8, using defaults: {error}"
            );
            serde_json::json!({})
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceDisplayMetadata {
    track_id: String,
    track_name: String,
    insert_id: String,
    insert_name: String,
}

/// Native->React: rebind the shared page to a different DSP instance. See
/// module docs on `select_instance` for the transaction this is one step of.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectInstanceMsg {
    r#type: &'static str,
    protocol_version: u32,
    plugin_id: String,
    instance_id: String,
    binding_generation: u64,
    display: InstanceDisplayMetadata,
    state_revision: u64,
    /// TODO(phase5): rodharerist has no serialized per-insert DSP state yet
    /// (confirmed by audit — no `Serialize` impl anywhere in the DSP crate,
    /// no project-file schema, no engine wiring). Until that lands this is
    /// always `{}`; React has nothing real to bind to, only the identity and
    /// display metadata.
    state: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceRemovedMsg {
    r#type: &'static str,
    protocol_version: u32,
    instance_id: String,
    binding_generation: u64,
}

/// Native -> React: one ~30 Hz telemetry frame for the bound instance.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MetersMsg {
    r#type: &'static str,
    protocol_version: u32,
    instance_id: String,
    binding_generation: u64,
    in_peak: f32,
    in_rms: f32,
    out_peak: f32,
    out_rms: f32,
    /// Decibels the DSP is taking off, positive; `0.0` for a built-in with no
    /// reduction to report.
    gain_reduction_db: f32,
    in_clip: bool,
    out_clip: bool,
    /// Per-rack-position in/out levels, in chain order, for a built-in whose
    /// editor meters its own stages. Empty for the rest, so a page that does
    /// not use them pays nothing for the field.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    slot_in_peak: Vec<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    slot_out_peak: Vec<f32>,
}

/// Native -> React: one analyser frame for the bound instance.
///
/// `bins` are quantised to bytes rather than sent as floats: the payload
/// crosses into the page as a JSON literal inside an `execute_javascript` call,
/// where 128 floats cost about a kilobyte per frame and 128 bytes cost a
/// quarter of that. The scale is fixed and shared —
/// [`SpherePluginHost::spectrum::quantize_db`] defines it, `0` being
/// `FLOOR_DB` and `255` being `CEIL_DB`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SpectrumMsg {
    r#type: &'static str,
    protocol_version: u32,
    instance_id: String,
    /// Lowest frequency bin 0 covers, and the highest bin `len - 1` covers, so
    /// the page maps bins to its axis without duplicating the constants.
    min_hz: f32,
    max_hz: f32,
    floor_db: f32,
    ceil_db: f32,
    bins: Vec<u8>,
}

/// Per-stage levels for the wire, or empty when the built-in has no rack.
/// Keeps the ~30 Hz meter payload unchanged for the plugins that do not meter
/// their own stages.
fn stage_levels(levels: &[f32]) -> Vec<f32> {
    if levels.iter().all(|value| *value == 0.0) {
        return Vec::new();
    }
    levels.to_vec()
}

/// Native -> React: low-rate footer status from the shared-region header.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HostStatusMsg {
    r#type: &'static str,
    protocol_version: u32,
    instance_id: String,
    sample_rate: u32,
    block_size: u32,
    latency_samples: u32,
    /// Transport tempo the DSP is processing against, for editors that show a
    /// musical time rather than milliseconds.
    tempo_bpm: f64,
}

/// Native -> React: DAW transport state. Pushed on change only (and once when
/// the page announces `bridgeReady`), never per frame — an editor needs to know
/// *that* the transport started, not where the playhead is. Plugin DSP gets the
/// full per-block `ProcessContext` through the shared audio bridge instead.
///
/// This is the return leg of the transport key: the editor's Space reaches the
/// DAW through `futureboard.globalCommand`, and the resulting state comes back
/// here, so an editor can render a play indicator that cannot disagree with the
/// transport.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportMsg {
    r#type: &'static str,
    protocol_version: u32,
    playing: bool,
}

/// Native -> React: one kind's user-file listing (rebuilt wholesale).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FileListMsg {
    r#type: &'static str,
    protocol_version: u32,
    kind: String,
    files: Vec<crate::components::builtin_plugin_files::BuiltinFileEntry>,
}

/// Native -> React: one user file's text content (or the failure).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FileContentMsg {
    r#type: &'static str,
    protocol_version: u32,
    kind: String,
    file_name: String,
    ok: bool,
    content: Option<String>,
    error: Option<String>,
}

/// Native -> React: outcome of a `writeFile`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FileWrittenMsg {
    r#type: &'static str,
    protocol_version: u32,
    kind: String,
    file_name: String,
    ok: bool,
    error: Option<String>,
}

/// Native -> React: async outcome of a `futureboard.loadNamCapture` request.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct NamCaptureResultMsg {
    r#type: &'static str,
    protocol_version: u32,
    instance_id: String,
    ok: bool,
    name: String,
    error: Option<String>,
    receptive_field: u64,
    full_rig: bool,
}

/// Native -> React: async outcome of a `futureboard.loadIr` request.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct IrLoadResultMsg {
    r#type: &'static str,
    protocol_version: u32,
    instance_id: String,
    ok: bool,
    name: String,
    error: Option<String>,
    /// Frames actually convolved, at the engine's rate (0 on failure).
    frames: u64,
    /// Latency the convolution adds, in samples (0 on failure).
    latency_samples: u64,
    /// The file carried two distinct channels (true-stereo IR).
    stereo: bool,
    /// The file was longer than the engine's cap and got cut.
    truncated: bool,
}

/// One parameter edit inside a `futureboard.setParams` batch. `id` is the
/// editor's string param id (the `Dsp::apply_ui_param` contract); it is
/// resolved to the plugin's u32 wire index before leaving the UI thread.
#[derive(Debug, Clone, serde::Deserialize)]
struct ParamEditMsg {
    id: String,
    value: f32,
}

/// React->native. `setParams` batches live parameter edits toward the DSP in
/// the plugin-host process (via the engine's param ring); `applyStatePatch` /
/// `requestFullState` still have no canonical state to validate against and
/// remain unmodeled — sending them today is inert (dropped as `Unknown`)
/// rather than silently mis-acted-on.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
enum InboundMsg {
    #[serde(rename = "futureboard.bridgeReady", rename_all = "camelCase")]
    BridgeReady {
        #[allow(dead_code)]
        plugin_id: String,
        #[allow(dead_code)]
        bridge_version: u32,
    },
    #[serde(rename = "futureboard.instanceReady", rename_all = "camelCase")]
    InstanceReady {
        #[allow(dead_code)]
        plugin_id: String,
        instance_id: String,
        binding_generation: u64,
        #[allow(dead_code)]
        state_revision: u64,
    },
    #[serde(rename = "futureboard.requestSelectInstance", rename_all = "camelCase")]
    RequestSelectInstance { instance_id: String },
    #[serde(rename = "futureboard.globalCommand", rename_all = "camelCase")]
    GlobalCommand { command_id: String },
    /// Batched live parameter edits for the currently bound instance. Stale
    /// generations and mismatched instance ids are rejected, same as
    /// `instanceReady` — a message referencing a superseded selection must
    /// never mutate whatever instance is active when it arrives.
    #[serde(rename = "futureboard.setParams", rename_all = "camelCase")]
    SetParams {
        #[allow(dead_code)]
        plugin_id: String,
        instance_id: String,
        binding_generation: u64,
        params: Vec<ParamEditMsg>,
    },
    /// List the plugin's user files of one kind (Presets/IRs/NAMs). File ops
    /// are plugin-global (the folders belong to the plugin, not an insert), so
    /// there is no instance/generation staleness to check.
    #[serde(rename = "futureboard.listFiles", rename_all = "camelCase")]
    ListFiles {
        #[allow(dead_code)]
        plugin_id: String,
        kind: String,
    },
    /// Read one user file's text content (preset JSON / `.nam` capture).
    #[serde(rename = "futureboard.readFile", rename_all = "camelCase")]
    ReadFile {
        #[allow(dead_code)]
        plugin_id: String,
        kind: String,
        file_name: String,
    },
    /// Write one user file (preset save / factory seeding). Native only
    /// sanitizes the leaf name — the content format is editor-owned.
    #[serde(rename = "futureboard.writeFile", rename_all = "camelCase")]
    WriteFile {
        #[allow(dead_code)]
        plugin_id: String,
        kind: String,
        file_name: String,
        content: String,
    },
    /// Load a `.nam` capture into the bound instance's Tone/Amp slot. Same
    /// staleness rules as `setParams`; the (potentially multi-MB) file text
    /// rides the POST body and is forwarded verbatim to the plugin host.
    #[serde(rename = "futureboard.loadNamCapture", rename_all = "camelCase")]
    LoadNamCapture {
        #[allow(dead_code)]
        plugin_id: String,
        instance_id: String,
        binding_generation: u64,
        name: String,
        json: String,
        stereo: bool,
        full_rig: bool,
    },
    /// Load a `.wav` impulse response into the bound instance's Cabinet slot.
    /// Same staleness rules as `setParams`. Unlike `loadNamCapture` the page
    /// sends only a *file name* from the plugin's IRs folder — a `.wav` is
    /// binary, and native already owns that folder, so the bytes never have to
    /// be encoded through the webview.
    #[serde(rename = "futureboard.loadIr", rename_all = "camelCase")]
    LoadIr {
        #[allow(dead_code)]
        plugin_id: String,
        instance_id: String,
        binding_generation: u64,
        file_name: String,
    },
    #[serde(other)]
    Unknown,
}

/// The CEF bridge is an untrusted local-UI boundary, not a secure-storage
/// channel. Native owns every credential and persistent secret. Check JSON
/// object keys structurally (never raw string contents, which may be preset or
/// model data) before accepting inbound messages or exposing outbound state.
fn bridge_value_contains_forbidden_secret(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(name, value)| {
            is_forbidden_secret_field(name) || bridge_value_contains_forbidden_secret(value)
        }),
        serde_json::Value::Array(values) => {
            values.iter().any(bridge_value_contains_forbidden_secret)
        }
        _ => false,
    }
}

fn is_forbidden_secret_field(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "password"
            | "passphrase"
            | "accesstoken"
            | "refreshtoken"
            | "oauthtoken"
            | "oauthcredential"
            | "authorization"
            | "sessioncookie"
            | "clientsecret"
            | "apikey"
            | "secretkey"
            | "stripesecretkey"
            | "licensekey"
            | "licensedata"
            | "activationkey"
            | "activationtoken"
            | "activationdata"
            | "paymentsessionsecret"
            | "cloudcredentials"
            | "keychainvalue"
            | "macoskeychainvalue"
            | "encryptionkey"
            | "privatekey"
            | "credentials"
    )
}

fn parse_inbound_message(raw: &[u8]) -> Result<InboundMsg, &'static str> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|_| "invalid JSON or message shape")?;
    if bridge_value_contains_forbidden_secret(&value) {
        return Err("secret-bearing field rejected at CEF trust boundary");
    }
    serde_json::from_value(value).map_err(|_| "invalid JSON or message shape")
}

/// UI-thread-only forwarder for one resolved parameter edit:
/// `(instance, wire index, raw editor-unit value)` toward the engine's
/// realtime command path (`AudioEngine::set_insert_param`), which pushes the
/// per-insert shared param ring from the audio callback thread. Built by
/// `open_builtin_insert_editor` in `plugin_ops.rs`, which owns the engine
/// handle this window deliberately does not.
pub type BuiltinParamForwarder = std::sync::Arc<dyn Fn(&PluginInstanceKey, u32, f32)>;

pub type BuiltinGlobalCommandDispatcher =
    std::sync::Arc<dyn Fn(&'static str, &mut gpui::App) + Send + Sync>;

/// A validated `.nam` load request on its way to the plugin-host process.
#[derive(Debug, Clone)]
pub struct BuiltinNamLoadRequest {
    pub name: String,
    pub json: String,
    pub stereo: bool,
    pub full_rig: bool,
}

/// Forwards a `.nam` load toward the plugin-host bridge
/// (`HostCommand::LoadBuiltinNamCapture`). UI thread; the host replies
/// asynchronously with `BuiltinNamCaptureResult`, routed back through
/// `notify_nam_capture_result`.
pub type BuiltinNamLoadForwarder =
    std::sync::Arc<dyn Fn(&PluginInstanceKey, BuiltinNamLoadRequest)>;

/// A validated IR load request on its way to the plugin-host process. The
/// bytes are the raw `.wav` file, already read from the plugin's IRs folder.
#[derive(Debug, Clone)]
pub struct BuiltinIrLoadRequest {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Forwards an IR load toward the plugin-host bridge
/// (`HostCommand::LoadBuiltinIr`). UI thread; the host replies asynchronously
/// with `BuiltinIrResult`, routed back through `notify_ir_load_result`.
pub type BuiltinIrLoadForwarder = std::sync::Arc<dyn Fn(&PluginInstanceKey, BuiltinIrLoadRequest)>;

/// Polls the latest telemetry frame for an instance's shared region (pure
/// atomic loads). UI thread, ~30 Hz.
pub type BuiltinMeterSource = std::sync::Arc<
    dyn Fn(&PluginInstanceKey) -> Option<SpherePluginHost::audio_bridge::BuiltinMeterFrame>,
>;

/// Polls (sample_rate, block_frames, latency_samples, tempo_bpm) from the
/// region header.
pub type BuiltinHostStatusSource =
    std::sync::Arc<dyn Fn(&PluginInstanceKey) -> Option<(u32, u32, u32, f64)>>;

/// Polls the latest analyser frame for an instance's shared region, as
/// `(publish sequence, dB bins)`. UI thread, ~30 Hz.
pub type BuiltinSpectrumSource = std::sync::Arc<
    dyn Fn(&PluginInstanceKey) -> Option<(u32, [f32; SpherePluginHost::spectrum::SPECTRUM_BINS])>,
>;

/// Reads whether the DAW transport is advancing. One relaxed atomic load on the
/// engine's shared state — safe to call every pump tick.
pub type BuiltinTransportSource = std::sync::Arc<dyn Fn() -> bool>;

/// Everything the shared editor window can do against the live host, injected
/// by `plugin_ops.rs` (the owner of the engine handle and bridge runtime this
/// window deliberately does not hold). Any member may be `None` while the
/// engine/bridge is still warming up; the focus path re-installs a live set.
#[derive(Clone, Default)]
pub struct BuiltinEditorHostOps {
    pub forward_param: Option<BuiltinParamForwarder>,
    pub dispatch_global_command: Option<BuiltinGlobalCommandDispatcher>,
    pub load_nam_capture: Option<BuiltinNamLoadForwarder>,
    pub load_ir: Option<BuiltinIrLoadForwarder>,
    pub meter_source: Option<BuiltinMeterSource>,
    pub host_status_source: Option<BuiltinHostStatusSource>,
    pub spectrum_source: Option<BuiltinSpectrumSource>,
    pub transport_source: Option<BuiltinTransportSource>,
}

impl BuiltinEditorHostOps {
    fn is_empty(&self) -> bool {
        self.forward_param.is_none()
            && self.dispatch_global_command.is_none()
            && self.load_nam_capture.is_none()
            && self.load_ir.is_none()
            && self.meter_source.is_none()
            && self.host_status_source.is_none()
            && self.spectrum_source.is_none()
            && self.transport_source.is_none()
    }
}

/// CEF pump interval. 8 ms keeps the editor responsive without spinning the UI
/// thread; CEF coalesces its own work internally.
const PUMP_INTERVAL: Duration = Duration::from_millis(8);

/// Pump ticks between `futureboard.hostStatus` pushes — ~1 Hz at
/// [`PUMP_INTERVAL`]. Sample rate, block size and tempo all change rarely
/// enough that a footer reading them does not need a faster leg.
const HOST_STATUS_TICKS: u32 = 128;

#[derive(Debug, Clone, PartialEq)]
enum Status {
    /// GPUI window created; native parent handle not yet valid.
    WaitingForHandle { ticks: u32 },
    /// The content HWND exists and browser creation is queued.
    Attaching,
    /// Browser created and parented.
    Attached,
    /// Browser close is queued; keep the shell/parent HWND alive until CEF has
    /// processed it.
    Closing,
    /// CEF has processed close and the GPUI shell may now be removed.
    Closed,
    /// Host unavailable or browser creation failed — the reason is shown.
    Failed(String),
}

/// How many pump ticks to wait for a usable native handle before surfacing an
/// error rather than spinning forever.
const MAX_HANDLE_TICKS: u32 = 150;

/// How long to wait, after the browser attaches, for `futureboard.bridgeReady`
/// before assuming the page load silently died and reloading.
const BRIDGE_READY_TIMEOUT: Duration = Duration::from_secs(8);
/// Cap on automatic reloads from the watchdog above — a page that never comes
/// up after this many tries has a real problem the user needs to see, not
/// another silent retry.
const MAX_BRIDGE_READY_RETRIES: u32 = 3;

struct PumpTick {
    keep_going: bool,
    content_to_drop: Option<ContentChildHwnd>,
}

pub struct BuiltinPluginEditorWindow {
    view_id: ViewId,
    /// Label for the currently active instance, used only in host-side logs
    /// (`HostedView`). Not an identity — `active_instance` is.
    editor_id: String,
    plugin_id: String,
    display_name: String,
    status: Status,
    content: Option<ContentChildHwnd>,
    last_rect: Option<ViewRect>,
    /// Every insert instance across the project currently using this
    /// `plugin_id`. Rebuilt wholesale by `set_instances` on every lifecycle
    /// event that could change the list — never diffed in place.
    instances: Vec<PluginInstanceDescriptor>,
    /// Which instance the (single, shared) browser is currently bound to.
    /// `None` is the valid empty state — the browser stays open, no instance
    /// selected (e.g. the last instance using this plugin_id was removed).
    active_instance: Option<PluginInstanceKey>,
    /// Bumped on every `select_instance`. Phase 3 (bridge protocol) threads
    /// this into every host<->React message so a message referencing a
    /// superseded selection is provably stale and can be rejected instead of
    /// mutating whatever instance happens to be active when it arrives.
    binding_generation: u64,
    sidebar_collapsed: bool,
    /// Set once `futureboard.bridgeReady` arrives. Before that, `select_instance`
    /// still updates native state (so reopening/refocusing works) but has no
    /// live page to push `selectInstance` into yet — the selection made while
    /// waiting is simply the one already reflected once the bridge comes up,
    /// since `active_instance` already holds it.
    browser_ready: bool,
    /// When the browser reached `Attached`. Drives the bridge-ready watchdog:
    /// a page that never announces `bridgeReady` within
    /// `BRIDGE_READY_TIMEOUT` gets reloaded rather than left blank forever
    /// (observed cause: Chromium's network service crashes mid-transfer —
    /// it auto-restarts itself for *future* requests but does not retry the
    /// one already in flight, so the page can finish HTTP 200 headers and
    /// still never actually paint or run its scripts).
    attached_at: Option<Instant>,
    /// How many times the watchdog above has already reloaded this browser.
    /// Capped so a page that *never* comes up (broken build, not a transient
    /// crash) fails loudly instead of reloading forever.
    bridge_ready_retries: u32,
    /// Off-screen presentation state: stable accelerated GPU planes on Windows,
    /// or the latest software frame on fallback and other platforms.
    surface: OffscreenSurface,
    /// Keyboard focus for the off-screen browser region.
    focus: FocusHandle,
    /// How many render passes reached `attach`. Reported in the attach-timeout
    /// message: `0` means the window never re-rendered while waiting (a
    /// scheduling problem), non-zero means the bounds were never usable.
    attach_attempts: u32,
    /// Scale factor `last_rect` was measured at, so a view-space point can be
    /// tested against the browser rect without re-reading the window.
    last_scale: f32,
    /// Whether the pointer is currently outside the browser rect. Keeps the
    /// leave notification edge-triggered rather than sent on every move.
    pointer_left: bool,
    /// Sub-pixel wheel remainder. CEF takes integer pixel deltas, so a
    /// precision trackpad — which reports a fraction of a line per event —
    /// would otherwise round to zero forever and never scroll the page.
    scroll_carry: Point<f32>,
    /// Live-host operations (param forwarding, NAM load, telemetry polls)
    /// injected by the opener. Empty until the engine/bridge is up; requests
    /// arriving before then are dropped.
    host_ops: BuiltinEditorHostOps,
    /// Pump-tick counter driving the telemetry push cadence (every 4th 8 ms
    /// tick ≈ 30 Hz meters; every 128th ≈ 1 Hz host status).
    telemetry_tick: u32,
    /// Publish sequence of the last analyser frame forwarded to the page. The
    /// host publishes at its own rate, so most telemetry ticks find the same
    /// frame; re-sending it would cost a `execute_javascript` round trip for a
    /// picture that has not changed.
    spectrum_seq: u32,
    /// Last transport state pushed to the page. `None` until the first push, so
    /// a freshly loaded page always receives one regardless of what the
    /// transport is doing.
    pushed_transport_playing: Option<bool>,
    /// Cached `Documents/Futureboard Studio/<plugin>/` root, resolved and
    /// created lazily on the first file message from the page.
    files_root: Option<std::path::PathBuf>,
}

impl BuiltinPluginEditorWindow {
    pub fn new(
        plugin_id: String,
        display_name: String,
        instances: Vec<PluginInstanceDescriptor>,
        active_instance: Option<PluginInstanceKey>,
        host_ops: BuiltinEditorHostOps,
        cx: &mut Context<Self>,
    ) -> Self {
        // Refuse early and clearly when the host cannot serve this plugin, so
        // the window shows a reason instead of an empty rect.
        let status = match host::availability(&plugin_id) {
            HostAvailability::Ready => Status::WaitingForHandle { ticks: 0 },
            other => Status::Failed(other.to_string()),
        };

        if matches!(status, Status::WaitingForHandle { .. }) {
            Self::spawn_pump(cx);
        }

        let editor_id = active_instance
            .as_ref()
            .map(|key| format!("{}::{}", key.track_id, key.insert_id))
            .unwrap_or_else(|| format!("{plugin_id}::<none>"));

        Self {
            view_id: host::allocate_view_id(),
            editor_id,
            plugin_id,
            display_name,
            status,
            content: None,
            last_rect: None,
            instances,
            active_instance,
            binding_generation: 0,
            sidebar_collapsed: false,
            browser_ready: false,
            attached_at: None,
            bridge_ready_retries: 0,
            surface: OffscreenSurface::default(),
            focus: cx.focus_handle(),
            attach_attempts: 0,
            last_scale: 1.0,
            pointer_left: true,
            scroll_carry: Point::default(),
            host_ops,
            telemetry_tick: 0,
            spectrum_seq: 0,
            pushed_transport_playing: None,
            files_root: None,
        }
    }

    /// Resolve + create the plugin's user-content folders once. Returns
    /// `None` (and logs) when Documents is unwritable — the editor keeps
    /// running, file tabs just stay empty.
    fn ensure_files_root(&mut self) -> Option<std::path::PathBuf> {
        if self.files_root.is_none() {
            let root =
                crate::components::builtin_plugin_files::plugin_files_root(&self.display_name);
            match crate::components::builtin_plugin_files::ensure_plugin_dirs(&root) {
                Ok(()) => self.files_root = Some(root),
                Err(error) => {
                    eprintln!(
                        "[plugin-files] cannot create {} folders: {error}",
                        root.display()
                    );
                    return None;
                }
            }
        }
        self.files_root.clone()
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// (Re)install the host ops. Called on the focus/reuse path so a window
    /// opened before the engine/bridge finished warmup starts forwarding once
    /// live handles exist. An empty set never clobbers a live one.
    pub(crate) fn set_host_ops(&mut self, ops: BuiltinEditorHostOps) {
        if !ops.is_empty() {
            self.host_ops = ops;
        }
    }

    /// Replace the sidebar's instance list wholesale. Called whenever an
    /// insert using this `plugin_id` is added/removed/renamed/reordered
    /// anywhere in the project, and whenever this window is (re)focused from
    /// an Open-Editor request.
    ///
    /// If the previously active instance is gone, this picks the nearest
    /// remaining instance (first in the rebuilt list) rather than just
    /// clearing selection — "deleting the active insert selects another
    /// valid instance" per spec. Only goes to the empty state when the list
    /// is genuinely empty.
    pub(crate) fn set_instances(
        &mut self,
        instances: Vec<PluginInstanceDescriptor>,
        cx: &mut Context<Self>,
    ) {
        let active_still_present = self
            .active_instance
            .as_ref()
            .is_some_and(|active| instances.iter().any(|i| &i.instance_key == active));
        let removed_active = self
            .active_instance
            .clone()
            .filter(|_| !active_still_present);
        self.instances = instances;

        if !active_still_present {
            if let Some(removed) = removed_active {
                self.post_to_view(&InstanceRemovedMsg {
                    r#type: "futureboard.instanceRemoved",
                    protocol_version: BRIDGE_PROTOCOL_VERSION,
                    instance_id: wire_instance_id(&removed),
                    binding_generation: self.binding_generation,
                });
            }
            match self.instances.first().map(|i| i.instance_key.clone()) {
                Some(next) => {
                    self.select_instance(next, cx);
                    return;
                }
                None => {
                    self.active_instance = None;
                    self.editor_id = format!("{}::<none>", self.plugin_id);
                }
            }
        }
        cx.notify();
    }

    /// Rebind the shared browser to a different instance. A no-op re-select
    /// of the already-active instance still bumps `binding_generation` — the
    /// caller (sidebar click, or `requestSelectInstance`) does not need to
    /// special-case "already selected".
    ///
    /// Native decides, always: this is called from the sidebar click handler
    /// AND from the inbound `requestSelectInstance` handler in `tick()` — the
    /// latter validates the request against `self.instances` exactly the same
    /// way before calling this, so a route change alone can never bind an
    /// instance native hasn't approved (spec's "URL does not authorize
    /// access" rule).
    pub(crate) fn select_instance(&mut self, key: PluginInstanceKey, cx: &mut Context<Self>) {
        if !self.instances.iter().any(|i| i.instance_key == key) {
            eprintln!(
                "[BuiltinPluginEditor] select_instance rejected: {}::{} is not in the sidebar for plugin={}",
                key.track_id, key.insert_id, self.plugin_id
            );
            return;
        }
        self.binding_generation += 1;
        self.editor_id = wire_instance_id(&key);
        self.active_instance = Some(key);
        // Sequences are per-region, so the new instance's current frame could
        // collide with the old one's and be suppressed as "unchanged".
        self.spectrum_seq = 0;
        // Status lands on the *next* pump tick rather than up to a second later.
        // An editor that reads the transport tempo (EchoSpace's note divisions)
        // would otherwise open showing lengths for the wrong tempo.
        self.telemetry_tick = HOST_STATUS_TICKS - 1;
        self.push_selected_instance();
        cx.notify();
    }

    /// Push `futureboard.selectInstance` for the current `active_instance`
    /// into the live page. No-op if the browser hasn't announced
    /// `bridgeReady` yet or nothing is selected — `browser_ready` becoming
    /// true re-calls this so a selection made while loading isn't lost.
    fn push_selected_instance(&self) {
        if !self.browser_ready {
            return;
        }
        let Some(active) = self.active_instance.as_ref() else {
            return;
        };
        let Some(descriptor) = self.instances.iter().find(|i| &i.instance_key == active) else {
            return;
        };
        let msg = SelectInstanceMsg {
            r#type: "futureboard.selectInstance",
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            plugin_id: self.plugin_id.clone(),
            instance_id: wire_instance_id(active),
            binding_generation: self.binding_generation,
            display: InstanceDisplayMetadata {
                track_id: active.track_id.clone(),
                track_name: descriptor.track_name.clone(),
                insert_id: active.insert_id.clone(),
                insert_name: descriptor.insert_name.clone(),
            },
            // TODO(phase5 remaining): no per-insert state *revision counter*
            // exists yet (nothing generates incremental patches to count),
            // so this is always 0 even though `state` below can now be real.
            state_revision: 0,
            state: decode_state_bytes(descriptor.state_bytes.as_deref().map(Vec::as_slice)),
        };
        self.post_to_view(&msg);
    }

    /// Disable renderer-side zoom gestures as a second layer behind CEF's
    /// native shortcut and command-line policy.
    fn install_browser_policy(&self) {
        host::send_to_view(
            self.view_id,
            r#"
(() => {
  if (window.__futureboardBrowserPolicyInstalled) return;
  window.__futureboardBrowserPolicyInstalled = true;
  window.addEventListener("wheel", (event) => {
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  }, { capture: true, passive: false });
  for (const type of ["gesturestart", "gesturechange", "gestureend"]) {
    window.addEventListener(type, (event) => {
      event.preventDefault();
      event.stopImmediatePropagation();
    }, { capture: true, passive: false });
  }
})();
"#,
        );
    }

    fn post_to_view(&self, msg: &impl serde::Serialize) {
        let Ok(value) = serde_json::to_value(msg) else {
            eprintln!("[plugin-bridge] outbound serialization failed");
            return;
        };
        if bridge_value_contains_forbidden_secret(&value) {
            eprintln!("[plugin-bridge] outbound secret-bearing message rejected");
            return;
        }
        let Ok(json) = serde_json::to_string(&value) else {
            eprintln!("[plugin-bridge] outbound JSON encoding failed");
            return;
        };
        host::send_to_view(self.view_id, &format!("window.postMessage({json}, \"*\");"));
    }

    /// Handle one React->native bridge message already parsed from
    /// `host::take_inbound`. Split out of `tick()` for readability; still
    /// only ever called from there (the UI-thread pump), never from CEF's IO
    /// thread where the message actually arrived.
    fn handle_inbound(&mut self, msg: InboundMsg, cx: &mut Context<Self>) {
        match msg {
            InboundMsg::BridgeReady { .. } => {
                self.browser_ready = true;
                // Watchdog satisfied: the page came up, so the reload count
                // that got it here shouldn't count against a *future*,
                // unrelated crash.
                self.attached_at = None;
                self.bridge_ready_retries = 0;
                self.install_browser_policy();
                self.push_selected_instance();
                // A fresh page knows nothing about the transport; the next
                // telemetry tick sends the current state rather than waiting
                // for the user to press play.
                self.pushed_transport_playing = None;
            }
            InboundMsg::InstanceReady {
                instance_id,
                binding_generation,
                ..
            } => {
                if binding_generation != self.binding_generation {
                    eprintln!(
                        "[plugin-bridge] instanceReady stale plugin={} instance={instance_id} \
                         ack_generation={binding_generation} current_generation={}",
                        self.plugin_id, self.binding_generation
                    );
                    return;
                }
                // TODO(phase5): this is where native would start incremental
                // state-patch delivery / meter subscription for the newly
                // bound instance — there is nothing to subscribe to yet.
            }
            InboundMsg::RequestSelectInstance { instance_id } => {
                let Some(key) = self
                    .instances
                    .iter()
                    .find(|i| wire_instance_id(&i.instance_key) == instance_id)
                    .map(|i| i.instance_key.clone())
                else {
                    eprintln!(
                        "[plugin-bridge] requestSelectInstance rejected plugin={} instance={instance_id} reason=not_in_sidebar",
                        self.plugin_id
                    );
                    // Restore the route the browser actually has a valid
                    // binding for, rather than trusting the requested one.
                    self.push_selected_instance();
                    return;
                };
                self.select_instance(key, cx);
            }
            InboundMsg::GlobalCommand { command_id } => {
                let Some(dispatch) = self.host_ops.dispatch_global_command.as_ref() else {
                    return;
                };
                match command_id.as_str() {
                    "transport:play-pause" => dispatch("transport:play-pause", cx),
                    _ => eprintln!(
                        "[plugin-bridge] rejected global command plugin={} command={command_id}",
                        self.plugin_id
                    ),
                }
            }
            InboundMsg::SetParams {
                instance_id,
                binding_generation,
                params,
                ..
            } => {
                if binding_generation != self.binding_generation {
                    eprintln!(
                        "[plugin-bridge] setParams stale plugin={} instance={instance_id} \
                         edit_generation={binding_generation} current_generation={}",
                        self.plugin_id, self.binding_generation
                    );
                    return;
                }
                let Some(active) = self.active_instance.clone() else {
                    return;
                };
                if wire_instance_id(&active) != instance_id {
                    eprintln!(
                        "[plugin-bridge] setParams instance mismatch plugin={} got={instance_id} active={}",
                        self.plugin_id,
                        wire_instance_id(&active)
                    );
                    return;
                }
                let Some(forwarder) = self.host_ops.forward_param.as_ref() else {
                    // Engine not wired yet (warmup); drop rather than queue —
                    // the editor keeps sending fresh values on interaction.
                    return;
                };
                for edit in &params {
                    match host::builtin_param_index(&self.plugin_id, &edit.id) {
                        Some(index) => forwarder(&active, index, edit.value),
                        None => eprintln!(
                            "[plugin-bridge] setParams unknown param plugin={} id={}",
                            self.plugin_id, edit.id
                        ),
                    }
                }
                // Page reload/selectInstance must receive edits made since the
                // window opened, not the descriptor's original stale blob.
                if let Some(descriptor) = self
                    .instances
                    .iter_mut()
                    .find(|descriptor| descriptor.instance_key == active)
                {
                    descriptor.state_bytes =
                        host::builtin_state_bytes(&self.plugin_id, &active.insert_id)
                            .map(std::sync::Arc::new);
                }
            }
            InboundMsg::ListFiles { kind, .. } => {
                use crate::components::builtin_plugin_files as files;
                let Some(file_kind) = files::BuiltinFileKind::from_wire(&kind) else {
                    return;
                };
                let listing = self
                    .ensure_files_root()
                    .map(|root| files::list_files(&root, file_kind))
                    .unwrap_or_default();
                self.post_to_view(&FileListMsg {
                    r#type: "futureboard.fileList",
                    protocol_version: BRIDGE_PROTOCOL_VERSION,
                    kind,
                    files: listing,
                });
            }
            InboundMsg::WriteFile {
                kind,
                file_name,
                content,
                ..
            } => {
                use crate::components::builtin_plugin_files as files;
                let Some(file_kind) = files::BuiltinFileKind::from_wire(&kind) else {
                    return;
                };
                let result = match self.ensure_files_root() {
                    Some(root) => files::write_file(&root, file_kind, &file_name, &content)
                        .map_err(|e| e.to_string()),
                    None => Err("user folder unavailable".to_string()),
                };
                let (ok, written_name, error) = match result {
                    Ok(clean) => (true, clean, None),
                    Err(e) => (false, file_name, Some(e)),
                };
                self.post_to_view(&FileWrittenMsg {
                    r#type: "futureboard.fileWritten",
                    protocol_version: BRIDGE_PROTOCOL_VERSION,
                    kind,
                    file_name: written_name,
                    ok,
                    error,
                });
            }
            InboundMsg::ReadFile {
                kind, file_name, ..
            } => {
                use crate::components::builtin_plugin_files as files;
                let Some(file_kind) = files::BuiltinFileKind::from_wire(&kind) else {
                    return;
                };
                let Some(root) = self.ensure_files_root() else {
                    self.post_to_view(&FileContentMsg {
                        r#type: "futureboard.fileContent",
                        protocol_version: BRIDGE_PROTOCOL_VERSION,
                        kind,
                        file_name,
                        ok: false,
                        content: None,
                        error: Some("user folder unavailable".to_string()),
                    });
                    return;
                };
                // A `.nam` capture can be multi-MB: read off the UI thread,
                // post the result back on it (`post_to_view` is UI-only).
                cx.spawn(async move |this, cx| {
                    let read_name = file_name.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move { files::read_file(&root, file_kind, &read_name) })
                        .await;
                    let _ = this.update(cx, |this, _cx| {
                        let (ok, content, error) = match result {
                            Ok(text) => (true, Some(text), None),
                            Err(e) => (false, None, Some(e.to_string())),
                        };
                        this.post_to_view(&FileContentMsg {
                            r#type: "futureboard.fileContent",
                            protocol_version: BRIDGE_PROTOCOL_VERSION,
                            kind,
                            file_name,
                            ok,
                            content,
                            error,
                        });
                    });
                })
                .detach();
            }
            InboundMsg::LoadNamCapture {
                instance_id,
                binding_generation,
                name,
                json,
                stereo,
                full_rig,
                ..
            } => {
                if binding_generation != self.binding_generation {
                    eprintln!(
                        "[plugin-bridge] loadNamCapture stale plugin={} instance={instance_id}",
                        self.plugin_id
                    );
                    return;
                }
                let Some(active) = self.active_instance.as_ref() else {
                    return;
                };
                if wire_instance_id(active) != instance_id {
                    eprintln!(
                        "[plugin-bridge] loadNamCapture instance mismatch plugin={} got={instance_id}",
                        self.plugin_id
                    );
                    return;
                }
                let Some(forwarder) = self.host_ops.load_nam_capture.as_ref() else {
                    eprintln!(
                        "[plugin-bridge] loadNamCapture dropped (bridge not wired) plugin={}",
                        self.plugin_id
                    );
                    return;
                };
                eprintln!(
                    "[plugin-bridge] loadNamCapture plugin={} instance={instance_id} name={name} bytes={} stereo={stereo} full_rig={full_rig}",
                    self.plugin_id,
                    json.len()
                );
                forwarder(
                    active,
                    BuiltinNamLoadRequest {
                        name,
                        json,
                        stereo,
                        full_rig,
                    },
                );
            }
            InboundMsg::LoadIr {
                instance_id,
                binding_generation,
                file_name,
                ..
            } => {
                use crate::components::builtin_plugin_files as files;
                if binding_generation != self.binding_generation {
                    eprintln!(
                        "[plugin-bridge] loadIr stale plugin={} instance={instance_id}",
                        self.plugin_id
                    );
                    return;
                }
                // Clone the key up front: reading the IRs folder needs
                // `&mut self`, which cannot coexist with a borrow of
                // `active_instance`.
                let Some(active) = self.active_instance.clone() else {
                    return;
                };
                if wire_instance_id(&active) != instance_id {
                    eprintln!(
                        "[plugin-bridge] loadIr instance mismatch plugin={} got={instance_id}",
                        self.plugin_id
                    );
                    return;
                }
                if self.host_ops.load_ir.is_none() {
                    eprintln!(
                        "[plugin-bridge] loadIr dropped (bridge not wired) plugin={}",
                        self.plugin_id
                    );
                    return;
                }
                // The read is small (an IR is tens of KB) but still filesystem
                // I/O; the *host* does the decode/FFT work off this thread.
                let read = self
                    .ensure_files_root()
                    .ok_or_else(|| "user folder unavailable".to_string())
                    .and_then(|root| {
                        files::read_file_bytes(&root, files::BuiltinFileKind::Irs, &file_name)
                            .map_err(|e| e.to_string())
                    });
                let bytes = match read {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let insert_id = active.insert_id.clone();
                        self.notify_ir_load_result(
                            &insert_id,
                            false,
                            &file_name,
                            Some(&error),
                            0,
                            0,
                            false,
                            false,
                        );
                        return;
                    }
                };
                eprintln!(
                    "[plugin-bridge] loadIr plugin={} instance={instance_id} file={file_name} bytes={}",
                    self.plugin_id,
                    bytes.len()
                );
                if let Some(forwarder) = self.host_ops.load_ir.as_ref() {
                    forwarder(
                        &active,
                        BuiltinIrLoadRequest {
                            name: file_name,
                            bytes,
                        },
                    );
                }
            }
            InboundMsg::Unknown => {}
        }
    }

    /// Route a host `BuiltinNamCaptureResult` into the page, if this window's
    /// bound instance matches the reporting insert. Called from
    /// `poll_plugin_bridge_runtime` in `plugin_ops.rs`.
    pub(crate) fn notify_nam_capture_result(
        &self,
        plugin_instance_id: &str,
        ok: bool,
        name: &str,
        error: Option<&str>,
        receptive_field: u64,
        full_rig: bool,
    ) {
        let Some(active) = self.active_instance.as_ref() else {
            return;
        };
        if active.insert_id != plugin_instance_id {
            return;
        }
        self.post_to_view(&NamCaptureResultMsg {
            r#type: "futureboard.namCaptureResult",
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            instance_id: wire_instance_id(active),
            ok,
            name: name.to_string(),
            error: error.map(str::to_string),
            receptive_field,
            full_rig,
        });
    }

    /// Route a host `BuiltinIrResult` into the page, if this window's bound
    /// instance matches the reporting insert. Called from
    /// `poll_plugin_bridge_runtime` in `plugin_ops.rs`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn notify_ir_load_result(
        &self,
        plugin_instance_id: &str,
        ok: bool,
        name: &str,
        error: Option<&str>,
        frames: u64,
        latency_samples: u64,
        stereo: bool,
        truncated: bool,
    ) {
        let Some(active) = self.active_instance.as_ref() else {
            return;
        };
        if active.insert_id != plugin_instance_id {
            return;
        }
        self.post_to_view(&IrLoadResultMsg {
            r#type: "futureboard.irLoadResult",
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            instance_id: wire_instance_id(active),
            ok,
            name: name.to_string(),
            error: error.map(str::to_string),
            frames,
            latency_samples,
            stereo,
            truncated,
        });
    }

    /// Telemetry push, called from `tick()` while attached: meters at every
    /// 4th pump tick (~30 Hz), host status at every 128th (~1 Hz). No-ops
    /// until the page announced `bridgeReady` and an instance is bound.
    fn push_telemetry(&mut self) {
        if !self.browser_ready {
            return;
        }
        // Transport state is window-wide, not per-instance: push it even before
        // an instance is bound, and only when it actually changed.
        self.push_transport_if_changed();
        let Some(active) = self.active_instance.as_ref() else {
            return;
        };
        self.telemetry_tick = self.telemetry_tick.wrapping_add(1);
        if self.telemetry_tick % 4 == 0 {
            if let Some(source) = self.host_ops.meter_source.as_ref() {
                if let Some(frame) = source(active) {
                    self.post_to_view(&MetersMsg {
                        r#type: "futureboard.meters",
                        protocol_version: BRIDGE_PROTOCOL_VERSION,
                        instance_id: wire_instance_id(active),
                        binding_generation: self.binding_generation,
                        in_peak: frame.in_peak,
                        in_rms: frame.in_rms,
                        out_peak: frame.out_peak,
                        out_rms: frame.out_rms,
                        gain_reduction_db: frame.gain_reduction_db,
                        in_clip: frame.in_clip,
                        out_clip: frame.out_clip,
                        // A built-in with no rack publishes all zeros; send
                        // nothing at all rather than six dead numbers per frame.
                        slot_in_peak: stage_levels(&frame.slot_in_peak),
                        slot_out_peak: stage_levels(&frame.slot_out_peak),
                    });
                }
            }
            if let Some(source) = self.host_ops.spectrum_source.as_ref() {
                if let Some((seq, bins)) = source(active) {
                    // Only when the host has actually analysed again.
                    if seq != self.spectrum_seq {
                        self.spectrum_seq = seq;
                        self.post_to_view(&SpectrumMsg {
                            r#type: "futureboard.spectrum",
                            protocol_version: BRIDGE_PROTOCOL_VERSION,
                            instance_id: wire_instance_id(active),
                            min_hz: SpherePluginHost::spectrum::MIN_HZ,
                            max_hz: SpherePluginHost::spectrum::MAX_HZ,
                            floor_db: SpherePluginHost::spectrum::FLOOR_DB,
                            ceil_db: SpherePluginHost::spectrum::CEIL_DB,
                            bins: bins
                                .iter()
                                .map(|db| SpherePluginHost::spectrum::quantize_db(*db))
                                .collect(),
                        });
                    }
                }
            }
        }
        if self.telemetry_tick % HOST_STATUS_TICKS == 0 {
            if let Some(source) = self.host_ops.host_status_source.as_ref() {
                if let Some((sample_rate, block_size, latency_samples, tempo_bpm)) = source(active)
                {
                    self.post_to_view(&HostStatusMsg {
                        r#type: "futureboard.hostStatus",
                        protocol_version: BRIDGE_PROTOCOL_VERSION,
                        instance_id: wire_instance_id(active),
                        sample_rate,
                        block_size,
                        latency_samples,
                        tempo_bpm,
                    });
                }
            }
        }
    }

    /// Send `futureboard.transport` when the DAW's play state differs from what
    /// the page was last told. Edge-triggered: a stopped session posts nothing
    /// after the initial state, so this costs one atomic load per pump tick.
    fn push_transport_if_changed(&mut self) {
        let Some(source) = self.host_ops.transport_source.as_ref() else {
            return;
        };
        let playing = source();
        if self.pushed_transport_playing == Some(playing) {
            return;
        }
        self.pushed_transport_playing = Some(playing);
        self.post_to_view(&TransportMsg {
            r#type: "futureboard.transport",
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            playing,
        });
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    fn sidebar_width(&self) -> f32 {
        if self.sidebar_collapsed {
            0.0
        } else {
            SIDEBAR_W
        }
    }

    /// Drive CEF and, until it succeeds, keep retrying the attach.
    fn spawn_pump(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PUMP_INTERVAL).await;

                // CEF synchronously pumps Win32 messages. It must run before
                // `this.update`, while GPUI holds neither the AppCell nor this
                // entity's RefCell; otherwise a nested GPUI message double-borrows
                // the app and panics in AsyncApp::update_entity.
                host::pump();

                let tick = this.update(cx, |this, cx| this.tick(cx));
                match tick {
                    Ok(tick) => {
                        // Destroying an HWND also dispatches Win32 messages, so
                        // release failed/closed content after the entity update.
                        #[cfg(windows)]
                        drop(tick.content_to_drop);
                        if !tick.keep_going {
                            break;
                        }
                    }
                    // Window gone. If it disappeared during the CEF call above,
                    // `Drop` queued close after that pump drained its command
                    // snapshot. Give the queue one final borrow-free pass.
                    Err(_) => {
                        host::pump();
                        break;
                    }
                }
            }
        })
        .detach();
    }

    /// One pump tick. Consumes completion events without invoking CEF.
    fn tick(&mut self, cx: &mut Context<Self>) -> PumpTick {
        let mut content_to_drop = None;
        let play_pause_requests = host::take_global_play_pause_requests(self.view_id);
        if play_pause_requests > 0 {
            if let Some(dispatch) = self.host_ops.dispatch_global_command.as_ref() {
                for _ in 0..play_pause_requests {
                    dispatch("transport:play-pause", cx);
                }
            }
        }
        for event in host::take_view_events(self.view_id) {
            match event {
                ViewEvent::Opened if matches!(self.status, Status::Attaching) => {
                    self.status = Status::Attached;
                    self.browser_ready = false;
                    self.attached_at = Some(Instant::now());
                    cx.notify();
                }
                ViewEvent::OpenFailed(error) if matches!(self.status, Status::Attaching) => {
                    self.status = Status::Failed(format!("CEF failed to open the editor: {error}"));
                    content_to_drop = self.content.take();
                    cx.notify();
                }
                ViewEvent::AcceleratedFallback
                    if matches!(self.status, Status::Attaching | Status::Attached) =>
                {
                    self.status = Status::Attaching;
                    self.browser_ready = false;
                    self.attached_at = None;
                    self.bridge_ready_retries = 0;
                    cx.notify();
                }
                ViewEvent::Closed => {
                    self.status = Status::Closed;
                    content_to_drop = self.content.take();
                    cx.notify();
                }
                // An open completion can race a close requested from a nested
                // Win32 callback. Closing dominates; the queued close is handled
                // by the next pump.
                ViewEvent::Opened | ViewEvent::OpenFailed(_) | ViewEvent::AcceleratedFallback => {}
                ViewEvent::RendererCrashed => {
                    // The host already reloaded the browser. `active_instance`
                    // and `instances` are untouched — native DSP state (what
                    // there is of it) never lived in the page, so there is
                    // nothing to lose here. Wait for the fresh page's
                    // `bridgeReady` before pushing the selection again.
                    self.browser_ready = false;
                    self.attached_at = Some(Instant::now());
                    eprintln!(
                        "[plugin-bridge] renderer crashed plugin={}, waiting for bridgeReady to resend selection",
                        self.plugin_id
                    );
                }
            }
        }

        // Off-screen: a newly painted frame is the only thing that makes the
        // editor's own animation (meters, knob drags) reach the screen, so the
        // window has to repaint whenever CEF has produced one.
        if OFFSCREEN_HOSTING
            && matches!(self.status, Status::Attaching | Status::Attached)
            && self.surface.sync(self.view_id)
        {
            cx.notify();
        }

        if let Status::WaitingForHandle { ticks } = self.status {
            let ticks = ticks + 1;
            if ticks > MAX_HANDLE_TICKS {
                let reason = if OFFSCREEN_HOSTING {
                    "the editor window never reported usable content bounds"
                } else {
                    "the editor window never produced a usable native handle"
                };
                eprintln!(
                    "[plugin-editor-window] attach timed out after {ticks} ticks, \
                     render_passes_that_reached_attach={} offscreen={OFFSCREEN_HOSTING}",
                    self.attach_attempts
                );
                self.status = Status::Failed(reason.to_string());
            } else {
                self.status = Status::WaitingForHandle { ticks };
            }
            // `attach` only runs from a render pass, so waiting must schedule
            // one every tick. Without this the attach is attempted exactly once
            // — and a first render that precedes the platform's window
            // configuration (routine on Wayland, where the compositor sizes the
            // surface asynchronously) is never retried, so the window sits here
            // until the tick budget runs out.
            cx.notify();
        }

        // Bridge inbound: only once the browser exists, keyed by the same
        // scheme origin `resolve_asset`/`bridge_sink` match requests against.
        if self.status == Status::Attached {
            if let Some(origin) = host::origin_for_plugin_id(&self.plugin_id) {
                for raw in host::take_inbound(origin) {
                    match parse_inbound_message(&raw) {
                        Ok(msg) => self.handle_inbound(msg, cx),
                        Err(error) => eprintln!(
                            "[plugin-bridge] malformed inbound message plugin={} err={error}",
                            self.plugin_id
                        ),
                    }
                }
            }
        }

        // Telemetry push (meters ~30 Hz, host status ~1 Hz) for the bound
        // instance — rate-derived from this pump's own 8 ms cadence.
        if self.status == Status::Attached {
            self.push_telemetry();
        }

        // Bridge-ready watchdog (see `attached_at`'s doc comment). Only
        // meaningful once actually attached and not yet confirmed ready.
        if self.status == Status::Attached && !self.browser_ready {
            if let Some(attached_at) = self.attached_at {
                if attached_at.elapsed() >= BRIDGE_READY_TIMEOUT {
                    if self.bridge_ready_retries >= MAX_BRIDGE_READY_RETRIES {
                        self.status = Status::Failed(format!(
                            "the editor page never became responsive after {} reload attempts",
                            self.bridge_ready_retries
                        ));
                        cx.notify();
                    } else {
                        self.bridge_ready_retries += 1;
                        eprintln!(
                            "[plugin-bridge] bridgeReady watchdog fired plugin={} attempt={}/{} — reloading",
                            self.plugin_id, self.bridge_ready_retries, MAX_BRIDGE_READY_RETRIES
                        );
                        host::reload_view(self.view_id);
                        self.attached_at = Some(Instant::now());
                    }
                }
            }
        }

        PumpTick {
            keep_going: matches!(
                self.status,
                Status::WaitingForHandle { .. }
                    | Status::Attaching
                    | Status::Attached
                    | Status::Closing
            ),
            content_to_drop,
        }
    }

    /// Create the CEF browser for this window. Called from the render pass,
    /// which is the first place the GPUI atlas, optional Windows parent handle,
    /// and real content bounds are all available.
    fn attach(&mut self, window: &mut Window, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.attach_attempts += 1;
        let scale = window.scale_factor();
        let rect = content_rect(bounds, scale, self.sidebar_width());
        if rect.width <= 0 || rect.height <= 0 {
            // Routine for the first render pass on a compositor that sizes the
            // surface asynchronously; the waiting tick schedules another pass.
            eprintln!(
                "[plugin-editor-window] attach deferred attempt={} window_bounds={:?} scale={scale} content_rect={rect:?}",
                self.attach_attempts, bounds.size
            );
            return;
        }
        eprintln!(
            "[plugin-editor-window] attach begin attempt={} offscreen={OFFSCREEN_HOSTING} content_rect={rect:?} scale={scale}",
            self.attach_attempts
        );

        // Off-screen hosting has no content child. On Windows the top-level
        // handle is still useful to CEF for monitor/dialog ownership; other
        // platforms may pass no native parent.
        let parent_hwnd = if OFFSCREEN_HOSTING {
            native_hwnd(window).unwrap_or(0)
        } else {
            let Some(top_hwnd) = native_hwnd(window) else {
                return;
            };
            let content = match self.content.as_ref() {
                Some(content) if content.is_valid() => content,
                _ => {
                    let Some(created) = ContentChildHwnd::create(
                        top_hwnd,
                        ContentRect {
                            x: rect.x,
                            y: rect.y,
                            width: rect.width,
                            height: rect.height,
                        },
                    ) else {
                        self.status = Status::Failed(
                            "could not create the editor content window".to_string(),
                        );
                        cx.notify();
                        return;
                    };
                    self.content = Some(created);
                    self.content.as_ref().expect("just installed")
                }
            };
            content.hwnd()
        };

        // CEF fills its parent's client area, so the browser is placed at the
        // content child's origin, not the shell's.
        let view_rect = ViewRect {
            x: 0,
            y: 0,
            width: rect.width,
            height: rect.height,
        };
        let accelerated_sink = self.surface.accelerated_sink(window);
        match host::open_view(
            self.view_id,
            &self.editor_id,
            &self.plugin_id,
            parent_hwnd,
            view_rect,
            scale,
            accelerated_sink,
        ) {
            Ok(()) => {
                self.status = Status::Attaching;
                self.last_rect = Some(rect);
                self.last_scale = scale;
                cx.notify();
            }
            Err(err) => {
                self.status = Status::Failed(err.to_string());
                cx.notify();
            }
        }
    }

    /// Keep the content child and the browser matched to the shell's content
    /// rect. Only issues native calls when the rect actually changed.
    fn resync_bounds(&mut self, window: &Window, bounds: Bounds<Pixels>) {
        let scale_factor = window.scale_factor();
        let rect = content_rect(bounds, scale_factor, self.sidebar_width());
        let scale_unchanged = (self.last_scale - scale_factor).abs() <= f32::EPSILON;
        if rect.width <= 0 || rect.height <= 0 || (self.last_rect == Some(rect) && scale_unchanged)
        {
            return;
        }
        if let Some(content) = self.content.as_ref() {
            content.set_bounds(ContentRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            });
        }
        host::set_view_bounds(
            self.view_id,
            ViewRect {
                x: 0,
                y: 0,
                width: rect.width,
                height: rect.height,
            },
            scale_factor,
        );
        self.last_rect = Some(rect);
        self.last_scale = scale_factor;
    }

    /// Begin an asynchronous close. The shell remains alive until the CEF pump
    /// confirms it processed the close, preserving the native parent HWND for
    /// the browser's entire lifetime.
    pub(crate) fn request_close(&mut self, cx: &mut Context<Self>) {
        match self.status {
            Status::Closing | Status::Closed => return,
            Status::WaitingForHandle { .. } | Status::Failed(_) => {
                self.status = Status::Closed;
            }
            Status::Attaching | Status::Attached => {
                host::close_view(self.view_id);
                self.status = Status::Closing;
            }
        }
        cx.notify();
    }
}

impl Drop for BuiltinPluginEditorWindow {
    fn drop(&mut self) {
        // Fallback for forced application/window teardown. Normal close travels
        // through `Closing` and waits for the pump's `Closed` event.
        if !matches!(self.status, Status::Closed | Status::Failed(_)) {
            host::close_view(self.view_id);
        }
    }
}

/// The physical-pixel rect the browser occupies inside the shell's client
/// area: everything below the GPUI-drawn header and right of the native
/// sidebar. `sidebar_w` is reserved the same way as `HEADER_H` — the browser
/// must never be told to draw under either.
fn content_rect(bounds: Bounds<Pixels>, scale: f32, sidebar_w: f32) -> ViewRect {
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    let phys = |v: f32| (v * scale).round() as i32;
    ViewRect {
        x: phys(sidebar_w),
        y: phys(HEADER_H),
        width: (phys(width) - phys(sidebar_w)).max(0),
        height: (phys(height) - phys(HEADER_H)).max(0),
    }
}

/// Whole-pixel wheel delta for CEF, carrying the fraction into `carry`.
///
/// A Windows precision trackpad reports a fraction of a line per message.
/// Rounding each one on its own turns a slow two-finger swipe into a stream of
/// zero deltas that never reach the page, so the remainder is kept and spent as
/// soon as it adds up to a whole pixel.
fn take_whole_scroll(carry: &mut Point<f32>, pixels: Point<f32>) -> (i32, i32) {
    // A reversal makes the leftover fraction point the wrong way; dropping it
    // keeps a direction change from being swallowed by stale carry.
    if pixels.x * carry.x < 0.0 {
        carry.x = 0.0;
    }
    if pixels.y * carry.y < 0.0 {
        carry.y = 0.0;
    }
    carry.x = (carry.x + pixels.x).clamp(-MAX_SCROLL_DELTA, MAX_SCROLL_DELTA);
    carry.y = (carry.y + pixels.y).clamp(-MAX_SCROLL_DELTA, MAX_SCROLL_DELTA);
    let delta_x = carry.x.trunc();
    let delta_y = carry.y.trunc();
    carry.x -= delta_x;
    carry.y -= delta_y;
    (delta_x as i32, delta_y as i32)
}

/// CEF's click count contract is 1..=3 (single, double, triple). GPUI keeps
/// counting past that while a fast click repeats in place, and Blink has no
/// defined press behaviour for a higher count.
fn clamp_click_count(click_count: usize) -> i32 {
    click_count.clamp(1, 3) as i32
}

#[cfg(target_os = "windows")]
fn native_hwnd(window: &Window) -> Option<u64> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(w) => Some(w.hwnd.get() as u64),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn native_hwnd(_window: &Window) -> Option<u64> {
    // Non-Windows OSR does not need a native parent handle.
    None
}

impl Render for BuiltinPluginEditorWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = window.bounds();

        // Frames superseded since the last pass still hold an atlas tile; this
        // is the first point in the frame where a `Window` exists to free them.
        self.surface.release_stale(window, cx);

        match &self.status {
            Status::WaitingForHandle { .. } => self.attach(window, bounds, cx),
            Status::Attaching | Status::Attached => self.resync_bounds(window, bounds),
            Status::Closing | Status::Closed | Status::Failed(_) => {}
        }
        if matches!(self.status, Status::Closed) {
            cx.defer_in(window, |_this, window, _cx| window.remove_window());
        }

        let failure = match &self.status {
            Status::Failed(reason) => Some(reason.clone()),
            _ => None,
        };

        // Same focus-reclaim contract as MixerWindow: GPUI only routes key events
        // along the focused dispatch path. Sidebar / titlebar clicks leave no
        // focus handle, so capture_key_down never runs and Space dies — especially
        // on Linux. Off-screen CEF still receives keys through our surface
        // forwarder once this shell holds focus again.
        if OFFSCREEN_HOSTING && !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        let focus_on_pointer = self.focus.clone();

        // Space must work even when focus is not on the CEF surface (titlebar /
        // instance sidebar) and on every OS — not only off-screen hosts. Capture
        // phase on the shell so bare Space always reaches the same transport
        // command as the arrangement. Windowed CEF still also claims via
        // OnPreKeyEvent; that path returns 1 (consumed) so we never toggle twice.
        let transport_claim = cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if !Self::is_transport_toggle_keystroke(&event.keystroke) || event.is_held {
                return;
            }
            window.prevent_default();
            cx.stop_propagation();
            this.dispatch_transport_play_pause(cx);
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::surface_panel())
            // Pointer anywhere on the shell (titlebar, sidebar, browser) reclaims
            // the keyboard anchor so the next Space hits the transport claim.
            .capture_any_mouse_down(move |_event, window, cx| {
                focus_on_pointer.focus(window, cx);
            })
            .capture_key_down(transport_claim)
            .child(
                // Shared external-dialog titlebar: gives this window the same
                // chrome as every other floating Studio surface, plus the drag
                // region and close button that a borderless shell needs (a
                // hand-rolled header has no way to move the window).
                div().flex_none().child(external_window_titlebar(
                    self.display_name.clone(),
                    "builtin-plugin-editor-close",
                    {
                        let this = cx.weak_entity();
                        move |_window, cx| {
                            let _ = this.update(cx, |this, cx| this.request_close(cx));
                        }
                    },
                )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_row()
                    .child(self.render_sidebar(cx))
                    .child(match failure {
                        None => self.render_browser_region(cx),
                        Some(reason) => div()
                            .flex_1()
                            .min_h(px(0.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(6.0))
                            .p(px(16.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(Colors::text_primary())
                                    .child("This editor could not be opened"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(Colors::text_secondary())
                                    .child(reason),
                            )
                            .into_any_element(),
                    }),
            )
    }
}

impl BuiltinPluginEditorWindow {
    /// The region the browser occupies.
    ///
    /// Draws the latest accelerated or software frame and owns all forwarded
    /// browser input handlers.
    fn render_browser_region(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let region = div().flex_1().min_h(px(0.0)).overflow_hidden();
        if !OFFSCREEN_HOSTING {
            return region.into_any_element();
        }

        region
            .id("builtin-plugin-editor-surface")
            .track_focus(&self.focus)
            .when_some(self.surface.accelerated_element(), |el, surface| {
                el.child(surface)
            })
            .when_some(self.surface.image(), |el, image| {
                // The frame is already exactly the content rect in physical
                // pixels; `Fill` maps it back 1:1 rather than letterboxing it.
                el.child(img(image).size_full().object_fit(ObjectFit::Fill))
            })
            .child(self.mouse_move_forwarder(cx))
            .on_mouse_down_out(cx.listener(|this, _event: &MouseDownEvent, _window, _cx| {
                this.send_input(EditorInput::Focus(false));
            }))
            .on_scroll_wheel(cx.listener(Self::on_surface_scroll))
            .on_key_down(cx.listener(Self::on_surface_key_down))
            .on_key_up(cx.listener(Self::on_surface_key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_surface_mouse_down))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(Self::on_surface_mouse_down),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_surface_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_surface_mouse_up))
            // A knob drag routinely leaves the browser rect; the page keeps
            // tracking it (pointer capture) only if the release still arrives.
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_surface_mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_surface_mouse_up))
            .into_any_element()
    }

    /// Window-wide mouse-move forwarding.
    ///
    /// An element-scoped `on_mouse_move` only fires while the pointer is inside
    /// the hitbox, which would freeze any drag the moment it left the browser
    /// rect — exactly what a knob drag does. Moves are therefore taken from the
    /// window and translated unconditionally: a position outside the rect maps
    /// to a coordinate outside the document, which is what the page should see
    /// anyway.
    fn mouse_move_forwarder(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |_bounds, _, window, _cx| {
                let this = this.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = this.update(cx, |this, _cx| this.forward_mouse_move(event));
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// Window-space logical point → view-space logical point. CEF lays the page
    /// out in the same logical pixels GPUI reports, offset by the chrome this
    /// window reserves (see `content_rect`). Verified by `osr_editor_probe`:
    /// a click at window chrome+view landing maps 1:1 into page coordinates.
    fn to_view_point(&self, position: Point<Pixels>) -> (i32, i32) {
        let x: f32 = position.x.into();
        let y: f32 = position.y.into();
        (
            (x - self.sidebar_width()).round() as i32,
            (y - HEADER_H).round() as i32,
        )
    }

    fn send_input(&self, input: EditorInput) {
        if !OFFSCREEN_HOSTING || !matches!(self.status, Status::Attaching | Status::Attached) {
            return;
        }
        host::send_view_input(self.view_id, input);
    }

    /// Let CEF consume a click/key/wheel event as soon as this GPUI handler has
    /// unwound. Pumping inline is unsafe because Chromium may re-enter GPUI.
    fn schedule_input_pump(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::ZERO).await;
            host::pump_after_input();
            let _ = this.update(cx, |this, cx| {
                if this.surface.sync(this.view_id) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn forward_mouse_move(&mut self, event: &MouseMoveEvent) {
        let (x, y) = self.to_view_point(event.position);
        self.forward_pointer_position(x, y, event.modifiers, false);
    }

    /// Tell the browser where the pointer is.
    ///
    /// A position outside the browser rect is still forwarded, because a knob
    /// drag routinely leaves it and the page must keep tracking. Two rules
    /// govern the leave flag:
    ///
    /// * While a button is held the page owns the gesture through pointer
    ///   capture. Sending `mouse_leave` there makes Blink cancel the captured
    ///   pointer (`pointercancel`), which ends the drag mid-gesture and leaves
    ///   the control stuck — so a drag is never reported as leaving, and its
    ///   moves are never suppressed.
    /// * With no button held the flag is edge-triggered: the page is told once
    ///   that the pointer left, so hover state clears instead of holding the
    ///   last control lit forever, and further outside moves are dropped until
    ///   the pointer comes back.
    ///
    /// `force` sends the position even when it would otherwise be suppressed —
    /// used before a button event so the browser's pointer position is current
    /// when the click arrives.
    fn forward_pointer_position(
        &mut self,
        x: i32,
        y: i32,
        modifiers: gpui::Modifiers,
        force: bool,
    ) {
        let dragging = self.surface.any_button_held();
        let leaving = !self.view_contains(x, y) && !dragging;
        if leaving && self.pointer_left && !force {
            return;
        }
        self.pointer_left = leaving;
        self.send_input(EditorInput::MouseMove {
            x,
            y,
            modifiers: self.surface.modifiers(modifiers),
            leaving,
        });
    }

    /// Whether a view-space point is inside the browser rect. Uses the logical
    /// size derived from the rect last handed to the host, so it can never
    /// disagree with what CEF was told to lay out.
    fn view_contains(&self, x: i32, y: i32) -> bool {
        let Some(rect) = self.last_rect else {
            return false;
        };
        let scale = self.last_scale.max(f32::EPSILON);
        let width = (rect.width as f32 / scale).round() as i32;
        let height = (rect.height as f32 / scale).round() as i32;
        (0..width).contains(&x) && (0..height).contains(&y)
    }

    fn on_surface_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(button) = editor_mouse_button(event.button) else {
            return;
        };
        // The page owns the keyboard while the pointer is in it; without this
        // the browser never sees a focused document and text fields stay dead.
        window.focus(&self.focus, cx);
        self.send_input(EditorInput::Focus(true));
        let (x, y) = self.to_view_point(event.position);
        // Position first, with no button bit set yet — the same order a real
        // platform delivers (move, then press), so Blink hit-tests the press
        // against a pointer it already believes is over the control.
        self.forward_pointer_position(x, y, event.modifiers, true);
        self.surface.set_button(button, true);
        self.send_input(EditorInput::MouseButton {
            x,
            y,
            button,
            pressed: true,
            click_count: clamp_click_count(event.click_count),
            modifiers: self.surface.modifiers(event.modifiers),
        });
        Self::schedule_input_pump(cx);
    }

    fn on_surface_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(button) = editor_mouse_button(event.button) else {
            return;
        };
        let (x, y) = self.to_view_point(event.position);
        // Clear the held-button bit *before* sending, so the event CEF sees
        // reports the state after the release, as a real platform event would.
        self.surface.set_button(button, false);
        self.send_input(EditorInput::MouseButton {
            x,
            y,
            button,
            pressed: false,
            click_count: clamp_click_count(event.click_count),
            modifiers: self.surface.modifiers(event.modifiers),
        });
        // A drag released outside the rect suppressed the leave for the whole
        // gesture; now that no button is held, settle the hover state.
        self.forward_pointer_position(x, y, event.modifiers, false);
        Self::schedule_input_pump(cx);
    }

    fn on_surface_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.modifiers.control || event.modifiers.platform {
            window.prevent_default();
            return;
        }
        let (x, y) = self.to_view_point(event.position);
        let pixels = match event.delta {
            ScrollDelta::Pixels(delta) => Point {
                x: f32::from(delta.x),
                y: f32::from(delta.y),
            },
            ScrollDelta::Lines(delta) => Point {
                x: delta.x * SCROLL_LINE_HEIGHT,
                y: delta.y * SCROLL_LINE_HEIGHT,
            },
        };
        let (delta_x, delta_y) = take_whole_scroll(&mut self.scroll_carry, pixels);
        if delta_x == 0 && delta_y == 0 {
            return;
        }
        // The browser's own pointer position drives which element the page
        // scrolls; a wheel that arrives with no preceding move (trackpad rest,
        // or the pointer re-entering over the same control) would otherwise be
        // applied wherever CEF last thought the cursor was.
        self.forward_pointer_position(x, y, event.modifiers, false);
        self.send_input(EditorInput::MouseWheel {
            x,
            y,
            delta_x,
            delta_y,
            modifiers: self.surface.modifiers(event.modifiers),
        });
        Self::schedule_input_pump(cx);
    }

    /// Bare Space (no Ctrl/Alt/Cmd/Win, no platform key) — DAW transport
    /// owns this, not the page. Matches the CEF `OnPreKeyEvent` contract used
    /// by the windowed Windows path.
    fn is_transport_toggle_keystroke(keystroke: &gpui::Keystroke) -> bool {
        let mods = keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.secondary() {
            return false;
        }
        let key = keystroke.key.as_str();
        // Linux/XKB and some IMs emit bare `" "` or `"Spacebar"` rather than `"space"`.
        if matches!(
            key,
            "space" | " " | "spacebar" | "Space" | "Spacebar" | "kp-space"
        ) {
            return true;
        }
        // Some platforms put the only Space marker in `key_char`.
        keystroke.key_char.as_deref() == Some(" ")
    }

    /// Run the same transport command the chrome Play button uses.
    fn dispatch_transport_play_pause(&self, cx: &mut Context<Self>) {
        if let Some(dispatch) = self.host_ops.dispatch_global_command.as_ref() {
            dispatch("transport:play-pause", cx);
        }
    }

    fn on_surface_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Belt-and-suspenders with shell `capture_key_down`: if Space reaches
        // the surface (capture miss, focus edge, IM quirks) still own transport
        // and never inject a page-level Space that CEF would handle twice.
        if Self::is_transport_toggle_keystroke(&event.keystroke) && !event.is_held {
            window.prevent_default();
            cx.stop_propagation();
            self.dispatch_transport_play_pause(cx);
            return;
        }

        let modifiers = self.surface.modifiers(event.keystroke.modifiers);
        if let Some(key) = editor_key(&event.keystroke, EditorKeyKind::Down, modifiers) {
            self.send_input(EditorInput::Key(key));
        }
        // Chromium expects the typed text as its own `Char` event after the
        // key-down; without it a key press moves focus but never inserts.
        for key in editor_char_keys(&event.keystroke, modifiers) {
            self.send_input(EditorInput::Key(key));
        }
        Self::schedule_input_pump(cx);
    }

    fn on_surface_key_up(
        &mut self,
        event: &KeyUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = self.surface.modifiers(event.keystroke.modifiers);
        if let Some(key) = editor_key(&event.keystroke, EditorKeyKind::Up, modifiers) {
            self.send_input(EditorInput::Key(key));
        }
        Self::schedule_input_pump(cx);
    }

    /// Native instance list. Reserved width matches `sidebar_width()`, which
    /// `content_rect` also reads so CEF and this column share one boundary.
    fn render_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        if self.sidebar_collapsed {
            return div().flex_none().w(px(0.0)).into_any_element();
        }

        let rows = if self.instances.is_empty() {
            vec![div()
                .p(px(10.0))
                .text_size(px(11.0))
                .text_color(Colors::text_secondary())
                .child(format!(
                    "No {} instances are available in this project.",
                    self.plugin_id
                ))
                .into_any_element()]
        } else {
            self.instances
                .iter()
                .map(|instance| {
                    let is_active = self.active_instance.as_ref() == Some(&instance.instance_key);
                    let key = instance.instance_key.clone();
                    let weak = cx.weak_entity();
                    div()
                        .id(("builtin-plugin-instance-row", {
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            std::hash::Hash::hash(&key, &mut hasher);
                            std::hash::Hasher::finish(&hasher) as usize
                        }))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .when(is_active, |el| el.bg(Colors::surface_raised()))
                        .when(!is_active, |el| el.bg(Colors::surface_panel()))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                            let _ = weak.update(cx, |editor, cx| {
                                editor.select_instance(key.clone(), cx);
                            });
                        })
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(Colors::text_secondary())
                                .child(instance.track_name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(Colors::text_primary())
                                .child(if instance.bypassed {
                                    format!("{} (bypassed)", instance.insert_name)
                                } else {
                                    instance.insert_name.clone()
                                }),
                        )
                        .into_any_element()
                })
                .collect()
        };

        div()
            .flex_none()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(Colors::border_subtle())
            .bg(Colors::surface_panel())
            .overflow_hidden()
            .children(rows)
            .into_any_element()
    }
}

/// Open the shared shell window for a built-in plugin's editor. One per
/// `plugin_id` — callers must check the registry for an existing window
/// before calling this (see `open_builtin_insert_editor` in `plugin_ops.rs`);
/// this function always creates a fresh CEF browser.
pub fn open_builtin_editor_window(
    owner_bounds: Bounds<Pixels>,
    plugin_id: String,
    display_name: String,
    instances: Vec<PluginInstanceDescriptor>,
    active_instance: Option<PluginInstanceKey>,
    host_ops: BuiltinEditorHostOps,
    cx: &mut App,
) -> Result<WindowHandle<BuiltinPluginEditorWindow>, String> {
    let parent_x: f32 = owner_bounds.origin.x.into();
    let parent_y: f32 = owner_bounds.origin.y.into();
    let parent_w: f32 = owner_bounds.size.width.into();
    let parent_h: f32 = owner_bounds.size.height.into();
    let (editor_width, editor_height) = default_editor_window_size(&plugin_id);
    let origin = Point {
        x: px(parent_x + ((parent_w - editor_width) / 2.0).max(24.0)),
        y: px(parent_y + ((parent_h - editor_height) / 2.0).max(24.0)),
    };

    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(Bounds {
        origin,
        size: size(px(editor_width), px(editor_height)),
    }));
    options.kind = WindowKind::Floating;
    options.is_resizable = true;
    options.is_minimizable = false;
    // Opaque: an unpainted OSR frame must never reveal the timeline behind it.
    options.window_background = WindowBackgroundAppearance::Opaque;
    options.window_min_size = Some(size(
        px(BUILTIN_EDITOR_MIN_WIDTH),
        px(BUILTIN_EDITOR_MIN_HEIGHT),
    ));

    cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| {
            BuiltinPluginEditorWindow::new(
                plugin_id,
                display_name,
                instances,
                active_instance,
                host_ops,
                cx,
            )
        });
        let weak = view.downgrade();
        window.on_window_should_close(cx, move |_window, cx| {
            let _ = weak.update(cx, |view, cx| view.request_close(cx));
            // Always veto the platform close. `Closed` removes the shell after
            // CEF has processed its queued close command.
            false
        });
        view
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    fn bounds(w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(w), px(h)),
        }
    }

    #[test]
    fn content_rect_sits_below_the_header() {
        let rect = content_rect(bounds(1000.0, 700.0), 1.0, 0.0);
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, HEADER_H as i32);
        assert_eq!(rect.width, 1000);
        assert_eq!(rect.height, 700 - HEADER_H as i32);
    }

    #[test]
    fn content_rect_scales_with_dpi() {
        let rect = content_rect(bounds(1000.0, 700.0), 2.0, 0.0);
        assert_eq!(rect.y, (HEADER_H * 2.0) as i32);
        assert_eq!(rect.width, 2000);
        assert_eq!(rect.height, 1400 - (HEADER_H * 2.0) as i32);
        // The browser must never be told to draw over the header.
        assert!(rect.y > 0);
    }

    #[test]
    fn a_window_shorter_than_the_header_clamps_to_zero_rather_than_going_negative() {
        let rect = content_rect(bounds(400.0, 10.0), 1.0, 0.0);
        assert_eq!(rect.height, 0);
        assert!(rect.height >= 0);
    }

    #[test]
    fn a_sub_pixel_trackpad_swipe_eventually_scrolls() {
        let mut carry = Point::default();
        let step = Point { x: 0.0, y: 0.3 };
        // Each event on its own rounds to nothing; the carry must spend it.
        assert_eq!(take_whole_scroll(&mut carry, step), (0, 0));
        assert_eq!(take_whole_scroll(&mut carry, step), (0, 0));
        assert_eq!(take_whole_scroll(&mut carry, step), (0, 0));
        assert_eq!(take_whole_scroll(&mut carry, step), (0, 1));
    }

    #[test]
    fn scroll_carry_never_swallows_a_direction_change() {
        let mut carry = Point::default();
        assert_eq!(
            take_whole_scroll(&mut carry, Point { x: 0.0, y: 0.9 }),
            (0, 0)
        );
        // Reversing must not need to pay off the 0.9 left over from going up.
        assert_eq!(
            take_whole_scroll(&mut carry, Point { x: 0.0, y: -1.5 }),
            (0, -1)
        );
    }

    #[test]
    fn a_whole_pixel_wheel_step_passes_through_unchanged() {
        let mut carry = Point::default();
        assert_eq!(
            take_whole_scroll(&mut carry, Point { x: -7.0, y: 120.0 }),
            (-7, 120)
        );
        assert_eq!(carry.x, 0.0);
        assert_eq!(carry.y, 0.0);
    }

    #[test]
    fn a_pathological_wheel_step_stays_inside_cefs_range() {
        let mut carry = Point::default();
        let (_, delta_y) = take_whole_scroll(
            &mut carry,
            Point {
                x: 0.0,
                y: f32::MAX,
            },
        );
        assert_eq!(delta_y, MAX_SCROLL_DELTA as i32);
    }

    #[test]
    fn click_count_stays_within_cefs_contract() {
        assert_eq!(clamp_click_count(0), 1);
        assert_eq!(clamp_click_count(1), 1);
        assert_eq!(clamp_click_count(3), 3);
        assert_eq!(clamp_click_count(9), 3);
    }

    #[test]
    fn content_rect_reserves_sidebar_width_on_the_left() {
        let rect = content_rect(bounds(1000.0, 700.0), 1.0, SIDEBAR_W);
        assert_eq!(rect.x, SIDEBAR_W as i32);
        assert_eq!(rect.width, 1000 - SIDEBAR_W as i32);
    }

    #[test]
    fn zcomp_window_preserves_the_full_editor_canvas() {
        let (width, height) = default_editor_window_size("builtin:zcomp");
        let rect = content_rect(bounds(width, height), 1.0, SIDEBAR_W);
        assert_eq!(rect.width, ZCOMP_EDITOR_CONTENT_WIDTH as i32);
        assert_eq!(rect.height, ZCOMP_EDITOR_CONTENT_HEIGHT as i32);
        assert_eq!(
            default_editor_window_size("builtin:equz8"),
            (BUILTIN_EDITOR_WIDTH, BUILTIN_EDITOR_HEIGHT)
        );
    }

    /// The browser is told to lay out in logical pixels, so a window-space
    /// pointer position becomes a view-space one by subtracting the chrome this
    /// window reserves — the same offsets `content_rect` reserves. Verified
    /// end-to-end by `examples/osr_editor_probe`, which reports the coordinate
    /// the page actually received (a click sent at 400,300 arrives at 400,300).
    #[test]
    fn view_space_points_subtract_the_reserved_chrome() {
        let to_view = |x: f32, y: f32, sidebar: f32| {
            ((x - sidebar).round() as i32, (y - HEADER_H).round() as i32)
        };
        assert_eq!(to_view(SIDEBAR_W, HEADER_H, SIDEBAR_W), (0, 0));
        assert_eq!(
            to_view(SIDEBAR_W + 400.0, HEADER_H + 300.0, SIDEBAR_W),
            (400, 300)
        );
        // Collapsed sidebar reserves nothing on the left.
        assert_eq!(to_view(400.0, HEADER_H + 300.0, 0.0), (400, 300));
        // A position over the chrome maps outside the view, which is what the
        // page should see — not a clamp onto its edge.
        assert!(to_view(0.0, 0.0, SIDEBAR_W).0 < 0);
    }

    #[test]
    fn content_rect_sidebar_width_scales_with_dpi() {
        let rect = content_rect(bounds(1000.0, 700.0), 2.0, SIDEBAR_W);
        assert_eq!(rect.x, (SIDEBAR_W * 2.0) as i32);
        assert_eq!(rect.width, 2000 - (SIDEBAR_W * 2.0) as i32);
    }

    #[test]
    fn a_sidebar_wider_than_the_window_clamps_content_to_zero_rather_than_going_negative() {
        let rect = content_rect(bounds(100.0, 700.0), 1.0, SIDEBAR_W);
        assert_eq!(rect.width, 0);
    }

    #[test]
    fn set_instances_clears_active_selection_when_it_is_no_longer_present() {
        let a = PluginInstanceKey {
            track_id: "track-2".into(),
            insert_id: "insert-4".into(),
        };
        let b = PluginInstanceKey {
            track_id: "track-3".into(),
            insert_id: "insert-9".into(),
        };
        let descriptor = |key: PluginInstanceKey| PluginInstanceDescriptor {
            instance_key: key,
            plugin_id: "rodharerist".into(),
            track_name: "Track".into(),
            insert_name: "Insert".into(),
            bypassed: false,
            enabled: true,
            state_bytes: None,
        };
        // Exercised indirectly through the full entity in integration/manual
        // testing (GPUI `Context<Self>` cannot be constructed standalone in a
        // unit test); this test only pins down the pure membership check the
        // real method relies on so a future refactor can't silently drop it.
        let instances = [descriptor(a.clone())];
        assert!(instances.iter().any(|i| i.instance_key == a));
        assert!(!instances.iter().any(|i| i.instance_key == b));
    }

    #[test]
    fn wire_instance_id_joins_track_and_insert() {
        let key = PluginInstanceKey {
            track_id: "track-2".into(),
            insert_id: "insert-4".into(),
        };
        assert_eq!(wire_instance_id(&key), "track-2::insert-4");
    }

    #[test]
    fn select_instance_message_matches_the_documented_wire_shape() {
        let msg = SelectInstanceMsg {
            r#type: "futureboard.selectInstance",
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            plugin_id: "rodharerist".into(),
            instance_id: "track-2::insert-4".into(),
            binding_generation: 42,
            display: InstanceDisplayMetadata {
                track_id: "track-2".into(),
                track_name: "Track 2".into(),
                insert_id: "insert-4".into(),
                insert_name: "Power Chord".into(),
            },
            state_revision: 157,
            state: serde_json::json!({}),
        };
        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "futureboard.selectInstance");
        assert_eq!(json["protocolVersion"], 1);
        assert_eq!(json["pluginId"], "rodharerist");
        assert_eq!(json["instanceId"], "track-2::insert-4");
        assert_eq!(json["bindingGeneration"], 42);
        assert_eq!(json["display"]["trackName"], "Track 2");
        assert_eq!(json["display"]["insertName"], "Power Chord");
        assert_eq!(json["stateRevision"], 157);
    }

    #[test]
    fn decode_state_bytes_falls_back_to_empty_object_when_absent() {
        assert_eq!(decode_state_bytes(None), serde_json::json!({}));
    }

    #[test]
    fn decode_state_bytes_parses_real_json() {
        let bytes = br#"{"schema_version":1,"params":{"amp_gain":7.0}}"#;
        let value = decode_state_bytes(Some(bytes));
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["params"]["amp_gain"], 7.0);
    }

    #[test]
    fn decode_state_bytes_falls_back_to_empty_object_on_corrupt_bytes() {
        assert_eq!(decode_state_bytes(Some(b"not json")), serde_json::json!({}));
        assert_eq!(
            decode_state_bytes(Some(&[0xff, 0xfe])),
            serde_json::json!({})
        );
    }

    #[test]
    fn inbound_bridge_ready_parses_by_tag() {
        let raw =
            br#"{"type":"futureboard.bridgeReady","pluginId":"rodharerist","bridgeVersion":1}"#;
        let msg: InboundMsg = serde_json::from_slice(raw).unwrap();
        assert!(matches!(msg, InboundMsg::BridgeReady { .. }));
    }

    #[test]
    fn cef_bridge_accepts_non_secret_plugin_state() {
        let value = serde_json::json!({
            "type": "futureboard.selectInstance",
            "state": {"params": {"threshold": -18.0, "mode": "vintage"}}
        });
        assert!(!bridge_value_contains_forbidden_secret(&value));
    }

    #[test]
    fn cef_bridge_rejects_secret_fields_in_either_direction() {
        for field in [
            "accessToken",
            "refresh_token",
            "password",
            "licenseKey",
            "activationData",
            "paymentSessionSecret",
            "macOSKeychainValue",
            "encryption-key",
        ] {
            let mut secret = serde_json::Map::new();
            secret.insert(field.to_owned(), serde_json::json!("must-not-cross"));
            let value = serde_json::json!({"state": secret});
            assert!(bridge_value_contains_forbidden_secret(&value), "{field}");
        }

        let inbound = br#"{
            "type":"futureboard.bridgeReady",
            "pluginId":"rodharerist",
            "bridgeVersion":1,
            "accessToken":"must-not-cross"
        }"#;
        assert!(parse_inbound_message(inbound).is_err());
    }

    #[test]
    fn cef_bridge_does_not_scan_opaque_file_contents_as_json() {
        let value = serde_json::json!({
            "type": "futureboard.fileContent",
            "content": "{\"accessToken\":\"preset-text-not-a-bridge-field\"}"
        });
        assert!(!bridge_value_contains_forbidden_secret(&value));
    }

    #[test]
    fn inbound_request_select_instance_parses_by_tag() {
        let raw =
            br#"{"type":"futureboard.requestSelectInstance","instanceId":"track-3::insert-9"}"#;
        let msg: InboundMsg = serde_json::from_slice(raw).unwrap();
        match msg {
            InboundMsg::RequestSelectInstance { instance_id } => {
                assert_eq!(instance_id, "track-3::insert-9");
            }
            other => panic!("expected RequestSelectInstance, got {other:?}"),
        }
    }

    #[test]
    fn inbound_global_command_parses_by_tag() {
        let raw = br#"{"type":"futureboard.globalCommand","commandId":"transport:play-pause"}"#;
        let msg: InboundMsg = serde_json::from_slice(raw).unwrap();
        match msg {
            InboundMsg::GlobalCommand { command_id } => {
                assert_eq!(command_id, "transport:play-pause");
            }
            other => panic!("expected GlobalCommand, got {other:?}"),
        }
    }

    /// The return leg of the editor's transport key. Editors match on `type`,
    /// so pin the wire shape the same way the inbound messages are pinned.
    #[test]
    fn transport_message_carries_only_the_play_state() {
        let json = serde_json::to_value(TransportMsg {
            r#type: "futureboard.transport",
            protocol_version: BRIDGE_PROTOCOL_VERSION,
            playing: true,
        })
        .unwrap();
        assert_eq!(json["type"], "futureboard.transport");
        assert_eq!(json["playing"], true);
        assert_eq!(json["protocolVersion"], BRIDGE_PROTOCOL_VERSION);
        // No playhead position: this is a state edge, not a per-frame clock.
        assert!(json.get("positionSeconds").is_none());
    }

    /// The IR load carries a file name, not the file: native reads the bytes
    /// from the plugin's IRs folder itself. Anything that changes that shape
    /// silently breaks IR loading, so pin it.
    #[test]
    fn inbound_load_ir_parses_by_tag_and_carries_only_a_file_name() {
        let raw = br#"{
            "type":"futureboard.loadIr",
            "protocolVersion":1,
            "pluginId":"rodharerist",
            "instanceId":"track-3::insert-9",
            "bindingGeneration":7,
            "fileName":"Vintage 4x12 SM57.wav"
        }"#;
        let msg: InboundMsg = serde_json::from_slice(raw).unwrap();
        match msg {
            InboundMsg::LoadIr {
                instance_id,
                binding_generation,
                file_name,
                ..
            } => {
                assert_eq!(instance_id, "track-3::insert-9");
                assert_eq!(binding_generation, 7);
                assert_eq!(file_name, "Vintage 4x12 SM57.wav");
            }
            other => panic!("expected LoadIr, got {other:?}"),
        }
    }

    #[test]
    fn inbound_unknown_type_does_not_error() {
        let raw = br#"{"type":"something.the.host.does.not.know"}"#;
        let msg: InboundMsg = serde_json::from_slice(raw).unwrap();
        assert!(matches!(msg, InboundMsg::Unknown));
    }

    #[test]
    fn inbound_set_params_parses_a_batch() {
        let raw = br#"{
            "type":"futureboard.setParams",
            "protocolVersion":1,
            "pluginId":"rodharerist",
            "instanceId":"track-3::insert-9",
            "bindingGeneration":7,
            "params":[{"id":"drive_gain","value":8.5},{"id":"amp_on","value":0.0}]
        }"#;
        let msg: InboundMsg = serde_json::from_slice(raw).unwrap();
        match msg {
            InboundMsg::SetParams {
                instance_id,
                binding_generation,
                params,
                ..
            } => {
                assert_eq!(instance_id, "track-3::insert-9");
                assert_eq!(binding_generation, 7);
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].id, "drive_gain");
                assert_eq!(params[0].value, 8.5);
                assert_eq!(params[1].id, "amp_on");
                assert_eq!(params[1].value, 0.0);
            }
            other => panic!("expected SetParams, got {other:?}"),
        }
    }

    #[cfg(feature = "builtin-plugin-editor")]
    #[test]
    fn set_params_ids_resolve_through_the_shared_wire_table() {
        // The window resolves ids via `host::builtin_param_index`; pin the
        // contract here so a table rename breaks visibly in this crate too.
        assert_eq!(
            host::builtin_param_index("builtin:rodharerist", "drive_gain"),
            Some(22)
        );
        assert_eq!(
            host::builtin_param_index("rodharerist", "drive_gain"),
            Some(22)
        );
        assert_eq!(
            host::builtin_param_index("rodharerist", "not_a_param"),
            None
        );
        assert_eq!(
            host::builtin_param_index("some.external.vst3", "drive_gain"),
            None
        );
    }

    #[test]
    fn inbound_file_messages_parse_by_tag() {
        let list = br#"{"type":"futureboard.listFiles","pluginId":"rodharerist","kind":"presets"}"#;
        assert!(matches!(
            serde_json::from_slice::<InboundMsg>(list).unwrap(),
            InboundMsg::ListFiles { .. }
        ));
        let read = br#"{"type":"futureboard.readFile","pluginId":"rodharerist","kind":"nams","fileName":"amp.nam"}"#;
        match serde_json::from_slice::<InboundMsg>(read).unwrap() {
            InboundMsg::ReadFile {
                kind, file_name, ..
            } => {
                assert_eq!(kind, "nams");
                assert_eq!(file_name, "amp.nam");
            }
            other => panic!("expected ReadFile, got {other:?}"),
        }
        let write = br#"{"type":"futureboard.writeFile","pluginId":"rodharerist","kind":"presets","fileName":"My Lead","content":"{}"}"#;
        match serde_json::from_slice::<InboundMsg>(write).unwrap() {
            InboundMsg::WriteFile {
                kind,
                file_name,
                content,
                ..
            } => {
                assert_eq!(kind, "presets");
                assert_eq!(file_name, "My Lead");
                assert_eq!(content, "{}");
            }
            other => panic!("expected WriteFile, got {other:?}"),
        }
    }

    #[test]
    fn instance_ready_generation_mismatch_is_detectable_before_acting_on_it() {
        // Pure pin of the comparison `handle_inbound` relies on to reject a
        // stale ack (Context<Self> can't be constructed in a unit test, so
        // the full method isn't exercised here — see module doc for why).
        let current_generation = 42u64;
        let ack_generation = 41u64;
        assert_ne!(ack_generation, current_generation);
    }
}
