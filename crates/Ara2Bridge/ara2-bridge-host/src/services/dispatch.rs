use super::{
    ArchiveReaderId, ArchiveWriterId, ArchivingProvider, AudioAccessProvider, AudioModificationId,
    AudioSourceId, ContentAccessProvider, HostAudioReader, HostContentReaderSnapshot,
    ModelUpdateProvider, MusicalContextId, PlaybackProvider, PlaybackRegionId,
};
use ara2_bridge_core::{ApiGeneration, AraError, ContentTimeRange};
use ara2_bridge_sys::*;
use std::collections::HashMap;
use std::ffi::CString;
use std::mem::{align_of, offset_of, size_of};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

struct ReaderState {
    reader: Mutex<Box<dyn HostAudioReader>>,
    source: AudioSourceId,
    use_64_bit_samples: bool,
    active: AtomicBool,
}

pub(super) struct ServiceState {
    audio: Arc<dyn AudioAccessProvider>,
    archiving: Arc<dyn ArchivingProvider>,
    model_update: Option<Arc<dyn ModelUpdateProvider>>,
    playback: Option<Arc<dyn PlaybackProvider>>,
    content: Option<Arc<dyn ContentAccessProvider>>,
    readers: Mutex<HashMap<usize, Arc<ReaderState>>>,
    content_readers: Mutex<HashMap<usize, Arc<HostContentReaderSnapshot>>>,
    archive_ids: Mutex<HashMap<usize, CString>>,
    provisional_references: Mutex<HashMap<usize, bool>>,
    poisoned: AtomicBool,
    diagnostics: Mutex<Vec<String>>,
}

impl ServiceState {
    pub(super) fn new(
        audio: Arc<dyn AudioAccessProvider>,
        archiving: Arc<dyn ArchivingProvider>,
        content: Option<Arc<dyn ContentAccessProvider>>,
        model_update: Option<Arc<dyn ModelUpdateProvider>>,
        playback: Option<Arc<dyn PlaybackProvider>>,
    ) -> Self {
        Self {
            audio,
            archiving,
            content,
            model_update,
            playback,
            readers: Mutex::new(HashMap::new()),
            content_readers: Mutex::new(HashMap::new()),
            archive_ids: Mutex::new(HashMap::new()),
            provisional_references: Mutex::new(HashMap::new()),
            poisoned: AtomicBool::new(false),
            diagnostics: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    pub(super) fn has_model_updates(&self) -> bool {
        self.model_update.is_some()
    }

    pub(super) fn has_content(&self) -> bool {
        self.content.is_some()
    }

    pub(super) fn has_playback(&self) -> bool {
        self.playback.is_some()
    }

    pub(super) fn diagnostics(&self) -> Vec<String> {
        lock(&self.diagnostics).clone()
    }

    pub(super) fn begin_provisional(&self, reference: usize) {
        lock(&self.provisional_references).insert(reference, false);
    }

    pub(super) fn observe_provisional(&self, reference: usize) {
        if let Some(observed) = lock(&self.provisional_references).get_mut(&reference) {
            *observed = true;
        }
    }

    pub(super) fn finish_provisional(&self, reference: usize) -> bool {
        lock(&self.provisional_references)
            .remove(&reference)
            .unwrap_or(false)
    }

    pub(super) fn document_archive_id(
        &self,
        reader: ArchiveReaderId,
    ) -> Result<Option<String>, AraError> {
        if self.is_poisoned() {
            return Err(AraError::InvalidState("host services are poisoned"));
        }
        match catch_unwind(AssertUnwindSafe(|| {
            self.archiving.document_archive_id(reader)
        })) {
            Ok(result) => result,
            Err(_) => {
                self.poisoned.store(true, Ordering::Release);
                self.record("panic contained while resolving an archive identifier".to_owned());
                Err(AraError::Peer("archiving provider panicked"))
            }
        }
    }

    pub(super) fn revoke_audio_source(&self, source: AudioSourceId) {
        let revoked = {
            let mut readers = lock(&self.readers);
            let keys = readers
                .iter()
                .filter_map(|(key, reader)| (reader.source == source).then_some(*key))
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| readers.remove(&key))
                .inspect(|reader| reader.active.store(false, Ordering::Release))
                .collect::<Vec<_>>()
        };
        for reader in revoked {
            drop(lock(&reader.reader));
        }
    }

    pub(super) fn revoke_all_readers(&self) {
        let revoked = {
            let mut readers = lock(&self.readers);
            readers
                .drain()
                .map(|(_, reader)| {
                    reader.active.store(false, Ordering::Release);
                    reader
                })
                .collect::<Vec<_>>()
        };
        for reader in revoked {
            drop(lock(&reader.reader));
        }
        lock(&self.content_readers).clear();
    }

    fn record(&self, message: String) {
        lock(&self.diagnostics).push(message);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn with_state<T, R>(
    reference: *mut R,
    fallback: T,
    operation: impl FnOnce(&ServiceState) -> Result<T, AraError>,
) -> T {
    if reference.is_null() {
        return fallback;
    }
    // SAFETY: advertised host references point to the live boxed `ServiceState`.
    let state = unsafe { &*reference.cast::<ServiceState>() };
    if state.is_poisoned() {
        return fallback;
    }
    match catch_unwind(AssertUnwindSafe(|| operation(state))) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            state.record(error.to_string());
            fallback
        }
        Err(_) => {
            state.poisoned.store(true, Ordering::Release);
            state.record("panic contained at ARA host callback boundary".to_owned());
            fallback
        }
    }
}

pub(super) fn audio_interface() -> ARAAudioAccessControllerInterface {
    ARAAudioAccessControllerInterface {
        structSize: size_of::<ARAAudioAccessControllerInterface>(),
        createAudioReaderForSource: Some(create_audio_reader),
        readAudioSamples: Some(read_audio_samples),
        destroyAudioReader: Some(destroy_audio_reader),
    }
}

pub(super) fn archive_interface(generation: ApiGeneration) -> ARAArchivingControllerInterface {
    let ara2 = generation >= ApiGeneration::V2Final;
    ARAArchivingControllerInterface {
        structSize: if ara2 {
            size_of::<ARAArchivingControllerInterface>()
        } else {
            offset_of!(ARAArchivingControllerInterface, getDocumentArchiveID)
        },
        getArchiveSize: Some(get_archive_size),
        readBytesFromArchive: Some(read_archive),
        writeBytesToArchive: Some(write_archive),
        notifyDocumentArchivingProgress: Some(archiving_progress),
        notifyDocumentUnarchivingProgress: Some(unarchiving_progress),
        getDocumentArchiveID: if ara2 {
            Some(document_archive_id)
        } else {
            None
        },
    }
}

pub(super) fn model_update_interface(
    generation: ApiGeneration,
) -> ARAModelUpdateControllerInterface {
    ARAModelUpdateControllerInterface {
        structSize: if generation >= ApiGeneration::V23Final {
            size_of::<ARAModelUpdateControllerInterface>()
        } else if generation >= ApiGeneration::V2Draft {
            offset_of!(ARAModelUpdateControllerInterface, notifyDocumentDataChanged)
        } else {
            offset_of!(
                ARAModelUpdateControllerInterface,
                notifyPlaybackRegionContentChanged
            )
        },
        notifyAudioSourceAnalysisProgress: Some(notify_analysis_progress),
        notifyAudioSourceContentChanged: Some(notify_source_content_changed),
        notifyAudioModificationContentChanged: Some(notify_modification_content_changed),
        notifyPlaybackRegionContentChanged: (generation >= ApiGeneration::V2Draft)
            .then_some(notify_region_content_changed),
        notifyDocumentDataChanged: (generation >= ApiGeneration::V23Final)
            .then_some(notify_document_data_changed),
    }
}

pub(super) fn playback_interface() -> ARAPlaybackControllerInterface {
    ARAPlaybackControllerInterface {
        structSize: size_of::<ARAPlaybackControllerInterface>(),
        requestStartPlayback: Some(request_start),
        requestStopPlayback: Some(request_stop),
        requestSetPlaybackPosition: Some(request_position),
        requestSetCycleRange: Some(request_cycle_range),
        requestEnableCycle: Some(request_cycle_enable),
    }
}

pub(super) fn content_interface() -> ARAContentAccessControllerInterface {
    ARAContentAccessControllerInterface {
        structSize: size_of::<ARAContentAccessControllerInterface>(),
        isMusicalContextContentAvailable: Some(context_content_available),
        getMusicalContextContentGrade: Some(context_content_grade),
        createMusicalContextContentReader: Some(create_context_content_reader),
        isAudioSourceContentAvailable: Some(source_content_available),
        getAudioSourceContentGrade: Some(source_content_grade),
        createAudioSourceContentReader: Some(create_source_content_reader),
        getContentReaderEventCount: Some(content_reader_count),
        getContentReaderDataForEvent: Some(content_reader_event),
        destroyContentReader: Some(destroy_content_reader),
    }
}

unsafe extern "C" fn create_audio_reader(
    host: ARAAudioAccessControllerHostRef,
    source: ARAAudioSourceHostRef,
    use_64_bit_samples: ARABool,
) -> ARAAudioReaderHostRef {
    with_state(host, null_mut(), |state| {
        if source.is_null() {
            return Err(AraError::InvalidArgument(
                "null audio source host reference",
            ));
        }
        state.observe_provisional(source as usize);
        let reader = state.audio.create_reader(
            AudioSourceId::from_ptr(source),
            use_64_bit_samples != kARAFalse,
        )?;
        if reader.channel_count() == 0 {
            return Err(AraError::InvalidArgument("audio reader has no channels"));
        }
        let reader = Arc::new(ReaderState {
            reader: Mutex::new(reader),
            source: AudioSourceId::from_ptr(source),
            use_64_bit_samples: use_64_bit_samples != kARAFalse,
            active: AtomicBool::new(true),
        });
        let pointer = Arc::as_ptr(&reader);
        lock(&state.readers).insert(pointer as usize, reader);
        Ok(pointer.cast_mut().cast())
    })
}

unsafe extern "C" fn read_audio_samples(
    host: ARAAudioAccessControllerHostRef,
    reader: ARAAudioReaderHostRef,
    sample_position: ARASamplePosition,
    samples_per_channel: ARASampleCount,
    buffers: *const *mut std::ffi::c_void,
) -> ARABool {
    with_state(host, kARAFalse, |state| {
        let count = usize::try_from(samples_per_channel)
            .map_err(|_| AraError::InvalidArgument("negative audio sample count"))?;
        let key = reader as usize;
        let reader = lock(&state.readers)
            .get(&key)
            .cloned()
            .ok_or(AraError::InvalidArgument("foreign or stale audio reader"))?;
        let mut implementation = lock(&reader.reader);
        let channels = implementation.channel_count();
        if buffers.is_null() && channels != 0 {
            return Err(AraError::InvalidArgument("null audio buffer array"));
        }
        if reader.use_64_bit_samples {
            // SAFETY: ARA supplies `channels` planar pointers, each writable for `count` f64 values.
            let mut planes = unsafe { planes::<f64>(buffers, channels, count)? };
            if !reader.active.load(Ordering::Acquire) {
                for plane in &mut planes {
                    plane.fill(0.0);
                }
                return Err(AraError::InvalidState("audio reader has been revoked"));
            }
            read_f64_with_silence(&mut **implementation, sample_position, &mut planes)?;
        } else {
            // SAFETY: ARA supplies `channels` planar pointers, each writable for `count` f32 values.
            let mut planes = unsafe { planes::<f32>(buffers, channels, count)? };
            if !reader.active.load(Ordering::Acquire) {
                for plane in &mut planes {
                    plane.fill(0.0);
                }
                return Err(AraError::InvalidState("audio reader has been revoked"));
            }
            read_f32_with_silence(&mut **implementation, sample_position, &mut planes)?;
        }
        Ok(kARATrue)
    })
}

fn valid_sample_window(
    reader: &dyn HostAudioReader,
    sample_position: i64,
    count: usize,
) -> Result<Option<(i64, usize, usize)>, AraError> {
    let source_end = reader.sample_count();
    if source_end < 0 {
        return Err(AraError::InvalidArgument(
            "negative audio source sample count",
        ));
    }
    let request_count = i64::try_from(count)
        .map_err(|_| AraError::InvalidArgument("audio sample count does not fit i64"))?;
    let request_end = sample_position
        .checked_add(request_count)
        .ok_or(AraError::InvalidArgument("audio sample range overflow"))?;
    let valid_start = sample_position.max(0).min(source_end);
    let valid_end = request_end.max(0).min(source_end);
    if valid_start >= valid_end {
        return Ok(None);
    }
    let output_offset = usize::try_from(valid_start - sample_position)
        .map_err(|_| AraError::InvalidArgument("audio output offset overflow"))?;
    let valid_count = usize::try_from(valid_end - valid_start)
        .map_err(|_| AraError::InvalidArgument("audio valid extent overflow"))?;
    Ok(Some((valid_start, output_offset, valid_count)))
}

fn read_f32_with_silence(
    reader: &mut dyn HostAudioReader,
    sample_position: i64,
    planes: &mut [&mut [f32]],
) -> Result<(), AraError> {
    for plane in planes.iter_mut() {
        plane.fill(0.0);
    }
    let count = planes.first().map_or(0, |plane| plane.len());
    let Some((valid_start, output_offset, valid_count)) =
        valid_sample_window(reader, sample_position, count)?
    else {
        return Ok(());
    };
    let mut valid_planes = planes
        .iter_mut()
        .map(|plane| &mut plane[output_offset..output_offset + valid_count])
        .collect::<Vec<_>>();
    let result = catch_unwind(AssertUnwindSafe(|| {
        reader.read_f32(valid_start, &mut valid_planes)
    }));
    drop(valid_planes);
    finish_audio_read(result, planes)
}

fn read_f64_with_silence(
    reader: &mut dyn HostAudioReader,
    sample_position: i64,
    planes: &mut [&mut [f64]],
) -> Result<(), AraError> {
    for plane in planes.iter_mut() {
        plane.fill(0.0);
    }
    let count = planes.first().map_or(0, |plane| plane.len());
    let Some((valid_start, output_offset, valid_count)) =
        valid_sample_window(reader, sample_position, count)?
    else {
        return Ok(());
    };
    let mut valid_planes = planes
        .iter_mut()
        .map(|plane| &mut plane[output_offset..output_offset + valid_count])
        .collect::<Vec<_>>();
    let result = catch_unwind(AssertUnwindSafe(|| {
        reader.read_f64(valid_start, &mut valid_planes)
    }));
    drop(valid_planes);
    finish_audio_read(result, planes)
}

fn finish_audio_read<T>(
    result: std::thread::Result<Result<(), AraError>>,
    planes: &mut [&mut [T]],
) -> Result<(), AraError>
where
    T: Copy + Default,
{
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            for plane in planes {
                plane.fill(T::default());
            }
            Err(error)
        }
        Err(payload) => {
            for plane in planes {
                plane.fill(T::default());
            }
            resume_unwind(payload)
        }
    }
}

unsafe fn planes<'a, T>(
    buffers: *const *mut std::ffi::c_void,
    channels: usize,
    count: usize,
) -> Result<Vec<&'a mut [T]>, AraError> {
    // SAFETY: the caller establishes that the pointer array contains `channels` entries.
    let pointers = unsafe { std::slice::from_raw_parts(buffers, channels) };
    let mut ranges = Vec::with_capacity(channels);
    for pointer in pointers {
        if pointer.is_null() && count != 0 {
            return Err(AraError::InvalidArgument("null planar audio buffer"));
        }
        let start = *pointer as usize;
        if count != 0 && start % align_of::<T>() != 0 {
            return Err(AraError::InvalidArgument("misaligned planar audio buffer"));
        }
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(AraError::InvalidArgument("audio buffer extent overflow"))?;
        let end = start
            .checked_add(bytes)
            .ok_or(AraError::InvalidArgument("audio buffer address overflow"))?;
        if ranges
            .iter()
            .any(|&(other_start, other_end)| start < other_end && other_start < end)
        {
            return Err(AraError::InvalidArgument(
                "overlapping planar audio buffers",
            ));
        }
        ranges.push((start, end));
    }
    Ok(pointers
        .iter()
        .map(|pointer| -> &mut [T] {
            if count == 0 {
                return &mut [];
            }
            // SAFETY: ranges were checked for null, overflow, and mutual overlap above.
            unsafe { std::slice::from_raw_parts_mut((*pointer).cast::<T>(), count) }
        })
        .collect())
}

unsafe extern "C" fn destroy_audio_reader(
    host: ARAAudioAccessControllerHostRef,
    reader: ARAAudioReaderHostRef,
) {
    with_state(host, (), |state| {
        let Some(reader) = lock(&state.readers).remove(&(reader as usize)) else {
            return Err(AraError::InvalidArgument("foreign or stale audio reader"));
        };
        reader.active.store(false, Ordering::Release);
        drop(lock(&reader.reader));
        Ok(())
    });
}

fn content(state: &ServiceState) -> Result<&dyn ContentAccessProvider, AraError> {
    state
        .content
        .as_deref()
        .ok_or(AraError::Unsupported("content-access host interface"))
}

unsafe extern "C" fn context_content_available(
    host: ARAContentAccessControllerHostRef,
    context: ARAMusicalContextHostRef,
    content_type: ARAContentType,
) -> ARABool {
    with_state(host, kARAFalse, |state| {
        if context.is_null() {
            return Err(AraError::InvalidArgument(
                "null musical context host reference",
            ));
        }
        state.observe_provisional(context as usize);
        Ok(
            if content(state)?
                .musical_context_grade(MusicalContextId::from_ptr(context), content_type)?
                .is_some()
            {
                kARATrue
            } else {
                kARAFalse
            },
        )
    })
}

unsafe extern "C" fn context_content_grade(
    host: ARAContentAccessControllerHostRef,
    context: ARAMusicalContextHostRef,
    content_type: ARAContentType,
) -> ARAContentGrade {
    with_state(host, kARAContentGradeInitial as ARAContentGrade, |state| {
        if context.is_null() {
            return Err(AraError::InvalidArgument(
                "null musical context host reference",
            ));
        }
        state.observe_provisional(context as usize);
        Ok(content(state)?
            .musical_context_grade(MusicalContextId::from_ptr(context), content_type)?
            .map_or(kARAContentGradeInitial as ARAContentGrade, |grade| {
                grade.as_raw()
            }))
    })
}

unsafe extern "C" fn create_context_content_reader(
    host: ARAContentAccessControllerHostRef,
    context: ARAMusicalContextHostRef,
    content_type: ARAContentType,
    range: *const ARAContentTimeRange,
) -> ARAContentReaderHostRef {
    with_state(host, null_mut(), |state| {
        if context.is_null() {
            return Err(AraError::InvalidArgument(
                "null musical context host reference",
            ));
        }
        state.observe_provisional(context as usize);
        let snapshot = content(state)?.musical_context_reader(
            MusicalContextId::from_ptr(context),
            content_type,
            // SAFETY: forwarded from the callback's ephemeral range contract.
            unsafe { copy_range(range)? },
        )?;
        publish_content_reader(state, content_type, snapshot)
    })
}

unsafe extern "C" fn source_content_available(
    host: ARAContentAccessControllerHostRef,
    source: ARAAudioSourceHostRef,
    content_type: ARAContentType,
) -> ARABool {
    with_state(host, kARAFalse, |state| {
        if source.is_null() {
            return Err(AraError::InvalidArgument(
                "null audio source host reference",
            ));
        }
        state.observe_provisional(source as usize);
        Ok(
            if content(state)?
                .audio_source_grade(AudioSourceId::from_ptr(source), content_type)?
                .is_some()
            {
                kARATrue
            } else {
                kARAFalse
            },
        )
    })
}

unsafe extern "C" fn source_content_grade(
    host: ARAContentAccessControllerHostRef,
    source: ARAAudioSourceHostRef,
    content_type: ARAContentType,
) -> ARAContentGrade {
    with_state(host, kARAContentGradeInitial as ARAContentGrade, |state| {
        if source.is_null() {
            return Err(AraError::InvalidArgument(
                "null audio source host reference",
            ));
        }
        state.observe_provisional(source as usize);
        Ok(content(state)?
            .audio_source_grade(AudioSourceId::from_ptr(source), content_type)?
            .map_or(kARAContentGradeInitial as ARAContentGrade, |grade| {
                grade.as_raw()
            }))
    })
}

unsafe extern "C" fn create_source_content_reader(
    host: ARAContentAccessControllerHostRef,
    source: ARAAudioSourceHostRef,
    content_type: ARAContentType,
    range: *const ARAContentTimeRange,
) -> ARAContentReaderHostRef {
    with_state(host, null_mut(), |state| {
        if source.is_null() {
            return Err(AraError::InvalidArgument(
                "null audio source host reference",
            ));
        }
        state.observe_provisional(source as usize);
        let snapshot = content(state)?.audio_source_reader(
            AudioSourceId::from_ptr(source),
            content_type,
            // SAFETY: forwarded from the callback's ephemeral range contract.
            unsafe { copy_range(range)? },
        )?;
        publish_content_reader(state, content_type, snapshot)
    })
}

fn publish_content_reader(
    state: &ServiceState,
    content_type: ARAContentType,
    snapshot: Option<HostContentReaderSnapshot>,
) -> Result<ARAContentReaderHostRef, AraError> {
    let Some(snapshot) = snapshot else {
        return Ok(null_mut());
    };
    if snapshot.content_type() != content_type {
        return Err(AraError::InvalidArgument("content snapshot type mismatch"));
    }
    let snapshot = Arc::new(snapshot);
    let pointer = Arc::as_ptr(&snapshot);
    lock(&state.content_readers).insert(pointer as usize, snapshot);
    Ok(pointer.cast_mut().cast())
}

unsafe extern "C" fn content_reader_count(
    host: ARAContentAccessControllerHostRef,
    reader: ARAContentReaderHostRef,
) -> ARAInt32 {
    with_state(host, 0, |state| {
        let reader = lock(&state.content_readers)
            .get(&(reader as usize))
            .cloned()
            .ok_or(AraError::InvalidArgument("foreign or stale content reader"))?;
        i32::try_from(reader.len())
            .map_err(|_| AraError::InvalidArgument("too many content events"))
    })
}

unsafe extern "C" fn content_reader_event(
    host: ARAContentAccessControllerHostRef,
    reader: ARAContentReaderHostRef,
    index: ARAInt32,
) -> *const std::ffi::c_void {
    with_state(host, null(), |state| {
        let index = usize::try_from(index)
            .map_err(|_| AraError::InvalidArgument("negative content event index"))?;
        let reader = lock(&state.content_readers)
            .get(&(reader as usize))
            .cloned()
            .ok_or(AraError::InvalidArgument("foreign or stale content reader"))?;
        reader.event_pointer(index).ok_or(AraError::InvalidArgument(
            "content event index out of bounds",
        ))
    })
}

unsafe extern "C" fn destroy_content_reader(
    host: ARAContentAccessControllerHostRef,
    reader: ARAContentReaderHostRef,
) {
    with_state(host, (), |state| {
        if lock(&state.content_readers)
            .remove(&(reader as usize))
            .is_none()
        {
            return Err(AraError::InvalidArgument("foreign or stale content reader"));
        }
        Ok(())
    });
}

fn model_updates(state: &ServiceState) -> Result<&dyn ModelUpdateProvider, AraError> {
    state
        .model_update
        .as_deref()
        .ok_or(AraError::Unsupported("model-update host interface"))
}

unsafe extern "C" fn notify_analysis_progress(
    host: ARAModelUpdateControllerHostRef,
    source: ARAAudioSourceHostRef,
    progress_state: ARAAnalysisProgressState,
    value: f32,
) {
    with_state(host, (), |state| {
        if source.is_null() || !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(AraError::InvalidArgument(
                "invalid source analysis progress notification",
            ));
        }
        state.observe_provisional(source as usize);
        model_updates(state)?.audio_source_analysis_progress(
            AudioSourceId::from_ptr(source),
            progress_state,
            value,
        )
    });
}

unsafe fn copy_range(
    range: *const ARAContentTimeRange,
) -> Result<Option<ContentTimeRange>, AraError> {
    // SAFETY: the enclosing ARA callback guarantees a readable optional range for this call.
    unsafe { ContentTimeRange::copy_optional_from_ffi(range) }
}

unsafe extern "C" fn notify_source_content_changed(
    host: ARAModelUpdateControllerHostRef,
    source: ARAAudioSourceHostRef,
    range: *const ARAContentTimeRange,
    flags: ARAContentUpdateFlags,
) {
    with_state(host, (), |state| {
        if source.is_null() {
            return Err(AraError::InvalidArgument(
                "null audio source host reference",
            ));
        }
        state.observe_provisional(source as usize);
        model_updates(state)?.audio_source_content_changed(
            AudioSourceId::from_ptr(source),
            // SAFETY: forwarded from the callback's ARA range contract.
            unsafe { copy_range(range)? },
            flags,
        )
    });
}

unsafe extern "C" fn notify_modification_content_changed(
    host: ARAModelUpdateControllerHostRef,
    modification: ARAAudioModificationHostRef,
    range: *const ARAContentTimeRange,
    flags: ARAContentUpdateFlags,
) {
    with_state(host, (), |state| {
        if modification.is_null() {
            return Err(AraError::InvalidArgument(
                "null audio modification host reference",
            ));
        }
        state.observe_provisional(modification as usize);
        model_updates(state)?.audio_modification_content_changed(
            AudioModificationId::from_ptr(modification),
            // SAFETY: forwarded from the callback's ARA range contract.
            unsafe { copy_range(range)? },
            flags,
        )
    });
}

unsafe extern "C" fn notify_region_content_changed(
    host: ARAModelUpdateControllerHostRef,
    region: ARAPlaybackRegionHostRef,
    range: *const ARAContentTimeRange,
    flags: ARAContentUpdateFlags,
) {
    with_state(host, (), |state| {
        if region.is_null() {
            return Err(AraError::InvalidArgument(
                "null playback region host reference",
            ));
        }
        state.observe_provisional(region as usize);
        model_updates(state)?.playback_region_content_changed(
            PlaybackRegionId::from_ptr(region),
            // SAFETY: forwarded from the callback's ARA range contract.
            unsafe { copy_range(range)? },
            flags,
        )
    });
}

unsafe extern "C" fn notify_document_data_changed(host: ARAModelUpdateControllerHostRef) {
    with_state(host, (), |state| {
        model_updates(state)?.document_data_changed()
    });
}

fn playback(state: &ServiceState) -> Result<&dyn PlaybackProvider, AraError> {
    state
        .playback
        .as_deref()
        .ok_or(AraError::Unsupported("playback host interface"))
}

unsafe extern "C" fn request_start(host: ARAPlaybackControllerHostRef) {
    with_state(host, (), |state| playback(state)?.start());
}

unsafe extern "C" fn request_stop(host: ARAPlaybackControllerHostRef) {
    with_state(host, (), |state| playback(state)?.stop());
}

unsafe extern "C" fn request_position(host: ARAPlaybackControllerHostRef, position: f64) {
    with_state(host, (), |state| {
        if !position.is_finite() {
            return Err(AraError::InvalidArgument("non-finite playback position"));
        }
        playback(state)?.set_position(position)
    });
}

unsafe extern "C" fn request_cycle_range(
    host: ARAPlaybackControllerHostRef,
    start: f64,
    duration: f64,
) {
    with_state(host, (), |state| {
        if !start.is_finite() || !duration.is_finite() || duration < 0.0 {
            return Err(AraError::InvalidArgument("invalid playback cycle range"));
        }
        playback(state)?.set_cycle_range(start, duration)
    });
}

unsafe extern "C" fn request_cycle_enable(host: ARAPlaybackControllerHostRef, enable: ARABool) {
    with_state(host, (), |state| {
        playback(state)?.enable_cycle(enable != kARAFalse)
    });
}

unsafe extern "C" fn get_archive_size(
    host: ARAArchivingControllerHostRef,
    reader: ARAArchiveReaderHostRef,
) -> ARASize {
    with_state(host, 0, |state| {
        state.archiving.len(ArchiveReaderId::from_ptr(reader))
    })
}

unsafe extern "C" fn read_archive(
    host: ARAArchivingControllerHostRef,
    reader: ARAArchiveReaderHostRef,
    position: ARASize,
    length: ARASize,
    buffer: *mut ARAByte,
) -> ARABool {
    with_state(host, kARAFalse, |state| {
        if buffer.is_null() && length != 0 {
            return Err(AraError::InvalidArgument("null archive read buffer"));
        }
        let bytes = if length == 0 {
            &mut []
        } else {
            // SAFETY: ARA supplies a writable buffer containing `length` bytes.
            unsafe { std::slice::from_raw_parts_mut(buffer, length) }
        };
        state
            .archiving
            .read_at(ArchiveReaderId::from_ptr(reader), position, bytes)?;
        Ok(kARATrue)
    })
}

unsafe extern "C" fn write_archive(
    host: ARAArchivingControllerHostRef,
    writer: ARAArchiveWriterHostRef,
    position: ARASize,
    length: ARASize,
    buffer: *const ARAByte,
) -> ARABool {
    with_state(host, kARAFalse, |state| {
        if buffer.is_null() && length != 0 {
            return Err(AraError::InvalidArgument("null archive write buffer"));
        }
        let bytes = if length == 0 {
            &[]
        } else {
            // SAFETY: ARA supplies a readable buffer containing `length` bytes.
            unsafe { std::slice::from_raw_parts(buffer, length) }
        };
        state
            .archiving
            .write_at(ArchiveWriterId::from_ptr(writer), position, bytes)?;
        Ok(kARATrue)
    })
}

unsafe extern "C" fn archiving_progress(host: ARAArchivingControllerHostRef, value: f32) {
    with_state(host, (), |state| {
        validate_progress(value)?;
        state.archiving.archiving_progress(value)
    });
}

unsafe extern "C" fn unarchiving_progress(host: ARAArchivingControllerHostRef, value: f32) {
    with_state(host, (), |state| {
        validate_progress(value)?;
        state.archiving.unarchiving_progress(value)
    });
}

fn validate_progress(value: f32) -> Result<(), AraError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(AraError::InvalidArgument(
            "archive progress outside 0.0..=1.0",
        ))
    }
}

unsafe extern "C" fn document_archive_id(
    host: ARAArchivingControllerHostRef,
    reader: ARAArchiveReaderHostRef,
) -> ARAPersistentID {
    with_state(host, null(), |state| {
        let key = reader as usize;
        let Some(id) = state
            .archiving
            .document_archive_id(ArchiveReaderId::from_ptr(reader))?
        else {
            return Ok(null());
        };
        let id =
            CString::new(id).map_err(|_| AraError::InvalidArgument("archive ID contains NUL"))?;
        let pointer = id.as_ptr();
        lock(&state.archive_ids).insert(key, id);
        Ok(pointer)
    })
}
