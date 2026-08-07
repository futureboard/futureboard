//! Master and Monitor / Control Room output routing over Audio Connections.
//!
//! ```txt
//! Master bus      -> master_output_connection_id   -> Output Audio Connection
//! Control Room    -> monitor_output_connection_id  -> Output Audio Connection
//!                    (None = Follow Master Output)
//! ```
//!
//! This module owns three things and nothing else:
//!
//! * **The choices** each selector offers ([`master_output_options`],
//!   [`monitor_output_options`]). Only *output* connections are ever listed, and
//!   a hardware port is never offered directly — the registry is still the only
//!   layer that maps a logical bus to physical ports.
//! * **The labels** those selections read as, including the two states that must
//!   never be silently repaired: an assignment whose connection is currently
//!   unavailable, and a Monitor following a Master that has no output.
//! * **The one-time bootstrap** that gives an older project a Master output so it
//!   does not become unexpectedly silent after upgrading.
//!
//! Neither field ever stores a device id, a port, or a resolved channel: they
//! store an [`AudioConnectionId`] and nothing more, so a rename or a re-patch of
//! that bus flows through without touching Master or Monitor.

use crate::audio_connections::{
    AudioConnection, AudioConnectionDirection, AudioConnectionId, AudioConnectionRegistry,
    AudioConnectionStatus, AvailablePorts, ChannelLayout,
};

/// Suffix shown when an assignment still resolves to a connection, but that
/// connection cannot currently reach hardware.
pub const UNAVAILABLE_SUFFIX: &str = "Unavailable";

/// Label for a Monitor that follows Master.
pub const FOLLOW_MASTER_LABEL: &str = "Follow Master Output";

/// Label for a Master with no output assignment.
pub const NO_OUTPUT_LABEL: &str = "No Output";

/// One entry in a Master or Monitor Output selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputChoice {
    /// Master: "No Output". Clears `master_output_connection_id`.
    NoOutput,
    /// Monitor: "Follow Master Output". Clears `monitor_output_connection_id`.
    FollowMaster,
    /// One Output Audio Connection.
    Connection {
        id: AudioConnectionId,
        label: String,
        /// False when this entry is only listed because it is the current
        /// assignment and can no longer reach hardware.
        available: bool,
    },
    /// Opens the Audio Connections window. Never a routing change.
    OpenAudioConnections,
}

impl OutputChoice {
    /// Text shown for this entry.
    pub fn label(&self) -> String {
        match self {
            Self::NoOutput => NO_OUTPUT_LABEL.to_string(),
            Self::FollowMaster => FOLLOW_MASTER_LABEL.to_string(),
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

    /// The connection this entry would assign, if any.
    pub fn connection_id(&self) -> Option<&AudioConnectionId> {
        match self {
            Self::Connection { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// True when a connection may be *offered* as an output destination.
///
/// Deliberately looser than "will produce sound": a disabled or unplugged bus is
/// hidden from the list, but an assignment already pointing at one is kept and
/// shown as unavailable rather than remapped.
fn is_offerable_output(connection: &AudioConnection) -> bool {
    connection.direction == AudioConnectionDirection::Output && connection.enabled
}

/// True when a connection can actually carry audio right now.
pub fn is_output_usable(registry: &AudioConnectionRegistry, id: &AudioConnectionId) -> bool {
    registry
        .get(id)
        .is_some_and(|connection| is_offerable_output(connection) && connection.status.is_usable())
}

/// Shared body of both selectors: enabled outputs, then the current assignment
/// if it did not make that list, then the escape hatch.
fn output_entries(
    registry: &AudioConnectionRegistry,
    current: Option<&AudioConnectionId>,
    mut entries: Vec<OutputChoice>,
) -> Vec<OutputChoice> {
    let mut listed_current = false;
    for connection in registry.by_direction(AudioConnectionDirection::Output) {
        if !connection.enabled {
            continue;
        }
        if Some(&connection.id) == current {
            listed_current = true;
        }
        entries.push(OutputChoice::Connection {
            id: connection.id.clone(),
            label: connection.name.clone(),
            available: connection.status.is_usable(),
        });
    }

    // A stale or disabled assignment stays visible and stays selected. Dropping
    // it from the list would make the selector show something the project does
    // not actually hold.
    if let Some(current) = current {
        if !listed_current {
            let label = registry
                .name_of(current)
                .map(str::to_string)
                .unwrap_or_else(|| current.as_str().to_string());
            entries.push(OutputChoice::Connection {
                id: current.clone(),
                label,
                available: false,
            });
        }
    }

    entries.push(OutputChoice::OpenAudioConnections);
    entries
}

/// Entries for the Master Output selector.
pub fn master_output_options(
    registry: &AudioConnectionRegistry,
    current: Option<&AudioConnectionId>,
) -> Vec<OutputChoice> {
    output_entries(registry, current, vec![OutputChoice::NoOutput])
}

/// Entries for the Monitor / Control Room Output selector.
pub fn monitor_output_options(
    registry: &AudioConnectionRegistry,
    current: Option<&AudioConnectionId>,
) -> Vec<OutputChoice> {
    output_entries(registry, current, vec![OutputChoice::FollowMaster])
}

/// Label for the Master Output chip.
pub fn master_output_label(
    registry: &AudioConnectionRegistry,
    master: Option<&AudioConnectionId>,
) -> String {
    let Some(id) = master else {
        return NO_OUTPUT_LABEL.to_string();
    };
    let Some(connection) = registry.get(id) else {
        // The bus is gone but the id is kept: say so instead of inventing a
        // destination.
        return format!("{} — {UNAVAILABLE_SUFFIX}", id.as_str());
    };
    if is_offerable_output(connection) && connection.status.is_usable() {
        connection.name.clone()
    } else {
        format!("{} — {UNAVAILABLE_SUFFIX}", connection.name)
    }
}

/// Label for the Monitor Output chip.
///
/// `None` here is never "Unassigned": it means Follow Master Output, and when
/// Master itself has no output that fact is spelled out rather than hidden.
pub fn monitor_output_label(
    registry: &AudioConnectionRegistry,
    master: Option<&AudioConnectionId>,
    monitor: Option<&AudioConnectionId>,
) -> String {
    match monitor {
        Some(id) => master_output_label(registry, Some(id)),
        None => match master {
            None => format!("{FOLLOW_MASTER_LABEL} — No Master Output"),
            Some(_) => FOLLOW_MASTER_LABEL.to_string(),
        },
    }
}

/// The Output Audio Connection the Control Room actually feeds.
///
/// The override wins; otherwise Monitor follows Master. There is deliberately no
/// third fallback — when both are `None` the monitoring path is silent.
pub fn effective_monitor_output(
    master: Option<&AudioConnectionId>,
    monitor: Option<&AudioConnectionId>,
) -> Option<AudioConnectionId> {
    monitor.or(master).cloned()
}

// ── Menu command encoding ───────────────────────────────────────────────────

/// Command id prefix for the Master Output menu.
pub const MASTER_OUTPUT_COMMAND_PREFIX: &str = "mixer:set-master-output:";
/// Command id prefix for the Monitor Output menu.
pub const MONITOR_OUTPUT_COMMAND_PREFIX: &str = "mixer:set-monitor-output:";
/// Suffix meaning "clear the assignment" — No Output for Master, Follow Master
/// Output for Monitor.
const CLEAR_SUFFIX: &str = "none";
/// Suffix meaning "open the Audio Connections window", not a routing change.
const OPEN_SUFFIX: &str = "open";

/// Menu command id for one choice.
pub fn output_command_id(prefix: &str, choice: &OutputChoice) -> String {
    match choice {
        OutputChoice::NoOutput | OutputChoice::FollowMaster => format!("{prefix}{CLEAR_SUFFIX}"),
        OutputChoice::Connection { id, .. } => format!("{prefix}id:{}", id.as_str()),
        OutputChoice::OpenAudioConnections => format!("{prefix}{OPEN_SUFFIX}"),
    }
}

/// What a Master/Monitor output command asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputCommand {
    /// No Output (Master) / Follow Master Output (Monitor).
    Clear,
    Assign(AudioConnectionId),
    OpenAudioConnections,
}

/// Decode a command id produced by [`output_command_id`].
pub fn parse_output_command(prefix: &str, command_id: &str) -> Option<OutputCommand> {
    let rest = command_id.strip_prefix(prefix)?;
    if rest == CLEAR_SUFFIX {
        return Some(OutputCommand::Clear);
    }
    if rest == OPEN_SUFFIX {
        return Some(OutputCommand::OpenAudioConnections);
    }
    rest.strip_prefix("id:")
        .filter(|id| !id.is_empty())
        .map(|id| OutputCommand::Assign(AudioConnectionId::from_stored(id)))
}

/// Whether `choice` is the one currently in effect, for the menu check mark.
pub fn is_current_choice(choice: &OutputChoice, current: Option<&AudioConnectionId>) -> bool {
    match choice {
        OutputChoice::NoOutput | OutputChoice::FollowMaster => current.is_none(),
        OutputChoice::Connection { id, .. } => current == Some(id),
        OutputChoice::OpenAudioConnections => false,
    }
}

// ── Compatibility bootstrap ─────────────────────────────────────────────────

/// What the one-time Master output bootstrap did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputBootstrap {
    /// The project already had output routing initialized; nothing was done.
    AlreadyInitialized,
    /// Exactly one valid Output connection existed and was assigned to Master.
    AssignedExisting(AudioConnectionId),
    /// No Output connection existed, so "Main Output" was created from the
    /// current valid output pair and assigned to Master.
    CreatedMainOutput(AudioConnectionId),
    /// Nothing usable exists. Master stays unassigned and the caller warns.
    NoValidOutput,
    /// More than one valid Output connection exists — picking one would be a
    /// guess, so Master stays unassigned.
    Ambiguous,
}

impl OutputBootstrap {
    /// The id Master should be set to, if the bootstrap chose one.
    pub fn assigned(&self) -> Option<&AudioConnectionId> {
        match self {
            Self::AssignedExisting(id) | Self::CreatedMainOutput(id) => Some(id),
            _ => None,
        }
    }

    /// Structured warning text when the project cannot be given an output.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::NoValidOutput => Some(
                "No valid output audio connection is available; Master has no output. \
                 Assign one in Audio Connections."
                    .to_string(),
            ),
            Self::Ambiguous => Some(
                "Several output audio connections are available; Master was left unassigned. \
                 Choose one in Audio Connections."
                    .to_string(),
            ),
            _ => None,
        }
    }
}

/// Give a project that has never initialized output routing a Master output.
///
/// Runs at most once per project (`already_initialized` is the persisted latch),
/// so a deliberately deleted output is never recreated on the next launch. It
/// only ever *assigns*; it never repoints an assignment the user already made.
pub fn bootstrap_master_output(
    registry: &mut AudioConnectionRegistry,
    ports: &AvailablePorts,
    already_initialized: bool,
) -> OutputBootstrap {
    if already_initialized {
        return OutputBootstrap::AlreadyInitialized;
    }

    let valid: Vec<AudioConnectionId> = registry
        .by_direction(AudioConnectionDirection::Output)
        .into_iter()
        .filter(|connection| is_offerable_output(connection) && connection.status.is_usable())
        .map(|connection| connection.id.clone())
        .collect();
    match valid.len() {
        1 => return OutputBootstrap::AssignedExisting(valid[0].clone()),
        0 => {}
        _ => return OutputBootstrap::Ambiguous,
    }

    // No valid output bus. Build "Main Output" over the first output device that
    // exposes a usable pair — this is a *new* logical bus, not a hidden
    // hardware fallback: it is visible and editable in Audio Connections.
    let Some(device_id) = first_stereo_output_device(ports) else {
        return OutputBootstrap::NoValidOutput;
    };
    let id = registry.add(
        AudioConnection::new(
            "Main Output",
            AudioConnectionDirection::Output,
            ChannelLayout::Stereo,
        )
        .bind_consecutive(&device_id, 0, |index| format!("Output {}", index + 1)),
    );
    registry.revalidate(ports);
    if registry
        .get(&id)
        .map(|connection| connection.status)
        .unwrap_or(AudioConnectionStatus::Disconnected)
        .is_usable()
    {
        OutputBootstrap::CreatedMainOutput(id)
    } else {
        // The pair collided with an existing bus or vanished between the scan
        // and the bind. Leave the row for the user rather than assigning a
        // destination that cannot carry audio.
        OutputBootstrap::NoValidOutput
    }
}

/// First device exposing at least two playback ports.
fn first_stereo_output_device(ports: &AvailablePorts) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    for port in &ports.ports {
        if port.direction != AudioConnectionDirection::Output {
            continue;
        }
        if seen.contains(&port.device_id.as_str()) {
            continue;
        }
        seen.push(port.device_id.as_str());
        if ports
            .ports_for(&port.device_id, AudioConnectionDirection::Output)
            .len()
            >= 2
        {
            return Some(port.device_id.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_connections::AudioConnection;

    fn device() -> AvailablePorts {
        AvailablePorts::for_device("dev-1", "System Audio Device", 4, 4)
    }

    fn registry() -> AudioConnectionRegistry {
        AudioConnectionRegistry::default_template(&device(), "dev-1")
    }

    fn output_id(registry: &AudioConnectionRegistry, name: &str) -> AudioConnectionId {
        registry
            .by_direction(AudioConnectionDirection::Output)
            .into_iter()
            .find(|c| c.name == name)
            .expect("output connection")
            .id
            .clone()
    }

    fn labels(options: &[OutputChoice]) -> Vec<String> {
        options.iter().map(OutputChoice::label).collect()
    }

    // ── Listing ─────────────────────────────────────────────────────────────

    #[test]
    fn master_lists_only_output_connections() {
        let registry = registry();
        let options = master_output_options(&registry, None);
        assert_eq!(options.first(), Some(&OutputChoice::NoOutput));
        assert_eq!(
            options.last(),
            Some(&OutputChoice::OpenAudioConnections),
            "the escape hatch is always last"
        );
        let listed: Vec<&AudioConnectionId> = options
            .iter()
            .filter_map(OutputChoice::connection_id)
            .collect();
        assert_eq!(listed.len(), 1);
        for id in listed {
            assert_eq!(
                registry.get(id).unwrap().direction,
                AudioConnectionDirection::Output,
                "an input bus must never be offered as an output destination"
            );
        }
    }

    #[test]
    fn monitor_lists_only_output_connections_and_leads_with_follow_master() {
        let registry = registry();
        let options = monitor_output_options(&registry, None);
        assert_eq!(options.first(), Some(&OutputChoice::FollowMaster));
        assert_eq!(options.last(), Some(&OutputChoice::OpenAudioConnections));
        for id in options.iter().filter_map(OutputChoice::connection_id) {
            assert_eq!(
                registry.get(id).unwrap().direction,
                AudioConnectionDirection::Output
            );
        }
    }

    #[test]
    fn a_disabled_output_is_not_offered() {
        let ports = device();
        let mut registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        registry.set_enabled(&main, false);
        registry.revalidate(&ports);

        let options = master_output_options(&registry, None);
        assert!(options
            .iter()
            .filter_map(OutputChoice::connection_id)
            .all(|id| *id != main));
    }

    // ── Preservation ────────────────────────────────────────────────────────

    #[test]
    fn an_unavailable_assignment_is_preserved_and_labelled_rather_than_remapped() {
        let mut registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        let headphones = registry.add(
            AudioConnection::new(
                "Headphones",
                AudioConnectionDirection::Output,
                ChannelLayout::Stereo,
            )
            .bind_consecutive("dev-1", 2, |i| format!("Output {}", i + 1)),
        );
        // The device disappears entirely.
        registry.revalidate(&AvailablePorts::default());

        assert_eq!(
            master_output_label(&registry, Some(&main)),
            "Main Output 1-2 — Unavailable"
        );
        let options = master_output_options(&registry, Some(&main));
        assert!(labels(&options).contains(&"Main Output 1-2 — Unavailable".to_string()));
        assert!(
            options
                .iter()
                .filter_map(OutputChoice::connection_id)
                .any(|id| *id == headphones),
            "other outputs stay listed; nothing is auto-selected"
        );
    }

    #[test]
    fn a_disabled_assignment_stays_selected_even_though_it_is_not_offered() {
        let ports = device();
        let mut registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        registry.set_enabled(&main, false);
        registry.revalidate(&ports);

        let options = master_output_options(&registry, Some(&main));
        let entry = options
            .iter()
            .find(|choice| choice.connection_id() == Some(&main))
            .expect("the current assignment is always visible");
        assert_eq!(entry.label(), "Main Output 1-2 — Unavailable");
    }

    #[test]
    fn renaming_a_connection_changes_the_label_but_not_the_id() {
        let ports = device();
        let mut registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        assert_eq!(
            master_output_label(&registry, Some(&main)),
            "Main Output 1-2"
        );

        registry.rename(&main, "Studio Monitors");
        registry.revalidate(&ports);

        assert_eq!(
            master_output_label(&registry, Some(&main)),
            "Studio Monitors"
        );
        assert_eq!(
            output_id(&registry, "Studio Monitors"),
            main,
            "the id is unchanged by a rename"
        );
    }

    // ── Effective resolution ────────────────────────────────────────────────

    #[test]
    fn monitor_none_means_follow_master_not_unassigned() {
        let registry = registry();
        let main = output_id(&registry, "Main Output 1-2");

        assert_eq!(
            monitor_output_label(&registry, Some(&main), None),
            FOLLOW_MASTER_LABEL
        );
        assert_eq!(
            effective_monitor_output(Some(&main), None),
            Some(main.clone())
        );
    }

    #[test]
    fn monitor_following_a_master_without_an_output_says_so_and_stays_silent() {
        let registry = registry();
        assert_eq!(
            monitor_output_label(&registry, None, None),
            "Follow Master Output — No Master Output"
        );
        assert_eq!(effective_monitor_output(None, None), None);
    }

    #[test]
    fn a_monitor_override_is_independent_of_master() {
        let mut registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        let headphones = registry.add(
            AudioConnection::new(
                "Headphones",
                AudioConnectionDirection::Output,
                ChannelLayout::Stereo,
            )
            .bind_consecutive("dev-1", 2, |i| format!("Output {}", i + 1)),
        );
        registry.revalidate(&device());

        assert_eq!(
            effective_monitor_output(Some(&main), Some(&headphones)),
            Some(headphones.clone())
        );
        assert_eq!(
            monitor_output_label(&registry, Some(&main), Some(&headphones)),
            "Headphones"
        );
    }

    // ── Menu commands ───────────────────────────────────────────────────────

    #[test]
    fn every_choice_round_trips_through_its_menu_command() {
        let registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        for (prefix, options) in [
            (
                MASTER_OUTPUT_COMMAND_PREFIX,
                master_output_options(&registry, Some(&main)),
            ),
            (
                MONITOR_OUTPUT_COMMAND_PREFIX,
                monitor_output_options(&registry, Some(&main)),
            ),
        ] {
            for choice in &options {
                let command = output_command_id(prefix, choice);
                let parsed = parse_output_command(prefix, &command).expect("round trip");
                match choice {
                    OutputChoice::NoOutput | OutputChoice::FollowMaster => {
                        assert_eq!(parsed, OutputCommand::Clear)
                    }
                    OutputChoice::Connection { id, .. } => {
                        assert_eq!(parsed, OutputCommand::Assign(id.clone()))
                    }
                    OutputChoice::OpenAudioConnections => {
                        assert_eq!(parsed, OutputCommand::OpenAudioConnections)
                    }
                }
            }
        }
    }

    #[test]
    fn a_command_for_the_other_selector_is_not_accepted() {
        let command = format!("{MASTER_OUTPUT_COMMAND_PREFIX}none");
        assert!(parse_output_command(MONITOR_OUTPUT_COMMAND_PREFIX, &command).is_none());
    }

    #[test]
    fn the_check_mark_follows_the_stored_assignment() {
        let registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        let options = monitor_output_options(&registry, None);

        assert!(
            is_current_choice(&options[0], None),
            "Follow Master is current"
        );
        let connection = options
            .iter()
            .find(|choice| choice.connection_id() == Some(&main))
            .unwrap();
        assert!(!is_current_choice(connection, None));
        assert!(is_current_choice(connection, Some(&main)));
        assert!(!is_current_choice(
            &OutputChoice::OpenAudioConnections,
            None
        ));
    }

    // ── Bootstrap ───────────────────────────────────────────────────────────

    #[test]
    fn bootstrap_assigns_the_single_valid_output() {
        let ports = device();
        let mut registry = registry();
        registry.revalidate(&ports);
        let main = output_id(&registry, "Main Output 1-2");

        let result = bootstrap_master_output(&mut registry, &ports, false);
        assert_eq!(result, OutputBootstrap::AssignedExisting(main));
        assert!(result.warning().is_none());
    }

    #[test]
    fn bootstrap_creates_main_output_when_none_exists() {
        let ports = device();
        let mut registry = AudioConnectionRegistry::new();

        let result = bootstrap_master_output(&mut registry, &ports, false);
        let id = result.assigned().expect("an output was created").clone();
        assert!(matches!(result, OutputBootstrap::CreatedMainOutput(_)));
        let created = registry.get(&id).unwrap();
        assert_eq!(created.name, "Main Output");
        assert_eq!(created.direction, AudioConnectionDirection::Output);
        assert_eq!(created.status, AudioConnectionStatus::Active);
    }

    #[test]
    fn bootstrap_without_any_valid_output_leaves_master_unassigned_and_warns() {
        let mut registry = AudioConnectionRegistry::new();
        let result = bootstrap_master_output(&mut registry, &AvailablePorts::default(), false);
        assert_eq!(result, OutputBootstrap::NoValidOutput);
        assert!(result.assigned().is_none());
        assert!(result.warning().is_some());
        assert!(registry.is_empty(), "no hidden fallback bus is created");
    }

    /// The latch is what stops a deleted default from coming back every launch.
    #[test]
    fn bootstrap_runs_once_only() {
        let ports = device();
        let mut registry = AudioConnectionRegistry::new();
        let first = bootstrap_master_output(&mut registry, &ports, false);
        assert!(first.assigned().is_some());
        let before = registry.len();

        let second = bootstrap_master_output(&mut registry, &ports, true);
        assert_eq!(second, OutputBootstrap::AlreadyInitialized);
        assert!(second.assigned().is_none());
        assert_eq!(registry.len(), before, "no second bus is created");
    }

    #[test]
    fn bootstrap_does_not_guess_between_several_valid_outputs() {
        let ports = device();
        let mut registry = registry();
        registry.add(
            AudioConnection::new(
                "Headphones",
                AudioConnectionDirection::Output,
                ChannelLayout::Stereo,
            )
            .bind_consecutive("dev-1", 2, |i| format!("Output {}", i + 1)),
        );
        registry.revalidate(&ports);

        let result = bootstrap_master_output(&mut registry, &ports, false);
        assert_eq!(result, OutputBootstrap::Ambiguous);
        assert!(result.warning().is_some());
    }
}
