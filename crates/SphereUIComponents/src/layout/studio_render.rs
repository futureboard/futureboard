use super::*;

impl Render for StudioLayout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.session_install_status.is_ready() {
            eprintln!("[StudioMount] blocked because session not ready");
            return div()
                .id("futureboard-studio-root")
                .role(Role::Application)
                .aria_label("Futureboard Studio")
                .size_full()
                .bg(Colors::surface_base());
        }

        publish_studio_main_hwnd(window);

        // Perf probe: if the MAIN DAW window re-renders while the insert picker is
        // open, that points the typing-stutter at a full StudioLayout repaint
        // rather than the picker window. Gated by the existing picker debug flag.
        if self.plugin_picker.is_open && crate::components::plugin_picker::picker_perf_debug() {
            eprintln!(
                "[picker-perf] StudioLayout::render (main DAW window repainted while picker open)"
            );
        }

        let _root_scope = crate::perf::PerfScope::enter("StudioLayout");
        let i18n = crate::i18n::I18n::from_app(cx);
        self.browser_search_input.placeholder = Some(i18n.tr("search.browser.placeholder"));
        self.project_switcher_search_input.placeholder =
            Some(i18n.tr("search.projects.placeholder"));
        // Frame pacing tick. See FrameDiagnostics docs — only counts
        // real repaints, not display refreshes.
        let reason = self.frame_reason();
        let reason_static: &'static str = match reason {
            "transport" => "transport",
            "panel-resize" => "panel-resize",
            "menu" => "menu",
            _ => "idle/interaction",
        };
        self.frame_diag.tick(reason);
        crate::perf::tick_root_frame(reason_static);
        if self
            .settings
            .read(cx)
            .current
            .performance
            .show_status_performance_metrics
        {
            self.notify_status_bar_if_changed(cx);
        }
        // Re-resolve the frame-pacing mode from settings (env override still
        // wins) and republish the poll cadence. Cheap; applies a Settings change
        // on the next frame without a dedicated observer.
        let frame_rate_mode = self.settings.read(cx).current.performance.frame_rate;
        self.frame_scheduler.refresh_from_settings(frame_rate_mode);
        self.maybe_autosave_project(cx);
        self.window_hooks.cached_bounds = Some(window.bounds());
        self.flush_deferred_insert_editor_opens(window, cx);

        // Keep the OS window title in sync with the project lifecycle state
        // (Part G/H), e.g. "Untitled Project — Unsaved" / "My Song — Saved".
        let title = self.window_title();
        if self.last_window_title.as_deref() != Some(title.as_str()) {
            window.set_window_title(&title);
            self.last_window_title = Some(title.clone());
        }

        // Pull a render-only track snapshot and current selection from the
        // Timeline. Do not clone every clip here: MIDI clips can contain tens of
        // thousands of notes/controllers, and a mixer selection clears clip
        // selection anyway. Cloning the whole project made a strip click block
        // the UI for seconds. The Inspector only needs the selected clip, so add
        // that one back to the otherwise clip-free track snapshot.
        let (tracks, selected_track_id, selected_clip_id, project_bpm) = {
            let t = self.timeline.read(cx);
            let selected_clip_id = t.state.selection.selected_clip_ids.first().cloned();
            let selected_clip = selected_clip_id.as_deref().and_then(|clip_id| {
                t.state.tracks.iter().find_map(|track| {
                    track
                        .clips
                        .iter()
                        .find(|clip| clip.id == clip_id)
                        .map(|clip| (track.id.clone(), clip.clone()))
                })
            });
            let mut tracks: Vec<_> = t
                .state
                .tracks
                .iter()
                .map(|track| {
                    let mut cloned = super::mixer_ops::clone_track_for_mixer(track);
                    let display_volume = t.state.display_track_volume(track);
                    cloned.volume = display_volume;
                    cloned.volume_effective = display_volume;
                    cloned
                })
                .collect();
            if let Some((track_id, clip)) = selected_clip {
                if let Some(track) = tracks.iter_mut().find(|track| track.id == track_id) {
                    track.clips.push(clip);
                }
            }
            (
                tracks,
                t.state.selection.selected_track_id.clone(),
                selected_clip_id,
                t.state.bpm as f64,
            )
        };

        let inspector_callbacks = self.build_inspector_callbacks(cx.entity().clone());

        // The audio-input combo lists logical connections, so no device
        // enumeration happens for it at all. The port inventory is read only
        // while the combo is open, and only to describe the selected bus.
        let audio_input_ports = if self.overlay.inspector_routing_combo
            == Some(crate::components::panel::InspectorRoutingCombo::AudioInput)
        {
            crate::audio_connections::current_available_ports()
        } else {
            crate::audio_connections::AvailablePorts::default()
        };
        let audio_output_buses: Vec<(String, String)> = if self.overlay.inspector_routing_combo
            == Some(crate::components::panel::InspectorRoutingCombo::AudioOutput)
        {
            tracks
                .iter()
                .filter(|track| {
                    crate::components::timeline::timeline_state::is_project_routing_track(track)
                })
                .map(|track| (track.id.clone(), track.name.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let audio_output_device = if self.overlay.inspector_routing_combo
            == Some(crate::components::panel::InspectorRoutingCombo::AudioOutput)
        {
            self.selected_output_device_channels(cx)
        } else {
            None
        };
        let instrument_targets: Vec<(String, String)> = if self.overlay.inspector_routing_combo
            == Some(crate::components::panel::InspectorRoutingCombo::MidiOut)
        {
            tracks
                .iter()
                .filter(|track| {
                    track.track_type
                        == crate::components::timeline::timeline_state::TrackType::Instrument
                })
                .map(|track| (track.id.clone(), track.name.clone()))
                .collect()
        } else {
            Vec::new()
        };
        // Real MIDI hardware/virtual ports, enabled in Preferences → MIDI —
        // the same cached registry Settings renders from (`device_registry`),
        // not a mocked/empty placeholder. Only enabled ports are offered as
        // routing targets, matching the Preferences toggle the user set.
        let (detected_midi_inputs, detected_midi_outputs): (Vec<String>, Vec<String>) = if matches!(
            self.overlay.inspector_routing_combo,
            Some(crate::components::panel::InspectorRoutingCombo::MidiInput)
                | Some(crate::components::panel::InspectorRoutingCombo::MidiOut)
        ) {
            let saved = self.settings.read(cx).current.hardware.midi.devices.clone();
            let detected = crate::device_registry::cached_midi_devices();
            let resolved = sphere_midi_service::resolve_midi_devices(&saved, &detected);
            let inputs = resolved
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
                .collect();
            let outputs = resolved
                .iter()
                .filter(|d| {
                    d.enabled
                        && matches!(
                            d.direction,
                            crate::settings::MidiDeviceDirection::Output
                                | crate::settings::MidiDeviceDirection::InputOutput
                        )
                })
                .map(|d| d.name.clone())
                .collect();
            (inputs, outputs)
        } else {
            (Vec::new(), Vec::new())
        };
        let inspector_routing_combo_overlay: Option<gpui::AnyElement> =
            if let (Some(combo), Some(anchor)) = (
                self.overlay.inspector_routing_combo,
                self.overlay.inspector_routing_combo_anchor,
            ) {
                selected_track_id.as_deref().and_then(|tid| {
                    tracks.iter().find(|t| t.id == tid).map(|track| {
                        let combo_audio_connections =
                            self.timeline.read(cx).state.audio_connections.clone();
                        let close = Arc::new({
                            let this = cx.entity().clone();
                            move |cx: &mut gpui::App| {
                                let _ = this.update(cx, |layout, cx| {
                                    layout.overlay.inspector_routing_combo = None;
                                    layout.overlay.inspector_routing_combo_anchor = None;
                                    cx.notify();
                                });
                            }
                        });
                        crate::components::panel::inspector_routing_combo_overlay(
                            track,
                            &combo_audio_connections,
                            combo,
                            anchor,
                            window,
                            &inspector_callbacks,
                            close,
                            audio_input_ports.clone(),
                            audio_output_buses.clone(),
                            audio_output_device.clone(),
                            instrument_targets.clone(),
                            detected_midi_inputs.clone(),
                            detected_midi_outputs.clone(),
                        )
                        .into_any_element()
                    })
                })
            } else {
                None
            };

        // Reconcile the Inspector name field with the current track selection.
        // Only reload when the bound track actually changes, so typing into the
        // field for the *selected* track is never clobbered mid-edit.
        if self.inspector_name_edit.name_bound.as_deref() != selected_track_id.as_deref() {
            match selected_track_id
                .as_deref()
                .and_then(|tid| tracks.iter().find(|t| t.id == tid))
            {
                Some(t) => {
                    self.inspector_name_edit
                        .name_input
                        .set_value(t.name.clone());
                    self.inspector_name_edit.name_bound = Some(t.id.clone());
                }
                None => {
                    self.inspector_name_edit.name_input.set_value("");
                    self.inspector_name_edit.name_bound = None;
                }
            }
        }
        let inspector_name_focused = self.inspector_name_edit.name_input.is_focused(window);
        if self.inspector_name_edit.clip_name_bound.as_deref() != selected_clip_id.as_deref() {
            match selected_clip_id.as_deref().and_then(|cid| {
                tracks
                    .iter()
                    .find_map(|t| t.clips.iter().find(|c| c.id == cid))
            }) {
                Some(c) => {
                    self.inspector_name_edit
                        .clip_name_input
                        .set_value(c.name.clone());
                    self.inspector_name_edit.clip_name_bound = Some(c.id.clone());
                }
                None => {
                    self.inspector_name_edit.clip_name_input.set_value("");
                    self.inspector_name_edit.clip_name_bound = None;
                }
            }
        }
        if self.panels.inspector {
            if let Some(target) = self.overlay.pending_text_focus.take() {
                match target {
                    TextMenuTarget::InspectorName => {
                        self.inspector_name_edit.name_input.select_all();
                        self.inspector_name_edit
                            .name_input
                            .focus_handle
                            .focus(window, cx);
                    }
                    TextMenuTarget::InspectorClipName => {
                        self.inspector_name_edit.clip_name_input.select_all();
                        self.inspector_name_edit
                            .clip_name_input
                            .focus_handle
                            .focus(window, cx);
                    }
                    _ => {}
                }
            }
        }
        let inspector_clip_name_focused =
            self.inspector_name_edit.clip_name_input.is_focused(window);
        let inspector_name_callbacks = crate::components::text_input::bind_mouse_selection(
            cx.entity().clone(),
            |layout: &mut StudioLayout| &mut layout.inspector_name_edit.name_input,
        );
        let inspector_clip_name_callbacks = crate::components::text_input::bind_mouse_selection(
            cx.entity().clone(),
            |layout: &mut StudioLayout| &mut layout.inspector_name_edit.clip_name_input,
        );
        let inspector_color_hex_focused = self
            .inspector_name_edit
            .color_picker
            .hex_input
            .is_focused(window);
        let inspector_color_hex_callbacks = crate::components::text_input::bind_mouse_selection(
            cx.entity().clone(),
            |layout: &mut StudioLayout| &mut layout.inspector_name_edit.color_picker.hex_input,
        );
        let inspector_color_hex_context: Arc<
            dyn Fn(&(f32, f32), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            Arc::new(move |(x, y): &(f32, f32), _window, cx| {
                let x = *x;
                let y = *y;
                let _ = this.update(cx, |this, cx| {
                    this.overlay.text_context_menu = Some(TextContextMenu {
                        target: TextMenuTarget::InspectorColorHex,
                        x,
                        y,
                    });
                    cx.notify();
                });
            })
        };
        let inspector_color_hex_callbacks = TextInputCallbacks {
            on_context_command: None,
            on_context_menu: Some(inspector_color_hex_context),
            on_mouse: inspector_color_hex_callbacks.on_mouse,
        };
        let inspector_color_callbacks = self
            .build_inspector_color_picker_callbacks(cx.entity().clone(), selected_track_id.clone());
        let inspector_color_open = self.inspector_name_edit.color_picker.open;

        crate::perf::count("tracks", tracks.len() as u64);

        // ── File browser callbacks ──────────────────────────────────────
        let on_browser_search_context: std::sync::Arc<
            dyn Fn(&(f32, f32), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |(x, y): &(f32, f32), _w, cx| {
                let x = *x;
                let y = *y;
                let _ = this.update(cx, |this, cx| {
                    this.menu_bar.open_menu_id = None;
                    this.menu_bar.submenu_path.clear();
                    this.project_switcher.is_open = false;
                    this.overlay.text_context_menu = Some(TextContextMenu {
                        target: TextMenuTarget::BrowserSearch,
                        x,
                        y,
                    });
                    cx.notify();
                });
            })
        };
        let browser_search_mouse_callbacks = crate::components::text_input::bind_mouse_selection(
            cx.entity().clone(),
            |layout: &mut StudioLayout| &mut layout.browser_search_input,
        );
        let browser_search_callbacks = TextInputCallbacks {
            on_context_command: None,
            on_context_menu: Some(on_browser_search_context),
            on_mouse: browser_search_mouse_callbacks.on_mouse,
        };

        let on_browser_toggle: std::sync::Arc<
            dyn Fn(&(String, Option<PathBuf>), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |(id, path): &(String, Option<PathBuf>), _w, cx| {
                let id = id.clone();
                let path = path.clone();
                let _ = this.update(cx, |this, cx| {
                    let expanded = this.file_browser.toggle_node(&id, path.as_deref());
                    if expanded {
                        // Drain any newly-expanded paths whose contents
                        // haven't been indexed yet and kick off a
                        // background load for each.
                        let pending = this.file_browser.paths_needing_load();
                        for p in pending {
                            this.file_browser.mark_loading(p.clone());
                            this.spawn_directory_load(cx, p);
                        }
                    }
                    cx.notify();
                });
            })
        };
        let on_browser_select: std::sync::Arc<
            dyn Fn(&PathBuf, &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |path: &PathBuf, _w, cx| {
                let path = path.clone();
                this.update(cx, |this, cx| {
                    this.file_browser.select(path.clone());
                    if crate::components::file_browser::is_audio_path(&path) {
                        // Visual mini-waveform preview always decodes on select.
                        this.ensure_browser_waveform(path.clone(), cx);
                        // Browser selection is also a real audible audition;
                        // decode happens off-thread in the engine, so this UI
                        // event only queues work and never blocks rendering.
                        if this.file_browser.preview_enabled {
                            let _ = this.audition_browser_file(&path);
                        }
                    }
                    cx.notify();
                });
            })
        };
        // Double-click on an audio file imports it onto the timeline using the
        // existing waveform-cache + import_audio_at path.
        let on_browser_activate: std::sync::Arc<
            dyn Fn(&PathBuf, &mut Window, &mut gpui::App) + 'static,
        > = {
            let timeline = self.timeline.clone();
            let layout = cx.entity().clone();
            std::sync::Arc::new(move |path: &PathBuf, _w, cx| {
                // Filter on extension before mutating timeline state so
                // double-clicking a non-audio file (e.g. .txt, .png) does
                // not create a phantom clip with the 8-bar fallback
                // duration that never resolves to real metadata.
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                if !is_supported_audio_ext(&ext) {
                    eprintln!(
                        "[import] ignoring non-audio activation: ext='{}' path={}",
                        ext,
                        path.display()
                    );
                    return;
                }

                let path = path.clone();
                let path_for_decode = path.clone();
                let timeline_for_decode = timeline.clone();
                timeline.update(cx, |t, cx| {
                    let path_key = path.to_string_lossy().to_string();
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Imported Audio".to_string());
                    t.state
                        .import_audio_to_selected_or_new_track(path_key, name);
                    cx.notify();
                });
                let _ = layout.update(cx, |this, cx| {
                    this.mark_dirty();
                    this.mark_engine_media_dirty();
                    this.schedule_audio_project_sync(cx, false, "timeline_audio_import");
                });
                let path_key = path_for_decode.to_string_lossy().to_string();
                let _ = layout.update(cx, move |this, cx| {
                    this.spawn_timeline_audio_import_jobs(
                        cx,
                        timeline_for_decode,
                        path_for_decode,
                        path_key,
                    );
                });
            })
        };
        let on_browser_context: std::sync::Arc<
            dyn Fn(&(Option<PathBuf>, f32, f32), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(
                move |(path, x, y): &(Option<PathBuf>, f32, f32), window, cx| {
                    let path = path.clone();
                    let x = *x;
                    let y = *y;
                    let window_id = window.window_handle().window_id();
                    StudioLayout::defer_update(&this, cx, move |this, cx| {
                        this.try_open_context_menu(
                            ContextMenuRequest::new(
                                window_id,
                                x,
                                y,
                                ContextMenuTarget::Extended(ContextTarget::Browser(path)),
                            ),
                            cx,
                        );
                    });
                },
            )
        };

        // Toolbar: collapse every expanded folder in one click.
        let on_browser_collapse_all: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |_w, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.file_browser.collapse_all();
                    cx.notify();
                });
            })
        };
        // Toolbar: drop cached listings for expanded folders and re-scan them.
        let on_browser_rescan: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |_w, cx| {
                let _ = this.update(cx, |this, cx| {
                    let paths = this.file_browser.invalidate_expanded();
                    for p in paths {
                        this.file_browser.mark_loading(p.clone());
                        this.spawn_directory_load(cx, p);
                    }
                    cx.notify();
                });
            })
        };
        let file_browser = self.file_browser.clone();
        let browser_scroll = self.browser_scroll.clone();

        let on_timeline_context: components::timeline::timeline::TimelineContextMenuCb = {
            let this = cx.entity().clone();
            std::sync::Arc::new(
                move |(target, x, y): &(TimelineContextTarget, f32, f32), window, cx| {
                    let target = target.clone();
                    let x = *x;
                    let y = *y;
                    let window_id = window.window_handle().window_id();
                    StudioLayout::defer_update(&this, cx, move |this, cx| {
                        let context_target = match target {
                            TimelineContextTarget::TimelineEmpty => ContextTarget::TimelineEmpty,
                            TimelineContextTarget::TrackLane { track_id, beat } => {
                                ContextTarget::TrackLane { track_id, beat }
                            }
                            TimelineContextTarget::TrackHeader(id) => {
                                this.timeline.update(cx, |timeline, cx| {
                                    timeline.state.select_track(&id);
                                    cx.notify();
                                });
                                ContextTarget::Track(id)
                            }
                            TimelineContextTarget::Clip(id) => {
                                this.timeline.update(cx, |timeline, cx| {
                                    timeline.state.select_clip(&id);
                                    cx.notify();
                                });
                                ContextTarget::Clip(id)
                            }
                            TimelineContextTarget::AudioClip { clip_id, .. }
                            | TimelineContextTarget::MidiClip { clip_id, .. } => {
                                this.timeline.update(cx, |timeline, cx| {
                                    if !timeline
                                        .state
                                        .selection
                                        .selected_clip_ids
                                        .iter()
                                        .any(|id| id == &clip_id)
                                    {
                                        timeline.state.select_clip(&clip_id);
                                        cx.notify();
                                    }
                                });
                                ContextTarget::Clip(clip_id)
                            }
                            TimelineContextTarget::Marker { marker_id, beat } => {
                                ContextTarget::TimelineMarker { marker_id, beat }
                            }
                            TimelineContextTarget::SongTextMarker { event_id, beat } => {
                                ContextTarget::SongTextMarker { event_id, beat }
                            }
                            TimelineContextTarget::AutomationLane {
                                track_id,
                                lane_id,
                                beat,
                            } => ContextTarget::AutomationLane {
                                track_id,
                                lane_id,
                                beat,
                            },
                            TimelineContextTarget::Ruler(beat) => {
                                ContextTarget::TimelineRuler { beat }
                            }
                            TimelineContextTarget::TempoTrack {
                                beat,
                                bpm,
                                point_id,
                            } => ContextTarget::TempoTrack {
                                beat,
                                bpm,
                                point_id,
                            },
                            TimelineContextTarget::TimeSignatureTrack { beat, point_id } => {
                                ContextTarget::TimeSignatureTrack { beat, point_id }
                            }
                            TimelineContextTarget::MarkerTrack { beat, marker_id } => {
                                ContextTarget::MarkerTrack { beat, marker_id }
                            }
                            TimelineContextTarget::RegionTrack { beat, region_id } => {
                                ContextTarget::RegionTrack { beat, region_id }
                            }
                            TimelineContextTarget::MarkerLaneHeader => ContextTarget::MarkerLane,
                            TimelineContextTarget::RegionLaneHeader => ContextTarget::RegionLane,
                            TimelineContextTarget::TempoLaneHeader => ContextTarget::Tempo,
                            TimelineContextTarget::TimeSignatureLaneHeader => {
                                ContextTarget::TimeSignature
                            }
                            TimelineContextTarget::AutomationTargetPicker { track_id } => {
                                ContextTarget::AutomationTargetPicker { track_id }
                            }
                        };
                        this.try_open_context_menu(
                            ContextMenuRequest::new(
                                window_id,
                                x,
                                y,
                                ContextMenuTarget::from_context_target(context_target),
                            ),
                            cx,
                        );
                    });
                },
            )
        };
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.set_context_menu_callback(Some(on_timeline_context));
        });

        let on_automation_control: components::timeline::automation_control_lane::AutomationControlCallback = {
            let this = cx.entity().clone();
            std::sync::Arc::new(
                move |(track_id, action, x, y): &(
                    String,
                    components::timeline::automation_control_lane::AutomationControlAction,
                    f32,
                    f32,
                ),
                      window: &mut gpui::Window,
                      cx: &mut gpui::App| {
                    let track_id = track_id.clone();
                    let action = *action;
                    let x = *x;
                    let y = *y;
                    StudioLayout::defer_update_in_window(&this, window, cx, move |this, window, cx| {
                        this.handle_automation_control_action(&track_id, action, x, y, window, cx);
                    });
                },
            )
        };
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.set_automation_control_callback(Some(on_automation_control));
        });

        let on_add_track: components::timeline::timeline::TimelineAddTrackCb = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |request, window, cx| {
                let request = *request;
                // Timeline's Add button fires while Timeline is mid-update
                // (`cx.listener`). Opening the dialog must not `timeline.read`
                // until that lease ends — defer like automation-control.
                StudioLayout::defer_update_in_window(
                    &this,
                    window,
                    cx,
                    move |this, _window, cx| {
                        this.open_add_track_external_window_with_context(
                            AddTrackKind::Audio,
                            request.track_count,
                            request.has_master_track,
                            None,
                            cx,
                        );
                    },
                );
            })
        };
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.set_add_track_callback(Some(on_add_track));
        });

        // ── Top-menu callbacks ─────────────────────────────────────────────
        let on_open_menu: std::sync::Arc<
            dyn Fn(&(String, f32), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |(id, anchor_x): &(String, f32), _w, cx| {
                let id = id.clone();
                let anchor_x = *anchor_x;
                this.update(cx, |this, cx| {
                    if this.menu_bar.open_menu_id.as_deref() == Some(id.as_str()) {
                        this.menu_bar.open_menu_id = None;
                    } else {
                        this.menu_bar.open_menu_id = Some(id);
                        this.menu_bar.anchor = titlebar_label_anchor(anchor_x);
                    }
                    this.menu_bar.submenu_path.clear();
                    this.overlay.open_popover = None;
                    this.project_switcher.is_open = false;
                    cx.notify();
                });
            })
        };
        let on_close_menu: std::sync::Arc<dyn Fn(&(), &mut Window, &mut gpui::App) + 'static> = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |_: &(), _w, cx| {
                this.update(cx, |this, cx| {
                    this.menu_bar.open_menu_id = None;
                    this.menu_bar.submenu_path.clear();
                    cx.notify();
                });
            })
        };
        let on_toggle_submenu: std::sync::Arc<
            dyn Fn(&(usize, String), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |(depth, id): &(usize, String), _w, cx| {
                let depth = *depth;
                let id = id.clone();
                this.update(cx, |this, cx| {
                    // Truncate the path to this depth, then toggle: if the
                    // requested id is already open at this depth, close it;
                    // otherwise open it (closing anything deeper).
                    let already_open = this.menu_bar.submenu_path.get(depth) == Some(&id);
                    this.menu_bar.submenu_path.truncate(depth);
                    if !already_open {
                        this.menu_bar.submenu_path.push(id);
                    }
                    cx.notify();
                });
            })
        };
        let on_menu_command: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |command: &String, w, cx| {
                let command = command.clone();
                let _ = this.update(cx, |this, cx| {
                    this.dispatch_command_id_from_bounds(&command, Some(w.bounds()), cx);
                    this.overlay.open_popover = None;
                    this.project_switcher.is_open = false;
                    cx.notify();
                });
            })
        };
        let on_project_open: std::sync::Arc<dyn Fn(&f32, &mut Window, &mut gpui::App) + 'static> = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |anchor_x: &f32, w, cx| {
                let anchor_x = *anchor_x;
                let _ = this.update(cx, |this, cx| {
                    this.menu_bar.open_menu_id = None;
                    this.menu_bar.submenu_path.clear();
                    this.overlay.open_popover = None;
                    this.overlay.text_context_menu = None;
                    this.project_switcher.is_open = !this.project_switcher.is_open;
                    this.project_switcher.anchor = project_title_anchor(anchor_x);
                    if this.project_switcher.is_open {
                        this.project_switcher.query.clear();
                        this.project_switcher_search_input.set_value("");
                        this.project_switcher_search_input.focus_handle.focus(w, cx);
                        this.project_switcher.selected_index = 0;
                        // Refresh which recents still exist on disk — off the UI
                        // thread, so opening the switcher never blocks on per-entry
                        // filesystem stats (a multi-hundred-ms stall on OneDrive).
                        this.spawn_refresh_recent_missing(cx);
                    }
                    cx.notify();
                });
            })
        };

        let open_menu_id = self.menu_bar.open_menu_id.clone();
        let menu_anchor = self.menu_bar.anchor;
        let submenu_path = self.menu_bar.submenu_path.clone();
        let viewport_width: f32 = window.bounds().size.width.into();
        let viewport_height: f32 = window.bounds().size.height.into();
        let ui_language = self.settings.read(cx).current.general.language.clone();
        let i18n = crate::i18n::I18n::new(&ui_language);

        let chrome_policy = crate::platform_chrome::PlatformChromePolicy::current();
        let dropdown_overlay = if chrome_policy.show_in_window_menubar {
            open_menu_id.as_ref().and_then(|id| {
                if id == components::menu_bar::MENU_PICKER_ID {
                    Some(
                        components::menu_bar::menu_picker_dropdown(
                            menu_anchor,
                            viewport_width,
                            viewport_height,
                            on_open_menu.clone(),
                            on_close_menu.clone(),
                            i18n,
                        )
                        .into_any_element(),
                    )
                } else {
                    let manifest = crate::menu::MenuManifest::load();
                    manifest.menus.iter().find(|m| &m.id == id).map(|menu| {
                        let mut runtime_menu = menu.clone();
                        let perf = self.settings.read(cx).current.performance.clone();
                        crate::menu::patch_checkbox_states(
                            &mut runtime_menu.items,
                            &[
                                ("window.show_browser", self.panels.browser),
                                ("window.show_inspector", self.panels.inspector),
                                // "Show Mixer" is checked when the mixer is on
                                // screen, which is what the command now toggles
                                // — not merely when the dock happens to be open
                                // on some other tab.
                                ("window.show_mixer", self.mixer_panel_chrome_visible()),
                                (
                                    "view.developer.perf_metrics",
                                    perf.show_status_performance_metrics,
                                ),
                                ("view.developer.perf_overlay", perf.show_performance_overlay),
                            ],
                        );
                        components::menu_dropdown::menu_dropdown(
                            &runtime_menu,
                            menu_anchor,
                            viewport_width,
                            viewport_height,
                            &submenu_path,
                            on_toggle_submenu.clone(),
                            on_menu_command.clone(),
                            on_close_menu.clone(),
                            i18n,
                        )
                        .into_any_element()
                    })
                }
            })
        } else {
            None
        };
        let on_close_popover: std::sync::Arc<dyn Fn(&(), &mut Window, &mut gpui::App) + 'static> = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |_: &(), _w, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.overlay.open_popover = None;
                    this.project_switcher.is_open = false;
                    this.command_palette.close();
                    this.overlay.text_context_menu = None;
                    cx.notify();
                });
            })
        };
        let on_popover_command: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(move |command: &String, w, cx| {
                let command = command.clone();
                let _ = this.update(cx, |this, cx| {
                    if this.overlay.open_popover.is_some()
                        && !this.validate_open_context_menu_action(cx)
                    {
                        eprintln!("[ContextMenu] action target stale, ignored");
                        this.close_context_menu(cx);
                        return;
                    }
                    this.dispatch_command_id_from_bounds(&command, Some(w.bounds()), cx);
                    this.close_context_menu(cx);
                    this.project_switcher.is_open = false;
                    this.command_palette.close();
                    cx.notify();
                });
            })
        };
        let on_switcher_row_action: std::sync::Arc<
            dyn Fn(
                    components::project_switcher::ProjectSwitcherRowEvent,
                    &mut Window,
                    &mut gpui::App,
                ) + Send
                + Sync,
        > = {
            let this = cx.entity().clone();
            std::sync::Arc::new(
                move |event: components::project_switcher::ProjectSwitcherRowEvent, w, cx| {
                    let _ = this.update(cx, |this, cx| {
                        let owner_bounds = Some(w.bounds());
                        match event {
                            components::project_switcher::ProjectSwitcherRowEvent::CurrentProject => {
                                this.handle_project_switch_current_row(cx);
                            }
                            components::project_switcher::ProjectSwitcherRowEvent::SwitchProject {
                                path,
                                name,
                                is_missing: _,
                            } => {
                                this.request_switch_project(
                                    crate::layout::project_switch::ProjectSwitchRequest {
                                        target_path: path,
                                        target_name: Some(name),
                                        source:
                                            crate::layout::project_switch::ProjectSwitchSource::ProjectSwitcher,
                                    },
                                    owner_bounds,
                                    cx,
                                );
                            }
                        }
                    });
                },
            )
        };
        let popover_overlay = if self.command_palette.is_open {
            let search_mouse_callbacks = crate::components::text_input::bind_mouse_selection(
                cx.entity().clone(),
                |layout: &mut StudioLayout| &mut layout.command_palette_input,
            );
            let on_palette_close: std::sync::Arc<
                dyn Fn(&(), &mut Window, &mut gpui::App) + 'static,
            > = {
                let this = cx.entity().clone();
                std::sync::Arc::new(move |_: &(), w, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.command_palette.close();
                        this.focus_handle.focus(w, cx);
                        cx.notify();
                    });
                })
            };
            let on_palette_command: std::sync::Arc<
                dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
            > = {
                let this = cx.entity().clone();
                std::sync::Arc::new(move |command: &String, w, cx| {
                    let command = command.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.command_palette.close();
                        this.focus_handle.focus(w, cx);
                        this.dispatch_command_id_from_bounds(&command, Some(w.bounds()), cx);
                        cx.notify();
                    });
                })
            };
            Some(
                components::command_palette_overlay(
                    &self.command_palette,
                    &self.command_palette_input,
                    self.command_palette_input.is_focused(window),
                    search_mouse_callbacks,
                    viewport_width,
                    viewport_height,
                    on_palette_command,
                    on_palette_close,
                )
                .into_any_element(),
            )
        } else if self.project_switcher.is_open {
            let search_mouse_callbacks = crate::components::text_input::bind_mouse_selection(
                cx.entity().clone(),
                |layout: &mut StudioLayout| &mut layout.project_switcher_search_input,
            );
            let search_context_callbacks = TextInputCallbacks {
                on_context_command: None,
                on_context_menu: Some(Arc::new({
                    let this = cx.entity().clone();
                    move |(x, y): &(f32, f32), _w, cx| {
                        let x = *x;
                        let y = *y;
                        let _ = this.update(cx, |this, cx| {
                            this.overlay.text_context_menu = Some(TextContextMenu {
                                target: TextMenuTarget::ProjectSwitcherSearch,
                                x,
                                y,
                            });
                            cx.notify();
                        });
                    }
                })),
                on_mouse: search_mouse_callbacks.on_mouse,
            };
            Some(
                components::project_switcher::project_switcher_popover(
                    &self.project_switcher,
                    &self.project_switcher_search_input,
                    self.project_switcher_search_input.is_focused(window),
                    search_context_callbacks,
                    viewport_width,
                    viewport_height,
                    on_switcher_row_action.clone(),
                    on_popover_command.clone(),
                    on_close_popover.clone(),
                    i18n,
                )
                .into_any_element(),
            )
        } else {
            match self.overlay.open_popover.clone() {
                Some(OpenPopover::Context { request }) => {
                    let target = request.target.to_context_target();
                    Some(
                        components::context_menu::context_menu_overlay(
                            self.context_entries(&target, cx),
                            request.x,
                            request.y,
                            viewport_width,
                            viewport_height,
                            on_popover_command.clone(),
                            on_close_popover.clone(),
                        )
                        .into_any_element(),
                    )
                }
                Some(OpenPopover::AutomationTargetPicker { track_id, x, y }) => {
                    use crate::components::timeline::automation_target_picker::automation_target_picker_overlay;

                    self.automation_picker_query =
                        self.automation_picker_search_input.value.clone();
                    let model = self
                        .timeline
                        .read(cx)
                        .state
                        .automation_picker_model(&track_id)
                        .unwrap_or_default();
                    let search_callbacks = crate::components::text_input::bind_mouse_selection(
                        cx.entity().clone(),
                        |layout: &mut StudioLayout| &mut layout.automation_picker_search_input,
                    );
                    Some(
                        automation_target_picker_overlay(
                            &model,
                            &track_id,
                            &self.automation_picker_query,
                            &self.automation_picker_search_input,
                            self.automation_picker_search_input.is_focused(window),
                            x,
                            y,
                            viewport_width,
                            viewport_height,
                            on_popover_command.clone(),
                            on_close_popover.clone(),
                            search_callbacks,
                        )
                        .into_any_element(),
                    )
                }
                None => None,
            }
        };
        // Settings is now an external window — no overlay needed.
        let settings_overlay: Option<gpui::AnyElement> = None;
        let text_context_overlay = self.overlay.text_context_menu.map(|menu| {
            let clipboard_has_text = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| !text.is_empty());
            let entries =
                text_input_context_entries(self.text_input(menu.target), clipboard_has_text);
            let command_target = cx.entity().clone();
            let close_target = cx.entity().clone();
            components::context_menu::context_menu_overlay(
                entries,
                menu.x,
                menu.y,
                viewport_width,
                viewport_height,
                Arc::new(move |command: &String, _window, cx| {
                    let command = command.clone();
                    let _ = command_target.update(cx, |this, cx| {
                        if let Some(menu) = this.overlay.text_context_menu {
                            let input = this.text_input_mut(menu.target);
                            let _ = input.apply_context_command(&command, cx);
                            this.sync_text_input_target(menu.target);
                        }
                        this.overlay.text_context_menu = None;
                        cx.notify();
                    });
                }),
                Arc::new(move |_: &(), _window, cx| {
                    let _ = close_target.update(cx, |this, cx| {
                        this.overlay.text_context_menu = None;
                        cx.notify();
                    });
                }),
            )
        });
        // Add Track moved to an external window.
        let virtual_keyboard_overlay = {
            let visible = self.virtual_keyboard.read(cx).state.visible;
            if visible {
                let window_active = window.is_window_active();
                if self.virtual_keyboard_window_active && !window_active {
                    // Deferred + panel-only: releasing through the sink here (we
                    // are inside StudioLayout::render's lease) would re-enter
                    // StudioLayout::update and panic. This is the multi-window
                    // crash path (focus leaving for / closing the popout editor).
                    self.defer_release_virtual_keyboard_notes(cx);
                }
                self.virtual_keyboard_window_active = window_active;
                let status = self.resolve_virtual_keyboard_target(cx);
                let target_key = status.target.as_ref().map(|target| {
                    format!(
                        "{}:{}",
                        target.track_id,
                        target.plugin_instance_id.as_deref().unwrap_or("")
                    )
                });
                if self.virtual_keyboard_last_target != target_key {
                    self.defer_release_virtual_keyboard_notes(cx);
                    self.virtual_keyboard_last_target = target_key;
                }
                let label = status.label;
                let hint = status.hint;
                let _ = self.virtual_keyboard.update(cx, |panel, cx| {
                    panel.set_target_status(label, hint);
                    cx.notify();
                });
                Some(self.virtual_keyboard.clone().into_any_element())
            } else {
                self.virtual_keyboard_last_target = None;
                self.virtual_keyboard_window_active = window.is_window_active();
                None
            }
        };

        // Phase 2b insert plugin picker overlay.
        let plugin_picker_overlay_el: Option<gpui::AnyElement> = if self.plugin_picker.is_open
            && self.plugin_picker_window.is_none()
        {
            let search_mouse_callbacks = crate::components::text_input::bind_mouse_selection(
                cx.entity().clone(),
                |layout: &mut StudioLayout| &mut layout.plugin_picker_search_input,
            );
            let search_context_callbacks = TextInputCallbacks {
                on_context_command: None,
                on_context_menu: Some(Arc::new({
                    let this = cx.entity().clone();
                    move |(x, y): &(f32, f32), _w, cx| {
                        let x = *x;
                        let y = *y;
                        let _ = this.update(cx, |this, cx| {
                            this.overlay.text_context_menu = Some(TextContextMenu {
                                target: TextMenuTarget::PluginPickerSearch,
                                x,
                                y,
                            });
                            cx.notify();
                        });
                    }
                })),
                on_mouse: search_mouse_callbacks.on_mouse,
            };
            let picker_callbacks = PluginPickerCallbacks {
                on_close: Arc::new({
                    let this = cx.entity().clone();
                    move |_: &(), _w, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.plugin_picker = PluginPickerState::closed();
                            cx.notify();
                        });
                    }
                }),
                on_select: Arc::new({
                    let this = cx.entity().clone();
                    move |plugin_id: &String, _w, cx| {
                        let plugin_id = plugin_id.clone();
                        let _ = this.update(cx, |this, cx| {
                            if let Some(index) = this.plugin_search_index.as_ref() {
                                let result = compute_filter_result(
                                    index,
                                    &this.plugin_picker.query,
                                    &this.plugin_picker.filters,
                                    &this.plugin_picker_prefs,
                                    std::env::var_os("FUTUREBOARD_PLUGIN_PICKER_DEBUG").is_some(),
                                );
                                if let Some(highlight) = result.indices.iter().position(|&idx| {
                                    index.plugin_at(idx).is_some_and(|p| p.id == plugin_id)
                                }) {
                                    this.plugin_picker.highlighted_index = highlight;
                                }
                            }
                            this.plugin_picker.selected_id = Some(plugin_id);
                            cx.notify();
                        });
                    }
                }),
                on_select_filter: Arc::new({
                    let this = cx.entity().clone();
                    move |filter: &PickerFilter, _w, cx| {
                        let filter = filter.clone();
                        let _ = this.update(cx, |this, cx| {
                            this.plugin_picker.set_sidebar_filter(filter);
                            if let Some(index) = this.plugin_search_index.as_ref() {
                                ensure_default_highlight(
                                    &mut this.plugin_picker,
                                    index,
                                    &this.plugin_picker_prefs,
                                );
                            }
                            cx.notify();
                        });
                    }
                }),
                on_toggle_favorite: Arc::new({
                    let this = cx.entity().clone();
                    move |plugin_id: &String, _w, cx| {
                        let plugin_id = plugin_id.clone();
                        let _ = this.update(cx, |this, cx| {
                            this.plugin_picker_prefs.toggle_favorite(&plugin_id);
                            cx.notify();
                        });
                    }
                }),
                on_pick: Arc::new({
                    let this = cx.entity().clone();
                    move |plugin_id: &String, w, cx| {
                        let plugin_id = plugin_id.clone();
                        let _ = this.update(cx, |this, cx| {
                            if let Some((track_id, insert_index, insert_id)) =
                                this.apply_picked_insert(&plugin_id, cx)
                            {
                                this.open_insert_editor(&track_id, insert_index, &insert_id, w, cx);
                            }
                        });
                    }
                }),
                on_retry_load: Arc::new({
                    let this = cx.entity().clone();
                    move |_: &(), _w, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.plugin_catalog.available = None;
                            this.plugin_search_index = None;
                            this.plugin_catalog.status = PluginCatalogStatus::Loading;
                            this.arm_catalog_load(cx);
                            cx.notify();
                        });
                    }
                }),
                on_open_plugin_manager: Arc::new({
                    let this = cx.entity().clone();
                    move |_: &(), window, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.plugin_picker = PluginPickerState::closed();
                            let _ = window;
                            this.open_plugin_manager_external_window(None, cx);
                            cx.notify();
                        });
                    }
                }),
                on_rebuild_database: Arc::new({
                    let this = cx.entity().clone();
                    move |_: &(), _w, cx| {
                        let _ = this.update(cx, |this, cx| {
                            // Drop the SQLite file outright; next picker open
                            // reports MissingDatabase, prompting Scan Now.
                            let _ = SpherePluginHost::plugin_db::delete_database_file();
                            this.plugin_catalog.available = None;
                            this.plugin_search_index = None;
                            this.plugin_catalog.status = PluginCatalogStatus::Loading;
                            this.arm_catalog_load(cx);
                            cx.notify();
                        });
                    }
                }),
            };
            let catalog_status = self.plugin_catalog.status.clone();
            Some(
                plugin_picker_overlay(
                    &self.plugin_picker,
                    self.plugin_search_index.clone(),
                    &self.plugin_picker_prefs,
                    catalog_status,
                    &self.plugin_picker_search_input,
                    self.plugin_picker_search_input.is_focused(window),
                    search_context_callbacks,
                    picker_callbacks,
                    self.plugin_picker_au_error.as_deref(),
                    &self.plugin_picker_scroll,
                )
                .into_any_element(),
            )
        } else {
            None
        };

        self.prune_insert_picker_window(cx);
        self.prune_mixer_window(cx);
        self.prune_midi_editor_window(cx);

        let transport_chrome = self.transport_chrome_state(cx);
        let panel_chrome = self.panel_chrome_state(cx);
        let show_browser = self.panels.browser;
        let show_inspector = self.panels.inspector;
        let show_mixer_docked = self.panels.mixer_docked;
        let active_panel = self.active_panel;

        let project_chrome = components::ProjectChromeState {
            name: self.project_session.display_name().to_string(),
            is_dirty: self.project_session.is_dirty,
            on_open_project_menu: on_project_open,
        };
        let shortcut_target = cx.entity().clone();
        // Docked MIDI editor — consulted in the key handler so Ctrl+A/C/V/X and
        // Delete route to the piano roll (its own `on_key_down`) when it holds
        // focus, instead of the global timeline clip commands.
        let midi_editor = self.piano_roll.clone();
        let solfege_editor = self.solfege_editor.clone();
        // Physical-keyboard musical typing updates the panel entity *directly*
        // (mirroring the mouse path), never nested inside a `StudioLayout`
        // update. The panel's key handler flushes through the event sink, which
        // re-enters `StudioLayout::update` to route the MIDI; wrapping these in
        // an outer `StudioLayout` lease double-leases and panics (the bug this
        // fixes — mouse clicks worked precisely because they never took that
        // outer lease). Separate clones because each closure is `move`.
        let virtual_keyboard_keydown = self.virtual_keyboard.clone();
        let virtual_keyboard_keyup = self.virtual_keyboard.clone();

        // Keep keyboard focus on our shortcut anchor so transport shortcuts
        // (Space, Enter, L, K, R, Home) reach `capture_key_down` below. GPUI
        // dispatches key events along the focused element's path; when focus is
        // None — OR stale (stuck on a search field whose overlay has since
        // closed, which GPUI still reports as "focused") — the dispatch path
        // falls back to the synthetic root node, which does NOT include this
        // div's `capture_key_down`, so every shortcut silently dies.
        //
        // Reclaim the anchor whenever it isn't focused and no *live* text field
        // is capturing the keyboard. This is intentionally stricter than
        // `window.focused().is_none()`: it also recovers from orphaned focus,
        // while never stealing focus from a field the user is actively typing in.
        // Only treat the docked piano roll as keyboard owner while its tab is
        // actually visible. Once the Editor tab is hidden/closed, GPUI still
        // reports its `FocusHandle` as focused (orphaned), which would otherwise
        // block this reclaim and leave Space/transport shortcuts dead until the
        // user clicks a control. See `docked_midi_editor_visible`.
        let docked_midi_editor_owns_keyboard =
            self.docked_midi_editor_visible() && midi_editor.read(cx).is_focused(window);
        let docked_song_text_editor_owns_keyboard = self.panels.inspector
            && self.right_dock_tab == RightDockTab::LyricEditor
            && self
                .lyric_editor_panel
                .read(cx)
                .is_text_input_focused(window);
        // Seed the shortcut anchor only when the window has no focused element.
        // Do not reclaim focus from buttons or sliders: AccessKit focus actions
        // and Tab navigation must remain stable for assistive technology.
        if window.focused(cx).is_none()
            && !docked_midi_editor_owns_keyboard
            && !docked_song_text_editor_owns_keyboard
            && !self.keyboard_text_capture_live(window)
        {
            self.focus_handle.focus(window, cx);
        }
        if self.command_palette.is_open && !self.command_palette_input.is_focused(window) {
            self.command_palette_input.focus_handle.focus(window, cx);
        }
        let focus_holder = self.focus_handle.clone();
        let focus_holder_on_pointer = self.focus_handle.clone();

        // Systemwide IME bridge: when a main-window text field owns focus, mount
        // the OS composition handler against it (routed to the focused field by
        // `impl EntityInputHandler for StudioLayout`). Coexists with the raw key
        // path; absent when no field is focused, so it never touches shortcuts.
        let ime_bridge = self
            .focused_text_input_handle(window)
            .map(|fh| crate::components::text_input::ime_input_bridge(cx.entity().clone(), fh));
        let shortcut_keydown_target = shortcut_target.clone();

        div()
            .id("futureboard-studio-root")
            .role(Role::Application)
            .aria_label(title.clone())
            // NOTE: `track_focus` deliberately lives on the tiny invisible
            // `focus_holder` child below, NOT on this root. Putting it on
            // the root makes GPUI insert a full-window Normal hitbox
            // (see `should_insert_hitbox` — `tracked_focus_handle.is_some()`
            // triggers it). That hitbox is benign for click dispatch, but
            // on Windows it lands above the chrome's
            // `WindowControlArea::Drag` hitbox in the `mouse_hit_test.ids`
            // vector — which the NCHITTEST callback iterates in
            // window-control-vector order, not z-order — and the OS sees
            // a non-caption hit, refusing to start the window move.
            // Hoisting focus onto a 0×0 child preserves shortcut
            // delivery without adding the full-window hitbox.
            .flex()
            .flex_col()
            .size_full()
            .relative()
            .bg(Colors::surface_base())
            .font(theme::ui_font())
            // Reclaim the studio shortcut anchor before the clicked child handles
            // the pointer. Focusable controls (text fields, piano roll, etc.) take
            // focus again in their own target handler, while non-focusable
            // arrangement clips now reliably route Delete/Cut to the Timeline.
            .capture_any_mouse_down(move |_event, window, cx| {
                focus_holder_on_pointer.focus(window, cx);
            })
            .capture_key_down(move |event, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key.eq_ignore_ascii_case("tab")
                    && !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function
                {
                    if modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                    return;
                }
                let handled = shortcut_keydown_target.update(cx, |this, cx| {
                    let handled = this.handle_command_palette_key(event, window, cx)
                        || this.handle_bpm_edit_key(event, window, cx)
                        || this.handle_ts_edit_key(event, window, cx)
                        || this.handle_settings_dialog_key(event, window, cx)
                        || this.handle_add_track_dialog_key(event, window, cx)
                        || this.handle_plugin_picker_key(event, window, cx)
                        || this.handle_automation_picker_key(event, window, cx)
                        || this.handle_project_switcher_key(event, window, cx)
                        || this.handle_inspector_key(event, window, cx)
                        || this.handle_browser_key(event, window, cx);
                    if handled {
                        cx.notify();
                    }
                    handled
                });
                if handled {
                    let key = event.keystroke.key.clone();
                    let _ = shortcut_keydown_target.update(cx, |this, _cx| {
                        this.shortcut_diagnostics.last_key_event = key;
                        this.shortcut_diagnostics.last_key_target = "focused-handler".to_string();
                        this.shortcut_diagnostics.last_key_consumed_by =
                            "pre-global-key-handler".to_string();
                        this.shortcut_diagnostics.focused_widget_kind =
                            this.focused_widget_kind(window);
                        this.shortcut_diagnostics.is_text_editing_context =
                            this.is_text_editing_context(window);
                    });
                    return;
                }
                let song_text_input_focused = shortcut_keydown_target
                    .read(cx)
                    .panels
                    .inspector
                    && shortcut_keydown_target.read(cx).right_dock_tab
                        == RightDockTab::LyricEditor
                    && shortcut_keydown_target
                        .read(cx)
                        .lyric_editor_panel
                        .read(cx)
                        .is_text_input_focused(window);
                let focus = FocusContext {
                    text_input_focused: shortcut_keydown_target
                        .read(cx)
                        .is_text_editing_context(window)
                        || song_text_input_focused,
                };
                let focused_widget_kind = shortcut_keydown_target.read(cx).focused_widget_kind(window);
                let key_for_diag = event.keystroke.key.clone();
                let _ = shortcut_keydown_target.update(cx, |this, _cx| {
                    this.shortcut_diagnostics.last_key_event = key_for_diag;
                    this.shortcut_diagnostics.last_key_target = focused_widget_kind.clone();
                    this.shortcut_diagnostics.last_key_consumed_by = "unhandled".to_string();
                    this.shortcut_diagnostics.focused_widget_kind = focused_widget_kind.clone();
                    this.shortcut_diagnostics.is_text_editing_context = focus.text_input_focused;
                });
                if key_debug() {
                    eprintln!(
                        "[key] key={:?} text_input_focused={} held={} (plugin editor, when active, \
                         consumes keys before this handler)",
                        event.keystroke.key, focus.text_input_focused, event.is_held
                    );
                }
                if focus.text_input_focused && is_text_input_key(event) {
                    let _ = shortcut_keydown_target.update(cx, |this, _cx| {
                        this.shortcut_diagnostics.last_key_consumed_by = "text-input".to_string();
                    });
                    if key_debug() {
                        eprintln!(
                            "[key] ignored key={:?} reason=text-input-focused (typed into field)",
                            event.keystroke.key
                        );
                    }
                    return;
                }
                // Update the panel entity directly — NOT through
                // `shortcut_keydown_target.update` — so the panel's event sink
                // can re-enter `StudioLayout::update` without a double-lease
                // panic. A Ctrl/Cmd/Alt/Fn chord is a shortcut, not a note, so
                // it is passed through to the dispatch path below.
                let mods = event.keystroke.modifiers;
                let command_modifier =
                    mods.control || mods.alt || mods.platform || mods.function;
                let window_id = window.window_handle().window_id();
                let virtual_keyboard_handled =
                    virtual_keyboard_keydown.update(cx, |keyboard, cx| {
                        keyboard.handle_key_down(
                            window_id,
                            event.keystroke.key.as_str(),
                            command_modifier,
                            event.is_held,
                            focus.text_input_focused,
                            cx,
                        )
                    });
                if virtual_keyboard_handled {
                    let _ = shortcut_keydown_target.update(cx, |this, _cx| {
                        this.shortcut_diagnostics.last_key_consumed_by =
                            "virtual-keyboard".to_string();
                    });
                    if components::VirtualKeyboardPanel::should_prevent_default_key(
                        event.keystroke.key.as_str(),
                    ) {
                        window.prevent_default();
                        cx.stop_propagation();
                        if key_debug() {
                            eprintln!(
                                "[VirtualKeyboard] prevented default system key behavior key={}",
                                event.keystroke.key
                            );
                        }
                    }
                    return;
                }
                if event.keystroke.key.as_str() == "escape" {
                    let _ = shortcut_keydown_target.update(cx, |this, cx| {
                        // Cancel an active BPM scrub first, restoring the value
                        // captured at drag start.
                        this.cancel_bpm_drag(cx);
                        let _ = this.timeline.update(cx, |timeline, cx| {
                            timeline.reset_input_state();
                            cx.notify();
                        });
                        this.menu_bar.open_menu_id = None;
                        this.menu_bar.submenu_path.clear();
                        this.command_palette.close();
                        this.overlay.open_popover = None;
                        this.overlay.text_context_menu = None;
                        this.project_switcher.is_open = false;
                        cx.notify();
                    });
                    return;
                }
                let command_id = shortcut_keydown_target.read(cx).shortcut_command_id(event);
                if let Some(command_id) = command_id {
                    // MIDI editor focus gate: when the docked piano roll holds
                    // keyboard focus, the A/C/V/X/Delete family belongs to it.
                    // Skip global dispatch (which would target timeline clips and
                    // could nested-update) and let the event bubble to the piano
                    // roll's `on_key_down`. See PART D/E of the shortcuts task.
                    if is_midi_routable_edit_command(&normalize_command_id(&command_id))
                        && shortcut_keydown_target.read(cx).docked_midi_editor_visible()
                        && midi_editor.read(cx).is_focused(window)
                    {
                        if edit_command_debug() {
                            eprintln!(
                                "[edit-command] command={command_id} target=MidiEditor \
                                 reason=focus-passthrough (handled by piano roll)"
                            );
                        }
                        return;
                    }
                    // Same gate for the Solfege Pitch tab, which replaces the
                    // piano roll in the dock for a Solfege clip and owns Delete
                    // for its pitch points.
                    if is_midi_routable_edit_command(&normalize_command_id(&command_id))
                        && shortcut_keydown_target.read(cx).docked_midi_editor_visible()
                        && solfege_editor.read(cx).pitch_grid_is_focused(window)
                    {
                        if edit_command_debug() {
                            eprintln!(
                                "[edit-command] command={command_id} target=SolfegePitch \
                                 reason=focus-passthrough (handled by pitch editor)"
                            );
                        }
                        return;
                    }
                    // Transport shortcuts go through the same dispatcher as the
                    // chrome Play button (transport:play-pause → PlayPause), so
                    // Spacebar and the button are always one command. Only the
                    // focus gate differs between them.
                    let is_transport = transport_command_from_id(&command_id).is_some();
                    if is_transport && !should_handle_global_transport_shortcut(&focus) {
                        if key_debug() {
                            eprintln!(
                                "[key] ignored command={command_id} reason=global-transport-shortcut-suppressed"
                            );
                        }
                        return;
                    }
                    if is_tap_tempo_command(&normalize_command_id(&command_id))
                        && shortcut_keydown_target
                            .read(cx)
                            .tap_tempo_shortcut_blocked(window)
                    {
                        if key_debug() {
                            eprintln!(
                                "[key] ignored command={command_id} reason=tap-tempo-shortcut-suppressed"
                            );
                        }
                        return;
                    }
                    if command_id == "transport:play-pause"
                        && event.keystroke.key.eq_ignore_ascii_case("space")
                    {
                        eprintln!("[KeyCommand] Spacebar -> TransportTogglePlay");
                        let _ = shortcut_keydown_target.update(cx, |this, _cx| {
                            this.shortcut_diagnostics.transport_toggle_shortcut_count = this
                                .shortcut_diagnostics
                                .transport_toggle_shortcut_count
                                .saturating_add(1);
                            this.shortcut_diagnostics.last_key_consumed_by =
                                "global-transport-shortcut".to_string();
                            crate::perf::count(
                                "transport_toggle_shortcut_count",
                                this.shortcut_diagnostics.transport_toggle_shortcut_count,
                            );
                        });
                    }
                    if key_debug() {
                        eprintln!("[key] dispatched command={command_id}");
                    }
                    let _ = shortcut_keydown_target.update(cx, |this, cx| {
                        this.dispatch_command_id_from_bounds(&command_id, Some(window.bounds()), cx);
                        cx.notify();
                    });
                } else if event.keystroke.key.eq_ignore_ascii_case("space")
                    && !event.is_held
                    && !focus.text_input_focused
                    && !mods.control
                    && !mods.alt
                    && !mods.platform
                    && !mods.function
                    && should_handle_global_transport_shortcut(&focus)
                {
                    if key_debug() {
                        eprintln!(
                            "[key] dispatched command=transport:play-pause reason=spacebar-fallback"
                        );
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                    let _ = shortcut_keydown_target.update(cx, |this, cx| {
                        this.shortcut_diagnostics.transport_toggle_shortcut_count = this
                            .shortcut_diagnostics
                            .transport_toggle_shortcut_count
                            .saturating_add(1);
                        this.shortcut_diagnostics.last_key_consumed_by =
                            "spacebar-fallback".to_string();
                        crate::perf::count(
                            "transport_toggle_shortcut_count",
                            this.shortcut_diagnostics.transport_toggle_shortcut_count,
                        );
                        this.dispatch_command_id_from_bounds(
                            "transport:play-pause",
                            Some(window.bounds()),
                            cx,
                        );
                        cx.notify();
                    });
                }
            })
            .capture_key_up({
                // Update the panel entity directly (see the key-down note): the
                // NoteOff flush re-enters `StudioLayout::update` via the sink, so
                // an outer `StudioLayout` lease here would double-lease and panic.
                let virtual_keyboard = virtual_keyboard_keyup.clone();
                move |event, window: &mut Window, cx| {
                    let window_id = window.window_handle().window_id();
                    let handled = virtual_keyboard.update(cx, |keyboard, cx| {
                        keyboard.handle_key_up(window_id, event.keystroke.key.as_str(), cx)
                    });
                    if handled {
                        if components::VirtualKeyboardPanel::should_prevent_default_key(
                            event.keystroke.key.as_str(),
                        ) {
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                        return;
                    }
                }
            })
            // Invisible focus anchor. 0×0 means no visible footprint and
            // an effectively unreachable hitbox; `track_focus` only needs
            // it to register the focus handle. The root's
            // `capture_key_down` still fires for any key while this
            // descendant is focused (capture phase: root → focused).
            .child(div().w(px(0.0)).h(px(0.0)).track_focus(&focus_holder))
            // Non-visual, non-interactive OS-IME bridge for the focused field.
            .children(ime_bridge)
            .child({
                let _s = crate::perf::PerfScope::enter("AppChrome");
                let close_target = cx.entity().clone();
                let on_window_close: components::ChromeActionCb = std::sync::Arc::new(
                    move |_: &(), window: &mut Window, cx: &mut gpui::App| {
                        let owner_bounds = Some(window.bounds());
                        let _ = close_target.update(cx, |studio, cx| {
                            studio.request_close(PendingCloseAction::QuitApp, owner_bounds, cx);
                        });
                    },
                );
                components::app_chrome(
                    window,
                    open_menu_id.as_deref(),
                    on_open_menu,
                    project_chrome,
                    transport_chrome,
                    panel_chrome,
                    Some(on_window_close),
                    i18n,
                )
            })
            .child({
                let mut main_row = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0();
                if show_browser {
                    main_row = main_row.child({
                        let _s = crate::perf::PerfScope::enter("Sidebar");
                        let owner = cx.entity().clone();
                        div()
                            .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                                let _ = owner.update(cx, |layout, cx| {
                                    layout.set_active_panel(WorkspaceActivePanel::Browser, cx);
                                });
                            })
                            .child(components::sidebar(
                            &file_browser,
                            browser_scroll,
                            &self.browser_search_input,
                            self.browser_search_input.is_focused(window),
                            active_panel == WorkspaceActivePanel::Browser,
                            browser_search_callbacks,
                            on_browser_toggle,
                            on_browser_select,
                            on_browser_activate,
                            on_browser_context,
                            on_browser_collapse_all,
                            on_browser_rescan,
                            i18n,
                        ))
                    });
                }
                main_row = main_row.child({
                    let owner = cx.entity().clone();
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                            let _ = owner.update(cx, |layout, cx| {
                                layout.set_active_panel(WorkspaceActivePanel::Arrangement, cx);
                            });
                        })
                        .child(self.timeline.clone())
                });
                if show_inspector {
                    main_row = main_row.child({
                        let _s = crate::perf::PerfScope::enter("RightDock");
                        let right_tab = self.right_dock_tab;
                        let owner = cx.entity().clone();
                        let content = match right_tab {
                            RightDockTab::Inspector => {
                                let inspector_audio_connections =
                                    self.timeline.read(cx).state.audio_connections.clone();
                                let selection_duration_beats = self.timeline.read(cx).state.arrangement_range.as_ref().and_then(|range| {
                                    let (start, end) = range.as_f32_range();
                                    let duration = (end - start).abs();
                                    (duration > 0.0001).then_some(duration)
                                });
                                let stretch_tempo = selected_clip_id.as_deref().map(|clip_id| {
                                    self.stretch_tempo_snapshot(clip_id)
                                });
                                crate::components::panel::inspector_panel(
                                    &tracks,
                                    &inspector_audio_connections,
                                    selected_track_id.as_deref(),
                                    selected_clip_id.as_deref(),
                                    find_clip_summary(
                                        &tracks,
                                        selected_clip_id.as_deref(),
                                        project_bpm,
                                        selection_duration_beats,
                                    ),
                                    stretch_tempo,
                                    &self.inspector_name_edit.name_input,
                                    inspector_name_focused,
                                    inspector_name_callbacks,
                                    &self.inspector_name_edit.clip_name_input,
                                    inspector_clip_name_focused,
                                    inspector_clip_name_callbacks,
                                    crate::components::panel::InspectorColorPicker {
                                        state: &self.inspector_name_edit.color_picker,
                                        hex_focused: inspector_color_hex_focused,
                                        hex_callbacks: inspector_color_hex_callbacks,
                                        callbacks: inspector_color_callbacks,
                                    },
                                    active_panel == WorkspaceActivePanel::Inspector,
                                    &inspector_callbacks,
                                    i18n,
                                ).into_any_element()
                            }
                            RightDockTab::Solfege => crate::components::panel::solfege_panel(
                                &tracks,
                                selected_track_id.as_deref(),
                                active_panel == WorkspaceActivePanel::Solfege,
                                self.solfege_editor.read(cx).selected_pitch_summary(cx),
                            )
                            .into_any_element(),
                            RightDockTab::ChordDisplay => self.chord_display_panel.clone().into_any_element(),
                            RightDockTab::LyricDisplay => self.lyric_display_panel.clone().into_any_element(),
                            RightDockTab::LyricEditor => self.lyric_editor_panel.clone().into_any_element(),
                        };
                        div()
                            .w(px(crate::components::panel::INSPECTOR_WIDTH))
                            .h_full()
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .border_l(px(1.0))
                            .border_color(Colors::border_subtle())
                            .child(right_dock_tab_bar(right_tab, owner))
                            .child(div().flex_1().min_h_0().overflow_hidden().child(content))
                    });
                }
                main_row
            })
            .children(if show_mixer_docked {
                let _s = crate::perf::PerfScope::enter("BottomPanel");
                Some(self.bottom_panel_shell.clone().into_any_element())
            } else {
                None
            })
            .child({
                let _s = crate::perf::PerfScope::enter("StatusBar");
                self.status_bar.clone()
            })
            // Click-outside dismissal for the Inspector colour picker. The
            // popover is deferred at a higher priority and occludes its own
            // clicks, so only a genuine outside click reaches this layer.
            .children(inspector_color_open.then(|| {
                let owner = cx.entity().clone();
                crate::components::form::select_dismiss_backdrop(std::sync::Arc::new(
                    move |_: &(), window: &mut Window, cx: &mut gpui::App| {
                        let _ = owner.update(cx, |this, cx| {
                            this.close_inspector_color_picker(window, cx);
                        });
                    },
                ))
            }))
            // Dropdown overlay — rendered last so it sits above every other
            // panel. The dropdown's own backdrop captures click-outside.
            .children(dropdown_overlay)
            .children(popover_overlay)
            .children(inspector_routing_combo_overlay)
            // Add Track moved to external window.
            .children(settings_overlay)
            .children(plugin_picker_overlay_el)
            .children(text_context_overlay)
            .children(virtual_keyboard_overlay)
            .children({
                if debug_active_panel_enabled() {
                    Some(
                        div()
                            .absolute()
                            .right(px(12.0))
                            .bottom(px(28.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(crate::theme::radius::CONTROL))
                            .border(px(1.0))
                            .border_color(Colors::panel_border_focused())
                            .bg(Colors::surface_canvas())
                            .text_size(px(10.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Colors::tab_text_active())
                            .child(format!("active_panel = {}", active_panel.label()))
                            .into_any_element(),
                    )
                } else {
                    None
                }
            })
            .children({
                let show_perf_overlay = self.settings.read(cx).current.performance.show_performance_overlay
                    || crate::perf::perf_hud_enabled();
                // Scope aggregation costs a timestamp per instrumented scope, so
                // it follows the overlay: on while the user is looking at it,
                // off (and cleared) the moment it closes.
                crate::perf::set_collection_requested(show_perf_overlay);
                if show_perf_overlay {
                    let snapshot = self.performance_overlay_snapshot(reason_static);
                    Some(components::performance_overlay(&snapshot).into_any_element())
                } else {
                    None
                }
            })
    }
}

fn right_dock_tab_bar(active: RightDockTab, owner: Entity<StudioLayout>) -> impl IntoElement {
    let popout_kind = match active {
        RightDockTab::Inspector | RightDockTab::Solfege => None,
        RightDockTab::ChordDisplay => Some(components::SongTextPanelKind::ChordDisplay),
        RightDockTab::LyricDisplay => Some(components::SongTextPanelKind::LyricDisplay),
        RightDockTab::LyricEditor => Some(components::SongTextPanelKind::LyricEditor),
    };
    let mut row = div()
        .id("right-dock-tabs")
        .role(Role::TabList)
        .aria_label("Right panel")
        .h(px(28.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .px(px(4.0))
        .gap(px(2.0))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_panel());
    for tab in [
        RightDockTab::Inspector,
        RightDockTab::Solfege,
        RightDockTab::ChordDisplay,
        RightDockTab::LyricDisplay,
        RightDockTab::LyricEditor,
    ] {
        row = row.child(right_dock_tab_button(tab, active, owner.clone()));
    }
    row.child(div().flex_1()).children(popout_kind.map(|kind| {
        let target = owner.clone();
        components::icon_button(
            Some(crate::assets::ICON_MAXIMIZE_PATH),
            "Open in a separate window",
            px(20.0),
            px(20.0),
            px(11.0),
            Colors::text_muted(),
        )
        .id("right-dock-popout")
        .role(Role::Button)
        .aria_label("Open panel in a separate window")
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.bg(Colors::surface_control_hover()))
        .cursor(gpui::CursorStyle::PointingHand)
        .on_click(move |_, window, cx| {
            let bounds = Some(window.bounds());
            let _ = target.update(cx, |layout, cx| {
                layout.open_song_text_external_window(kind, bounds, cx);
            });
        })
    }))
}

fn right_dock_tab_button(
    tab: RightDockTab,
    active: RightDockTab,
    owner: Entity<StudioLayout>,
) -> impl IntoElement {
    let selected = tab == active;
    let label = match tab {
        RightDockTab::Inspector => "Inspect",
        RightDockTab::Solfege => "Solfege",
        RightDockTab::ChordDisplay => "Chords",
        RightDockTab::LyricDisplay => "Lyrics",
        RightDockTab::LyricEditor => "Edit",
    };
    div()
        .id(("right-dock-tab", tab as u32))
        .role(Role::Tab)
        .aria_label(label)
        .aria_selected(selected)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.bg(Colors::tab_bg_hover()))
        .h(px(22.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(crate::theme::radius::CONTROL))
        .bg(if selected {
            Colors::tab_bg_active()
        } else {
            Colors::surface_panel()
        })
        .text_size(px(10.0))
        .font_weight(if selected {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .text_color(if selected {
            Colors::tab_text_active()
        } else {
            Colors::tab_text_muted()
        })
        .cursor(gpui::CursorStyle::PointingHand)
        .on_click(move |_, _, cx| {
            let _ = owner.update(cx, |layout, cx| {
                layout.panels.inspector = true;
                layout.right_dock_tab = tab;
                layout.set_active_panel(tab.active_panel(), cx);
            });
        })
        .child(label)
}

#[cfg(target_os = "windows")]
fn publish_studio_main_hwnd(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::Win32(w) = handle.as_raw() {
            SpherePluginHost::plugin_host_main_window::set_main_window_hwnd(w.hwnd.get());
        }
    }
}

/// Publish the studio window's X11 id as the plugin-host owner/DPI reference.
/// Matches `studio_native_hwnd` in plugin_ops: X11/XWayland only; pure Wayland
/// leaves the published handle unset.
#[cfg(not(target_os = "windows"))]
fn publish_studio_main_hwnd(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let id = match handle.as_raw() {
        RawWindowHandle::Xcb(w) => Some(w.window.get() as isize),
        RawWindowHandle::Xlib(w) => Some(w.window as isize),
        _ => None,
    };
    if let Some(id) = id {
        SpherePluginHost::plugin_host_main_window::set_main_window_hwnd(id);
    }
}

/// `FUTUREBOARD_DEBUG_ACTIVE_PANEL=1` — draw the active-panel debug pill.
/// Cached so the root render doesn't hit the OS env store every frame
/// (matches the `OnceLock` idiom used for every other debug flag).
fn debug_active_panel_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_DEBUG_ACTIVE_PANEL").is_some())
}
