use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineSelection {
    pub selected_track_id: Option<String>,
    /// Ordered multi-track selection. `selected_track_id` remains the primary
    /// track used by the Inspector and single-target commands.
    pub selected_track_ids: Vec<String>,
    /// Stable anchor used by Shift-click range selection.
    pub track_selection_anchor_id: Option<String>,
    pub selected_clip_ids: Vec<String>,
    /// Shared Song Text selection used by the ruler and all panel/window views.
    pub selected_song_text_event_ids: Vec<SongTextEventId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRangeSelection {
    pub start_beat: f64,
    pub end_beat: f64,
    pub track_ids: Vec<String>,
}

impl TimelineRangeSelection {
    pub fn new(start_beat: f64, end_beat: f64, track_ids: Vec<String>) -> Self {
        let (start_beat, end_beat) = if start_beat <= end_beat {
            (start_beat, end_beat)
        } else {
            (end_beat, start_beat)
        };
        Self {
            start_beat,
            end_beat,
            track_ids,
        }
    }

    pub fn as_f32_range(&self) -> (f32, f32) {
        (self.start_beat as f32, self.end_beat as f32)
    }
}

impl TimelineState {
    pub fn selected_range_track_ids(&self) -> Vec<String> {
        match self.selection.selected_track_id.as_ref() {
            Some(primary)
                if self.selection.selected_track_ids.is_empty()
                    || !self.selection.selected_track_ids.contains(primary) =>
            {
                vec![primary.clone()]
            }
            Some(_) => self.selection.selected_track_ids.clone(),
            None => Vec::new(),
        }
    }

    pub fn track_ids_between(&self, a: &str, b: &str) -> Vec<String> {
        let Some(a_index) = self.tracks.iter().position(|track| track.id == a) else {
            return Vec::new();
        };
        let Some(b_index) = self.tracks.iter().position(|track| track.id == b) else {
            return vec![a.to_string()];
        };
        let (lo, hi) = if a_index <= b_index {
            (a_index, b_index)
        } else {
            (b_index, a_index)
        };
        self.tracks[lo..=hi]
            .iter()
            .map(|track| track.id.clone())
            .collect()
    }

    /// Commit what a marquee enclosed: the tracks it crossed and the clips it
    /// touched.
    ///
    /// Tracks are part of the result, not a side effect of the clips. A band
    /// pulled across five tracks that leaves four unselected is the gesture
    /// failing, and a band pulled across empty lanes has no clips to hit at all
    /// yet still says exactly which tracks the user meant.
    ///
    /// `anchor` is the track the drag started on. It becomes the primary and
    /// the anchor for a following Shift-click, rather than whichever track
    /// happens to be topmost — extending a selection should continue from where
    /// the cursor actually went down.
    ///
    /// Additive keeps what was already selected and adds to it; replace does
    /// what its name says.
    pub fn apply_marquee_selection(
        &mut self,
        track_ids: &[String],
        clip_ids: Vec<String>,
        anchor: &str,
        additive: bool,
    ) {
        if additive {
            for clip_id in clip_ids {
                if !self.selection.selected_clip_ids.contains(&clip_id) {
                    self.selection.selected_clip_ids.push(clip_id);
                }
            }
            for track_id in track_ids {
                if !self.selection.selected_track_ids.contains(track_id) {
                    self.selection.selected_track_ids.push(track_id.clone());
                }
            }
            if self.selection.selected_track_id.is_none() {
                self.selection.selected_track_id = Some(anchor.to_string());
            }
        } else {
            self.selection.selected_clip_ids = clip_ids;
            self.selection.selected_track_ids = track_ids.to_vec();
            self.selection.selected_track_id = track_ids
                .iter()
                .find(|id| id.as_str() == anchor)
                .cloned()
                .or_else(|| track_ids.first().cloned());
        }
        if !track_ids.is_empty() {
            self.selection.track_selection_anchor_id = Some(anchor.to_string());
        }
    }

    pub fn select_track(&mut self, track_id: &str) {
        self.selection.selected_track_id = Some(track_id.to_string());
        self.selection.selected_track_ids = vec![track_id.to_string()];
        self.selection.track_selection_anchor_id = Some(track_id.to_string());
        self.selection.selected_clip_ids.clear();
        self.selection.selected_song_text_event_ids.clear();
        self.arrangement_range = None;
    }

    pub fn select_track_with_modifiers(&mut self, track_id: &str, additive: bool, range: bool) {
        match self.selection.selected_track_id.clone() {
            Some(primary) if !self.selection.selected_track_ids.contains(&primary) => {
                self.selection.selected_track_ids = vec![primary];
            }
            None => self.selection.selected_track_ids.clear(),
            _ => {}
        }
        if range {
            let anchor = self
                .selection
                .track_selection_anchor_id
                .as_deref()
                .or(self.selection.selected_track_id.as_deref())
                .unwrap_or(track_id)
                .to_string();
            let range_ids = self.track_ids_between(&anchor, track_id);
            if additive {
                for id in range_ids {
                    if !self.selection.selected_track_ids.contains(&id) {
                        self.selection.selected_track_ids.push(id);
                    }
                }
            } else {
                self.selection.selected_track_ids = range_ids;
            }
            self.selection.selected_track_id = Some(track_id.to_string());
            self.selection.track_selection_anchor_id = Some(anchor);
        } else if additive {
            if let Some(index) = self
                .selection
                .selected_track_ids
                .iter()
                .position(|id| id == track_id)
            {
                self.selection.selected_track_ids.remove(index);
                if self.selection.selected_track_id.as_deref() == Some(track_id) {
                    self.selection.selected_track_id =
                        self.selection.selected_track_ids.last().cloned();
                }
            } else {
                self.selection.selected_track_ids.push(track_id.to_string());
                self.selection.selected_track_id = Some(track_id.to_string());
            }
            self.selection.track_selection_anchor_id = self.selection.selected_track_id.clone();
        } else {
            self.selection.selected_track_id = Some(track_id.to_string());
            self.selection.selected_track_ids = vec![track_id.to_string()];
            self.selection.track_selection_anchor_id = Some(track_id.to_string());
        }
        self.selection.selected_clip_ids.clear();
        self.selection.selected_song_text_event_ids.clear();
        self.arrangement_range = None;
    }

    pub fn is_track_selected(&self, track_id: &str) -> bool {
        match self.selection.selected_track_id.as_ref() {
            Some(primary)
                if self.selection.selected_track_ids.is_empty()
                    || !self.selection.selected_track_ids.contains(primary) =>
            {
                primary == track_id
            }
            Some(_) => self
                .selection
                .selected_track_ids
                .iter()
                .any(|id| id == track_id),
            None => false,
        }
    }

    pub fn select_clip(&mut self, clip_id: &str) {
        self.selection.selected_clip_ids = vec![clip_id.to_string()];
        self.selection.selected_song_text_event_ids.clear();
        self.arrangement_range = None;
        if let Some(track) = self
            .tracks
            .iter()
            .find(|t| t.clips.iter().any(|c| c.id == clip_id))
        {
            self.selection.selected_track_id = Some(track.id.clone());
            self.selection.selected_track_ids = vec![track.id.clone()];
            self.selection.track_selection_anchor_id = Some(track.id.clone());
        }
    }

    pub fn select_clip_additive(&mut self, clip_id: &str) {
        self.selection.selected_song_text_event_ids.clear();
        self.arrangement_range = None;
        if let Some(pos) = self
            .selection
            .selected_clip_ids
            .iter()
            .position(|id| id == clip_id)
        {
            self.selection.selected_clip_ids.remove(pos);
        } else {
            self.selection.selected_clip_ids.push(clip_id.to_string());
        }
        if let Some(track) = self
            .tracks
            .iter()
            .find(|t| t.clips.iter().any(|c| c.id == clip_id))
        {
            self.selection.selected_track_id = Some(track.id.clone());
            self.selection.selected_track_ids = vec![track.id.clone()];
            self.selection.track_selection_anchor_id = Some(track.id.clone());
        }
    }

    pub fn select_song_text_event(&mut self, id: &str, additive: bool) {
        self.selection.selected_clip_ids.clear();
        self.arrangement_range = None;
        if additive {
            if let Some(index) = self
                .selection
                .selected_song_text_event_ids
                .iter()
                .position(|selected| selected == id)
            {
                self.selection.selected_song_text_event_ids.remove(index);
            } else {
                self.selection
                    .selected_song_text_event_ids
                    .push(id.to_string());
            }
        } else {
            self.selection.selected_song_text_event_ids = vec![id.to_string()];
        }
    }

    pub fn selected_song_text_event(&self) -> Option<&SongTextEvent> {
        let id = self.selection.selected_song_text_event_ids.first()?;
        self.song_text_event(id)
    }

    pub fn clear_song_text_selection(&mut self) {
        self.selection.selected_song_text_event_ids.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this covers: a marquee pulled across several tracks selected the
    /// clips it touched but left the *tracks* alone — only the topmost one came
    /// out selected — so "select across tracks" did not work, and a band across
    /// empty lanes selected nothing at all.
    #[test]
    fn a_marquee_selects_every_track_it_crossed() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let first = state.create_audio_track();
        let second = state.create_audio_track();
        let third = state.create_audio_track();
        let spanned = vec![first.clone(), second.clone(), third.clone()];

        // Dragged from the middle track outwards, with no clips under the band.
        state.apply_marquee_selection(&spanned, Vec::new(), &second, false);

        assert_eq!(state.selection.selected_track_ids, spanned);
        assert!(
            state.selection.selected_clip_ids.is_empty(),
            "no clips were under the band"
        );
        // The primary and the anchor stay where the drag started, so a Shift
        // click afterwards extends from the cursor rather than from the top.
        assert_eq!(
            state.selection.selected_track_id.as_deref(),
            Some(second.as_str())
        );
        assert_eq!(
            state.selection.track_selection_anchor_id.as_deref(),
            Some(second.as_str())
        );
    }

    #[test]
    fn a_replacing_marquee_drops_what_was_selected_before() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let first = state.create_audio_track();
        let second = state.create_audio_track();

        state.select_track(&first);
        state.selection.selected_clip_ids = vec!["stale-clip".to_string()];

        state.apply_marquee_selection(
            &[second.clone()],
            vec!["fresh-clip".to_string()],
            &second,
            false,
        );

        assert_eq!(state.selection.selected_track_ids, vec![second.clone()]);
        assert_eq!(
            state.selection.selected_clip_ids,
            vec!["fresh-clip".to_string()]
        );
    }

    /// Ctrl-drag adds to what is already selected — tracks included — and never
    /// lists the same track twice however many times it is crossed.
    #[test]
    fn an_additive_marquee_unions_tracks_without_duplicating_them() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let first = state.create_audio_track();
        let second = state.create_audio_track();
        let third = state.create_audio_track();

        state.select_track(&first);
        state.apply_marquee_selection(
            &[second.clone(), third.clone()],
            vec!["clip-b".to_string()],
            &second,
            true,
        );
        state.apply_marquee_selection(
            &[first.clone(), second.clone()],
            vec!["clip-b".to_string(), "clip-a".to_string()],
            &first,
            true,
        );

        assert_eq!(
            state.selection.selected_track_ids,
            vec![first.clone(), second.clone(), third.clone()]
        );
        assert_eq!(
            state.selection.selected_clip_ids,
            vec!["clip-b".to_string(), "clip-a".to_string()],
            "a clip already selected must not be listed twice"
        );
        // Additive keeps the primary it had.
        assert_eq!(
            state.selection.selected_track_id.as_deref(),
            Some(first.as_str())
        );
    }

    /// An anchor outside the band (the drag left the arrangement) still has to
    /// produce a primary, or the selection has tracks and no focus.
    #[test]
    fn a_marquee_whose_anchor_is_not_in_the_span_still_has_a_primary() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let first = state.create_audio_track();
        let second = state.create_audio_track();

        state.apply_marquee_selection(&[second.clone()], Vec::new(), &first, false);

        assert_eq!(
            state.selection.selected_track_id.as_deref(),
            Some(second.as_str())
        );
    }

    #[test]
    fn ctrl_toggles_tracks_and_shift_selects_anchor_range() {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let first = state.create_audio_track();
        let second = state.create_audio_track();
        let third = state.create_audio_track();

        state.select_track(&first);
        state.select_track_with_modifiers(&third, true, false);
        assert_eq!(
            state.selection.selected_track_ids,
            vec![first.clone(), third.clone()]
        );

        state.select_track_with_modifiers(&first, true, false);
        assert_eq!(state.selection.selected_track_ids, vec![third.clone()]);

        state.select_track(&first);
        state.select_track_with_modifiers(&third, false, true);
        assert_eq!(
            state.selection.selected_track_ids,
            vec![first, second, third]
        );
    }
}
