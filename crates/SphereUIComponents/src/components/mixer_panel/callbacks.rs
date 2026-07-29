use gpui::{App, Window};

/// Bundle of mixer interactions hooked up from the layout. Closures land in
/// the same TimelineState mutation methods used by the TrackHeader so the two
/// views can never disagree.
#[derive(Clone)]
pub struct MixerCallbacks {
    pub on_select_track:
        std::sync::Arc<dyn Fn(&String, bool, bool, &mut Window, &mut App) + 'static>,
    pub on_volume_change: std::sync::Arc<dyn Fn(&(String, f32), &mut Window, &mut App) + 'static>,
    pub on_volume_drag_start:
        std::sync::Arc<dyn Fn(&(String, f32), &mut Window, &mut App) + 'static>,
    pub on_volume_drag_preview:
        std::sync::Arc<dyn Fn(&(String, f32), &mut Window, &mut App) + 'static>,
    pub on_volume_drag_commit: std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    pub on_pan_change: std::sync::Arc<dyn Fn(&(String, f32), &mut Window, &mut App) + 'static>,
    pub on_toggle_mute: std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    pub on_toggle_solo: std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    pub on_toggle_arm: std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    pub on_toggle_input: std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    pub on_master_volume_change: std::sync::Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>,
    pub on_master_volume_drag_start: std::sync::Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>,
    pub on_master_volume_drag_preview:
        std::sync::Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>,
    pub on_master_volume_drag_commit: std::sync::Arc<dyn Fn(&mut Window, &mut App) + 'static>,
    pub on_context_menu:
        Option<std::sync::Arc<dyn Fn(&(String, f32, f32), &mut Window, &mut App) + 'static>>,
    /// Open the insert plugin picker overlay for the track (Phase 2b). The
    /// slot is created only when the user picks a plugin; an empty registry
    /// offers a stub fallback so the project round-trip stays exercisable.
    pub on_add_insert: std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    /// Remove the named insert slot from the track.
    pub on_remove_insert:
        std::sync::Arc<dyn Fn(&(String, String), &mut Window, &mut App) + 'static>,
    /// Toggle bypass on the named insert slot.
    pub on_toggle_insert_bypass:
        std::sync::Arc<dyn Fn(&(String, String), &mut Window, &mut App) + 'static>,
    /// Expand/collapse the VSTi output sub-strips for a track/insert group.
    pub on_toggle_vsti_output_group:
        std::sync::Arc<dyn Fn(&String, &mut Window, &mut App) + 'static>,
    /// Drag-reorder commit for an insert slot. `(track_id, dragged_insert_id,
    /// insertion_index)` where `insertion_index` is the gap (0..=len) the
    /// dragged slot moves into. Identity is the stable `plugin_instance_id`,
    /// never the visual index. One completed drag = one undo entry (mirrors the
    /// Inspector's `on_reorder_insert` / `reorder_insert_cb`).
    pub on_reorder_insert:
        std::sync::Arc<dyn Fn(&(String, String, usize), &mut Window, &mut App) + 'static>,
    /// Drop a `.pst` plug-in preset from the browser into a concrete insert slot.
    /// `(preset_path, track_id, insert_index)` uses the full insert-chain index.
    pub on_drop_plugin_preset: std::sync::Arc<
        dyn Fn(&(std::path::PathBuf, String, usize), &mut Window, &mut App) + 'static,
    >,
    /// User clicked the slot chip — Phase 4 will open the native plugin
    /// editor; Phase 1 logs the request.
    pub on_open_insert_editor:
        std::sync::Arc<dyn Fn(&(String, usize, String), &mut Window, &mut App) + 'static>,
    /// Open the send target picker for `(track_id, x, y)`.
    pub on_add_send: std::sync::Arc<dyn Fn(&(String, f32, f32), &mut Window, &mut App) + 'static>,
    /// Remove the named send `(track_id, send_id)`.
    pub on_remove_send: std::sync::Arc<dyn Fn(&(String, String), &mut Window, &mut App) + 'static>,
    /// Set the send gain `(track_id, send_id, gain_db)`.
    pub on_send_gain_change:
        std::sync::Arc<dyn Fn(&(String, String, f32), &mut Window, &mut App) + 'static>,
    /// Drag-reorder commit for a send slot. `(track_id, dragged_send_id,
    /// insertion_index)` where `insertion_index` is the visual gap.
    pub on_reorder_send:
        std::sync::Arc<dyn Fn(&(String, String, usize), &mut Window, &mut App) + 'static>,
}

/// Inert callbacks for fallback UI when the studio entity is unavailable.
pub fn noop_mixer_callbacks() -> MixerCallbacks {
    use std::sync::Arc;

    let noop_track = Arc::new(|_: &String, _: &mut Window, _: &mut App| {});
    let noop_select = Arc::new(|_: &String, _: bool, _: bool, _: &mut Window, _: &mut App| {});
    let noop_vol = Arc::new(|_: &(String, f32), _: &mut Window, _: &mut App| {});
    let noop_vol_commit = Arc::new(|_: &String, _: &mut Window, _: &mut App| {});
    let noop_pan = Arc::new(|_: &(String, f32), _: &mut Window, _: &mut App| {});
    let noop_master = Arc::new(|_: &f32, _: &mut Window, _: &mut App| {});
    let noop_master_commit = Arc::new(|_: &mut Window, _: &mut App| {});
    let noop_insert_pair = Arc::new(|_: &(String, String), _: &mut Window, _: &mut App| {});
    let noop_insert_open = Arc::new(|_: &(String, usize, String), _: &mut Window, _: &mut App| {});
    let noop_insert_reorder =
        Arc::new(|_: &(String, String, usize), _: &mut Window, _: &mut App| {});
    let noop_preset_drop =
        Arc::new(|_: &(std::path::PathBuf, String, usize), _: &mut Window, _: &mut App| {});
    let noop_add_send = Arc::new(|_: &(String, f32, f32), _: &mut Window, _: &mut App| {});
    let noop_send_reorder = Arc::new(|_: &(String, String, usize), _: &mut Window, _: &mut App| {});
    let noop_send_gain = Arc::new(|_: &(String, String, f32), _: &mut Window, _: &mut App| {});
    MixerCallbacks {
        on_select_track: noop_select,
        on_volume_change: noop_vol.clone(),
        on_volume_drag_start: noop_vol.clone(),
        on_volume_drag_preview: noop_vol,
        on_volume_drag_commit: noop_vol_commit,
        on_pan_change: noop_pan,
        on_toggle_mute: noop_track.clone(),
        on_toggle_solo: noop_track.clone(),
        on_toggle_arm: noop_track.clone(),
        on_toggle_input: noop_track.clone(),
        on_master_volume_change: noop_master.clone(),
        on_master_volume_drag_start: noop_master.clone(),
        on_master_volume_drag_preview: noop_master,
        on_master_volume_drag_commit: noop_master_commit,
        on_context_menu: None,
        on_add_insert: noop_track.clone(),
        on_remove_insert: noop_insert_pair.clone(),
        on_toggle_insert_bypass: noop_insert_pair.clone(),
        on_toggle_vsti_output_group: Arc::new(|_: &String, _: &mut Window, _: &mut App| {}),
        on_reorder_insert: noop_insert_reorder,
        on_drop_plugin_preset: noop_preset_drop,
        on_open_insert_editor: noop_insert_open.clone(),
        on_add_send: noop_add_send,
        on_remove_send: noop_insert_pair,
        on_send_gain_change: noop_send_gain,
        on_reorder_send: noop_send_reorder,
    }
}
