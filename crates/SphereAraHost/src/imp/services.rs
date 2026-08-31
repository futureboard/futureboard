//! ARA host services: the callbacks a plug-in makes back into Futureboard.
//!
//! Every provider here is called from plug-in threads, never from the session's
//! model thread. They therefore own everything they need and never call back
//! into [`crate::imp::Session`].
//!
//! ARA names graph objects in callbacks by the *address* of the host record that
//! created them. [`GraphIndex`] is the reverse map from those addresses to
//! Futureboard's own persistent keys; the session fills it right after each
//! create call, using `DocumentSession::*_ref(handle).as_raw()`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ara2_bridge_core::{
    AraError, BarSignatureEvent, BarSignatures, ContentGrade, ContentKind, ContentTimeRange, Tempo,
    TempoEvent,
};
use ara2_bridge_host::{
    ArchiveReaderId, ArchiveWriterId, ArchivingProvider, AudioAccessProvider, AudioModificationId,
    AudioSourceId, ContentAccessProvider, HostAudioReader, HostContentReaderSnapshot,
    HostContentSnapshot, ModelUpdateProvider, MusicalContextId, PlaybackProvider, PlaybackRegionId,
};

use crate::model::{
    AraAudioAccess, AraClipKey, AraModelObserver, AraModelUpdate, AraMusicalTimeline, AraSourceKey,
    AraTransportControl, AraTransportRequest,
};

/// Reverse lookup from ARA object identity to Futureboard's persistent keys.
///
/// The maps are small (one entry per asset / clip) and are read on plug-in
/// threads, so a plain mutex is the right tool: no allocation happens under the
/// lock on the read path, and none of these calls sit on an audio thread.
#[derive(Default)]
pub(crate) struct GraphIndex {
    sources: Mutex<HashMap<usize, AraSourceKey>>,
    modifications: Mutex<HashMap<usize, AraClipKey>>,
    regions: Mutex<HashMap<usize, AraClipKey>>,
    musical_context: Mutex<Option<usize>>,
}

impl GraphIndex {
    pub(crate) fn insert_source(&self, address: usize, key: AraSourceKey) {
        let _ = self.sources.lock().map(|mut map| map.insert(address, key));
    }

    pub(crate) fn remove_source(&self, address: usize) {
        let _ = self.sources.lock().map(|mut map| map.remove(&address));
    }

    pub(crate) fn insert_modification(&self, address: usize, key: AraClipKey) {
        let _ = self
            .modifications
            .lock()
            .map(|mut map| map.insert(address, key));
    }

    pub(crate) fn remove_modification(&self, address: usize) {
        let _ = self
            .modifications
            .lock()
            .map(|mut map| map.remove(&address));
    }

    pub(crate) fn insert_region(&self, address: usize, key: AraClipKey) {
        let _ = self.regions.lock().map(|mut map| map.insert(address, key));
    }

    pub(crate) fn remove_region(&self, address: usize) {
        let _ = self.regions.lock().map(|mut map| map.remove(&address));
    }

    pub(crate) fn set_musical_context(&self, address: Option<usize>) {
        if let Ok(mut slot) = self.musical_context.lock() {
            *slot = address;
        }
    }

    fn source_key(&self, source: AudioSourceId) -> Option<AraSourceKey> {
        self.sources
            .lock()
            .ok()
            .and_then(|map| map.get(&source.as_usize()).cloned())
    }

    fn modification_key(&self, modification: AudioModificationId) -> Option<AraClipKey> {
        self.modifications
            .lock()
            .ok()
            .and_then(|map| map.get(&modification.as_usize()).cloned())
    }

    fn region_key(&self, region: PlaybackRegionId) -> Option<AraClipKey> {
        self.regions
            .lock()
            .ok()
            .and_then(|map| map.get(&region.as_usize()).cloned())
    }

    fn is_musical_context(&self, context: MusicalContextId) -> bool {
        self.musical_context
            .lock()
            .ok()
            .and_then(|slot| *slot)
            .is_some_and(|address| address == context.as_usize())
    }
}

/// Serves random-access reads for every ARA audio source.
pub(crate) struct AudioService {
    access: Arc<dyn AraAudioAccess>,
    index: Arc<GraphIndex>,
}

impl AudioService {
    pub(crate) fn new(access: Arc<dyn AraAudioAccess>, index: Arc<GraphIndex>) -> Self {
        Self { access, index }
    }
}

impl AudioAccessProvider for AudioService {
    fn create_reader(
        &self,
        source: AudioSourceId,
        use_64_bit_samples: bool,
    ) -> Result<Box<dyn HostAudioReader>, AraError> {
        let key = self
            .index
            .source_key(source)
            .ok_or(AraError::Peer("ARA audio source is not known to the host"))?;
        let reader = self
            .access
            .open_reader(&key)
            .map_err(|_| AraError::Peer("host could not open the ARA audio source"))?;
        let channels = reader.channel_count();
        Ok(Box::new(SampleReaderAdapter {
            reader,
            channels,
            // Planar scratch is grown once per reader on the first read of a
            // given block size and reused afterwards, so steady-state reads do
            // not allocate.
            scratch: Vec::new(),
            wants_f64: use_64_bit_samples,
        }))
    }
}

/// Adapts Futureboard's f32 planar reader to ARA's f32/f64 reader contract.
struct SampleReaderAdapter {
    reader: Box<dyn crate::model::AraSampleReader>,
    channels: usize,
    scratch: Vec<f32>,
    wants_f64: bool,
}

impl SampleReaderAdapter {
    /// Reads into `self.scratch` laid out as `channels` contiguous planes.
    fn fill_scratch(&mut self, sample_position: i64, frames: usize) -> Result<(), AraError> {
        let needed = self.channels * frames;
        if self.scratch.len() < needed {
            self.scratch.resize(needed, 0.0);
        }
        let (used, _) = self.scratch.split_at_mut(needed);
        let mut planes: Vec<&mut [f32]> = used.chunks_mut(frames).collect();
        self.reader
            .read_planar_f32(sample_position, &mut planes)
            .map_err(|_| AraError::Peer("host audio read failed"))
    }
}

impl HostAudioReader for SampleReaderAdapter {
    fn channel_count(&self) -> usize {
        self.channels
    }

    fn sample_count(&self) -> i64 {
        self.reader.frame_count()
    }

    fn read_f32(
        &mut self,
        sample_position: i64,
        buffers: &mut [&mut [f32]],
    ) -> Result<(), AraError> {
        if buffers.len() != self.channels {
            return Err(AraError::InvalidArgument("ARA read channel-count mismatch"));
        }
        let frames = buffers.first().map_or(0, |plane| plane.len());
        if frames == 0 {
            return Ok(());
        }
        self.reader
            .read_planar_f32(sample_position, buffers)
            .map_err(|_| AraError::Peer("host audio read failed"))
    }

    fn read_f64(
        &mut self,
        sample_position: i64,
        buffers: &mut [&mut [f64]],
    ) -> Result<(), AraError> {
        if !self.wants_f64 {
            return Err(AraError::Unsupported("64-bit audio reads"));
        }
        if buffers.len() != self.channels {
            return Err(AraError::InvalidArgument("ARA read channel-count mismatch"));
        }
        let frames = buffers.first().map_or(0, |plane| plane.len());
        if frames == 0 {
            return Ok(());
        }
        self.fill_scratch(sample_position, frames)?;
        for (channel, plane) in buffers.iter_mut().enumerate() {
            let start = channel * frames;
            for (out, sample) in plane.iter_mut().zip(&self.scratch[start..start + frames]) {
                *out = f64::from(*sample);
            }
        }
        Ok(())
    }
}

/// One archive buffer plus the archive identifier it was written under.
#[derive(Default)]
pub(crate) struct ArchiveSlot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) archive_id: Option<String>,
}

/// Position-addressed archive storage shared with the plug-in's persistence
/// callbacks.
///
/// Slots are keyed by the address of the token the session passes to
/// `store_document_to_archive` / `restore_document_from_archive`, which is
/// exactly the identity ARA hands back as `ArchiveReaderId` / `ArchiveWriterId`.
#[derive(Default)]
pub(crate) struct ArchiveStore {
    slots: Mutex<HashMap<usize, ArchiveSlot>>,
}

impl ArchiveStore {
    pub(crate) fn open(&self, address: usize, slot: ArchiveSlot) {
        let _ = self.slots.lock().map(|mut map| map.insert(address, slot));
    }

    pub(crate) fn take(&self, address: usize) -> Option<ArchiveSlot> {
        self.slots
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&address))
    }
}

/// Serves the plug-in's archive reads and writes.
pub(crate) struct ArchiveService {
    store: Arc<ArchiveStore>,
}

impl ArchiveService {
    pub(crate) fn new(store: Arc<ArchiveStore>) -> Self {
        Self { store }
    }

    fn with_slot<T>(
        &self,
        address: usize,
        action: impl FnOnce(&mut ArchiveSlot) -> Result<T, AraError>,
    ) -> Result<T, AraError> {
        let mut slots = self
            .store
            .slots
            .lock()
            .map_err(|_| AraError::Peer("ARA archive store is poisoned"))?;
        let slot = slots
            .get_mut(&address)
            .ok_or(AraError::Peer("unknown ARA archive handle"))?;
        action(slot)
    }
}

impl ArchivingProvider for ArchiveService {
    fn len(&self, reader: ArchiveReaderId) -> Result<usize, AraError> {
        self.with_slot(reader.as_usize(), |slot| Ok(slot.bytes.len()))
    }

    fn read_at(
        &self,
        reader: ArchiveReaderId,
        position: usize,
        buffer: &mut [u8],
    ) -> Result<(), AraError> {
        self.with_slot(reader.as_usize(), |slot| {
            let end = position
                .checked_add(buffer.len())
                .ok_or(AraError::InvalidArgument("ARA archive read overflows"))?;
            if end > slot.bytes.len() {
                return Err(AraError::InvalidArgument("ARA archive read past the end"));
            }
            buffer.copy_from_slice(&slot.bytes[position..end]);
            Ok(())
        })
    }

    fn write_at(
        &self,
        writer: ArchiveWriterId,
        position: usize,
        buffer: &[u8],
    ) -> Result<(), AraError> {
        self.with_slot(writer.as_usize(), |slot| {
            let end = position
                .checked_add(buffer.len())
                .ok_or(AraError::InvalidArgument("ARA archive write overflows"))?;
            if slot.bytes.len() < end {
                slot.bytes.resize(end, 0);
            }
            slot.bytes[position..end].copy_from_slice(buffer);
            Ok(())
        })
    }

    fn document_archive_id(&self, reader: ArchiveReaderId) -> Result<Option<String>, AraError> {
        self.with_slot(reader.as_usize(), |slot| Ok(slot.archive_id.clone()))
    }
}

/// Forwards asynchronous plug-in notifications to the application.
pub(crate) struct ModelService {
    observer: Arc<dyn AraModelObserver>,
    index: Arc<GraphIndex>,
}

impl ModelService {
    pub(crate) fn new(observer: Arc<dyn AraModelObserver>, index: Arc<GraphIndex>) -> Self {
        Self { observer, index }
    }
}

impl ModelUpdateProvider for ModelService {
    fn audio_source_analysis_progress(
        &self,
        source: AudioSourceId,
        state: i32,
        value: f32,
    ) -> Result<(), AraError> {
        self.observer.notify(AraModelUpdate::AnalysisProgress {
            source: self.index.source_key(source),
            state,
            value,
        });
        Ok(())
    }

    fn audio_source_content_changed(
        &self,
        source: AudioSourceId,
        _range: Option<ContentTimeRange>,
        _flags: i32,
    ) -> Result<(), AraError> {
        self.observer.notify(AraModelUpdate::SourceContentChanged {
            source: self.index.source_key(source),
        });
        Ok(())
    }

    fn audio_modification_content_changed(
        &self,
        modification: AudioModificationId,
        _range: Option<ContentTimeRange>,
        _flags: i32,
    ) -> Result<(), AraError> {
        self.observer
            .notify(AraModelUpdate::ModificationContentChanged {
                clip: self.index.modification_key(modification),
            });
        Ok(())
    }

    fn playback_region_content_changed(
        &self,
        region: PlaybackRegionId,
        _range: Option<ContentTimeRange>,
        _flags: i32,
    ) -> Result<(), AraError> {
        self.observer.notify(AraModelUpdate::RegionContentChanged {
            clip: self.index.region_key(region),
        });
        Ok(())
    }

    fn document_data_changed(&self) -> Result<(), AraError> {
        self.observer.notify(AraModelUpdate::DocumentDataChanged);
        Ok(())
    }
}

/// Forwards the plug-in editor's transport requests.
pub(crate) struct TransportService {
    control: Arc<dyn AraTransportControl>,
}

impl TransportService {
    pub(crate) fn new(control: Arc<dyn AraTransportControl>) -> Self {
        Self { control }
    }
}

impl PlaybackProvider for TransportService {
    fn start(&self) -> Result<(), AraError> {
        self.control.request(AraTransportRequest::Start);
        Ok(())
    }

    fn stop(&self) -> Result<(), AraError> {
        self.control.request(AraTransportRequest::Stop);
        Ok(())
    }

    fn set_position(&self, position: f64) -> Result<(), AraError> {
        self.control
            .request(AraTransportRequest::SetPosition(position));
        Ok(())
    }

    fn set_cycle_range(&self, start: f64, duration: f64) -> Result<(), AraError> {
        self.control
            .request(AraTransportRequest::SetCycleRange { start, duration });
        Ok(())
    }

    fn enable_cycle(&self, enable: bool) -> Result<(), AraError> {
        self.control
            .request(AraTransportRequest::EnableCycle(enable));
        Ok(())
    }
}

/// Publishes the project's tempo map and bar signatures to the plug-in.
///
/// Futureboard has no key-signature or chord track, and analysis of audio
/// sources is the plug-in's job, not the host's — every other content type
/// reports unavailable rather than inventing data.
pub(crate) struct ContentService {
    index: Arc<GraphIndex>,
    timeline: Mutex<AraMusicalTimeline>,
}

impl ContentService {
    pub(crate) fn new(index: Arc<GraphIndex>) -> Self {
        Self {
            index,
            timeline: Mutex::new(AraMusicalTimeline::default()),
        }
    }

    /// Replaces the published timeline. Called on the model thread.
    pub(crate) fn publish(&self, timeline: &AraMusicalTimeline) {
        if let Ok(mut slot) = self.timeline.lock() {
            slot.clone_from(timeline);
        }
    }

    /// Whether a timeline has been published yet.
    pub(crate) fn has_timeline(&self) -> bool {
        self.timeline
            .lock()
            .map(|timeline| !timeline.tempo.is_empty())
            .unwrap_or(false)
    }
}

impl ContentAccessProvider for ContentService {
    fn musical_context_grade(
        &self,
        context: MusicalContextId,
        content_type: i32,
    ) -> Result<Option<ContentGrade>, AraError> {
        if !self.index.is_musical_context(context) || !self.has_timeline() {
            return Ok(None);
        }
        if content_type == <Tempo as ContentKind>::RAW_TYPE
            || content_type == <BarSignatures as ContentKind>::RAW_TYPE
        {
            // The user authored this tempo map, so it is approved, not detected.
            Ok(Some(ContentGrade::APPROVED))
        } else {
            Ok(None)
        }
    }

    fn musical_context_reader(
        &self,
        context: MusicalContextId,
        content_type: i32,
        _range: Option<ContentTimeRange>,
    ) -> Result<Option<HostContentReaderSnapshot>, AraError> {
        if !self.index.is_musical_context(context) {
            return Ok(None);
        }
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| AraError::Peer("ARA musical timeline is poisoned"))?;

        if content_type == <Tempo as ContentKind>::RAW_TYPE {
            if timeline.tempo.len() < 2 {
                return Ok(None);
            }
            let mut events = Vec::with_capacity(timeline.tempo.len());
            for entry in &timeline.tempo {
                events.push(TempoEvent::new(entry.time_seconds, entry.quarter_position)?);
            }
            let snapshot = HostContentSnapshot::<Tempo>::new(events)?;
            return Ok(Some(snapshot.into_reader(ContentGrade::APPROVED)));
        }

        if content_type == <BarSignatures as ContentKind>::RAW_TYPE {
            if timeline.bars.is_empty() {
                return Ok(None);
            }
            let mut events = Vec::with_capacity(timeline.bars.len());
            for bar in &timeline.bars {
                events.push(BarSignatureEvent::new(
                    bar.numerator,
                    bar.denominator,
                    bar.quarter_position,
                )?);
            }
            let snapshot = HostContentSnapshot::<BarSignatures>::new(events)?;
            return Ok(Some(snapshot.into_reader(ContentGrade::APPROVED)));
        }

        Ok(None)
    }

    fn audio_source_grade(
        &self,
        _source: AudioSourceId,
        _content_type: i32,
    ) -> Result<Option<ContentGrade>, AraError> {
        // The host performs no audio analysis of its own; the plug-in is the
        // authority on what is inside an audio source.
        Ok(None)
    }

    fn audio_source_reader(
        &self,
        _source: AudioSourceId,
        _content_type: i32,
        _range: Option<ContentTimeRange>,
    ) -> Result<Option<HostContentReaderSnapshot>, AraError> {
        Ok(None)
    }
}

/// Shares one [`ContentService`] between the host-services vtable and the
/// session, which keeps publishing new timelines into it.
///
/// `Arc` is not a fundamental type, so the orphan rule forbids implementing a
/// foreign trait for `Arc<ContentService>` directly; this newtype is the
/// standard way around that.
pub(crate) struct SharedContent(pub(crate) Arc<ContentService>);

impl ContentAccessProvider for SharedContent {
    fn musical_context_grade(
        &self,
        context: MusicalContextId,
        content_type: i32,
    ) -> Result<Option<ContentGrade>, AraError> {
        self.0.musical_context_grade(context, content_type)
    }

    fn musical_context_reader(
        &self,
        context: MusicalContextId,
        content_type: i32,
        range: Option<ContentTimeRange>,
    ) -> Result<Option<HostContentReaderSnapshot>, AraError> {
        self.0.musical_context_reader(context, content_type, range)
    }

    fn audio_source_grade(
        &self,
        source: AudioSourceId,
        content_type: i32,
    ) -> Result<Option<ContentGrade>, AraError> {
        self.0.audio_source_grade(source, content_type)
    }

    fn audio_source_reader(
        &self,
        source: AudioSourceId,
        content_type: i32,
        range: Option<ContentTimeRange>,
    ) -> Result<Option<HostContentReaderSnapshot>, AraError> {
        self.0.audio_source_reader(source, content_type, range)
    }
}
