//! Checked processing, licensing, chunk, and realtime-query host operations.

use super::{AudioModificationHandle, AudioSourceHandle, DocumentSession, PlaybackRegionHandle};
use ara2_bridge_core::{
    ApiGeneration, AraBool, AraError, ForeignStr, LicenseCapabilities, LicenseRequest,
    ProcessingAlgorithmProperties,
};
use ara2_bridge_sys::{access::read_field, *};
use std::mem::offset_of;
use std::ptr::NonNull;

const MAXIMUM_ALGORITHMS: usize = 65_536;
const MAXIMUM_STRING_BYTES: usize = 16 * 1024;

/// Metadata returned after successfully storing an audio-file ARA chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAudioFileChunk {
    document_archive_id: String,
    open_automatically: bool,
}

impl StoredAudioFileChunk {
    /// Returns the compatible document archive ID selected by the plug-in.
    pub fn document_archive_id(&self) -> &str {
        &self.document_archive_id
    }

    /// Returns whether the stored state should be imported automatically.
    pub const fn open_automatically(&self) -> bool {
        self.open_automatically
    }
}

impl<'factory, 'services> DocumentSession<'factory, 'services> {
    /// Copies the stable processing-algorithm catalog advertised by the controller.
    pub fn processing_algorithms(
        &mut self,
    ) -> Result<Vec<ProcessingAlgorithmProperties>, AraError> {
        self.require_processing_operation()?;
        let count = self.controller.processing_algorithms_count()?;
        if count > MAXIMUM_ALGORITHMS {
            return Err(AraError::Peer(
                "processing algorithm count exceeds safety bound",
            ));
        }
        (0..count)
            .map(|index| {
                // SAFETY: the index is within the count returned immediately above.
                let pointer = unsafe {
                    self.controller
                        .raw_get_processing_algorithm_properties(index as i32)?
                };
                // SAFETY: the callback publishes controller-lifetime property backing.
                unsafe { copy_algorithm_properties(pointer) }
            })
            .collect()
    }

    /// Returns the active processing-algorithm index for one live audio source.
    pub fn processing_algorithm_for_audio_source(
        &mut self,
        source: AudioSourceHandle,
    ) -> Result<usize, AraError> {
        self.require_processing_operation()?;
        let count = self.controller.processing_algorithms_count()?;
        let peer = self.processing_audio_source_peer(source)?;
        // SAFETY: the source peer belongs to this controller.
        let index = unsafe {
            self.controller
                .raw_get_processing_algorithm_for_audio_source(peer)?
        };
        let index = usize::try_from(index)
            .map_err(|_| AraError::Peer("negative processing algorithm index"))?;
        if index >= count {
            Err(AraError::Peer(
                "processing algorithm index is out of bounds",
            ))
        } else {
            Ok(index)
        }
    }

    /// Builds the factory capability set used to validate license requests.
    pub fn license_capabilities(&self) -> Result<LicenseCapabilities, AraError> {
        LicenseCapabilities::new(
            self.analyzable_content_types.iter().copied(),
            self.supported_transformations,
        )
    }

    /// Queries licensing for a previously subset-validated capability request.
    pub fn is_licensed_for_capabilities(
        &mut self,
        request: &LicenseRequest,
    ) -> Result<bool, AraError> {
        self.require_processing_operation()?;
        let content_types = request.content_types();
        // SAFETY: request backing and content array remain valid through this synchronous call.
        let licensed = unsafe {
            self.controller.raw_is_licensed_for_capabilities(
                AraBool::from(request.run_modal_activation()).into_raw(),
                content_types.len(),
                if content_types.is_empty() {
                    std::ptr::null()
                } else {
                    content_types.as_ptr()
                },
                request.transformations().bits() as ARAPlaybackTransformationFlags,
            )?
        };
        Ok(AraBool::from_raw(licensed).get())
    }

    /// Returns whether a modification preserves its source signal exactly.
    pub fn audio_modification_preserves_source_signal(
        &mut self,
        modification: AudioModificationHandle,
    ) -> Result<bool, AraError> {
        self.require_processing_operation()?;
        let peer = self
            .audio_modifications
            .get(modification)?
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr();
        // SAFETY: the peer belongs to this live controller.
        let result = unsafe {
            self.controller
                .raw_is_audio_modification_preserving_audio_source_signal(peer)?
        };
        Ok(AraBool::from_raw(result).get())
    }

    /// Stores one source into an ARA audio-file chunk and returns the selected metadata.
    pub fn store_audio_source_to_audio_file_chunk<T>(
        &mut self,
        writer: &T,
        source: AudioSourceHandle,
    ) -> Result<StoredAudioFileChunk, AraError> {
        self.require_processing_operation()?;
        if self.controller.generation() < ApiGeneration::V2Final {
            return Err(AraError::Unsupported(
                "audio-file chunks before ARA 2 Final",
            ));
        }
        let peer = self.processing_audio_source_peer(source)?;
        let writer = std::ptr::from_ref(writer).cast_mut().cast();
        let mut archive_id = std::ptr::null();
        let mut open_automatically = kARAFalse;
        // SAFETY: writer identity and output pointers remain live through this synchronous call.
        let accepted = unsafe {
            self.controller.raw_store_audio_source_to_audio_file_chunk(
                writer,
                peer,
                &mut archive_id,
                &mut open_automatically,
            )?
        };
        if !AraBool::from_raw(accepted).get() {
            return Err(AraError::Peer("plug-in rejected audio-file chunk storage"));
        }
        // SAFETY: successful storage publishes controller-lifetime persistent-ID backing.
        let archive_id =
            unsafe { ForeignStr::copy_persistent_id(archive_id, MAXIMUM_STRING_BYTES)? }
                .into_string();
        if !self.compatible_archive_ids.contains(&archive_id) {
            return Err(AraError::Peer(
                "plug-in returned an incompatible archive ID",
            ));
        }
        Ok(StoredAudioFileChunk {
            document_archive_id: archive_id,
            open_automatically: AraBool::from_raw(open_automatically).get(),
        })
    }

    /// Returns the nonnegative playback head and tail extension times for a live region.
    pub fn playback_region_head_and_tail_time(
        &mut self,
        region: PlaybackRegionHandle,
    ) -> Result<(f64, f64), AraError> {
        self.require_processing_operation()?;
        let peer = self
            .playback_regions
            .get(region)?
            .peer
            .ok_or(AraError::InvalidState("playback region is provisional"))?
            .as_ptr();
        let mut head = 0.0;
        let mut tail = 0.0;
        // SAFETY: peer and output pointers remain live through the callback.
        unsafe {
            self.controller
                .raw_get_playback_region_head_and_tail_time(peer, &mut head, &mut tail)?
        };
        if !head.is_finite() || !tail.is_finite() || head < 0.0 || tail < 0.0 {
            Err(AraError::Peer("invalid playback-region head/tail time"))
        } else {
            Ok((head, tail))
        }
    }

    fn require_processing_operation(&self) -> Result<(), AraError> {
        if self.editing {
            Err(AraError::InvalidState(
                "processing query is unavailable while editing",
            ))
        } else if self.poisoned || self.services.is_poisoned() {
            Err(AraError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn processing_audio_source_peer(
        &self,
        source: AudioSourceHandle,
    ) -> Result<ARAAudioSourceRef, AraError> {
        Ok(self
            .audio_sources
            .get(source)?
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr())
    }
}

unsafe fn copy_algorithm_properties(
    pointer: *const ARAProcessingAlgorithmProperties,
) -> Result<ProcessingAlgorithmProperties, AraError> {
    let pointer = NonNull::new(pointer.cast_mut())
        .ok_or(AraError::Peer("null processing algorithm properties"))?;
    let base = pointer.as_ptr().cast::<u8>();
    // SAFETY: forwarded callback contract makes the header readable.
    let struct_size = unsafe { read_field::<ARASize>(base, 0) };
    if struct_size < kARAProcessingAlgorithmPropertiesMinSize as usize {
        return Err(AraError::Abi("truncated processing algorithm properties"));
    }
    // SAFETY: the validated minimum prefix represents both pointer fields.
    let persistent_id = unsafe {
        read_field::<ARAPersistentID>(
            base,
            offset_of!(ARAProcessingAlgorithmProperties, persistentID),
        )
    };
    // SAFETY: same validated prefix.
    let name = unsafe {
        read_field::<ARAUtf8String>(base, offset_of!(ARAProcessingAlgorithmProperties, name))
    };
    // SAFETY: the controller retains both bounded strings for its lifetime.
    let persistent_id =
        unsafe { ForeignStr::copy_persistent_id(persistent_id, MAXIMUM_STRING_BYTES)? };
    // SAFETY: same controller-lifetime bounded backing.
    let name = unsafe { ForeignStr::copy_display(name, MAXIMUM_STRING_BYTES)? };
    ProcessingAlgorithmProperties::new(persistent_id.as_str(), name.as_str())
}
