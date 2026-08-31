//! Routes the bottom editor panel between AudioEditor, MidiEditor, and empty state.

use gpui::{div, px, Context, Entity, IntoElement, ParentElement, Render, Styled, Window};
use sphere_audio_editor::{editor_kind_for_clip, ClipEditorKind};

use crate::components::ara_editor_host::AraEditorHost;
use crate::components::audio_editor_adapter::{audio_editor_theme, clip_type_hint_for_selection};
use crate::components::audio_editor_host::AudioEditorHost;
use crate::components::piano_roll::PianoRoll;
use crate::components::solfege_editor::SolfegeEditorPanel;
use crate::components::timeline::timeline::Timeline;
use crate::components::timeline::timeline_state::ClipType;
use crate::theme::Colors;

pub struct ClipEditorPanel {
    timeline: Entity<Timeline>,
    piano_roll: Entity<PianoRoll>,
    solfege_editor: Entity<SolfegeEditorPanel>,
    audio_editor: Entity<AudioEditorHost>,
    ara_editor: Entity<AraEditorHost>,
    /// Last branch reported by the trace, so the choice is logged on change
    /// rather than every frame.
    traced: Option<&'static str>,
}

impl ClipEditorPanel {
    pub fn new(
        timeline: Entity<Timeline>,
        piano_roll: Entity<PianoRoll>,
        solfege_editor: Entity<SolfegeEditorPanel>,
        audio_editor: Entity<AudioEditorHost>,
        ara_editor: Entity<AraEditorHost>,
    ) -> Self {
        Self {
            timeline,
            piano_roll,
            solfege_editor,
            audio_editor,
            ara_editor,
            traced: None,
        }
    }

    /// Reports which editor the tab resolved to, once per change.
    fn trace(&mut self, branch: &'static str) {
        if self.traced == Some(branch) {
            return;
        }
        if std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").is_some() {
            eprintln!("[ara-panel] editor tab -> {branch}");
        }
        self.traced = Some(branch);
    }

    /// Whether the selected clip sits on a track an ARA plug-in is processing.
    ///
    /// Not part of [`ClipEditorKind`]: ARA is a track processor, not a clip
    /// type, and `sphere_audio_editor` knows nothing about plug-ins.
    fn ara_selected(&self, cx: &Context<Self>) -> bool {
        let state = &self.timeline.read(cx).state;
        let Some(clip_id) = state.selection.selected_clip_ids.first() else {
            return false;
        };
        state
            .find_clip(clip_id)
            .is_some_and(|(track, _)| track.ara.is_some())
    }

    fn current_kind(&self, cx: &Context<Self>) -> ClipEditorKind {
        let hint = clip_type_hint_for_selection(&self.timeline.read(cx).state);
        editor_kind_for_clip(hint)
    }

    fn solfege_selected(&self, cx: &Context<Self>) -> bool {
        let state = &self.timeline.read(cx).state;
        let Some(clip_id) = state.selection.selected_clip_ids.first() else {
            return false;
        };
        state.find_clip(clip_id).is_some_and(|(track, clip)| {
            track.solfege.is_some() && matches!(&clip.clip_type, ClipType::Midi { .. })
        })
    }
}

impl Render for ClipEditorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.current_kind(cx) {
            ClipEditorKind::Audio if self.ara_selected(cx) => {
                self.trace("ara");
                self.ara_editor.clone().into_any_element()
            }
            ClipEditorKind::Audio => {
                self.trace("audio");
                self.audio_editor.clone().into_any_element()
            }
            ClipEditorKind::Midi if self.solfege_selected(cx) => {
                self.trace("solfege");
                self.solfege_editor.clone().into_any_element()
            }
            ClipEditorKind::Midi => {
                self.trace("midi");
                self.piano_roll.clone().into_any_element()
            }
            ClipEditorKind::Empty => {
                self.trace("empty");
                empty_editor_panel().into_any_element()
            }
        }
    }
}

fn empty_editor_panel() -> impl IntoElement {
    let theme = audio_editor_theme();
    div()
        .flex()
        .items_center()
        .justify_center()
        .size_full()
        .bg(Colors::surface_base())
        .text_size(px(11.0))
        .text_color(theme.text_muted)
        .child("Select a clip to edit")
}
