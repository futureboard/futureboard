use super::*;

/// One recorded pass on a track.
///
/// A take is not a second copy of the audio — it *is* an arrangement clip, plus
/// the record of which pass produced it. Keeping the clip as the storage is
/// what makes an inactive take editable, movable and exportable like anything
/// else the moment it is made active again, instead of a special object that
/// only the take list understands.
///
/// Takes that overlap each other are alternates of the same performance: making
/// one active mutes the others it collides with, which is the whole of comping
/// at this level. Takes that do not overlap are simply different parts of the
/// track and all stay active.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackTake {
    pub id: String,
    /// Shown in the take list. Defaults to `Take N`, renameable.
    pub name: String,
    /// The arrangement clip this take produced. A take whose clip has been
    /// deleted is pruned — see [`TimelineState::prune_orphaned_takes`].
    pub clip_id: String,
    /// Whether this take is the one heard. Exactly one of a set of overlapping
    /// takes is active at a time.
    pub active: bool,
    /// Local timestamp of the pass, for the take row's secondary line. Stored
    /// as text because it is a label, never a value anything computes with.
    pub recorded_at: String,
}

impl TrackTake {
    pub fn new(id: impl Into<String>, name: impl Into<String>, clip_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            clip_id: clip_id.into(),
            active: true,
            recorded_at: String::new(),
        }
    }
}

impl TrackState {
    /// Take rows the header should draw, newest first — the order a player
    /// looks for the pass they just did.
    pub fn takes_newest_first(&self) -> impl Iterator<Item = &TrackTake> {
        self.takes.iter().rev()
    }

    pub fn take(&self, take_id: &str) -> Option<&TrackTake> {
        self.takes.iter().find(|take| take.id == take_id)
    }

    /// How many takes overlap the busiest point on this track — the number the
    /// header's "Takes" badge shows. `1` means every take is its own region and
    /// nothing is being comped.
    pub fn take_stack_depth(&self) -> usize {
        let mut depth = 0usize;
        for take in &self.takes {
            let Some(bounds) = self.take_bounds(take) else {
                continue;
            };
            let overlapping = self
                .takes
                .iter()
                .filter(|other| {
                    self.take_bounds(other)
                        .is_some_and(|other_bounds| ranges_overlap(bounds, other_bounds))
                })
                .count();
            depth = depth.max(overlapping);
        }
        depth
    }

    fn take_bounds(&self, take: &TrackTake) -> Option<(f32, f32)> {
        let clip = self.clips.iter().find(|clip| clip.id == take.clip_id)?;
        Some((
            clip.start_beat,
            clip.start_beat + clip.duration_beats.max(0.0),
        ))
    }
}

/// Whether two half-open beat ranges share any time at all.
fn ranges_overlap(a: (f32, f32), b: (f32, f32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

impl TimelineState {
    /// Register the clip a recording pass just produced as a take.
    ///
    /// The new take is the active one, and every take it overlaps becomes an
    /// alternate — which is what a second pass over the same bars means. A pass
    /// somewhere else on the track collides with nothing and leaves the rest
    /// alone.
    ///
    /// Returns the new take's id, or `None` when the track or clip is gone.
    pub fn register_recorded_take(
        &mut self,
        track_id: &str,
        clip_id: &str,
        recorded_at: String,
    ) -> Option<String> {
        let take_id = self.next_take_id(track_id);
        let track = self.tracks.iter_mut().find(|track| track.id == track_id)?;
        if !track.clips.iter().any(|clip| clip.id == clip_id) {
            return None;
        }
        let name = format!("Take {}", track.takes.len() + 1);
        track.takes.push(TrackTake {
            id: take_id.clone(),
            name,
            clip_id: clip_id.to_string(),
            active: true,
            recorded_at,
        });
        // A track that has never been comped shows no take lane; the second
        // overlapping pass is what makes one worth opening.
        if track.take_stack_depth() > 1 {
            track.takes_expanded = true;
        }
        self.set_active_take(track_id, &take_id);
        Some(take_id)
    }

    /// Make `take_id` the take that is heard, muting every take it overlaps and
    /// unmuting itself.
    ///
    /// Mute is the mechanism because it is the one the engine, the export and
    /// the mixer already agree on: an inactive take is a muted clip, not a clip
    /// in a parallel universe the renderer has to learn about.
    ///
    /// Returns `true` when anything changed.
    pub fn set_active_take(&mut self, track_id: &str, take_id: &str) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        let Some(target) = track.take(take_id).cloned() else {
            return false;
        };
        let Some(target_bounds) = track
            .clips
            .iter()
            .find(|clip| clip.id == target.clip_id)
            .map(|clip| {
                (
                    clip.start_beat,
                    clip.start_beat + clip.duration_beats.max(0.0),
                )
            })
        else {
            return false;
        };

        // Resolve the collision set first: the mutable walk below cannot also
        // be reading clip bounds off the same track.
        let colliding: Vec<(String, String)> = track
            .takes
            .iter()
            .filter(|take| take.id != target.id)
            .filter_map(|take| {
                let clip = track.clips.iter().find(|clip| clip.id == take.clip_id)?;
                let bounds = (
                    clip.start_beat,
                    clip.start_beat + clip.duration_beats.max(0.0),
                );
                ranges_overlap(target_bounds, bounds)
                    .then(|| (take.id.clone(), take.clip_id.clone()))
            })
            .collect();

        let mut changed = false;
        for take in &mut track.takes {
            let should_be_active =
                take.id == target.id || !colliding.iter().any(|(id, _)| *id == take.id);
            if take.active != should_be_active {
                take.active = should_be_active;
                changed = true;
            }
        }
        let muted_clips: Vec<String> = colliding.into_iter().map(|(_, clip)| clip).collect();
        for clip in &mut track.clips {
            let should_be_muted = muted_clips.iter().any(|id| id == &clip.id);
            // Only clips this take list owns: a clip that is not a take keeps
            // whatever mute the user gave it.
            let is_take_clip = track.takes.iter().any(|take| take.clip_id == clip.id);
            if !is_take_clip {
                continue;
            }
            if clip.muted != should_be_muted {
                clip.muted = should_be_muted;
                changed = true;
            }
        }
        changed
    }

    /// Delete a take *and the clip it holds*. There is nothing else it is.
    pub fn delete_take(&mut self, track_id: &str, take_id: &str) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        let Some(index) = track.takes.iter().position(|take| take.id == take_id) else {
            return false;
        };
        let removed = track.takes.remove(index);
        track.clips.retain(|clip| clip.id != removed.clip_id);
        if track.takes.is_empty() {
            track.takes_expanded = false;
        }
        // Deleting the take that was heard leaves its slot silent, so promote
        // the most recent surviving take that covered the same ground.
        if removed.active {
            if let Some(next) = track.takes.last().map(|take| take.id.clone()) {
                self.set_active_take(track_id, &next);
            }
        }
        true
    }

    pub fn rename_take(&mut self, track_id: &str, take_id: &str, name: String) -> bool {
        let name = name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        let Some(take) = track.takes.iter_mut().find(|take| take.id == take_id) else {
            return false;
        };
        if take.name == name {
            return false;
        }
        take.name = name;
        true
    }

    pub fn toggle_takes_expanded(&mut self, track_id: &str) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        track.takes_expanded = !track.takes_expanded;
        true
    }

    /// Drop takes whose clip no longer exists.
    ///
    /// A take is a pointer to a clip, and a clip can be deleted from the
    /// arrangement like any other. Called after edits that remove clips, so the
    /// take list never offers a row that would do nothing.
    pub fn prune_orphaned_takes(&mut self) -> bool {
        let mut changed = false;
        for track in &mut self.tracks {
            let before = track.takes.len();
            track
                .takes
                .retain(|take| track.clips.iter().any(|clip| clip.id == take.clip_id));
            if track.takes.len() != before {
                changed = true;
            }
            if track.takes.is_empty() && track.takes_expanded {
                track.takes_expanded = false;
            }
        }
        changed
    }

    /// An id no take on `track_id` is using.
    fn next_take_id(&self, track_id: &str) -> String {
        let used: Vec<&str> = self
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.takes.iter().map(|take| take.id.as_str()).collect())
            .unwrap_or_default();
        let mut n = used.len() as u32 + 1;
        loop {
            let candidate = format!("{track_id}-take-{n}");
            if !used.iter().any(|id| *id == candidate) {
                return candidate;
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with_clips(bounds: &[(f32, f32)]) -> TimelineState {
        let mut state = TimelineState::default();
        state.tracks.clear();
        let track_id = state.create_audio_track();
        let track = state
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .expect("track");
        for (index, (start, len)) in bounds.iter().enumerate() {
            track.clips.push(ClipState {
                id: format!("clip-{index}"),
                name: format!("Clip {index}"),
                start_beat: *start,
                duration_beats: *len,
                source_duration_seconds: None,
                offset_beats: 0.0,
                gain: 1.0,
                clip_type: ClipType::Audio {
                    file_id: String::new(),
                    source_path: None,
                },
                muted: false,
                audio_import: AudioImportState::Ready,
                stretch: AudioClipStretchState::default(),
            });
        }
        state
    }

    fn track_id(state: &TimelineState) -> String {
        state.tracks[0].id.clone()
    }

    /// The second pass over the same bars is an alternate, not a layer. Playing
    /// both at once is the failure this exists to prevent.
    #[test]
    fn a_second_overlapping_take_mutes_the_first() {
        let mut state = track_with_clips(&[(0.0, 8.0), (0.0, 8.0)]);
        let id = track_id(&state);
        let first = state
            .register_recorded_take(&id, "clip-0", String::new())
            .expect("first take");
        let second = state
            .register_recorded_take(&id, "clip-1", String::new())
            .expect("second take");

        let track = &state.tracks[0];
        assert!(!track.take(&first).unwrap().active);
        assert!(track.take(&second).unwrap().active);
        assert!(track.clips[0].muted, "the earlier take is still audible");
        assert!(!track.clips[1].muted);
    }

    /// Two passes on different parts of the track are not alternates of each
    /// other, and muting one because the other arrived would silence a part of
    /// the arrangement nobody asked to replace.
    #[test]
    fn takes_that_do_not_overlap_all_stay_active() {
        let mut state = track_with_clips(&[(0.0, 4.0), (8.0, 4.0)]);
        let id = track_id(&state);
        let first = state
            .register_recorded_take(&id, "clip-0", String::new())
            .unwrap();
        let second = state
            .register_recorded_take(&id, "clip-1", String::new())
            .unwrap();

        let track = &state.tracks[0];
        assert!(track.take(&first).unwrap().active);
        assert!(track.take(&second).unwrap().active);
        assert!(!track.clips[0].muted);
        assert!(!track.clips[1].muted);
        assert_eq!(track.take_stack_depth(), 1, "nothing is being comped");
    }

    #[test]
    fn choosing_an_earlier_take_puts_it_back_and_mutes_the_later_one() {
        let mut state = track_with_clips(&[(0.0, 8.0), (0.0, 8.0)]);
        let id = track_id(&state);
        let first = state
            .register_recorded_take(&id, "clip-0", String::new())
            .unwrap();
        let second = state
            .register_recorded_take(&id, "clip-1", String::new())
            .unwrap();

        assert!(state.set_active_take(&id, &first));
        let track = &state.tracks[0];
        assert!(track.take(&first).unwrap().active);
        assert!(!track.take(&second).unwrap().active);
        assert!(!track.clips[0].muted);
        assert!(track.clips[1].muted);
    }

    /// A take *is* its clip, so deleting one deletes the audio from the
    /// arrangement — and the slot it leaves must not stay silent.
    #[test]
    fn deleting_the_active_take_removes_its_clip_and_promotes_another() {
        let mut state = track_with_clips(&[(0.0, 8.0), (0.0, 8.0)]);
        let id = track_id(&state);
        let first = state
            .register_recorded_take(&id, "clip-0", String::new())
            .unwrap();
        let second = state
            .register_recorded_take(&id, "clip-1", String::new())
            .unwrap();

        assert!(state.delete_take(&id, &second));
        let track = &state.tracks[0];
        assert_eq!(track.takes.len(), 1);
        assert_eq!(track.clips.len(), 1);
        assert!(track.take(&first).unwrap().active);
        assert!(!track.clips[0].muted, "the surviving take is still muted");
    }

    /// A clip deleted from the arrangement takes its take row with it — a row
    /// that points at nothing would be a control that does nothing.
    #[test]
    fn a_take_whose_clip_was_deleted_is_pruned() {
        let mut state = track_with_clips(&[(0.0, 8.0)]);
        let id = track_id(&state);
        state
            .register_recorded_take(&id, "clip-0", String::new())
            .unwrap();
        state.tracks[0].clips.clear();
        assert!(state.prune_orphaned_takes());
        assert!(state.tracks[0].takes.is_empty());
        assert!(!state.tracks[0].takes_expanded);
    }

    /// The lane opens itself the moment there is something to choose between,
    /// and not before — one take is not a comp.
    #[test]
    fn the_take_lane_opens_on_the_first_real_alternative() {
        let mut state = track_with_clips(&[(0.0, 8.0), (0.0, 8.0)]);
        let id = track_id(&state);
        state
            .register_recorded_take(&id, "clip-0", String::new())
            .unwrap();
        assert!(!state.tracks[0].takes_expanded);
        state
            .register_recorded_take(&id, "clip-1", String::new())
            .unwrap();
        assert!(state.tracks[0].takes_expanded);
        assert_eq!(state.tracks[0].take_stack_depth(), 2);
    }

    #[test]
    fn take_ids_are_unique_per_track() {
        let mut state = track_with_clips(&[(0.0, 4.0), (4.0, 4.0)]);
        let id = track_id(&state);
        let a = state
            .register_recorded_take(&id, "clip-0", String::new())
            .unwrap();
        let b = state
            .register_recorded_take(&id, "clip-1", String::new())
            .unwrap();
        assert_ne!(a, b);
    }
}
