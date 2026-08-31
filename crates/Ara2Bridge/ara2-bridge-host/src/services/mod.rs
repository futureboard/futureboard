//! Stable host-service instances and semantic provider traits.

mod builder;
mod content;
mod dispatch;

use ara2_bridge_core::{AraError, ContentTimeRange};

pub use builder::{HostServices, HostServicesBuilder};
pub use content::{ContentAccessProvider, HostContentReaderSnapshot, HostContentSnapshot};

macro_rules! opaque_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub struct $name(usize);

        impl $name {
            pub(crate) fn from_ptr<T>(pointer: *mut T) -> Self {
                Self(pointer as usize)
            }

            /// Returns the address identity as a plain integer.
            ///
            /// Service callbacks name graph objects by this identity, so a host needs it to
            /// correlate a callback with its own record. The value is an address and is only
            /// meaningful while the owning document session is alive; it is never dereferenced.
            pub const fn as_usize(self) -> usize {
                self.0
            }

            /// Rebuilds the identity from a host-owned model-reference address.
            ///
            /// Pair with [`ModelRef::as_raw`](ara2_bridge_core::ModelRef::as_raw) on a handle from
            /// the owning [`DocumentSession`](crate::DocumentSession) to build a lookup table from
            /// this identity to a host record, or to name a source for
            /// [`HostServices::revoke_audio_source_readers`](crate::HostServices::revoke_audio_source_readers),
            /// which is otherwise uncallable from outside this crate.
            pub const fn from_address(value: usize) -> Self {
                Self(value)
            }
        }
    };
}

opaque_id!(
    AudioSourceId,
    "Address identity of a document-owned audio-source host record."
);
opaque_id!(
    MusicalContextId,
    "Address identity of a document-owned musical-context host record."
);
opaque_id!(
    AudioModificationId,
    "Address identity of a document-owned audio-modification host record."
);
opaque_id!(
    PlaybackRegionId,
    "Address identity of a document-owned playback-region host record."
);
opaque_id!(
    ArchiveReaderId,
    "Address identity of an archive reader supplied for one plug-in call."
);
opaque_id!(
    ArchiveWriterId,
    "Address identity of an archive writer supplied for one plug-in call."
);

/// One host audio reader created for a plug-in.
///
/// A reader is called by at most one thread at a time. Different readers may be
/// called concurrently.
pub trait HostAudioReader: Send + 'static {
    /// Returns the number of planar channels expected by every read.
    fn channel_count(&self) -> usize;

    /// Returns the source length in samples per channel.
    fn sample_count(&self) -> i64;

    /// Reads 32-bit planar samples.
    fn read_f32(
        &mut self,
        _sample_position: i64,
        _buffers: &mut [&mut [f32]],
    ) -> Result<(), AraError> {
        Err(AraError::Unsupported("32-bit audio reads"))
    }

    /// Reads 64-bit planar samples.
    fn read_f64(
        &mut self,
        _sample_position: i64,
        _buffers: &mut [&mut [f64]],
    ) -> Result<(), AraError> {
        Err(AraError::Unsupported("64-bit audio reads"))
    }
}

/// Resolves audio-source identities into independent reader instances.
pub trait AudioAccessProvider: Send + Sync + 'static {
    /// Creates a reader using the sample precision requested by the plug-in.
    fn create_reader(
        &self,
        source: AudioSourceId,
        use_64_bit_samples: bool,
    ) -> Result<Box<dyn HostAudioReader>, AraError>;
}

/// Supplies position-based archive I/O for document persistence.
pub trait ArchivingProvider: Send + Sync + 'static {
    /// Returns the byte length of a reader.
    fn len(&self, reader: ArchiveReaderId) -> Result<usize, AraError>;

    /// Reads exactly `buffer.len()` bytes at `position`.
    fn read_at(
        &self,
        reader: ArchiveReaderId,
        position: usize,
        buffer: &mut [u8],
    ) -> Result<(), AraError>;

    /// Writes exactly `buffer.len()` bytes at `position`.
    fn write_at(
        &self,
        writer: ArchiveWriterId,
        position: usize,
        buffer: &[u8],
    ) -> Result<(), AraError>;

    /// Reports document archiving progress in the inclusive range `0.0..=1.0`.
    fn archiving_progress(&self, _value: f32) -> Result<(), AraError> {
        Ok(())
    }

    /// Reports document restoration progress in the inclusive range `0.0..=1.0`.
    fn unarchiving_progress(&self, _value: f32) -> Result<(), AraError> {
        Ok(())
    }

    /// Returns the persistent archive ID for an ARA 2 reader, if known.
    fn document_archive_id(&self, _reader: ArchiveReaderId) -> Result<Option<String>, AraError> {
        Ok(None)
    }
}

/// Receives asynchronous plug-in model and analysis notifications.
pub trait ModelUpdateProvider: Send + Sync + 'static {
    /// Reports one source-analysis progress transition.
    fn audio_source_analysis_progress(
        &self,
        _source: AudioSourceId,
        _state: i32,
        _value: f32,
    ) -> Result<(), AraError> {
        Ok(())
    }

    /// Reports changed audio-source content.
    fn audio_source_content_changed(
        &self,
        _source: AudioSourceId,
        _range: Option<ContentTimeRange>,
        _flags: i32,
    ) -> Result<(), AraError> {
        Ok(())
    }

    /// Reports changed audio-modification content.
    fn audio_modification_content_changed(
        &self,
        _modification: AudioModificationId,
        _range: Option<ContentTimeRange>,
        _flags: i32,
    ) -> Result<(), AraError> {
        Ok(())
    }

    /// Reports changed playback-region content.
    fn playback_region_content_changed(
        &self,
        _region: PlaybackRegionId,
        _range: Option<ContentTimeRange>,
        _flags: i32,
    ) -> Result<(), AraError> {
        Ok(())
    }

    /// Reports that persistent document-level data changed.
    fn document_data_changed(&self) -> Result<(), AraError> {
        Ok(())
    }
}

/// Receives plug-in requests to control host transport playback.
pub trait PlaybackProvider: Send + Sync + 'static {
    /// Requests playback start.
    fn start(&self) -> Result<(), AraError>;
    /// Requests playback stop.
    fn stop(&self) -> Result<(), AraError>;
    /// Requests a new playback position in seconds.
    fn set_position(&self, position: f64) -> Result<(), AraError>;
    /// Requests a cycle range in seconds.
    fn set_cycle_range(&self, start: f64, duration: f64) -> Result<(), AraError>;
    /// Requests cycle enablement or disablement.
    fn enable_cycle(&self, enable: bool) -> Result<(), AraError>;
}
