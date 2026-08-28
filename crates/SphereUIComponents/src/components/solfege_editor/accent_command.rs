//! The Analyze Accent command: request, background pass, one undo entry.
//!
//! ```text
//! editor  ──request──>  background worker  ──snapshot──>  editor
//!                       (features, rule,                  (apply + record
//!                        FBMX correction)                   one EditMidiNotes)
//! ```
//!
//! Nothing about the analysis runs on the render thread. The model is read from
//! disk on the worker, the whole-phrase GRU pass runs there, and what comes back
//! is a plain `Vec<AccentState>` the editor applies inside one `Timeline::update`
//! — so a five-hundred-note clip cannot stall a frame, and a slow model load
//! cannot stall one either.
//!
//! ## Scope, and why the analysis is always whole-clip
//!
//! A selection changes *what is written*, never *what is read*. Accent is a
//! contextual judgement — where the note sits in its bar, whether it is the peak
//! of its phrase, what follows it — so analysing a three-note selection in
//! isolation would give those notes a phrase of three notes and a metrical
//! reading with no bar before it. The pass always sees the whole clip; the
//! selection filters which notes take the result.
//!
//! ## One undo entry
//!
//! The command applies to a snapshot of the clip's notes and records exactly one
//! [`EditCommand::EditMidiNotes`], so two hundred analysed notes are one Ctrl+Z
//! and not two hundred. An analysis that changes nothing records no entry at
//! all, so re-running it on an unchanged clip does not litter the history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use gpui::Context;

use crate::components::edit::edit_commands::EditCommand;
use crate::components::timeline::timeline_state::{AccentState, MidiControllerKind, MidiNoteState};
use crate::solfege::accent::{
    apply_accents, apply_to_notes, dynamics_contour, dynamics_lane_is_writable,
    AccentAnalysisStats, AccentAnalyzer, Meter,
};
use crate::solfege::AccentReplacePolicy;

use super::{SolfegeEditContext, SolfegeEditorPanel};

/// The controller the Dynamics lane writes.
///
/// CC 1, which `solfege_acoustic::VoicebankRenderer::control_change` acts on by
/// calling `set_dynamic` — so a contour written here reaches the sounding voice
/// rather than being discarded like the gesture controllers this instrument
/// does not read.
const DYNAMICS_CONTROLLER: MidiControllerKind = MidiControllerKind::CC(1);

/// What the editor is showing about the last or current analysis.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum AccentAnalysisState {
    #[default]
    Idle,
    /// A pass is in flight. Carries the note count so the status line says what
    /// is being analysed rather than showing an unlabelled spinner.
    Running {
        notes: usize,
    },
    Done {
        summary: String,
    },
    Failed {
        message: String,
    },
}

// There are deliberately no progress stages here.
//
// An earlier version of this file had three — preparing the phrase, loading the
// model, running it — and two of them could never fire, because the whole pass
// is one closure that finishes before anything could observe it partway
// through. Measured in release, a thousand notes analyse in **0.83 ms**; the
// only part that can take a visible moment is the first read of a model file,
// and packages that carry one are the exception rather than the rule.
//
// So the running state says what it is doing once and does not animate. Section
// 29 of the brief asks for real progress or none, and a three-stage readout for
// something already finished would be the fake kind.

/// Everything the worker needs, captured on the main thread.
///
/// A plain owned struct rather than a borrow of timeline state: the worker
/// outlives the `Timeline::update` that produced it, and the clip may be edited
/// while it runs. What comes back is matched against the clip by note id, so an
/// edit during the pass drops the stale notes rather than mis-assigning them.
struct AnalysisRequest {
    clip_id: String,
    notes: Vec<MidiNoteState>,
    clip_start_beat: f32,
    tempo_bpm: f32,
    meter: Meter,
    model_path: Option<PathBuf>,
}

struct AnalysisResult {
    clip_id: String,
    /// Analysed accent per note id, in clip order.
    accents: Vec<(u64, AccentState)>,
    stats: AccentAnalysisStats,
    elapsed_ms: f64,
}

/// Loaded accent analysers, keyed by the model file they came from.
///
/// A `.sfm` is 146 MB and its accent section is a few kilobytes, but finding
/// those kilobytes still means reading and verifying the container. Doing it
/// once per analysis would make the second Analyze Accent as slow as the first
/// for no reason. Keyed on path, length and mtime so replacing the model on
/// disk is picked up rather than served stale.
type CacheKey = (PathBuf, u64, Option<std::time::SystemTime>);
static ANALYZERS: OnceLock<Mutex<HashMap<CacheKey, AccentAnalyzer>>> = OnceLock::new();

fn cached_analyzer(path: &Path) -> AccentAnalyzer {
    let Ok(metadata) = std::fs::metadata(path) else {
        return AccentAnalyzer::rule_only();
    };
    let key = (path.to_path_buf(), metadata.len(), metadata.modified().ok());
    let cache = ANALYZERS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(found) = guard.get(&key) {
            return found.clone();
        }
    }
    let analyzer = load_analyzer(path);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, analyzer.clone());
    }
    analyzer
}

/// Read an accent model out of a Solfege package, or a loose `.fbmx`.
///
/// Blocking, and deliberately tolerant: a model that is missing, unreadable, or
/// built for a different feature schema leaves the rule analyser in place. The
/// instrument still plays and Analyze Accent still works — it just works the
/// way it works for an instrument that never shipped a model, which is a state
/// the feature has to support anyway.
fn load_analyzer(path: &Path) -> AccentAnalyzer {
    let Ok(bytes) = std::fs::read(path) else {
        return AccentAnalyzer::rule_only();
    };
    let is_sfm = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sfm"));
    if is_sfm {
        return solfege_model::SfmFile::from_bytes(&bytes)
            .ok()
            .and_then(|model| {
                model
                    .section(solfege_model::FBMX_ACCENT_TAG)
                    .map(AccentAnalyzer::from_fbmx)
            })
            .unwrap_or_else(AccentAnalyzer::rule_only);
    }
    AccentAnalyzer::from_fbmx(&bytes)
}

impl SolfegeEditorPanel {
    /// Whether an analysis is in flight, for the toolbar.
    pub fn accent_analysis(&self) -> &AccentAnalysisState {
        &self.accent
    }

    /// Run Analyze Accent over the selected clip.
    ///
    /// `policy` decides what happens to notes a person has already edited; the
    /// selection, if any, decides which notes take the result.
    pub fn analyze_accent(&mut self, policy: AccentReplacePolicy, cx: &mut Context<Self>) {
        if matches!(self.accent, AccentAnalysisState::Running { .. }) {
            return;
        }
        let Some(ctx) = self.edit_context(cx) else {
            self.accent = AccentAnalysisState::Failed {
                message: "Select a Solfege MIDI clip to analyse".to_string(),
            };
            cx.notify();
            return;
        };
        let Some(request) = self.build_request(&ctx, cx) else {
            self.accent = AccentAnalysisState::Failed {
                message: "This clip has no notes to analyse".to_string(),
            };
            cx.notify();
            return;
        };

        // Only the selection is *written*; the pass always reads the whole clip.
        let targets: Vec<u64> = self.piano_roll.read(cx).selected_note_ids();
        let notes = request.notes.len();
        self.accent = AccentAnalysisState::Running { notes };
        cx.notify();

        let task = cx.spawn(async move |panel, cx| {
            let executor = cx.background_executor().clone();
            let result = executor.spawn(async move { run_analysis(request) }).await;
            panel
                .update(cx, |panel, cx| {
                    panel.finish_analysis(result, &targets, policy, cx)
                })
                .ok();
        });
        // The task owns itself; dropping the handle would cancel the analysis
        // the moment this function returns.
        task.detach();
    }

    fn build_request(
        &self,
        ctx: &SolfegeEditContext,
        cx: &Context<Self>,
    ) -> Option<AnalysisRequest> {
        let timeline = self.timeline.read(cx);
        let notes = timeline.state.midi_clip_notes(&ctx.clip_id)?.clone();
        if notes.is_empty() {
            return None;
        }
        let signature = timeline
            .state
            .time_signature_map
            .time_signature_at_beat(ctx.clip_start_beat as f64);
        // The tempo where the clip starts, not the project's static BPM: a
        // project with a tempo map would otherwise analyse every clip as if it
        // were at the opening tempo.
        let tempo = timeline
            .state
            .tempo_map
            .bpm_at_beat(ctx.clip_start_beat as f64, timeline.state.bpm as f64)
            as f32;
        Some(AnalysisRequest {
            clip_id: ctx.clip_id.clone(),
            notes,
            clip_start_beat: ctx.clip_start_beat,
            tempo_bpm: tempo,
            meter: Meter::new(
                signature.numerator,
                signature.denominator,
                &signature.effective_grouping(),
            ),
            model_path: ctx.solfege.model_path.as_ref().map(PathBuf::from),
        })
    }

    /// Apply a finished pass and record it as one undoable edit.
    fn finish_analysis(
        &mut self,
        result: AnalysisResult,
        targets: &[u64],
        policy: AccentReplacePolicy,
        cx: &mut Context<Self>,
    ) {
        let clip_id = result.clip_id;
        let previous: Vec<MidiNoteState> = self
            .timeline
            .read(cx)
            .state
            .midi_clip_notes(&clip_id)
            .cloned()
            .unwrap_or_default();
        if previous.is_empty() {
            self.accent = AccentAnalysisState::Failed {
                message: "The clip changed while it was being analysed".to_string(),
            };
            cx.notify();
            return;
        }

        // Match by note id, not by position. A note added or deleted while the
        // pass ran would otherwise shift every accent after it onto the wrong
        // note — silently, and in a way that looks like a bad model.
        let by_id: HashMap<u64, AccentState> = result.accents.into_iter().collect();
        let mut next = previous.clone();
        let mut analysed: Vec<AccentState> = Vec::with_capacity(next.len());
        let mut selected: Vec<MidiNoteState> = Vec::new();
        for note in &next {
            analysed.push(
                by_id
                    .get(&note.id)
                    .copied()
                    .unwrap_or_else(|| note.accent.unwrap_or_else(AccentState::neutral)),
            );
        }
        let write_all = targets.is_empty();
        let mut writable: Vec<MidiNoteState> = Vec::new();
        let mut writable_accents: Vec<AccentState> = Vec::new();
        for (index, note) in next.iter().enumerate() {
            if !by_id.contains_key(&note.id) {
                continue;
            }
            if write_all || targets.contains(&note.id) {
                writable.push(note.clone());
                writable_accents.push(analysed[index]);
            }
        }
        let changed = apply_accents(&mut writable, &writable_accents, policy);
        for updated in writable {
            if let Some(slot) = next.iter_mut().find(|note| note.id == updated.id) {
                slot.accent = updated.accent;
            }
        }
        selected.clear();

        if changed == 0 {
            self.accent = AccentAnalysisState::Done {
                summary: format!(
                    "Accent unchanged — {} note{} already match the analysis",
                    previous.len(),
                    if previous.len() == 1 { "" } else { "s" }
                ),
            };
            cx.notify();
            return;
        }

        let clip = clip_id.clone();
        self.timeline.update(cx, |timeline, tcx| {
            timeline.state.overwrite_midi_notes(&clip, &next);
            timeline.record_executed_command(
                EditCommand::EditMidiNotes {
                    clip_id: clip.clone(),
                    prev: previous,
                    next: next.clone(),
                },
                tcx,
            );
        });

        let stats = result.stats;
        self.accent = AccentAnalysisState::Done {
            summary: format!(
                "Analysed {} note{} in {:.0} ms — {}, spread {:.0}%{}",
                changed,
                if changed == 1 { "" } else { "s" },
                result.elapsed_ms,
                if stats.used_model {
                    "trained model"
                } else {
                    "rule analyser"
                },
                stats.prominence_spread * 100.0,
                if write_all { "" } else { " (selection)" }
            ),
        };
        cx.notify();
    }
}

impl SolfegeEditorPanel {
    /// Apply the clip's accents to its performance.
    ///
    /// The second, explicit half of the workflow. Analyze Accent produces a
    /// reading; this acts on it — moving notes, revoicing them, and writing a
    /// dynamics contour — and it is a separate command because a tool that
    /// rewrote a musician's timings the moment it finished analysing would be a
    /// tool nobody could leave switched on.
    ///
    /// Synchronous. There is no model involved and no file to read: this is
    /// arithmetic over the clip's own notes, and on the largest clip the editor
    /// can hold it is far below a frame.
    pub fn apply_accent_to_performance(&mut self, cx: &mut Context<Self>) {
        let Some(ctx) = self.edit_context(cx) else {
            self.accent = AccentAnalysisState::Failed {
                message: "Select a Solfege MIDI clip".to_string(),
            };
            cx.notify();
            return;
        };

        let (previous, seconds_per_beat, existing_dynamics) = {
            let timeline = self.timeline.read(cx);
            let Some(notes) = timeline.state.midi_clip_notes(&ctx.clip_id).cloned() else {
                return;
            };
            (
                notes,
                timeline.state.seconds_per_beat(),
                timeline
                    .state
                    .controller_points_snapshot(&ctx.clip_id, DYNAMICS_CONTROLLER),
            )
        };
        if previous.iter().all(|note| note.accent.is_none()) {
            self.accent = AccentAnalysisState::Failed {
                message: "No accents to apply — run Analyze Accent first".to_string(),
            };
            cx.notify();
            return;
        }

        let mut next = previous.clone();
        let mut applied = apply_to_notes(&mut next, seconds_per_beat, ctx.clip_beats);

        // Accent is not Dynamics. A lane a musician has drawn into is theirs,
        // and this refuses it out loud rather than quietly.
        let writable = dynamics_lane_is_writable(&existing_dynamics);
        applied.dynamics_skipped = !writable;
        let mut point_id = 1u64;
        let contour = writable
            .then(|| dynamics_contour(&next, &mut point_id))
            .flatten();
        if let Some(points) = contour.as_ref() {
            applied.dynamics_points = points.len();
        }

        if !applied.changed_anything() {
            self.accent = AccentAnalysisState::Done {
                summary: if applied.dynamics_skipped {
                    "Nothing to apply — the Dynamics lane is hand-drawn and was left alone"
                        .to_string()
                } else {
                    "Nothing to apply — every accent is neutral".to_string()
                },
            };
            cx.notify();
            return;
        }

        let clip = ctx.clip_id.clone();
        let contour_for_update = contour.clone();
        self.timeline.update(cx, |timeline, tcx| {
            timeline.state.overwrite_midi_notes(&clip, &next);
            timeline.record_executed_command(
                EditCommand::EditMidiNotes {
                    clip_id: clip.clone(),
                    prev: previous,
                    next: next.clone(),
                },
                tcx,
            );
            if let Some(points) = contour_for_update {
                timeline.state.set_controller_lane_points(
                    &clip,
                    DYNAMICS_CONTROLLER,
                    points.clone(),
                );
                timeline.record_executed_command(
                    EditCommand::SetControllerPoints {
                        clip_id: clip.clone(),
                        kind: DYNAMICS_CONTROLLER,
                        prev: existing_dynamics,
                        next: points,
                    },
                    tcx,
                );
            }
        });

        self.accent = AccentAnalysisState::Done {
            summary: format!(
                "Applied — {} moved, {} revoiced, {} dynamics point{}{}",
                applied.notes_moved,
                applied.notes_revoiced,
                applied.dynamics_points,
                if applied.dynamics_points == 1 {
                    ""
                } else {
                    "s"
                },
                if applied.dynamics_skipped {
                    "; hand-drawn Dynamics left alone"
                } else {
                    ""
                }
            ),
        };
        cx.notify();
    }
}

fn run_analysis(request: AnalysisRequest) -> AnalysisResult {
    let started = Instant::now();
    let analyzer = request
        .model_path
        .as_deref()
        .map(cached_analyzer)
        .unwrap_or_else(AccentAnalyzer::rule_only);
    let (accents, stats) = analyzer.analyze(
        &request.notes,
        request.clip_start_beat,
        request.tempo_bpm,
        &request.meter,
    );
    AnalysisResult {
        clip_id: request.clip_id,
        accents: request
            .notes
            .iter()
            .map(|note| note.id)
            .zip(accents)
            .collect(),
        stats,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    }
}
