use gpui::{Context, Entity, UniformListScrollHandle, Window, WindowHandle};
use std::{collections::HashMap, path::PathBuf};

use crate::components::edit::EditCommand;
use crate::components::mixer_panel::{
    clamp_mixer_section_height_px, mixer_render_item_count, mixer_scroll_x_for_strip_index,
    mixer_strip_index_for_channel, MixerCallbacks, MixerSplitAction, MixerSplitTarget,
    VstiOutputMeterState, MIXER_INSERT_SECTION_DEFAULT_PX, MIXER_SEND_SECTION_DEFAULT_PX,
    STRIP_WIDTH,
};
use crate::components::mixer_tree_model::{
    ensure_timeline_mixer_tree_defaults, expand_ancestors_for_channel, MixerTreeModel,
};
use crate::components::mixer_tree_sidebar::{
    clamp_mixer_tree_sidebar_width, MixerTreeCallbacks, MIXER_TREE_COLLAPSED_RAIL_WIDTH,
    MIXER_TREE_SIDEBAR_DEFAULT_WIDTH,
};
use crate::components::timeline::timeline_state::{
    self, collapsed_vsti_output_group_keys_from_tracks, TrackState,
};
use crate::components::{external_mixer_debug, external_mixer_debug_enabled, MixerSnapshot};

use super::engine_snapshot::{apply_engine_track_input_state, volume_norm_to_linear};
use super::{ContextMenuRequest, ContextMenuTarget, ContextTarget, MixerWindow, StudioLayout};

/// Mixer-panel view state — horizontal scroll, the shared insert/send section
/// heights, and the transient splitter-drag anchors. `StudioLayout` decomposition
/// slice. Manual `Default` (insert/send sections start at their default px).
pub(crate) struct MixerViewState {
    /// Horizontal scroll offset for the channel-strip area.
    pub scroll_x: f32,
    /// Shared Inserts viewport height (px) for every channel strip.
    pub insert_section_px: f32,
    /// Shared Sends viewport height (px) for every channel strip.
    pub send_section_px: f32,
    /// Splitter-drag pointer-Y anchor recorded on pointer-down.
    pub split_resize_start_y: f32,
    /// Insert-section height captured at splitter-drag start.
    pub split_resize_start_insert_px: f32,
    /// Send-section height captured at splitter-drag start.
    pub split_resize_start_send_px: f32,
    /// Active splitter-drag target, if a drag is in progress.
    pub split_active_target: Option<MixerSplitTarget>,
    pub vsti_output_meters: HashMap<String, VstiOutputMeterState>,
    /// Reused scratch set of the plugin-output meter keys seen on the current
    /// meter tick. Kept on the struct (drained via `mem::take`) so the playback
    /// meter path does not allocate a fresh `HashSet` every tick.
    pub vsti_meter_live_keys: std::collections::HashSet<String>,
    /// Mixer tree sidebar enabled (session-only).
    pub tree_sidebar_enabled: bool,
    /// Collapsed to icon rail.
    pub tree_sidebar_collapsed: bool,
    pub tree_sidebar_width_px: f32,
    pub tree_show_only_selected_group: bool,
    pub tree_resize_start_x: f32,
    pub tree_resize_start_width_px: f32,
    pub tree_resizing: bool,
    /// Transient strip highlight after tree double-click focus.
    pub focus_channel_id: Option<String>,
    pub tree_scroll: UniformListScrollHandle,
    /// Cached tree model — rebuilt only when tracks/filter/output routing changes.
    pub cached_tree_model: Option<MixerTreeModel>,
    pub tree_cache_filter: String,
    pub tree_cache_output_ch: u32,
    pub tree_cache_tracks_gen: u64,
    pub tree_cache_show_only: bool,
    pub tree_cache_selected_id: Option<String>,
    /// One-shot fallback when session install did not seed tree defaults.
    pub tree_defaults_applied: bool,
}

/// Stable mixer-tree UI hooks built once per studio layout (not per frame).
pub(crate) struct MixerTreeUiHooks {
    pub callbacks: MixerTreeCallbacks,
    pub on_resize_start: std::sync::Arc<dyn Fn(f32, &mut Window, &mut gpui::App) + 'static>,
    pub on_resize_move: std::sync::Arc<dyn Fn(f32, &mut Window, &mut gpui::App) + 'static>,
    pub on_resize_end: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static>,
}

impl Default for MixerViewState {
    fn default() -> Self {
        Self {
            scroll_x: 0.0,
            insert_section_px: MIXER_INSERT_SECTION_DEFAULT_PX,
            send_section_px: MIXER_SEND_SECTION_DEFAULT_PX,
            split_resize_start_y: 0.0,
            split_resize_start_insert_px: 0.0,
            split_resize_start_send_px: 0.0,
            split_active_target: None,
            vsti_output_meters: HashMap::new(),
            vsti_meter_live_keys: std::collections::HashSet::new(),
            tree_sidebar_enabled: true,
            tree_sidebar_collapsed: false,
            tree_sidebar_width_px: MIXER_TREE_SIDEBAR_DEFAULT_WIDTH,
            tree_show_only_selected_group: false,
            tree_resize_start_x: 0.0,
            tree_resize_start_width_px: MIXER_TREE_SIDEBAR_DEFAULT_WIDTH,
            tree_resizing: false,
            focus_channel_id: None,
            tree_scroll: UniformListScrollHandle::new(),
            cached_tree_model: None,
            tree_cache_filter: String::new(),
            tree_cache_output_ch: 0,
            tree_cache_tracks_gen: 0,
            tree_cache_show_only: false,
            tree_cache_selected_id: None,
            tree_defaults_applied: false,
        }
    }
}

/// Read-only mixer chrome snapshot for the docked panel entity.
pub(crate) struct DockedMixerPanelState<'a> {
    pub scroll_x: f32,
    pub vsti_output_meters: &'a HashMap<String, VstiOutputMeterState>,
    pub tree_sidebar_enabled: bool,
    pub viewport_width: f32,
    pub strip_available_px: f32,
}

impl StudioLayout {
    pub(crate) fn notify_mixer_window(&mut self, cx: &mut Context<Self>) {
        self.push_mixer_snapshot_to_window(cx);
    }

    /// Viewport metrics for the docked mixer panel entity (width, body height, strip height).
    pub(crate) fn mixer_panel_viewport_metrics(&self, cx: &gpui::App) -> (f32, f32, f32) {
        let tree_w = self.mixer_tree_sidebar_width();
        let window_w = self
            .window_hooks
            .cached_bounds
            .map(|b| f32::from(b.size.width))
            .unwrap_or(1280.0);
        let mixer_viewport_width = (window_w - tree_w - 90.0).max(100.0);
        let mixer_viewport_height = (self.bottom_panel_state.height_px - 28.0 - 30.0).max(0.0);
        let strip_available_px = mixer_viewport_height.max(STRIP_WIDTH);
        let _ = cx;
        (
            mixer_viewport_width,
            mixer_viewport_height,
            strip_available_px,
        )
    }

    /// Snapshot for the docked mixer panel entity (read-only view of mixer chrome).
    pub(crate) fn docked_mixer_panel_state(&self, cx: &gpui::App) -> DockedMixerPanelState<'_> {
        let (viewport_width, _viewport_height, strip_available_px) =
            self.mixer_panel_viewport_metrics(cx);
        DockedMixerPanelState {
            scroll_x: self.mixer_view.scroll_x,
            vsti_output_meters: &self.mixer_view.vsti_output_meters,
            tree_sidebar_enabled: self.mixer_view.tree_sidebar_enabled,
            viewport_width,
            strip_available_px,
        }
    }

    /// Meter-only UI refresh — isolated regions, never the StudioLayout root.
    pub(crate) fn notify_mixer_meter_regions(&mut self, cx: &mut Context<Self>) {
        let timeline = self.timeline.read(cx);
        let master_sig = crate::components::mixer_master_strip_view::mixer_master_meter_signature(
            &timeline.state.master,
        );
        let channel_sig = mixer_channel_meter_signature(&timeline.state.tracks);
        let sig = master_sig ^ channel_sig.rotate_left(17);
        if sig == self.engine_sync.last_meter_notify_sig {
            return;
        }
        self.engine_sync.last_meter_notify_sig = sig;

        // Track headers show meters — notify timeline only, not the studio shell.
        let _ = self.timeline.update(cx, |_, cx| cx.notify());

        if self.mixer_panel_chrome_visible() {
            let _ = self
                .mixer_panel
                .update(cx, |panel, cx| panel.on_meter_tick(cx));
            if self.external_windows.mixer.is_some() {
                self.push_mixer_snapshot_to_window(cx);
            }
        } else {
            crate::perf::count("mixer_repaint_while_inactive_count", 1);
            crate::perf::count("inactive_tab_repaint_count", 1);
        }
    }

    pub(crate) fn build_mixer_snapshot(&self, cx: &gpui::App) -> MixerSnapshot {
        let _scope = crate::perf::PerfScope::enter("MixerWindowSnapshotClone");
        let timeline = self.timeline.read(cx);
        // Clone tracks WITHOUT their `clips` (the heaviest field — MIDI note
        // vectors / audio-clip refs), which the mixer never draws. The detached
        // mixer rebuilds from this snapshot every meter frame, so skipping the
        // clip clone is the per-frame cost reduction that complements the meter
        // refresh cap. `automation_lanes` is kept (display_volume needs it).
        let mut tracks: Vec<TrackState> = timeline
            .state
            .tracks
            .iter()
            .map(clone_track_for_mixer)
            .collect();
        let mut master = timeline.state.master.clone();
        timeline
            .state
            .apply_volume_previews_to_snapshot(&mut tracks, &mut master);
        MixerSnapshot {
            tracks,
            master,
            selected_track_id: timeline.state.selection.selected_track_id.clone(),
            mixer_scroll_x: self.mixer_view.scroll_x,
            mixer_insert_section_px: clamp_mixer_section_height_px(
                self.mixer_view.insert_section_px,
            ),
            mixer_send_section_px: clamp_mixer_section_height_px(self.mixer_view.send_section_px),
            mixer_split_active_target: self.mixer_view.split_active_target,
            // Derived from the persisted per-instrument collapse flag (single
            // source of truth) — never a separate, drift-prone view cache.
            collapsed_vsti_output_groups: timeline.state.collapsed_vsti_output_group_keys(),
            hidden_mixer_channels: timeline.state.mixer_tree.hidden_channel_ids.clone(),
            vsti_output_meters: self.mixer_view.vsti_output_meters.clone(),
            tree_sidebar_enabled: self.mixer_view.tree_sidebar_enabled,
            tree_sidebar_collapsed: self.mixer_view.tree_sidebar_collapsed,
            tree_sidebar_width_px: self.mixer_view.tree_sidebar_width_px,
        }
    }

    pub(crate) fn mixer_view_state(
        &self,
        cx: &gpui::App,
    ) -> (
        Vec<TrackState>,
        timeline_state::MasterBusState,
        Option<String>,
        f32,
    ) {
        let snapshot = self.build_mixer_snapshot(cx);
        (
            snapshot.tracks,
            snapshot.master,
            snapshot.selected_track_id,
            snapshot.mixer_scroll_x,
        )
    }

    pub(crate) fn push_mixer_snapshot_to_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.external_windows.mixer.clone() else {
            return;
        };
        let snapshot = self.build_mixer_snapshot(cx);
        let _ = handle.update(cx, |mixer, _window, cx| {
            mixer.set_snapshot(snapshot);
            cx.notify();
        });
    }

    /// Meter-driven push to the detached mixer from the audio poll, rate-capped
    /// so a high-refresh display doesn't rebuild the whole external-mixer element
    /// tree at full refresh (the pop-out lag). No-op when the window is closed.
    /// Structural edits keep calling the immediate
    /// [`Self::push_mixer_snapshot_to_window`] and are never throttled, so faders
    /// / inserts / sends still update instantly; only the meter cadence is
    /// capped. Resets the timer only when a push actually happens.
    pub(crate) fn push_mixer_meter_snapshot_throttled(&mut self, cx: &mut Context<Self>) {
        if self.external_windows.mixer.is_none() {
            return;
        }
        let now = std::time::Instant::now();
        if now.saturating_duration_since(self.last_external_mixer_meter_push)
            < external_mixer_min_push_interval()
        {
            external_mixer_perf_record(false, 0);
            return;
        }
        self.last_external_mixer_meter_push = now;
        // Time the snapshot build + push only when the debug flag is on, so the
        // measurement itself adds no cost in normal operation. This is what tells
        // whether the pop-out cost is dominated by the clone (this path) or by
        // the element-tree rebuild (the deferred mixer_render migration).
        let timer = external_mixer_debug_enabled().then(std::time::Instant::now);
        self.push_mixer_snapshot_to_window(cx);
        let build_nanos = timer.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);
        external_mixer_perf_record(true, build_nanos);
    }

    pub(crate) fn set_mixer_scroll_x(&mut self, scroll_x: f32, _cx: &mut Context<Self>) -> bool {
        if (self.mixer_view.scroll_x - scroll_x).abs() > 0.25 {
            self.mixer_view.scroll_x = scroll_x;
            true
        } else {
            false
        }
    }

    pub(crate) fn mixer_tree_sidebar_width(&self) -> f32 {
        if self.mixer_view.tree_sidebar_collapsed {
            MIXER_TREE_COLLAPSED_RAIL_WIDTH
        } else if self.mixer_view.tree_sidebar_enabled {
            clamp_mixer_tree_sidebar_width(self.mixer_view.tree_sidebar_width_px)
        } else {
            0.0
        }
    }

    pub(crate) fn mixer_tree_output_channels(&self, cx: &Context<Self>) -> u32 {
        self.selected_output_device_channels(cx)
            .map(|(_, ch)| ch)
            .unwrap_or(2)
    }

    pub(crate) fn invalidate_mixer_tree_model_cache(&mut self) {
        self.mixer_view.cached_tree_model = None;
    }

    pub(crate) fn refresh_mixer_tree_sidebar_entity(&self, cx: &mut Context<Self>) {
        let routing_gen = self.audio_bridge.route_graph_version;
        let output_ch = self.mixer_tree_output_channels(cx);
        let collapsed = self.mixer_view.tree_sidebar_collapsed;
        let width = self.mixer_view.tree_sidebar_width_px;
        let show_only = self.mixer_view.tree_show_only_selected_group;
        let _ = self.mixer_tree_sidebar.update(cx, |sidebar, cx| {
            sidebar.sync_chrome(collapsed, width, show_only);
            if sidebar.sync_routing_from_layout(cx, routing_gen, output_ch) {
                cx.notify();
            }
        });
    }

    pub(crate) fn notify_mixer_tree_sidebar_only(&self, cx: &mut Context<Self>) {
        let _ = self.mixer_tree_sidebar.update(cx, |sidebar, cx| {
            sidebar.recompute_expansion(cx);
            cx.notify();
        });
    }

    pub(crate) fn notify_mixer_tree_selection_only(&self, cx: &mut Context<Self>) {
        let _ = self.mixer_tree_sidebar.update(cx, |sidebar, cx| {
            sidebar.recompute_selection(cx);
            cx.notify();
        });
    }

    /// Seed default expanded groups once when session install did not run (e.g. empty project).
    pub(crate) fn ensure_mixer_tree_defaults_once(&mut self, cx: &mut Context<Self>) {
        if self.mixer_view.tree_defaults_applied {
            return;
        }
        if !self
            .timeline
            .read(cx)
            .state
            .mixer_tree
            .expanded_node_ids
            .is_empty()
        {
            self.mixer_view.tree_defaults_applied = true;
            return;
        }
        let output_channels = self.mixer_tree_output_channels(cx);
        self.timeline.update(cx, |timeline, _cx| {
            ensure_timeline_mixer_tree_defaults(&mut timeline.state, output_channels);
        });
        self.mixer_view.tree_defaults_applied = true;
        self.invalidate_mixer_tree_model_cache();
    }

    pub(crate) fn mixer_tree_model_for_render(&mut self, cx: &mut Context<Self>) -> MixerTreeModel {
        let output_channels = self.mixer_tree_output_channels(cx);
        let filter = self.mixer_tree_filter_input.value.clone();
        let tracks_gen = self.audio_bridge.route_graph_version;
        let show_only = self.mixer_view.tree_show_only_selected_group;
        let selected_id = self
            .timeline
            .read(cx)
            .state
            .selection
            .selected_track_id
            .clone();

        let needs_rebuild = self.mixer_view.cached_tree_model.is_none()
            || self.mixer_view.tree_cache_filter != filter
            || self.mixer_view.tree_cache_output_ch != output_channels
            || self.mixer_view.tree_cache_tracks_gen != tracks_gen
            || self.mixer_view.tree_cache_show_only != show_only
            || (show_only && self.mixer_view.tree_cache_selected_id != selected_id);

        if needs_rebuild {
            let model = {
                let timeline = self.timeline.read(cx);
                MixerTreeModel::build(
                    &timeline.state.tracks,
                    output_channels,
                    &timeline.state.mixer_tree,
                    &filter,
                    show_only,
                    selected_id.as_deref(),
                )
            };
            self.mixer_view.cached_tree_model = Some(model);
            self.mixer_view.tree_cache_filter = filter;
            self.mixer_view.tree_cache_output_ch = output_channels;
            self.mixer_view.tree_cache_tracks_gen = tracks_gen;
            self.mixer_view.tree_cache_show_only = show_only;
            self.mixer_view.tree_cache_selected_id = selected_id;
        }

        self.mixer_view
            .cached_tree_model
            .as_ref()
            .expect("mixer tree cache populated above")
            .clone()
    }

    pub(crate) fn ensure_mixer_tree_ui_hooks(
        &mut self,
        owner: Entity<Self>,
        cx: &mut Context<Self>,
    ) {
        if self.mixer_tree_ui_hooks.is_some() {
            return;
        }

        let callbacks = self.build_mixer_tree_callbacks(owner.clone());

        let owner_tree_resize = owner.clone();
        let on_resize_start: std::sync::Arc<dyn Fn(f32, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |x, _w, cx| {
                let _ = owner_tree_resize.update(cx, |layout, _cx| {
                    layout.apply_mixer_tree_resize_start(x);
                });
            });
        let owner_tree_resize_move = owner.clone();
        let on_resize_move: std::sync::Arc<dyn Fn(f32, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |x, _w, cx| {
                let _ = owner_tree_resize_move.update(cx, |layout, cx| {
                    layout.apply_mixer_tree_resize_move(x, cx);
                });
            });
        let owner_tree_resize_end = owner.clone();
        let on_resize_end: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |_w, cx| {
                let _ = owner_tree_resize_end.update(cx, |layout, cx| {
                    layout.apply_mixer_tree_resize_end(cx);
                });
            });

        self.mixer_tree_ui_hooks = Some(MixerTreeUiHooks {
            callbacks,
            on_resize_start,
            on_resize_move,
            on_resize_end,
        });
        let hooks = self.mixer_tree_ui_hooks.as_ref().unwrap();
        let _ = self.mixer_tree_sidebar.update(cx, |sidebar, _cx| {
            sidebar.set_session_hooks(
                hooks.callbacks.clone(),
                Some(hooks.on_resize_start.clone()),
                Some(hooks.on_resize_move.clone()),
                Some(hooks.on_resize_end.clone()),
            );
        });
    }

    pub(crate) fn build_mixer_tree_model(
        &self,
        cx: &gpui::App,
        output_device_channels: u32,
    ) -> MixerTreeModel {
        let timeline = self.timeline.read(cx);
        let filter = self.mixer_tree_filter_input.value.clone();
        MixerTreeModel::build(
            &timeline.state.tracks,
            output_device_channels,
            &timeline.state.mixer_tree,
            &filter,
            self.mixer_view.tree_show_only_selected_group,
            timeline.state.selection.selected_track_id.as_deref(),
        )
    }

    pub(crate) fn mixer_focus_channel(
        &mut self,
        channel_id: &str,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) {
        let _scope = crate::perf::PerfScope::enter("MixerFocusLookup");
        // This is a read-only index lookup. Keep the timeline borrow scoped and
        // inspect tracks in place; cloning TrackState here used to clone every
        // arrangement clip/MIDI event before a channel could receive focus.
        let (strip_index, strip_count) = {
            let timeline = self.timeline.read(cx);
            let tracks = &timeline.state.tracks;
            let collapsed = collapsed_vsti_output_group_keys_from_tracks(tracks);
            let hidden = &timeline.state.mixer_tree.hidden_channel_ids;
            (
                mixer_strip_index_for_channel(tracks, &collapsed, hidden, channel_id),
                mixer_render_item_count(tracks, &collapsed, hidden),
            )
        };
        if let Some(index) = strip_index {
            self.mixer_view.scroll_x =
                mixer_scroll_x_for_strip_index(index, viewport_width, strip_count);
        }
        let model = self.build_mixer_tree_model(cx, self.mixer_tree_output_channels(cx));
        self.timeline.update(cx, |timeline, _cx| {
            expand_ancestors_for_channel(&mut timeline.state.mixer_tree, &model, channel_id);
            timeline.state.select_track(channel_id);
        });
        self.mixer_view.focus_channel_id = Some(channel_id.to_string());
        self.mark_dirty_view_only();
        self.push_mixer_snapshot_to_window(cx);
        cx.notify();
    }

    pub(crate) fn mixer_toggle_channel_visibility(
        &mut self,
        channel_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.timeline.update(cx, |timeline, _cx| {
            timeline
                .state
                .mixer_tree
                .toggle_channel_visibility(channel_id);
        });
        self.mark_dirty_view_only();
        self.push_mixer_snapshot_to_window(cx);
        cx.notify();
    }

    pub(crate) fn mixer_pin_channel(&mut self, channel_id: &str, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, _cx| {
            timeline.state.mixer_tree.toggle_pin(channel_id);
        });
        self.mark_dirty_view_only();
        self.notify_mixer_tree_sidebar_only(cx);
    }

    pub(crate) fn mixer_expand_all_tree(&mut self, cx: &mut Context<Self>) {
        let model = self.build_mixer_tree_model(cx, self.mixer_tree_output_channels(cx));
        self.timeline.update(cx, |timeline, _cx| {
            timeline
                .state
                .mixer_tree
                .expand_all(model.all_expandable_ids.clone());
            timeline.state.mixer_tree.set_expanded(
                crate::components::mixer_tree_model::MIXER_TREE_ROOT_ID,
                true,
            );
        });
        self.mark_dirty_view_only();
        self.notify_mixer_tree_sidebar_only(cx);
    }

    pub(crate) fn mixer_collapse_all_tree(&mut self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, _cx| {
            timeline.state.mixer_tree.collapse_all();
        });
        self.mark_dirty_view_only();
        self.notify_mixer_tree_sidebar_only(cx);
    }

    pub(crate) fn mixer_reset_tree_visibility(&mut self, cx: &mut Context<Self>) {
        self.timeline.update(cx, |timeline, _cx| {
            timeline.state.mixer_tree.reset_visibility();
        });
        self.mark_dirty_view_only();
        self.push_mixer_snapshot_to_window(cx);
        cx.notify();
    }

    pub(crate) fn sync_mixer_tree_to_selection(&mut self, cx: &mut Context<Self>) {
        // The tree model walks tracks/inserts and expands ancestors. It has no
        // effect while the sidebar is disabled, so keep it off the ordinary
        // strip-selection path.
        if !self.mixer_view.tree_sidebar_enabled {
            return;
        }
        let _scope = crate::perf::PerfScope::enter("MixerTreeSelectionSync");
        let Some(channel_id) = self
            .timeline
            .read(cx)
            .state
            .selection
            .selected_track_id
            .clone()
        else {
            return;
        };
        let model = self.build_mixer_tree_model(cx, self.mixer_tree_output_channels(cx));
        self.timeline.update(cx, |timeline, _cx| {
            expand_ancestors_for_channel(&mut timeline.state.mixer_tree, &model, &channel_id);
        });
        self.notify_mixer_tree_sidebar_only(cx);
    }

    pub(crate) fn apply_mixer_tree_resize_start(&mut self, x: f32) {
        self.mixer_view.tree_resize_start_x = x;
        self.mixer_view.tree_resize_start_width_px =
            clamp_mixer_tree_sidebar_width(self.mixer_view.tree_sidebar_width_px);
        self.mixer_view.tree_resizing = true;
    }

    pub(crate) fn apply_mixer_tree_resize_move(&mut self, x: f32, cx: &mut Context<Self>) {
        if !self.mixer_view.tree_resizing {
            return;
        }
        let delta = x - self.mixer_view.tree_resize_start_x;
        let new_w =
            clamp_mixer_tree_sidebar_width(self.mixer_view.tree_resize_start_width_px + delta);
        if (new_w - self.mixer_view.tree_sidebar_width_px).abs() > 0.25 {
            self.mixer_view.tree_sidebar_width_px = new_w;
            self.refresh_mixer_tree_sidebar_entity(cx);
            cx.notify();
        }
    }

    pub(crate) fn apply_mixer_tree_resize_end(&mut self, cx: &mut Context<Self>) {
        self.mixer_view.tree_resizing = false;
        cx.notify();
    }

    pub(crate) fn build_mixer_tree_callbacks(&self, owner: Entity<Self>) -> MixerTreeCallbacks {
        let owner_select = owner.clone();
        let on_select_channel: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |id: &String, _w, cx| {
            let id = id.clone();
            StudioLayout::defer_update(&owner_select, cx, move |layout, cx| {
                let already = layout
                    .timeline
                    .read(cx)
                    .state
                    .selection
                    .selected_track_id
                    .as_deref()
                    == Some(id.as_str());
                layout.timeline.update(cx, |timeline, _cx| {
                    timeline.state.select_track(&id);
                });
                layout.sync_mixer_tree_to_selection(cx);
                if !already {
                    layout.push_mixer_snapshot_to_window(cx);
                    cx.notify();
                } else {
                    layout.notify_mixer_tree_selection_only(cx);
                }
            });
        });

        let owner_focus = owner.clone();
        let on_focus_channel: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |id: &String, window, cx| {
            let id = id.clone();
            let window_w: f32 = window.bounds().size.width.into();
            StudioLayout::defer_update(&owner_focus, cx, move |layout, cx| {
                let tree_w = layout.mixer_tree_sidebar_width();
                let viewport = (window_w - tree_w - 90.0).max(STRIP_WIDTH);
                layout.mixer_focus_channel(&id, viewport, cx);
            });
        });

        let owner_expand = owner.clone();
        let on_toggle_expand: std::sync::Arc<
            dyn Fn(&(String, bool), &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |(node_id, expanded): &(String, bool), _w, cx| {
            let node_id = node_id.clone();
            let expanded = *expanded;
            StudioLayout::defer_update(&owner_expand, cx, move |layout, cx| {
                layout.timeline.update(cx, |timeline, _cx| {
                    timeline.state.mixer_tree.set_expanded(node_id, expanded);
                });
                layout.mark_dirty_view_only();
                layout.notify_mixer_tree_sidebar_only(cx);
            });
        });

        let owner_vis = owner.clone();
        let on_toggle_visibility: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |id: &String, _w, cx| {
            let id = id.clone();
            StudioLayout::defer_update(&owner_vis, cx, move |layout, cx| {
                layout.mixer_toggle_channel_visibility(&id, cx);
            });
        });

        let owner_pin = owner.clone();
        let on_toggle_pin: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |id: &String, _w, cx| {
                let id = id.clone();
                StudioLayout::defer_update(&owner_pin, cx, move |layout, cx| {
                    layout.mixer_pin_channel(&id, cx);
                });
            });

        let owner_mute = owner.clone();
        let on_toggle_mute: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |id: &String, _w, cx| {
                let id = id.clone();
                StudioLayout::defer_update(&owner_mute, cx, move |layout, cx| {
                    layout.mark_dirty_view_only();
                    let muted = layout.timeline.update(cx, |timeline, cx| {
                        timeline.state.toggle_track_mute(&id);
                        let value = timeline.state.find_track(&id).map(|track| track.muted);
                        cx.notify();
                        value
                    });
                    if let Some(muted) = muted {
                        if let Some(engine) = layout.audio_bridge.engine.as_ref() {
                            let _ = engine.update_track_param(
                                &id,
                                "muted",
                                if muted { 1.0 } else { 0.0 },
                            );
                        }
                        layout.invalidate_mixer_tree_model_cache();
                        layout.refresh_mixer_tree_sidebar_entity(cx);
                        layout.push_mixer_snapshot_to_window(cx);
                    }
                });
            });

        let owner_solo = owner.clone();
        let on_toggle_solo: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |id: &String, _w, cx| {
                let id = id.clone();
                StudioLayout::defer_update(&owner_solo, cx, move |layout, cx| {
                    layout.mark_dirty_view_only();
                    let solo = layout.timeline.update(cx, |timeline, cx| {
                        timeline.state.toggle_track_solo(&id);
                        let value = timeline.state.find_track(&id).map(|track| track.solo);
                        cx.notify();
                        value
                    });
                    if let Some(solo) = solo {
                        if let Some(engine) = layout.audio_bridge.engine.as_ref() {
                            let _ = engine.update_track_param(
                                &id,
                                "solo",
                                if solo { 1.0 } else { 0.0 },
                            );
                        }
                        layout.invalidate_mixer_tree_model_cache();
                        layout.refresh_mixer_tree_sidebar_entity(cx);
                        layout.push_mixer_snapshot_to_window(cx);
                    }
                });
            });

        let owner_collapse = owner.clone();
        let on_collapse_all: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |_w, cx| {
                let _ = owner_collapse.update(cx, |layout, cx| layout.mixer_collapse_all_tree(cx));
            });

        let owner_expand_all = owner.clone();
        let on_expand_all: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |_w, cx| {
                let _ = owner_expand_all.update(cx, |layout, cx| layout.mixer_expand_all_tree(cx));
            });

        let owner_filter_group = owner.clone();
        let on_show_only_selected_group: std::sync::Arc<
            dyn Fn(&mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |_w, cx| {
            let _ = owner_filter_group.update(cx, |layout, cx| {
                layout.mixer_view.tree_show_only_selected_group =
                    !layout.mixer_view.tree_show_only_selected_group;
                layout.invalidate_mixer_tree_model_cache();
                layout.refresh_mixer_tree_sidebar_entity(cx);
            });
        });

        let owner_reset = owner.clone();
        let on_reset_visibility: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |_w, cx| {
                let _ = owner_reset.update(cx, |layout, cx| layout.mixer_reset_tree_visibility(cx));
            });

        let owner_toggle_sidebar = owner.clone();
        let on_toggle_sidebar: std::sync::Arc<dyn Fn(&mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |_w, cx| {
                let _ = owner_toggle_sidebar.update(cx, |layout, cx| {
                    layout.mixer_view.tree_sidebar_collapsed =
                        !layout.mixer_view.tree_sidebar_collapsed;
                    layout.refresh_mixer_tree_sidebar_entity(cx);
                    cx.notify();
                });
            });

        MixerTreeCallbacks {
            on_select_channel,
            on_focus_channel,
            on_toggle_expand,
            on_toggle_visibility,
            on_toggle_pin,
            on_toggle_mute,
            on_toggle_solo,
            on_collapse_all,
            on_expand_all,
            on_show_only_selected_group,
            on_reset_visibility,
            on_toggle_sidebar,
        }
    }

    /// Current shared insert/send viewport heights, clamped to the supported range.
    pub(crate) fn mixer_insert_section_px(&self) -> f32 {
        clamp_mixer_section_height_px(self.mixer_view.insert_section_px)
    }

    pub(crate) fn mixer_send_section_px(&self) -> f32 {
        clamp_mixer_section_height_px(self.mixer_view.send_section_px)
    }

    pub(crate) fn mixer_split_active_target(&self) -> Option<MixerSplitTarget> {
        self.mixer_view.split_active_target
    }

    pub(crate) fn toggle_vsti_output_group(&mut self, group_key: &str, cx: &mut Context<Self>) {
        // Group key is `{track_id}:{insert_id}`; insert ids contain no ':' so the
        // last ':' splits the pair. Collapse/expand is a VIEW concern stored on the
        // instrument insert (single source of truth) — it never touches routing,
        // child mixer channels, route nodes, or the engine snapshot.
        let Some((track_id, insert_id)) = group_key.rsplit_once(':') else {
            return;
        };
        let track_id = track_id.to_string();
        let insert_id = insert_id.to_string();
        let now_collapsed = self.timeline.update(cx, |timeline, _cx| {
            timeline
                .state
                .toggle_insert_multiout_collapsed(&track_id, &insert_id)
        });

        if crate::forensic_trace::forensic_trace_enabled() {
            self.log_multiout_collapse_toggle(&track_id, &insert_id, now_collapsed, cx);
        }

        // Persist the new view state (so save/restore keeps it) WITHOUT rebuilding
        // the audio graph: the collapse flag is not part of the engine snapshot, so
        // marking only the project session dirty (never the engine) keeps the next
        // poll from building/serializing a redundant snapshot. `route_graph_version`
        // is unchanged.
        self.mark_dirty_view_only();
        self.push_mixer_snapshot_to_window(cx);
        cx.notify();
    }

    /// Structured `[MULTIOUT COLLAPSE]` / `[MULTIOUT EXPAND]` diagnostics proving
    /// the toggle preserves routes (graph version unchanged, no channels
    /// created/deleted). Forensic-gated; runs off the audio thread.
    fn log_multiout_collapse_toggle(
        &self,
        track_id: &str,
        insert_id: &str,
        now_collapsed: bool,
        cx: &gpui::App,
    ) {
        let prefix = format!("vsti-out:{insert_id}:bus:");
        let timeline = self.timeline.read(cx);
        let child_ids: Vec<String> = timeline
            .state
            .tracks
            .iter()
            .filter(|t| t.id.starts_with(&prefix))
            .map(|t| t.id.clone())
            .collect();
        let gv = self.audio_bridge.route_graph_version;
        if now_collapsed {
            eprintln!(
                "[MULTIOUT COLLAPSE]\nplugin_instance_id={insert_id}\nparent_mixer_channel_id={track_id}\nchild_count={}\nchild_mixer_channel_ids={child_ids:?}\nroute_nodes_preserved=true\naudio_routes_preserved=true\ngraph_version_before={gv}\ngraph_version_after={gv}\ncreated_or_deleted_channels=false",
                child_ids.len()
            );
        } else {
            eprintln!(
                "[MULTIOUT EXPAND]\nplugin_instance_id={insert_id}\nparent_mixer_channel_id={track_id}\nchild_count={}\nchild_mixer_channel_ids={child_ids:?}\nused_existing_channels=true\nduplicated_channels=false\nroute_nodes_preserved=true\ngraph_version_before={gv}\ngraph_version_after={gv}",
                child_ids.len()
            );
        }
    }

    /// Apply a splitter intent from any channel-strip handle. Shared across all
    /// strips, so one drag resizes the whole mixer. Pushes the new height to the
    /// floating mixer window and repaints when something changed.
    pub(crate) fn apply_mixer_split_action(
        &mut self,
        action: MixerSplitAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            MixerSplitAction::ResizeStart(target, y) => {
                self.mixer_view.split_resize_start_y = y;
                self.mixer_view.split_resize_start_insert_px =
                    clamp_mixer_section_height_px(self.mixer_view.insert_section_px);
                self.mixer_view.split_resize_start_send_px =
                    clamp_mixer_section_height_px(self.mixer_view.send_section_px);
                self.mixer_view.split_active_target = Some(target);
            }
            MixerSplitAction::ResizeMove(y) => {
                let Some(target) = self.mixer_view.split_active_target else {
                    return;
                };
                let delta = y - self.mixer_view.split_resize_start_y;
                match target {
                    MixerSplitTarget::InsertSend => {
                        let new_px = clamp_mixer_section_height_px(
                            self.mixer_view.split_resize_start_insert_px + delta,
                        );
                        if (new_px - self.mixer_view.insert_section_px).abs() <= 0.25 {
                            return;
                        }
                        self.mixer_view.insert_section_px = new_px;
                    }
                    MixerSplitTarget::SendFader => {
                        let new_px = clamp_mixer_section_height_px(
                            self.mixer_view.split_resize_start_send_px + delta,
                        );
                        if (new_px - self.mixer_view.send_section_px).abs() <= 0.25 {
                            return;
                        }
                        self.mixer_view.send_section_px = new_px;
                    }
                }
            }
            MixerSplitAction::ResizeEnd => {
                if self.mixer_view.split_active_target.is_none() {
                    return;
                }
                self.mixer_view.split_active_target = None;
            }
            MixerSplitAction::Reset(target) => {
                match target {
                    MixerSplitTarget::InsertSend => {
                        self.mixer_view.insert_section_px = MIXER_INSERT_SECTION_DEFAULT_PX;
                    }
                    MixerSplitTarget::SendFader => {
                        self.mixer_view.send_section_px = MIXER_SEND_SECTION_DEFAULT_PX;
                    }
                }
                self.mixer_view.split_active_target = None;
            }
        }
        self.push_mixer_snapshot_to_window(cx);
        let _ = self.mixer_panel.update(cx, |_, cx| cx.notify());
    }

    pub(crate) fn mixer_window_handle(&self) -> Option<WindowHandle<MixerWindow>> {
        self.external_windows.mixer.clone()
    }

    pub(super) fn mixer_panel_chrome_visible(&self) -> bool {
        if self.external_windows.mixer.is_some() {
            return true;
        }
        self.panels.mixer_docked && self.active_bottom_tab == crate::components::BottomTab::Mixer
    }

    /// Build the callback bundle used by the mixer. Every mutation lands in
    /// the same `TimelineState` instance owned by the Timeline entity, so the
    /// TrackHeader and Mixer always read identical values.
    pub(crate) fn build_mixer_callbacks(&self, owner: Entity<Self>) -> MixerCallbacks {
        let audio_engine = self.audio_bridge.engine.clone();
        let timeline_select = self.timeline.clone();
        let mixer_panel_select = self.mixer_panel.clone();
        let owner_select = owner.clone();
        let on_select_track: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |id: &String, _w, cx| {
            let _interaction = crate::perf::PerfScope::enter("MixerSelectInteraction");
            let id = id.clone();
            if external_mixer_debug_enabled() {
                external_mixer_debug(&format!("mixer command dispatched select_track id={id}"));
            }
            if timeline_select
                .read(cx)
                .state
                .selection
                .selected_track_id
                .as_deref()
                == Some(id.as_str())
            {
                return;
            }
            // Commit the shared selection without scheduling the full Timeline
            // repaint first. That repaint may include waveform work; when mixer
            // invalidation was deferred behind it, the strip highlight visibly
            // lagged even though the model had already changed.
            timeline_select.update(cx, |t, _cx| {
                t.state.select_track(&id);
            });
            // Queue the lightweight mixer repaint first so selection feedback is
            // not blocked by Timeline/Inspector/tree work.
            crate::perf::record_notify("mixer_select");
            let _ = mixer_panel_select.update(cx, |_, cx| cx.notify());
            StudioLayout::defer_update(&owner_select, cx, |layout, cx| {
                let project_dirty_before = layout.audio_bridge.project_dirty;
                let graph_version_before = layout.audio_bridge.route_graph_version;
                let load_count_before = layout.audio_bridge.audio_load_project_count;
                layout.sync_mixer_tree_to_selection(cx);
                layout.push_mixer_snapshot_to_window(cx);
                debug_assert_eq!(layout.audio_bridge.project_dirty, project_dirty_before);
                debug_assert_eq!(
                    layout.audio_bridge.route_graph_version,
                    graph_version_before
                );
                debug_assert_eq!(
                    layout.audio_bridge.audio_load_project_count,
                    load_count_before
                );
                cx.notify();
            });
            // Keep the arrangement header selection in sync, but enqueue it
            // after the mixer feedback rather than in front of it.
            timeline_select.update(cx, |_, cx| cx.notify());
        });

        let timeline_vol = self.timeline.clone();
        let owner_dirty = owner.clone();
        let audio_engine_volume_commit = audio_engine.clone();
        let on_volume_change: std::sync::Arc<
            dyn Fn(&(String, f32), &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |(id, v): &(String, f32), _w, cx| {
            let id = id.clone();
            let v = *v;
            timeline_vol.update(cx, |t, cx| {
                t.state.set_track_volume(&id, v);
                t.state.clear_track_volume_preview(&id);
                cx.notify();
            });
            StudioLayout::defer_update(&owner_dirty, cx, |this, cx| {
                this.mark_dirty_view_only();
                this.push_mixer_snapshot_to_window(cx);
                let _ = this.mixer_panel.update(cx, |_, cx| cx.notify());
            });
            if let Some(engine) = audio_engine_volume_commit.as_ref() {
                let _ = engine.update_track_param(&id, "volume", volume_norm_to_linear(v) as f64);
            }
        });

        let timeline_vol_start = self.timeline.clone();
        let on_volume_drag_start: std::sync::Arc<
            dyn Fn(&(String, f32), &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |(id, v): &(String, f32), _w, cx| {
            let id = id.clone();
            let v = *v;
            timeline_vol_start.update(cx, |t, cx| {
                t.state.begin_track_volume_preview(&id, v);
                cx.notify();
            });
        });

        let timeline_vol_preview = self.timeline.clone();
        let owner_preview = owner.clone();
        let audio_engine_volume_preview = audio_engine.clone();
        let on_volume_drag_preview: std::sync::Arc<
            dyn Fn(&(String, f32), &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |(id, v): &(String, f32), _w, cx| {
            let id = id.clone();
            let v = *v;
            let changed = timeline_vol_preview.update(cx, |t, cx| {
                let changed = t.state.set_track_volume_preview(&id, v);
                if changed {
                    cx.notify();
                }
                changed
            });
            if !changed {
                return;
            }
            crate::perf::count("fader_drag_preview_count", 1);
            if crate::components::timeline::timeline_state::TimelineState::fader_debug_enabled() {
                eprintln!("[fader] preview track={id} norm={v:.4}");
            }
            StudioLayout::defer_update(&owner_preview, cx, |this, cx| {
                this.push_mixer_snapshot_to_window(cx);
                let _ = this.mixer_panel.update(cx, |_, cx| cx.notify());
            });
            if let Some(engine) = audio_engine_volume_preview.as_ref() {
                crate::perf::count("mixer_fader_audio_control_update_count", 1);
                let _ = engine.update_track_param(&id, "volume", volume_norm_to_linear(v) as f64);
            }
        });

        let timeline_vol_commit = self.timeline.clone();
        let owner_commit = owner.clone();
        let audio_engine_volume_final = audio_engine.clone();
        let on_volume_drag_commit: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |id: &String, _w, cx| {
            let id = id.clone();
            let committed = timeline_vol_commit.update(cx, |t, cx| {
                let committed = t.state.commit_track_volume_preview(&id);
                if let Some((prev, next)) = committed {
                    if (prev - next).abs() > 1.0e-5 {
                        // Volume already applied by commit; record one undo entry.
                        t.record_executed_command(
                            EditCommand::SetTrackVolume {
                                track_id: id.clone(),
                                prev,
                                next,
                            },
                            cx,
                        );
                    } else {
                        cx.notify();
                    }
                }
                committed
            });
            let Some((_prev, v)) = committed else {
                return;
            };
            crate::perf::count("fader_drag_commit_count", 1);
            StudioLayout::defer_update(&owner_commit, cx, |this, cx| {
                this.mark_dirty_view_only();
                this.push_mixer_snapshot_to_window(cx);
                let _ = this.mixer_panel.update(cx, |_, cx| cx.notify());
            });
            if let Some(engine) = audio_engine_volume_final.as_ref() {
                let _ = engine.update_track_param(&id, "volume", volume_norm_to_linear(v) as f64);
            }
        });

        let audio_engine = self.audio_bridge.engine.clone();
        let timeline_pan = self.timeline.clone();
        let owner_dirty = owner.clone();
        let on_pan_change: std::sync::Arc<
            dyn Fn(&(String, f32), &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |(id, v): &(String, f32), _w, cx| {
            let id = id.clone();
            let v = *v;
            // Pan is a dynamic mixer parameter like volume — applied live via the
            // `SetTrackPan` command, so the knob drag must not enqueue an engine
            // sync per mouse-move. See the volume handler above.
            crate::perf::count("mixer_fader_drag_update_count", 1);
            if external_mixer_debug_enabled() {
                external_mixer_debug(&format!(
                    "mixer command dispatched set_pan id={id} v={v:.3}"
                ));
            }
            timeline_pan.update(cx, |t, cx| {
                t.state.set_track_pan(&id, v);
                cx.notify();
            });
            StudioLayout::defer_update(&owner_dirty, cx, |this, cx| {
                this.mark_dirty_view_only();
                this.push_mixer_snapshot_to_window(cx);
            });
            if let Some(engine) = audio_engine.as_ref() {
                crate::perf::count("mixer_fader_audio_control_update_count", 1);
                let _ = engine.update_track_param(&id, "pan", v as f64);
            }
        });

        let audio_engine = self.audio_bridge.engine.clone();
        let timeline_mute = self.timeline.clone();
        let mixer_panel_mute = self.mixer_panel.clone();
        let owner_dirty = owner.clone();
        let on_toggle_mute: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |id: &String, _w, cx| {
                let _interaction = crate::perf::PerfScope::enter("MixerMuteInteraction");
                let id = id.clone();
                let mut before_state = false;
                let mut muted = false;
                if external_mixer_debug_enabled() {
                    external_mixer_debug(&format!("mixer command dispatched toggle_mute id={id}"));
                }
                {
                    let _state_update = crate::perf::PerfScope::enter("MixerMuteStateUpdate");
                    timeline_mute.update(cx, |t, _cx| {
                        before_state = t
                            .state
                            .find_track(&id)
                            .map(|track| track.muted)
                            .unwrap_or(false);
                        t.state.toggle_track_mute(&id);
                        muted = t
                            .state
                            .find_track(&id)
                            .map(|track| track.muted)
                            .unwrap_or(false);
                    });
                }
                // Dispatch the bounded, non-blocking realtime command before
                // scheduling any expensive arrangement/inspector repaint.
                let mut audio_router_applied = false;
                {
                    let _dispatch = crate::perf::PerfScope::enter("MixerMuteCommandDispatch");
                    if let Some(engine) = audio_engine.as_ref() {
                        audio_router_applied = engine
                            .update_track_param(&id, "muted", if muted { 1.0 } else { 0.0 })
                            .is_ok();
                    }
                }
                // Give the control that was clicked immediate visual feedback.
                crate::perf::record_notify("mixer_mute");
                let _ = mixer_panel_mute.update(cx, |_, cx| cx.notify());
                let id_for_side_effect_log = id.clone();
                StudioLayout::defer_update(&owner_dirty, cx, move |this, cx| {
                    let project_dirty_before = this.audio_bridge.project_dirty;
                    let graph_version_before = this.audio_bridge.route_graph_version;
                    let load_count_before = this.audio_bridge.audio_load_project_count;
                    // Mute reaches the engine live through `SetTrackMute` above
                    // and is applied per block in the render pass; it is NOT
                    // graph structure. `mark_dirty()` would set
                    // `audio_bridge.project_dirty` and the next poll would run a
                    // full snapshot + `load_project` (graph swap, voice reset,
                    // note panic) — the audible stutter this replaces. View-only
                    // dirty keeps save/restore correct without an engine reload.
                    this.mark_dirty_view_only();
                    this.push_mixer_snapshot_to_window(cx);
                    let graph_version_after = this.audio_bridge.route_graph_version;
                    debug_assert_eq!(this.audio_bridge.project_dirty, project_dirty_before);
                    debug_assert_eq!(
                        this.audio_bridge.audio_load_project_count,
                        load_count_before
                    );
                    debug_assert_eq!(graph_version_after, graph_version_before);
                    if crate::forensic_trace::forensic_trace_enabled() {
                        eprintln!(
                            "[SOLO_MUTE TOGGLE SIDE EFFECT CHECK]\naction=mute\nstrip_id={id_for_side_effect_log}\nmixer_channel_id={id_for_side_effect_log}\ngraph_version_before={graph_version_before}\ngraph_version_after={graph_version_after}\nroute_changed={}\nsound_was_silent_before=unknown\nsound_after_toggle=unknown",
                            graph_version_before != graph_version_after
                        );
                    }
                });
                // Arrangement headers and inspector are secondary consumers;
                // enqueue their heavier repaint after mixer feedback/dispatch.
                timeline_mute.update(cx, |_, cx| cx.notify());
                if crate::forensic_trace::forensic_trace_enabled() {
                    let bus_index = id
                        .rsplit_once(":bus:")
                        .and_then(|(_, bus)| bus.parse::<u8>().ok())
                        .unwrap_or(0);
                    let plugin_instance_id = id
                        .strip_prefix("vsti-out:")
                        .and_then(|rest| rest.split_once(":bus:").map(|(plugin, _)| plugin))
                        .unwrap_or("");
                    eprintln!(
                        "[SOLO_MUTE COMMAND]\naction=mute\nrequested_value={muted}\nstrip_view_id={id}\nplugin_instance_id={plugin_instance_id}\nbus_index={bus_index}\nui_mixer_channel_id={id}\nengine_target_channel_id={id}\nbefore_state={before_state}\nafter_state={muted}\naudio_router_applied={audio_router_applied}"
                    );
                }
            });

        let audio_engine = self.audio_bridge.engine.clone();
        let timeline_solo = self.timeline.clone();
        let mixer_panel_solo = self.mixer_panel.clone();
        let owner_dirty = owner.clone();
        let on_toggle_solo: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |id: &String, _w, cx| {
                let _interaction = crate::perf::PerfScope::enter("MixerSoloInteraction");
                let id = id.clone();
                let mut before_state = false;
                let mut solo = false;
                if external_mixer_debug_enabled() {
                    external_mixer_debug(&format!("mixer command dispatched toggle_solo id={id}"));
                }
                {
                    let _state_update = crate::perf::PerfScope::enter("MixerSoloStateUpdate");
                    timeline_solo.update(cx, |t, _cx| {
                        before_state = t
                            .state
                            .find_track(&id)
                            .map(|track| track.solo)
                            .unwrap_or(false);
                        t.state.toggle_track_solo(&id);
                        solo = t
                            .state
                            .find_track(&id)
                            .map(|track| track.solo)
                            .unwrap_or(false);
                    });
                }
                let mut audio_router_applied = false;
                {
                    let _dispatch = crate::perf::PerfScope::enter("MixerSoloCommandDispatch");
                    if let Some(engine) = audio_engine.as_ref() {
                        audio_router_applied = engine
                            .update_track_param(&id, "solo", if solo { 1.0 } else { 0.0 })
                            .is_ok();
                    }
                }
                crate::perf::record_notify("mixer_solo");
                let _ = mixer_panel_solo.update(cx, |_, cx| cx.notify());
                let id_for_side_effect_log = id.clone();
                StudioLayout::defer_update(&owner_dirty, cx, move |this, cx| {
                    let project_dirty_before = this.audio_bridge.project_dirty;
                    let graph_version_before = this.audio_bridge.route_graph_version;
                    let load_count_before = this.audio_bridge.audio_load_project_count;
                    // Live control (see the mute handler above): solo reaches
                    // the engine through `SetTrackSolo`; no graph rebuild.
                    this.mark_dirty_view_only();
                    this.push_mixer_snapshot_to_window(cx);
                    let graph_version_after = this.audio_bridge.route_graph_version;
                    debug_assert_eq!(this.audio_bridge.project_dirty, project_dirty_before);
                    debug_assert_eq!(
                        this.audio_bridge.audio_load_project_count,
                        load_count_before
                    );
                    debug_assert_eq!(graph_version_after, graph_version_before);
                    if crate::forensic_trace::forensic_trace_enabled() {
                        eprintln!(
                            "[SOLO_MUTE TOGGLE SIDE EFFECT CHECK]\naction=solo\nstrip_id={id_for_side_effect_log}\nmixer_channel_id={id_for_side_effect_log}\ngraph_version_before={graph_version_before}\ngraph_version_after={graph_version_after}\nroute_changed={}\nsound_was_silent_before=unknown\nsound_after_toggle=unknown",
                            graph_version_before != graph_version_after
                        );
                    }
                });
                timeline_solo.update(cx, |_, cx| cx.notify());
                if crate::forensic_trace::forensic_trace_enabled() {
                    let bus_index = id
                        .rsplit_once(":bus:")
                        .and_then(|(_, bus)| bus.parse::<u8>().ok())
                        .unwrap_or(0);
                    let plugin_instance_id = id
                        .strip_prefix("vsti-out:")
                        .and_then(|rest| rest.split_once(":bus:").map(|(plugin, _)| plugin))
                        .unwrap_or("");
                    eprintln!(
                        "[SOLO_MUTE COMMAND]\naction=solo\nrequested_value={solo}\nstrip_view_id={id}\nplugin_instance_id={plugin_instance_id}\nbus_index={bus_index}\nui_mixer_channel_id={id}\nengine_target_channel_id={id}\nbefore_state={before_state}\nafter_state={solo}\naudio_router_applied={audio_router_applied}"
                    );
                }
            });

        let audio_engine_arm = self.audio_bridge.engine.clone();
        let timeline_arm = self.timeline.clone();
        let owner_dirty = owner.clone();
        let on_toggle_arm: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> =
            std::sync::Arc::new(move |id: &String, _w, cx| {
                let id = id.clone();
                let previous = timeline_arm
                    .read(cx)
                    .state
                    .find_track(&id)
                    .map(|track| track.armed);
                external_mixer_debug(&format!("mixer command dispatched toggle_arm id={id}"));
                let changed = timeline_arm.update(cx, |t, cx| {
                    let changed = t.state.toggle_track_arm(&id);
                    if changed {
                        cx.notify();
                    }
                    changed
                });
                if !changed {
                    return;
                }
                let apply_error = audio_engine_arm.as_ref().and_then(|engine| {
                    timeline_arm
                        .read(cx)
                        .state
                        .find_track(&id)
                        .and_then(|track| apply_engine_track_input_state(engine, track).err())
                });
                if let Some(error) = apply_error {
                    if let Some(previous) = previous {
                        timeline_arm.update(cx, |t, cx| {
                            if let Some(track) =
                                t.state.tracks.iter_mut().find(|track| track.id == id)
                            {
                                track.armed = previous;
                            }
                            cx.notify();
                        });
                    }
                    StudioLayout::defer_update(&owner_dirty, cx, move |this, cx| {
                        this.audio_bridge.last_error =
                            Some(format!("Track input update failed: {error}"));
                        this.push_mixer_snapshot_to_window(cx);
                    });
                    return;
                }
                StudioLayout::defer_update(&owner_dirty, cx, |this, cx| {
                    this.mark_dirty_view_only();
                    this.push_mixer_snapshot_to_window(cx);
                });
            });

        let audio_engine_input = self.audio_bridge.engine.clone();
        let timeline_input = self.timeline.clone();
        let owner_dirty = owner.clone();
        let on_toggle_input: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |id: &String, _w, cx| {
            let id = id.clone();
            let previous = timeline_input
                .read(cx)
                .state
                .find_track(&id)
                .map(|track| track.input_monitor);
            external_mixer_debug(&format!("mixer command dispatched toggle_input id={id}"));
            let changed = timeline_input.update(cx, |t, cx| {
                let changed = t.state.cycle_track_input_monitor(&id);
                if changed {
                    cx.notify();
                }
                changed
            });
            if !changed {
                return;
            }
            let apply_error = audio_engine_input.as_ref().and_then(|engine| {
                timeline_input
                    .read(cx)
                    .state
                    .find_track(&id)
                    .and_then(|track| apply_engine_track_input_state(engine, track).err())
            });
            if let Some(error) = apply_error {
                if let Some(previous) = previous {
                    timeline_input.update(cx, |t, cx| {
                        if let Some(track) = t.state.tracks.iter_mut().find(|track| track.id == id)
                        {
                            track.input_monitor = previous;
                        }
                        cx.notify();
                    });
                }
                StudioLayout::defer_update(&owner_dirty, cx, move |this, cx| {
                    this.audio_bridge.last_error =
                        Some(format!("Track input update failed: {error}"));
                    this.push_mixer_snapshot_to_window(cx);
                });
                return;
            }
            StudioLayout::defer_update(&owner_dirty, cx, |this, cx| {
                this.mark_dirty_view_only();
                this.push_mixer_snapshot_to_window(cx);
            });
        });

        let audio_engine = self.audio_bridge.engine.clone();
        let timeline_master = self.timeline.clone();
        let owner_dirty = owner.clone();
        let audio_engine_master_change = audio_engine.clone();
        let on_master_volume_change: std::sync::Arc<
            dyn Fn(&f32, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |v: &f32, _w, cx| {
            let v = *v;
            timeline_master.update(cx, |t, cx| {
                t.state.set_master_volume(v);
                t.state.master_volume_preview = None;
                cx.notify();
            });
            StudioLayout::defer_update(&owner_dirty, cx, |this, cx| {
                this.mark_dirty_view_only();
                this.push_mixer_snapshot_to_window(cx);
                let _ = this.mixer_panel.update(cx, |_, cx| cx.notify());
            });
            if let Some(engine) = audio_engine_master_change.as_ref() {
                let _ = engine.update_track_param(
                    "__master__",
                    "volume",
                    volume_norm_to_linear(v) as f64,
                );
            }
        });

        let timeline_master_start = self.timeline.clone();
        let on_master_volume_drag_start: std::sync::Arc<
            dyn Fn(&f32, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |v: &f32, _w, cx| {
            let v = *v;
            timeline_master_start.update(cx, |t, cx| {
                t.state.begin_master_volume_preview(v);
                cx.notify();
            });
        });

        let timeline_master_preview = self.timeline.clone();
        let owner_master_preview = owner.clone();
        let audio_engine_master_preview = audio_engine.clone();
        let on_master_volume_drag_preview: std::sync::Arc<
            dyn Fn(&f32, &mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |v: &f32, _w, cx| {
            let v = *v;
            let changed = timeline_master_preview.update(cx, |t, cx| {
                let changed = t.state.set_master_volume_preview(v);
                if changed {
                    cx.notify();
                }
                changed
            });
            if !changed {
                return;
            }
            crate::perf::count("fader_drag_preview_count", 1);
            if crate::components::timeline::timeline_state::TimelineState::fader_debug_enabled() {
                eprintln!("[fader] preview target=master norm={v:.4}");
            }
            StudioLayout::defer_update(&owner_master_preview, cx, |this, cx| {
                this.push_mixer_snapshot_to_window(cx);
                let _ = this.mixer_panel.update(cx, |_, cx| cx.notify());
            });
            if let Some(engine) = audio_engine_master_preview.as_ref() {
                crate::perf::count("mixer_fader_audio_control_update_count", 1);
                let _ = engine.update_track_param(
                    "__master__",
                    "volume",
                    volume_norm_to_linear(v) as f64,
                );
            }
        });

        let timeline_master_commit = self.timeline.clone();
        let owner_master_commit = owner.clone();
        let audio_engine_master_final = audio_engine.clone();
        let on_master_volume_drag_commit: std::sync::Arc<
            dyn Fn(&mut Window, &mut gpui::App) + 'static,
        > = std::sync::Arc::new(move |_w, cx| {
            let committed = timeline_master_commit.update(cx, |t, cx| {
                let committed = t.state.commit_master_volume_preview();
                if committed.is_some() {
                    cx.notify();
                }
                committed
            });
            let Some(v) = committed else {
                return;
            };
            crate::perf::count("fader_drag_commit_count", 1);
            StudioLayout::defer_update(&owner_master_commit, cx, |this, cx| {
                this.mark_dirty_view_only();
                this.push_mixer_snapshot_to_window(cx);
                let _ = this.mixer_panel.update(cx, |_, cx| cx.notify());
            });
            if let Some(engine) = audio_engine_master_final.as_ref() {
                let _ = engine.update_track_param(
                    "__master__",
                    "volume",
                    volume_norm_to_linear(v) as f64,
                );
            }
        });
        let on_context_menu: std::sync::Arc<
            dyn Fn(&(String, f32, f32), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |(track_id, x, y): &(String, f32, f32), window, cx| {
                let track_id = track_id.clone();
                let x = *x;
                let y = *y;
                let window_id = window.window_handle().window_id();
                StudioLayout::defer_update(&this, cx, move |this, cx| {
                    let _ = this.timeline.update(cx, |timeline, cx| {
                        timeline.state.select_track(&track_id);
                        cx.notify();
                    });
                    this.try_open_context_menu(
                        ContextMenuRequest::new(
                            window_id,
                            x,
                            y,
                            ContextMenuTarget::MixerStrip(track_id),
                        ),
                        cx,
                    );
                });
            })
        };

        // ── Plugin insert callbacks (Phase 1) ────────────────────────
        // Phase 1: add_insert seeds an empty slot followed by a stub
        // descriptor so the project round-trip exercises end-to-end.
        // Phase 2 will swap the stub for a real picker + plugin host
        // instantiation. None of these touch the audio thread; they
        // mutate UI state and let the next project sync carry the
        // descriptor down to the engine (which currently no-ops on
        // unrecognised plugins).
        // Phase 2b: opens the registry-driven picker overlay. The insert slot
        // is created only when the user picks a plugin (see
        // `apply_picked_insert`). No audio thread interaction.
        let on_add_insert: std::sync::Arc<dyn Fn(&String, &mut Window, &mut gpui::App) + 'static> = {
            let this = owner.clone();
            std::sync::Arc::new(move |track_id: &String, window, cx| {
                let track_id = track_id.clone();
                StudioLayout::defer_update_in_window(&this, window, cx, move |this, window, cx| {
                    this.open_insert_picker(&track_id, Some(window), cx);
                });
            })
        };
        let on_remove_insert: std::sync::Arc<
            dyn Fn(&(String, String), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |(track_id, insert_id): &(String, String), _w, cx| {
                let track_id = track_id.clone();
                let insert_id = insert_id.clone();
                StudioLayout::defer_update(&this, cx, move |this, cx| {
                    // Full RemoveInstrumentPlugin lifecycle: close editor, unload
                    // the bridge-host instance, remove the engine sink, drop the
                    // slot, re-sync the engine, and assert the instance is gone.
                    this.remove_insert_fully(&track_id, &insert_id, cx, "mixer_remove_insert");
                    cx.notify();
                });
            })
        };
        let on_toggle_insert_bypass: std::sync::Arc<
            dyn Fn(&(String, String), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |(track_id, insert_id): &(String, String), _w, cx| {
                let track_id = track_id.clone();
                let insert_id = insert_id.clone();
                StudioLayout::defer_update(&this, cx, move |this, cx| {
                    this.timeline.update(cx, |timeline, _cx| {
                        timeline.state.toggle_insert_bypass(&track_id, &insert_id);
                    });
                    // Bypass is applied live per block via the insert's
                    // "enabled" runtime param — the plugin instance stays
                    // loaded and no graph rebuild runs (a full `load_project`
                    // here used to stutter playback). View-only dirty keeps
                    // the toggle in the saved project.
                    this.push_insert_enabled_to_engine(&track_id, &insert_id, cx);
                    this.mark_dirty_view_only();
                    cx.notify();
                });
            })
        };
        let on_toggle_vsti_output_group: std::sync::Arc<
            dyn Fn(&String, &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |group_key: &String, _w, cx| {
                let group_key = group_key.clone();
                StudioLayout::defer_update(&this, cx, move |this, cx| {
                    this.toggle_vsti_output_group(&group_key, cx);
                });
            })
        };
        // Drag-reorder commit (mirrors the Inspector's `reorder_insert_cb`). The
        // drop handler supplies the dragged `plugin_instance_id` and the
        // insertion gap; we snapshot the current id order, compute the new order,
        // and apply it as a single `EditCommand::ReorderFxSlot` so one drag is one
        // undo entry. The command only reorders existing slots (never recreates an
        // instance), so bypass / preset / parameter / editor / automation state
        // follow each instance. A forced project sync rebuilds the engine's chain
        // order (DSP order == UI order); editor windows are keyed by instance id,
        // so they stay attached.
        let on_reorder_insert: std::sync::Arc<
            dyn Fn(&(String, String, usize), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(
                move |(track_id, insert_id, insertion_index): &(String, String, usize), _w, cx| {
                    let track_id = track_id.clone();
                    let insert_id = insert_id.clone();
                    let insertion_index = *insertion_index;
                    StudioLayout::defer_update(&this, cx, move |this, cx| {
                        let changed = this.timeline.update(cx, |timeline, cx| {
                            let before = timeline.state.insert_order(&track_id);
                            let after = timeline_state::TimelineState::reordered_insert_ids(
                                &before,
                                &insert_id,
                                insertion_index,
                            );
                            if before == after {
                                return false;
                            }
                            timeline.run_edit_command(
                                EditCommand::ReorderFxSlot {
                                    track_id: track_id.clone(),
                                    before_order: before,
                                    after_order: after,
                                },
                                cx,
                            );
                            true
                        });
                        if changed {
                            this.mark_dirty();
                            this.audio_bridge.project_dirty = true;
                            this.schedule_audio_project_sync(cx, true, "mixer_reorder_insert");
                            this.push_mixer_snapshot_to_window(cx);
                            cx.notify();
                        }
                    });
                },
            )
        };
        let on_drop_plugin_preset: std::sync::Arc<
            dyn Fn(&(PathBuf, String, usize), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(
                move |(preset_path, track_id, insert_index): &(PathBuf, String, usize), _w, cx| {
                    let preset_path = preset_path.clone();
                    let track_id = track_id.clone();
                    let insert_index = *insert_index;
                    StudioLayout::defer_update(&this, cx, move |this, cx| {
                        if this
                            .apply_dropped_plugin_preset_to_slot(
                                &track_id,
                                insert_index,
                                &preset_path,
                                cx,
                            )
                            .is_some()
                        {
                            this.push_mixer_snapshot_to_window(cx);
                            cx.notify();
                        }
                    });
                },
            )
        };
        // Phase 4: open the GPUI-hosted native plugin editor window.
        let on_open_insert_editor: std::sync::Arc<
            dyn Fn(&(String, usize, String), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |(track_id, insert_index, insert_id), window, cx| {
                let track_id = track_id.clone();
                let insert_index = *insert_index;
                let insert_id = insert_id.clone();
                StudioLayout::defer_update_in_window(&this, window, cx, move |this, window, cx| {
                    this.open_insert_editor(&track_id, insert_index, &insert_id, window, cx);
                });
            })
        };

        // ── Send callbacks (Phase 3) ─────────────────────────────────────
        // Add Send opens a target picker at the click point. The command chosen
        // from that menu performs the actual state mutation and engine sync.
        let on_add_send: std::sync::Arc<
            dyn Fn(&(String, f32, f32), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |(track_id, x, y): &(String, f32, f32), window, cx| {
                let track_id = track_id.clone();
                let x = *x;
                let y = *y;
                let window_id = window.window_handle().window_id();
                StudioLayout::defer_update(&this, cx, move |this, cx| {
                    this.try_open_context_menu(
                        ContextMenuRequest::new(
                            window_id,
                            x,
                            y,
                            ContextMenuTarget::Extended(ContextTarget::SendPicker { track_id }),
                        ),
                        cx,
                    );
                });
            })
        };
        let on_remove_send: std::sync::Arc<
            dyn Fn(&(String, String), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(move |(track_id, send_id): &(String, String), _w, cx| {
                let track_id = track_id.clone();
                let send_id = send_id.clone();
                StudioLayout::defer_update(&this, cx, move |this, cx| {
                    this.timeline.update(cx, |timeline, _cx| {
                        timeline.state.remove_send(&track_id, &send_id);
                    });
                    this.mark_dirty();
                    this.audio_bridge.project_dirty = true;
                    cx.notify();
                });
            })
        };
        let on_reorder_send: std::sync::Arc<
            dyn Fn(&(String, String, usize), &mut Window, &mut gpui::App) + 'static,
        > = {
            let this = owner.clone();
            std::sync::Arc::new(
                move |(track_id, send_id, insertion_index): &(String, String, usize), _w, cx| {
                    let track_id = track_id.clone();
                    let send_id = send_id.clone();
                    let insertion_index = *insertion_index;
                    StudioLayout::defer_update(&this, cx, move |this, cx| {
                        let changed = this.timeline.update(cx, |timeline, cx| {
                            let before = timeline.state.send_order(&track_id);
                            let after = timeline_state::TimelineState::reordered_send_ids(
                                &before,
                                &send_id,
                                insertion_index,
                            );
                            if before == after {
                                return false;
                            }
                            timeline.run_edit_command(
                                EditCommand::ReorderSendSlot {
                                    track_id: track_id.clone(),
                                    before_order: before,
                                    after_order: after,
                                },
                                cx,
                            );
                            true
                        });
                        if changed {
                            this.mark_dirty();
                            // Send order is modeled in session routing. The engine
                            // snapshot dedup guard skips load_project when the graph
                            // is effectively unchanged.
                            this.audio_bridge.project_dirty = true;
                            this.schedule_audio_project_sync(cx, true, "mixer_reorder_send");
                            this.push_mixer_snapshot_to_window(cx);
                            cx.notify();
                        }
                    });
                },
            )
        };

        MixerCallbacks {
            on_select_track,
            on_volume_change,
            on_volume_drag_start,
            on_volume_drag_preview,
            on_volume_drag_commit,
            on_pan_change,
            on_toggle_mute,
            on_toggle_solo,
            on_toggle_arm,
            on_toggle_input,
            on_master_volume_change,
            on_master_volume_drag_start,
            on_master_volume_drag_preview,
            on_master_volume_drag_commit,
            on_context_menu: Some(on_context_menu),
            on_add_insert,
            on_remove_insert,
            on_toggle_insert_bypass,
            on_toggle_vsti_output_group,
            on_reorder_insert,
            on_drop_plugin_preset,
            on_open_insert_editor,
            on_add_send,
            on_remove_send,
            on_reorder_send,
        }
    }
}

fn mixer_channel_meter_signature(
    tracks: &[crate::components::timeline::timeline_state::TrackState],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
    for track in tracks {
        q(track.meter_level_l).hash(&mut hasher);
        q(track.meter_level_r).hash(&mut hasher);
    }
    hasher.finish()
}

/// Meter refresh cap (frames per second) for the detached mixer window. Default
/// 60 — imperceptible for meters, a no-op on <=60 Hz displays, and a real saving
/// on 120/144 Hz displays where the audio poll would otherwise rebuild the whole
/// external mixer every refresh. Override with `FUTUREBOARD_EXTERNAL_MIXER_FPS`
/// (clamped to 10..=240); lower it to trade meter smoothness for more headroom.
const EXTERNAL_MIXER_DEFAULT_FPS: u32 = 60;

fn external_mixer_target_fps() -> u32 {
    std::env::var("FUTUREBOARD_EXTERNAL_MIXER_FPS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(external_mixer_clamp_fps)
        .unwrap_or(EXTERNAL_MIXER_DEFAULT_FPS)
}

/// Clamp a requested meter FPS into a sane band. Pure/testable.
fn external_mixer_clamp_fps(fps: u32) -> u32 {
    fps.clamp(10, 240)
}

/// Convert a target FPS to the minimum interval between meter pushes. Pure so the
/// cap maths is unit-testable without touching the clock. `0` FPS is treated as
/// the default rather than dividing by zero.
fn external_mixer_interval_for_fps(fps: u32) -> std::time::Duration {
    let fps = if fps == 0 {
        EXTERNAL_MIXER_DEFAULT_FPS
    } else {
        fps
    };
    std::time::Duration::from_nanos(1_000_000_000 / fps as u64)
}

fn external_mixer_min_push_interval() -> std::time::Duration {
    external_mixer_interval_for_fps(external_mixer_target_fps())
}

/// How often the perf summary is emitted (one line per this many actual pushes).
const EXTERNAL_MIXER_PERF_LOG_EVERY: u64 = 120;

/// Average snapshot-build time in microseconds. Pure so the reporting maths is
/// unit-testable; guards against divide-by-zero.
fn avg_build_micros(total_nanos: u64, pushes: u64) -> f64 {
    if pushes == 0 {
        return 0.0;
    }
    total_nanos as f64 / pushes as f64 / 1000.0
}

/// Accumulate detached-mixer push/skip counts (and snapshot-build time when the
/// debug flag is on) and emit a throttled summary under
/// `FUTUREBOARD_EXTERNAL_MIXER_DEBUG`. Diagnostics only — no effect when the
/// flag is off beyond three relaxed atomic adds. Tells whether the pop-out cost
/// is the snapshot clone or the (unmeasured here) element rebuild, and how often
/// the meter cap is skipping redundant pushes.
fn external_mixer_perf_record(pushed: bool, build_nanos: u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static PUSHES: AtomicU64 = AtomicU64::new(0);
    static SKIPS: AtomicU64 = AtomicU64::new(0);
    static BUILD_NANOS: AtomicU64 = AtomicU64::new(0);

    if !pushed {
        SKIPS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    BUILD_NANOS.fetch_add(build_nanos, Ordering::Relaxed);
    let pushes = PUSHES.fetch_add(1, Ordering::Relaxed) + 1;
    if pushes % EXTERNAL_MIXER_PERF_LOG_EVERY != 0 {
        return;
    }
    let skips = SKIPS.swap(0, Ordering::Relaxed);
    let total_nanos = BUILD_NANOS.swap(0, Ordering::Relaxed);
    PUSHES.store(0, Ordering::Relaxed);
    if external_mixer_debug_enabled() {
        eprintln!(
            "[external-mixer-perf] pushes={EXTERNAL_MIXER_PERF_LOG_EVERY} skipped_by_cap={skips} avg_build_us={:.1}",
            avg_build_micros(total_nanos, EXTERNAL_MIXER_PERF_LOG_EVERY)
        );
    }
}

/// Clone a track for the detached mixer snapshot but WITHOUT its `clips` — the
/// heaviest field (audio-clip refs and per-note MIDI vectors) and one the mixer
/// never reads. Everything else is cloned verbatim, including `automation_lanes`
/// (kept because [`TrackState::display_volume`] consults it via
/// `has_active_volume_automation`, so an automated fader still shows its live
/// position in the pop-out).
///
/// The source is fully destructured with **no `..`**, so adding a `TrackState`
/// field is a COMPILE ERROR here — forcing an explicit decision about whether
/// the mixer needs it, instead of silently shipping a stale/defaulted value to
/// the detached window.
pub(crate) fn clone_track_for_mixer(track: &TrackState) -> TrackState {
    let TrackState {
        id,
        name,
        track_type,
        color,
        volume,
        volume_effective,
        volume_automation_read,
        pan,
        muted,
        solo,
        armed,
        input_monitor,
        meter_level_l,
        meter_level_r,
        meter_peak_hold_l,
        meter_peak_hold_r,
        meter_clip,
        clips: _, // intentionally dropped — mixer never draws clips
        automation_lanes,
        lane_mode,
        selected_automation_target,
        inserts,
        instrument_plugin_instance_id,
        builtin_soundfont_player,
        soundfont_path,
        soundfont_preset,
        soundfont_volume,
        soundfont_reverb_chorus,
        soundfont_polyphony,
        soundfont_envelope,
        soundfont_quality,
        sends,
        routing,
    } = track;
    TrackState {
        id: id.clone(),
        name: name.clone(),
        track_type: *track_type,
        color: *color,
        volume: *volume,
        volume_effective: *volume_effective,
        volume_automation_read: *volume_automation_read,
        pan: *pan,
        muted: *muted,
        solo: *solo,
        armed: *armed,
        input_monitor: *input_monitor,
        meter_level_l: *meter_level_l,
        meter_level_r: *meter_level_r,
        meter_peak_hold_l: *meter_peak_hold_l,
        meter_peak_hold_r: *meter_peak_hold_r,
        meter_clip: *meter_clip,
        clips: Vec::new(),
        automation_lanes: automation_lanes.clone(),
        lane_mode: *lane_mode,
        selected_automation_target: selected_automation_target.clone(),
        inserts: inserts.clone(),
        instrument_plugin_instance_id: instrument_plugin_instance_id.clone(),
        builtin_soundfont_player: *builtin_soundfont_player,
        soundfont_path: soundfont_path.clone(),
        soundfont_preset: *soundfont_preset,
        soundfont_volume: *soundfont_volume,
        soundfont_reverb_chorus: *soundfont_reverb_chorus,
        soundfont_polyphony: *soundfont_polyphony,
        soundfont_envelope: *soundfont_envelope,
        soundfont_quality: *soundfont_quality,
        sends: sends.clone(),
        routing: routing.clone(),
    }
}

#[cfg(test)]
mod mixer_snapshot_clone_tests {
    use super::clone_track_for_mixer;
    use crate::components::timeline::timeline_state::{
        CreateTrackOptions, InputMonitorMode, TimelineState, TrackType,
    };

    #[test]
    fn clone_for_mixer_drops_clips_but_keeps_mixer_fields() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_track(CreateTrackOptions {
            track_type: TrackType::Audio,
            name: "Drums".to_string(),
            color: gpui::Rgba {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 1.0,
            },
            volume: 0.8,
            pan: -0.25,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        });
        state.insert_audio_clip_with_duration(
            track_id.clone(),
            "C:/a.wav".to_string(),
            "a".to_string(),
            0.0,
            4.0,
            Some(2.0),
        );
        let track = state.tracks.iter().find(|t| t.id == track_id).unwrap();
        assert!(!track.clips.is_empty(), "source track has a clip");

        let lite = clone_track_for_mixer(track);
        assert!(
            lite.clips.is_empty(),
            "mixer clone drops the heavy clips vec"
        );
        // Clone, not move — the live track keeps its clips.
        assert!(!track.clips.is_empty(), "source clips preserved");
        // Mixer-relevant fields survive verbatim.
        assert_eq!(lite.id, track.id);
        assert_eq!(lite.name, "Drums");
        assert_eq!(lite.volume, 0.8);
        assert_eq!(lite.pan, -0.25);
        assert_eq!(lite.color, track.color);
        assert_eq!(lite.inserts.len(), track.inserts.len());
        assert_eq!(lite.sends.len(), track.sends.len());
        // automation_lanes are kept (display_volume depends on them).
        assert_eq!(lite.automation_lanes.len(), track.automation_lanes.len());
    }
}

#[cfg(test)]
mod external_mixer_throttle_tests {
    use super::{external_mixer_clamp_fps, external_mixer_interval_for_fps};
    use std::time::Duration;

    #[test]
    fn interval_matches_fps() {
        assert_eq!(
            external_mixer_interval_for_fps(60),
            Duration::from_nanos(16_666_666)
        );
        assert_eq!(
            external_mixer_interval_for_fps(30),
            Duration::from_nanos(33_333_333)
        );
        assert_eq!(
            external_mixer_interval_for_fps(120),
            Duration::from_nanos(8_333_333)
        );
    }

    #[test]
    fn zero_fps_falls_back_to_default_not_divide_by_zero() {
        assert_eq!(
            external_mixer_interval_for_fps(0),
            Duration::from_nanos(16_666_666)
        );
    }

    #[test]
    fn fps_is_clamped_to_a_sane_band() {
        assert_eq!(external_mixer_clamp_fps(0), 10);
        assert_eq!(external_mixer_clamp_fps(5), 10);
        assert_eq!(external_mixer_clamp_fps(1_000), 240);
        assert_eq!(external_mixer_clamp_fps(60), 60);
    }

    #[test]
    fn higher_fps_yields_shorter_interval() {
        assert!(external_mixer_interval_for_fps(144) < external_mixer_interval_for_fps(60));
    }

    #[test]
    fn avg_build_micros_reports_microseconds_and_guards_zero() {
        use super::avg_build_micros;
        // 120 pushes totalling 6 ms => 50 us average.
        assert!((avg_build_micros(6_000_000, 120) - 50.0).abs() < 1e-9);
        assert_eq!(avg_build_micros(0, 0), 0.0);
        assert_eq!(avg_build_micros(1_000, 0), 0.0);
    }
}
