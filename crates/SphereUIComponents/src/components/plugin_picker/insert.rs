//! Insert target metadata and validation for the plugin picker.

use crate::components::timeline::timeline_state::TrackType;
use SpherePluginHost::{PluginKind, PluginStatus, RegistryPlugin};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginInsertKind {
    Instrument,
    Effect,
}

#[derive(Debug, Clone)]
pub struct PluginInsertTarget {
    pub track_id: String,
    pub track_name: String,
    pub track_type: TrackType,
    pub next_slot_index: usize,
    pub desired_kind: PluginInsertKind,
}

impl PluginInsertTarget {
    pub fn label(&self) -> String {
        let kind = match self.desired_kind {
            PluginInsertKind::Instrument => "Instrument",
            PluginInsertKind::Effect => "Effect",
        };
        format!(
            "Insert into: {} / {kind} Slot {}",
            self.track_name,
            self.next_slot_index + 1
        )
    }

    pub fn accepts_instrument(&self) -> bool {
        matches!(self.track_type, TrackType::Instrument | TrackType::Midi)
    }

    pub fn accepts_effect(&self) -> bool {
        !matches!(self.track_type, TrackType::Midi)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertValidation {
    Ok,
    NotInsertable,
    FailedScan,
    InstrumentOnEffectTrack,
    InstrumentOnInvalidTrack,
    UnsupportedFormat,
}

impl InsertValidation {
    pub fn message(&self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::NotInsertable => Some("This plug-in is not available for insert."),
            Self::FailedScan => Some("This plug-in failed scan and cannot be inserted."),
            Self::InstrumentOnEffectTrack => {
                Some("Instrument plug-ins require an instrument or MIDI track.")
            }
            Self::InstrumentOnInvalidTrack => Some("This track cannot host instrument plug-ins."),
            Self::UnsupportedFormat => Some("This plug-in format is not supported for insert yet."),
        }
    }
}

pub fn is_insertable(plugin: &RegistryPlugin) -> bool {
    plugin.supports_insert()
        && plugin.status == PluginStatus::PresetReady
        && plugin.scan_status.is_usable()
}

pub fn validate_insert(plugin: &RegistryPlugin, target: &PluginInsertTarget) -> InsertValidation {
    if !is_insertable(plugin) {
        if !plugin.scan_status.is_usable() {
            return InsertValidation::FailedScan;
        }
        if !plugin.supports_insert() {
            return InsertValidation::UnsupportedFormat;
        }
        return InsertValidation::NotInsertable;
    }

    match plugin.kind {
        PluginKind::Instrument => {
            if target.desired_kind == PluginInsertKind::Effect {
                return InsertValidation::InstrumentOnEffectTrack;
            }
            if !target.accepts_instrument() {
                if target.accepts_effect() {
                    return InsertValidation::InstrumentOnEffectTrack;
                }
                return InsertValidation::InstrumentOnInvalidTrack;
            }
        }
        // A plug-in that never declared its class is offered as an insert:
        // refusing it would make an effect that simply forgot to tag itself
        // impossible to load. It is still not offered for an instrument slot,
        // where a wrong choice produces a silent track.
        PluginKind::Effect | PluginKind::Unknown => {
            if target.desired_kind == PluginInsertKind::Instrument {
                return InsertValidation::InstrumentOnInvalidTrack;
            }
            if !target.accepts_effect() {
                return InsertValidation::InstrumentOnInvalidTrack;
            }
        }
    }

    InsertValidation::Ok
}
