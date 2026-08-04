//! Studio-side ownership of the Video track and the Video Player window.
//!
//! Two responsibilities, both control-rate — nothing here runs on a realtime or
//! render-hot path:
//!
//! * resolve *which frame* the reference video should be showing, from the
//!   playhead and the Video track's clips, and push that to the player window;
//! * import a video file onto the Video track.
//!
//! Decoding lives entirely in `sphere_video_player`, behind the player window.
//! This module never touches a decoder.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Bounds, Context};

use crate::components::timeline::timeline_state::{ClipState, ClipType, TrackType};
use crate::components::video_player_window::{open_video_player_window, VideoPlayerSnapshot};
use crate::layout::StudioLayout;

impl StudioLayout {
    /// The video clip covering `beat` on the Video track, if any.
    ///
    /// Returns the clip rather than a path so the caller can use its placement
    /// (`start_beat`/`offset_beats`) to convert a timeline position into a
    /// position inside the media.
    fn video_clip_at_beat(&self, beat: f32, cx: &gpui::App) -> Option<ClipState> {
        let timeline = self.timeline.read(cx);
        let track = timeline
            .state
            .tracks
            .iter()
            .find(|track| track.track_type == TrackType::Video)?;
        track
            .clips
            .iter()
            .find(|clip| {
                !clip.muted
                    && beat >= clip.start_beat
                    && beat < clip.start_beat + clip.duration_beats
            })
            .cloned()
    }

    /// Builds the snapshot the player window renders from.
    ///
    /// The media position is derived from the clip's placement, so trimming the
    /// clip's head or sliding it along the arrangement re-times the picture
    /// exactly the way it re-times an audio clip — there is no separate video
    /// offset to keep in sync.
    pub(crate) fn build_video_player_snapshot(&self, cx: &gpui::App) -> VideoPlayerSnapshot {
        let (playhead_beats, seconds_per_beat) = {
            let timeline = self.timeline.read(cx);
            (
                timeline.state.transport.playhead_beats,
                timeline.state.seconds_per_beat(),
            )
        };
        let timeline_seconds = (playhead_beats * seconds_per_beat) as f64;

        let Some(clip) = self.video_clip_at_beat(playhead_beats, cx) else {
            return VideoPlayerSnapshot {
                source_path: None,
                source_seconds: 0.0,
                timeline_seconds,
            };
        };

        let ClipType::Video {
            source_path: Some(source_path),
            ..
        } = &clip.clip_type
        else {
            return VideoPlayerSnapshot {
                source_path: None,
                source_seconds: 0.0,
                timeline_seconds,
            };
        };

        let beats_into_clip = (playhead_beats - clip.start_beat).max(0.0);
        let source_beats = beats_into_clip + clip.offset_beats.max(0.0);
        VideoPlayerSnapshot {
            source_path: Some(PathBuf::from(source_path)),
            source_seconds: (source_beats * seconds_per_beat) as f64,
            timeline_seconds,
        }
    }

    /// Pushes the current frame position to the player window. No-op when the
    /// window is closed, so the transport tick pays nothing for a feature that
    /// is not on screen.
    pub(crate) fn push_video_player_snapshot_to_window(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.external_windows.video_player.clone() else {
            return;
        };
        let snapshot = self.build_video_player_snapshot(cx);
        let _ = handle.update(cx, |player, _window, cx| {
            if player.set_snapshot(snapshot) {
                cx.notify();
            }
        });
    }

    /// Opens the Video Player, or focuses it if it is already open.
    pub(crate) fn open_video_player_window(
        &mut self,
        owner_bounds: Option<Bounds<gpui::Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.external_windows.video_player.clone() {
            if handle
                .update(cx, |_player, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            self.external_windows.video_player = None;
        }

        let snapshot = self.build_video_player_snapshot(cx);
        let studio = cx.entity().clone();
        let on_close: Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + Send + Sync> =
            Arc::new(move |_, app| {
                let _ = studio.update(app, |layout, cx| {
                    layout.external_windows.video_player = None;
                    cx.notify();
                });
            });

        match open_video_player_window(owner_bounds, snapshot, on_close, cx) {
            Ok(handle) => self.external_windows.video_player = Some(handle),
            Err(err) => eprintln!("[video-player] failed to open window: {err}"),
        }
    }
}
