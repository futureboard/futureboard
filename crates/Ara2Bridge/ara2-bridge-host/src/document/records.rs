//! Stable graph records retained before crossing create callbacks.

use ara2_bridge_core::{
    AudioModificationKind, AudioModificationProperties, AudioSourceKind, AudioSourceProperties,
    Handle, MusicalContextKind, MusicalContextProperties, PlaybackRegionProperties,
    RegionSequenceKind, RegionSequenceProperties,
};
use ara2_bridge_sys::{
    ARAAudioModificationRefMarkupType, ARAAudioSourceRefMarkupType, ARAMusicalContextRefMarkupType,
    ARAPlaybackRegionRefMarkupType, ARARegionSequenceRefMarkupType,
};
use std::ptr::NonNull;

pub(crate) struct MusicalContextRecord {
    pub(crate) properties: MusicalContextProperties,
    pub(crate) peer: Option<NonNull<ARAMusicalContextRefMarkupType>>,
}

pub(crate) struct RegionSequenceRecord {
    pub(crate) properties: RegionSequenceProperties,
    pub(crate) peer: Option<NonNull<ARARegionSequenceRefMarkupType>>,
    pub(crate) context: Handle<MusicalContextKind>,
}

pub(crate) struct AudioSourceRecord {
    pub(crate) properties: AudioSourceProperties,
    pub(crate) peer: Option<NonNull<ARAAudioSourceRefMarkupType>>,
    pub(crate) active: bool,
    pub(crate) samples_access_enabled: bool,
}

pub(crate) struct AudioModificationRecord {
    pub(crate) properties: AudioModificationProperties,
    pub(crate) peer: Option<NonNull<ARAAudioModificationRefMarkupType>>,
    pub(crate) source: Handle<AudioSourceKind>,
    pub(crate) active: bool,
}

pub(crate) struct PlaybackRegionRecord {
    pub(crate) properties: PlaybackRegionProperties,
    pub(crate) peer: Option<NonNull<ARAPlaybackRegionRefMarkupType>>,
    pub(crate) modification: Handle<AudioModificationKind>,
    pub(crate) sequence: Option<Handle<RegionSequenceKind>>,
    pub(crate) context: Option<Handle<MusicalContextKind>>,
}
