//! Pinned master strip — isolated invalidation from channel scroller / tree.

use gpui::{Context, IntoElement, Render, Window};

use crate::components::mixer_panel::{master_strip, MixerCallbacks, MixerSplit};
use crate::components::timeline::timeline::Timeline;
use crate::i18n::I18n;
use crate::theme::Colors;

pub struct MixerMasterStripView {
    timeline: gpui::Entity<Timeline>,
    callbacks: MixerCallbacks,
    split: MixerSplit,
    strip_available_px: f32,
    last_meter_sig: u64,
    last_structure_key: u64,
}

impl MixerMasterStripView {
    pub fn new(
        timeline: gpui::Entity<Timeline>,
        callbacks: MixerCallbacks,
        split: MixerSplit,
        strip_available_px: f32,
    ) -> Self {
        Self {
            timeline,
            callbacks,
            split,
            strip_available_px,
            last_meter_sig: u64::MAX,
            last_structure_key: u64::MAX,
        }
    }

    pub fn sync_props(
        &mut self,
        callbacks: MixerCallbacks,
        split: MixerSplit,
        strip_available_px: f32,
    ) -> bool {
        let key = structure_key(&split, strip_available_px);
        let changed = key != self.last_structure_key;
        self.callbacks = callbacks;
        self.split = split;
        self.strip_available_px = strip_available_px;
        if changed {
            self.last_structure_key = key;
            crate::perf::count("mixer_static_snapshot_rebuild_count", 1);
        }
        changed
    }

    /// Meter-only tick — repaints when the quantised master meter signature moves.
    pub fn on_meter_tick(&mut self, meter_sig: u64, cx: &mut Context<Self>) -> bool {
        if meter_sig == self.last_meter_sig {
            return false;
        }
        self.last_meter_sig = meter_sig;
        crate::perf::count("mixer_meter_update_count", 1);
        crate::perf::count("mixer_meter_repaint_count", 1);
        cx.notify();
        true
    }
}

impl Render for MixerMasterStripView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::perf::PerfScope::enter("MixerMasterStrip");
        crate::perf::count("mixer_master_layout_count", 1);
        crate::perf::count("mixer_master_paint_count", 1);

        let timeline = self.timeline.read(cx);
        let mut master = timeline.state.master.clone();
        if let Some(v) = timeline.state.master_volume_preview {
            master.volume = v;
        }
        let meter_sig = master_meter_signature(&master);
        self.last_meter_sig = meter_sig;

        let accent = Colors::accent_primary();
        let on_master = self.callbacks.on_master_volume_change.clone();
        let i18n = I18n::from_app(cx);
        master_strip(
            accent,
            &master,
            on_master,
            &self.callbacks,
            &self.split,
            self.strip_available_px,
            i18n,
        )
    }
}

fn structure_key(split: &MixerSplit, strip_available_px: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let q = |v: f32| (v * 4.0).round() as i64;
    q(split.insert_px).hash(&mut hasher);
    q(split.send_px).hash(&mut hasher);
    split.active_target.hash(&mut hasher);
    q(strip_available_px).hash(&mut hasher);
    hasher.finish()
}

fn master_meter_signature(
    master: &crate::components::timeline::timeline_state::MasterBusState,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
    q(master.meter_level_l).hash(&mut hasher);
    q(master.meter_level_r).hash(&mut hasher);
    q(master.meter_peak_hold_l).hash(&mut hasher);
    q(master.meter_peak_hold_r).hash(&mut hasher);
    master.meter_clip.hash(&mut hasher);
    hasher.finish()
}

pub fn mixer_master_meter_signature(
    master: &crate::components::timeline::timeline_state::MasterBusState,
) -> u64 {
    master_meter_signature(master)
}
