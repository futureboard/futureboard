//! Track audio-input selection over logical Input Audio Connections.
//!
//! ```txt
//! Add Track / Inspector  ->  Input Audio Connection  ->  AudioConnectionRegistry
//!                                                            -> physical ports
//! ```
//!
//! The counterpart to [`crate::output_routing`], and the only place a track
//! input selector gets its choices. Two rules define it:
//!
//! * **No physical devices.** Neither Add Track nor the Inspector enumerates
//!   hardware. A device or channel only becomes selectable once the user has
//!   made it a logical bus in Audio Connections, which is why both selectors end
//!   with [`InputChoice::OpenAudioConnections`].
//! * **`None` is No Input.** It is never "system default input": a track with no
//!   connection captures nothing. Wanting default hardware is expressed by
//!   creating a bus for it, not by leaving the field empty.

use crate::audio_connections::{
    AudioConnection, AudioConnectionDirection, AudioConnectionId, AudioConnectionRegistry,
    AvailablePorts, ChannelLayout,
};
use crate::output_routing::UNAVAILABLE_SUFFIX;

/// Label for a track with no input connection.
pub const NO_INPUT_LABEL: &str = "No Input";

/// Label shown when the stored id is not in this project's registry at all.
pub const MISSING_CONNECTION_LABEL: &str = "Missing connection";

/// What a track Input selector asks for. Deliberately carries no device or
/// channel: the only routing value a track can be given is a connection id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackInputSelection {
    /// Capture nothing. Never a system default input.
    NoInput,
    Connection(AudioConnectionId),
    /// Not a routing change — opens the window where buses are created.
    OpenAudioConnections,
}

/// One entry in a track Input selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputChoice {
    /// Captures nothing. Not a system default.
    NoInput,
    Connection {
        id: AudioConnectionId,
        label: String,
        /// Channel layout, so a selector can group Mono and Stereo entries.
        layout: ChannelLayout,
        /// False when this entry is only listed because it is the current
        /// assignment and cannot currently reach hardware.
        available: bool,
    },
    /// Opens the Audio Connections window. Never a routing change.
    OpenAudioConnections,
}

impl InputChoice {
    pub fn label(&self) -> String {
        match self {
            Self::NoInput => NO_INPUT_LABEL.to_string(),
            Self::Connection {
                label, available, ..
            } => {
                if *available {
                    label.clone()
                } else {
                    format!("{label} — {UNAVAILABLE_SUFFIX}")
                }
            }
            Self::OpenAudioConnections => "Open Audio Connections...".to_string(),
        }
    }

    pub fn connection_id(&self) -> Option<&AudioConnectionId> {
        match self {
            Self::Connection { id, .. } => Some(id),
            _ => None,
        }
    }

    /// What choosing this entry asks the layout to do.
    pub fn selection(&self) -> TrackInputSelection {
        match self {
            Self::NoInput => TrackInputSelection::NoInput,
            Self::Connection { id, .. } => TrackInputSelection::Connection(id.clone()),
            Self::OpenAudioConnections => TrackInputSelection::OpenAudioConnections,
        }
    }

    /// Group heading a selector may file this entry under.
    pub fn group(&self) -> Option<&'static str> {
        match self {
            Self::Connection { layout, .. } => Some(match layout {
                ChannelLayout::Mono => "Mono",
                ChannelLayout::Stereo => "Stereo",
                ChannelLayout::Custom { .. } => "Multichannel",
            }),
            _ => None,
        }
    }
}

fn is_offerable_input(connection: &AudioConnection) -> bool {
    connection.direction == AudioConnectionDirection::Input && connection.enabled
}

/// Entries for a track Input selector.
///
/// `track_channels` orders the list the way [`AudioConnectionRegistry::input_choices_for`]
/// does — exact layout matches first, then legitimate up-mixes — without ever
/// hiding a usable bus. A stale or disabled current assignment is appended so
/// the selector shows what the track actually holds.
pub fn track_input_options(
    registry: &AudioConnectionRegistry,
    track_channels: usize,
    current: Option<&AudioConnectionId>,
) -> Vec<InputChoice> {
    let mut entries = vec![InputChoice::NoInput];
    let mut listed_current = false;
    for connection in registry.input_choices_for(track_channels) {
        if !is_offerable_input(connection) {
            continue;
        }
        if Some(&connection.id) == current {
            listed_current = true;
        }
        entries.push(InputChoice::Connection {
            id: connection.id.clone(),
            label: connection.name.clone(),
            layout: connection.channel_layout,
            available: connection.status.is_usable(),
        });
    }

    // A disconnected device must never make the track look unassigned.
    if let Some(current) = current {
        if !listed_current {
            let (label, layout) = registry
                .get(current)
                .map(|connection| (connection.name.clone(), connection.channel_layout))
                .unwrap_or_else(|| (MISSING_CONNECTION_LABEL.to_string(), ChannelLayout::Mono));
            entries.push(InputChoice::Connection {
                id: current.clone(),
                label,
                layout,
                available: false,
            });
        }
    }

    entries.push(InputChoice::OpenAudioConnections);
    entries
}

/// Primary label for a track's input.
pub fn track_input_label(
    registry: &AudioConnectionRegistry,
    current: Option<&AudioConnectionId>,
) -> String {
    let Some(id) = current else {
        return NO_INPUT_LABEL.to_string();
    };
    match registry.get(id) {
        Some(connection) if is_offerable_input(connection) && connection.status.is_usable() => {
            connection.name.clone()
        }
        Some(connection) => format!("{} — {UNAVAILABLE_SUFFIX}", connection.name),
        // The id survives, but this project has no such bus. Say so rather than
        // showing No Input, which would misreport what the track holds.
        None => MISSING_CONNECTION_LABEL.to_string(),
    }
}

/// Secondary line for a connection: `Mono · Studio 24c · Input 1`.
///
/// Purely descriptive. The hardware behind a bus lives in the registry, so this
/// is rebuilt on demand rather than cached on the track.
pub fn track_input_detail(
    registry: &AudioConnectionRegistry,
    ports: &AvailablePorts,
    id: &AudioConnectionId,
) -> Option<String> {
    let connection = registry.get(id)?;
    let layout = match connection.channel_layout {
        ChannelLayout::Mono => "Mono".to_string(),
        ChannelLayout::Stereo => "Stereo".to_string(),
        ChannelLayout::Custom { channels } => format!("{channels} ch"),
    };
    let device = registry
        .device_display_name(id, ports)
        .unwrap_or_else(|| "No Device".to_string());
    Some(format!(
        "{layout} · {device} · {}",
        connection.port_summary()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_connections::{AudioConnection, AudioConnectionStatus};

    fn device() -> AvailablePorts {
        AvailablePorts::for_device("dev-1", "Studio 24c", 4, 4)
    }

    fn registry() -> AudioConnectionRegistry {
        AudioConnectionRegistry::default_template(&device(), "dev-1")
    }

    fn input_id(registry: &AudioConnectionRegistry, name: &str) -> AudioConnectionId {
        registry
            .by_direction(AudioConnectionDirection::Input)
            .into_iter()
            .find(|c| c.name == name)
            .expect("input connection")
            .id
            .clone()
    }

    #[test]
    fn a_track_input_selector_lists_only_input_connections() {
        let registry = registry();
        let options = track_input_options(&registry, 1, None);

        assert_eq!(options.first(), Some(&InputChoice::NoInput));
        assert_eq!(options.last(), Some(&InputChoice::OpenAudioConnections));
        for id in options.iter().filter_map(InputChoice::connection_id) {
            assert_eq!(
                registry.get(id).unwrap().direction,
                AudioConnectionDirection::Input,
                "an output bus must never be offered as a track input"
            );
        }
    }

    /// The list is built from logical buses, so no device or channel string can
    /// reach it: creating hardware mappings is Audio Connections' job.
    #[test]
    fn no_physical_device_is_enumerated_in_the_selector() {
        let registry = registry();
        let labels: Vec<String> = track_input_options(&registry, 2, None)
            .iter()
            .map(InputChoice::label)
            .collect();
        assert!(
            !labels.iter().any(|label| label.contains("Studio 24c")),
            "device names belong in Audio Connections, not the track selector: {labels:?}"
        );
        assert!(labels
            .iter()
            .any(|label| label == "Open Audio Connections..."));
    }

    #[test]
    fn a_stereo_track_sees_stereo_first_then_legitimate_mono_upmixes() {
        let registry = registry();
        let options = track_input_options(&registry, 2, None);
        let names: Vec<String> = options
            .iter()
            .filter_map(InputChoice::connection_id)
            .map(|id| registry.name_of(id).unwrap().to_string())
            .collect();
        assert_eq!(names.first().map(String::as_str), Some("Stereo Input 1-2"));
        assert!(names.iter().any(|name| name == "Mono Input 1"));
    }

    #[test]
    fn entries_carry_a_group_for_selectors_that_want_headings() {
        let registry = registry();
        let options = track_input_options(&registry, 2, None);
        let mono = options
            .iter()
            .find(|choice| {
                choice
                    .connection_id()
                    .is_some_and(|id| registry.name_of(id) == Some("Mono Input 1"))
            })
            .unwrap();
        assert_eq!(mono.group(), Some("Mono"));
        assert_eq!(InputChoice::NoInput.group(), None);
    }

    #[test]
    fn a_disabled_input_is_not_offered_but_a_current_assignment_stays_visible() {
        let ports = device();
        let mut registry = registry();
        let mic = input_id(&registry, "Mono Input 1");
        registry.set_enabled(&mic, false);
        registry.revalidate(&ports);

        assert!(track_input_options(&registry, 1, None)
            .iter()
            .filter_map(InputChoice::connection_id)
            .all(|id| *id != mic));

        let entry = track_input_options(&registry, 1, Some(&mic))
            .into_iter()
            .find(|choice| choice.connection_id() == Some(&mic))
            .expect("the current assignment is always visible");
        assert_eq!(entry.label(), "Mono Input 1 — Unavailable");
    }

    /// Disconnecting the interface must not clear the track's routing.
    #[test]
    fn a_missing_device_is_labelled_unavailable_without_clearing_the_assignment() {
        let mut registry = registry();
        let mic = input_id(&registry, "Mono Input 1");
        registry.revalidate(&AvailablePorts::default());

        assert_eq!(
            registry.get(&mic).unwrap().status,
            AudioConnectionStatus::DeviceMissing
        );
        assert_eq!(
            track_input_label(&registry, Some(&mic)),
            "Mono Input 1 — Unavailable"
        );
        assert_eq!(
            track_input_options(&registry, 1, Some(&mic))
                .iter()
                .filter(|choice| choice.connection_id() == Some(&mic))
                .count(),
            1,
            "the assignment stays selectable exactly once"
        );
    }

    #[test]
    fn no_input_is_the_label_for_an_unassigned_track() {
        let registry = registry();
        assert_eq!(track_input_label(&registry, None), NO_INPUT_LABEL);
    }

    /// An id from another project resolves to nothing here; the selector says so
    /// rather than quietly reporting No Input.
    #[test]
    fn an_unresolvable_id_is_reported_as_missing_not_as_no_input() {
        let registry = registry();
        let ghost = AudioConnectionId::from_stored("ac-ghost");
        assert_eq!(
            track_input_label(&registry, Some(&ghost)),
            MISSING_CONNECTION_LABEL
        );
    }

    #[test]
    fn the_detail_line_names_layout_device_and_ports() {
        let ports = device();
        let registry = registry();
        let stereo = input_id(&registry, "Stereo Input 1-2");
        assert_eq!(
            track_input_detail(&registry, &ports, &stereo).as_deref(),
            Some("Stereo · Studio 24c · Input 1 / Input 2")
        );
    }

    #[test]
    fn renaming_a_connection_changes_the_label_only() {
        let ports = device();
        let mut registry = registry();
        let mic = input_id(&registry, "Mono Input 1");
        registry.rename(&mic, "Vocal Mic");
        registry.revalidate(&ports);

        assert_eq!(track_input_label(&registry, Some(&mic)), "Vocal Mic");
        assert_eq!(input_id(&registry, "Vocal Mic"), mic);
    }

    /// A bus is never created as a side effect of picking one.
    #[test]
    fn listing_choices_never_mutates_the_registry() {
        let registry = registry();
        let before = registry.clone();
        let _ = track_input_options(&registry, 1, None);
        let _ = track_input_label(&registry, None);
        assert_eq!(registry, before);
    }
}
