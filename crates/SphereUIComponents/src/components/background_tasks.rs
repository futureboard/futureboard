use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    div, px, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, Window,
};

use crate::theme::Colors;

pub type BackgroundTaskToggleCb = Arc<dyn Fn(&(), &mut Window, &mut App) + 'static>;
pub type BackgroundTaskCancelCb = Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>;

const RECENT_COMPLETE_MS: u64 = 30_000;
const KEEP_COMPLETED: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackgroundTaskKind {
    Import,
    MediaCopy,
    MetadataScan,
    Waveform,
    PeakGeneration,
    PeakLoading,
    NativeSync,
    ProjectSave,
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskStatus {
    Queued,
    Running,
    Paused,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundTaskProgress {
    pub current: u32,
    pub total: u32,
}

#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub kind: BackgroundTaskKind,
    pub title: String,
    pub detail: Option<String>,
    pub status: BackgroundTaskStatus,
    pub progress: Option<BackgroundTaskProgress>,
    pub started_at: Option<u64>,
    pub updated_at: u64,
    pub error: Option<String>,
    pub cancellable: bool,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackgroundTaskUpdate {
    pub kind: BackgroundTaskKind,
    pub title: String,
    pub detail: Option<String>,
    pub status: BackgroundTaskStatus,
    pub progress: Option<BackgroundTaskProgress>,
    pub error: Option<String>,
    pub cancellable: bool,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BackgroundTaskStore {
    pub tasks: HashMap<String, BackgroundTask>,
    pub panel_open: bool,
}

#[derive(Debug, Clone)]
pub struct BackgroundTaskSummary {
    pub label: String,
    pub tone: BackgroundTaskTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskTone {
    Idle,
    Active,
    Info,
    Warning,
    Error,
}

impl BackgroundTaskStore {
    pub fn add_or_update(&mut self, id: impl Into<String>, update: BackgroundTaskUpdate) {
        let id = id.into();
        let now = now_ms();
        let started_at = if update.status == BackgroundTaskStatus::Running {
            self.tasks
                .get(&id)
                .and_then(|task| task.started_at)
                .or(Some(now))
        } else {
            self.tasks.get(&id).and_then(|task| task.started_at)
        };
        self.tasks.insert(
            id.clone(),
            BackgroundTask {
                id,
                kind: update.kind,
                title: update.title,
                detail: update.detail,
                status: update.status,
                progress: update.progress,
                started_at,
                updated_at: now,
                error: update.error,
                cancellable: update.cancellable,
                parent_id: update.parent_id,
            },
        );
        self.prune_completed();
    }

    pub fn complete(&mut self, id: &str, detail: Option<String>) {
        self.patch_status(id, BackgroundTaskStatus::Complete, detail, None);
    }

    pub fn fail(&mut self, id: &str, error: impl Into<String>) {
        self.patch_status(id, BackgroundTaskStatus::Failed, None, Some(error.into()));
    }

    pub fn cancel(&mut self, id: &str) {
        self.patch_status(id, BackgroundTaskStatus::Cancelled, None, None);
    }

    pub fn set_panel_open(&mut self, open: bool) {
        self.panel_open = open;
    }

    pub fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
    }

    pub fn sorted_tasks(&self) -> Vec<BackgroundTask> {
        let mut tasks = self.tasks.values().cloned().collect::<Vec<_>>();
        tasks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        tasks
    }

    pub fn summary(&self) -> BackgroundTaskSummary {
        let tasks = self.tasks.values().collect::<Vec<_>>();
        let failed = tasks
            .iter()
            .filter(|task| task.status == BackgroundTaskStatus::Failed)
            .count();
        if failed > 0 {
            return BackgroundTaskSummary {
                label: format!(
                    "{failed} background job{} failed",
                    if failed == 1 { "" } else { "s" }
                ),
                tone: BackgroundTaskTone::Error,
            };
        }
        if let Some(task) = tasks.iter().find(|task| {
            task.kind == BackgroundTaskKind::Import && task.status == BackgroundTaskStatus::Running
        }) {
            return BackgroundTaskSummary {
                label: progress_label("Importing audio", task),
                tone: BackgroundTaskTone::Active,
            };
        }
        if let Some(task) = tasks.iter().find(|task| {
            matches!(
                task.kind,
                BackgroundTaskKind::PeakGeneration | BackgroundTaskKind::Waveform
            ) && task.status == BackgroundTaskStatus::Running
        }) {
            return BackgroundTaskSummary {
                label: progress_label("Generating waveforms", task),
                tone: BackgroundTaskTone::Info,
            };
        }
        if tasks.iter().any(|task| {
            task.kind == BackgroundTaskKind::PeakLoading
                && task.status == BackgroundTaskStatus::Running
        }) {
            return BackgroundTaskSummary {
                label: "Loading peak chunks...".to_string(),
                tone: BackgroundTaskTone::Info,
            };
        }
        if let Some(task) = tasks.iter().find(|task| {
            matches!(
                task.kind,
                BackgroundTaskKind::NativeSync | BackgroundTaskKind::ProjectSave
            ) && matches!(
                task.status,
                BackgroundTaskStatus::Running | BackgroundTaskStatus::Queued
            )
        }) {
            return BackgroundTaskSummary {
                label: if task.kind == BackgroundTaskKind::ProjectSave {
                    "Saving project...".to_string()
                } else {
                    "Syncing native engine...".to_string()
                },
                tone: BackgroundTaskTone::Warning,
            };
        }
        BackgroundTaskSummary {
            label: "Background idle".to_string(),
            tone: BackgroundTaskTone::Idle,
        }
    }

    fn patch_status(
        &mut self,
        id: &str,
        status: BackgroundTaskStatus,
        detail: Option<String>,
        error: Option<String>,
    ) {
        let Some(task) = self.tasks.get_mut(id) else {
            return;
        };
        task.status = status;
        if detail.is_some() {
            task.detail = detail;
        }
        task.error = error;
        task.updated_at = now_ms();
        self.prune_completed();
    }

    fn prune_completed(&mut self) {
        let now = now_ms();
        let mut completed = self
            .tasks
            .values()
            .filter(|task| {
                matches!(
                    task.status,
                    BackgroundTaskStatus::Complete | BackgroundTaskStatus::Cancelled
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        completed.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let keep = completed
            .into_iter()
            .enumerate()
            .filter(|(index, task)| {
                *index < KEEP_COMPLETED || now.saturating_sub(task.updated_at) < RECENT_COMPLETE_MS
            })
            .map(|(_, task)| task.id)
            .collect::<std::collections::HashSet<_>>();
        self.tasks.retain(|id, task| {
            !matches!(
                task.status,
                BackgroundTaskStatus::Complete | BackgroundTaskStatus::Cancelled
            ) || keep.contains(id)
        });
    }
}

pub fn background_task_button(
    store: &BackgroundTaskStore,
    on_toggle: BackgroundTaskToggleCb,
) -> impl IntoElement {
    let summary = store.summary();
    let color = tone_color(summary.tone);
    div()
        .id("background-task-status")
        .h(px(18.0))
        .max_w(px(260.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(7.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(if store.panel_open {
            Colors::border_accent()
        } else {
            Colors::border_subtle()
        })
        .bg(if store.panel_open {
            Colors::accent_muted()
        } else {
            Colors::surface_input()
        })
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(Colors::surface_hover()))
        .on_click(move |_, w, cx| on_toggle(&(), w, cx))
        .child(
            div()
                .w(px(5.0))
                .h(px(5.0))
                .rounded(px(crate::theme::radius::PILL))
                .bg(color),
        )
        .child(
            div()
                .truncate()
                .text_size(px(10.0))
                .text_color(Colors::text_secondary())
                .child(summary.label),
        )
}

pub fn background_task_panel(
    store: &BackgroundTaskStore,
    on_close: BackgroundTaskToggleCb,
    on_cancel: BackgroundTaskCancelCb,
) -> impl IntoElement {
    let tasks = store.sorted_tasks();
    let active = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                BackgroundTaskStatus::Running | BackgroundTaskStatus::Paused
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let queued = tasks
        .iter()
        .filter(|task| task.status == BackgroundTaskStatus::Queued)
        .cloned()
        .collect::<Vec<_>>();
    let failed = tasks
        .iter()
        .filter(|task| task.status == BackgroundTaskStatus::Failed)
        .cloned()
        .collect::<Vec<_>>();
    let complete = tasks
        .iter()
        .filter(|task| task.status == BackgroundTaskStatus::Complete)
        .take(8)
        .cloned()
        .collect::<Vec<_>>();

    div()
        .absolute()
        .right(px(12.0))
        .bottom(px(26.0))
        .w(px(360.0))
        .max_h(px(392.0))
        .flex()
        .flex_col()
        .rounded(px(crate::theme::radius::SURFACE))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_panel())
        .shadow(vec![gpui::BoxShadow {
            color: Colors::surface_overlay().into(),
            offset: gpui::point(px(0.0), px(16.0)),
            blur_radius: px(34.0),
            spread_radius: px(0.0),
            inset: false,
        }])
        .occlude()
        .child(
            div()
                .h(px(32.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px(px(10.0))
                .border_b(px(1.0))
                .border_color(Colors::border_subtle())
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child("Background Tasks"),
                )
                .child(
                    div()
                        .id("background-task-panel-close")
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(crate::theme::radius::CONTROL))
                        .text_size(px(10.0))
                        .text_color(Colors::text_faint())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .hover(|s| {
                            s.bg(Colors::surface_hover())
                                .text_color(Colors::text_primary())
                        })
                        .on_click(move |_, w, cx| on_close(&(), w, cx))
                        .child("Close"),
                ),
        )
        .child(
            div()
                .id("background-task-panel-scroll")
                .max_h(px(360.0))
                .overflow_y_scroll()
                .p(px(8.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(task_section(
                    "Active",
                    active,
                    "No active jobs",
                    on_cancel.clone(),
                ))
                .child(task_section(
                    "Queued",
                    queued,
                    "Queue is clear",
                    on_cancel.clone(),
                ))
                .child(task_section(
                    "Failed",
                    failed,
                    "No failures",
                    on_cancel.clone(),
                ))
                .child(task_section(
                    "Recently Completed",
                    complete,
                    "Nothing completed yet",
                    on_cancel,
                )),
        )
}

fn task_section(
    title: &'static str,
    tasks: Vec<BackgroundTask>,
    empty: &'static str,
    on_cancel: BackgroundTaskCancelCb,
) -> impl IntoElement {
    let mut section = div().flex().flex_col().gap(px(4.0)).child(
        div()
            .px(px(2.0))
            .text_size(px(9.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(Colors::text_faint())
            .child(title),
    );
    if tasks.is_empty() {
        section = section.child(
            div()
                .px(px(2.0))
                .py(px(1.0))
                .text_size(px(10.0))
                .text_color(Colors::text_faint())
                .child(empty),
        );
    } else {
        for task in tasks {
            section = section.child(task_row(task, on_cancel.clone()));
        }
    }
    section
}

fn task_row(task: BackgroundTask, on_cancel: BackgroundTaskCancelCb) -> impl IntoElement {
    let pct = task.progress.and_then(|progress| {
        if progress.total == 0 {
            None
        } else {
            Some((progress.current as f32 / progress.total as f32).clamp(0.0, 1.0))
        }
    });
    let id = task.id.clone();
    let cancel_element_id = stable_task_id(&task.id);
    div()
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_input())
        .px(px(8.0))
        .py(px(6.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .w(px(20.0))
                        .text_size(px(10.0))
                        .text_color(Colors::text_faint())
                        .child(task_icon(task.kind)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(Colors::text_primary())
                        .child(task.title.clone()),
                )
                .child(status_badge(task.status))
                .children(task.cancellable.then(|| {
                    div()
                        .id(("background-task-cancel", cancel_element_id))
                        .px(px(5.0))
                        .py(px(1.0))
                        .rounded(px(crate::theme::radius::CONTROL))
                        .text_size(px(9.0))
                        .text_color(Colors::text_faint())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .hover(|s| {
                            s.bg(Colors::surface_hover())
                                .text_color(Colors::text_primary())
                        })
                        .on_click(move |_, w, cx| on_cancel(&id, w, cx))
                        .child("Cancel")
                })),
        )
        .children(task.detail.clone().map(|detail| {
            div()
                .pl(px(27.0))
                .truncate()
                .text_size(px(10.0))
                .text_color(Colors::text_faint())
                .child(detail)
        }))
        .children(pct.map(|pct| {
            div()
                .h(px(3.0))
                .rounded(px(crate::theme::radius::PILL))
                .overflow_hidden()
                .bg(Colors::border_subtle())
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(pct))
                        .rounded(px(crate::theme::radius::PILL))
                        .bg(Colors::accent_primary()),
                )
        }))
        .children(task.error.clone().map(|error| {
            div()
                .pl(px(27.0))
                .truncate()
                .text_size(px(10.0))
                .text_color(Colors::status_error())
                .child(error)
        }))
}

fn status_badge(status: BackgroundTaskStatus) -> impl IntoElement {
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .text_size(px(9.0))
        .text_color(Colors::text_muted())
        .child(status_label(status))
}

fn progress_label(prefix: &str, task: &BackgroundTask) -> String {
    if let Some(progress) = task.progress {
        if progress.total > 0 {
            return format!("{prefix} {}/{}", progress.current, progress.total);
        }
    }
    prefix.to_string()
}

fn task_icon(kind: BackgroundTaskKind) -> &'static str {
    match kind {
        BackgroundTaskKind::Import => "IN",
        BackgroundTaskKind::MediaCopy => "CP",
        BackgroundTaskKind::MetadataScan => "MD",
        BackgroundTaskKind::Waveform
        | BackgroundTaskKind::PeakGeneration
        | BackgroundTaskKind::PeakLoading => "WF",
        BackgroundTaskKind::NativeSync => "NS",
        BackgroundTaskKind::ProjectSave => "SV",
        BackgroundTaskKind::Recording => "RC",
    }
}

fn status_label(status: BackgroundTaskStatus) -> &'static str {
    match status {
        BackgroundTaskStatus::Queued => "queued",
        BackgroundTaskStatus::Running => "running",
        BackgroundTaskStatus::Paused => "paused",
        BackgroundTaskStatus::Complete => "complete",
        BackgroundTaskStatus::Failed => "failed",
        BackgroundTaskStatus::Cancelled => "cancelled",
    }
}

fn tone_color(tone: BackgroundTaskTone) -> gpui::Rgba {
    match tone {
        BackgroundTaskTone::Idle => Colors::status_success(),
        BackgroundTaskTone::Active => Colors::accent_primary(),
        BackgroundTaskTone::Info => Colors::accent_primary(),
        BackgroundTaskTone::Warning => Colors::status_warning(),
        BackgroundTaskTone::Error => Colors::status_error(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn stable_task_id(id: &str) -> u64 {
    let mut hash = 14_695_981_039_346_656_037u64;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}
