use gpui::Context;

use std::path::{Path, PathBuf};

use crate::components::file_browser::read_directory;

use super::StudioLayout;
impl StudioLayout {
    /// Ask the engine to audition (preview-play) a browser audio file.
    ///
    /// The Browser keeps audition controls hidden until this path can produce
    /// audible output. Returns whether audio is actually audible.
    pub(crate) fn audition_browser_file(&mut self, path: &Path) -> bool {
        let Some(engine) = self.audio_bridge.engine.as_ref() else {
            eprintln!("[browser-preview] no engine; cannot audition");
            return false;
        };
        match engine.audition_file(path.to_string_lossy().into_owned()) {
            Ok(audible) => {
                if audible {
                    self.file_browser.set_preview_playing(path.to_path_buf());
                }
                audible
            }
            Err(error) => {
                eprintln!("[browser-preview] audition error: {error}");
                false
            }
        }
    }

    /// Pull the Browser preview playhead published by the render callback into
    /// the browser state. Returns whether the preview pane needs a repaint.
    ///
    /// One relaxed atomic read per UI poll — the engine owns the position, so
    /// the pane never runs its own clock and can never drift from the audio.
    pub(crate) fn poll_browser_preview_playhead(&mut self) -> bool {
        let position = self
            .audio_bridge
            .engine
            .as_ref()
            .and_then(|engine| engine.audition_position_seconds())
            .map(|seconds| seconds as f32);
        self.file_browser.apply_preview_position(position)
    }

    /// Ensure the mini waveform peaks for `path` are decoded for the preview
    /// pane. Decode runs on the background executor (never in render); the
    /// result lands in the shared waveform cache the sidebar reads from. Cached
    /// or already-in-flight files are skipped.
    pub(crate) fn ensure_browser_waveform(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        use crate::components::timeline::waveform_cache;
        let key = path.to_string_lossy().to_string();
        if waveform_cache::get_preview_arc(&key).is_some() {
            return; // already decoded
        }
        if !self.file_browser.begin_waveform_load(path.clone()) {
            return; // decode already running
        }
        let decode_path = path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { waveform_cache::decode_and_cache_file(&decode_path).is_some() })
                .await;
            let _ = this.update(cx, move |this, cx| {
                this.file_browser.end_waveform_load(&path);
                if !result {
                    eprintln!(
                        "[browser-preview] waveform decode failed path={}",
                        path.display()
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Run a single-level directory scan on the GPUI background executor,
    /// then push the result back into `file_browser.index` on the UI
    /// thread. Never blocks render — this is the only place `read_dir`
    /// is allowed to happen at runtime.
    pub(super) fn spawn_directory_load(&mut self, cx: &mut Context<Self>, path: PathBuf) {
        let started = std::time::Instant::now();
        let path_for_log = path.clone();
        let task_id = format!("metadata-scan:{}", path_for_log.to_string_lossy());
        eprintln!("[indexer] load requested: {}", path_for_log.display());
        self.start_background_task(
            task_id.clone(),
            crate::components::BackgroundTaskKind::MetadataScan,
            "Scan browser folder",
            Some(path_for_log.to_string_lossy().to_string()),
            None,
            false,
        );
        cx.spawn(async move |this, cx| {
            let scan_path = path.clone();
            let task_id_for_update = task_id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { read_directory(&scan_path) })
                .await;
            let elapsed = started.elapsed();
            let _ = this.update(cx, move |this, cx| {
                match result {
                    (entries, None) => {
                        eprintln!(
                            "[indexer] load completed: {} ({} entries, {} ms)",
                            path.display(),
                            entries.len(),
                            elapsed.as_millis()
                        );
                        this.file_browser.apply_loaded(path, entries);
                        this.complete_background_task(
                            &task_id_for_update,
                            Some(format!("{} ms", elapsed.as_millis())),
                        );
                    }
                    (_, Some(error)) => {
                        eprintln!(
                            "[indexer] load failed: {} -> {} ({} ms)",
                            path.display(),
                            error,
                            elapsed.as_millis()
                        );
                        this.fail_background_task(&task_id_for_update, error.clone());
                        this.file_browser.apply_error(path, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
