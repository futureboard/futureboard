//! Audio Jam inside Futureboard Studio.
//!
//! ```txt
//! Studio UI (GPUI)
//!   │
//!   ├─ JamController ──▶ sphere-jam-client  (REST, signaling, media, clock)
//!   │        │
//!   │        ├─ JamEngineSink   ──▶ SphereDirectAudioEngine jam bus ──▶ track
//!   │        └─ JamEngineSource ◀── SphereDirectAudioEngine jam bus ◀── master
//!   │
//!   └─ Audio Connections ──▶ jam streams appear as input ports
//! ```
//!
//! Three decisions shape everything here.
//!
//! **The engine keeps the device.** A jam is a producer and consumer of buffers
//! the existing engine already fills and drains. It opens no WASAPI, CoreAudio
//! or ALSA client of its own, so there is nothing to contend for and no second
//! clock to reconcile.
//!
//! **A remote stream is an input port.** Routing a performer to a track goes
//! through the same Audio Connections layer hardware does — stable ids,
//! non-destructive device loss, one place that maps logical to physical. The
//! only difference is the device id prefix; see
//! [`DirectAudio::JAM_DEVICE_PREFIX`].
//!
//! **Identity is the account, never the username.** A saved routing stores a
//! user id; the display name follows whatever that account calls itself today.
//!
//! Nothing in this module is called from an audio callback.

pub mod publish;
pub mod resample;
pub mod sink;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use sphere_jam_client::api::{CreateInviteRequest, CreateJamRequest, JamApiClient};
use sphere_jam_client::bridge::{JamPublishRequest, JamPublishSourceKind};
use sphere_jam_client::clock::SessionClock;
use sphere_jam_client::config::JamConfig;
use sphere_jam_client::credentials::{JamCredentialProvider, SharedCredentials};
use sphere_jam_client::error::{JamError, Result};
use sphere_jam_client::ids::{JamId, StreamId, UserId};
use sphere_jam_client::protocol::{ParticipantSummary, StreamSummary};
use sphere_jam_client::session::{JamEvent, JamSession, JamSessionOptions, JamSnapshot, JamState};
use DirectAudio::engine::SharedState;
use DirectAudio::jam_bus::{jam_device_id, publish_key_track, PUBLISH_KEY_MASTER};

use crate::audio_connections::{AudioConnectionDirection, AvailablePort, AvailablePorts};

pub use publish::JamEngineSource;
pub use sink::JamEngineSink;

/// The account token Studio already holds.
///
/// The jam client never learns a password and never runs a sign-in flow. It
/// asks for a bearer token at the moment it needs one, so signing out in
/// Studio takes effect on the next jam call rather than on the next restart.
struct StudioCredentials;

impl JamCredentialProvider for StudioCredentials {
    fn access_token(&self) -> Result<String> {
        crate::auth::session_token()
            .ok_or_else(|| JamError::Auth("no Futureboard account is signed in".to_string()))
    }

    fn account_hint(&self) -> Option<String> {
        crate::auth::current_profile().map(|profile| profile.id)
    }
}

/// One remote stream, flattened for the UI and for the routing layer.
#[derive(Debug, Clone, Default)]
pub struct JamStreamView {
    pub stream_id: String,
    pub user_id: String,
    pub device_id: String,
    /// The `@handle` of the publisher, for display only.
    pub handle: String,
    pub display_name: String,
    pub stream_name: String,
    pub channels: usize,
    pub channel_labels: Vec<String>,
    pub sample_rate: i32,
    pub codec: String,
    /// Whether the server has resolved a format for this receiver — that is,
    /// whether audio is actually on its way.
    pub receiving: bool,
    /// Peak since the last UI read, linear.
    pub peak: f32,
    /// Round trip of the packets carrying it, in milliseconds.
    pub rtt_ms: f64,
}

impl JamStreamView {
    /// What a track input menu shows: `@hachi224 · Guitar`.
    pub fn menu_label(&self) -> String {
        if self.handle.is_empty() {
            self.stream_name.clone()
        } else {
            format!("{} · {}", self.handle, self.stream_name)
        }
    }

    /// The Audio Connections device id this stream is addressed by.
    pub fn device_id(&self) -> String {
        jam_device_id(&self.stream_id)
    }
}

/// The whole jam, as a UI frame sees it.
///
/// Published once per poll rather than read live, so a panel render never takes
/// a lock the network threads also want.
#[derive(Debug, Clone, Default)]
pub struct JamUiState {
    pub configured: bool,
    pub signed_in: bool,
    pub state_label: String,
    pub connected: bool,
    pub jam_id: String,
    pub jam_name: String,
    pub public_id: String,
    pub join_url: String,
    pub region_label: String,
    pub transport_label: String,
    pub rtt_ms: f64,
    pub clock_offset_ms: f64,
    pub clock_drift_ppm: f64,
    pub clock_locked: bool,
    pub packets_in: u64,
    pub packets_out: u64,
    pub participants: Vec<ParticipantSummary>,
    pub streams: Vec<JamStreamView>,
    /// Streams this Studio is publishing.
    pub publishing: Vec<String>,
    pub last_error: Option<String>,
    /// The most recent invite link minted from this Studio, shown once so it
    /// can be copied. Never persisted.
    pub invite_link: Option<String>,
}

impl JamUiState {
    /// Every stream belonging to one account, in menu order.
    pub fn streams_for_user(&self, user_id: &str) -> Vec<&JamStreamView> {
        self.streams
            .iter()
            .filter(|stream| stream.user_id == user_id)
            .collect()
    }

    /// Accounts present in the room, in display order, each with its streams.
    pub fn by_participant(&self) -> Vec<(&ParticipantSummary, Vec<&JamStreamView>)> {
        self.participants
            .iter()
            .map(|participant| {
                let streams = self
                    .streams
                    .iter()
                    .filter(|stream| {
                        stream.user_id == participant.user.id
                            && stream.device_id == participant.device_id
                    })
                    .collect();
                (participant, streams)
            })
            .collect()
    }
}

/// The published UI state. Separate from the controller so the routing layer
/// can read it without ever waiting on the network.
fn ui_state() -> &'static RwLock<JamUiState> {
    static STATE: OnceLock<RwLock<JamUiState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(JamUiState::default()))
}

/// The current jam, for a UI frame.
pub fn snapshot() -> JamUiState {
    ui_state()
        .read()
        .map(|state| state.clone())
        .unwrap_or_default()
}

/// Jam streams as Audio Connections input ports.
///
/// This is what makes a remote performer selectable in the same Input menu a
/// hardware channel is. One port per channel, named the way the publisher
/// labelled it, under a device id that carries the stream's immutable id.
pub fn available_ports() -> AvailablePorts {
    let state = snapshot();
    let mut ports = Vec::new();
    for stream in &state.streams {
        let device_id = stream.device_id();
        let device_name = stream.menu_label();
        for channel in 0..stream.channels.max(1) {
            let port_name = stream
                .channel_labels
                .get(channel)
                .cloned()
                .unwrap_or_else(|| match (stream.channels, channel) {
                    (1, _) => "Mono".to_string(),
                    (2, 0) => "L".to_string(),
                    (2, 1) => "R".to_string(),
                    _ => format!("Ch {}", channel + 1),
                });
            ports.push(AvailablePort {
                device_id: device_id.clone(),
                device_name: device_name.clone(),
                port_name,
                port_index: channel as u32,
                direction: AudioConnectionDirection::Input,
            });
        }
    }
    AvailablePorts { ports }
}

/// The process-wide controller.
///
/// One jam per Studio: a second one is a second application, which keeps event
/// ordering, the resume token and the audio bus unambiguous.
fn controller() -> &'static Mutex<Option<JamController>> {
    static CONTROLLER: OnceLock<Mutex<Option<JamController>>> = OnceLock::new();
    CONTROLLER.get_or_init(|| Mutex::new(None))
}

/// Install the controller once the engine exists. Idempotent.
///
/// Installing also starts the poll thread, because the jam has to keep its
/// published state current whether or not the panel is open: tracks routed to a
/// remote performer keep playing after the window is closed, and the routing
/// layer reads the same snapshot to decide what is still selectable.
pub fn install(shared: Arc<SharedState>, engine: Option<DirectAudio::AudioEngine>) -> Result<()> {
    let mut guard = controller()
        .lock()
        .map_err(|_| JamError::Session("the jam controller lock was poisoned".to_string()))?;
    if guard.is_some() {
        return Ok(());
    }
    let created = JamController::new(shared, engine)?;
    created.publish_ui_state();
    *guard = Some(created);
    drop(guard);
    spawn_poll_thread();
    Ok(())
}

/// How often the published state is refreshed while a jam is live. The rest of
/// Studio meters at 30 Hz; anything faster repaints for packets nobody can see
/// arriving.
const POLL_ACTIVE: std::time::Duration = std::time::Duration::from_millis(33);

/// How often it is refreshed while nothing is connected. A lock and an early
/// return four times a second is not worth optimising further, and it is what
/// notices the moment a jam starts.
const POLL_IDLE: std::time::Duration = std::time::Duration::from_millis(250);

/// Keep the published jam state current, independently of any window.
fn spawn_poll_thread() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("jam-poll".to_string())
        .spawn(|| loop {
            let live = match controller().lock() {
                Ok(mut guard) => match guard.as_mut() {
                    Some(controller) => {
                        controller.poll();
                        controller.state().live()
                    }
                    None => false,
                },
                // A poisoned lock means a panic elsewhere already took the
                // controller down; there is nothing left to poll.
                Err(_) => return,
            };
            std::thread::sleep(if live { POLL_ACTIVE } else { POLL_IDLE });
        });
}

/// Whether a controller has been installed.
pub fn installed() -> bool {
    controller()
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Run `edit` against the installed controller.
///
/// Returns an error rather than silently doing nothing when no controller
/// exists: a menu item that appears to work and does not is worse than one that
/// explains itself.
pub fn with_controller<T>(edit: impl FnOnce(&mut JamController) -> Result<T>) -> Result<T> {
    let mut guard = controller()
        .lock()
        .map_err(|_| JamError::Session("the jam controller lock was poisoned".to_string()))?;
    match guard.as_mut() {
        Some(controller) => edit(controller),
        None => Err(JamError::Session(
            "the audio engine is not running yet".to_string(),
        )),
    }
}

/// Drain jam events and refresh the published UI state.
///
/// Called once per UI frame. Returns the events so a caller can react to a
/// stream appearing — re-resolving track routing, for instance — without
/// polling the snapshot for differences.
pub fn poll() -> Vec<JamEvent> {
    let Ok(mut guard) = controller().lock() else {
        return Vec::new();
    };
    match guard.as_mut() {
        Some(controller) => controller.poll(),
        None => Vec::new(),
    }
}

/// The jam session's clock, when a session is running.
pub fn session_clock() -> Option<Arc<Mutex<SessionClock>>> {
    controller()
        .lock()
        .ok()?
        .as_ref()
        .map(|controller| Arc::clone(&controller.clock))
}

/// Owns the jam session and the two bridges into the engine.
pub struct JamController {
    config: JamConfig,
    credentials: SharedCredentials,
    shared: Arc<SharedState>,
    /// The engine, for the control-plane half of a track publish. `None` in a
    /// build with no engine running, where sharing a track is simply refused.
    engine: Option<DirectAudio::AudioEngine>,
    clock: Arc<Mutex<SessionClock>>,
    sink: Arc<JamEngineSink>,
    api: JamApiClient,
    session: Option<JamSession>,
    device_id: String,
    /// Publish keys currently bound in the engine bus, so leaving releases
    /// exactly what joining claimed.
    published_keys: Vec<String>,
    /// The last invite minted here, held only until the UI has shown it.
    invite_link: Option<String>,
    last_error: Option<String>,
}

impl JamController {
    fn new(shared: Arc<SharedState>, engine: Option<DirectAudio::AudioEngine>) -> Result<Self> {
        let config = JamConfig::from_env()?;
        let credentials: SharedCredentials = Arc::new(StudioCredentials);
        let clock = Arc::new(Mutex::new(SessionClock::default()));
        let sink = Arc::new(JamEngineSink::new(Arc::clone(&shared), Arc::clone(&clock)));
        let api = JamApiClient::new(config.clone(), Arc::clone(&credentials))?;
        Ok(Self {
            config,
            credentials,
            shared,
            engine,
            clock,
            sink,
            api,
            session: None,
            device_id: device_id(),
            published_keys: Vec::new(),
            invite_link: None,
            last_error: None,
        })
    }

    pub fn config(&self) -> &JamConfig {
        &self.config
    }

    pub fn api(&self) -> &JamApiClient {
        &self.api
    }

    pub fn state(&self) -> JamState {
        self.session
            .as_ref()
            .map(|session| session.state())
            .unwrap_or(JamState::Disconnected)
    }

    /// Create a jam and join it. Returns the shareable code.
    pub fn create_and_join(&mut self, name: &str) -> Result<String> {
        let created = self.api.create_jam(&CreateJamRequest {
            name: name.to_string(),
            region: self.config.preferred_region.wire_value().to_string(),
            ..Default::default()
        })?;
        let code = created.jam.public_id.clone();
        let jam_id = JamId::new(created.jam.id.clone());
        self.set_join_url(created.join_url.clone());
        self.join(jam_id, String::new())?;
        Ok(code)
    }

    /// Follow an invite link: exchange the secret, then join what it admitted
    /// this account to.
    pub fn join_with_invite(&mut self, code: &str, secret: &str) -> Result<()> {
        let exchanged = self.api.exchange_invite(secret, code)?;
        let jam_id = JamId::new(exchanged.jam.id.clone());
        self.join(jam_id, exchanged.access_token)
    }

    /// Join a jam this account is already a member of, or was invited to.
    pub fn join(&mut self, jam_id: JamId, access_token: String) -> Result<()> {
        if self.session.is_none() {
            let sink: Arc<dyn sphere_jam_client::JamAudioSink> =
                Arc::clone(&self.sink) as Arc<dyn sphere_jam_client::JamAudioSink>;
            let mut options = JamSessionOptions::new(self.device_id.clone(), sink);
            options.device_name = device_name();
            options.publish_sample_rate = PUBLISH_SAMPLE_RATE as i32;
            let session = JamSession::spawn_with_clock(
                self.config.clone(),
                Arc::clone(&self.credentials),
                options,
                Arc::clone(&self.clock),
            )?;
            self.session = Some(session);
        }
        let Some(session) = self.session.as_ref() else {
            return Err(JamError::Session("the jam worker is gone".to_string()));
        };
        session.join(jam_id, access_token)
    }

    /// Mint an invite for the current jam. The link is a bearer secret and is
    /// returned once; it is never stored.
    pub fn create_invite(&mut self, role: &str) -> Result<String> {
        let jam_id = self
            .current_jam_id()
            .ok_or_else(|| JamError::Session("not in a jam".to_string()))?;
        let created = self.api.create_invite(
            jam_id.as_str(),
            &CreateInviteRequest {
                role: role.to_string(),
                max_uses: 8,
                ..Default::default()
            },
        )?;
        self.invite_link = Some(created.link.clone());
        Ok(created.link)
    }

    /// Publish the master bus.
    ///
    /// The tap is on the master feed the engine already renders, before the
    /// Control Room touches it — a jam listener hears the mix, not this
    /// engineer's dim, mono or monitor inserts.
    pub fn publish_master(&mut self, name: &str) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Err(JamError::Session("not in a jam".to_string()));
        };
        if self
            .shared
            .jam_bus
            .bind_publish(PUBLISH_KEY_MASTER)
            .is_none()
        {
            return Err(JamError::Audio(
                "no publish slot is free in the audio engine".to_string(),
            ));
        }
        self.published_keys.push(PUBLISH_KEY_MASTER.to_string());

        let source = Arc::new(JamEngineSource::new(
            Arc::clone(&self.shared),
            Arc::clone(&self.clock),
            PUBLISH_KEY_MASTER,
            PUBLISH_SAMPLE_RATE,
        ));
        session.publish(
            JamPublishRequest::stereo(name.to_string(), JamPublishSourceKind::Master),
            source,
        )
    }

    /// Share one track or bus over the jam.
    ///
    /// The engine claims the publish slot and starts writing that track's
    /// post-fader block into it; the jam client pulls from the other side. The
    /// two halves are deliberately separate calls, because the engine owns the
    /// slot and the jam owns the stream, and a failure in either must not leave
    /// the other half running.
    pub fn publish_track(&mut self, track_id: &str, name: &str) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Err(JamError::Session("not in a jam".to_string()));
        };
        let Some(engine) = self.engine.as_ref() else {
            return Err(JamError::Audio(
                "the audio engine is not running".to_string(),
            ));
        };
        engine
            .set_track_jam_publish(track_id, true)
            .map_err(|error| JamError::Audio(error.to_string()))?;

        let key = publish_key_track(track_id);
        let source = Arc::new(JamEngineSource::new(
            Arc::clone(&self.shared),
            Arc::clone(&self.clock),
            key.clone(),
            PUBLISH_SAMPLE_RATE,
        ));
        match session.publish(
            JamPublishRequest::stereo(
                name.to_string(),
                JamPublishSourceKind::Track {
                    track_id: track_id.to_string(),
                },
            ),
            source,
        ) {
            Ok(()) => {
                self.published_keys.push(key);
                Ok(())
            }
            Err(error) => {
                // The engine is already writing into a slot nothing will read.
                // Stop it rather than leaving a tap running for a stream that
                // was never announced.
                let _ = engine.set_track_jam_publish(track_id, false);
                Err(error)
            }
        }
    }

    /// Stop sharing a track.
    pub fn unpublish_track(&mut self, track_id: &str) -> Result<()> {
        if let Some(engine) = self.engine.as_ref() {
            let _ = engine.set_track_jam_publish(track_id, false);
        }
        let key = publish_key_track(track_id);
        self.published_keys.retain(|held| held != &key);
        self.shared.jam_bus.release_publish(&key);
        Ok(())
    }

    /// Leave the jam, release every bus slot, and stop the worker.
    pub fn leave(&mut self) -> Result<()> {
        if let Some(session) = self.session.as_ref() {
            let _ = session.leave();
        }
        // Dropping the handle stops the worker and joins its threads.
        self.session = None;
        for key in self.published_keys.drain(..) {
            self.shared.jam_bus.release_publish(&key);
        }
        self.sink.clear();
        self.shared.jam_bus.release_all();
        self.publish_ui_state();
        Ok(())
    }

    /// Drain worker events and refresh the published UI state.
    pub fn poll(&mut self) -> Vec<JamEvent> {
        let events = self
            .session
            .as_ref()
            .map(|session| session.drain_events())
            .unwrap_or_default();
        for event in &events {
            if let JamEvent::Error { message, fatal, .. } = event {
                self.last_error = Some(message.clone());
                if *fatal {
                    for key in self.published_keys.drain(..) {
                        self.shared.jam_bus.release_publish(&key);
                    }
                }
            }
            if let JamEvent::StreamRemoved(stream) = event {
                self.shared.jam_bus.release_input(stream.as_str());
            }
        }
        self.publish_ui_state();
        events
    }

    fn current_jam_id(&self) -> Option<JamId> {
        self.session.as_ref()?.snapshot().jam_id
    }

    fn set_join_url(&mut self, url: String) {
        if let Ok(mut state) = ui_state().write() {
            state.join_url = url;
        }
    }

    /// Rebuild the published UI state from the session snapshot and the bus.
    fn publish_ui_state(&self) {
        let session_snapshot = self
            .session
            .as_ref()
            .map(|session| session.snapshot())
            .unwrap_or_default();
        let mut next = build_ui_state(&session_snapshot, &self.config, &self.shared);
        next.last_error = self
            .last_error
            .clone()
            .or_else(|| session_snapshot.last_error.clone());
        next.invite_link = self.invite_link.clone();
        next.publishing = self.published_keys.clone();

        if let Ok(mut state) = ui_state().write() {
            // The join url comes from the REST response, which the session
            // snapshot never sees; carry it forward rather than blanking it.
            let join_url = if next.join_url.is_empty() {
                state.join_url.clone()
            } else {
                next.join_url.clone()
            };
            next.join_url = join_url;
            *state = next;
        }
    }
}

impl Drop for JamController {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

/// The rate Studio publishes at.
///
/// 48 kHz is the session clock's own rate and what every client is required to
/// handle, so it is the one choice that never fails to negotiate. The project
/// rate is unaffected: the publish tap converts.
pub const PUBLISH_SAMPLE_RATE: u32 = 48_000;

/// Assemble the flattened UI state from a session snapshot.
///
/// Split out so it can be tested without a network: everything it needs is in
/// the snapshot and the bus.
fn build_ui_state(snapshot: &JamSnapshot, config: &JamConfig, shared: &SharedState) -> JamUiState {
    let mut streams = Vec::with_capacity(snapshot.streams.len());
    for summary in &snapshot.streams {
        let id = StreamId::new(summary.id.clone());
        let handle = snapshot
            .participants
            .iter()
            .find(|participant| participant.id == summary.participant_id)
            .map(|participant| participant.user.handle())
            .unwrap_or_default();
        let display_name = snapshot
            .participants
            .iter()
            .find(|participant| participant.id == summary.participant_id)
            .map(|participant| participant.user.label())
            .unwrap_or_default();
        let receiving = snapshot
            .formats
            .iter()
            .any(|(stream_id, _)| stream_id == &id);
        let peak = shared
            .jam_bus
            .input_slot_for(&summary.id)
            .and_then(|index| shared.jam_bus.input(index))
            .map(|slot| slot.take_peak())
            .unwrap_or(0.0);

        streams.push(JamStreamView {
            stream_id: summary.id.clone(),
            user_id: summary.user_id.clone(),
            device_id: summary.device_id.clone(),
            handle,
            display_name,
            stream_name: summary.name.clone(),
            channels: summary.channels.max(0) as usize,
            channel_labels: channel_labels(summary),
            sample_rate: summary.sample_rate,
            codec: summary.codec.as_str().to_string(),
            receiving,
            peak,
            rtt_ms: snapshot.rtt_ms,
        });
    }

    JamUiState {
        configured: true,
        signed_in: crate::auth::session_token().is_some() || config.dev_token.is_some(),
        state_label: snapshot.state_label.to_string(),
        connected: snapshot.state_label == JamState::Connected.label(),
        jam_id: snapshot
            .jam_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default(),
        jam_name: snapshot.jam_name.clone(),
        public_id: snapshot.public_id.clone(),
        join_url: snapshot.join_url.clone(),
        region_label: region_label(snapshot),
        transport_label: snapshot
            .transport
            .map(|kind| kind.as_str().to_uppercase())
            .unwrap_or_else(|| "—".to_string()),
        rtt_ms: snapshot.rtt_ms,
        clock_offset_ms: snapshot.clock_offset_ms,
        clock_drift_ppm: snapshot.clock_drift_ppm,
        clock_locked: snapshot.clock_locked,
        packets_in: snapshot.transport_stats.packets_in,
        packets_out: snapshot.transport_stats.packets_out,
        participants: snapshot.participants.clone(),
        streams,
        publishing: Vec::new(),
        last_error: snapshot.last_error.clone(),
        invite_link: None,
    }
}

fn channel_labels(summary: &StreamSummary) -> Vec<String> {
    let channels = summary.channels.max(0) as usize;
    (0..channels)
        .map(|index| {
            summary
                .channel_metadata
                .iter()
                .find(|meta| meta.index as usize == index)
                .map(|meta| {
                    if meta.label.is_empty() {
                        meta.role.clone()
                    } else {
                        meta.label.clone()
                    }
                })
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| match (channels, index) {
                    (1, _) => "Mono".to_string(),
                    (2, 0) => "L".to_string(),
                    (2, 1) => "R".to_string(),
                    _ => format!("Ch {}", index + 1),
                })
        })
        .collect()
}

fn region_label(snapshot: &JamSnapshot) -> String {
    if !snapshot.region.display_name.is_empty() {
        snapshot.region.display_name.clone()
    } else if !snapshot.region.city.is_empty() {
        snapshot.region.city.clone()
    } else {
        snapshot.region.id.clone()
    }
}

/// This installation's device id, persisted beside the session file.
///
/// The server treats (account, device) as one participant, so reusing it across
/// restarts is what makes a relaunch re-attach instead of leaving a ghost
/// participant in the room until its resume window expires.
fn device_id() -> String {
    const FILE: &str = "jam-device-id";
    let path = crate::paths::FutureboardPaths::resolve()
        .app_data
        .join(FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return existing;
        }
    }
    let minted = sphere_jam_client::generate_device_id("studio");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &minted);
    minted
}

fn device_name() -> String {
    // The machine name is what another participant sees next to the account
    // when the same person is in the room on two devices.
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("Futureboard Studio · {name}"))
        .unwrap_or_else(|| "Futureboard Studio".to_string())
}

/// Whether a jam device id names a stream this client is currently receiving.
///
/// Used by the routing layer to render a saved route as "waiting for" rather
/// than as broken when the performer has not joined yet.
pub fn stream_is_live(device_id: &str) -> bool {
    let Some(stream_id) = DirectAudio::jam_stream_id(device_id) else {
        return false;
    };
    snapshot()
        .streams
        .iter()
        .any(|stream| stream.stream_id == stream_id && stream.receiving)
}

/// The account a saved jam route belongs to, for the "waiting for" label.
pub fn user_for_stream(device_id: &str) -> Option<UserId> {
    let stream_id = DirectAudio::jam_stream_id(device_id)?;
    snapshot()
        .streams
        .iter()
        .find(|stream| stream.stream_id == stream_id)
        .map(|stream| UserId::new(stream.user_id.clone()))
}

/// Diagnostic counters for the debug panel.
pub fn bus_diagnostics(shared: &SharedState) -> Vec<(String, u64, u64)> {
    shared
        .jam_bus
        .bound_inputs()
        .into_iter()
        .filter_map(|(stream_id, index)| {
            let slot = shared.jam_bus.input(index)?;
            Some((stream_id, slot.underruns(), slot.overruns()))
        })
        .collect()
}

/// The engine sample rate the jam branch converts into.
pub fn engine_rate(shared: &SharedState) -> u32 {
    let rate = shared.sample_rate.load(Ordering::Relaxed);
    if rate == 0 {
        48_000
    } else {
        rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sphere_jam_client::protocol::{
        AudioCodec, AudioFormat, ChannelMetadata, SampleFormat, TransportKind, UserSummary,
    };

    fn participant(id: &str, user: &str, username: &str, device: &str) -> ParticipantSummary {
        ParticipantSummary {
            id: id.to_string(),
            user: UserSummary {
                id: user.to_string(),
                username: username.to_string(),
                display_name: username.to_string(),
                avatar_url: String::new(),
            },
            device_id: device.to_string(),
            connection_state: "connected".to_string(),
            ..Default::default()
        }
    }

    fn stream(id: &str, participant: &str, user: &str, device: &str, name: &str) -> StreamSummary {
        StreamSummary {
            id: id.to_string(),
            media_alias: 1,
            participant_id: participant.to_string(),
            user_id: user.to_string(),
            device_id: device.to_string(),
            name: name.to_string(),
            codec: AudioCodec::Pcm,
            sample_rate: 48_000,
            sample_format: SampleFormat::F32Le,
            channels: 2,
            channel_metadata: vec![
                ChannelMetadata {
                    index: 0,
                    label: String::new(),
                    role: "L".to_string(),
                },
                ChannelMetadata {
                    index: 1,
                    label: String::new(),
                    role: "R".to_string(),
                },
            ],
            active: true,
            ..Default::default()
        }
    }

    fn snapshot_with_one_guitar() -> JamSnapshot {
        JamSnapshot {
            state_label: JamState::Connected.label(),
            jam_id: Some(JamId::new("jam_1")),
            jam_name: "Saturday Session".to_string(),
            public_id: "J8KM4V".to_string(),
            participants: vec![participant("pcp_1", "usr_1", "hachi224", "studio-mac")],
            streams: vec![stream("str_1", "pcp_1", "usr_1", "studio-mac", "Guitar")],
            formats: vec![(
                StreamId::new("str_1"),
                AudioFormat {
                    codec: AudioCodec::Pcm,
                    sample_rate: 48_000,
                    channels: 2,
                    format: SampleFormat::F32Le,
                    bitrate: 0,
                    frame_samples: 128,
                },
            )],
            transport: Some(TransportKind::Udp),
            rtt_ms: 12.4,
            ..Default::default()
        }
    }

    #[test]
    fn a_room_snapshot_flattens_into_something_a_panel_can_render() {
        let shared = SharedState::default();
        let state = build_ui_state(&snapshot_with_one_guitar(), &JamConfig::default(), &shared);
        assert!(state.connected);
        assert_eq!(state.jam_name, "Saturday Session");
        assert_eq!(state.transport_label, "UDP");
        assert_eq!(state.streams.len(), 1);

        let guitar = &state.streams[0];
        assert_eq!(guitar.menu_label(), "@hachi224 · Guitar");
        assert_eq!(guitar.channel_labels, vec!["L", "R"]);
        assert!(guitar.receiving, "a selected format means audio is coming");
        assert_eq!(guitar.device_id(), "jam:str_1");
    }

    #[test]
    fn a_stream_with_no_selected_format_is_listed_but_not_marked_as_arriving() {
        let mut snapshot = snapshot_with_one_guitar();
        snapshot.formats.clear();
        let shared = SharedState::default();
        let state = build_ui_state(&snapshot, &JamConfig::default(), &shared);
        assert_eq!(state.streams.len(), 1);
        assert!(!state.streams[0].receiving);
    }

    #[test]
    fn streams_are_grouped_by_the_participant_that_publishes_them() {
        let mut snapshot = snapshot_with_one_guitar();
        // The same account on a second device, with its own stream.
        snapshot
            .participants
            .push(participant("pcp_2", "usr_1", "hachi224", "phone"));
        snapshot
            .streams
            .push(stream("str_2", "pcp_2", "usr_1", "phone", "Talkback"));

        let shared = SharedState::default();
        let state = build_ui_state(&snapshot, &JamConfig::default(), &shared);
        let grouped = state.by_participant();
        assert_eq!(grouped.len(), 2, "one account, two devices, two rows");
        assert_eq!(grouped[0].1[0].stream_name, "Guitar");
        assert_eq!(grouped[1].1[0].stream_name, "Talkback");
        // And both are still the same account.
        assert_eq!(state.streams_for_user("usr_1").len(), 2);
    }

    #[test]
    fn channel_labels_fall_back_to_the_layout_convention() {
        let mut summary = stream("str_1", "pcp_1", "usr_1", "dev", "Guitar");
        summary.channel_metadata.clear();
        assert_eq!(channel_labels(&summary), vec!["L", "R"]);

        summary.channels = 1;
        summary.channel_metadata.clear();
        assert_eq!(channel_labels(&summary), vec!["Mono"]);
    }
}
