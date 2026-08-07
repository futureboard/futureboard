//! One project-level surface for routing warnings.
//!
//! Everything that can go wrong with Audio Connections — a missing input or
//! output device, a missing port, an unavailable Master or Monitor output,
//! overlapping hardware writes, and the legacy-project migration warnings —
//! lands here and is reported **once**, aggregated.
//!
//! Two rules shape this:
//!
//! * **One surface, not one dialog per track.** A project whose interface is
//!   unplugged can have every track affected; that is one condition, not forty
//!   modals. The summary names the count and the detail list stays available.
//! * **`eprintln!` is diagnostics, never the user-facing report.** Anything the
//!   user has to act on is in the aggregate; the log stays for developers.

use std::time::{Duration, Instant};

/// How long an aggregated warning summary stays in the status bar.
const WARNING_NOTICE_DURATION: Duration = Duration::from_secs(10);

/// What a routing warning is about. The kind decides how several of them
/// collapse into one sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RoutingWarningKind {
    /// A connection's input device is not present.
    InputDeviceMissing,
    /// A connection's output device is not present.
    OutputDeviceMissing,
    /// The device is present but a bound port is not.
    PortMissing,
    /// Two output buses resolve to the same physical port.
    OutputOverlap,
    /// Master references an output that cannot currently carry audio.
    MasterOutputUnavailable,
    /// The Monitor override cannot currently carry audio.
    MonitorOutputUnavailable,
    /// A legacy project's MIDI input could not be migrated cleanly.
    LegacyMidiConflict,
    /// A legacy project's audio routing could not be migrated cleanly.
    LegacyAudioMigration,
    /// Anything else the registry reported.
    Other,
}

impl RoutingWarningKind {
    /// Sentence naming `count` occurrences of this condition.
    pub fn summary(self, count: usize) -> String {
        match self {
            Self::InputDeviceMissing => {
                format!("{count} input connection(s) have a missing audio device")
            }
            Self::OutputDeviceMissing => {
                format!("{count} output connection(s) have a missing audio device")
            }
            Self::PortMissing => format!("{count} connection(s) have a missing port"),
            Self::OutputOverlap => {
                format!("{count} output connection(s) overlap on the same hardware port")
            }
            Self::MasterOutputUnavailable => "Master output is unavailable".to_string(),
            Self::MonitorOutputUnavailable => "Monitor output is unavailable".to_string(),
            Self::LegacyMidiConflict => {
                format!("{count} track(s) had conflicting legacy MIDI input")
            }
            Self::LegacyAudioMigration => {
                format!("{count} track(s) had legacy audio routing that could not be migrated")
            }
            Self::Other => format!("{count} routing warning(s)"),
        }
    }
}

/// One warning, with the detail text kept for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingWarning {
    pub kind: RoutingWarningKind,
    /// Full text, shown in the detail list rather than the summary line.
    pub detail: String,
}

impl RoutingWarning {
    pub fn new(kind: RoutingWarningKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Classify a registry warning string. The registry reports prose; this maps
    /// it onto a kind so several of them can collapse into one sentence.
    pub fn from_registry_text(text: impl Into<String>) -> Self {
        let detail = text.into();
        let lower = detail.to_ascii_lowercase();
        let kind = if lower.contains("overlap") || lower.contains("conflict") {
            RoutingWarningKind::OutputOverlap
        } else if lower.contains("port") && lower.contains("missing") {
            RoutingWarningKind::PortMissing
        } else if lower.contains("device") && lower.contains("missing") {
            if lower.contains("output") {
                RoutingWarningKind::OutputDeviceMissing
            } else {
                RoutingWarningKind::InputDeviceMissing
            }
        } else {
            RoutingWarningKind::Other
        };
        Self { kind, detail }
    }
}

/// The project's routing warning state: the current detail list plus the
/// aggregated line the status bar shows.
#[derive(Debug, Clone, Default)]
pub struct RoutingWarningState {
    /// Every warning from the most recent report, for diagnostics.
    pub details: Vec<RoutingWarning>,
    /// Aggregated summary line.
    pub summary: String,
    /// While set in the future, the status bar shows [`Self::summary`].
    pub notice_until: Option<Instant>,
}

impl RoutingWarningState {
    /// Replace the current report. Returns the summary shown, or `None` when
    /// there is nothing to report — which also clears any live notice, so a
    /// resolved condition stops being advertised.
    pub fn report(&mut self, warnings: Vec<RoutingWarning>) -> Option<String> {
        if warnings.is_empty() {
            self.details.clear();
            self.summary.clear();
            self.notice_until = None;
            return None;
        }
        let summary = aggregate_summary(&warnings);
        self.details = warnings;
        self.summary = summary.clone();
        self.notice_until = Some(Instant::now() + WARNING_NOTICE_DURATION);
        Some(summary)
    }

    /// The summary line while its notice window is open.
    pub fn active_notice(&self) -> Option<&str> {
        self.notice_until
            .filter(|until| *until > Instant::now())
            .map(|_| self.summary.as_str())
            .filter(|summary| !summary.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.details.is_empty()
    }
}

/// Collapse warnings into one sentence: one clause per *kind*, never one per
/// affected track.
pub fn aggregate_summary(warnings: &[RoutingWarning]) -> String {
    let mut kinds: Vec<RoutingWarningKind> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    for warning in warnings {
        match kinds.iter().position(|kind| *kind == warning.kind) {
            Some(index) => counts[index] += 1,
            None => {
                kinds.push(warning.kind);
                counts.push(1);
            }
        }
    }
    let mut clauses: Vec<(RoutingWarningKind, usize)> = kinds.into_iter().zip(counts).collect();
    // Stable order so the same condition always reads the same way.
    clauses.sort_by_key(|(kind, _)| *kind);
    clauses
        .into_iter()
        .map(|(kind, count)| kind.summary(count))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_tracks_hitting_one_condition_collapse_into_one_clause() {
        let warnings = vec![
            RoutingWarning::new(RoutingWarningKind::PortMissing, "Track A"),
            RoutingWarning::new(RoutingWarningKind::PortMissing, "Track B"),
            RoutingWarning::new(RoutingWarningKind::PortMissing, "Track C"),
        ];
        assert_eq!(
            aggregate_summary(&warnings),
            "3 connection(s) have a missing port",
            "one sentence for the condition, not one per track"
        );
    }

    #[test]
    fn distinct_conditions_each_get_one_clause_in_a_stable_order() {
        let warnings = vec![
            RoutingWarning::new(RoutingWarningKind::MasterOutputUnavailable, "Main Output"),
            RoutingWarning::new(RoutingWarningKind::InputDeviceMissing, "Mic"),
            RoutingWarning::new(RoutingWarningKind::InputDeviceMissing, "Guitar"),
        ];
        let summary = aggregate_summary(&warnings);
        assert_eq!(
            summary,
            "2 input connection(s) have a missing audio device; Master output is unavailable"
        );
        // Order is a property of the kind, not of arrival order.
        let reversed: Vec<RoutingWarning> = warnings.into_iter().rev().collect();
        assert_eq!(aggregate_summary(&reversed), summary);
    }

    #[test]
    fn reporting_keeps_the_details_for_diagnostics() {
        let mut state = RoutingWarningState::default();
        let summary = state
            .report(vec![
                RoutingWarning::new(RoutingWarningKind::PortMissing, "Mic: Input 3 missing"),
                RoutingWarning::new(RoutingWarningKind::PortMissing, "Guitar: Input 4 missing"),
            ])
            .expect("a summary");

        assert_eq!(summary, "2 connection(s) have a missing port");
        assert_eq!(state.details.len(), 2, "the full list stays available");
        assert!(state.details[0].detail.contains("Input 3"));
        assert!(state.active_notice().is_some());
    }

    /// A resolved condition must stop being advertised.
    #[test]
    fn reporting_nothing_clears_the_surface() {
        let mut state = RoutingWarningState::default();
        state.report(vec![RoutingWarning::new(
            RoutingWarningKind::PortMissing,
            "Mic",
        )]);
        assert!(!state.is_empty());

        assert!(state.report(Vec::new()).is_none());
        assert!(state.is_empty());
        assert!(state.active_notice().is_none());
        assert!(state.summary.is_empty());
    }

    #[test]
    fn registry_prose_is_classified_rather_than_shown_raw() {
        assert_eq!(
            RoutingWarning::from_registry_text("Mic: audio device missing").kind,
            RoutingWarningKind::InputDeviceMissing
        );
        assert_eq!(
            RoutingWarning::from_registry_text("Main Output: output device missing").kind,
            RoutingWarningKind::OutputDeviceMissing
        );
        assert_eq!(
            RoutingWarning::from_registry_text("Headphones: port missing").kind,
            RoutingWarningKind::PortMissing
        );
        assert_eq!(
            RoutingWarning::from_registry_text("Cue: conflict with Main Output").kind,
            RoutingWarningKind::OutputOverlap
        );
        assert_eq!(
            RoutingWarning::from_registry_text("something new").kind,
            RoutingWarningKind::Other
        );
    }

    /// The summary is one line regardless of how many things are wrong, so it
    /// can never become a wall of modals.
    #[test]
    fn a_project_where_everything_is_broken_still_reports_one_line() {
        let warnings: Vec<RoutingWarning> = (0..200)
            .map(|i| RoutingWarning::new(RoutingWarningKind::PortMissing, format!("Track {i}")))
            .collect();
        let summary = aggregate_summary(&warnings);
        assert!(!summary.contains('\n'));
        assert_eq!(summary, "200 connection(s) have a missing port");
    }
}
