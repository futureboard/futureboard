//! Audio Connections panel — the editor for the project's logical audio buses.
//!
//! The panel owns **no** routing data. Every row is read from the project's
//! [`AudioConnectionRegistry`] each render, and every edit goes through the
//! registry's structured mutation API. The only state here is transient view
//! state: which tab is showing, which rows are selected, whether an inline
//! editor or a dropdown is open, and any pending destructive confirmation.
//!
//! That split is what keeps a stale connection id from surviving a project
//! switch — [`AudioConnectionsPanelState::on_project_changed`] clears the
//! transient state and nothing else needs resetting.

use crate::audio_connections::{
    AudioConnectionDirection, AudioConnectionId, AudioConnectionRegistry, AudioConnectionStatus,
    ChannelLayout,
};

/// Which tab the panel is showing. Session state; never persisted in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionsTab {
    #[default]
    Inputs,
    Outputs,
}

impl ConnectionsTab {
    pub fn direction(self) -> AudioConnectionDirection {
        match self {
            Self::Inputs => AudioConnectionDirection::Input,
            Self::Outputs => AudioConnectionDirection::Output,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inputs => "Inputs",
            Self::Outputs => "Outputs",
        }
    }
}

/// An open inline cell editor. Only the name column is inline-editable in
/// Turn C; the rest are dropdowns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineNameEdit {
    pub connection_id: AudioConnectionId,
    pub draft: String,
}

/// Which dropdown is open, so only one can be at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenDropdown {
    Layout(AudioConnectionId),
    Device(AudioConnectionId),
    Port {
        connection_id: AudioConnectionId,
        logical_channel: usize,
    },
    AddBus,
}

/// A destructive action awaiting confirmation, with everything the dialog
/// needs to describe the consequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingConfirmation {
    Remove {
        connection_id: AudioConnectionId,
        connection_name: String,
        /// Tracks whose input would become unassigned.
        affected_tracks: Vec<String>,
    },
    ResetDefaults {
        direction: AudioConnectionDirection,
        /// Tracks that reference any bus in this direction.
        affected_tracks: Vec<String>,
    },
}

impl PendingConfirmation {
    /// Whether this action would unassign anything, so the caller can skip the
    /// dialog for a harmless removal.
    pub fn is_destructive(&self) -> bool {
        match self {
            Self::Remove {
                affected_tracks, ..
            } => !affected_tracks.is_empty(),
            Self::ResetDefaults { .. } => true,
        }
    }
}

/// One rendered table row, derived from the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRow {
    pub id: AudioConnectionId,
    pub name: String,
    pub enabled: bool,
    pub layout: ChannelLayout,
    pub device_label: String,
    /// Left / Mono cell text.
    pub left_port: String,
    /// Right cell text. `None` for a mono bus, so the column renders empty
    /// rather than showing a meaningless dash.
    pub right_port: Option<String>,
    pub status: AudioConnectionStatus,
    pub status_detail: Option<String>,
    pub selected: bool,
}

/// Transient panel state.
#[derive(Debug, Clone, Default)]
pub struct AudioConnectionsPanelState {
    pub tab: ConnectionsTab,
    selected: Vec<AudioConnectionId>,
    pub inline_edit: Option<InlineNameEdit>,
    pub open_dropdown: Option<OpenDropdown>,
    pub pending: Option<PendingConfirmation>,
    /// Warnings from the most recent mutation, shown in the footer.
    pub warnings: Vec<String>,
}

impl AudioConnectionsPanelState {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Tabs ────────────────────────────────────────────────────────────────

    /// Switch tabs. Selection is dropped because ids from the other direction
    /// are meaningless here, and any open editor is closed so a committed edit
    /// cannot land on a row the user can no longer see.
    pub fn set_tab(&mut self, tab: ConnectionsTab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.selected.clear();
        self.close_transient();
    }

    /// Close every editor, dropdown, and pending dialog.
    pub fn close_transient(&mut self) {
        self.inline_edit = None;
        self.open_dropdown = None;
        self.pending = None;
    }

    /// Reset for a different project. Ids never cross projects.
    pub fn on_project_changed(&mut self) {
        self.selected.clear();
        self.close_transient();
        self.warnings.clear();
    }

    // ── Selection ───────────────────────────────────────────────────────────

    pub fn selected(&self) -> &[AudioConnectionId] {
        &self.selected
    }

    pub fn is_selected(&self, id: &AudioConnectionId) -> bool {
        self.selected.contains(id)
    }

    pub fn select_only(&mut self, id: AudioConnectionId) {
        self.selected = vec![id];
    }

    pub fn toggle_selected(&mut self, id: AudioConnectionId) {
        match self.selected.iter().position(|existing| *existing == id) {
            Some(index) => {
                self.selected.remove(index);
            }
            None => self.selected.push(id),
        }
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// The single selected connection, when exactly one is selected. Toolbar
    /// actions that only make sense for one row use this.
    pub fn single_selection(&self) -> Option<&AudioConnectionId> {
        match self.selected.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Drop selections that no longer exist — after a removal or a project
    /// reload.
    pub fn prune_selection(&mut self, registry: &AudioConnectionRegistry) {
        self.selected.retain(|id| registry.get(id).is_some());
        if let Some(edit) = &self.inline_edit {
            if registry.get(&edit.connection_id).is_none() {
                self.inline_edit = None;
            }
        }
    }

    /// Move the selection up or down within the current tab's rows.
    pub fn move_selection(&mut self, registry: &AudioConnectionRegistry, delta: i32) {
        let rows: Vec<AudioConnectionId> = registry
            .by_direction(self.tab.direction())
            .into_iter()
            .map(|connection| connection.id.clone())
            .collect();
        if rows.is_empty() {
            return;
        }
        let current = self
            .single_selection()
            .and_then(|id| rows.iter().position(|row| row == id));
        let next = match current {
            Some(index) => (index as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
            // No selection: Down lands on the first row, Up on the last.
            None if delta >= 0 => 0,
            None => rows.len() - 1,
        };
        self.select_only(rows[next].clone());
    }

    // ── Inline rename ───────────────────────────────────────────────────────

    pub fn begin_rename(&mut self, registry: &AudioConnectionRegistry, id: &AudioConnectionId) {
        let Some(connection) = registry.get(id) else {
            return;
        };
        self.open_dropdown = None;
        self.inline_edit = Some(InlineNameEdit {
            connection_id: id.clone(),
            draft: connection.name.clone(),
        });
    }

    pub fn update_rename_draft(&mut self, draft: impl Into<String>) {
        if let Some(edit) = self.inline_edit.as_mut() {
            edit.draft = draft.into();
        }
    }

    pub fn cancel_rename(&mut self) {
        self.inline_edit = None;
    }

    /// Commit the open rename. Returns the `(id, draft)` for the caller to
    /// push through `registry.update_name`, or `None` when nothing is open.
    pub fn take_rename(&mut self) -> Option<(AudioConnectionId, String)> {
        self.inline_edit
            .take()
            .map(|edit| (edit.connection_id, edit.draft))
    }

    // ── Dropdowns ───────────────────────────────────────────────────────────

    pub fn toggle_dropdown(&mut self, dropdown: OpenDropdown) {
        // Opening a dropdown ends any inline edit, so a stale draft can never
        // be committed by a later click elsewhere.
        self.inline_edit = None;
        self.open_dropdown = if self.open_dropdown.as_ref() == Some(&dropdown) {
            None
        } else {
            Some(dropdown)
        };
    }

    pub fn close_dropdown(&mut self) {
        self.open_dropdown = None;
    }

    pub fn is_dropdown_open(&self, dropdown: &OpenDropdown) -> bool {
        self.open_dropdown.as_ref() == Some(dropdown)
    }

    // ── Warnings ────────────────────────────────────────────────────────────

    pub fn set_warnings(&mut self, warnings: Vec<String>) {
        self.warnings = warnings;
    }

    // ── Rows ────────────────────────────────────────────────────────────────

    /// Build the rows for the active tab.
    pub fn rows(&self, registry: &AudioConnectionRegistry) -> Vec<ConnectionRow> {
        registry
            .by_direction(self.tab.direction())
            .into_iter()
            .map(|connection| {
                let is_stereo = connection.channel_layout.channel_count() > 1;
                let port_text = |logical: usize| {
                    connection
                        .binding(logical)
                        .map(|binding| binding.physical_port_id.port_name.clone())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "—".to_string())
                };
                ConnectionRow {
                    id: connection.id.clone(),
                    name: connection.name.clone(),
                    enabled: connection.enabled,
                    layout: connection.channel_layout,
                    device_label: connection
                        .device_id
                        .clone()
                        .unwrap_or_else(|| "Unassigned".to_string()),
                    left_port: port_text(0),
                    right_port: is_stereo.then(|| port_text(1)),
                    status: connection.status,
                    status_detail: status_detail(connection.status),
                    selected: self.is_selected(&connection.id),
                }
            })
            .collect()
    }

    /// Footer summary: `(row count, count of rows needing attention)`.
    pub fn summary(&self, registry: &AudioConnectionRegistry) -> (usize, usize) {
        let rows = registry.by_direction(self.tab.direction());
        let attention = rows
            .iter()
            .filter(|connection| {
                !matches!(
                    connection.status,
                    AudioConnectionStatus::Active | AudioConnectionStatus::Disabled
                )
            })
            .count();
        (rows.len(), attention)
    }
}

/// Longer explanation for a status, shown as the cell tooltip. `None` when the
/// short label already says everything.
pub fn status_detail(status: AudioConnectionStatus) -> Option<String> {
    match status {
        AudioConnectionStatus::Active => None,
        AudioConnectionStatus::DeviceMissing => {
            Some("The assigned audio device is not currently available.".to_string())
        }
        AudioConnectionStatus::PortMissing => {
            Some("The device is available but an assigned port is not.".to_string())
        }
        AudioConnectionStatus::Disconnected => {
            Some("No physical ports are assigned yet.".to_string())
        }
        AudioConnectionStatus::Disabled => {
            Some("Disabled. Mappings are kept and this bus resolves to silence.".to_string())
        }
        AudioConnectionStatus::Conflict => Some(
            "This mapping collides with another assignment. Left and Right must differ, and two \
             outputs must not write the same port."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_connections::{AudioConnection, AvailablePorts};

    fn ports() -> AvailablePorts {
        AvailablePorts::for_device("dev-1", "Interface", 4, 4)
    }

    fn registry() -> AudioConnectionRegistry {
        AudioConnectionRegistry::default_template(&ports(), "dev-1")
    }

    // ── Tabs ────────────────────────────────────────────────────────────────

    #[test]
    fn the_inputs_tab_shows_only_input_connections() {
        let registry = registry();
        let panel = AudioConnectionsPanelState::new();
        assert_eq!(panel.tab, ConnectionsTab::Inputs);
        let rows = panel.rows(&registry);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.name.contains("Input")));
    }

    #[test]
    fn the_outputs_tab_shows_only_output_connections() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        panel.set_tab(ConnectionsTab::Outputs);
        let rows = panel.rows(&registry);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Main Output 1-2");
    }

    #[test]
    fn switching_tabs_clears_selection_and_open_editors() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();
        panel.select_only(id.clone());
        panel.begin_rename(&registry, &id);
        panel.toggle_dropdown(OpenDropdown::Device(id));

        panel.set_tab(ConnectionsTab::Outputs);
        assert!(panel.selected().is_empty());
        assert!(panel.inline_edit.is_none());
        assert!(panel.open_dropdown.is_none());
    }

    // ── Rows ────────────────────────────────────────────────────────────────

    #[test]
    fn a_mono_row_has_no_right_port_cell_and_a_stereo_row_does() {
        let registry = registry();
        let panel = AudioConnectionsPanelState::new();
        let rows = panel.rows(&registry);

        let mono = rows
            .iter()
            .find(|row| row.layout == ChannelLayout::Mono)
            .unwrap();
        assert!(mono.right_port.is_none());
        assert_eq!(mono.left_port, "Input 1");

        let stereo = rows
            .iter()
            .find(|row| row.layout == ChannelLayout::Stereo)
            .unwrap();
        assert_eq!(stereo.left_port, "Input 1");
        assert_eq!(stereo.right_port.as_deref(), Some("Input 2"));
    }

    #[test]
    fn the_footer_counts_rows_needing_attention() {
        let mut registry = registry();
        let panel = AudioConnectionsPanelState::new();
        assert_eq!(panel.summary(&registry), (3, 0));

        registry.revalidate(&AvailablePorts::default());
        let (rows, attention) = panel.summary(&registry);
        assert_eq!(rows, 3);
        assert_eq!(attention, 3, "every row is DeviceMissing");
    }

    // ── Selection / keyboard ────────────────────────────────────────────────

    #[test]
    fn arrow_selection_walks_the_active_tab_and_clamps_at_the_ends() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let rows: Vec<_> = registry
            .by_direction(AudioConnectionDirection::Input)
            .into_iter()
            .map(|c| c.id.clone())
            .collect();

        panel.move_selection(&registry, 1);
        assert_eq!(panel.single_selection(), Some(&rows[0]));
        panel.move_selection(&registry, 1);
        assert_eq!(panel.single_selection(), Some(&rows[1]));
        panel.move_selection(&registry, -1);
        assert_eq!(panel.single_selection(), Some(&rows[0]));
        // Already at the top — stays put rather than wrapping.
        panel.move_selection(&registry, -1);
        assert_eq!(panel.single_selection(), Some(&rows[0]));
    }

    #[test]
    fn removed_rows_are_pruned_from_the_selection_and_editor() {
        let mut registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();
        panel.select_only(id.clone());
        panel.begin_rename(&registry, &id);

        registry.remove(&id);
        panel.prune_selection(&registry);
        assert!(panel.selected().is_empty());
        assert!(panel.inline_edit.is_none());
    }

    #[test]
    fn single_selection_is_none_when_several_rows_are_selected() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let rows = registry.by_direction(AudioConnectionDirection::Input);
        panel.select_only(rows[0].id.clone());
        panel.toggle_selected(rows[1].id.clone());
        assert_eq!(panel.selected().len(), 2);
        assert!(panel.single_selection().is_none());

        panel.toggle_selected(rows[1].id.clone());
        assert_eq!(panel.single_selection(), Some(&rows[0].id));
    }

    // ── Inline rename ───────────────────────────────────────────────────────

    #[test]
    fn a_committed_rename_hands_back_the_draft_and_keeps_the_id() {
        let mut registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();

        panel.begin_rename(&registry, &id);
        panel.update_rename_draft("  Microphone  ");
        let (edited_id, draft) = panel.take_rename().expect("rename open");
        assert_eq!(edited_id, id);

        let mutation = registry.update_name(&edited_id, &draft);
        assert_eq!(registry.name_of(&id), Some("Microphone"), "trimmed");
        assert!(
            !mutation.needs_routing_rebuild,
            "a name change must not rebuild routing"
        );
        assert!(registry.get(&id).is_some(), "the id is unchanged");
    }

    #[test]
    fn cancelling_a_rename_discards_the_draft() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();
        panel.begin_rename(&registry, &id);
        panel.update_rename_draft("Discarded");
        panel.cancel_rename();
        assert!(panel.take_rename().is_none());
    }

    #[test]
    fn opening_a_dropdown_ends_an_inline_edit() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();
        panel.begin_rename(&registry, &id);
        panel.toggle_dropdown(OpenDropdown::Layout(id));
        assert!(
            panel.inline_edit.is_none(),
            "a stale draft must not survive to be committed by a later click"
        );
    }

    #[test]
    fn only_one_dropdown_is_open_at_a_time() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();
        panel.toggle_dropdown(OpenDropdown::Layout(id.clone()));
        assert!(panel.is_dropdown_open(&OpenDropdown::Layout(id.clone())));

        panel.toggle_dropdown(OpenDropdown::Device(id.clone()));
        assert!(!panel.is_dropdown_open(&OpenDropdown::Layout(id.clone())));
        assert!(panel.is_dropdown_open(&OpenDropdown::Device(id.clone())));

        // Toggling the same one closes it.
        panel.toggle_dropdown(OpenDropdown::Device(id));
        assert!(panel.open_dropdown.is_none());
    }

    // ── Project switching ───────────────────────────────────────────────────

    #[test]
    fn switching_projects_drops_every_reference_to_the_old_one() {
        let registry = registry();
        let mut panel = AudioConnectionsPanelState::new();
        let id = registry.by_direction(AudioConnectionDirection::Input)[0]
            .id
            .clone();
        panel.select_only(id.clone());
        panel.begin_rename(&registry, &id);
        panel.pending = Some(PendingConfirmation::Remove {
            connection_id: id,
            connection_name: "x".to_string(),
            affected_tracks: vec!["track-1".to_string()],
        });
        panel.set_warnings(vec!["stale".to_string()]);

        panel.on_project_changed();
        assert!(panel.selected().is_empty());
        assert!(panel.inline_edit.is_none());
        assert!(panel.pending.is_none());
        assert!(panel.warnings.is_empty());
    }

    // ── Confirmation ────────────────────────────────────────────────────────

    #[test]
    fn removing_an_unreferenced_connection_needs_no_confirmation() {
        let confirmation = PendingConfirmation::Remove {
            connection_id: AudioConnectionId::from_stored("ac-1"),
            connection_name: "Spare".to_string(),
            affected_tracks: Vec::new(),
        };
        assert!(!confirmation.is_destructive());
    }

    #[test]
    fn removing_a_referenced_connection_is_destructive() {
        let confirmation = PendingConfirmation::Remove {
            connection_id: AudioConnectionId::from_stored("ac-1"),
            connection_name: "Microphone".to_string(),
            affected_tracks: vec!["track-1".to_string(), "track-2".to_string()],
        };
        assert!(confirmation.is_destructive());
    }

    #[test]
    fn reset_defaults_always_confirms() {
        assert!(PendingConfirmation::ResetDefaults {
            direction: AudioConnectionDirection::Input,
            affected_tracks: Vec::new(),
        }
        .is_destructive());
    }

    // ── Status presentation ─────────────────────────────────────────────────

    #[test]
    fn every_non_active_status_has_an_explanation() {
        for status in [
            AudioConnectionStatus::DeviceMissing,
            AudioConnectionStatus::PortMissing,
            AudioConnectionStatus::Disconnected,
            AudioConnectionStatus::Disabled,
            AudioConnectionStatus::Conflict,
        ] {
            assert!(
                status_detail(status).is_some(),
                "{status:?} needs a tooltip"
            );
            assert!(!status.label().is_empty());
        }
        assert!(status_detail(AudioConnectionStatus::Active).is_none());
    }

    /// A connection loaded from a project with no device and no bindings must
    /// render without panicking.
    #[test]
    fn a_malformed_connection_renders_safely() {
        let mut registry = AudioConnectionRegistry::new();
        registry.add(AudioConnection::new(
            "Broken",
            AudioConnectionDirection::Input,
            ChannelLayout::Stereo,
        ));
        registry.revalidate(&AvailablePorts::default());

        let panel = AudioConnectionsPanelState::new();
        let rows = panel.rows(&registry);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].left_port, "—");
        assert_eq!(rows[0].right_port.as_deref(), Some("—"));
        assert_eq!(rows[0].device_label, "Unassigned");
        assert_eq!(rows[0].status, AudioConnectionStatus::Disconnected);
    }
}
