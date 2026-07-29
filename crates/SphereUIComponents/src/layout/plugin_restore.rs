//! Awaitable plugin/VST restore during the session install transaction.

use std::time::{Duration, Instant};

use gpui::{App, Context};

use super::StudioLayout;
use crate::components::progress_dialog::ProgressBarValue;
use crate::components::timeline::timeline_state::{
    InsertPluginFormat, PluginRuntimeBackend, PluginRuntimeState, TrackType, MASTER_TRACK_ID,
};

const PLUGIN_RESTORE_TIMEOUT: Duration = Duration::from_secs(120);
const AUDIO_ENGINE_WAIT: Duration = Duration::from_secs(30);
const GRAPH_SYNC_WAIT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone)]
pub(super) struct PluginRestoreTarget {
    pub track_id: String,
    pub slot_id: String,
    pub display_name: String,
    pub track_name: String,
    pub is_instrument: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PluginRestoreReport {
    pub warnings: Vec<String>,
    pub restored: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PluginRestoreWaitOutcome {
    Pending,
    Ready,
    Failed,
    Missing,
    Timeout,
    Disconnected,
}

impl StudioLayout {
    pub(super) fn set_session_install_progress(
        &mut self,
        detail: impl Into<String>,
        progress: ProgressBarValue,
        cx: &mut Context<Self>,
    ) {
        self.session_install_detail = detail.into();
        self.session_install_progress = progress;
        cx.notify();
    }

    pub(super) fn collect_plugin_restore_targets(&self, cx: &App) -> Vec<PluginRestoreTarget> {
        use crate::components::plugin_picker::STUB_PLUGIN_ID;

        let state = &self.timeline.read(cx).state;
        let mut targets = Vec::new();

        for track in &state.tracks {
            let track_name = track.name.clone();
            let is_instrument_track =
                matches!(track.track_type, TrackType::Instrument | TrackType::Midi);
            for (index, slot) in track.inserts.iter().enumerate() {
                if slot.plugin_id.as_deref() == Some(STUB_PLUGIN_ID) {
                    continue;
                }
                let is_instrument = is_instrument_track
                    && (track.instrument_plugin_instance_id.as_deref() == Some(slot.id.as_str())
                        || index == 0);
                if let Some(target) = target_from_slot(&track.id, &track_name, slot, is_instrument)
                {
                    targets.push(target);
                }
            }
        }

        let master_name = "Master".to_string();
        for slot in &state.master.inserts {
            if slot.plugin_id.as_deref() == Some(STUB_PLUGIN_ID) {
                continue;
            }
            if let Some(target) = target_from_slot(MASTER_TRACK_ID, &master_name, slot, false) {
                targets.push(target);
            }
        }

        targets
    }

    pub(super) fn begin_async_plugin_restore_and_finalize(
        &mut self,
        package: crate::loading_session::LoadedSessionPackage,
        cx: &mut Context<Self>,
    ) {
        self.validate_session_references(cx);
        self.update_virtual_keyboard_target_status(cx);
        self.schedule_loaded_project_waveforms(&package, cx);
        self.mark_engine_media_dirty();
        self.set_session_install_progress("Preparing session", ProgressBarValue::value(0.15), cx);

        let package = package;
        let entity = cx.entity().clone();
        cx.spawn(async move |_this, mut cx| {
            if !wait_until(&mut cx, &entity, AUDIO_ENGINE_WAIT, |layout| {
                layout.audio_bridge.engine.is_some()
            })
            .await
            {
                let _ = entity.update(cx, |layout, cx| {
                    layout.finish_session_install_with_report(
                        package,
                        PluginRestoreReport {
                            warnings: vec![
                                "Audio engine was not ready in time; some plugins may still be loading."
                                    .to_string(),
                            ],
                            ..PluginRestoreReport::default()
                        },
                        cx,
                    );
                });
                return;
            }

            let targets = entity.update(cx, |layout, cx| {
                layout.set_session_install_progress(
                    "Loading Plugins",
                    ProgressBarValue::value(0.2),
                    cx,
                );
                layout.prepare_bridge_plugin_restore_batch(cx);
                if !super::plugin_bridge_runtime::bridge_enabled() {
                    layout.schedule_audio_project_sync(cx, true, "session_install_in_process");
                }
                layout.collect_plugin_restore_targets(cx)
            });

            let total = targets.len().max(1);
            let mut report = PluginRestoreReport::default();

            for (index, target) in targets.iter().enumerate() {
                let progress = 0.2 + (0.55 * (index as f32) / total as f32);
                let kind = if target.is_instrument {
                    "instrument"
                } else {
                    "insert effect"
                };
                let detail = format!(
                    "Loading Plugin {}/{}: {} on {}",
                    index + 1,
                    total,
                    target.display_name,
                    target.track_name
                );
                let _ = entity.update(cx, |layout, cx| {
                    layout.set_session_install_progress(
                        detail.clone(),
                        ProgressBarValue::value(progress),
                        cx,
                    );
                });

                let outcome = entity.update(cx, |layout, cx| {
                    layout.restore_one_plugin_target(target, cx)
                });

                match outcome {
                    PluginRestoreWaitOutcome::Ready => {
                        report.restored += 1;
                    }
                    PluginRestoreWaitOutcome::Missing => {
                        report.failed += 1;
                        report.warnings.push(format!(
                            "Missing plugin on {}: {}",
                            target.track_name, target.display_name
                        ));
                    }
                    PluginRestoreWaitOutcome::Failed | PluginRestoreWaitOutcome::Timeout => {
                        report.failed += 1;
                        report.warnings.push(format!(
                            "Failed to restore {} on {}",
                            target.display_name, target.track_name
                        ));
                    }
                    PluginRestoreWaitOutcome::Disconnected => {
                        report.failed += 1;
                        report.warnings.push(
                            "Plugin bridge host disconnected during restore.".to_string(),
                        );
                        break;
                    }
                    PluginRestoreWaitOutcome::Pending => {
                        let waited =
                            wait_for_plugin_terminal(&mut cx, &entity, &target.slot_id).await;
                        match waited {
                            PluginRestoreWaitOutcome::Ready => report.restored += 1,
                            PluginRestoreWaitOutcome::Missing => {
                                report.failed += 1;
                                report.warnings.push(format!(
                                    "Missing plugin on {}: {}",
                                    target.track_name, target.display_name
                                ));
                            }
                            PluginRestoreWaitOutcome::Failed
                            | PluginRestoreWaitOutcome::Timeout
                            | PluginRestoreWaitOutcome::Disconnected => {
                                report.failed += 1;
                                report.warnings.push(format!(
                                    "Failed to restore {} on {} ({kind})",
                                    target.display_name, target.track_name
                                ));
                            }
                            PluginRestoreWaitOutcome::Pending => {}
                        }
                    }
                }
            }

            let _ = entity.update(cx, |layout, cx| {
                layout.set_session_install_progress(
                    "Rebuilding audio graph",
                    ProgressBarValue::value(0.82),
                    cx,
                );
                layout.sync_plugin_bridge_sinks_to_engine(cx, "session_install_restore");
                layout.schedule_audio_project_sync(cx, true, "session_install_restore");
            });

            let _ = wait_until(&mut cx, &entity, GRAPH_SYNC_WAIT, |layout| {
                !layout.audio_bridge.project_dirty
                    && !layout.audio_bridge.media_dirty
                    && !layout.audio_bridge.sync_in_flight
            })
            .await;

            let _ = entity.update(cx, |layout, cx| {
                layout.set_session_install_progress(
                    "Finalizing session",
                    ProgressBarValue::value(0.95),
                    cx,
                );
                layout.finish_session_install_with_report(package, report, cx);
            });
        })
        .detach();
    }

    fn restore_one_plugin_target(
        &mut self,
        target: &PluginRestoreTarget,
        cx: &mut Context<Self>,
    ) -> PluginRestoreWaitOutcome {
        if super::plugin_bridge_runtime::bridge_enabled() {
            return self.restore_bridge_target(target, cx);
        }
        self.restore_in_process_target(target, cx)
    }

    fn restore_bridge_target(
        &mut self,
        target: &PluginRestoreTarget,
        cx: &mut Context<Self>,
    ) -> PluginRestoreWaitOutcome {
        let terminal = self.plugin_restore_terminal_state(&target.track_id, &target.slot_id, cx);
        if terminal.is_ready() {
            return terminal;
        }

        let slot = self
            .timeline
            .read(cx)
            .state
            .find_insert_slot(&target.track_id, &target.slot_id)
            .cloned();
        let Some(slot) = slot else {
            return PluginRestoreWaitOutcome::Failed;
        };

        let is_builtin = slot
            .plugin_id
            .as_deref()
            .is_some_and(SpherePluginHost::builtin_audio_bridge_supported);
        if !is_builtin && !slot.plugin_path.as_ref().is_some_and(|p| p.exists()) {
            let reason = slot
                .plugin_path
                .as_ref()
                .map(|p| format!("Plugin file not found: {}", p.display()))
                .unwrap_or_else(|| "Plugin file not found".to_string());
            let _ = self.timeline.update(cx, |timeline, _cx| {
                timeline.state.set_insert_runtime(
                    &target.track_id,
                    &target.slot_id,
                    PluginRuntimeBackend::ExternalBridge,
                    PluginRuntimeState::Missing(reason),
                    None,
                );
            });
            return PluginRestoreWaitOutcome::Missing;
        }

        if self.load_bridge_insert_for_slot(&target.track_id, &target.slot_id, cx) {
            self.poll_plugin_restore_terminal(&target.slot_id, cx)
        } else {
            PluginRestoreWaitOutcome::Failed
        }
    }

    fn restore_in_process_target(
        &mut self,
        target: &PluginRestoreTarget,
        _cx: &mut Context<Self>,
    ) -> PluginRestoreWaitOutcome {
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            return PluginRestoreWaitOutcome::Pending;
        };
        let statuses = engine.insert_statuses();
        if let Some(st) = statuses.iter().find(|st| st.insert_id == target.slot_id) {
            if st.ready {
                return PluginRestoreWaitOutcome::Ready;
            }
            return PluginRestoreWaitOutcome::Failed;
        }
        PluginRestoreWaitOutcome::Pending
    }

    pub(super) fn poll_plugin_restore_terminal(
        &mut self,
        slot_id: &str,
        cx: &mut Context<Self>,
    ) -> PluginRestoreWaitOutcome {
        self.poll_plugin_bridge_runtime(cx);
        let owners = self
            .timeline
            .read(cx)
            .state
            .insert_owner_ids_containing(slot_id);
        for track_id in owners {
            let outcome = self.plugin_restore_terminal_state(&track_id, slot_id, cx);
            if outcome != PluginRestoreWaitOutcome::Pending {
                return outcome;
            }
        }
        PluginRestoreWaitOutcome::Pending
    }

    fn plugin_restore_terminal_state(
        &self,
        track_id: &str,
        slot_id: &str,
        cx: &App,
    ) -> PluginRestoreWaitOutcome {
        let Some(slot) = self
            .timeline
            .read(cx)
            .state
            .find_insert_slot(track_id, slot_id)
        else {
            return PluginRestoreWaitOutcome::Failed;
        };
        match &slot.runtime_state {
            PluginRuntimeState::Active
            | PluginRuntimeState::Loaded
            | PluginRuntimeState::Ready
            | PluginRuntimeState::EditorOpen
            | PluginRuntimeState::EditorClosed => PluginRestoreWaitOutcome::Ready,
            PluginRuntimeState::Missing(_) => PluginRestoreWaitOutcome::Missing,
            PluginRuntimeState::Failed(_) => PluginRestoreWaitOutcome::Failed,
            PluginRuntimeState::Loading | PluginRuntimeState::NotLoaded => {
                PluginRestoreWaitOutcome::Pending
            }
            _ => PluginRestoreWaitOutcome::Pending,
        }
    }

    pub(super) fn finish_session_install_with_report(
        &mut self,
        package: crate::loading_session::LoadedSessionPackage,
        report: PluginRestoreReport,
        cx: &mut Context<Self>,
    ) {
        self.session_install_warnings = report.warnings.clone();
        self.session_install_status = crate::app_state::SessionInstallStatus::Ready;
        self.project_state = if self.project_session.project_file_path.is_some() {
            crate::app_state::ProjectState::SavedProject { path: package.path }
        } else {
            crate::app_state::ProjectState::UnsavedWorkspace
        };
        self.session_install_detail.clear();
        self.session_install_progress = ProgressBarValue::value(1.0);

        session_log!(
            "install complete plugins restored={} failed={} warnings={}",
            report.restored,
            report.failed,
            report.warnings.len()
        );

        if !report.warnings.is_empty() {
            for warning in &report.warnings {
                eprintln!("[PluginRestore] warning: {warning}");
            }
            self.queue_session_load_warning_dialog(report.warnings, cx);
        }

        cx.notify();
    }

    pub(super) fn queue_session_load_warning_dialog(
        &mut self,
        warnings: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        use crate::components::message_box_dialog::{MessageBoxKind, MessageBoxOptions};
        use std::sync::Arc;

        let summary = if warnings.len() == 1 {
            warnings[0].clone()
        } else {
            format!(
                "{} plugin restore warning(s):\n\n{}",
                warnings.len(),
                warnings
                    .iter()
                    .take(8)
                    .map(|w| format!("• {w}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let owner_bounds = self.studio_window_bounds(cx);
        let options = MessageBoxOptions {
            kind: MessageBoxKind::Warning,
            title: "Project Loaded With Warnings".to_string(),
            message: summary,
            detail: None,
            buttons: vec!["OK".to_string()],
            default_id: 0,
            cancel_id: None,
        };
        let _ = crate::components::message_box_dialog::open_message_box_window(
            owner_bounds,
            options,
            Arc::new(|_result, _window, _cx| {}),
            cx,
        );
    }
}

fn target_from_slot(
    track_id: &str,
    track_name: &str,
    slot: &crate::components::timeline::timeline_state::InsertSlotState,
    is_instrument: bool,
) -> Option<PluginRestoreTarget> {
    let is_builtin = slot
        .plugin_id
        .as_deref()
        .is_some_and(SpherePluginHost::builtin_audio_bridge_supported);
    if !is_builtin {
        if slot.plugin_format != Some(InsertPluginFormat::Vst3) {
            return None;
        }
        let path = slot.plugin_path.as_ref()?;
        if path.as_os_str().is_empty() {
            return None;
        }
    }
    Some(PluginRestoreTarget {
        track_id: track_id.to_string(),
        slot_id: slot.id.clone(),
        display_name: slot.display_name.clone(),
        track_name: track_name.to_string(),
        is_instrument,
    })
}

async fn wait_for_plugin_terminal(
    cx: &mut gpui::AsyncApp,
    entity: &gpui::Entity<StudioLayout>,
    slot_id: &str,
) -> PluginRestoreWaitOutcome {
    let deadline = Instant::now() + PLUGIN_RESTORE_TIMEOUT;
    while Instant::now() < deadline {
        cx.background_executor().timer(POLL_INTERVAL).await;
        let outcome = entity.update(cx, |layout, cx| {
            layout.poll_plugin_restore_terminal(slot_id, cx)
        });
        if outcome != PluginRestoreWaitOutcome::Pending {
            return outcome;
        }
    }
    PluginRestoreWaitOutcome::Timeout
}

async fn wait_until(
    cx: &mut gpui::AsyncApp,
    entity: &gpui::Entity<StudioLayout>,
    timeout: Duration,
    mut predicate: impl FnMut(&StudioLayout) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let ready = entity.update(cx, |layout, _cx| predicate(layout));
        if ready {
            return true;
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    }
    false
}

impl PluginRestoreWaitOutcome {
    fn is_ready(self) -> bool {
        matches!(self, PluginRestoreWaitOutcome::Ready)
    }
}

macro_rules! session_log {
    ($($arg:tt)*) => {
        eprintln!("[SessionLoad] {}", format!($($arg)*))
    };
}
use session_log;
