//! Balanced host editing guard and provisional graph mutations.

use super::{
    records::{
        AudioModificationRecord, AudioSourceRecord, MusicalContextRecord, PlaybackRegionRecord,
        RegionSequenceRecord,
    },
    AudioModificationHandle, AudioSourceHandle, DocumentSession, MusicalContextHandle,
    PlaybackRegionHandle, RegionSequenceHandle,
};
use ara2_bridge_core::{
    ApiGeneration, AraBool, AraError, AudioModificationKind, AudioModificationProperties,
    AudioSourceKind, AudioSourceProperties, ContentTimeRange, ContentUpdateScopes,
    DocumentProperties, ModelRef, MusicalContextKind, MusicalContextProperties, PlaybackRegionKind,
    PlaybackRegionProperties, RegionSequenceKind, RegionSequenceProperties, RestoreFilter,
};
use ara2_bridge_sys::{
    ARAArchiveReaderHostRef, ARAAudioModificationHostRef, ARAAudioModificationRef,
    ARAAudioSourceHostRef, ARAAudioSourceRef, ARAMusicalContextRef, ARAPlaybackRegionHostRef,
    ARAPlaybackRegionRef, ARARegionSequenceHostRef, ARARegionSequenceRef,
};
use std::ptr::NonNull;

enum EndMode {
    Editing,
    LegacyRestore(ARAArchiveReaderHostRef),
}

/// One balanced ARA document editing scope.
pub struct EditSession<'session, 'factory, 'services> {
    session: &'session mut DocumentSession<'factory, 'services>,
    end_mode: EndMode,
    finished: bool,
}

impl<'session, 'factory, 'services> EditSession<'session, 'factory, 'services> {
    pub(crate) fn begin(
        session: &'session mut DocumentSession<'factory, 'services>,
    ) -> Result<Self, AraError> {
        if session.editing {
            return Err(AraError::InvalidState("document is already being edited"));
        }
        if session.poisoned {
            return Err(AraError::InvalidState("document session is poisoned"));
        }
        // SAFETY: the session owns a live controller and serializes edit scopes through `&mut`.
        unsafe { session.controller.raw_begin_editing()? };
        session.editing = true;
        Ok(Self {
            session,
            end_mode: EndMode::Editing,
            finished: false,
        })
    }

    /// Selects one advertised processing algorithm for a live audio source.
    pub fn request_processing_algorithm(
        &mut self,
        source: AudioSourceHandle,
        algorithm_index: usize,
    ) -> Result<(), AraError> {
        let count = self.session.controller.processing_algorithms_count()?;
        if algorithm_index >= count {
            return Err(AraError::InvalidArgument(
                "processing algorithm index is out of bounds",
            ));
        }
        let algorithm_index = i32::try_from(algorithm_index)
            .map_err(|_| AraError::InvalidArgument("processing algorithm index is too large"))?;
        let peer = self
            .session
            .audio_sources
            .get(source)?
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        // SAFETY: this editing guard serializes a request for a live source owned by the controller.
        unsafe {
            self.session
                .controller
                .raw_request_processing_algorithm_for_audio_source(peer, algorithm_index)
        }
    }

    pub(crate) fn begin_legacy_restore<T>(
        session: &'session mut DocumentSession<'factory, 'services>,
        reader: &'session T,
    ) -> Result<Self, AraError> {
        if session.controller.generation() >= ApiGeneration::V2Final {
            return Err(AraError::Unsupported(
                "legacy restoration on ARA 2 Final or newer",
            ));
        }
        if session.editing {
            return Err(AraError::InvalidState("document is already being edited"));
        }
        if session.poisoned {
            return Err(AraError::InvalidState("document session is poisoned"));
        }
        let reader = std::ptr::from_ref(reader).cast_mut().cast();
        // SAFETY: the returned guard retains both the session and reader borrows through end.
        let accepted = unsafe {
            session
                .controller
                .raw_begin_restoring_document_from_archive(reader)?
        };
        if !AraBool::from_raw(accepted).get() {
            return Err(AraError::Peer("plug-in rejected document restoration"));
        }
        session.editing = true;
        Ok(Self {
            session,
            end_mode: EndMode::LegacyRestore(reader),
            finished: false,
        })
    }

    fn end_scope(&mut self) -> Result<(), AraError> {
        match self.end_mode {
            EndMode::Editing => {
                // SAFETY: this guard issued exactly one matching begin-editing call.
                unsafe { self.session.controller.raw_end_editing() }
            }
            EndMode::LegacyRestore(reader) => {
                // SAFETY: this guard issued a successful legacy begin with this same reader.
                let accepted = unsafe {
                    self.session
                        .controller
                        .raw_end_restoring_document_from_archive(reader)?
                };
                if AraBool::from_raw(accepted).get() {
                    Ok(())
                } else {
                    Err(AraError::Peer(
                        "plug-in rejected the end of document restoration",
                    ))
                }
            }
        }
    }

    /// Updates the document properties retained by both peers.
    pub fn update_document_properties(
        &mut self,
        properties: DocumentProperties,
    ) -> Result<(), AraError> {
        let ffi_properties = properties.as_ffi();
        // SAFETY: the owned property backing remains live through the synchronous call.
        unsafe {
            self.session
                .controller
                .raw_update_document_properties(ffi_properties.as_ref().as_ptr())?
        };
        self.session.properties = properties;
        Ok(())
    }

    /// Restores matching ARA 2 object state from one archive within this edit scope.
    pub fn restore_objects_from_archive<T>(
        &mut self,
        reader: &T,
        filter: Option<&RestoreFilter>,
    ) -> Result<(), AraError> {
        if self.session.controller.generation() < ApiGeneration::V2Final {
            return Err(AraError::Unsupported(
                "partial object restoration before ARA 2 Final",
            ));
        }
        let archive_id = self
            .session
            .services
            .document_archive_id(reader)?
            .filter(|archive_id| !archive_id.is_empty())
            .ok_or(AraError::InvalidArgument(
                "ARA 2 archive reader has no document archive ID",
            ))?;
        if !self.session.compatible_archive_ids.contains(&archive_id) {
            return Err(AraError::InvalidArgument(
                "archive document ID is incompatible with this plug-in",
            ));
        }
        if let Some(filter) = filter {
            for mapping in filter.audio_sources() {
                if !self
                    .session
                    .audio_sources
                    .values()
                    .any(|source| source.properties.persistent_id() == mapping.current_id())
                {
                    return Err(AraError::InvalidArgument(
                        "restore filter references an unknown audio-source ID",
                    ));
                }
            }
            for mapping in filter.audio_modifications() {
                if !self
                    .session
                    .audio_modifications
                    .values()
                    .any(|modification| {
                        modification.properties.persistent_id() == mapping.current_id()
                    })
                {
                    return Err(AraError::InvalidArgument(
                        "restore filter references an unknown audio-modification ID",
                    ));
                }
            }
        }
        let reader = std::ptr::from_ref(reader).cast_mut().cast();
        let ffi_filter = filter.map(RestoreFilter::as_ffi);
        let filter = ffi_filter
            .as_ref()
            .map_or(std::ptr::null(), |filter| filter.as_ref().as_ptr());
        // SAFETY: the reader identity and optional pinned filter remain valid through this call.
        let accepted = unsafe {
            self.session
                .controller
                .raw_restore_objects_from_archive(reader, filter)?
        };
        if AraBool::from_raw(accepted).get() {
            Ok(())
        } else {
            Err(AraError::Peer("plug-in rejected object restoration"))
        }
    }

    /// Provisionally registers and creates one musical context.
    pub fn create_musical_context(
        &mut self,
        properties: MusicalContextProperties,
    ) -> Result<MusicalContextHandle, AraError> {
        let handle = self.session.musical_contexts.insert(MusicalContextRecord {
            properties,
            peer: None,
        })?;
        let host_ref = self
            .session
            .musical_contexts
            .opaque_pointer(handle)?
            .as_ptr()
            .cast();
        let ffi_properties = self
            .session
            .musical_contexts
            .get(handle)?
            .properties
            .as_ffi(self.session.controller.generation())?;
        self.session.services.begin_provisional(host_ref);
        // SAFETY: the provisional registry cell and copied property backing remain live through
        // the synchronous call; the generated shell supplies the owned controller reference.
        let result = unsafe {
            self.session
                .controller
                .raw_create_musical_context(host_ref, ffi_properties.as_ref().as_ptr())
        };
        let provisional_escaped = self.session.services.finish_provisional(host_ref);
        drop(ffi_properties);
        let peer = match result {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.session.musical_contexts.remove(handle);
                self.session.poisoned |= provisional_escaped;
                return Err(error);
            }
        };
        let Some(peer) = NonNull::new(peer) else {
            let _ = self.session.musical_contexts.remove(handle);
            self.session.poisoned |= provisional_escaped;
            return Err(AraError::Peer("plug-in rejected musical context"));
        };
        self.session.musical_contexts.get_mut(handle)?.peer = Some(peer);
        Ok(handle)
    }

    /// Returns a checked musical-context reference while this edit holds the session borrow.
    pub fn musical_context_ref(
        &self,
        handle: MusicalContextHandle,
    ) -> Result<ModelRef<MusicalContextKind>, AraError> {
        self.session.musical_contexts.model_ref(handle)
    }

    /// Returns a checked region-sequence reference while this edit holds the session borrow.
    pub fn region_sequence_ref(
        &self,
        handle: RegionSequenceHandle,
    ) -> Result<ModelRef<RegionSequenceKind>, AraError> {
        self.session.region_sequences.model_ref(handle)
    }

    /// Returns a checked audio-source reference while this edit holds the session borrow.
    ///
    /// A plug-in may call back into the host — `createAudioReaderForSource` in
    /// particular — synchronously from inside `createAudioSource`, before the
    /// edit is finished. A host that can only resolve object identities after
    /// `endEditing()` therefore rejects the plug-in's very first request, so
    /// these accessors exist to let it register identities as it creates them.
    pub fn audio_source_ref(
        &self,
        handle: AudioSourceHandle,
    ) -> Result<ModelRef<AudioSourceKind>, AraError> {
        self.session.audio_sources.model_ref(handle)
    }

    /// Returns a checked audio-modification reference while this edit holds the session borrow.
    pub fn audio_modification_ref(
        &self,
        handle: AudioModificationHandle,
    ) -> Result<ModelRef<AudioModificationKind>, AraError> {
        self.session.audio_modifications.model_ref(handle)
    }

    /// Returns a checked playback-region reference while this edit holds the session borrow.
    pub fn playback_region_ref(
        &self,
        handle: PlaybackRegionHandle,
    ) -> Result<ModelRef<PlaybackRegionKind>, AraError> {
        self.session.playback_regions.model_ref(handle)
    }

    /// Provisionally registers and creates an ARA 2 region sequence.
    pub fn create_region_sequence(
        &mut self,
        properties: RegionSequenceProperties,
    ) -> Result<RegionSequenceHandle, AraError> {
        if self.session.controller.generation() < ApiGeneration::V2Draft {
            return Err(AraError::Unsupported("ARA 1 region sequences"));
        }
        let context = self
            .session
            .musical_contexts
            .handle_from_opaque(properties.musical_context().as_raw())?;
        let context_peer = self
            .session
            .musical_contexts
            .get(context)?
            .peer
            .ok_or(AraError::InvalidState("musical context is provisional"))?;
        // SAFETY: the plug-in owns this typed peer for the live controller and context record.
        let context_peer =
            unsafe { ModelRef::<MusicalContextKind>::from_raw(context_peer.as_ptr().cast())? };
        let peer_properties = RegionSequenceProperties::new(
            properties.name(),
            properties.order_index(),
            context_peer,
            properties.color().cloned(),
        )?;
        let handle = self.session.region_sequences.insert(RegionSequenceRecord {
            properties,
            peer: None,
            context,
        })?;
        let host_ref: ARARegionSequenceHostRef = self
            .session
            .region_sequences
            .opaque_pointer(handle)?
            .as_ptr()
            .cast();
        let ffi_properties = peer_properties.as_ffi(self.session.controller.generation())?;
        self.session.services.begin_provisional(host_ref);
        // SAFETY: the provisional cell and property backing remain live through the call.
        let result = unsafe {
            self.session
                .controller
                .raw_create_region_sequence(host_ref, ffi_properties.as_ref().as_ptr())
        };
        let provisional_escaped = self.session.services.finish_provisional(host_ref);
        drop(ffi_properties);
        let peer = match result {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.session.region_sequences.remove(handle);
                self.session.poisoned |= provisional_escaped;
                return Err(error);
            }
        };
        let Some(peer) = NonNull::new(peer) else {
            let _ = self.session.region_sequences.remove(handle);
            self.session.poisoned |= provisional_escaped;
            return Err(AraError::Peer("plug-in rejected region sequence"));
        };
        self.session.region_sequences.get_mut(handle)?.peer = Some(peer);
        Ok(handle)
    }

    /// Provisionally registers and creates one audio source with a document-unique ID.
    pub fn create_audio_source(
        &mut self,
        properties: AudioSourceProperties,
    ) -> Result<AudioSourceHandle, AraError> {
        let persistent_id = properties.persistent_id().to_owned();
        if !self.session.persistent_ids.insert(persistent_id.clone()) {
            return Err(AraError::InvalidArgument("duplicate persistent ID"));
        }
        let handle = match self.session.audio_sources.insert(AudioSourceRecord {
            properties,
            peer: None,
            active: true,
            samples_access_enabled: false,
        }) {
            Ok(handle) => handle,
            Err(error) => {
                self.session.persistent_ids.remove(&persistent_id);
                return Err(error);
            }
        };
        let host_ref: ARAAudioSourceHostRef = self
            .session
            .audio_sources
            .opaque_pointer(handle)?
            .as_ptr()
            .cast();
        let ffi_properties = self
            .session
            .audio_sources
            .get(handle)?
            .properties
            .as_ffi(self.session.controller.generation())?;
        self.session.services.begin_provisional(host_ref);
        // SAFETY: the provisional cell and property backing remain live through the call.
        let result = unsafe {
            self.session
                .controller
                .raw_create_audio_source(host_ref, ffi_properties.as_ref().as_ptr())
        };
        let provisional_escaped = self.session.services.finish_provisional(host_ref);
        drop(ffi_properties);
        let peer = match result {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.session.audio_sources.remove(handle);
                self.session.persistent_ids.remove(&persistent_id);
                if provisional_escaped {
                    self.session.poisoned = true;
                }
                return Err(error);
            }
        };
        let Some(peer) = NonNull::new(peer) else {
            let _ = self.session.audio_sources.remove(handle);
            self.session.persistent_ids.remove(&persistent_id);
            if provisional_escaped {
                self.session.poisoned = true;
            }
            return Err(AraError::Peer("plug-in rejected audio source"));
        };
        self.session.audio_sources.get_mut(handle)?.peer = Some(peer);
        Ok(handle)
    }

    /// Provisionally registers and creates one audio modification for a live source.
    pub fn create_audio_modification(
        &mut self,
        source: AudioSourceHandle,
        properties: AudioModificationProperties,
    ) -> Result<AudioModificationHandle, AraError> {
        let source_peer: ARAAudioSourceRef = self
            .session
            .audio_sources
            .get(source)?
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        let persistent_id = properties.persistent_id().to_owned();
        if !self.session.persistent_ids.insert(persistent_id.clone()) {
            return Err(AraError::InvalidArgument("duplicate persistent ID"));
        }
        let handle = match self
            .session
            .audio_modifications
            .insert(AudioModificationRecord {
                properties,
                peer: None,
                source,
                active: true,
            }) {
            Ok(handle) => handle,
            Err(error) => {
                self.session.persistent_ids.remove(&persistent_id);
                return Err(error);
            }
        };
        let host_ref: ARAAudioModificationHostRef = self
            .session
            .audio_modifications
            .opaque_pointer(handle)?
            .as_ptr()
            .cast();
        let ffi_properties = self
            .session
            .audio_modifications
            .get(handle)?
            .properties
            .as_ffi();
        self.session.services.begin_provisional(host_ref);
        // SAFETY: all referenced records and call backing remain live through the call.
        let result = unsafe {
            self.session.controller.raw_create_audio_modification(
                source_peer,
                host_ref,
                ffi_properties.as_ref().as_ptr(),
            )
        };
        let provisional_escaped = self.session.services.finish_provisional(host_ref);
        drop(ffi_properties);
        let peer = match result {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.session.audio_modifications.remove(handle);
                self.session.persistent_ids.remove(&persistent_id);
                self.session.poisoned |= provisional_escaped;
                return Err(error);
            }
        };
        let Some(peer) = NonNull::new(peer) else {
            let _ = self.session.audio_modifications.remove(handle);
            self.session.persistent_ids.remove(&persistent_id);
            self.session.poisoned |= provisional_escaped;
            return Err(AraError::Peer("plug-in rejected audio modification"));
        };
        self.session.audio_modifications.get_mut(handle)?.peer = Some(peer);
        Ok(handle)
    }

    /// Clones one live audio modification into a distinct host identity and persistent ID.
    pub fn clone_audio_modification(
        &mut self,
        original: AudioModificationHandle,
        properties: AudioModificationProperties,
    ) -> Result<AudioModificationHandle, AraError> {
        let original_record = self.session.audio_modifications.get(original)?;
        if !original_record.active {
            return Err(AraError::InvalidState("audio modification is deactivated"));
        }
        let source = original_record.source;
        if !self.session.audio_sources.get(source)?.active {
            return Err(AraError::InvalidState("audio source is deactivated"));
        }
        let original_peer: ARAAudioModificationRef = original_record
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr();
        let persistent_id = properties.persistent_id().to_owned();
        if !self.session.persistent_ids.insert(persistent_id.clone()) {
            return Err(AraError::InvalidArgument("duplicate persistent ID"));
        }
        let handle = match self
            .session
            .audio_modifications
            .insert(AudioModificationRecord {
                properties,
                peer: None,
                source,
                active: true,
            }) {
            Ok(handle) => handle,
            Err(error) => {
                self.session.persistent_ids.remove(&persistent_id);
                return Err(error);
            }
        };
        let host_ref: ARAAudioModificationHostRef = self
            .session
            .audio_modifications
            .opaque_pointer(handle)?
            .as_ptr()
            .cast();
        let ffi_properties = self
            .session
            .audio_modifications
            .get(handle)?
            .properties
            .as_ffi();
        self.session.services.begin_provisional(host_ref);
        // SAFETY: both records and the copied property backing remain live through the call.
        let result = unsafe {
            self.session.controller.raw_clone_audio_modification(
                original_peer,
                host_ref,
                ffi_properties.as_ref().as_ptr(),
            )
        };
        let provisional_escaped = self.session.services.finish_provisional(host_ref);
        drop(ffi_properties);
        let peer = match result {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.session.audio_modifications.remove(handle);
                self.session.persistent_ids.remove(&persistent_id);
                self.session.poisoned |= provisional_escaped;
                return Err(error);
            }
        };
        let Some(peer) = NonNull::new(peer) else {
            let _ = self.session.audio_modifications.remove(handle);
            self.session.persistent_ids.remove(&persistent_id);
            self.session.poisoned |= provisional_escaped;
            return Err(AraError::Peer("plug-in rejected cloned audio modification"));
        };
        self.session.audio_modifications.get_mut(handle)?.peer = Some(peer);
        Ok(handle)
    }

    /// Provisionally registers and creates one playback region with checked graph edges.
    pub fn create_playback_region(
        &mut self,
        modification: AudioModificationHandle,
        properties: PlaybackRegionProperties,
    ) -> Result<PlaybackRegionHandle, AraError> {
        let modification_peer: ARAAudioModificationRef = self
            .session
            .audio_modifications
            .get(modification)?
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr();
        let (sequence, context, peer_properties) = if self.session.controller.generation()
            >= ApiGeneration::V2Draft
        {
            let reference = properties
                .region_sequence()
                .ok_or(AraError::InvalidArgument(
                    "ARA 2 playback region requires a region sequence",
                ))?;
            let sequence = self
                .session
                .region_sequences
                .handle_from_opaque(reference.as_raw())?;
            let sequence_peer = self
                .session
                .region_sequences
                .get(sequence)?
                .peer
                .ok_or(AraError::InvalidState("region sequence is provisional"))?;
            // SAFETY: the plug-in owns this typed peer for the live controller and sequence.
            let sequence_peer =
                unsafe { ModelRef::<RegionSequenceKind>::from_raw(sequence_peer.as_ptr().cast())? };
            (
                Some(sequence),
                None,
                properties.clone().with_region_sequence(sequence_peer)?,
            )
        } else {
            let reference = properties
                .musical_context()
                .ok_or(AraError::InvalidArgument(
                    "ARA 1 playback region requires a musical context",
                ))?;
            let context = self
                .session
                .musical_contexts
                .handle_from_opaque(reference.as_raw())?;
            let context_peer = self
                .session
                .musical_contexts
                .get(context)?
                .peer
                .ok_or(AraError::InvalidState("musical context is provisional"))?;
            // SAFETY: the plug-in owns this typed peer for the live controller and context.
            let context_peer =
                unsafe { ModelRef::<MusicalContextKind>::from_raw(context_peer.as_ptr().cast())? };
            (
                None,
                Some(context),
                properties.clone().with_musical_context(context_peer)?,
            )
        };
        let handle = self.session.playback_regions.insert(PlaybackRegionRecord {
            properties,
            peer: None,
            modification,
            sequence,
            context,
        })?;
        let host_ref: ARAPlaybackRegionHostRef = self
            .session
            .playback_regions
            .opaque_pointer(handle)?
            .as_ptr()
            .cast();
        let ffi_properties = peer_properties.as_ffi(self.session.controller.generation())?;
        self.session.services.begin_provisional(host_ref);
        // SAFETY: all graph records and property backing remain live through the call.
        let result = unsafe {
            self.session.controller.raw_create_playback_region(
                modification_peer,
                host_ref,
                ffi_properties.as_ref().as_ptr(),
            )
        };
        let provisional_escaped = self.session.services.finish_provisional(host_ref);
        drop(ffi_properties);
        let peer = match result {
            Ok(peer) => peer,
            Err(error) => {
                let _ = self.session.playback_regions.remove(handle);
                self.session.poisoned |= provisional_escaped;
                return Err(error);
            }
        };
        let Some(peer) = NonNull::new(peer) else {
            let _ = self.session.playback_regions.remove(handle);
            self.session.poisoned |= provisional_escaped;
            return Err(AraError::Peer("plug-in rejected playback region"));
        };
        self.session.playback_regions.get_mut(handle)?.peer = Some(peer);
        Ok(handle)
    }

    /// Updates a live musical context.
    pub fn update_musical_context(
        &mut self,
        handle: MusicalContextHandle,
        properties: MusicalContextProperties,
    ) -> Result<(), AraError> {
        let peer: ARAMusicalContextRef = self
            .session
            .musical_contexts
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("musical context is provisional"))?
            .as_ptr();
        let ffi_properties = properties.as_ffi(self.session.controller.generation())?;
        // SAFETY: the record and owned property backing remain live through the call.
        unsafe {
            self.session
                .controller
                .raw_update_musical_context_properties(peer, ffi_properties.as_ref().as_ptr())?
        };
        self.session.musical_contexts.get_mut(handle)?.properties = properties;
        Ok(())
    }

    /// Notifies the plug-in that host musical-context content changed.
    pub fn update_musical_context_content(
        &mut self,
        handle: MusicalContextHandle,
        range: Option<ContentTimeRange>,
        flags: ContentUpdateScopes,
    ) -> Result<(), AraError> {
        let peer: ARAMusicalContextRef = self
            .session
            .musical_contexts
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("musical context is provisional"))?
            .as_ptr();
        let range = range
            .as_ref()
            .map_or(std::ptr::null(), ContentTimeRange::as_ptr);
        // SAFETY: the context peer and optional owned range remain live through the call.
        unsafe {
            self.session.controller.raw_update_musical_context_content(
                peer,
                range,
                flags.bits(),
            )?;
        }
        Ok(())
    }

    /// Updates a region sequence and its checked musical-context edge.
    pub fn update_region_sequence(
        &mut self,
        handle: RegionSequenceHandle,
        properties: RegionSequenceProperties,
    ) -> Result<(), AraError> {
        if self.session.controller.generation() < ApiGeneration::V2Draft {
            return Err(AraError::Unsupported("ARA 1 region sequences"));
        }
        let context = self
            .session
            .musical_contexts
            .handle_from_opaque(properties.musical_context().as_raw())?;
        let context_peer = self
            .session
            .musical_contexts
            .get(context)?
            .peer
            .ok_or(AraError::InvalidState("musical context is provisional"))?;
        // SAFETY: the plug-in owns this typed peer for the live controller and context record.
        let context_peer =
            unsafe { ModelRef::<MusicalContextKind>::from_raw(context_peer.as_ptr().cast())? };
        let peer_properties = RegionSequenceProperties::new(
            properties.name(),
            properties.order_index(),
            context_peer,
            properties.color().cloned(),
        )?;
        let peer: ARARegionSequenceRef = self
            .session
            .region_sequences
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("region sequence is provisional"))?
            .as_ptr();
        let ffi_properties = peer_properties.as_ffi(self.session.controller.generation())?;
        // SAFETY: all checked graph records and property backing remain live through the call.
        unsafe {
            self.session
                .controller
                .raw_update_region_sequence_properties(peer, ffi_properties.as_ref().as_ptr())?
        };
        let record = self.session.region_sequences.get_mut(handle)?;
        record.properties = properties;
        record.context = context;
        Ok(())
    }

    /// Updates a live audio source, preserving document-wide persistent-ID uniqueness.
    pub fn update_audio_source(
        &mut self,
        handle: AudioSourceHandle,
        properties: AudioSourceProperties,
    ) -> Result<(), AraError> {
        let record = self.session.audio_sources.get(handle)?;
        if !record.active {
            return Err(AraError::InvalidState("audio source is deactivated"));
        }
        let peer: ARAAudioSourceRef = record
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        let old_id = record.properties.persistent_id().to_owned();
        let new_id = properties.persistent_id().to_owned();
        if old_id != new_id && self.session.persistent_ids.contains(&new_id) {
            return Err(AraError::InvalidArgument("duplicate persistent ID"));
        }
        let ffi_properties = properties.as_ffi(self.session.controller.generation())?;
        // SAFETY: the record and complete owned properties remain live through the call.
        unsafe {
            self.session
                .controller
                .raw_update_audio_source_properties(peer, ffi_properties.as_ref().as_ptr())?
        };
        if old_id != new_id {
            self.session.persistent_ids.remove(&old_id);
            self.session.persistent_ids.insert(new_id);
        }
        self.session.audio_sources.get_mut(handle)?.properties = properties;
        Ok(())
    }

    /// Notifies the plug-in that host audio-source samples or content changed.
    pub fn update_audio_source_content(
        &mut self,
        handle: AudioSourceHandle,
        range: Option<ContentTimeRange>,
        flags: ContentUpdateScopes,
    ) -> Result<(), AraError> {
        let record = self.session.audio_sources.get(handle)?;
        if !record.active {
            return Err(AraError::InvalidState("audio source is deactivated"));
        }
        let peer: ARAAudioSourceRef = record
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        let range = range
            .as_ref()
            .map_or(std::ptr::null(), ContentTimeRange::as_ptr);
        // SAFETY: the source peer and optional owned range remain live through the call.
        unsafe {
            self.session
                .controller
                .raw_update_audio_source_content(peer, range, flags.bits())?;
        }
        Ok(())
    }

    /// Updates a live audio modification, preserving document-wide persistent-ID uniqueness.
    pub fn update_audio_modification(
        &mut self,
        handle: AudioModificationHandle,
        properties: AudioModificationProperties,
    ) -> Result<(), AraError> {
        let record = self.session.audio_modifications.get(handle)?;
        if !record.active {
            return Err(AraError::InvalidState("audio modification is deactivated"));
        }
        let peer: ARAAudioModificationRef = record
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr();
        let old_id = record.properties.persistent_id().to_owned();
        let new_id = properties.persistent_id().to_owned();
        if old_id != new_id && self.session.persistent_ids.contains(&new_id) {
            return Err(AraError::InvalidArgument("duplicate persistent ID"));
        }
        let ffi_properties = properties.as_ffi();
        // SAFETY: the record and complete owned properties remain live through the call.
        unsafe {
            self.session
                .controller
                .raw_update_audio_modification_properties(peer, ffi_properties.as_ref().as_ptr())?
        };
        if old_id != new_id {
            self.session.persistent_ids.remove(&old_id);
            self.session.persistent_ids.insert(new_id);
        }
        self.session.audio_modifications.get_mut(handle)?.properties = properties;
        Ok(())
    }

    /// Updates a playback region and its checked generation-specific graph edge.
    pub fn update_playback_region(
        &mut self,
        handle: PlaybackRegionHandle,
        properties: PlaybackRegionProperties,
    ) -> Result<(), AraError> {
        let generation = self.session.controller.generation();
        let (sequence, context, peer_properties) = if generation >= ApiGeneration::V2Draft {
            let reference = properties
                .region_sequence()
                .ok_or(AraError::InvalidArgument(
                    "ARA 2 playback region requires a region sequence",
                ))?;
            let sequence = self
                .session
                .region_sequences
                .handle_from_opaque(reference.as_raw())?;
            let sequence_peer = self
                .session
                .region_sequences
                .get(sequence)?
                .peer
                .ok_or(AraError::InvalidState("region sequence is provisional"))?;
            // SAFETY: the plug-in owns this typed peer for the live controller and sequence.
            let sequence_peer =
                unsafe { ModelRef::<RegionSequenceKind>::from_raw(sequence_peer.as_ptr().cast())? };
            (
                Some(sequence),
                None,
                properties.clone().with_region_sequence(sequence_peer)?,
            )
        } else {
            let reference = properties
                .musical_context()
                .ok_or(AraError::InvalidArgument(
                    "ARA 1 playback region requires a musical context",
                ))?;
            let context = self
                .session
                .musical_contexts
                .handle_from_opaque(reference.as_raw())?;
            let context_peer = self
                .session
                .musical_contexts
                .get(context)?
                .peer
                .ok_or(AraError::InvalidState("musical context is provisional"))?;
            // SAFETY: the plug-in owns this typed peer for the live controller and context.
            let context_peer =
                unsafe { ModelRef::<MusicalContextKind>::from_raw(context_peer.as_ptr().cast())? };
            (
                None,
                Some(context),
                properties.clone().with_musical_context(context_peer)?,
            )
        };
        let peer: ARAPlaybackRegionRef = self
            .session
            .playback_regions
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("playback region is provisional"))?
            .as_ptr();
        let ffi_properties = peer_properties.as_ffi(generation)?;
        // SAFETY: all checked graph records and property backing remain live through the call.
        unsafe {
            self.session
                .controller
                .raw_update_playback_region_properties(peer, ffi_properties.as_ref().as_ptr())?
        };
        let record = self.session.playback_regions.get_mut(handle)?;
        record.properties = properties;
        record.sequence = sequence;
        record.context = context;
        Ok(())
    }

    /// Enables or synchronously disables plug-in sample access for one active source.
    pub fn set_audio_source_samples_access(
        &mut self,
        handle: AudioSourceHandle,
        enable: bool,
    ) -> Result<(), AraError> {
        self.session.set_audio_source_samples_access(handle, enable)
    }

    /// Changes undo-history activation after enforcing source/modification ordering.
    pub fn set_audio_source_deactivated(
        &mut self,
        handle: AudioSourceHandle,
        deactivate: bool,
    ) -> Result<(), AraError> {
        let record = self.session.audio_sources.get(handle)?;
        if deactivate
            && self
                .session
                .audio_modifications
                .values()
                .any(|modification| modification.source == handle && modification.active)
        {
            return Err(AraError::InvalidState(
                "audio source still has active modifications",
            ));
        }
        let peer = record
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        // SAFETY: the source record owns this live peer and graph ordering was checked locally.
        unsafe {
            self.session
                .controller
                .raw_deactivate_audio_source_for_undo_history(
                    peer,
                    AraBool::from(deactivate).into_raw(),
                )?
        };
        self.session.audio_sources.get_mut(handle)?.active = !deactivate;
        Ok(())
    }

    /// Changes modification undo-history activation after enforcing graph ordering.
    pub fn set_audio_modification_deactivated(
        &mut self,
        handle: AudioModificationHandle,
        deactivate: bool,
    ) -> Result<(), AraError> {
        let record = self.session.audio_modifications.get(handle)?;
        if deactivate
            && self
                .session
                .playback_regions
                .values()
                .any(|region| region.modification == handle)
        {
            return Err(AraError::InvalidState(
                "audio modification still has playback regions",
            ));
        }
        if !deactivate && !self.session.audio_sources.get(record.source)?.active {
            return Err(AraError::InvalidState("audio source is deactivated"));
        }
        let peer = record
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr();
        // SAFETY: the modification record owns this live peer and ordering was checked locally.
        unsafe {
            self.session
                .controller
                .raw_deactivate_audio_modification_for_undo_history(
                    peer,
                    AraBool::from(deactivate).into_raw(),
                )?
        };
        self.session.audio_modifications.get_mut(handle)?.active = !deactivate;
        Ok(())
    }

    /// Destroys one context after proving no dependent graph object remains.
    pub fn destroy_musical_context(
        &mut self,
        handle: MusicalContextHandle,
    ) -> Result<(), AraError> {
        if self
            .session
            .region_sequences
            .values()
            .any(|sequence| sequence.context == handle)
            || self
                .session
                .playback_regions
                .values()
                .any(|region| region.context == Some(handle))
        {
            return Err(AraError::InvalidState(
                "musical context still has dependent regions or sequences",
            ));
        }
        let peer: ARAMusicalContextRef = self
            .session
            .musical_contexts
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("musical context is provisional"))?
            .as_ptr();
        // SAFETY: the live record owns the peer reference and this editing guard serializes use.
        unsafe { self.session.controller.raw_destroy_musical_context(peer)? };
        self.session.musical_contexts.remove(handle)?;
        Ok(())
    }

    /// Destroys a region sequence after proving no playback region references it.
    pub fn destroy_region_sequence(
        &mut self,
        handle: RegionSequenceHandle,
    ) -> Result<(), AraError> {
        if self
            .session
            .playback_regions
            .values()
            .any(|region| region.sequence == Some(handle))
        {
            return Err(AraError::InvalidState(
                "region sequence still has playback regions",
            ));
        }
        let peer: ARARegionSequenceRef = self
            .session
            .region_sequences
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("region sequence is provisional"))?
            .as_ptr();
        // SAFETY: the record owns the live peer ref and no dependent graph edge remains.
        unsafe { self.session.controller.raw_destroy_region_sequence(peer)? };
        self.session.region_sequences.remove(handle)?;
        Ok(())
    }

    /// Destroys an audio source after proving no modification references it.
    pub fn destroy_audio_source(&mut self, handle: AudioSourceHandle) -> Result<(), AraError> {
        if self
            .session
            .audio_modifications
            .values()
            .any(|modification| modification.source == handle)
        {
            return Err(AraError::InvalidState(
                "audio source still has modifications",
            ));
        }
        let record = self.session.audio_sources.get(handle)?;
        let peer: ARAAudioSourceRef = record
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr();
        let persistent_id = record.properties.persistent_id().to_owned();
        // SAFETY: the record owns the live peer ref and no dependent graph edge remains.
        unsafe { self.session.controller.raw_destroy_audio_source(peer)? };
        self.session.audio_sources.remove(handle)?;
        self.session.persistent_ids.remove(&persistent_id);
        Ok(())
    }

    /// Destroys an audio modification after proving no playback region references it.
    pub fn destroy_audio_modification(
        &mut self,
        handle: AudioModificationHandle,
    ) -> Result<(), AraError> {
        if self
            .session
            .playback_regions
            .values()
            .any(|region| region.modification == handle)
        {
            return Err(AraError::InvalidState(
                "audio modification still has playback regions",
            ));
        }
        let record = self.session.audio_modifications.get(handle)?;
        let peer: ARAAudioModificationRef = record
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr();
        let persistent_id = record.properties.persistent_id().to_owned();
        // SAFETY: the record owns the live peer ref and no dependent graph edge remains.
        unsafe {
            self.session
                .controller
                .raw_destroy_audio_modification(peer)?
        };
        self.session.audio_modifications.remove(handle)?;
        self.session.persistent_ids.remove(&persistent_id);
        Ok(())
    }

    /// Destroys one leaf playback region.
    pub fn destroy_playback_region(
        &mut self,
        handle: PlaybackRegionHandle,
    ) -> Result<(), AraError> {
        let peer: ARAPlaybackRegionRef = self
            .session
            .playback_regions
            .get(handle)?
            .peer
            .ok_or(AraError::InvalidState("playback region is provisional"))?
            .as_ptr();
        // SAFETY: playback regions are graph leaves and the record owns the live peer ref.
        unsafe { self.session.controller.raw_destroy_playback_region(peer)? };
        self.session.playback_regions.remove(handle)?;
        Ok(())
    }

    pub(crate) fn discard_playback_region(&mut self, handle: PlaybackRegionHandle) {
        let _ = self.session.playback_regions.remove(handle);
    }

    pub(crate) fn discard_audio_modification(&mut self, handle: AudioModificationHandle) {
        if let Ok(record) = self.session.audio_modifications.remove(handle) {
            self.session
                .persistent_ids
                .remove(record.properties.persistent_id());
        }
    }

    pub(crate) fn discard_audio_source(&mut self, handle: AudioSourceHandle) {
        if let Ok(record) = self.session.audio_sources.remove(handle) {
            self.session
                .persistent_ids
                .remove(record.properties.persistent_id());
        }
    }

    pub(crate) fn discard_region_sequence(&mut self, handle: RegionSequenceHandle) {
        let _ = self.session.region_sequences.remove(handle);
    }

    pub(crate) fn discard_musical_context(&mut self, handle: MusicalContextHandle) {
        let _ = self.session.musical_contexts.remove(handle);
    }

    /// Ends editing exactly once and reports locally observable dispatch errors.
    pub fn finish(mut self) -> Result<(), AraError> {
        let result = self.end_scope();
        self.session.editing = false;
        self.finished = true;
        result
    }
}

impl Drop for EditSession<'_, '_, '_> {
    fn drop(&mut self) {
        if !self.finished {
            if self.end_scope().is_err() {
                self.session.poisoned = true;
            }
            self.session.editing = false;
            self.finished = true;
        }
    }
}
