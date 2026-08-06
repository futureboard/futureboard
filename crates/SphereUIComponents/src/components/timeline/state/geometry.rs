use super::*;

pub fn beat_to_x(beat: f64, viewport: &TimelineViewport) -> f32 {
    ((beat.max(0.0) as f32) * viewport.pixels_per_beat - viewport.scroll_x).round()
}

pub fn x_to_beat(x: f32, viewport: &TimelineViewport) -> f64 {
    ((x + viewport.scroll_x) / viewport.pixels_per_beat.max(0.0001)).max(0.0) as f64
}

pub fn snap_beat(beat: f64, snap: SnapSettings) -> f64 {
    // Arrangement clips historically clamp to ≥ 0; pre-roll-capable callers
    // should use [`super::musical_snap::snap_beat`] directly.
    super::musical_snap::snap_beat(beat, snap.to_musical(), false).max(0.0)
}

pub fn track_at_y(y: f32, layout: &TrackLayout) -> Option<TrackId> {
    let content_y = y + layout.scroll_y;
    layout
        .rows
        .iter()
        .find(|row| content_y >= row.y && content_y < row.y + row.height)
        .map(|row| row.track_id.clone())
}

pub fn clip_rect(
    clip: &ClipState,
    viewport: &TimelineViewport,
    layout: &TrackLayout,
    track_id: &str,
) -> gpui::Bounds<gpui::Pixels> {
    let x = beat_to_x(clip.start_beat as f64, viewport);
    let w =
        ((clip.duration_beats.max(0.0) as f64 * viewport.pixels_per_beat as f64) as f32).max(1.0);
    let row = layout
        .row_for_track(track_id)
        .map(|row| (row.y, row.height))
        .unwrap_or((0.0, layout.track_height));
    let y = row.0 - layout.scroll_y;
    gpui::bounds(
        gpui::point(gpui::px(x), gpui::px(y)),
        gpui::size(gpui::px(w), gpui::px(row.1)),
    )
}

impl TimelineState {
    pub fn time_to_content_x(&self, time_sec: f32) -> f32 {
        (time_sec * self.viewport.pixels_per_second - self.viewport.scroll_x).round()
    }

    pub fn content_x_to_time(&self, x: f32) -> f32 {
        ((x + self.viewport.scroll_x) / self.viewport.pixels_per_second).max(0.0)
    }

    pub fn beats_to_x(&self, beats: f32) -> f32 {
        beat_to_x(beats as f64, &self.viewport)
    }

    pub fn x_to_beats(&self, x: f32) -> f32 {
        x_to_beat(x, &self.viewport) as f32
    }

    pub fn beat_to_x(&self, beat: f32) -> f32 {
        self.beats_to_x(beat)
    }

    pub fn x_to_beat(&self, x: f32) -> f64 {
        x_to_beat(x, &self.viewport)
    }

    /// Window-space x of the arrangement lane origin — the left edge of the
    /// scrollable clip area, i.e. past the browser panel and the track headers.
    ///
    /// The browser panel is collapsible, so its width comes from the measured
    /// shell metrics rather than a constant. Every gesture that resolves a
    /// window-space pointer x (clip move, clip edge-resize, ruler scrub, lane
    /// tools, automation, tempo, song text) must map through this so pointer
    /// coordinates and drawing share one transform.
    pub fn lane_origin_x(&self) -> f32 {
        self.viewport.panel_origin_x + HEADER_WIDTH
    }

    /// Convert a window-space x into arrangement-lane content x.
    pub fn lane_x_from_window_x(&self, window_x: f32) -> f32 {
        window_x - self.lane_origin_x()
    }

    /// Convert a window-space x straight to timeline beats.
    pub fn beats_from_window_x(&self, window_x: f32) -> f32 {
        self.x_to_beats(self.lane_x_from_window_x(window_x))
    }

    pub fn arrangement_track_layout(&self) -> TrackLayout {
        TrackLayout::from_state(self)
    }

    pub fn snap_time(&self, seconds: f32) -> f32 {
        if !self.snap_to_grid || self.grid_division == SnapDivision::Off {
            return seconds;
        }
        let ppb = self.viewport.pixels_per_second * self.seconds_per_beat();
        let bpb = self.beats_per_bar();
        let sub_div = match self.grid_division {
            SnapDivision::Auto => self.get_grid_sub_beats(ppb),
            SnapDivision::Bar1 => bpb,
            _ => self.grid_division.step_beats(bpb),
        };
        if sub_div <= 0.0 {
            return seconds;
        }
        let spb = self.seconds_per_beat();
        let total_beats = seconds / spb;
        let snapped = (total_beats / sub_div).round() * sub_div;
        (snapped * spb).max(0.0)
    }

    /// Snap a beat value to the current grid (or return it unchanged when snap is off).
    pub fn snap_beats(&self, beats: f32) -> f32 {
        self.snap_beats_with_bypass(beats, false)
    }

    /// Snap a beat value, optionally bypassing the grid (Shift held during drag).
    pub fn snap_beats_with_bypass(&self, beats: f32, bypass: bool) -> f32 {
        let mut snap = SnapSettings::from_timeline(self);
        snap.beats_per_bar = self.beats_per_bar_at_beat(beats as f64);
        super::musical_snap::snap_beat(beats as f64, snap.to_musical(), bypass) as f32
    }
}
