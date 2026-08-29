//! The Solfege instrument editor: two primary tabs, `MIDI` and `Pitch`.
//!
//! ```text
//! [MIDI] [Pitch]
//! ```
//!
//! - **MIDI** is the note / performance editor. It hosts the DAW's real
//!   [`PianoRoll`] and stacks compact performance lanes under it. Articulation,
//!   velocity, dynamics, expression, and instrument gestures are *lanes and note
//!   properties here* — never separate editor pages.
//! - **Pitch** is a dedicated large surface for continuous pitch performance:
//!   micro pitch, portamento, scoops, falls, and vibrato shape.
//!
//! Both tabs read and write the same [`MidiNoteState`] objects on the same clip
//! through the same [`EditCommand`] history, and both render through the piano
//! roll's [`PianoRollViewport`], so there is exactly one timeline viewport,
//! one scroll owner, and no duplicated note data.
//!
//! Engine configuration (instrument, model, quality) is not here — it lives in
//! the Inspector (`components::panel::solfege_panel`).

mod accent_command;
pub(crate) mod lanes;
mod pitch;

use gpui::{
    div, px, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window,
};

use crate::components::piano_roll::{PianoRoll, PianoRollViewport};
use crate::components::timeline::timeline::Timeline;
use crate::components::timeline::timeline_state::ClipType;
use crate::solfege::{InstrumentCapabilities, SolfegeTrackState};
use crate::theme::Colors;

pub use accent_command::AccentAnalysisState;
pub use lanes::SolfegeLaneStack;
pub use pitch::{PitchEditorState, PitchTool};

/// The only two primary editor tabs. Articulation, velocity, expression,
/// dynamics, vibrato, bow, and breath are deliberately **not** tabs — they are
/// lanes inside [`SolfegeEditorTab::Midi`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SolfegeEditorTab {
    #[default]
    Midi,
    Pitch,
}

impl SolfegeEditorTab {
    pub const ALL: [SolfegeEditorTab; 2] = [SolfegeEditorTab::Midi, SolfegeEditorTab::Pitch];

    pub fn label(self) -> &'static str {
        match self {
            Self::Midi => "MIDI",
            Self::Pitch => "Pitch",
        }
    }
}

/// Pitch facts about one note, shaped for the Inspector.
///
/// Deliberately a small value type rather than a borrow of editor state: the
/// Inspector is a family of free functions over borrowed project state, and
/// this keeps it that way.
#[derive(Debug, Clone, PartialEq)]
pub struct SolfegePitchSummary {
    /// Scientific pitch name of the note's notated pitch, e.g. `"D4"`.
    pub name: String,
    /// Project-timeline start, in beats.
    pub start_beats: f32,
    pub length_beats: f32,
    /// Cent deviation at the note start.
    pub deviation_cents: f32,
    /// Lowest and highest cent deviation across the note.
    pub range_cents: (f32, f32),
    /// Number of manual pitch breakpoints.
    pub point_count: usize,
    pub articulation: Option<&'static str>,
}

/// Everything the editor needs about the clip it is editing, resolved once per
/// frame so lane and pitch rendering never re-walk the track list.
#[derive(Debug, Clone)]
pub(crate) struct SolfegeEditContext {
    pub track_id: String,
    pub clip_id: String,
    /// Where the clip sits on the arrangement timeline. Clip-local beats plus
    /// this is a project beat, which is what the transport speaks.
    pub clip_start_beat: f32,
    /// Clip length in beats — the horizontal extent every surface shares.
    pub clip_beats: f32,
    pub solfege: SolfegeTrackState,
    pub capabilities: InstrumentCapabilities,
}

pub struct SolfegeEditorPanel {
    timeline: Entity<Timeline>,
    /// The DAW's MIDI note editor. Also the single owner of the shared
    /// horizontal viewport (zoom + scroll) for both tabs.
    piano_roll: Entity<PianoRoll>,
    active_tab: SolfegeEditorTab,
    lanes: SolfegeLaneStack,
    pitch: PitchEditorState,
    /// State of the last or current Analyze Accent pass. Transient UI state,
    /// not project data: the accents it produces live on the notes.
    accent: AccentAnalysisState,
    /// Whether the Analyze Accent options popover is open.
    accent_menu_open: bool,
}

impl SolfegeEditorPanel {
    pub fn new(
        timeline: Entity<Timeline>,
        piano_roll: Entity<PianoRoll>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            timeline,
            piano_roll,
            active_tab: SolfegeEditorTab::default(),
            lanes: SolfegeLaneStack::default(),
            pitch: PitchEditorState::new(cx),
            accent: AccentAnalysisState::default(),
            accent_menu_open: false,
        }
    }

    pub fn active_tab(&self) -> SolfegeEditorTab {
        self.active_tab
    }

    /// Switch tabs. Both tabs share the viewport and the note data, so this is
    /// purely which surface is on screen.
    pub fn set_active_tab(&mut self, tab: SolfegeEditorTab, cx: &mut Context<Self>) {
        if self.active_tab != tab {
            self.active_tab = tab;
            self.lanes.close_menus();
            cx.notify();
        }
    }

    /// Resolve the selected Solfege MIDI clip. `None` when the selection is not
    /// a MIDI clip on a Solfege track — the panel then shows the plain grid.
    pub(crate) fn edit_context(&self, cx: &Context<Self>) -> Option<SolfegeEditContext> {
        let state = &self.timeline.read(cx).state;
        let clip_id = state.selection.selected_clip_ids.first()?;
        let (track, clip) = state.find_clip(clip_id)?;
        let solfege = track.solfege.clone()?;
        if !matches!(&clip.clip_type, ClipType::Midi { .. }) {
            return None;
        }
        Some(SolfegeEditContext {
            track_id: track.id.clone(),
            clip_id: clip.id.clone(),
            clip_start_beat: clip.start_beat,
            clip_beats: clip.duration_beats.max(1.0),
            capabilities: solfege.capabilities(),
            solfege,
        })
    }

    /// `true` while the Pitch tab's canvas holds keyboard focus.
    ///
    /// The studio's capture-phase shortcut router checks this before dispatching
    /// the `edit:*` family globally: without it, Delete would delete the
    /// selected arrangement clip instead of the selected pitch points. This is
    /// the same focus gate the docked piano roll already gets.
    pub fn pitch_grid_is_focused(&self, window: &gpui::Window) -> bool {
        self.active_tab == SolfegeEditorTab::Pitch && self.pitch.is_focused(window)
    }

    /// Concise pitch facts about the note the Pitch tab is editing, for the
    /// Inspector. `None` unless the Pitch tab is open with a note selected, so
    /// the dock never shows stale note data while the MIDI tab is in front.
    ///
    /// Read through `&App` rather than `&Context<Self>` so the studio layout —
    /// which only holds the entity — can call it during its own render.
    pub fn selected_pitch_summary(&self, cx: &gpui::App) -> Option<SolfegePitchSummary> {
        if self.active_tab != SolfegeEditorTab::Pitch {
            return None;
        }
        let note_id = self.pitch.selected_note()?;
        let state = &self.timeline.read(cx).state;
        let clip_id = state.selection.selected_clip_ids.first()?;
        let (_, clip) = state.find_clip(clip_id)?;
        let note = state.midi_note(clip_id, note_id)?;
        let curve = note.pitch_curve.as_ref();
        let (low, high) = curve
            .map(|curve| {
                curve.points.iter().fold((0.0f32, 0.0f32), |(lo, hi), p| {
                    (lo.min(p.cents), hi.max(p.cents))
                })
            })
            .unwrap_or((0.0, 0.0));
        Some(SolfegePitchSummary {
            name: crate::components::piano_roll::note_name(note.pitch as i32),
            start_beats: clip.start_beat + note.start,
            length_beats: note.duration,
            deviation_cents: curve.map(|curve| curve.cents_at(0.0)).unwrap_or(0.0),
            range_cents: (low, high),
            point_count: curve.map(|curve| curve.len()).unwrap_or(0),
            articulation: note.articulation.map(|a| a.name()),
        })
    }

    /// The shared MIDI timeline viewport, owned by the piano roll.
    pub(crate) fn viewport(&self, cx: &Context<Self>) -> PianoRollViewport {
        self.piano_roll.read(cx).viewport()
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div()
            .id("solfege-editor-tabs")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(3.0))
            .px(px(6.0))
            .h(px(26.0))
            .flex_none()
            .border_b(px(1.0))
            .border_color(Colors::border_subtle())
            .bg(Colors::surface_titlebar());
        for (index, tab) in SolfegeEditorTab::ALL.into_iter().enumerate() {
            let active = self.active_tab == tab;
            row = row.child(
                div()
                    .id(("solfege-editor-tab", index))
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(18.0))
                    .px(px(10.0))
                    .rounded(px(crate::theme::radius::CONTROL))
                    .bg(if active {
                        Colors::accent_muted()
                    } else {
                        Colors::surface_input()
                    })
                    .text_size(px(10.0))
                    .text_color(if active {
                        Colors::text_primary()
                    } else {
                        Colors::text_muted()
                    })
                    .cursor(gpui::CursorStyle::PointingHand)
                    .hover(|style| style.bg(Colors::surface_hover()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        this.set_active_tab(tab, cx);
                    }))
                    .child(tab.label()),
            );
        }
        row
    }
}

impl Render for SolfegeEditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tab_bar(cx);
        let context = self.edit_context(cx);
        let body = match self.active_tab {
            SolfegeEditorTab::Midi => self.render_midi_tab(context, cx).into_any_element(),
            SolfegeEditorTab::Pitch => self
                .render_pitch_tab(context, window, cx)
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(Colors::surface_base())
            .child(tabs)
            .child(body)
    }
}

impl SolfegeEditorPanel {
    /// MIDI tab: the piano roll takes the majority of the viewport, with the
    /// visible performance lanes stacked compactly beneath it.
    fn render_midi_tab(
        &mut self,
        context: Option<SolfegeEditContext>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lane_stack = context
            .as_ref()
            .map(|ctx| self.render_lane_stack(ctx, cx).into_any_element());
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.0))
            .child(
                // The musical canvas stays visible with or without notes: the
                // piano roll draws ruler, keys and grid either way, plus the
                // empty-clip hint naming the gesture that creates a note.
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(self.piano_roll.clone()),
            )
            .children(lane_stack)
    }
}
