//! Production mixer panel entity — region-isolated invalidation from StudioLayout.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    div, px, App, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    Window,
};

use crate::components::mixer_master_strip_view::{
    mixer_master_meter_signature, MixerMasterStripView,
};
use crate::components::mixer_panel::{
    build_mixer_render_snapshot, collect_mixer_render_items, mixer_center_lightweight,
    mixer_render_item_count, mixer_strip_scroller, mixer_sub_header, mixer_visible_item_range,
    MixerRenderItem, MixerSplit, MixerSplitAction, MixerSplitDrag, VstiOutputMeterState,
};
use crate::components::mixer_surface::render_mixer_primitives;
use crate::components::mixer_tree_sidebar_view::MixerTreeSidebar;
use crate::components::timeline::timeline::Timeline;
use crate::i18n::I18n;
use crate::layout::StudioLayout;
use crate::theme::Colors;

pub struct MixerPanelView {
    owner: Entity<StudioLayout>,
    timeline: Entity<Timeline>,
    tree_sidebar: Entity<MixerTreeSidebar>,
    master_strip: Entity<MixerMasterStripView>,
    last_structure_key: u64,
    last_meter_sig: u64,
    last_channel_meter_sig: u64,
}

impl MixerPanelView {
    pub fn new(
        owner: Entity<StudioLayout>,
        timeline: Entity<Timeline>,
        tree_sidebar: Entity<MixerTreeSidebar>,
        master_strip: Entity<MixerMasterStripView>,
    ) -> Self {
        Self {
            owner,
            timeline,
            tree_sidebar,
            master_strip,
            last_structure_key: u64::MAX,
            last_meter_sig: u64::MAX,
            last_channel_meter_sig: u64::MAX,
        }
    }

    /// Meter tick from the audio poll — never invalidates StudioLayout root.
    pub fn on_meter_tick(&mut self, cx: &mut Context<Self>) {
        let (master_sig, channel_sig, strip_count) = {
            let timeline = self.timeline.read(cx);
            let collapsed =
                crate::components::timeline::timeline_state::collapsed_vsti_output_group_keys_from_tracks(
                    &timeline.state.tracks,
                );
            let hidden = &timeline.state.mixer_tree.hidden_channel_ids;
            (
                mixer_master_meter_signature(&timeline.state.master),
                channel_meter_signature(&timeline.state.tracks),
                mixer_render_item_count(&timeline.state.tracks, &collapsed, hidden),
            )
        };
        let master_changed = master_sig != self.last_meter_sig;
        let channel_changed = channel_sig != self.last_channel_meter_sig;

        if !master_changed && !channel_changed {
            return;
        }

        if master_changed {
            self.last_meter_sig = master_sig;
            let _ = self.master_strip.update(cx, |master, cx| {
                master.on_meter_tick(master_sig, cx);
            });
        }

        if channel_changed {
            self.last_channel_meter_sig = channel_sig;
            if strip_count > 0 {
                crate::perf::count("mixer_meter_update_count", 1);
                crate::perf::count("mixer_meter_repaint_count", 1);
                cx.notify();
            }
        }
    }

    fn read_view_state(&self, cx: &App) -> MixerPanelViewState {
        let _scope = crate::perf::PerfScope::enter("MixerViewStateClone");
        let owner = self.owner.read(cx);
        let chrome = owner.docked_mixer_panel_state(cx);
        let timeline = self.timeline.read(cx);
        let collapsed =
            crate::components::timeline::timeline_state::collapsed_vsti_output_group_keys_from_tracks(
                &timeline.state.tracks,
            );
        let hidden = timeline.state.mixer_tree.hidden_channel_ids.clone();
        let render_items = collect_mixer_render_items(&timeline.state.tracks, &collapsed, &hidden);
        let strip_count = render_items.len();
        let visible_range =
            mixer_visible_item_range(strip_count, chrome.scroll_x, chrome.viewport_width);
        let mut detailed_track_indices = HashSet::new();
        for item in &render_items[visible_range] {
            match *item {
                MixerRenderItem::Track { track_index } => {
                    detailed_track_indices.insert(track_index);
                }
                MixerRenderItem::VstiOutput {
                    parent_index,
                    child_index,
                    ..
                } => {
                    detailed_track_indices.insert(parent_index);
                    detailed_track_indices.insert(child_index);
                }
            }
        }

        // Mixer strips never inspect arrangement clips. A full TrackState clone
        // also clones every MIDI note/controller vector, which made a simple
        // selection repaint take seconds in large projects.
        let mut tracks: Vec<_> = timeline
            .state
            .tracks
            .iter()
            .enumerate()
            .map(|(track_index, track)| {
                if detailed_track_indices.contains(&track_index) {
                    crate::layout::clone_track_for_mixer(track)
                } else {
                    crate::layout::clone_track_for_mixer_summary(track)
                }
            })
            .collect();
        let mut master = timeline.state.master.clone();
        timeline
            .state
            .apply_volume_previews_to_snapshot(&mut tracks, &mut master);

        MixerPanelViewState {
            tracks,
            master,
            selected_track_id: timeline.state.selection.selected_track_id.clone(),
            selected_track_ids: timeline.state.selection.selected_track_ids.clone(),
            collapsed,
            hidden,
            vsti_output_meters: chrome.vsti_output_meters.clone(),
            scroll_x: chrome.scroll_x,
            viewport_width: chrome.viewport_width,
            strip_available_px: chrome.strip_available_px,
            strip_count,
            track_count: timeline.state.tracks.len(),
            tree_enabled: chrome.tree_sidebar_enabled,
            gpu_decor: crate::components::mixer_surface::mixer_gpu_primitives_active(),
        }
    }

    fn structure_key(state: &MixerPanelViewState, split: &MixerSplit) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let q = |v: f32| (v * 4.0).round() as i64;
        state.strip_count.hash(&mut hasher);
        state.track_count.hash(&mut hasher);
        state.selected_track_id.as_deref().hash(&mut hasher);
        state.selected_track_ids.hash(&mut hasher);
        q(state.scroll_x).hash(&mut hasher);
        q(state.viewport_width).hash(&mut hasher);
        q(state.strip_available_px).hash(&mut hasher);
        q(split.insert_px).hash(&mut hasher);
        q(split.send_px).hash(&mut hasher);
        split.active_target.hash(&mut hasher);
        state.hidden.len().hash(&mut hasher);
        state.collapsed.len().hash(&mut hasher);
        hasher.finish()
    }
}

struct MixerPanelViewState {
    tracks: Vec<crate::components::timeline::timeline_state::TrackState>,
    master: crate::components::timeline::timeline_state::MasterBusState,
    selected_track_id: Option<String>,
    selected_track_ids: Vec<String>,
    collapsed: HashSet<String>,
    hidden: HashSet<String>,
    vsti_output_meters: HashMap<String, VstiOutputMeterState>,
    scroll_x: f32,
    viewport_width: f32,
    strip_available_px: f32,
    strip_count: usize,
    track_count: usize,
    tree_enabled: bool,
    gpu_decor: bool,
}

impl Render for MixerPanelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::perf::PerfScope::enter("MixerPanel");
        crate::perf::count("mixer_root_layout_count", 1);
        crate::perf::count("mixer_root_paint_count", 1);

        let i18n = I18n::from_app(cx);
        let owner_entity = self.owner.clone();
        let callbacks = self
            .owner
            .read(cx)
            .build_mixer_callbacks(owner_entity.clone());
        let split = build_mixer_split(&self.owner, cx);
        let state = self.read_view_state(cx);

        let structure_key = Self::structure_key(&state, &split);
        if structure_key != self.last_structure_key {
            self.last_structure_key = structure_key;
            crate::perf::count("mixer_static_snapshot_rebuild_count", 1);
        }

        let _ = self.master_strip.update(cx, |master, _cx| {
            master.sync_props(callbacks.clone(), split.clone(), state.strip_available_px);
        });

        let panel_entity = cx.entity();
        let on_scroll = build_scroll_handler(owner_entity.clone(), panel_entity);
        let split_for_move = split.clone();
        let split_for_end = split.clone();

        let mut channel_row = if state.strip_count == 0 {
            crate::perf::count("mixer_center_paint_count", 1);
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .child(mixer_center_lightweight(
                    state.viewport_width,
                    state.strip_available_px,
                ))
                .child(div().w(px(1.0)).h_full().bg(Colors::border_default()))
                .child(self.master_strip.clone())
        } else {
            let strip_row = mixer_strip_scroller(
                &state.tracks,
                state.selected_track_id.as_deref(),
                &state.selected_track_ids,
                callbacks.clone(),
                &state.collapsed,
                &state.hidden,
                &state.vsti_output_meters,
                state.scroll_x,
                state.viewport_width,
                state.strip_available_px,
                &split,
                on_scroll,
                state.gpu_decor,
                i18n,
            );
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .child(strip_row)
                .child(div().w(px(1.0)).h_full().bg(Colors::border_default()))
                .child(self.master_strip.clone())
        };

        // In GPU-decoration mode the strip elements intentionally omit their
        // background, accent, and separator. Compose the same primitive layer
        // used by the detached Mixer Window behind the docked strip row.
        if state.gpu_decor && state.strip_count > 0 {
            let snapshot = build_mixer_render_snapshot(
                &state.tracks,
                &state.collapsed,
                &state.hidden,
                state.selected_track_id.as_deref(),
                &state.selected_track_ids,
                state.scroll_x,
                state.viewport_width,
                state.strip_available_px,
            );
            let primitives = render_mixer_primitives(&snapshot);
            channel_row = div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(primitives)
                .child(channel_row.size_full());
        }

        let body = if state.tree_enabled {
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .child(self.tree_sidebar.clone())
                .child(channel_row)
        } else {
            channel_row
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(Colors::surface_window())
            .on_drag_move::<MixerSplitDrag>(move |event, w, cx| {
                let y: f32 = event.event.position.y.into();
                (split_for_move.on_action)(MixerSplitAction::ResizeMove(y), w, cx);
            })
            .on_mouse_up(gpui::MouseButton::Left, move |_e, w, cx| {
                (split_for_end.on_action)(MixerSplitAction::ResizeEnd, w, cx);
            })
            .child(mixer_sub_header(state.track_count, i18n))
            .child(body)
    }
}

fn build_mixer_split(owner: &Entity<StudioLayout>, cx: &App) -> MixerSplit {
    let owner_entity = owner.clone();
    let on_action: Arc<dyn Fn(MixerSplitAction, &mut Window, &mut App) + 'static> =
        Arc::new(move |action, _w, cx| {
            let _ = owner_entity.update(cx, |layout, cx| {
                layout.apply_mixer_split_action(action, cx);
            });
        });
    let layout = owner.read(cx);
    crate::components::mixer_panel::MixerSplit {
        insert_px: layout.mixer_insert_section_px(),
        send_px: layout.mixer_send_section_px(),
        active_target: layout.mixer_split_active_target(),
        on_action,
    }
}

fn build_scroll_handler(
    owner: Entity<StudioLayout>,
    panel: Entity<MixerPanelView>,
) -> Arc<dyn Fn(f32, &mut Window, &mut App) + 'static> {
    Arc::new(move |new_x, _w, cx| {
        let _ = owner.update(cx, |layout, cx| {
            if layout.set_mixer_scroll_x(new_x, cx) {
                layout.push_mixer_snapshot_to_window(cx);
                let _ = panel.update(cx, |_, cx| cx.notify());
            }
        });
    })
}

fn channel_meter_signature(
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

/// Docked mixer shell: the panel entity is the MixerPanelRoot and owns its body
/// children, including the tree sidebar when enabled.
pub fn docked_mixer_shell(mixer_panel: Entity<MixerPanelView>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .size_full()
        .child(mixer_panel)
}
