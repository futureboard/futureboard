use gpui::{App, Bounds, Context, Window};

use std::path::PathBuf;
use std::sync::Arc;

use crate::components::add_track_dialog::{
    open_add_track_window, AddTrackDialogState, AddTrackKind, AudioFormat, InstrumentMode,
    FIRST_STEREO_PAIR_INPUT_LABEL,
};
use crate::components::combo_box::dedupe_preserve_order;
use crate::components::keymap_window::{open_keymap_window, KeymapChangedCb};
use crate::components::midi_editor_window::{midi_editor_debug, open_midi_editor_window};
use crate::components::settings_dialog::{
    open_settings_window, AudioDeviceListsProvider, OnSettingUpdate, SettingsAudioDeviceLists,
};
use crate::components::timeline::timeline_state::{
    self, ClipType, CreateTrackOptions, InsertPluginFormat, TrackAudioFormat, TrackInputRouting,
    TrackMidiInputRouting, TrackOutputRouting, TrackType,
};
use crate::components::{external_mixer_debug, open_mixer_window};
use crate::window_position::resolve_owner_bounds_with_preferred;
use SpherePluginHost::{PluginFormat as RegistryPluginFormat, PluginKind};

use super::helpers::{cleaned_track_name, numbered_name_stem};
use super::{ContextMenuTarget, OpenPopover, StudioLayout};

fn add_track_instrument_plugins_from_catalog(
    catalog: &super::plugin_ops::PluginCatalogState,
) -> Vec<SpherePluginHost::RegistryPlugin> {
    catalog
        .available
        .as_ref()
        .map(|plugins| {
            plugins
                .iter()
                .filter(|plugin| {
                    plugin.kind == PluginKind::Instrument
                        && plugin.supports_insert()
                        && plugin.scan_status.is_usable()
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn dialog_audio_format(format: AudioFormat) -> TrackAudioFormat {
    match format {
        AudioFormat::Mono => TrackAudioFormat::Mono,
        AudioFormat::Stereo => TrackAudioFormat::Stereo,
    }
}

fn dialog_audio_input_routing(
    label: &str,
    format: AudioFormat,
    input_device: Option<&(String, u32)>,
) -> TrackInputRouting {
    match label {
        "None" => TrackInputRouting::None,
        "Input 1" | "Input 2" => {
            let Some((device_id, channels)) = input_device else {
                return TrackInputRouting::AllInputs;
            };
            let channel = if label == "Input 2" { 1 } else { 0 };
            if channel >= *channels {
                return TrackInputRouting::AllInputs;
            }
            TrackInputRouting::AudioDeviceChannel {
                device_id: device_id.clone(),
                channel,
            }
        }
        FIRST_STEREO_PAIR_INPUT_LABEL if format == AudioFormat::Stereo => {
            let Some((device_id, channels)) = input_device else {
                return TrackInputRouting::AllInputs;
            };
            if *channels < 2 {
                return TrackInputRouting::AllInputs;
            }
            TrackInputRouting::AudioDeviceChannels {
                device_id: device_id.clone(),
                channels: vec![0, 1],
            }
        }
        _ => TrackInputRouting::AllInputs,
    }
}

fn dialog_audio_output_routing(label: &str) -> TrackOutputRouting {
    match label {
        "None" => TrackOutputRouting::None,
        _ => TrackOutputRouting::Main,
    }
}

fn dialog_midi_input_routing(label: &str) -> TrackMidiInputRouting {
    match label {
        "All MIDI Inputs" => TrackMidiInputRouting::AllInputs,
        "None" => TrackMidiInputRouting::None,
        device => TrackMidiInputRouting::MidiDevice {
            device_id: device.to_string(),
        },
    }
}

/// Real, enabled MIDI input device names for the Add Track dialog's
/// Instrument/MIDI routing selects — the same cached registry + Preferences
/// enable-state resolution Settings and the Inspector routing combo use
/// (`device_registry` + `resolve_midi_devices`), not a mocked list.
fn add_track_midi_input_devices(schema: &crate::settings::SettingsSchema) -> Vec<String> {
    let saved = schema.hardware.midi.devices.clone();
    let detected = crate::device_registry::cached_midi_devices();
    let resolved = sphere_midi_service::resolve_midi_devices(&saved, &detected);
    resolved
        .iter()
        .filter(|d| {
            d.enabled
                && matches!(
                    d.direction,
                    crate::settings::MidiDeviceDirection::Input
                        | crate::settings::MidiDeviceDirection::InputOutput
                )
        })
        .map(|d| d.name.clone())
        .collect()
}

/// Studio-window / app-integration hooks — this workspace's own window handle,
/// the last known window bounds (used to position child windows without
/// re-entering the root `WindowHandle`), and the app-level "re-open Welcome"
/// hook. `StudioLayout` decomposition slice (all Option → derived `Default`).
#[derive(Default)]
pub(crate) struct StudioWindowHooks {
    /// Handle to this workspace's own window; `None` until wired by the app layer.
    pub self_window: Option<gpui::WindowHandle<StudioLayout>>,
    /// Last known main workspace bounds, updated during render.
    pub cached_bounds: Option<Bounds<gpui::Pixels>>,
    /// App-level hook that re-opens the Welcome window (invoked by close_project).
    pub on_request_welcome: Option<Arc<dyn Fn(&mut gpui::App) + 'static>>,
    /// App-level hook for in-studio project open/replace — keeps the root studio
    /// window alive and swaps the session in place.
    pub on_request_project_load: Option<
        Arc<dyn Fn(PathBuf, super::project_ops::ProjectOpenOptions, &mut gpui::App) + 'static>,
    >,
    /// App-level hook for visible session shutdown (close project).
    pub on_request_session_shutdown: Option<
        Arc<
            dyn Fn(
                    crate::session_shutdown::SessionShutdownReason,
                    Option<Bounds<gpui::Pixels>>,
                    gpui::WindowHandle<StudioLayout>,
                    &mut gpui::App,
                ) + 'static,
        >,
    >,
}

/// Floating MIDI editor window state — the single editor window handle (switches
/// clip on open) and the owner bounds parked for a deferred open. `StudioLayout`
/// decomposition slice (both Option → derived `Default`).
#[derive(Default)]
pub(crate) struct MidiEditorWindowState {
    /// Global floating MIDI editor window; `None` when closed.
    pub window: Option<gpui::WindowHandle<crate::components::midi_editor_window::MidiEditorWindow>>,
    /// Owner bounds for a deferred editor open.
    pub pending_open: Option<Bounds<gpui::Pixels>>,
}

/// Detached / external window handles owned by the studio (settings, mixer,
/// add-track, plugin-manager, export-arrangement) plus the bounds parked for a
/// deferred external-mixer open. `StudioLayout` decomposition slice (all Option
/// → derived `Default`).
#[derive(Default)]
pub(crate) struct ExternalWindows {
    /// External Settings window; `None` when closed.
    pub settings: Option<gpui::WindowHandle<crate::components::settings_dialog::SettingsWindow>>,
    /// Detached mixer window (multi-monitor layouts).
    pub mixer: Option<gpui::WindowHandle<crate::components::MixerWindow>>,
    /// Bounds for an external-mixer open deferred to after the current update.
    pub pending_mixer_open: Option<Bounds<gpui::Pixels>>,
    /// Add Track dialog window.
    pub add_track: Option<gpui::WindowHandle<crate::components::add_track_dialog::AddTrackWindow>>,
    /// Plugin Manager window.
    pub plugin_manager:
        Option<gpui::WindowHandle<crate::components::plugin_manager::PluginManagerWindow>>,
    /// Export Arrangement window.
    pub export_arrangement: Option<gpui::WindowHandle<crate::export::ExportArrangementWindow>>,
    /// Stem Extractor (MDX-NET) dialog window.
    pub stem_extractor:
        Option<gpui::WindowHandle<crate::components::stem_extractor_dialog::StemExtractorWindow>>,
    /// Keymap / keyboard shortcuts editor window.
    pub keymap: Option<gpui::WindowHandle<crate::components::keymap_window::KeymapWindow>>,
    /// Built-in Soundfont Player MDI window.
    pub soundfont_player: Option<
        gpui::WindowHandle<crate::components::soundfont_player_window::SoundfontPlayerWindow>,
    >,
    /// Audio Routing Matrix ("Audio Connections") window.
    pub routing_matrix:
        Option<gpui::WindowHandle<crate::components::routing_matrix_window::RoutingMatrixWindow>>,
    pub chord_display:
        Option<gpui::WindowHandle<crate::components::song_text_panel::SongTextWindow>>,
    pub lyric_display:
        Option<gpui::WindowHandle<crate::components::song_text_panel::SongTextWindow>>,
    pub lyric_editor:
        Option<gpui::WindowHandle<crate::components::song_text_panel::SongTextWindow>>,
}

impl StudioLayout {
    pub(crate) fn open_song_text_external_window(
        &mut self,
        kind: crate::components::SongTextPanelKind,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let slot = match kind {
            crate::components::SongTextPanelKind::ChordDisplay => {
                &mut self.external_windows.chord_display
            }
            crate::components::SongTextPanelKind::LyricDisplay => {
                &mut self.external_windows.lyric_display
            }
            crate::components::SongTextPanelKind::LyricEditor => {
                &mut self.external_windows.lyric_editor
            }
        };
        if let Some(handle) = slot.clone() {
            if handle
                .update(cx, |_view, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            *slot = None;
        }
        let owner = cx.entity().clone();
        let on_close: Arc<dyn Fn(crate::components::SongTextPanelKind, &mut App) + Send + Sync> =
            Arc::new(move |closed_kind, app| {
                let _ = owner.update(app, |layout, cx| {
                    match closed_kind {
                        crate::components::SongTextPanelKind::ChordDisplay => {
                            layout.external_windows.chord_display = None
                        }
                        crate::components::SongTextPanelKind::LyricDisplay => {
                            layout.external_windows.lyric_display = None
                        }
                        crate::components::SongTextPanelKind::LyricEditor => {
                            layout.external_windows.lyric_editor = None
                        }
                    }
                    cx.notify();
                });
            });
        match crate::components::open_song_text_window(
            owner_bounds,
            self.timeline.clone(),
            kind,
            on_close,
            cx,
        ) {
            Ok(handle) => *slot = Some(handle),
            Err(error) => eprintln!("[song-text] failed to open {}: {error}", kind.title()),
        }
        cx.notify();
    }

    pub(super) fn update_add_track_instrument_plugins(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.external_windows.add_track.clone() else {
            return;
        };
        let instrument_plugins = add_track_instrument_plugins_from_catalog(&self.plugin_catalog);
        let _ = handle.update(cx, |add_track, _window, cx| {
            add_track.set_instrument_plugins(instrument_plugins);
            cx.notify();
        });
    }

    pub(super) fn open_add_track_external_window(
        &mut self,
        kind: AddTrackKind,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let mut track_count = 0;
        let mut has_master_track = false;
        let _ = self.timeline.update(cx, |timeline, _cx| {
            track_count = timeline.state.tracks.len();
            has_master_track = timeline
                .state
                .tracks
                .iter()
                .any(|track| track.track_type == TrackType::Master);
        });

        self.open_add_track_external_window_with_context(
            kind,
            track_count,
            has_master_track,
            owner_bounds,
            cx,
        );
    }

    /// Opens/activates the Add Track external window without reading/updating the Timeline.
    ///
    /// This is critical for callbacks originating from Timeline events: Timeline may already be
    /// mid-update, and calling `self.timeline.update(...)` would panic (GPUI re-entrancy guard).
    pub(super) fn open_add_track_external_window_with_context(
        &mut self,
        kind: AddTrackKind,
        track_count: usize,
        has_master_track: bool,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        // If window is already open, activate and refresh its context.
        let default_monitor_mode = self
            .settings
            .read(cx)
            .current
            .recording
            .default_monitor_mode
            .add_track_value();
        // Real MIDI input devices scanned fresh on every open so a device
        // plugged/unplugged since the last open (or since Preferences was
        // touched) is reflected immediately — cheap, non-destructive scan
        // (see `device_registry::scan_midi`), never done per paint.
        crate::device_registry::scan_midi();
        let midi_input_devices = add_track_midi_input_devices(&self.settings.read(cx).current);

        if let Some(handle) = self.external_windows.add_track.clone() {
            if handle
                .update(cx, |win, window, cx| {
                    win.set_instrument_plugins(add_track_instrument_plugins_from_catalog(
                        &self.plugin_catalog,
                    ));
                    win.set_midi_input_devices(midi_input_devices.clone());
                    win.set_context(kind, track_count, has_master_track, default_monitor_mode);
                    window.activate_window();
                    cx.notify();
                })
                .is_ok()
            {
                return;
            }
            self.external_windows.add_track = None;
        }

        self.menu_bar.open_menu_id = None;
        self.menu_bar.submenu_path.clear();
        self.overlay.open_popover = None;
        self.overlay.text_context_menu = None;

        let owner_bounds =
            resolve_owner_bounds_with_preferred(owner_bounds, self.studio_window_bounds(cx), cx);

        if self.plugin_catalog.available.is_none()
            || !matches!(
                self.plugin_catalog.status,
                crate::components::plugin_picker::CatalogStatus::Ready
            )
        {
            self.arm_catalog_load(cx);
        }
        let instrument_plugins = add_track_instrument_plugins_from_catalog(&self.plugin_catalog);

        let layout = cx.entity().clone();
        let language = self.settings.read(cx).current.general.language.clone();
        let on_confirm_request: Arc<dyn Fn(AddTrackDialogState, String, &mut gpui::App) + 'static> =
            Arc::new(move |dialog, _name, cx| {
                let Some(track_type) = dialog.selected_kind.native_track_type() else {
                    return;
                };
                let _ = layout.update(cx, |this, cx| {
                    this.mark_dirty();
                    let selected_input_device = this.selected_input_device_channels(cx);
                    let mut bridge_inserts = Vec::new();
                    let _ = this.timeline.update(cx, |timeline, cx| {
                        let count = dialog.count.clamp(1, 128) as usize;
                        let base_name =
                            cleaned_track_name(&dialog.track_name, dialog.selected_kind);
                        let mut selected_track_id = None;
                        let mut created_ids = Vec::new();
                        for i in 0..count {
                            let name = if count == 1 {
                                base_name.clone()
                            } else {
                                format!(
                                    "{} {}",
                                    numbered_name_stem(&base_name),
                                    dialog.next_number + i
                                )
                            };
                            // Auto color → generated palette color per track.
                            // Custom color → the user's chosen color (applied to
                            // every track created in this batch).
                            let color = if dialog.auto_color {
                                timeline
                                    .state
                                    .track_color_for_index(dialog.base_track_count + i)
                            } else if let Some(custom) = dialog.custom_color {
                                custom
                            } else {
                                timeline.state.track_color_for_index(dialog.color_index + i)
                            };
                            let id = timeline.state.create_track(CreateTrackOptions {
                                track_type,
                                name,
                                color,
                                volume: timeline_state::volume::db_to_norm(0.0),
                                pan: 0.0,
                                armed: dialog.selected_kind == AddTrackKind::Audio
                                    && dialog.arm_track,
                                input_monitor: match dialog.monitor_mode {
                                    "input" => timeline_state::InputMonitorMode::Always,
                                    "auto" => timeline_state::InputMonitorMode::WhenRecordArmed,
                                    _ => timeline_state::InputMonitorMode::Off,
                                },
                            });
                            if dialog.selected_kind == AddTrackKind::Instrument
                                || dialog.selected_kind == AddTrackKind::Midi
                            {
                                let midi_input = dialog_midi_input_routing(&dialog.input_label);
                                timeline.state.set_track_midi_input(&id, midi_input);
                            }
                            if dialog.selected_kind == AddTrackKind::Audio {
                                let audio_format = dialog_audio_format(dialog.audio_format);
                                let input = dialog_audio_input_routing(
                                    &dialog.input_label,
                                    dialog.audio_format,
                                    selected_input_device.as_ref(),
                                );
                                let output = dialog_audio_output_routing(&dialog.output_label);
                                timeline.state.set_track_audio_format(&id, audio_format);
                                timeline.state.set_track_input_routing(&id, input);
                                timeline.state.set_track_output_routing(&id, output);
                            }
                            if dialog.selected_kind == AddTrackKind::Instrument
                                && dialog.instrument_mode == InstrumentMode::Vsti
                            {
                                if let Some(plugin_id) = dialog.instrument_plugin_id.as_deref() {
                                    let instrument_registry =
                                        add_track_instrument_plugins_from_catalog(
                                            &this.plugin_catalog,
                                        );
                                    if let Some(reg) = instrument_registry.iter().find(|p| {
                                        p.id == plugin_id
                                            || p.class_id.as_deref() == Some(plugin_id)
                                            || p.name.eq_ignore_ascii_case(plugin_id)
                                    }) {
                                        if let Some(slot_id) = timeline.state.add_insert(&id) {
                                            let format = match reg.format {
                                                RegistryPluginFormat::Vst3 => {
                                                    InsertPluginFormat::Vst3
                                                }
                                                RegistryPluginFormat::Clap => {
                                                    InsertPluginFormat::Clap
                                                }
                                                RegistryPluginFormat::Au => InsertPluginFormat::Au,
                                                RegistryPluginFormat::Lv2 => {
                                                    InsertPluginFormat::Lv2
                                                }
                                                RegistryPluginFormat::Unknown => {
                                                    InsertPluginFormat::Unknown
                                                }
                                            };
                                            let plugin_uid = reg
                                                .class_id
                                                .clone()
                                                .unwrap_or_else(|| reg.id.clone());
                                            timeline.state.set_insert_plugin(
                                                &id,
                                                &slot_id,
                                                plugin_uid,
                                                Some(reg.path.clone()),
                                                format,
                                                Some(reg.vendor.clone())
                                                    .filter(|vendor| !vendor.trim().is_empty()),
                                                reg.name.clone(),
                                            );
                                            if format == InsertPluginFormat::Vst3 {
                                                // Auto-open the editor for a freshly
                                                // added instrument: mark the slot
                                                // pending so the host editor shell
                                                // opens (with its "Loading Plugin"
                                                // overlay) as soon as the runtime
                                                // instance is ready — matching the
                                                // insert picker (apply_picked_insert).
                                                // Skip for batch creation (count > 1)
                                                // to avoid opening many windows at once.
                                                if count == 1
                                                    && super::plugin_bridge_runtime::bridge_enabled(
                                                    )
                                                {
                                                    timeline.state.set_insert_pending_editor_open(
                                                        &id, &slot_id, true,
                                                    );
                                                }
                                                bridge_inserts.push((id.clone(), slot_id));
                                            }
                                        }
                                    }
                                }
                            } else if dialog.selected_kind == AddTrackKind::Instrument
                                && dialog.instrument_mode == InstrumentMode::SoundfontPlayer
                            {
                                // Built-in Soundfont Player is not a hosted plugin — it
                                // never goes through the VST3/CLAP/AU/LV2 bridge or
                                // plugin registry, so it gets a plain track marker
                                // instead of an insert. Inspector shows an Open button
                                // that opens the Soundfont Player MDI window for it.
                                timeline.state.set_track_builtin_soundfont_player(&id, true);
                            }
                            created_ids.push(id.clone());
                            selected_track_id = Some(id);
                        }
                        if let Some(id) = selected_track_id {
                            timeline.state.select_track(&id);
                        }
                        crate::components::add_track_dialog::add_track_debug(&format!(
                            "created tracks kind={} count={} ids={:?}",
                            dialog.selected_kind.tab_label(),
                            count,
                            created_ids
                        ));
                        cx.notify();
                    });
                    let mut bridge_loaded = false;
                    for (track_id, slot_id) in bridge_inserts {
                        bridge_loaded |= this.load_bridge_insert_for_slot(&track_id, &slot_id, cx);
                    }
                    if !bridge_loaded {
                        this.audio_bridge.project_dirty = true;
                        this.schedule_audio_project_sync(cx, true, "add_track_dialog");
                    }
                    cx.notify();
                });
            });

        match open_add_track_window(
            owner_bounds,
            kind,
            track_count,
            has_master_track,
            default_monitor_mode,
            language,
            instrument_plugins,
            midi_input_devices,
            on_confirm_request,
            cx,
        ) {
            Ok(handle) => self.external_windows.add_track = Some(handle),
            Err(err) => eprintln!("[add-track] failed to open window: {err}"),
        }
    }

    pub(super) fn open_settings_dialog(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let open_started = std::time::Instant::now();
        // If window is already open, activate it
        if let Some(handle) = self.external_windows.settings.clone() {
            if handle
                .update(cx, |_settings, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.external_windows.settings = None;
        }

        self.menu_bar.open_menu_id = None;
        self.menu_bar.submenu_path.clear();
        self.overlay.open_popover = None;
        self.project_switcher.is_open = false;
        self.overlay.text_context_menu = None;

        let owner_bounds =
            resolve_owner_bounds_with_preferred(owner_bounds, self.studio_window_bounds(cx), cx);
        let settings = self.settings.clone();
        let owner = cx.entity().clone();
        let schema = self.settings.read(cx).current.clone();

        let device_lists_provider: Option<AudioDeviceListsProvider> =
            self.audio_bridge.engine.clone().map(|engine| {
                Arc::new(move |driver_type: &str| {
                    let backend = super::native_audio_backend_from_driver_type(driver_type);
                    let input_devices = engine.list_input_devices_for_backend(backend);
                    let output_devices = engine.list_output_devices_for_backend(backend);
                    SettingsAudioDeviceLists {
                        inputs: dedupe_preserve_order(
                            &input_devices
                                .iter()
                                .map(|d| d.name.clone())
                                .collect::<Vec<_>>(),
                        ),
                        outputs: dedupe_preserve_order(
                            &output_devices
                                .iter()
                                .map(|d| d.name.clone())
                                .collect::<Vec<_>>(),
                        ),
                        input_channels: input_devices
                            .into_iter()
                            .map(|d| (d.name, d.channels))
                            .collect(),
                        output_channels: output_devices
                            .into_iter()
                            .map(|d| (d.name, d.channels))
                            .collect(),
                    }
                }) as AudioDeviceListsProvider
            });
        // Do NOT enumerate/probe synchronously here when a live engine provider
        // exists — that would block opening Settings on hardware enumeration /
        // WDM-KS probing. The window populates backend-scoped lists off the UI
        // thread on its first render. The no-engine path still builds cheap
        // placeholder lists below.
        let initial_device_lists = SettingsAudioDeviceLists::default();

        let mut available_inputs = initial_device_lists.inputs.clone();
        if !available_inputs.contains(&schema.hardware.audio.device_in)
            && !schema.hardware.audio.device_in.is_empty()
            && device_lists_provider.is_none()
        {
            available_inputs.push(schema.hardware.audio.device_in.clone());
        }
        if available_inputs.is_empty() && device_lists_provider.is_none() {
            available_inputs.push("Built-in Microphone".to_string());
        }
        available_inputs = dedupe_preserve_order(&available_inputs);

        let mut available_outputs = initial_device_lists.outputs.clone();
        if !available_outputs.contains(&schema.hardware.audio.device_out)
            && !schema.hardware.audio.device_out.is_empty()
            && device_lists_provider.is_none()
        {
            available_outputs.push(schema.hardware.audio.device_out.clone());
        }
        if available_outputs.is_empty() && device_lists_provider.is_none() {
            available_outputs.push("Speakers (Realtek)".to_string());
        }
        available_outputs = dedupe_preserve_order(&available_outputs);

        // (device name, channel count) for the read-only channel lists (Phase C).
        let available_input_channels = initial_device_lists.input_channels.clone();
        let available_output_channels = initial_device_lists.output_channels.clone();

        // Only list backends that actually exist on this build/edition — a
        // Windows-only option like "WASAPI Shared" must never be selectable
        // on Linux/macOS, and Exclusive-only ASIO must never appear on
        // Community (or an Exclusive build without ASIO entitlement).
        let available_backends = crate::settings::available_audio_driver_types();

        let on_update: OnSettingUpdate = Arc::new(move |updater, cx| {
            let _ = owner.update(cx, |this, cx| {
                // A sample-rate change while the engine is live is intercepted
                // here (confirmation dialog) instead of silently restarting the
                // audio engine — see `handle_setting_update`.
                this.handle_setting_update(updater, cx);
            });
        });

        // "Open Keyboard Shortcuts…" (Settings > Shortcuts) reuses the exact
        // window `help:keyboard-shortcuts` opens — only the studio window
        // holds the live `KeymapManager`, so Settings calls back into it
        // instead of duplicating the editor.
        let keymap_owner = cx.entity().clone();
        let on_open_keyboard_shortcuts: Option<
            crate::components::settings_dialog::OnOpenKeyboardShortcuts,
        > = Some(Arc::new(move |window, cx| {
            let owner_bounds = Some(window.bounds());
            let _ = keymap_owner.update(cx, |this, cx| {
                this.open_keymap_window(owner_bounds, cx);
            });
        }));

        let engine_for_latency = self.audio_bridge.engine.clone();
        let deferred_rate = self.audio_bridge.sample_rate_deferred_target.clone();
        let latency_provider: crate::components::settings_dialog::AudioLatencySnapshotProvider =
            Arc::new(move || {
                let mut snapshot = engine_for_latency
                    .as_ref()
                    .map(crate::settings::SettingsAudioLatencySnapshot::from_engine)
                    .unwrap_or_else(crate::settings::SettingsAudioLatencySnapshot::unavailable);
                // A deferred ("Later") rate is pending only until the active device
                // rate actually matches it — then it resolves automatically.
                let target = deferred_rate.load(std::sync::atomic::Ordering::Relaxed);
                if target != 0 && target != snapshot.active_sample_rate {
                    snapshot.restart_pending = true;
                    snapshot.deferred_sample_rate = target;
                }
                snapshot
            });
        let input_test_start: Option<crate::components::settings_dialog::InputTestStartFn> =
            self.audio_bridge.engine.clone().map(|engine| {
                Arc::new(move |device_id: Option<String>| {
                    let device_id = device_id.filter(|id| !id.trim().is_empty());
                    engine
                        .start_input_test(device_id.as_deref())
                        .map_err(|error| error.to_string())
                }) as crate::components::settings_dialog::InputTestStartFn
            });
        let input_test_stop: Option<crate::components::settings_dialog::InputTestStopFn> =
            self.audio_bridge.engine.clone().map(|engine| {
                Arc::new(move || {
                    engine.stop_input_test();
                }) as crate::components::settings_dialog::InputTestStopFn
            });
        let input_test_level: Option<crate::components::settings_dialog::InputTestLevelFn> =
            self.audio_bridge.engine.clone().map(|engine| {
                Arc::new(move || engine.input_test_level())
                    as crate::components::settings_dialog::InputTestLevelFn
            });

        match open_settings_window(
            owner_bounds,
            settings,
            available_inputs,
            available_outputs,
            available_backends,
            available_input_channels,
            available_output_channels,
            device_lists_provider,
            latency_provider,
            input_test_start,
            input_test_stop,
            input_test_level,
            on_update,
            on_open_keyboard_shortcuts,
            cx,
        ) {
            Ok(handle) => self.external_windows.settings = Some(handle),
            Err(err) => eprintln!("[settings] failed to open settings window: {err}"),
        }

        if crate::components::settings_dialog::settings_perf_debug_enabled() {
            eprintln!(
                "[settings-perf] open_settings_dialog took {:.1}ms (synchronous; device probing deferred off-thread)",
                open_started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    pub(super) fn close_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.external_windows.settings.take() {
            let _ = handle.update(cx, |_settings, window, _cx| window.remove_window());
        }
        self.overlay.text_context_menu = None;
        cx.notify();
    }

    pub(super) fn open_keymap_window(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.external_windows.keymap.clone() {
            if handle
                .update(cx, |_keymap, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.external_windows.keymap = None;
        }

        let manager = self.keymap_manager.clone();
        let studio = cx.entity().clone();
        let on_changed: KeymapChangedCb = Arc::new(move |manager, app| {
            let _ = studio.update(app, |layout, cx| {
                layout.keymap_manager = manager;
                cx.notify();
            });
        });

        match open_keymap_window(owner_bounds, manager, on_changed, cx) {
            Ok(handle) => self.external_windows.keymap = Some(handle),
            Err(err) => eprintln!("[keymap] failed to open window: {err}"),
        }
    }

    /// Builds the routing-matrix view-model from the current timeline tracks.
    fn build_routing_matrix_snapshot(
        &self,
        cx: &gpui::App,
    ) -> crate::components::RoutingMatrixSnapshot {
        crate::components::RoutingMatrixSnapshot {
            tracks: self.timeline.read(cx).state.tracks.clone(),
        }
    }

    /// Pushes a refreshed snapshot to the routing-matrix window if it is open.
    pub(crate) fn push_routing_matrix_snapshot_to_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.external_windows.routing_matrix.clone() else {
            return;
        };
        let snapshot = self.build_routing_matrix_snapshot(cx);
        let _ = handle.update(cx, |window, _w, cx| {
            window.set_snapshot(snapshot);
            cx.notify();
        });
    }

    /// Opens the Audio Routing Matrix ("Audio Connections") window, or focuses
    /// it if already open. Toggling a send cell routes back through the owner
    /// (`defer_update`) to mutate track state and push a refreshed snapshot.
    pub(super) fn open_routing_matrix_window(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.external_windows.routing_matrix.clone() {
            if handle
                .update(cx, |_window, w, _cx| w.activate_window())
                .is_ok()
            {
                return;
            }
            self.external_windows.routing_matrix = None;
        }

        let snapshot = self.build_routing_matrix_snapshot(cx);

        let studio = cx.entity().clone();
        let on_toggle_send: crate::components::routing_matrix_window::ToggleSendCb = {
            let studio = studio.clone();
            Arc::new(move |source_id: String, dest_id: String, _w, cx| {
                StudioLayout::defer_update(&studio, cx, move |this, cx| {
                    let changed = this.timeline.update(cx, |timeline, _cx| {
                        let already = timeline
                            .state
                            .tracks
                            .iter()
                            .find(|t| t.id == source_id)
                            .and_then(|t| {
                                t.sends
                                    .iter()
                                    .find(|s| s.target_track_id == dest_id)
                                    .cloned()
                            });
                        if let Some(send) = already {
                            timeline.state.remove_send(&source_id, &send.id);
                            true
                        } else {
                            timeline
                                .state
                                .add_send_to_target(&source_id, &dest_id)
                                .is_some()
                        }
                    });
                    if changed {
                        this.mark_dirty();
                        this.audio_bridge.project_dirty = true;
                    }
                    this.push_routing_matrix_snapshot_to_window(cx);
                    cx.notify();
                });
            })
        };

        let on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync> = Arc::new(move |_, app| {
            let _ = studio.update(app, |layout, cx| {
                layout.external_windows.routing_matrix = None;
                cx.notify();
            });
        });

        match crate::components::open_routing_matrix_window(
            owner_bounds,
            snapshot,
            on_toggle_send,
            on_close,
            cx,
        ) {
            Ok(handle) => self.external_windows.routing_matrix = Some(handle),
            Err(err) => eprintln!("[routing-matrix] failed to open window: {err}"),
        }
    }

    /// A track's persisted Soundfont Player settings, so an opening window shows
    /// the `.sf2` and preset the engine is already playing rather than an empty
    /// panel.
    fn soundfont_track_state(
        &self,
        track_id: &str,
        cx: &App,
    ) -> crate::components::soundfont_player_window::SoundfontPlayerTrackState {
        use crate::components::soundfont_player_window::SoundfontPlayerTrackState;
        let timeline = self.timeline.read(cx);
        let Some(track) = timeline.state.find_track(track_id) else {
            return SoundfontPlayerTrackState::default();
        };
        SoundfontPlayerTrackState {
            path: track.soundfont_path.clone(),
            preset: track.soundfont_preset,
            volume: track.soundfont_volume,
            reverb_chorus: track.soundfont_reverb_chorus,
            polyphony: track.soundfont_polyphony,
            envelope: track.soundfont_envelope,
            quality: track.soundfont_quality,
        }
    }

    /// Opens the built-in Soundfont Player MDI window, or focuses it (and its
    /// document) if already open. Called from the Inspector's Open button for
    /// an Instrument track whose `builtin_soundfont_player` marker is set.
    pub(super) fn open_soundfont_player_window(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        track_id: String,
        cx: &mut Context<Self>,
    ) {
        let initial = self.soundfont_track_state(&track_id, cx);
        if let Some(handle) = self.external_windows.soundfont_player.clone() {
            let activated = handle
                .update(cx, |window, w, cx| {
                    window.focus_soundfont_player(track_id.clone(), initial.clone(), cx);
                    w.activate_window();
                    cx.notify();
                })
                .is_ok();
            if activated {
                return;
            }
            self.external_windows.soundfont_player = None;
        }

        let studio = cx.entity().clone();
        let on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync> = Arc::new({
            let studio = studio.clone();
            move |_, app| {
                let _ = studio.update(app, |layout, cx| {
                    layout.external_windows.soundfont_player = None;
                    cx.notify();
                });
            }
        });
        let on_update_track: Arc<
            dyn Fn(crate::components::soundfont_player_window::SoundfontPlayerTrackUpdate, &mut App)
                + Send
                + Sync,
        > = Arc::new(move |update, app| {
            let _ = studio.update(app, |layout, cx| {
                let changed = layout.timeline.update(cx, |timeline, cx| {
                    let changed = timeline.state.set_track_soundfont_player_state(
                        &update.track_id,
                        update.settings.clone(),
                    );
                    if changed {
                        cx.notify();
                    }
                    changed
                });
                if changed {
                    layout.mark_dirty();
                    layout.audio_bridge.project_dirty = true;
                    layout.schedule_audio_project_sync(cx, false, "soundfont_player_update");
                }
                cx.notify();
            });
        });

        // The window's own player is control-side only. Auditioning routes
        // through the engine's MIDI preview so the sound comes from the track's
        // instrument — the same signal path playback uses.
        let studio = cx.entity().clone();
        let on_preview: Arc<
            dyn Fn(
                    &str,
                    crate::components::soundfont_player_window::SoundfontPlayerPreview,
                    &mut App,
                ) + Send
                + Sync,
        > = Arc::new(move |track_id, event, app| {
            let _ = studio.update(app, |layout, cx| {
                layout.dispatch_soundfont_preview(track_id, event, cx);
            });
        });

        match crate::components::soundfont_player_window::open_soundfont_player_window(
            owner_bounds,
            track_id,
            initial,
            on_close,
            on_update_track,
            on_preview,
            cx,
        ) {
            Ok(handle) => self.external_windows.soundfont_player = Some(handle),
            Err(err) => eprintln!("[soundfont-player] failed to open window: {err}"),
        }
    }

    pub(crate) fn open_mixer_external_window(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        external_mixer_debug("external mixer open requested");
        let owner_bounds =
            resolve_owner_bounds_with_preferred(owner_bounds, self.studio_window_bounds(cx), cx);
        self.external_windows.pending_mixer_open = owner_bounds;
        self.schedule_pending_mixer_external_open(cx);
        cx.notify();
    }

    pub(super) fn schedule_pending_mixer_external_open(&mut self, cx: &mut Context<Self>) {
        if self.external_windows.pending_mixer_open.is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(0))
                .await;
            let _ = this.update(cx, |layout, cx| {
                layout.flush_pending_mixer_external_open(cx)
            });
        })
        .detach();
    }

    pub(super) fn flush_pending_mixer_external_open(&mut self, cx: &mut Context<Self>) {
        let owner_bounds = resolve_owner_bounds_with_preferred(
            self.external_windows.pending_mixer_open.take(),
            self.studio_window_bounds(cx),
            cx,
        );
        let Some(owner_bounds) = owner_bounds else {
            return;
        };

        self.prune_mixer_window(cx);
        if let Some(handle) = self.external_windows.mixer.clone() {
            if handle
                .update(cx, |_mixer, window, _cx| window.activate_window())
                .is_ok()
            {
                self.panels.mixer_docked = false;
                self.push_mixer_snapshot_to_window(cx);
                cx.notify();
                return;
            }
            self.external_windows.mixer = None;
        }

        self.menu_bar.open_menu_id = None;
        self.menu_bar.submenu_path.clear();
        self.overlay.open_popover = None;
        self.panels.mixer_docked = false;

        let snapshot = self.build_mixer_snapshot(cx);
        let callbacks = self.build_mixer_callbacks(cx.entity().clone());
        let owner = cx.entity().clone();
        let on_close: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + Send + Sync> =
            std::sync::Arc::new(move |_window, cx| {
                let _ = owner.update(cx, |layout, cx| layout.note_mixer_window_closed(cx));
            });
        let scroll_owner = cx.entity().clone();
        let on_mixer_scroll: std::sync::Arc<
            dyn Fn(f32, &mut Window, &mut gpui::App) + Send + Sync,
        > = std::sync::Arc::new(move |new_x: f32, _w, cx| {
            let _ = scroll_owner.update(cx, |layout, cx| {
                if layout.set_mixer_scroll_x(new_x, cx) {
                    layout.push_mixer_snapshot_to_window(cx);
                }
            });
        });
        let split_owner = cx.entity().clone();
        let on_mixer_split: std::sync::Arc<
            dyn Fn(crate::components::mixer_panel::MixerSplitAction, &mut Window, &mut gpui::App)
                + Send
                + Sync,
        > = std::sync::Arc::new(move |action, _w, cx| {
            let _ =
                split_owner.update(cx, |layout, cx| layout.apply_mixer_split_action(action, cx));
        });
        let dispatch_owner = cx.entity().clone();
        let dispatch_key: std::sync::Arc<
            dyn Fn(&gpui::KeyDownEvent, &mut gpui::App) -> bool + Send + Sync,
        > = std::sync::Arc::new(move |event, cx| {
            if event.is_held {
                return false;
            }
            let Some(command_id) = dispatch_owner.read(cx).shortcut_command_id(event) else {
                return false;
            };
            let _ = dispatch_owner.update(cx, |layout, cx| {
                let owner_bounds = layout.studio_window_bounds(cx);
                layout.dispatch_command_id_from_bounds(&command_id, owner_bounds, cx);
            });
            true
        });

        match open_mixer_window(
            owner_bounds,
            snapshot,
            self.mixer_tree_sidebar.clone(),
            callbacks,
            on_close,
            on_mixer_scroll,
            on_mixer_split,
            dispatch_key,
            cx,
        ) {
            Ok(handle) => {
                self.external_windows.mixer = Some(handle);
                // Removing the docked mixer changes the arrangement's client
                // rectangle. Force both DirectComposition surfaces to repaint
                // once so no pixels from the old bottom-panel bounds survive.
                cx.refresh_windows();
                cx.notify();
            }
            Err(err) => {
                eprintln!("[mixer] failed to open external mixer window: {err}");
                self.panels.mixer_docked = true;
                self.set_active_panel(crate::layout::WorkspaceActivePanel::Mixer, cx);
                cx.notify();
            }
        }
    }

    pub(crate) fn close_mixer_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.external_windows.mixer.take() {
            let _ = handle.update(cx, |_mixer, window, _cx| window.remove_window());
        }
        cx.notify();
    }

    pub(super) fn note_mixer_window_closed(&mut self, cx: &mut Context<Self>) {
        self.external_windows.mixer = None;
        cx.notify();
    }

    pub(super) fn prune_mixer_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.external_windows.mixer.clone() else {
            return;
        };
        if handle.update(cx, |_mixer, _window, _cx| ()).is_err() {
            self.external_windows.mixer = None;
        }
    }

    pub(crate) fn open_midi_editor_external_window(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if self.selected_midi_clip_id(cx).is_none() {
            return;
        }
        let owner_bounds =
            resolve_owner_bounds_with_preferred(owner_bounds, self.studio_window_bounds(cx), cx);
        self.midi_editor.pending_open = owner_bounds;
        self.schedule_pending_midi_editor_open(cx);
        cx.notify();
    }

    pub(super) fn schedule_pending_midi_editor_open(&mut self, cx: &mut Context<Self>) {
        if self.midi_editor.pending_open.is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(0))
                .await;
            let _ = this.update(cx, |layout, cx| layout.flush_pending_midi_editor_open(cx));
        })
        .detach();
    }

    pub(super) fn flush_pending_midi_editor_open(&mut self, cx: &mut Context<Self>) {
        let owner_bounds = resolve_owner_bounds_with_preferred(
            self.midi_editor.pending_open.take(),
            self.studio_window_bounds(cx),
            cx,
        );
        let Some(owner_bounds) = owner_bounds else {
            return;
        };

        if let Some(OpenPopover::Context { request }) = self.overlay.open_popover.as_ref() {
            if let ContextMenuTarget::Clip(clip_id) = &request.target {
                let clip_id = clip_id.clone();
                if self
                    .timeline
                    .read(cx)
                    .state
                    .find_clip(&clip_id)
                    .is_some_and(|(_, c)| matches!(c.clip_type, ClipType::Midi { .. }))
                {
                    self.select_midi_clip(&clip_id, cx);
                }
            }
        }

        self.prune_midi_editor_window(cx);
        if let Some(handle) = self.midi_editor.window.clone() {
            if handle
                .update(cx, |_w, window, _cx| window.activate_window())
                .is_ok()
            {
                midi_editor_debug("focus existing window");
                if let Some(clip_id) = self.selected_midi_clip_id(cx) {
                    if let Some((track, clip)) = self.timeline.read(cx).state.find_clip(&clip_id) {
                        midi_editor_debug(&format!(
                            "switch target clip clip={} track={}",
                            clip.name, track.name
                        ));
                    }
                }
                cx.notify();
                return;
            }
            self.midi_editor.window = None;
        }

        let clip_label = self
            .selected_midi_clip_id(cx)
            .and_then(|id| self.timeline.read(cx).state.find_clip(&id))
            .map(|(t, c)| (c.name.clone(), t.name.clone()));
        if let Some((clip_name, track_name)) = clip_label.as_ref() {
            midi_editor_debug(&format!("open window clip={clip_name} track={track_name}"));
        } else {
            midi_editor_debug("open window (no MIDI clip selected)");
        }

        self.menu_bar.open_menu_id = None;
        self.menu_bar.submenu_path.clear();

        let timeline = self.timeline.clone();
        let piano_roll = self.piano_roll_floating.clone();
        let virtual_keyboard = self.virtual_keyboard.clone();
        let owner = cx.entity().clone();
        let on_close: Arc<dyn Fn(&mut Window, &mut gpui::App) + Send + Sync> =
            Arc::new(move |window, cx| {
                // Capture the id while the window is still alive. The deferred
                // cleanup runs after `remove_window`, so it must not consult the
                // stored handle for metadata from an already-destroyed window.
                let window_id = window.window_handle().window_id();
                StudioLayout::defer_update(&owner, cx, move |layout, cx| {
                    layout.note_midi_editor_window_closed(window_id, cx);
                });
            });
        let dispatch_owner = cx.entity().clone();
        let dispatch_command: Arc<dyn Fn(&'static str, &mut gpui::App) + Send + Sync> =
            Arc::new(move |command_id, cx| {
                let _ = dispatch_owner.update(cx, |layout, cx| {
                    layout.dispatch_command_id(command_id, cx);
                    cx.notify();
                });
            });

        match open_midi_editor_window(
            Some(owner_bounds),
            timeline,
            piano_roll,
            virtual_keyboard,
            on_close,
            dispatch_command,
            cx,
        ) {
            Ok(handle) => {
                // Register the popout as a musical-typing source so its held
                // notes are released if it closes (register doesn't flush, so it
                // is safe to call directly here).
                let window_id = handle.window_id();
                let _ = self
                    .virtual_keyboard
                    .update(cx, |keyboard, _cx| keyboard.register_window(window_id));
                midi_editor_debug(&format!(
                    "register virtual-keyboard window id={}",
                    window_id.as_u64()
                ));
                self.midi_editor.window = Some(handle);
                cx.notify();
            }
            Err(err) => eprintln!("[midi-editor] failed to open window: {err}"),
        }
    }

    pub(crate) fn close_midi_editor_window(&mut self, cx: &mut Context<Self>) {
        let _ = self.piano_roll_floating.update(cx, |roll, cx| {
            roll.preview_all_notes_off("editor_close", cx);
        });
        if let Some(handle) = self.midi_editor.window.take() {
            // Drop any musical-typing notes the popout still held, and never
            // touch the window handle after it is removed.
            self.unregister_virtual_keyboard_window(handle.window_id(), cx);
            let _ = handle.update(cx, |_w, window, _cx| window.remove_window());
        }
        cx.notify();
    }

    pub(super) fn note_midi_editor_window_closed(
        &mut self,
        window_id: gpui::WindowId,
        cx: &mut Context<Self>,
    ) {
        let _ = self.piano_roll_floating.update(cx, |roll, cx| {
            roll.preview_all_notes_off("editor_close", cx);
        });
        midi_editor_debug(&format!(
            "unregister virtual-keyboard window id={}",
            window_id.as_u64()
        ));
        self.unregister_virtual_keyboard_window(window_id, cx);
        self.midi_editor.window = None;
        cx.notify();
    }

    pub(super) fn prune_midi_editor_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.midi_editor.window.clone() else {
            return;
        };
        if handle.update(cx, |_w, _window, _cx| ()).is_err() {
            self.midi_editor.window = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_input_number_always_maps_to_one_channel() {
        let device = ("input-device".to_string(), 4);
        for format in [AudioFormat::Mono, AudioFormat::Stereo] {
            assert_eq!(
                dialog_audio_input_routing("Input 2", format, Some(&device)),
                TrackInputRouting::AudioDeviceChannel {
                    device_id: device.0.clone(),
                    channel: 1,
                }
            );
        }
    }

    #[test]
    fn dialog_stereo_pair_label_maps_to_first_pair_only_for_stereo() {
        let device = ("input-device".to_string(), 4);
        assert_eq!(
            dialog_audio_input_routing(
                FIRST_STEREO_PAIR_INPUT_LABEL,
                AudioFormat::Stereo,
                Some(&device),
            ),
            TrackInputRouting::AudioDeviceChannels {
                device_id: device.0.clone(),
                channels: vec![0, 1],
            }
        );
        assert_eq!(
            dialog_audio_input_routing(
                FIRST_STEREO_PAIR_INPUT_LABEL,
                AudioFormat::Mono,
                Some(&device),
            ),
            TrackInputRouting::AllInputs
        );
    }
}
