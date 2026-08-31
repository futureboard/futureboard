//! Checked host-owned document graph and scoped editing orchestration.

mod content;
mod edit;
mod processing;
mod records;

use crate::extension::ExtensionState;
use crate::{DocumentController, ExtensionController, ExtensionRoles, HostServices, LoadedFactory};
use ara2_bridge_core::{
    ApiGeneration, AraBool, AraError, AudioModificationKind, AudioSourceKind, DocumentProperties,
    Handle, ModelRef, MusicalContextKind, PlaybackRegionKind, PlaybackTransformationFlags,
    RegionSequenceKind, Registry, RegistrySession, StoreFilter, StoreFilterBuilder,
};
use ara2_bridge_sys::{
    ARADocumentControllerRef, ARAPlaybackRegionRef, ARAPlugInExtensionInstance,
    ARARegionSequenceRef,
};
use records::{
    AudioModificationRecord, AudioSourceRecord, MusicalContextRecord, PlaybackRegionRecord,
    RegionSequenceRecord,
};
use std::collections::HashSet;
use std::fmt;
use std::rc::Weak;

pub use content::PluginContentReaderBackend;
pub use edit::EditSession;
pub use processing::StoredAudioFileChunk;

/// One operation that failed during best-effort explicit document teardown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseFailure {
    operation: &'static str,
    error: AraError,
}

impl CloseFailure {
    /// Returns the semantic teardown operation that failed.
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// Returns the underlying checked dispatch error.
    pub const fn error(&self) -> &AraError {
        &self.error
    }
}

/// Aggregate failures reported after explicit document teardown has attempted every safe step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseError {
    failures: Vec<CloseFailure>,
}

impl CloseError {
    /// Returns failures in the order their teardown operations were attempted.
    pub fn failures(&self) -> &[CloseFailure] {
        &self.failures
    }
}

impl fmt::Display for CloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} document teardown operation(s) failed",
            self.failures.len()
        )
    }
}

impl std::error::Error for CloseError {}

/// Stable typed identity of a host-owned musical context.
pub type MusicalContextHandle = Handle<MusicalContextKind>;
/// Stable typed identity of a host-owned region sequence.
pub type RegionSequenceHandle = Handle<RegionSequenceKind>;
/// Stable typed identity of a host-owned audio source.
pub type AudioSourceHandle = Handle<AudioSourceKind>;
/// Stable typed identity of a host-owned audio modification.
pub type AudioModificationHandle = Handle<AudioModificationKind>;
/// Stable typed identity of a host-owned playback region.
pub type PlaybackRegionHandle = Handle<PlaybackRegionKind>;

/// One host-owned document graph paired with a foreign plug-in controller.
pub struct DocumentSession<'factory, 'services> {
    pub(crate) controller: DocumentController<'factory, 'services>,
    pub(crate) services: &'services HostServices,
    pub(crate) compatible_archive_ids: HashSet<String>,
    pub(crate) analyzable_content_types: HashSet<i32>,
    pub(crate) supported_transformations: PlaybackTransformationFlags,
    pub(crate) properties: DocumentProperties,
    pub(crate) musical_contexts: Registry<MusicalContextKind, MusicalContextRecord>,
    pub(crate) region_sequences: Registry<RegionSequenceKind, RegionSequenceRecord>,
    pub(crate) audio_sources: Registry<AudioSourceKind, AudioSourceRecord>,
    pub(crate) audio_modifications: Registry<AudioModificationKind, AudioModificationRecord>,
    pub(crate) playback_regions: Registry<PlaybackRegionKind, PlaybackRegionRecord>,
    pub(crate) persistent_ids: HashSet<String>,
    pub(crate) editing: bool,
    pub(crate) poisoned: bool,
    pub(crate) extensions: Vec<Weak<ExtensionState>>,
}

impl<'factory, 'services> DocumentSession<'factory, 'services> {
    /// Creates a foreign controller and an empty host document graph.
    pub fn new(
        factory: &'factory LoadedFactory<'_>,
        services: &'services HostServices,
        properties: DocumentProperties,
    ) -> Result<Self, AraError> {
        let controller = factory.create_document_controller(services, &properties)?;
        let registry_session = RegistrySession::new();
        let mut compatible_archive_ids = factory
            .metadata()
            .compatible_archive_ids()
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        compatible_archive_ids.insert(factory.metadata().document_archive_id().to_owned());
        let analyzable_content_types = factory
            .metadata()
            .analyzable_content_types()
            .iter()
            .copied()
            .collect();
        let supported_transformations = PlaybackTransformationFlags::from_bits_retain(
            factory.metadata().playback_transformations() as u32,
        );
        Ok(Self {
            controller,
            services,
            compatible_archive_ids,
            analyzable_content_types,
            supported_transformations,
            properties,
            musical_contexts: Registry::in_session(
                registry_session,
                Registry::<MusicalContextKind, MusicalContextRecord>::DEFAULT_CAPACITY,
            ),
            region_sequences: Registry::in_session(
                registry_session,
                Registry::<RegionSequenceKind, RegionSequenceRecord>::DEFAULT_CAPACITY,
            ),
            audio_sources: Registry::in_session(
                registry_session,
                Registry::<AudioSourceKind, AudioSourceRecord>::DEFAULT_CAPACITY,
            ),
            audio_modifications: Registry::in_session(
                registry_session,
                Registry::<AudioModificationKind, AudioModificationRecord>::DEFAULT_CAPACITY,
            ),
            playback_regions: Registry::in_session(
                registry_session,
                Registry::<PlaybackRegionKind, PlaybackRegionRecord>::DEFAULT_CAPACITY,
            ),
            persistent_ids: HashSet::new(),
            editing: false,
            poisoned: false,
            extensions: Vec::new(),
        })
    }

    /// Begins one balanced document-editing scope.
    pub fn edit(&mut self) -> Result<EditSession<'_, 'factory, 'services>, AraError> {
        EditSession::begin(self)
    }

    /// Returns the foreign controller reference this document drives.
    ///
    /// A companion API binds a processor to a document controller before
    /// [`Self::bind_extension`] can validate the resulting extension instance, and
    /// that binding call needs this reference — `ARACLAP.h`, `ARAVST3.h`, and
    /// `ARAAudioUnit.h` all take it directly. The reference stays valid until this
    /// session is dropped or closed; it is opaque and must never be dereferenced.
    pub fn controller_ref(&self) -> ARADocumentControllerRef {
        self.controller.as_raw_ref()
    }

    /// Returns the ARA API generation negotiated with the plug-in.
    pub fn generation(&self) -> ApiGeneration {
        self.controller.generation()
    }

    /// Binds a companion-owned extension instance to this document's checked graph.
    ///
    /// # Safety
    ///
    /// The instance and every represented interface/reference pair must remain valid until the
    /// returned controller and all assignments from it are dropped. The companion API must have
    /// bound this exact instance to this document controller using the supplied role sets.
    pub unsafe fn bind_extension<'extension>(
        &mut self,
        instance: *const ARAPlugInExtensionInstance,
        known: ExtensionRoles,
        assigned: ExtensionRoles,
    ) -> Result<ExtensionController<'extension>, AraError> {
        // SAFETY: the caller forwards the extension backing and binding contract documented above.
        let extension = unsafe {
            ExtensionController::bind(
                instance,
                self.controller.generation(),
                known,
                assigned,
                self,
            )?
        };
        self.extensions.push(extension.weak_state());
        Ok(extension)
    }

    /// Explicitly destroys every live graph object leaf-first and then destroys the controller.
    ///
    /// Teardown continues after individual checked-dispatch failures. All failures are returned
    /// together after the controller has been released.
    pub fn close(mut self) -> Result<(), CloseError> {
        let mut failures = Vec::new();
        self.services.revoke_all_document_readers();
        for extension in self.extensions.drain(..) {
            if let Some(extension) = extension.upgrade() {
                if let Err(error) = extension.shutdown() {
                    failures.push(CloseFailure {
                        operation: "remove extension assignments",
                        error,
                    });
                }
            }
        }
        let playback_regions = self.playback_regions.handles().collect::<Vec<_>>();
        let audio_modifications = self.audio_modifications.handles().collect::<Vec<_>>();
        let audio_sources = self.audio_sources.handles().collect::<Vec<_>>();
        let region_sequences = self.region_sequences.handles().collect::<Vec<_>>();
        let musical_contexts = self.musical_contexts.handles().collect::<Vec<_>>();
        match self.edit() {
            Ok(mut edit) => {
                for handle in playback_regions {
                    if let Err(error) = edit.destroy_playback_region(handle) {
                        failures.push(CloseFailure {
                            operation: "destroy playback region",
                            error,
                        });
                        edit.discard_playback_region(handle);
                    }
                }
                for handle in audio_modifications {
                    if let Err(error) = edit.destroy_audio_modification(handle) {
                        failures.push(CloseFailure {
                            operation: "destroy audio modification",
                            error,
                        });
                        edit.discard_audio_modification(handle);
                    }
                }
                for handle in audio_sources {
                    if let Err(error) = edit.destroy_audio_source(handle) {
                        failures.push(CloseFailure {
                            operation: "destroy audio source",
                            error,
                        });
                        edit.discard_audio_source(handle);
                    }
                }
                for handle in region_sequences {
                    if let Err(error) = edit.destroy_region_sequence(handle) {
                        failures.push(CloseFailure {
                            operation: "destroy region sequence",
                            error,
                        });
                        edit.discard_region_sequence(handle);
                    }
                }
                for handle in musical_contexts {
                    if let Err(error) = edit.destroy_musical_context(handle) {
                        failures.push(CloseFailure {
                            operation: "destroy musical context",
                            error,
                        });
                        edit.discard_musical_context(handle);
                    }
                }
                if let Err(error) = edit.finish() {
                    failures.push(CloseFailure {
                        operation: "end editing",
                        error,
                    });
                }
            }
            Err(error) => failures.push(CloseFailure {
                operation: "begin editing",
                error,
            }),
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(CloseError { failures })
        }
    }

    /// Begins the dedicated full-document restoration scope used before ARA 2 Final.
    pub fn restore_document_from_archive<'session, T>(
        &'session mut self,
        reader: &'session T,
    ) -> Result<EditSession<'session, 'factory, 'services>, AraError> {
        EditSession::begin_legacy_restore(self, reader)
    }

    /// Starts a checked partial-store filter for this document graph.
    pub fn store_filter_builder(&self) -> StoreFilterBuilder {
        StoreFilter::builder(self.audio_sources.session_id())
    }

    /// Stores the complete document through the legacy persistence callback.
    pub fn store_document_to_archive<T>(&mut self, writer: &T) -> Result<(), AraError> {
        if self.editing {
            return Err(AraError::InvalidState(
                "cannot store the document while editing",
            ));
        }
        if self.poisoned {
            return Err(AraError::InvalidState("document session is poisoned"));
        }
        let writer = std::ptr::from_ref(writer).cast_mut().cast();
        // SAFETY: the writer identity remains live through this synchronous call.
        let accepted = unsafe { self.controller.raw_store_document_to_archive(writer)? };
        if AraBool::from_raw(accepted).get() {
            Ok(())
        } else {
            Err(AraError::Peer("plug-in rejected document storage"))
        }
    }

    /// Stores selected ARA 2 object state while no editing scope is active.
    pub fn store_objects_to_archive<T>(
        &mut self,
        writer: &T,
        filter: Option<&StoreFilter>,
    ) -> Result<(), AraError> {
        if self.controller.generation() < ara2_bridge_core::ApiGeneration::V2Final {
            return Err(AraError::Unsupported(
                "partial object storage before ARA 2 Final",
            ));
        }
        if self.editing {
            return Err(AraError::InvalidState("cannot store objects while editing"));
        }
        if self.poisoned {
            return Err(AraError::InvalidState("document session is poisoned"));
        }
        if filter.is_some_and(|filter| filter.session() != self.audio_sources.session_id()) {
            return Err(AraError::InvalidArgument(
                "store filter belongs to another document",
            ));
        }
        let ffi_filter = filter
            .map(|filter| {
                filter.as_ffi(
                    |handle| {
                        self.audio_sources
                            .get(handle)?
                            .peer
                            .map(|peer| peer.as_ptr())
                            .ok_or(AraError::InvalidState("audio source is provisional"))
                    },
                    |handle| {
                        self.audio_modifications
                            .get(handle)?
                            .peer
                            .map(|peer| peer.as_ptr())
                            .ok_or(AraError::InvalidState("audio modification is provisional"))
                    },
                )
            })
            .transpose()?;
        let filter = ffi_filter
            .as_ref()
            .map_or(std::ptr::null(), |filter| filter.as_ref().as_ptr());
        let writer = std::ptr::from_ref(writer).cast_mut().cast();
        // SAFETY: writer identity and optional pinned filter remain valid through the call.
        let accepted = unsafe {
            self.controller
                .raw_store_objects_to_archive(writer, filter)?
        };
        if AraBool::from_raw(accepted).get() {
            Ok(())
        } else {
            Err(AraError::Peer("plug-in rejected object storage"))
        }
    }

    /// Returns a checked stable host reference for property construction.
    pub fn musical_context_ref(
        &self,
        handle: MusicalContextHandle,
    ) -> Result<ModelRef<MusicalContextKind>, AraError> {
        self.musical_contexts.model_ref(handle)
    }

    /// Returns a checked stable host region-sequence reference for property construction.
    pub fn region_sequence_ref(
        &self,
        handle: RegionSequenceHandle,
    ) -> Result<ModelRef<RegionSequenceKind>, AraError> {
        self.region_sequences.model_ref(handle)
    }

    /// Returns a checked stable host audio-source reference.
    pub fn audio_source_ref(
        &self,
        handle: AudioSourceHandle,
    ) -> Result<ModelRef<AudioSourceKind>, AraError> {
        self.audio_sources.model_ref(handle)
    }

    /// Returns a checked stable host audio-modification reference.
    pub fn audio_modification_ref(
        &self,
        handle: AudioModificationHandle,
    ) -> Result<ModelRef<AudioModificationKind>, AraError> {
        self.audio_modifications.model_ref(handle)
    }

    /// Returns a checked stable host playback-region reference.
    pub fn playback_region_ref(
        &self,
        handle: PlaybackRegionHandle,
    ) -> Result<ModelRef<PlaybackRegionKind>, AraError> {
        self.playback_regions.model_ref(handle)
    }

    pub(crate) fn extension_session_id(&self) -> RegistrySession {
        self.playback_regions.session_id()
    }

    pub(crate) fn extension_playback_region_peer(
        &self,
        handle: PlaybackRegionHandle,
    ) -> Result<ARAPlaybackRegionRef, AraError> {
        Ok(self
            .playback_regions
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("playback region is provisional"))?
            .as_ptr())
    }

    pub(crate) fn extension_region_sequence_peer(
        &self,
        handle: RegionSequenceHandle,
    ) -> Result<ARARegionSequenceRef, AraError> {
        Ok(self
            .region_sequences
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("region sequence is provisional"))?
            .as_ptr())
    }

    /// Enables or synchronously disables plug-in sample access outside or inside editing.
    pub fn set_audio_source_samples_access(
        &mut self,
        handle: AudioSourceHandle,
        enable: bool,
    ) -> Result<(), AraError> {
        if self.poisoned {
            return Err(AraError::InvalidState("document session is poisoned"));
        }
        let record = self.audio_sources.get(handle)?;
        if !record.active {
            return Err(AraError::InvalidState("audio source is deactivated"));
        }
        let peer = record
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        // SAFETY: the source record owns this live peer for the controller lifetime.
        unsafe {
            self.controller
                .raw_enable_audio_source_samples_access(peer, AraBool::from(enable).into_raw())?
        };
        self.audio_sources.get_mut(handle)?.samples_access_enabled = enable;
        Ok(())
    }

    /// Returns whether an assertion or impossible foreign result quarantined this session.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Returns the current document properties.
    pub fn properties(&self) -> &DocumentProperties {
        &self.properties
    }
}
