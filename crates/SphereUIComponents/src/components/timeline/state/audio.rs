use super::*;

impl TimelineState {
    /// Drop a clip onto the timeline. `drop_x` and `drop_y` are in the track
    /// area coordinate system (header_width and ruler_height already stripped).
    /// Imports a clip with unknown metadata. The 2-bar duration is a temporary
    /// placeholder and must be replaced by DirectAudioEngine metadata.
    pub fn import_audio_at(
        &mut self,
        source_path: String,
        clip_name: String,
        drop_x: f32,
        drop_y: f32,
    ) -> String {
        eprintln!(
            "[import] drop path={} clip={} drop_x={:.1} drop_y={:.1}",
            source_path, clip_name, drop_x, drop_y
        );
        // Resolve target track: an existing lane under drop_y, otherwise create one.
        let track_id = match self.track_index_at_y(drop_y) {
            Some(idx) if matches!(self.tracks[idx].track_type, TrackType::Audio) => {
                self.tracks[idx].id.clone()
            }
            _ => self.create_audio_track(),
        };

        // Resolve start beat with snap.
        let raw_beats = self.x_to_beats(drop_x.max(0.0));
        let start_beat = self.snap_beats(raw_beats).max(0.0);

        self.insert_audio_clip(track_id, source_path, clip_name, start_beat)
    }

    pub fn import_audio_to_selected_or_new_track(
        &mut self,
        source_path: String,
        clip_name: String,
    ) -> String {
        let track_id = self
            .selected_audio_track_id()
            .unwrap_or_else(|| self.create_audio_track());
        eprintln!(
            "[import] browser path={} clip={} resolved_track_id={}",
            source_path, clip_name, track_id
        );
        let start_beat = self.snap_beats(self.x_to_beats(0.0)).max(0.0);
        self.insert_audio_clip(track_id, source_path, clip_name, start_beat)
    }

    pub(crate) fn insert_audio_clip_with_duration(
        &mut self,
        track_id: String,
        source_path: String,
        clip_name: String,
        start_beat: f32,
        duration_beats: f32,
        source_duration_seconds: Option<f64>,
    ) -> String {
        let track_id = if self.tracks.iter().any(|track| track.id == track_id) {
            track_id
        } else {
            eprintln!(
                "[recording] target track id={track_id} missing; creating fallback audio track"
            );
            self.create_audio_track()
        };

        let clip_id = self.next_clip_id();
        let new_clip = ClipState {
            id: clip_id.clone(),
            name: clip_name,
            start_beat: start_beat.max(0.0),
            duration_beats,
            source_duration_seconds,
            offset_beats: 0.0,
            gain: 1.0,
            clip_type: ClipType::Audio {
                file_id: source_path.clone(),
                source_path: Some(source_path),
            },
            muted: false,
            audio_import: AudioImportState::Pending,
            stretch: AudioClipStretchState::default(),
        };

        if let ClipType::Audio {
            source_path: Some(path),
            ..
        } = &new_clip.clip_type
        {
            eprintln!(
                "[Timeline] created audio clip clip_id={clip_id} source={path} start_beat={:.3} duration_beats={:.3}",
                new_clip.start_beat, new_clip.duration_beats
            );
        }

        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) {
            track.clips.push(new_clip);
        }
        self.selection.selected_track_id = Some(track_id);
        self.selection.selected_clip_ids = vec![clip_id.clone()];
        clip_id
    }

    fn insert_audio_clip(
        &mut self,
        track_id: String,
        source_path: String,
        clip_name: String,
        start_beat: f32,
    ) -> String {
        let track_id = if self.tracks.iter().any(|track| track.id == track_id) {
            track_id
        } else {
            eprintln!(
                "[import] target track id={} missing; creating fallback audio track",
                track_id
            );
            self.create_audio_track()
        };

        let duration_beats = 8.0;
        eprintln!(
            "[audio-import] WARNING using fallback duration because metadata is pending: path={} duration_beats=8.0",
            source_path
        );
        self.insert_audio_clip_with_duration(
            track_id,
            source_path,
            clip_name,
            start_beat,
            duration_beats,
            None,
        )
    }

    /// Apply decoded source metadata to every clip sharing `asset_key`
    /// (`ClipState::audio_asset_key`, i.e. the clip's `file_id`). Keyed on the
    /// asset id rather than the path so it still matches after a clip's
    /// `source_path` is rewritten (e.g. copy-into-project).
    pub fn update_audio_clip_metadata(
        &mut self,
        asset_key: &str,
        format: &str,
        sample_rate: u32,
        channels: u16,
        total_frames: u64,
        duration_seconds: f64,
    ) -> bool {
        if duration_seconds <= 0.0 {
            return false;
        }
        let duration_beats = self.seconds_to_beats(duration_seconds);
        let mut changed = false;
        let mut matched = false;
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.audio_asset_key() == Some(asset_key) {
                    matched = true;
                    clip.source_duration_seconds = Some(duration_seconds);
                    clip.stretch.original_sample_rate = sample_rate;
                    clip.stretch.project_sample_rate = sample_rate;
                    clip.stretch.original_duration_samples =
                        clip.stretch.original_duration_samples.max(total_frames);
                    if clip.stretch.source_end_samples <= clip.stretch.source_start_samples {
                        clip.stretch.source_start_samples = 0;
                        clip.stretch.source_end_samples = total_frames;
                    }
                    if (clip.duration_beats - duration_beats).abs() > 0.001 {
                        clip.duration_beats = duration_beats;
                        changed = true;
                    }
                }
            }
        }
        if matched {
            self.log_audio_meta(
                asset_key,
                format,
                sample_rate,
                channels,
                total_frames,
                duration_seconds,
            );
            self.log_audio_import(duration_beats);
        }
        changed
    }

    /// Retarget the stable asset id (`file_id`) for every clip sharing `old_key`.
    /// Used after copy-into-project so the cache key matches the saved project
    /// relative path. Returns `true` if any clip changed.
    pub fn retarget_audio_asset_id(&mut self, old_key: &str, new_key: &str) -> bool {
        if old_key == new_key {
            return false;
        }
        let mut changed = false;
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.audio_asset_key() != Some(old_key) {
                    continue;
                }
                if let ClipType::Audio { file_id, .. } = &mut clip.clip_type {
                    if file_id.as_str() != new_key {
                        *file_id = new_key.to_string();
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Point every clip sharing `asset_key` at a new resolvable `source_path`
    /// (e.g. after copying the source into the project folder). Returns `true`
    /// if any clip changed.
    pub fn retarget_audio_source(&mut self, asset_key: &str, new_source_path: &str) -> bool {
        let mut changed = false;
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.audio_asset_key() != Some(asset_key) {
                    continue;
                }
                if let ClipType::Audio { source_path, .. } = &mut clip.clip_type {
                    if source_path.as_deref() != Some(new_source_path) {
                        *source_path = Some(new_source_path.to_string());
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Set the import state on every clip sharing `asset_key` (the `file_id`).
    pub fn set_audio_import_for_asset(&mut self, asset_key: &str, state: AudioImportState) {
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.audio_asset_key() == Some(asset_key) {
                    clip.audio_import = state.clone();
                }
            }
        }
    }

    pub fn audio_source_duration_seconds(&self, asset_key: &str) -> Option<f64> {
        self.tracks.iter().find_map(|track| {
            track.clips.iter().find_map(|clip| {
                if clip.audio_asset_key() == Some(asset_key) {
                    return clip.source_duration_seconds;
                }
                None
            })
        })
    }

    fn log_audio_meta(
        &self,
        source_path: &str,
        format: &str,
        sample_rate: u32,
        channels: u16,
        total_frames: u64,
        duration_seconds: f64,
    ) {
        eprintln!("[audio-meta] path={}", source_path);
        eprintln!("[audio-meta] format={}", format);
        eprintln!("[audio-meta] sample_rate={}", sample_rate);
        eprintln!("[audio-meta] channels={}", channels);
        eprintln!("[audio-meta] total_frames={}", total_frames);
        eprintln!("[audio-meta] duration_seconds={:.6}", duration_seconds);
    }

    fn log_audio_import(&self, duration_beats: f32) {
        let bars_4_4 = duration_beats / 4.0;
        eprintln!("[audio-import] bpm={:.3}", self.bpm);
        eprintln!("[audio-import] duration_beats={:.6}", duration_beats);
        eprintln!("[audio-import] bars_4_4={:.6}", bars_4_4);
    }
}
