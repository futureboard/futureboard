//! Studio side of the plug-in editor window's chrome.
//!
//! The window renders the strip; everything it shows and everything it asks for
//! is owned here, because the insert, the engine and the preset files are the
//! studio's. See [`crate::components::plugin_editor_chrome`] for the view.

use std::path::PathBuf;

use gpui::Context;

use crate::components::plugin_editor_chrome::{
    PluginEditorAction, PluginEditorChrome, PluginEditorTab,
};
use crate::layout::StudioLayout;

/// Extension for a stored preset file: the plug-in's own opaque state bytes,
/// exactly as it handed them over. Nothing here parses or edits them — a preset
/// that a plug-in cannot read back is worse than no preset at all.
const PRESET_EXTENSION: &str = "fbstate";

/// Set to keep the pre-GPUI Win32 editor shell for bridged inserts.
///
/// The editor moved into a GPUI window so its chrome — the insert it belongs
/// to, active, presets, what it costs — can live in the titlebar strip. This is
/// the way back if that window misbehaves on a machine, not a supported second
/// mode: it will go once the GPUI path has been through a release.
pub(super) fn legacy_native_editor_shell() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_PLUGIN_EDITOR_LEGACY_SHELL").is_some())
}

/// Where one plug-in's user presets live.
///
/// Under the registry's preset root so presets sit beside everything else the
/// plug-in system writes, in a folder named for the plug-in rather than the
/// insert: a preset saved on one track is meant to be reachable from another.
fn preset_dir(plugin_id: &str) -> Option<PathBuf> {
    let safe: String = plugin_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        return None;
    }
    Some(
        SpherePluginHost::registry::default_preset_root()
            .join("User")
            .join(safe),
    )
}

/// Preset names for one plug-in, sorted, without extensions.
fn list_presets(plugin_id: &str) -> Vec<String> {
    let Some(dir) = preset_dir(plugin_id) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(PRESET_EXTENSION) {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

impl StudioLayout {
    /// Pushes fresh chrome into every open plug-in editor window.
    ///
    /// Called from the same poll that drives the bridge, so the readouts follow
    /// the engine without a timer of their own. An unchanged chrome notifies
    /// nothing, so a steady CPU reading does not repaint the window.
    pub(super) fn refresh_plugin_editor_chrome(&mut self, cx: &mut Context<Self>) {
        if self.plugin_editors.open.is_empty() {
            return;
        }
        let sample_rate = self
            .audio_bridge
            .stats
            .as_ref()
            .map(|stats| stats.sample_rate)
            .unwrap_or(0);
        let handles: Vec<_> = self.plugin_editors.open.values().cloned().collect();
        for handle in handles {
            let Ok(Some(chrome)) = handle.update(cx, |editor, _window, _cx| {
                let (track_id, insert_id) = editor.insert_key();
                Some((track_id.to_string(), insert_id.to_string()))
            }) else {
                continue;
            };
            let (track_id, insert_id) = chrome;
            let Some(chrome) =
                self.plugin_editor_chrome_for(&track_id, &insert_id, sample_rate, cx)
            else {
                continue;
            };
            let tabs = self.plugin_editor_tabs_for(&track_id, cx);
            let _ = handle.update(cx, |editor, _window, cx| {
                editor.set_chrome(chrome, cx);
                editor.set_tabs(tabs, cx);
            });
        }
    }

    /// Builds one insert's chrome from the project and the engine.
    fn plugin_editor_chrome_for(
        &self,
        track_id: &str,
        insert_id: &str,
        sample_rate: u32,
        cx: &Context<Self>,
    ) -> Option<PluginEditorChrome> {
        let state = &self.timeline.read(cx).state;
        let slots = state.insert_slots(track_id)?;
        let (index, slot) = slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.id == insert_id)?;
        let presets = slot
            .plugin_id
            .as_deref()
            .map(list_presets)
            .unwrap_or_default();
        // A bridged plug-in reports through its shared region, which the host
        // writes and the bridge runtime holds; the engine's control-side graph
        // is a clone from when the stream opened and never sees either value.
        // An in-process insert has no region, so that one does come from the
        // engine.
        let bridged = self
            .plugin_editors
            .bridge_runtime
            .as_ref()
            .and_then(|runtime| runtime.lock().ok()?.instance_load(insert_id));
        let (cpu_load, latency_samples) = match bridged {
            Some((share, latency)) => (share, latency),
            None => {
                let load = self
                    .audio_bridge
                    .engine
                    .as_ref()
                    .and_then(|engine| engine.insert_load(track_id, insert_id));
                (
                    load.map(|(share, _)| share),
                    load.map(|(_, latency)| latency).unwrap_or(0),
                )
            }
        };
        let track_name = state
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.name.clone())
            .unwrap_or_default();
        Some(PluginEditorChrome {
            plugin_name: slot.display_name.clone(),
            track_name,
            insert_number: index + 1,
            active: slot.enabled && !slot.bypassed,
            latency_samples,
            sample_rate,
            cpu_load,
            preset_index: self
                .plugin_editors
                .preset_selection
                .get(&(track_id.to_string(), insert_id.to_string()))
                .copied()
                .filter(|index| *index < presets.len()),
            presets,
        })
    }

    /// The plug-ins open on one channel, in slot order.
    ///
    /// A tab exists for every insert the user has opened an editor for on this
    /// channel; the window shows one of them at a time. Slot order rather than
    /// the order they were opened, because that is the order the audio actually
    /// goes through them in.
    fn plugin_editor_tabs_for(&self, track_id: &str, cx: &Context<Self>) -> Vec<PluginEditorTab> {
        let Some(open) = self.plugin_editors.editor_tabs.get(track_id) else {
            return Vec::new();
        };
        if open.is_empty() {
            return Vec::new();
        }
        let state = &self.timeline.read(cx).state;
        let Some(slots) = state.insert_slots(track_id) else {
            return Vec::new();
        };
        slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| open.iter().any(|id| id == &slot.id))
            .map(|(index, slot)| PluginEditorTab {
                insert_id: slot.id.clone(),
                display_name: slot.display_name.clone(),
                insert_number: index + 1,
            })
            .collect()
    }

    /// Brings one of a channel's open plug-ins to the front of its window.
    fn select_plugin_editor_tab(
        &mut self,
        track_id: &str,
        insert_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.plugin_editor_window_for(track_id) else {
            return;
        };
        let display_name = self
            .timeline
            .read(cx)
            .state
            .insert_slots(track_id)
            .and_then(|slots| {
                slots
                    .iter()
                    .find(|slot| slot.id == insert_id)
                    .map(|slot| slot.display_name.clone())
            })
            .unwrap_or_else(|| insert_id.to_string());
        let _ = handle.update(cx, |editor, window, cx| {
            editor.activate_tab(insert_id, &display_name, window, cx);
        });
    }

    /// Closes one tab. The last one closes the window with it.
    fn close_plugin_editor_tab(&mut self, track_id: &str, insert_id: &str, cx: &mut Context<Self>) {
        let remaining: Vec<String> = {
            let tabs = self
                .plugin_editors
                .editor_tabs
                .entry(track_id.to_string())
                .or_default();
            tabs.retain(|id| id != insert_id);
            tabs.clone()
        };
        let was_active = self
            .plugin_editor_window_for(track_id)
            .and_then(|handle| {
                handle
                    .update(cx, |editor, _window, _cx| {
                        editor.insert_key().1 == insert_id
                    })
                    .ok()
            })
            .unwrap_or(false);
        // The plug-in keeps processing; only its editor closed.
        self.close_bridge_editor(cx, track_id, insert_id);
        let Some(handle) = self.plugin_editor_window_for(track_id) else {
            return;
        };
        let Some(next) = remaining.first().cloned() else {
            self.plugin_editors.editor_tabs.remove(track_id);
            self.plugin_editors
                .open
                .retain(|(track, _), _| track != track_id);
            let _ = handle.update(cx, |_editor, window, _cx| window.remove_window());
            return;
        };
        if was_active {
            self.select_plugin_editor_tab(track_id, &next, cx);
        }
        cx.notify();
    }

    /// The editor window hosting one channel, whichever insert opened it.
    pub(super) fn plugin_editor_window_for(
        &self,
        track_id: &str,
    ) -> Option<gpui::WindowHandle<crate::components::plugin_editor_window::PluginEditorWindow>>
    {
        self.plugin_editors
            .open
            .iter()
            .find(|((track, _), _)| track == track_id)
            .map(|(_, handle)| *handle)
    }

    /// Applies whatever the chrome's controls asked for since the last poll.
    pub(super) fn drain_plugin_editor_chrome_actions(&mut self, cx: &mut Context<Self>) {
        if self.plugin_editors.open.is_empty() {
            return;
        }
        let handles: Vec<_> = self.plugin_editors.open.values().cloned().collect();
        let mut requests: Vec<(String, String, PluginEditorAction)> = Vec::new();
        for handle in handles {
            let _ = handle.update(cx, |editor, _window, _cx| {
                let (track_id, insert_id) = editor.insert_key();
                let (track_id, insert_id) = (track_id.to_string(), insert_id.to_string());
                for action in editor.take_chrome_actions() {
                    requests.push((track_id.clone(), insert_id.clone(), action));
                }
            });
        }
        for (track_id, insert_id, action) in requests {
            self.apply_plugin_editor_action(&track_id, &insert_id, action, cx);
        }
    }

    fn apply_plugin_editor_action(
        &mut self,
        track_id: &str,
        insert_id: &str,
        action: PluginEditorAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            PluginEditorAction::SetActive(active) => {
                // The same live path the inspector's own toggle uses: the
                // runtime "enabled" param, no graph rebuild.
                let changed = self.timeline.update(cx, |timeline, cx| {
                    let Some(slots) = timeline.state.insert_slots_mut(track_id) else {
                        return false;
                    };
                    let Some(slot) = slots.iter_mut().find(|slot| slot.id == insert_id) else {
                        return false;
                    };
                    if slot.enabled == active && !slot.bypassed {
                        return false;
                    }
                    slot.enabled = active;
                    // Bypass and disable say the same thing from the editor's
                    // side, so turning the plug-in back on clears both.
                    if active {
                        slot.bypassed = false;
                    }
                    cx.notify();
                    true
                });
                if changed {
                    self.push_insert_enabled_to_engine(track_id, insert_id, cx);
                    self.mark_dirty_view_only();
                    self.push_mixer_snapshot_to_window(cx);
                    cx.notify();
                }
            }
            PluginEditorAction::StepPreset(delta) => {
                self.step_plugin_editor_preset(track_id, insert_id, delta, cx);
            }
            PluginEditorAction::SavePreset => {
                self.save_plugin_editor_preset(track_id, insert_id, cx);
            }
            PluginEditorAction::SelectTab(target) => {
                self.select_plugin_editor_tab(track_id, &target, cx);
            }
            PluginEditorAction::CloseTab(target) => {
                self.close_plugin_editor_tab(track_id, &target, cx);
            }
        }
    }

    /// Moves through the preset list and loads what it lands on.
    fn step_plugin_editor_preset(
        &mut self,
        track_id: &str,
        insert_id: &str,
        delta: i32,
        cx: &mut Context<Self>,
    ) {
        let Some(plugin_id) = self.insert_plugin_id(track_id, insert_id, cx) else {
            return;
        };
        let presets = list_presets(&plugin_id);
        if presets.is_empty() {
            return;
        }
        let key = (track_id.to_string(), insert_id.to_string());
        let current = self
            .plugin_editors
            .preset_selection
            .get(&key)
            .copied()
            .unwrap_or(0) as i64;
        let count = presets.len() as i64;
        let next = ((current + delta as i64) % count + count) % count;
        let index = next as usize;
        let Some(dir) = preset_dir(&plugin_id) else {
            return;
        };
        let path = dir.join(format!("{}.{PRESET_EXTENSION}", presets[index]));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("[plugin-preset] could not read {}: {error}", path.display());
                return;
            }
        };
        // Straight back to the plug-in as opaque state, and into the project so
        // a save keeps what is actually loaded.
        if let Some(runtime) = self.plugin_editors.bridge_runtime.as_ref().cloned() {
            if let Ok(mut runtime) = runtime.lock() {
                if let Err(error) = runtime.send_plugin_state(insert_id, &bytes) {
                    eprintln!("[plugin-preset] SetPluginState failed: {error}");
                    return;
                }
            }
        }
        self.timeline.update(cx, |timeline, cx| {
            if let Some(slots) = timeline.state.insert_slots_mut(track_id) {
                if let Some(slot) = slots.iter_mut().find(|slot| slot.id == insert_id) {
                    slot.vst3_state = Some(std::sync::Arc::new(bytes.clone()));
                }
            }
            cx.notify();
        });
        self.plugin_editors.preset_selection.insert(key, index);
        self.mark_dirty_view_only();
        eprintln!(
            "[plugin-preset] loaded '{}' for insert={insert_id} bytes={}",
            presets[index],
            bytes.len()
        );
        cx.notify();
    }

    /// Captures the plug-in's current state and writes it as a new preset.
    fn save_plugin_editor_preset(
        &mut self,
        track_id: &str,
        insert_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(plugin_id) = self.insert_plugin_id(track_id, insert_id, cx) else {
            return;
        };
        let Some(dir) = preset_dir(&plugin_id) else {
            return;
        };
        // Asked for fresh rather than reusing the project's copy: the point of
        // saving from the editor is to keep what the user just dialled in, and
        // the project's copy is only as new as the last save.
        let captured = self
            .plugin_editors
            .bridge_runtime
            .as_ref()
            .cloned()
            .and_then(|runtime| {
                runtime.lock().ok().map(|mut runtime| {
                    runtime.request_plugin_states(
                        std::slice::from_ref(&insert_id.to_string()),
                        std::time::Duration::from_millis(1500),
                    )
                })
            })
            .and_then(|mut states| states.remove(insert_id));
        let bytes = match captured {
            Some(bytes) if !bytes.is_empty() => bytes,
            _ => {
                self.ara.last_error = None;
                eprintln!(
                    "[plugin-preset] nothing to save for insert={insert_id}: the plug-in \
                     returned no state"
                );
                return;
            }
        };
        if let Err(error) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "[plugin-preset] could not create {}: {error}",
                dir.display()
            );
            return;
        }
        // Numbered rather than prompting: the editor has no room for a dialog,
        // and a preset the user can rename on disk beats one they cannot save.
        let existing = list_presets(&plugin_id);
        let mut index = existing.len() + 1;
        let path = loop {
            let candidate = dir.join(format!("Preset {index}.{PRESET_EXTENSION}"));
            if !candidate.exists() {
                break candidate;
            }
            index += 1;
        };
        if let Err(error) = std::fs::write(&path, &bytes) {
            eprintln!(
                "[plugin-preset] could not write {}: {error}",
                path.display()
            );
            return;
        }
        eprintln!(
            "[plugin-preset] saved {} bytes={}",
            path.display(),
            bytes.len()
        );
        let names = list_presets(&plugin_id);
        if let Some(position) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| names.iter().position(|name| name == stem))
        {
            self.plugin_editors
                .preset_selection
                .insert((track_id.to_string(), insert_id.to_string()), position);
        }
        cx.notify();
    }

    fn insert_plugin_id(
        &self,
        track_id: &str,
        insert_id: &str,
        cx: &Context<Self>,
    ) -> Option<String> {
        let state = &self.timeline.read(cx).state;
        state
            .insert_slots(track_id)?
            .iter()
            .find(|slot| slot.id == insert_id)
            .and_then(|slot| slot.plugin_id.clone())
    }
}
