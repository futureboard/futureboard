use super::dispatch::{
    archive_interface, audio_interface, content_interface, model_update_interface,
    playback_interface, ServiceState,
};
use super::{
    ArchiveReaderId, ArchivingProvider, AudioAccessProvider, ContentAccessProvider,
    ModelUpdateProvider, PlaybackProvider,
};
use ara2_bridge_core::{ApiGeneration, AraError};
use ara2_bridge_sys::{
    ARAArchivingControllerInterface, ARAAudioAccessControllerInterface,
    ARAContentAccessControllerInterface, ARADocumentControllerHostInstance,
    ARAModelUpdateControllerInterface, ARAPlaybackControllerInterface,
};
use std::mem::size_of;
use std::ptr::{null, null_mut, NonNull};
use std::sync::Arc;

/// Builder for one document's stable ARA host-service instance.
#[derive(Default)]
pub struct HostServicesBuilder {
    audio: Option<Arc<dyn AudioAccessProvider>>,
    archiving: Option<Arc<dyn ArchivingProvider>>,
    content: Option<Arc<dyn ContentAccessProvider>>,
    model_update: Option<Arc<dyn ModelUpdateProvider>>,
    playback: Option<Arc<dyn PlaybackProvider>>,
}

impl HostServicesBuilder {
    /// Creates an empty builder. Audio and archiving providers are required.
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the required audio-access provider.
    pub fn audio(mut self, provider: impl AudioAccessProvider) -> Self {
        self.audio = Some(Arc::new(provider));
        self
    }

    /// Installs the required archiving provider.
    pub fn archiving(mut self, provider: impl ArchivingProvider) -> Self {
        self.archiving = Some(Arc::new(provider));
        self
    }

    /// Installs the optional model-update notification provider.
    pub fn model_updates(mut self, provider: impl ModelUpdateProvider) -> Self {
        self.model_update = Some(Arc::new(provider));
        self
    }

    /// Installs the optional content-access provider.
    pub fn content(mut self, provider: impl ContentAccessProvider) -> Self {
        self.content = Some(Arc::new(provider));
        self
    }

    /// Installs the optional playback-control request provider.
    pub fn playback(mut self, provider: impl PlaybackProvider) -> Self {
        self.playback = Some(Arc::new(provider));
        self
    }

    /// Builds stable host references and vtables for `generation`.
    pub fn build(self, generation: ApiGeneration) -> Result<HostServices, AraError> {
        let audio = self.audio.ok_or(AraError::InvalidArgument(
            "audio access provider is required",
        ))?;
        let archiving = self
            .archiving
            .ok_or(AraError::InvalidArgument("archiving provider is required"))?;
        let state = NonNull::new(Box::into_raw(Box::new(ServiceState::new(
            audio,
            archiving,
            self.content,
            self.model_update,
            self.playback,
        ))))
        .expect("Box never yields a null pointer");
        let audio_interface =
            NonNull::new(Box::into_raw(Box::new(audio_interface()))).expect("Box is non-null");
        let archive_interface =
            NonNull::new(Box::into_raw(Box::new(archive_interface(generation))))
                .expect("Box is non-null");
        // SAFETY: the state allocation was just created and remains owned below.
        let model_update_interface = unsafe { state.as_ref() }
            .has_model_updates()
            .then(|| NonNull::new(Box::into_raw(Box::new(model_update_interface(generation)))))
            .flatten();
        // SAFETY: same live state allocation.
        let content_interface = unsafe { state.as_ref() }
            .has_content()
            .then(|| NonNull::new(Box::into_raw(Box::new(content_interface()))))
            .flatten();
        // SAFETY: same live state allocation.
        let playback_interface = unsafe { state.as_ref() }
            .has_playback()
            .then(|| NonNull::new(Box::into_raw(Box::new(playback_interface()))))
            .flatten();
        let instance = NonNull::new(Box::into_raw(Box::new(ARADocumentControllerHostInstance {
            structSize: size_of::<ARADocumentControllerHostInstance>(),
            audioAccessControllerHostRef: state.as_ptr().cast(),
            audioAccessControllerInterface: audio_interface.as_ptr(),
            archivingControllerHostRef: state.as_ptr().cast(),
            archivingControllerInterface: archive_interface.as_ptr(),
            contentAccessControllerHostRef: content_interface
                .map_or(null_mut(), |_| state.as_ptr().cast()),
            contentAccessControllerInterface: content_interface.map_or(null(), |v| v.as_ptr()),
            modelUpdateControllerHostRef: model_update_interface
                .map_or(null_mut(), |_| state.as_ptr().cast()),
            modelUpdateControllerInterface: model_update_interface.map_or(null(), |v| v.as_ptr()),
            playbackControllerHostRef: playback_interface
                .map_or(null_mut(), |_| state.as_ptr().cast()),
            playbackControllerInterface: playback_interface.map_or(null(), |v| v.as_ptr()),
        })))
        .expect("Box is non-null");
        Ok(HostServices {
            instance,
            state,
            audio_interface,
            archive_interface,
            content_interface,
            model_update_interface,
            playback_interface,
        })
    }
}

/// Owned ARA host-service references and interfaces for one document.
pub struct HostServices {
    instance: NonNull<ARADocumentControllerHostInstance>,
    state: NonNull<ServiceState>,
    audio_interface: NonNull<ARAAudioAccessControllerInterface>,
    archive_interface: NonNull<ARAArchivingControllerInterface>,
    content_interface: Option<NonNull<ARAContentAccessControllerInterface>>,
    model_update_interface: Option<NonNull<ARAModelUpdateControllerInterface>>,
    playback_interface: Option<NonNull<ARAPlaybackControllerInterface>>,
}

impl HostServices {
    /// Returns the stable host instance record.
    pub fn instance(&self) -> &ARADocumentControllerHostInstance {
        // SAFETY: `self` uniquely owns the allocation and does not mutate the instance record.
        unsafe { self.instance.as_ref() }
    }

    /// Returns the stable raw host instance pointer passed to a plug-in factory.
    pub fn instance_ptr(&self) -> *const ARADocumentControllerHostInstance {
        self.instance.as_ptr()
    }

    /// Returns whether a contained provider panic quarantined this document.
    pub fn is_poisoned(&self) -> bool {
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.is_poisoned()
    }

    /// Returns callback diagnostics recorded so far.
    pub fn diagnostics(&self) -> Vec<String> {
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.diagnostics()
    }

    /// Resolves the persistent archive identifier for one synchronous reader token.
    pub fn document_archive_id<T>(&self, reader: &T) -> Result<Option<String>, AraError> {
        let reader = ArchiveReaderId::from_ptr(std::ptr::from_ref(reader).cast_mut());
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.document_archive_id(reader)
    }

    pub(crate) fn begin_provisional<T>(&self, reference: *mut T) {
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.begin_provisional(reference as usize);
    }

    pub(crate) fn finish_provisional<T>(&self, reference: *mut T) -> bool {
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.finish_provisional(reference as usize)
    }

    /// Synchronously revokes and destroys every reader for `source`.
    pub fn revoke_audio_source_readers(&self, source: super::AudioSourceId) {
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.revoke_audio_source(source);
    }

    pub(crate) fn revoke_all_document_readers(&self) {
        // SAFETY: the state allocation remains live until `Drop`.
        unsafe { self.state.as_ref() }.revoke_all_readers();
    }
}

// SAFETY: published records and vtables are immutable, providers are `Send + Sync`, and mutable
// callback state is protected by atomics and mutexes. The last owner must not be dropped while a
// foreign callback is in flight.
unsafe impl Send for HostServices {}
// SAFETY: same invariants as the `Send` implementation above.
unsafe impl Sync for HostServices {}

impl Drop for HostServices {
    fn drop(&mut self) {
        // SAFETY: these pointers came from distinct `Box::into_raw` calls and are reclaimed once.
        unsafe {
            drop(Box::from_raw(self.instance.as_ptr()));
            drop(Box::from_raw(self.audio_interface.as_ptr()));
            drop(Box::from_raw(self.archive_interface.as_ptr()));
            if let Some(interface) = self.content_interface {
                drop(Box::from_raw(interface.as_ptr()));
            }
            if let Some(interface) = self.model_update_interface {
                drop(Box::from_raw(interface.as_ptr()));
            }
            if let Some(interface) = self.playback_interface {
                drop(Box::from_raw(interface.as_ptr()));
            }
            drop(Box::from_raw(self.state.as_ptr()));
        }
    }
}
