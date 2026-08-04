//! Off-thread frame service for the Video Player window.
//!
//! One worker thread owns the decoder. The UI never blocks on decode: it posts
//! the position it wants (coalescing — only the newest request survives) and
//! later polls for the most recent decoded frame. A frame is published with a
//! monotonically increasing revision so the UI can tell "nothing new" from
//! "same timestamp, new decode" without comparing pixels.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::{VideoDecoder, VideoError, VideoFrame, VideoInfo};

/// A decoded frame shared with the UI. Cloning is a refcount bump — frames are
/// full uncompressed images and must never be deep-copied per render.
pub type VideoPreviewFrame = Arc<VideoFrame>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewStatus {
    /// The worker has not finished opening the file yet.
    Opening,
    /// The file is open and frames can be produced.
    Ready,
    /// This build has no decode backend for the platform.
    Unsupported,
    /// Opening or decoding failed; the message is shown in the player window.
    Failed(String),
}

impl PreviewStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Failed(message) => Some(message.as_str()),
            _ => None,
        }
    }
}

struct Request {
    /// Newest requested position in seconds; `None` when nothing is pending.
    position_seconds: Option<f64>,
    stop: bool,
}

struct Shared {
    request: Mutex<Request>,
    wake: Condvar,
    stopping: AtomicBool,
    /// Latest decoded frame plus the status/info the UI renders around it.
    published: Mutex<Published>,
    revision: AtomicU64,
}

#[derive(Default)]
struct Published {
    frame: Option<VideoPreviewFrame>,
    info: Option<VideoInfo>,
    status: Option<PreviewStatus>,
}

/// Handle to a running preview worker. Dropping it stops the worker and closes
/// the file.
pub struct VideoPreview {
    path: PathBuf,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl VideoPreview {
    /// Spawns the worker and starts opening `path`. Returns immediately; the
    /// caller polls [`Self::status`] to learn whether the file opened.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let shared = Arc::new(Shared {
            request: Mutex::new(Request {
                position_seconds: None,
                stop: false,
            }),
            wake: Condvar::new(),
            stopping: AtomicBool::new(false),
            published: Mutex::new(Published {
                status: Some(PreviewStatus::Opening),
                ..Published::default()
            }),
            revision: AtomicU64::new(0),
        });

        let worker = {
            let shared = Arc::clone(&shared);
            let path = path.clone();
            std::thread::Builder::new()
                .name("fb-video-preview".into())
                .spawn(move || run_worker(path, shared))
                .ok()
        };

        if worker.is_none() {
            shared.publish_status(PreviewStatus::Failed(
                "could not start the video preview thread".into(),
            ));
        }

        Self {
            path,
            shared,
            worker,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Asks for the frame at `seconds`. Cheap and non-blocking; repeated calls
    /// while the worker is busy collapse into a single decode of the newest
    /// position, which is what makes scrubbing keep up.
    pub fn request_position(&self, seconds: f64) {
        let Ok(mut request) = self.shared.request.lock() else {
            return;
        };
        request.position_seconds = Some(seconds.max(0.0));
        drop(request);
        self.shared.wake.notify_one();
    }

    pub fn status(&self) -> PreviewStatus {
        self.shared
            .published
            .lock()
            .ok()
            .and_then(|published| published.status.clone())
            .unwrap_or(PreviewStatus::Opening)
    }

    pub fn info(&self) -> Option<VideoInfo> {
        self.shared
            .published
            .lock()
            .ok()
            .and_then(|published| published.info)
    }

    /// Current publish revision. Store it alongside a rendered frame and pass it
    /// to [`Self::frame_if_newer`] to avoid rebuilding the GPU image every tick.
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }

    /// The latest frame when it is newer than `seen_revision`.
    pub fn frame_if_newer(&self, seen_revision: u64) -> Option<(u64, VideoPreviewFrame)> {
        let revision = self.revision();
        if revision <= seen_revision {
            return None;
        }
        let published = self.shared.published.lock().ok()?;
        published
            .frame
            .as_ref()
            .map(|frame| (revision, Arc::clone(frame)))
    }

    /// The latest frame regardless of revision.
    pub fn frame(&self) -> Option<VideoPreviewFrame> {
        let published = self.shared.published.lock().ok()?;
        published.frame.as_ref().map(Arc::clone)
    }
}

impl Drop for VideoPreview {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        if let Ok(mut request) = self.shared.request.lock() {
            request.stop = true;
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Shared {
    fn publish_status(&self, status: PreviewStatus) {
        if let Ok(mut published) = self.published.lock() {
            published.status = Some(status);
        }
    }

    fn publish_info(&self, info: VideoInfo) {
        if let Ok(mut published) = self.published.lock() {
            published.info = Some(info);
        }
    }

    fn publish_frame(&self, frame: VideoFrame) {
        if let Ok(mut published) = self.published.lock() {
            published.frame = Some(Arc::new(frame));
        }
        self.revision.fetch_add(1, Ordering::Release);
    }

    /// Blocks until a position is requested or the handle is dropped. Returns
    /// `None` to stop the worker.
    fn wait_for_request(&self) -> Option<f64> {
        let mut request = self.request.lock().ok()?;
        loop {
            if request.stop || self.stopping.load(Ordering::Acquire) {
                return None;
            }
            if let Some(position) = request.position_seconds.take() {
                return Some(position);
            }
            request = self.wake.wait(request).ok()?;
        }
    }
}

fn run_worker(path: PathBuf, shared: Arc<Shared>) {
    let mut decoder = match VideoDecoder::open(&path) {
        Ok(decoder) => decoder,
        Err(VideoError::UnsupportedPlatform) => {
            shared.publish_status(PreviewStatus::Unsupported);
            return;
        }
        Err(err) => {
            shared.publish_status(PreviewStatus::Failed(err.to_string()));
            return;
        }
    };

    shared.publish_info(decoder.info());
    shared.publish_status(PreviewStatus::Ready);

    // Show the opening frame immediately so the window is not blank before the
    // first transport tick arrives.
    if let Ok(frame) = decoder.frame_at(0.0) {
        shared.publish_frame(frame);
    }

    while let Some(position) = shared.wait_for_request() {
        match decoder.frame_at(position) {
            Ok(frame) => shared.publish_frame(frame),
            // Past the end of a shorter reference video the last frame stays on
            // screen; that is expected, not an error worth surfacing.
            Err(VideoError::EndOfStream) => {}
            Err(err) => shared.publish_status(PreviewStatus::Failed(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_a_missing_file_reports_failure_and_stops_cleanly() {
        let preview = VideoPreview::open(crate::tests::missing_video_path());
        // The worker resolves the open before it waits for a position.
        for _ in 0..200 {
            if !matches!(preview.status(), PreviewStatus::Opening) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(matches!(
            preview.status(),
            PreviewStatus::Failed(_) | PreviewStatus::Unsupported
        ));
        assert_eq!(preview.revision(), 0);
        assert!(preview.frame().is_none());
    }

    #[test]
    fn requesting_a_position_before_the_worker_is_ready_does_not_block() {
        let preview = VideoPreview::open(crate::tests::missing_video_path());
        preview.request_position(4.25);
        preview.request_position(9.5);
        assert!(preview.frame_if_newer(0).is_none());
    }
}
