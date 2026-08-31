//! Video track and reference-video clip state.
//!
//! A project holds at most one Video track (see [`TrackType::is_singleton`]).
//! It carries no audio, so nothing here touches routing, the mixer, or the
//! engine graph — importing a video only places a clip.
//!
//! Clip length is the honest unknown at import time: the container's duration
//! is only known once the decoder has opened the file, which happens off this
//! thread in the Video Player. A freshly imported clip therefore gets a
//! placeholder length that [`TimelineState::set_video_clip_duration_seconds`]
//! replaces as soon as a real duration is available.

use super::*;

/// Placeholder clip length used until the decoder reports a real duration.
/// Four bars at the project's meter reads as "a clip", not as a glitch.
const VIDEO_PLACEHOLDER_BEATS: f32 = 16.0;

impl TimelineState {
    /// The project's Video track id, if it has one.
    pub fn video_track_id(&self) -> Option<String> {
        self.tracks
            .iter()
            .find(|track| track.track_type == TrackType::Video)
            .map(|track| track.id.clone())
    }

    /// Returns the existing Video track, creating it if the project has none.
    /// Never creates a second one — the type is a singleton.
    pub fn ensure_video_track(&mut self) -> String {
        if let Some(id) = self.video_track_id() {
            return id;
        }
        self.create_track(CreateTrackOptions {
            track_type: TrackType::Video,
            name: "Video".to_string(),
            color: crate::theme::Colors::accent_purple(),
            volume: volume::db_to_norm(0.0),
            pan: 0.0,
            armed: false,
            input_monitor: InputMonitorMode::Off,
        })
    }

    /// Places a reference video on the Video track at `start_beat`, creating the
    /// track if needed. Returns the new clip's id.
    pub fn insert_video_clip(
        &mut self,
        source_path: String,
        clip_name: String,
        start_beat: f32,
    ) -> String {
        let track_id = self.ensure_video_track();
        let clip_id = self.next_clip_id();
        let clip = ClipState {
            id: clip_id.clone(),
            name: clip_name,
            start_beat: start_beat.max(0.0),
            duration_beats: VIDEO_PLACEHOLDER_BEATS,
            source_duration_seconds: None,
            offset_beats: 0.0,
            gain: 1.0,
            clip_type: ClipType::Video {
                file_id: source_path.clone(),
                source_path: Some(source_path),
            },
            muted: false,
            audio_import: AudioImportState::Ready,
            stretch: AudioClipStretchState::default(),
            ara: None,
        };

        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) {
            track.clips.push(clip);
        }
        clip_id
    }

    /// Imports a video and snaps it to the timeline position under `drop_x`.
    /// The drop's vertical position is ignored on purpose: a reference video
    /// belongs on the Video track wherever it was dropped, and dropping one onto
    /// an audio lane should not silently create an audio clip from it.
    pub fn import_video_at(
        &mut self,
        source_path: String,
        clip_name: String,
        drop_x: f32,
    ) -> String {
        let start_beat = self.snap_beats(self.x_to_beats(drop_x.max(0.0))).max(0.0);
        self.insert_video_clip(source_path, clip_name, start_beat)
    }

    /// Replaces a video clip's placeholder length with the real media duration
    /// once the decoder has reported one. Returns `true` when the clip changed.
    ///
    /// Only grows/shrinks a clip the user has not trimmed themselves — a clip
    /// whose `source_duration_seconds` is already set has a known length and is
    /// left alone, so a later re-probe cannot undo a trim.
    pub fn set_video_clip_duration_seconds(&mut self, clip_id: &str, seconds: f64) -> bool {
        if !(seconds.is_finite() && seconds > 0.0) {
            return false;
        }
        let seconds_per_beat = self.seconds_per_beat().max(f32::EPSILON);
        for track in &mut self.tracks {
            let Some(clip) = track.clips.iter_mut().find(|clip| clip.id == clip_id) else {
                continue;
            };
            if !matches!(clip.clip_type, ClipType::Video { .. })
                || clip.source_duration_seconds.is_some()
            {
                return false;
            }
            let duration_beats = (seconds as f32 / seconds_per_beat).max(0.25);
            clip.source_duration_seconds = Some(seconds);
            clip.duration_beats = duration_beats;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_video(path: &str) -> (TimelineState, String) {
        let mut state = TimelineState::default();
        let clip_id = state.insert_video_clip(path.to_string(), "Reference".to_string(), 0.0);
        (state, clip_id)
    }

    #[test]
    fn importing_twice_reuses_the_single_video_track() {
        let (mut state, _) = state_with_video("C:/clips/a.mp4");
        state.insert_video_clip("C:/clips/b.mp4".to_string(), "B".to_string(), 8.0);

        let video_tracks = state
            .tracks
            .iter()
            .filter(|track| track.track_type == TrackType::Video)
            .count();
        assert_eq!(video_tracks, 1);
        assert_eq!(state.tracks[0].clips.len(), 2);
    }

    #[test]
    fn ensure_video_track_is_idempotent() {
        let mut state = TimelineState::default();
        let first = state.ensure_video_track();
        let second = state.ensure_video_track();
        assert_eq!(first, second);
        assert_eq!(state.tracks.len(), 1);
    }

    #[test]
    fn real_duration_replaces_the_placeholder_length() {
        let (mut state, clip_id) = state_with_video("C:/clips/a.mp4");
        assert_eq!(
            state.tracks[0].clips[0].duration_beats,
            VIDEO_PLACEHOLDER_BEATS
        );

        // 120 bpm default → 0.5 s per beat, so 10 s is 20 beats.
        assert!(state.set_video_clip_duration_seconds(&clip_id, 10.0));
        assert!((state.tracks[0].clips[0].duration_beats - 20.0).abs() < 1.0e-3);
    }

    #[test]
    fn a_clip_with_a_known_duration_is_not_re_lengthened() {
        let (mut state, clip_id) = state_with_video("C:/clips/a.mp4");
        assert!(state.set_video_clip_duration_seconds(&clip_id, 10.0));
        // A second report (e.g. reopening the player) must not fight a trim.
        assert!(!state.set_video_clip_duration_seconds(&clip_id, 30.0));
        assert!((state.tracks[0].clips[0].duration_beats - 20.0).abs() < 1.0e-3);
    }

    #[test]
    fn nonsense_durations_are_ignored() {
        let (mut state, clip_id) = state_with_video("C:/clips/a.mp4");
        assert!(!state.set_video_clip_duration_seconds(&clip_id, 0.0));
        assert!(!state.set_video_clip_duration_seconds(&clip_id, f64::NAN));
        assert!(!state.set_video_clip_duration_seconds(&clip_id, -3.0));
        assert_eq!(
            state.tracks[0].clips[0].duration_beats,
            VIDEO_PLACEHOLDER_BEATS
        );
    }
}
