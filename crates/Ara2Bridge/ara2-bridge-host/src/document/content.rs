//! Typed host access to plug-in content and analysis operations.

use super::{AudioModificationHandle, AudioSourceHandle, DocumentSession, PlaybackRegionHandle};
use crate::DocumentController;
use ara2_bridge_core::{
    AraBool, AraError, ContentGrade, ContentKind, ContentReader, ContentReaderBackend,
    ContentTimeRange,
};
use ara2_bridge_sys::{ARAContentReaderRef, ARAContentType};
use std::ffi::c_void;
use std::ptr::NonNull;

/// Exclusively owned plug-in content-reader backend tied to a borrowed document controller.
pub struct PluginContentReaderBackend<'reader, 'factory, 'services> {
    controller: &'reader mut DocumentController<'factory, 'services>,
    reader: NonNull<ara2_bridge_sys::ARAContentReaderRefMarkupType>,
    content_type: ARAContentType,
    destroyed: bool,
}

impl std::fmt::Debug for PluginContentReaderBackend<'_, '_, '_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginContentReaderBackend")
            .field("reader", &self.reader)
            .field("content_type", &self.content_type)
            .field("destroyed", &self.destroyed)
            .finish_non_exhaustive()
    }
}

// SAFETY: construction exclusively owns one non-null reader and borrows the controller mutably;
// dispatch preserves the ARA ephemeral-event lifetime until the next backend operation.
unsafe impl ContentReaderBackend for PluginContentReaderBackend<'_, '_, '_> {
    fn raw_content_type(&self) -> ARAContentType {
        self.content_type
    }

    fn event_count(&mut self) -> Result<i32, AraError> {
        // SAFETY: this backend exclusively owns the live peer reader.
        unsafe {
            self.controller
                .raw_get_content_reader_event_count(self.reader.as_ptr())
        }
    }

    unsafe fn event_data(&mut self, index: i32) -> Result<(*const c_void, usize), AraError> {
        // SAFETY: this backend exclusively owns the reader and forwards the requested index.
        let pointer = unsafe {
            self.controller
                .raw_get_content_reader_data_for_event(self.reader.as_ptr(), index)?
        };
        let pointer = NonNull::new(pointer.cast_mut())
            .ok_or(AraError::Peer("plug-in returned null content event data"))?;
        Ok((pointer.as_ptr(), raw_event_extent(self.content_type)?))
    }

    fn destroy(&mut self) {
        if !self.destroyed {
            // SAFETY: the backend owns this reader exactly once.
            let _ = unsafe {
                self.controller
                    .raw_destroy_content_reader(self.reader.as_ptr())
            };
            self.destroyed = true;
        }
    }
}

impl<'factory, 'services> DocumentSession<'factory, 'services> {
    /// Flushes pending plug-in model notifications at a legal host synchronization point.
    pub fn notify_model_updates(&mut self) -> Result<(), AraError> {
        self.require_content_operation()?;
        // SAFETY: the session exclusively owns the live controller for this call.
        unsafe { self.controller.raw_notify_model_updates() }
    }

    /// Returns whether the plug-in exposes typed content for a live audio source.
    pub fn audio_source_content_available<K: ContentKind>(
        &mut self,
        source: AudioSourceHandle,
    ) -> Result<bool, AraError> {
        self.require_content_operation()?;
        let peer = self.audio_source_peer(source)?;
        // SAFETY: peer belongs to this live controller and `K` supplies a released content type.
        let result = unsafe {
            self.controller
                .raw_is_audio_source_content_available(peer, K::RAW_TYPE)?
        };
        Ok(AraBool::from_raw(result).get())
    }

    /// Returns the plug-in's grade for typed content on a live audio source.
    pub fn audio_source_content_grade<K: ContentKind>(
        &mut self,
        source: AudioSourceHandle,
    ) -> Result<ContentGrade, AraError> {
        self.require_content_operation()?;
        let peer = self.audio_source_peer(source)?;
        // SAFETY: peer and content type are validated above.
        let grade = unsafe {
            self.controller
                .raw_get_audio_source_content_grade(peer, K::RAW_TYPE)?
        };
        Ok(ContentGrade::from_raw(grade))
    }

    /// Returns whether analysis remains incomplete for a typed source content kind.
    pub fn audio_source_content_analysis_incomplete<K: ContentKind>(
        &mut self,
        source: AudioSourceHandle,
    ) -> Result<bool, AraError> {
        self.require_content_operation()?;
        let peer = self.audio_source_peer(source)?;
        // SAFETY: peer belongs to this controller and the content kind is released.
        let result = unsafe {
            self.controller
                .raw_is_audio_source_content_analysis_incomplete(peer, K::RAW_TYPE)?
        };
        Ok(AraBool::from_raw(result).get())
    }

    /// Requests analysis of one typed content kind for a live audio source.
    pub fn request_audio_source_content_analysis<K: ContentKind>(
        &mut self,
        source: AudioSourceHandle,
    ) -> Result<(), AraError> {
        self.require_content_operation()?;
        if !self.analyzable_content_types.contains(&K::RAW_TYPE) {
            return Err(AraError::Unsupported("content analysis type"));
        }
        let peer = self.audio_source_peer(source)?;
        let content_type = K::RAW_TYPE;
        // SAFETY: the peer is live and the one-element content array remains valid through call.
        unsafe {
            self.controller
                .raw_request_audio_source_content_analysis(peer, 1, &content_type)
        }
    }

    /// Creates an exclusively borrowed typed content reader for a live audio source.
    pub fn audio_source_content_reader<K: ContentKind>(
        &mut self,
        source: AudioSourceHandle,
        range: Option<ContentTimeRange>,
    ) -> Result<
        Option<ContentReader<K, PluginContentReaderBackend<'_, 'factory, 'services>>>,
        AraError,
    > {
        self.require_content_operation()?;
        let peer = self.audio_source_peer(source)?;
        // SAFETY: peer and optional range backing remain live through reader creation.
        let reader: ARAContentReaderRef = unsafe {
            self.controller.raw_create_audio_source_content_reader(
                peer,
                K::RAW_TYPE,
                range
                    .as_ref()
                    .map_or(std::ptr::null(), ContentTimeRange::as_ptr),
            )?
        };
        let Some(reader) = NonNull::new(reader) else {
            return Ok(None);
        };
        ContentReader::new(PluginContentReaderBackend {
            controller: &mut self.controller,
            reader,
            content_type: K::RAW_TYPE,
            destroyed: false,
        })
        .map(Some)
    }

    /// Returns whether the plug-in exposes typed content for a live audio modification.
    pub fn audio_modification_content_available<K: ContentKind>(
        &mut self,
        modification: AudioModificationHandle,
    ) -> Result<bool, AraError> {
        self.require_content_operation()?;
        let peer = self.audio_modification_peer(modification)?;
        // SAFETY: peer and content kind are validated above.
        let result = unsafe {
            self.controller
                .raw_is_audio_modification_content_available(peer, K::RAW_TYPE)?
        };
        Ok(AraBool::from_raw(result).get())
    }

    /// Returns the plug-in's grade for typed audio-modification content.
    pub fn audio_modification_content_grade<K: ContentKind>(
        &mut self,
        modification: AudioModificationHandle,
    ) -> Result<ContentGrade, AraError> {
        self.require_content_operation()?;
        let peer = self.audio_modification_peer(modification)?;
        // SAFETY: peer and content kind are validated above.
        let grade = unsafe {
            self.controller
                .raw_get_audio_modification_content_grade(peer, K::RAW_TYPE)?
        };
        Ok(ContentGrade::from_raw(grade))
    }

    /// Creates an exclusively borrowed typed reader for audio-modification content.
    pub fn audio_modification_content_reader<K: ContentKind>(
        &mut self,
        modification: AudioModificationHandle,
        range: Option<ContentTimeRange>,
    ) -> Result<
        Option<ContentReader<K, PluginContentReaderBackend<'_, 'factory, 'services>>>,
        AraError,
    > {
        self.require_content_operation()?;
        let peer = self.audio_modification_peer(modification)?;
        // SAFETY: peer and optional range backing remain live through reader creation.
        let reader = unsafe {
            self.controller
                .raw_create_audio_modification_content_reader(
                    peer,
                    K::RAW_TYPE,
                    range
                        .as_ref()
                        .map_or(std::ptr::null(), ContentTimeRange::as_ptr),
                )?
        };
        self.finish_reader::<K>(reader)
    }

    /// Returns whether the plug-in exposes typed content for a live playback region.
    pub fn playback_region_content_available<K: ContentKind>(
        &mut self,
        region: PlaybackRegionHandle,
    ) -> Result<bool, AraError> {
        self.require_content_operation()?;
        let peer = self.playback_region_peer(region)?;
        // SAFETY: peer and content kind are validated above.
        let result = unsafe {
            self.controller
                .raw_is_playback_region_content_available(peer, K::RAW_TYPE)?
        };
        Ok(AraBool::from_raw(result).get())
    }

    /// Returns the plug-in's grade for typed playback-region content.
    pub fn playback_region_content_grade<K: ContentKind>(
        &mut self,
        region: PlaybackRegionHandle,
    ) -> Result<ContentGrade, AraError> {
        self.require_content_operation()?;
        let peer = self.playback_region_peer(region)?;
        // SAFETY: peer and content kind are validated above.
        let grade = unsafe {
            self.controller
                .raw_get_playback_region_content_grade(peer, K::RAW_TYPE)?
        };
        Ok(ContentGrade::from_raw(grade))
    }

    /// Creates an exclusively borrowed typed reader for playback-region content.
    pub fn playback_region_content_reader<K: ContentKind>(
        &mut self,
        region: PlaybackRegionHandle,
        range: Option<ContentTimeRange>,
    ) -> Result<
        Option<ContentReader<K, PluginContentReaderBackend<'_, 'factory, 'services>>>,
        AraError,
    > {
        self.require_content_operation()?;
        let peer = self.playback_region_peer(region)?;
        // SAFETY: peer and optional range backing remain live through reader creation.
        let reader = unsafe {
            self.controller.raw_create_playback_region_content_reader(
                peer,
                K::RAW_TYPE,
                range
                    .as_ref()
                    .map_or(std::ptr::null(), ContentTimeRange::as_ptr),
            )?
        };
        self.finish_reader::<K>(reader)
    }

    fn finish_reader<K: ContentKind>(
        &mut self,
        reader: ARAContentReaderRef,
    ) -> Result<
        Option<ContentReader<K, PluginContentReaderBackend<'_, 'factory, 'services>>>,
        AraError,
    > {
        let Some(reader) = NonNull::new(reader) else {
            return Ok(None);
        };
        ContentReader::new(PluginContentReaderBackend {
            controller: &mut self.controller,
            reader,
            content_type: K::RAW_TYPE,
            destroyed: false,
        })
        .map(Some)
    }

    fn require_content_operation(&self) -> Result<(), AraError> {
        if self.editing {
            Err(AraError::InvalidState(
                "content operation is unavailable while editing",
            ))
        } else if self.poisoned || self.services.is_poisoned() {
            Err(AraError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn audio_source_peer(
        &self,
        source: AudioSourceHandle,
    ) -> Result<ara2_bridge_sys::ARAAudioSourceRef, AraError> {
        Ok(self
            .audio_sources
            .get(source)?
            .peer
            .ok_or(AraError::InvalidState("audio source is provisional"))?
            .as_ptr())
    }

    fn audio_modification_peer(
        &self,
        modification: AudioModificationHandle,
    ) -> Result<ara2_bridge_sys::ARAAudioModificationRef, AraError> {
        Ok(self
            .audio_modifications
            .get(modification)?
            .peer
            .ok_or(AraError::InvalidState("audio modification is provisional"))?
            .as_ptr())
    }

    fn playback_region_peer(
        &self,
        region: PlaybackRegionHandle,
    ) -> Result<ara2_bridge_sys::ARAPlaybackRegionRef, AraError> {
        Ok(self
            .playback_regions
            .get(region)?
            .peer
            .ok_or(AraError::InvalidState("playback region is provisional"))?
            .as_ptr())
    }
}

fn raw_event_extent(content_type: ARAContentType) -> Result<usize, AraError> {
    use ara2_bridge_sys::*;
    match content_type {
        value if value == kARAContentTypeTempoEntries as i32 => {
            Ok(std::mem::size_of::<ARAContentTempoEntry>())
        }
        value if value == kARAContentTypeBarSignatures as i32 => {
            Ok(std::mem::size_of::<ARAContentBarSignature>())
        }
        value if value == kARAContentTypeNotes as i32 => Ok(std::mem::size_of::<ARAContentNote>()),
        value if value == kARAContentTypeStaticTuning as i32 => {
            Ok(std::mem::size_of::<ARAContentTuning>())
        }
        value if value == kARAContentTypeKeySignatures as i32 => {
            Ok(std::mem::size_of::<ARAContentKeySignature>())
        }
        value if value == kARAContentTypeSheetChords as i32 => {
            Ok(std::mem::size_of::<ARAContentChord>())
        }
        _ => Err(AraError::Unsupported("unknown content event extent")),
    }
}
