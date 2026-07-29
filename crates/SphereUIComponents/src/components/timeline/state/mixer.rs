use super::*;

/// Origin of a track volume change, so the base/effective model can route the
/// write correctly and never let an automation-follow display update masquerade
/// as a user fader edit (which would fight automation / spam dirty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeUpdateSource {
    /// User dragged the mixer/track-header/inspector fader — edits base only.
    UserFader,
    /// Automation read at the playhead — edits effective only.
    AutomationRead,
    /// Project load / programmatic reset — sets base and effective together.
    ProjectLoad,
}

/// Volume / dB mapping helpers. Linear in dB between the soft floor and a
/// little headroom above unity.
pub mod volume {
    pub const MIN_DB: f32 = -60.0;
    pub const MAX_DB: f32 = 6.0;

    pub fn norm_to_db(norm: f32) -> f32 {
        let n = norm.clamp(0.0, 1.0);
        MIN_DB + n * (MAX_DB - MIN_DB)
    }

    pub fn db_to_norm(db: f32) -> f32 {
        ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0)
    }

    pub fn format_db(norm: f32) -> String {
        let db = norm_to_db(norm);
        if norm <= 0.001 || db <= MIN_DB + 0.05 {
            "-∞".to_string()
        } else if db >= 0.0 {
            format!("+{:.1}", db)
        } else {
            format!("{:.1}", db)
        }
    }
}

impl TimelineState {
    // ── Single-source-of-truth mutations ─────────────────────────────────────
    // These are the only paths that should mutate per-track UI state. Both the
    // timeline TrackHeader and the bottom-panel Mixer call into these, so the
    // two views can never drift apart.

    pub fn fader_debug_enabled() -> bool {
        std::env::var_os("FUTUREBOARD_FADER_DEBUG").is_some()
    }

    pub fn display_master_volume(&self) -> f32 {
        self.master_volume_preview.unwrap_or(self.master.volume)
    }

    pub fn display_track_volume(&self, track: &TrackState) -> f32 {
        self.track_volume_previews
            .get(&track.id)
            .copied()
            .unwrap_or_else(|| track.display_volume())
    }

    pub fn set_master_volume(&mut self, norm: f32) {
        self.master.volume = norm.clamp(0.0, 1.0);
    }

    pub fn begin_master_volume_preview(&mut self, norm: f32) {
        self.master_volume_preview = Some(norm.clamp(0.0, 1.0));
        if Self::fader_debug_enabled() {
            eprintln!(
                "[fader] drag start target=master norm={:.4}",
                norm.clamp(0.0, 1.0)
            );
        }
    }

    pub fn set_master_volume_preview(&mut self, norm: f32) -> bool {
        let v = norm.clamp(0.0, 1.0);
        let changed = self
            .master_volume_preview
            .map(|prev| (prev - v).abs() > 1.0e-5)
            .unwrap_or(true);
        if changed {
            self.master_volume_preview = Some(v);
        }
        changed
    }

    pub fn commit_master_volume_preview(&mut self) -> Option<f32> {
        let v = self.master_volume_preview.take()?;
        self.set_master_volume(v);
        if Self::fader_debug_enabled() {
            eprintln!("[fader] commit target=master norm={v:.4}");
        }
        Some(v)
    }

    pub fn begin_track_volume_preview(&mut self, track_id: &str, norm: f32) {
        let v = norm.clamp(0.0, 1.0);
        if !self.track_volume_gesture_origin.contains_key(track_id) {
            let origin = self
                .tracks
                .iter()
                .find(|t| t.id == track_id)
                .map(|t| t.volume)
                .unwrap_or(v);
            self.track_volume_gesture_origin
                .insert(track_id.to_string(), origin);
        }
        self.track_volume_previews.insert(track_id.to_string(), v);
        if Self::fader_debug_enabled() {
            eprintln!("[fader] drag start track={track_id} norm={v:.4}");
        }
    }

    pub fn set_track_volume_preview(&mut self, track_id: &str, norm: f32) -> bool {
        let v = norm.clamp(0.0, 1.0);
        let changed = self
            .track_volume_previews
            .get(track_id)
            .map(|prev| (*prev - v).abs() > 1.0e-5)
            .unwrap_or(true);
        if changed {
            self.track_volume_previews.insert(track_id.to_string(), v);
        }
        changed
    }

    /// Commit a fader drag. Returns `(prev, next)` when a preview existed so
    /// the caller can record one undo entry for the whole gesture.
    pub fn commit_track_volume_preview(&mut self, track_id: &str) -> Option<(f32, f32)> {
        let next = self.track_volume_previews.remove(track_id)?;
        let prev = self
            .track_volume_gesture_origin
            .remove(track_id)
            .unwrap_or(next);
        self.set_track_volume(track_id, next);
        if Self::fader_debug_enabled() {
            eprintln!("[fader] commit track={track_id} prev={prev:.4} next={next:.4}");
        }
        Some((prev, next))
    }

    pub fn clear_track_volume_preview(&mut self, track_id: &str) {
        self.track_volume_previews.remove(track_id);
        self.track_volume_gesture_origin.remove(track_id);
    }

    pub fn apply_volume_previews_to_snapshot(
        &self,
        tracks: &mut [TrackState],
        master: &mut MasterBusState,
    ) {
        if let Some(v) = self.master_volume_preview {
            master.volume = v;
        }
        for track in tracks {
            if let Some(v) = self.track_volume_previews.get(&track.id).copied() {
                track.volume = v;
                track.volume_effective = v;
            }
        }
    }

    /// Set a track's manual/base fader volume (the `UserFader` path). When
    /// automation read is off — or there is no active volume automation — the
    /// effective volume follows the base immediately so the display and runtime
    /// track the fader. When automation read is on with an active lane, base is
    /// updated underneath but effective stays automation-driven (DAW behavior).
    pub fn set_track_volume(&mut self, track_id: &str, norm: f32) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            let v = norm.clamp(0.0, 1.0);
            t.volume = v;
            if !(t.volume_automation_read && t.has_active_volume_automation()) {
                t.volume_effective = v;
            }
            if automation_sync_debug_enabled() {
                eprintln!(
                    "[automation-sync] target=TrackVolume({}) base={:.3}({}) effective={:.3} reason=fader_drag",
                    t.id,
                    v,
                    volume::format_db(v),
                    t.volume_effective,
                );
            }
        }
    }

    /// Toggle whether Track Volume automation drives this track's effective
    /// value. Returns `true` if the flag changed. The caller should follow with
    /// [`Self::recompute_effective_volumes`] at the current playhead so the
    /// fader/inspector preview updates immediately.
    pub fn set_track_volume_automation_read(&mut self, track_id: &str, read: bool) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.volume_automation_read != read {
                t.volume_automation_read = read;
                if !read {
                    t.volume_effective = t.volume;
                }
                return true;
            }
        }
        false
    }

    /// Recompute every track's effective volume from its Track Volume automation
    /// lane at `beat`. UI-only: faders/inspector read [`TrackState::display_volume`]
    /// which prefers the effective value. Returns `true` if any effective value
    /// changed (so the caller can `notify`). `reason` is only used for the
    /// `[automation-sync]` trace and should be one of `playback_tick`, `seek`,
    /// or `point_edit`.
    pub fn recompute_effective_volumes(&mut self, beat: f32, reason: &str) -> bool {
        let debug = automation_sync_debug_enabled();
        let mut changed = false;
        for track in &mut self.tracks {
            let resolved = track
                .automation_lanes
                .iter()
                .find(|l| {
                    l.enabled
                        && matches!(l.target, AutomationTarget::TrackVolume)
                        && !l.points.is_empty()
                })
                .map(|l| evaluate_automation(&l.points, beat as f64, l.target.default_value()));
            let new_effective = match (track.volume_automation_read, resolved) {
                (true, Some(v)) => v,
                _ => track.volume,
            };
            if (track.volume_effective - new_effective).abs() > 1.0e-5 {
                if debug {
                    eprintln!(
                        "[automation-sync] target=TrackVolume({}) beat={:.3} value={:.3}({}) base={:.3}({}) effective {:.3}→{:.3} reason={}",
                        track.id,
                        beat,
                        new_effective,
                        volume::format_db(new_effective),
                        track.volume,
                        volume::format_db(track.volume),
                        track.volume_effective,
                        new_effective,
                        reason,
                    );
                }
                track.volume_effective = new_effective;
                changed = true;
            }
        }
        changed
    }

    pub fn set_track_pan(&mut self, track_id: &str, pan: f32) {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.pan = pan.clamp(-1.0, 1.0);
        }
    }

    pub fn begin_track_pan_preview(&mut self, track_id: &str) {
        if self.track_pan_gesture_origin.contains_key(track_id) {
            return;
        }
        if let Some(pan) = self
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.pan)
        {
            self.track_pan_gesture_origin
                .insert(track_id.to_string(), pan);
        }
    }

    pub fn set_track_pan_preview(&mut self, track_id: &str, pan: f32) -> bool {
        let next = pan.clamp(-1.0, 1.0);
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        if (track.pan - next).abs() <= 1.0e-5 {
            return false;
        }
        track.pan = next;
        true
    }

    pub fn commit_track_pan_preview(&mut self, track_id: &str) -> Option<(f32, f32)> {
        let prev = self.track_pan_gesture_origin.remove(track_id)?;
        let next = self
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.pan)?;
        Some((prev, next))
    }

    pub fn toggle_track_mute(&mut self, track_id: &str) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.muted = !t.muted;
            return true;
        }
        false
    }

    /// Set mute without toggling. Returns `true` when the stored value changed.
    pub fn set_track_mute(&mut self, track_id: &str, muted: bool) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.muted != muted {
                t.muted = muted;
                return true;
            }
        }
        false
    }

    pub fn toggle_track_solo(&mut self, track_id: &str) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.solo = !t.solo;
            return true;
        }
        false
    }

    /// Set solo without toggling. Returns `true` when the stored value changed.
    pub fn set_track_solo(&mut self, track_id: &str, solo: bool) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            if t.solo != solo {
                t.solo = solo;
                return true;
            }
        }
        false
    }

    pub fn toggle_track_arm(&mut self, track_id: &str) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.armed = !t.armed;
            return true;
        }
        false
    }

    pub fn cycle_track_input_monitor(&mut self, track_id: &str) -> bool {
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == track_id) {
            t.input_monitor = t.input_monitor.cycle();
            return true;
        }
        false
    }
}
