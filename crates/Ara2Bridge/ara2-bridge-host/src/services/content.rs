use super::{AudioSourceId, MusicalContextId};
use ara2_bridge_core::{
    validate_event_sequence, AraError, BarSignatures, ContentGrade, ContentKind, ContentTimeRange,
    KeySignatures, Notes, SheetChords, StaticTuning, Tempo,
};
use ara2_bridge_sys::*;
use std::ffi::{c_void, CString};
use std::marker::PhantomData;

/// Immutable validated events prepared for one host content reader.
pub struct HostContentSnapshot<K: ContentKind> {
    events: Box<[K::Event]>,
    _kind: PhantomData<K>,
}

impl<K: ContentKind> HostContentSnapshot<K> {
    /// Validates event values and ordering before retaining them.
    pub fn new(events: impl IntoIterator<Item = K::Event>) -> Result<Self, AraError> {
        let events = events.into_iter().collect::<Vec<_>>().into_boxed_slice();
        validate_event_sequence::<K>(&events)?;
        Ok(Self {
            events,
            _kind: PhantomData,
        })
    }
}

struct NamedEvents<R> {
    _names: Box<[Option<CString>]>,
    raw: Box<[R]>,
}

enum EventStorage {
    Tempo(Box<[ARAContentTempoEntry]>),
    Bars(Box<[ARAContentBarSignature]>),
    Notes(Box<[ARAContentNote]>),
    Tuning(NamedEvents<ARAContentTuning>),
    Keys(NamedEvents<ARAContentKeySignature>),
    Chords(NamedEvents<ARAContentChord>),
}

/// Type-erased immutable event storage retained until reader destruction.
pub struct HostContentReaderSnapshot {
    content_type: ARAContentType,
    grade: ContentGrade,
    events: EventStorage,
}

impl HostContentReaderSnapshot {
    /// Returns the raw ARA content type represented by this snapshot.
    pub fn content_type(&self) -> ARAContentType {
        self.content_type
    }

    /// Returns the quality grade supplied for this snapshot.
    pub fn grade(&self) -> ContentGrade {
        self.grade
    }

    /// Returns the number of retained events.
    pub fn len(&self) -> usize {
        match &self.events {
            EventStorage::Tempo(v) => v.len(),
            EventStorage::Bars(v) => v.len(),
            EventStorage::Notes(v) => v.len(),
            EventStorage::Tuning(v) => v.raw.len(),
            EventStorage::Keys(v) => v.raw.len(),
            EventStorage::Chords(v) => v.raw.len(),
        }
    }

    /// Returns whether the snapshot contains no events.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn event_pointer(&self, index: usize) -> Option<*const c_void> {
        macro_rules! pointer {
            ($events:expr) => {
                $events
                    .get(index)
                    .map(|event| std::ptr::from_ref(event).cast())
            };
        }
        match &self.events {
            EventStorage::Tempo(v) => pointer!(v),
            EventStorage::Bars(v) => pointer!(v),
            EventStorage::Notes(v) => pointer!(v),
            EventStorage::Tuning(v) => pointer!(v.raw),
            EventStorage::Keys(v) => pointer!(v.raw),
            EventStorage::Chords(v) => pointer!(v.raw),
        }
    }
}

// SAFETY: snapshots are immutable after construction. Their raw name pointers target CString
// allocations owned by the same snapshot and remain readable until the snapshot is dropped.
unsafe impl Send for HostContentReaderSnapshot {}
// SAFETY: same immutable self-owned backing invariant as the `Send` implementation.
unsafe impl Sync for HostContentReaderSnapshot {}

impl HostContentSnapshot<Tempo> {
    /// Erases tempo events for ABI publication.
    pub fn into_reader(self, grade: ContentGrade) -> HostContentReaderSnapshot {
        let raw = self
            .events
            .iter()
            .map(|e| ARAContentTempoEntry {
                timePosition: e.time_position(),
                quarterPosition: e.quarter_position(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        HostContentReaderSnapshot {
            content_type: Tempo::RAW_TYPE,
            grade,
            events: EventStorage::Tempo(raw),
        }
    }
}

impl HostContentSnapshot<BarSignatures> {
    /// Erases bar-signature events for ABI publication.
    pub fn into_reader(self, grade: ContentGrade) -> HostContentReaderSnapshot {
        let raw = self
            .events
            .iter()
            .map(|e| ARAContentBarSignature {
                numerator: e.numerator(),
                denominator: e.denominator(),
                position: e.position(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        HostContentReaderSnapshot {
            content_type: BarSignatures::RAW_TYPE,
            grade,
            events: EventStorage::Bars(raw),
        }
    }
}

impl HostContentSnapshot<Notes> {
    /// Erases note events for ABI publication.
    pub fn into_reader(self, grade: ContentGrade) -> HostContentReaderSnapshot {
        let raw = self
            .events
            .iter()
            .map(|e| ARAContentNote {
                frequency: e.frequency().unwrap_or(kARAInvalidFrequency),
                pitchNumber: e.pitch().unwrap_or(kARAInvalidPitchNumber),
                volume: e.volume(),
                startPosition: e.start_position(),
                attackDuration: e.attack_duration(),
                noteDuration: e.note_duration(),
                signalDuration: e.signal_duration(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        HostContentReaderSnapshot {
            content_type: Notes::RAW_TYPE,
            grade,
            events: EventStorage::Notes(raw),
        }
    }
}

fn names(values: impl Iterator<Item = Option<String>>) -> Box<[Option<CString>]> {
    values
        .map(|v| v.map(|v| CString::new(v).expect("validated content name")))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

impl HostContentSnapshot<StaticTuning> {
    /// Erases tuning events for ABI publication.
    pub fn into_reader(self, grade: ContentGrade) -> HostContentReaderSnapshot {
        let names = names(self.events.iter().map(|e| e.name().map(str::to_owned)));
        let raw = self
            .events
            .iter()
            .zip(names.iter())
            .map(|(e, name)| ARAContentTuning {
                concertPitchFrequency: e.concert_pitch_frequency(),
                root: e.root(),
                tunings: *e.tunings(),
                name: name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        HostContentReaderSnapshot {
            content_type: StaticTuning::RAW_TYPE,
            grade,
            events: EventStorage::Tuning(NamedEvents { _names: names, raw }),
        }
    }
}

impl HostContentSnapshot<KeySignatures> {
    /// Erases key-signature events for ABI publication.
    pub fn into_reader(self, grade: ContentGrade) -> HostContentReaderSnapshot {
        let names = names(self.events.iter().map(|e| e.name().map(str::to_owned)));
        let raw = self
            .events
            .iter()
            .zip(names.iter())
            .map(|(e, name)| ARAContentKeySignature {
                root: e.root(),
                intervals: e.intervals().map(|v| v.as_raw()),
                name: name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                position: e.position(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        HostContentReaderSnapshot {
            content_type: KeySignatures::RAW_TYPE,
            grade,
            events: EventStorage::Keys(NamedEvents { _names: names, raw }),
        }
    }
}

impl HostContentSnapshot<SheetChords> {
    /// Erases chord events for ABI publication.
    pub fn into_reader(self, grade: ContentGrade) -> HostContentReaderSnapshot {
        let names = names(self.events.iter().map(|e| e.name().map(str::to_owned)));
        let raw = self
            .events
            .iter()
            .zip(names.iter())
            .map(|(e, name)| ARAContentChord {
                root: e.root(),
                bass: e.bass(),
                intervals: e.intervals().map(|v| v.as_raw()),
                name: name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                position: e.position(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        HostContentReaderSnapshot {
            content_type: SheetChords::RAW_TYPE,
            grade,
            events: EventStorage::Chords(NamedEvents { _names: names, raw }),
        }
    }
}

/// Supplies optional musical-context and audio-source content snapshots.
pub trait ContentAccessProvider: Send + Sync + 'static {
    /// Returns context content grade, or `None` when unavailable.
    fn musical_context_grade(
        &self,
        context: MusicalContextId,
        content_type: i32,
    ) -> Result<Option<ContentGrade>, AraError>;
    /// Creates a context reader snapshot.
    fn musical_context_reader(
        &self,
        context: MusicalContextId,
        content_type: i32,
        range: Option<ContentTimeRange>,
    ) -> Result<Option<HostContentReaderSnapshot>, AraError>;
    /// Returns source content grade, or `None` when unavailable.
    fn audio_source_grade(
        &self,
        source: AudioSourceId,
        content_type: i32,
    ) -> Result<Option<ContentGrade>, AraError>;
    /// Creates a source reader snapshot.
    fn audio_source_reader(
        &self,
        source: AudioSourceId,
        content_type: i32,
        range: Option<ContentTimeRange>,
    ) -> Result<Option<HostContentReaderSnapshot>, AraError>;
}
