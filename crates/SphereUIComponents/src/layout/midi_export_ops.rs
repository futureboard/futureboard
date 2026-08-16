//! StudioLayout integration for MIDI file export.
//!
//! Two entry points, both a straight Save-As rather than a dialog window: unlike
//! an audio render there is nothing to configure — no format, quality, or
//! sample rate — so an extra window would only add a click.
//!
//!   * `file:export-midi`  — the arrangement (or the active loop span).
//!   * `midi:export-clip`  — one MIDI clip, from the MIDI editor or the
//!     arrangement's clip context menu.
//!
//! The export model is built under a short UI borrow and the bytes are
//! serialized from that owned snapshot, so no timeline entity is held across
//! the file dialog await.

use std::sync::Arc;

use gpui::{Bounds, Context};

use super::StudioLayout;
use crate::components::midi_export_dialog::{
    open_midi_export_dialog, MidiExportDialogSetup, MidiExportTrackChoice,
};
use crate::components::timeline::midi_export::{
    build_arrangement_export, build_clip_export, default_export_options, exportable_tracks,
    loop_export_range, MidiExport, MidiExportOptions,
};

impl StudioLayout {
    /// File ▸ Export MIDI File… — opens the options dialog; the export itself
    /// runs when the user confirms.
    pub(super) fn open_export_midi_dialog(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        // Focus an already-open dialog rather than spawning a second one.
        if let Some(handle) = self.external_windows.export_midi.clone() {
            if handle
                .update(cx, |_w, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.external_windows.export_midi = None;
        }
        self.dismiss_menus_for_export();

        // Snapshot under a short borrow: the dialog holds no timeline entity.
        let (options, tracks, loop_range) = {
            let state = &self.timeline.read(cx).state;
            (
                default_export_options(state),
                exportable_tracks(state),
                loop_export_range(state),
            )
        };
        if tracks.is_empty() {
            eprintln!("[MidiExport] arrangement has no MIDI content to export");
        }

        let setup = MidiExportDialogSetup {
            options,
            tracks: tracks
                .into_iter()
                .map(|(id, name)| MidiExportTrackChoice {
                    id,
                    name,
                    selected: true,
                })
                .collect(),
            has_loop_range: loop_range.is_some(),
            loop_range_label: loop_range.map(|range| {
                format!(
                    "beats {:.0}–{:.0}",
                    range.start_beats + 1.0,
                    range.end_beats + 1.0
                )
            }),
        };

        let this = cx.entity().clone();
        let on_export = Arc::new(
            move |options: MidiExportOptions, _window: &mut gpui::Window, cx: &mut gpui::App| {
                let _ = this.update(cx, |layout, cx| {
                    layout.external_windows.export_midi = None;
                    layout.run_arrangement_midi_export(options, cx);
                });
            },
        );
        let closed = cx.entity().clone();
        let on_close = Arc::new(move |_window: &mut gpui::Window, cx: &mut gpui::App| {
            let _ = closed.update(cx, |layout, _cx| {
                layout.external_windows.export_midi = None;
            });
        });

        let owner_bounds = crate::window_position::resolve_owner_bounds_with_preferred(
            owner_bounds,
            self.studio_window_bounds(cx),
            cx,
        );
        match open_midi_export_dialog(owner_bounds, setup, on_export, on_close, cx) {
            Ok(handle) => self.external_windows.export_midi = Some(handle),
            Err(err) => eprintln!("[MidiExport] failed to open export dialog: {err}"),
        }
    }

    /// Build and save with the options the dialog returned.
    fn run_arrangement_midi_export(&mut self, options: MidiExportOptions, cx: &mut Context<Self>) {
        let project_name = self.project_session.name.clone();
        let export = {
            let state = &self.timeline.read(cx).state;
            build_arrangement_export(state, &project_name, &options)
        };
        if export.tracks.is_empty() {
            eprintln!("[MidiExport] nothing selected to export");
        }
        self.save_midi_export(export, sanitize_file_stem(&project_name), cx);
    }

    /// Export one MIDI clip as a standalone file, its notes starting at bar 1.
    ///
    /// Resolves the clip the same way the rest of the clip commands do: the
    /// context-menu target in the arrangement, otherwise the selection — which
    /// is exactly the clip the MIDI editor is showing.
    pub(super) fn export_midi_clip_file(&mut self, cx: &mut Context<Self>) {
        self.dismiss_menus_for_export();
        let Some(clip_id) = self.context_clip_id_or_selected(cx) else {
            eprintln!("[MidiExport] no clip selected to export");
            return;
        };
        let project_name = self.project_session.name.clone();
        let built = {
            let state = &self.timeline.read(cx).state;
            let clip_name = state
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .find(|clip| clip.id == clip_id)
                .map(|clip| clip.name.clone())
                .unwrap_or_else(|| project_name.clone());
            build_clip_export(state, &clip_id, &clip_name).map(|export| (export, clip_name))
        };
        let Some((export, clip_name)) = built else {
            // The command is offered only for MIDI clips, so this means the
            // selection moved between opening the menu and choosing the item.
            eprintln!("[MidiExport] clip {clip_id} is not a MIDI clip");
            return;
        };
        self.save_midi_export(export, sanitize_file_stem(&clip_name), cx);
    }

    fn dismiss_menus_for_export(&mut self) {
        self.menu_bar.open_menu_id = None;
        self.menu_bar.submenu_path.clear();
        self.overlay.open_popover = None;
        self.overlay.text_context_menu = None;
    }

    /// Ask for a path, then write the serialized file.
    #[cfg(feature = "native-dialogs")]
    fn save_midi_export(
        &mut self,
        export: MidiExport,
        default_stem: String,
        cx: &mut Context<Self>,
    ) {
        // Default next to the project's other exports when it lives on disk.
        let start_dir = self.project_session.folder_path.as_ref().map(|root| {
            let exports = root.join("Exports");
            let _ = std::fs::create_dir_all(&exports);
            exports
        });
        let file_name = format!("{default_stem}.mid");

        cx.spawn(async move |_this, _cx| {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Export MIDI File")
                .set_file_name(&file_name)
                .add_filter("MIDI File", &["mid", "midi"]);
            if let Some(dir) = start_dir {
                dialog = dialog.set_directory(dir);
            }
            let Some(handle) = dialog.save_file().await else {
                return; // user cancelled
            };
            let path = handle.path().to_path_buf();
            // Serialize off the UI borrow — `export` is owned data by now.
            let bytes = export.to_smf_bytes();
            match std::fs::write(&path, &bytes) {
                Ok(()) => eprintln!(
                    "[MidiExport] wrote {} ({} bytes, {} track(s))",
                    path.display(),
                    bytes.len(),
                    export.tracks.len()
                ),
                Err(error) => {
                    eprintln!(
                        "[MidiExport] write failed path={} err={error}",
                        path.display()
                    );
                    // A user-initiated export that silently does nothing is
                    // worse than an extra dialog.
                    rfd::AsyncMessageDialog::new()
                        .set_level(rfd::MessageLevel::Error)
                        .set_title("Export MIDI File")
                        .set_description(format!("Could not write {}:\n{error}", path.display()))
                        .show()
                        .await;
                }
            }
        })
        .detach();
    }

    #[cfg(not(feature = "native-dialogs"))]
    fn save_midi_export(
        &mut self,
        _export: MidiExport,
        _default_stem: String,
        _cx: &mut Context<Self>,
    ) {
        eprintln!("[MidiExport] native file dialogs are disabled in this build");
    }
}

/// Strip characters that are illegal in file names so the default export path is
/// always valid. Mirrors the audio exporter's helper.
fn sanitize_file_stem(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if stem.is_empty() {
        "Export".to_string()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_file_stem;

    #[test]
    fn illegal_path_characters_are_replaced() {
        assert_eq!(sanitize_file_stem("My: Song?"), "My_ Song_");
        assert_eq!(sanitize_file_stem("  "), "Export");
        assert_eq!(sanitize_file_stem("Fine Name"), "Fine Name");
    }
}
