//! ARA session ownership.
//!
//! Binding an audio clip to an ARA plug-in means four things have to agree: the
//! clip's [`ClipAraBinding`], the ARA document graph, the plug-in instance the
//! engine renders, and the saved archive. This module owns all four and is the
//! only place they are changed together.
//!
//! # Shape
//!
//! One ARA session per **(plug-in, track)**. A session owns one in-process VST3
//! instance, one ARA document, and one playback renderer, and every ARA clip on
//! that track shares them. That is what the engine's per-track renderer model
//! requires — a renderer's output lands in exactly one track's buffer — and it
//! matches how ARA is deployed in DAWs that host the plug-in as a track insert.
//!
//! # Threading
//!
//! [`sphere_ara_host::AraSession`] is the ARA model thread and lives here, on
//! the GPUI main thread. The plug-in calls back on its own threads; those
//! callbacks land on the provider objects below, which own everything they need
//! and hand results to the UI through bounded queues drained each frame. No
//! provider touches GPUI state or the audio thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sphere_ara_host::{
    AraAudioAccess, AraClipKey, AraGraph, AraHostError, AraModelObserver, AraModelUpdate,
    AraMusicalTimeline, AraRendererId, AraResult, AraRoles, AraSampleReader, AraSession,
    AraSessionConfig, AraSourceKey, AraTransportControl, AraTransportRequest,
};

/// Identifies one ARA session: a plug-in hosted on one track.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AraSessionKey {
    pub plugin_id: String,
    pub track_id: String,
}

/// How many pending plug-in callbacks the UI will buffer before dropping.
///
/// Bounded on purpose: a plug-in analysing a long clip emits progress
/// continuously, and an unbounded queue would grow without limit whenever the UI
/// falls behind. Dropping the oldest keeps the newest progress, which is the
/// only value anyone looks at.
const INBOX_CAPACITY: usize = 512;

/// Bounded drop-oldest queue shared with plug-in threads.
struct Inbox<T> {
    items: Mutex<std::collections::VecDeque<T>>,
}

impl<T> Default for Inbox<T> {
    fn default() -> Self {
        Self {
            items: Mutex::new(std::collections::VecDeque::with_capacity(INBOX_CAPACITY)),
        }
    }
}

impl<T> Inbox<T> {
    fn push(&self, item: T) {
        if let Ok(mut items) = self.items.lock() {
            if items.len() >= INBOX_CAPACITY {
                items.pop_front();
            }
            items.push_back(item);
        }
    }

    fn drain(&self) -> Vec<T> {
        self.items
            .lock()
            .map(|mut items| items.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Decoded PCM for every ARA audio source, keyed by asset id.
///
/// ARA hands the plug-in random access over the whole source, so the source is
/// decoded once in full rather than streamed: a streaming reader is a
/// forward-biased ring and would underrun on the seeks an analysis pass makes.
/// `load_audio_file` already refuses anything above its in-memory ceiling, so an
/// oversized file fails here instead of thrashing.
#[derive(Default)]
struct AudioLibrary {
    paths: Mutex<HashMap<AraSourceKey, PathBuf>>,
    decoded: Mutex<HashMap<AraSourceKey, Arc<DirectAudio::AudioFileBuffer>>>,
}

impl AudioLibrary {
    fn publish(&self, paths: HashMap<AraSourceKey, PathBuf>) {
        if let Ok(mut slot) = self.paths.lock() {
            *slot = paths;
        }
        // Drop decodes for sources that are no longer referenced, so removing
        // the last ARA clip of a file releases its buffer.
        if let (Ok(paths), Ok(mut decoded)) = (self.paths.lock(), self.decoded.lock()) {
            decoded.retain(|key, _| paths.contains_key(key));
        }
    }

    fn buffer(&self, key: &AraSourceKey) -> AraResult<Arc<DirectAudio::AudioFileBuffer>> {
        if let Ok(decoded) = self.decoded.lock() {
            if let Some(buffer) = decoded.get(key) {
                return Ok(Arc::clone(buffer));
            }
        }
        let path = self
            .paths
            .lock()
            .ok()
            .and_then(|paths| paths.get(key).cloned())
            .ok_or_else(|| {
                AraHostError::host(format!("no media path for ARA source '{}'", key.as_str()))
            })?;
        let buffer = DirectAudio::load_audio_file(&path.to_string_lossy())
            .map(Arc::new)
            .map_err(|error| {
                AraHostError::host(format!("could not decode '{}': {error}", path.display()))
            })?;
        if let Ok(mut decoded) = self.decoded.lock() {
            decoded.insert(key.clone(), Arc::clone(&buffer));
        }
        Ok(buffer)
    }
}

impl AraAudioAccess for AudioLibrary {
    fn open_reader(&self, source: &AraSourceKey) -> AraResult<Box<dyn AraSampleReader>> {
        let buffer = self.buffer(source)?;
        Ok(Box::new(BufferReader { buffer }))
    }
}

/// Planar view over one decoded, interleaved source buffer.
struct BufferReader {
    buffer: Arc<DirectAudio::AudioFileBuffer>,
}

impl AraSampleReader for BufferReader {
    fn channel_count(&self) -> usize {
        self.buffer.channels.max(1)
    }

    fn frame_count(&self) -> i64 {
        self.buffer.frames as i64
    }

    fn read_planar_f32(&mut self, start_frame: i64, out: &mut [&mut [f32]]) -> AraResult<()> {
        let channels = self.channel_count();
        if out.len() != channels {
            return Err(AraHostError::invalid("ARA read channel-count mismatch"));
        }
        let frames = self.buffer.frames as i64;
        for (channel, plane) in out.iter_mut().enumerate() {
            for (offset, sample) in plane.iter_mut().enumerate() {
                let frame = start_frame.saturating_add(offset as i64);
                // ARA permits reads that run past either end of the source; they
                // must come back as silence rather than as an error.
                *sample = if frame < 0 || frame >= frames {
                    0.0
                } else {
                    let index = frame as usize * channels + channel;
                    self.buffer.samples.get(index).copied().unwrap_or(0.0)
                };
            }
        }
        Ok(())
    }
}

/// Queues transport requests from plug-in editors for the UI to apply.
struct TransportBridge {
    inbox: Arc<Inbox<AraTransportRequest>>,
}

impl AraTransportControl for TransportBridge {
    fn request(&self, request: AraTransportRequest) {
        self.inbox.push(request);
    }
}

/// Queues plug-in model updates for the UI to apply.
struct ModelBridge {
    inbox: Arc<Inbox<AraModelUpdate>>,
}

impl AraModelObserver for ModelBridge {
    fn notify(&self, update: AraModelUpdate) {
        self.inbox.push(update);
    }
}

/// One ARA plug-in hosted on one track.
struct AraTrackSession {
    session: AraSession,
    /// Keeps the ARA main-factory class instance alive. Declared before
    /// `processor` so it is released before the module that owns it.
    _factory: DirectAudio::AraMainFactory,
    /// The in-process VST3 instance the engine renders. Cloned into the engine's
    /// renderer list; both handles share one C++ processor.
    processor: DirectAudio::Vst3RuntimeProcessor,
    renderer: AraRendererId,
    /// Clips currently assigned to the renderer, in graph order.
    clips: Vec<AraClipKey>,
    /// Sources this session's clips read from, so the audio library can be
    /// rebuilt without consulting the timeline.
    audio: Arc<AudioLibrary>,
    /// Archive identifier the plug-in writes under, captured at open.
    archive_id: String,
    plugin_name: String,
}

/// Every live ARA session, plus the queues its plug-ins post into.
#[derive(Default)]
pub struct AraState {
    sessions: HashMap<AraSessionKey, AraTrackSession>,
    transport_inbox: Arc<Inbox<AraTransportRequest>>,
    model_inbox: Arc<Inbox<AraModelUpdate>>,
    /// Archives loaded from the project, waiting for their session to open.
    ///
    /// A saved project restores clip bindings long before the plug-ins are
    /// instantiated, so the bytes are parked here and handed over the moment the
    /// matching session opens.
    pending_archives: HashMap<AraSessionKey, (String, Vec<u8>)>,
    /// Last error surfaced to the user, if any.
    pub last_error: Option<String>,
}

impl std::fmt::Debug for AraState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AraState")
            .field("sessions", &self.sessions.len())
            .field("pending_archives", &self.pending_archives.len())
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl AraState {
    /// Whether any clip is currently bound.
    pub fn is_active(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Whether this build can host ARA plug-ins at all.
    pub fn is_supported() -> bool {
        AraSession::is_supported()
    }

    /// Display name of the plug-in bound on this key, if any.
    pub fn plugin_name(&self, key: &AraSessionKey) -> Option<&str> {
        self.sessions
            .get(key)
            .map(|session| session.plugin_name.as_str())
    }

    /// Live sessions, for building the engine's renderer lists and for saving.
    pub fn keys(&self) -> impl Iterator<Item = &AraSessionKey> {
        self.sessions.keys()
    }

    /// Parks archives restored from a project until their sessions open.
    pub fn load_archives(
        &mut self,
        archives: impl IntoIterator<Item = (AraSessionKey, String, Vec<u8>)>,
    ) {
        self.pending_archives.clear();
        for (key, archive_id, data) in archives {
            self.pending_archives.insert(key, (archive_id, data));
        }
    }

    /// Serialises every live session's document for saving.
    ///
    /// Sessions that have never opened keep the archive they were loaded with,
    /// so saving a project whose ARA plug-ins were never instantiated does not
    /// throw their edits away.
    pub fn store_archives(&mut self) -> Vec<(AraSessionKey, String, Vec<u8>)> {
        let mut stored = Vec::new();
        for (key, session) in self.sessions.iter_mut() {
            match session.session.store_archive() {
                Ok(data) => stored.push((key.clone(), session.archive_id.clone(), data)),
                Err(error) => {
                    self.last_error = Some(format!(
                        "{} could not save its ARA state: {error}",
                        session.plugin_name
                    ));
                }
            }
        }
        for (key, (archive_id, data)) in &self.pending_archives {
            if !self.sessions.contains_key(key) {
                stored.push((key.clone(), archive_id.clone(), data.clone()));
            }
        }
        stored
    }

    /// Drains transport requests posted by plug-in editors.
    pub fn take_transport_requests(&self) -> Vec<AraTransportRequest> {
        self.transport_inbox.drain()
    }

    /// Drains model updates posted by plug-ins.
    pub fn take_model_updates(&self) -> Vec<AraModelUpdate> {
        self.model_inbox.drain()
    }

    /// Opens a session for `key`, or returns the existing one.
    ///
    /// `engine` instantiates the plug-in in this process; ARA cannot use the
    /// out-of-process host because its callbacks read project state directly.
    fn ensure_session(
        &mut self,
        engine: &DirectAudio::AudioEngine,
        key: &AraSessionKey,
        plugin_name: &str,
        plugin_path: &str,
        class_id: &str,
    ) -> AraResult<&mut AraTrackSession> {
        if self.sessions.contains_key(key) {
            return Ok(self
                .sessions
                .get_mut(key)
                .expect("presence checked immediately above"));
        }

        let processor = engine
            .create_ara_processor(plugin_path, class_id)
            .ok_or_else(|| {
                AraHostError::unsupported(format!("{plugin_name} could not be loaded for ARA"))
            })?;
        let factory = processor.ara_main_factory().ok_or_else(|| {
            AraHostError::unsupported(format!("{plugin_name} exposes no ARA main factory"))
        })?;
        // SAFETY: the guard owns a live reference to the plug-in's ARA
        // main-factory class, and is kept in the session beside the processor
        // whose module provides it.
        let factory_ptr = unsafe { sphere_ara_host::vst3_ara_factory(factory.as_ptr()) }?;

        let audio = Arc::new(AudioLibrary::default());
        let config = AraSessionConfig {
            document_name: Some(format!("Futureboard — {}", key.track_id)),
            audio: Arc::clone(&audio) as Arc<dyn AraAudioAccess>,
            transport: Arc::new(TransportBridge {
                inbox: Arc::clone(&self.transport_inbox),
            }),
            observer: Arc::new(ModelBridge {
                inbox: Arc::clone(&self.model_inbox),
            }),
        };
        // SAFETY: `factory_ptr` came from the guard above, which stays alive in
        // the session for as long as the returned document does.
        let mut session = unsafe { AraSession::open(factory_ptr, config) }?;

        // SAFETY: the component belongs to `processor`, which the session owns
        // and outlives every use of the binding.
        let renderer =
            unsafe { session.bind_renderer(processor.ara_component_unknown(), AraRoles::ALL) }?;
        let archive_id = session.factory().document_archive_id.clone();

        self.sessions.insert(
            key.clone(),
            AraTrackSession {
                session,
                _factory: factory,
                processor,
                renderer,
                clips: Vec::new(),
                audio,
                archive_id,
                plugin_name: plugin_name.to_owned(),
            },
        );
        Ok(self
            .sessions
            .get_mut(key)
            .expect("inserted immediately above"))
    }

    /// Applies a freshly built graph to one session and re-assigns its regions.
    ///
    /// Rendering is disabled across the change: a playback region may not be
    /// destroyed while a renderer still holds it, and the plug-in is entitled to
    /// assume the region set is stable while it renders.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        engine: &DirectAudio::AudioEngine,
        key: &AraSessionKey,
        plugin_name: &str,
        plugin_path: &str,
        class_id: &str,
        timeline: &AraMusicalTimeline,
        graph: &AraGraph,
        media_paths: HashMap<AraSourceKey, PathBuf>,
    ) -> AraResult<()> {
        let pending = self.pending_archives.remove(key);
        let session = self.ensure_session(engine, key, plugin_name, plugin_path, class_id)?;

        session.audio.publish(media_paths);
        session.session.set_rendering(session.renderer, false)?;
        session.session.set_musical_timeline(timeline)?;
        session.session.apply_graph(graph)?;

        // Restore before the regions are assigned and before playback, so the
        // plug-in's stored edits are in place the first time it renders.
        if let Some((archive_id, data)) = pending {
            if let Err(error) = session.session.restore_archive(&archive_id, &data) {
                self.last_error = Some(format!(
                    "{plugin_name} could not restore its saved ARA state: {error}"
                ));
                // Re-borrow: `self.last_error` above ended the previous borrow.
                return self.finish_apply(engine, key, graph);
            }
        }
        self.finish_apply(engine, key, graph)
    }

    fn finish_apply(
        &mut self,
        engine: &DirectAudio::AudioEngine,
        key: &AraSessionKey,
        graph: &AraGraph,
    ) -> AraResult<()> {
        let Some(session) = self.sessions.get_mut(key) else {
            return Err(AraHostError::invalid("ARA session disappeared mid-apply"));
        };
        session.clips = graph
            .regions
            .iter()
            .map(|region| region.key.clone())
            .collect();
        session
            .session
            .set_renderer_regions(session.renderer, &session.clips)?;
        session.session.set_rendering(session.renderer, true)?;

        Self::install_renderers(engine, &self.sessions, &key.track_id);
        Ok(())
    }

    /// Rebuilds and installs the engine's renderer list for one track.
    ///
    /// The engine replaces a track's whole list at once, so every session on the
    /// track has to be sent together — installing one at a time would drop the
    /// others.
    fn install_renderers(
        engine: &DirectAudio::AudioEngine,
        sessions: &HashMap<AraSessionKey, AraTrackSession>,
        track_id: &str,
    ) {
        let renderers: Vec<DirectAudio::RuntimeAraRenderer> = sessions
            .iter()
            .filter(|(key, _)| key.track_id == track_id)
            .map(|(key, session)| DirectAudio::RuntimeAraRenderer {
                instance_id: format!("ara:{}:{}", key.plugin_id, key.track_id),
                latency_samples: session.processor.get_latency_samples().max(0) as u32,
                processor: session.processor.clone(),
            })
            .collect();
        if let Err(error) = engine.set_ara_renderers(track_id.to_string(), renderers) {
            eprintln!("[ARA] could not install renderers for track {track_id}: {error}");
        }
    }

    /// Tears down one session and removes its renderer from the track.
    pub fn close(&mut self, engine: &DirectAudio::AudioEngine, key: &AraSessionKey) {
        let Some(session) = self.sessions.remove(key) else {
            return;
        };
        let plugin_name = session.plugin_name.clone();
        // Order matters: stop the engine calling the instance, then release the
        // ARA graph, then let the processor drop.
        Self::install_renderers(engine, &self.sessions, &key.track_id);
        let AraTrackSession {
            session, processor, ..
        } = session;
        if let Err(error) = session.close() {
            self.last_error = Some(format!("{plugin_name} reported errors on close: {error}"));
        }
        processor.set_destroy_reason("ara-unbound");
        self.pending_archives.remove(key);
    }

    /// Tears down every session, e.g. when a project closes.
    pub fn close_all(&mut self, engine: &DirectAudio::AudioEngine) {
        for key in self.sessions.keys().cloned().collect::<Vec<_>>() {
            self.close(engine, &key);
        }
        self.pending_archives.clear();
    }
}
