//! Native audio engine lifecycle owner for the Rust shell.
//!
//! `NativeAudioState` proves the Rust-Native Futureboard Studio binary can
//! own the [`AudioEngine`] lifecycle directly, without going through NAPI.
//!
//! Stage 1 scope: build, start, stop, poll stats, enumerate devices. No
//! timeline scheduling, mixer routing, or plugin processing yet.
//!
//! Failures never panic — errors are captured into `last_error` so the
//! status bar / inspector can surface them.

// The crate is published with `[lib] name = "DirectAudio"` so the N-API output
// is `DirectAudio.node`. Rust consumers import its symbols through that same
// name — alias it locally for readability.
use DirectAudio::{
    AudioBackend, AudioDeviceId, AudioEngine, EngineConfig, EngineDeviceInfo, EngineStats,
    SphereAudioError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    pub config: EngineConfig,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            config: AudioEngine::default_config(),
        }
    }
}

pub struct NativeAudioState {
    /// Last successfully applied audio settings.
    pub active_settings: AudioSettings,
    /// UI-editable settings. Backend changes reset selected devices here only;
    /// no stream lifecycle work happens until Apply.
    pub draft_settings: AudioSettings,
    /// Compatibility mirror of `active_settings.config` for older call sites.
    pub config: EngineConfig,
    pub engine: Option<AudioEngine>,
    pub running: bool,
    pub last_error: Option<String>,
    pub stats: Option<EngineStats>,
}

impl Default for NativeAudioState {
    fn default() -> Self {
        let active_settings = AudioSettings::default();
        Self {
            config: active_settings.config.clone(),
            draft_settings: active_settings.clone(),
            active_settings,
            engine: None,
            running: false,
            last_error: None,
            stats: None,
        }
    }
}

// Stage 1: `start` / `stop` / `toggle_transport` / `set_backend` are part of
// the public API the next stages will wire into the UI/command dispatcher.
// They are intentionally unused right now.
#[allow(dead_code)]
impl NativeAudioState {
    /// Build a fresh state with default configuration. Does **not** create
    /// the engine yet — call [`Self::initialize_engine`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the [`AudioEngine`] handle. Idempotent: re-calling rebuilds
    /// the handle (e.g. after a config change).
    pub fn initialize_engine(&mut self) -> Result<(), SphereAudioError> {
        match AudioEngine::new(self.active_settings.config.clone()) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Open the audio device and start the stream. Lazily initializes the
    /// engine handle if needed.
    pub fn start(&mut self) -> Result<(), SphereAudioError> {
        if self.engine.is_none() {
            self.initialize_engine()?;
        }
        let engine = self
            .engine
            .as_mut()
            .ok_or(SphereAudioError::EngineNotOpen)?;
        match engine.start() {
            Ok(()) => {
                self.running = true;
                self.last_error = None;
                self.refresh_stats();
                Ok(())
            }
            Err(e) => {
                self.running = false;
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Stop the stream and close the device. Safe to call when already
    /// stopped — does nothing.
    pub fn stop(&mut self) -> Result<(), SphereAudioError> {
        if let Some(engine) = self.engine.as_mut() {
            if let Err(e) = engine.stop() {
                self.last_error = Some(e.to_string());
                return Err(e);
            }
        }
        self.running = false;
        self.refresh_stats();
        Ok(())
    }

    /// Toggle the transport play/pause if the engine is running. Returns
    /// the new transport state, or `None` if the engine is not started.
    pub fn toggle_transport(&mut self) -> Option<bool> {
        let engine = self.engine.as_ref()?;
        match engine.toggle_transport() {
            Ok(playing) => {
                self.last_error = None;
                Some(playing)
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                None
            }
        }
    }

    /// Refresh the cached [`EngineStats`] snapshot. Cheap — used by status
    /// bar polling.
    pub fn refresh_stats(&mut self) -> Option<&EngineStats> {
        if let Some(engine) = self.engine.as_ref() {
            let s = engine.stats();
            self.running = s.running;
            self.stats = Some(s);
        }
        self.stats.as_ref()
    }

    /// Enumerate output devices for the selected draft backend. This keeps the
    /// dropdown backend-scoped even before Apply is pressed.
    pub fn list_devices(&self) -> Vec<EngineDeviceInfo> {
        self.engine
            .as_ref()
            .map(|e| e.list_output_devices_for_backend(self.draft_settings.config.backend))
            .unwrap_or_default()
    }

    pub fn list_input_devices(&self) -> Vec<EngineDeviceInfo> {
        self.engine
            .as_ref()
            .map(|e| e.list_input_devices_for_backend(self.draft_settings.config.backend))
            .unwrap_or_default()
    }

    /// Engine semver — useful for the About box.
    pub fn version(&self) -> Option<String> {
        self.engine.as_ref().map(|e| e.version())
    }

    /// Update the draft backend only. Device selections are backend-scoped, so
    /// switching backend clears both selected devices and does not touch streams.
    pub fn set_backend(&mut self, backend: AudioBackend) {
        if self.draft_settings.config.backend != backend {
            self.draft_settings.config.backend = backend;
            self.draft_settings.config.input_device = None;
            self.draft_settings.config.output_device = None;
        }
    }

    pub fn set_output_device(&mut self, device: Option<AudioDeviceId>) {
        self.draft_settings.config.output_device = device;
    }

    pub fn set_input_device(&mut self, device: Option<AudioDeviceId>) {
        self.draft_settings.config.input_device = device;
    }

    pub fn apply_audio_settings(&mut self) -> Result<(), SphereAudioError> {
        self.apply_audio_settings_with(|state, config| {
            state.stop()?;
            state.engine = Some(AudioEngine::new(config.clone())?);
            state.start()?;
            Ok(())
        })
    }

    fn apply_audio_settings_with<F>(&mut self, mut apply_stream: F) -> Result<(), SphereAudioError>
    where
        F: FnMut(&mut Self, &EngineConfig) -> Result<(), SphereAudioError>,
    {
        let previous = self.active_settings.clone();
        let draft = self.draft_settings.clone();
        if let Err(error) = AudioEngine::validate_config(&draft.config) {
            self.draft_settings = previous.clone();
            self.config = previous.config;
            let message = format!(
                "{} Apply failed: {}. Reverted to previous audio settings.",
                draft.config.backend.display_name(),
                error
            );
            self.last_error = Some(message.clone());
            return Err(SphereAudioError::StreamOpenFailed(message));
        }

        if let Err(error) = apply_stream(self, &draft.config) {
            self.active_settings = previous.clone();
            self.draft_settings = previous.clone();
            self.config = previous.config;
            let message = format!(
                "{} Apply failed: {}. Reverted to previous audio settings.",
                draft.config.backend.display_name(),
                error
            );
            self.last_error = Some(message.clone());
            return Err(SphereAudioError::StreamOpenFailed(message));
        }

        self.active_settings = draft.clone();
        self.draft_settings = draft;
        self.config = self.active_settings.config.clone();
        self.last_error = None;
        Ok(())
    }
}

impl Drop for NativeAudioState {
    fn drop(&mut self) {
        if self.running {
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasapi_id() -> AudioDeviceId {
        AudioDeviceId::WasapiEndpoint("wasapi-endpoint".into())
    }

    fn wdm_ks_id() -> AudioDeviceId {
        AudioDeviceId::WdmKsFilterPin {
            filter_path: "ks-filter".into(),
            pin_id: 3,
        }
    }

    #[test]
    fn backend_change_resets_selected_devices() {
        let mut state = NativeAudioState::new();
        state.set_output_device(Some(AudioDeviceId::DauxEndpoint("out".into())));
        state.set_input_device(Some(AudioDeviceId::DauxEndpoint("in".into())));

        state.set_backend(AudioBackend::WdmKs);

        assert_eq!(state.draft_settings.config.backend, AudioBackend::WdmKs);
        assert_eq!(state.draft_settings.config.output_device, None);
        assert_eq!(state.draft_settings.config.input_device, None);
        assert_eq!(state.active_settings.config.backend, AudioBackend::Auto);
    }

    #[test]
    fn wasapi_device_cannot_be_applied_to_wdm_ks() {
        let mut state = NativeAudioState::new();
        state.set_backend(AudioBackend::WdmKs);
        state.set_output_device(Some(wasapi_id()));

        let result = state.apply_audio_settings_with(|_, _| Ok(()));

        assert!(result.is_err());
        assert_eq!(state.active_settings.config.backend, AudioBackend::Auto);
        assert_eq!(state.draft_settings.config.backend, AudioBackend::Auto);
        assert!(state
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("DAUx WDM-KS Apply failed"));
    }

    #[test]
    fn wdm_ks_device_cannot_be_applied_to_wasapi() {
        let mut state = NativeAudioState::new();
        state.set_backend(AudioBackend::WasapiExclusive);
        state.set_output_device(Some(wdm_ks_id()));

        let result = state.apply_audio_settings_with(|_, _| Ok(()));

        assert!(result.is_err());
        assert_eq!(state.active_settings.config.backend, AudioBackend::Auto);
        assert_eq!(state.draft_settings.config.backend, AudioBackend::Auto);
        assert!(state
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("DAUx WASAPI Exclusive Apply failed"));
    }

    #[test]
    fn wasapi_shared_accepts_daux_system_device() {
        let mut state = NativeAudioState::new();
        state.set_backend(AudioBackend::WasapiShared);
        state.set_output_device(Some(AudioDeviceId::DauxEndpoint("system-default".into())));

        state.apply_audio_settings_with(|_, _| Ok(())).unwrap();

        assert_eq!(
            state.active_settings.config.backend,
            AudioBackend::WasapiShared
        );
    }

    #[test]
    fn asio_device_cannot_be_applied_to_wasapi_exclusive() {
        let mut state = NativeAudioState::new();
        state.set_backend(AudioBackend::WasapiExclusive);
        state.set_output_device(Some(AudioDeviceId::AsioDevice("asio-driver".into())));

        let result = state.apply_audio_settings_with(|_, _| Ok(()));

        assert!(result.is_err());
        assert!(state
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("DAUx WASAPI Exclusive Apply failed"));
    }

    #[test]
    fn failed_apply_preserves_previous_active_settings() {
        let mut state = NativeAudioState::new();
        state.set_backend(AudioBackend::Cpal);
        state.set_output_device(Some(AudioDeviceId::DauxEndpoint("cpal-out".into())));
        state.apply_audio_settings_with(|_, _| Ok(())).unwrap();
        let previous = state.active_settings.clone();

        state.set_backend(AudioBackend::WasapiExclusive);
        state.set_output_device(Some(wasapi_id()));
        let result = state.apply_audio_settings_with(|_, _| {
            Err(SphereAudioError::StreamOpenFailed(
                "simulated open failure".into(),
            ))
        });

        assert!(result.is_err());
        assert_eq!(state.active_settings, previous);
        assert_eq!(state.draft_settings, previous);
        assert!(state
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("DAUx WASAPI Exclusive Apply failed"));
    }

    #[test]
    fn successful_apply_commits_draft_to_active_settings() {
        let mut state = NativeAudioState::new();
        state.set_backend(AudioBackend::WasapiExclusive);
        state.set_output_device(Some(wasapi_id()));

        state.apply_audio_settings_with(|_, _| Ok(())).unwrap();

        assert_eq!(
            state.active_settings.config.backend,
            AudioBackend::WasapiExclusive
        );
        assert_eq!(state.draft_settings, state.active_settings);
        assert_eq!(state.config, state.active_settings.config);
        assert_eq!(state.last_error, None);
    }
}
