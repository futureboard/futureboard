//! Video preview/reference decoding for the Futureboard Studio Video Track.
//!
//! Scope is deliberately narrow: this crate decodes **single still frames at a
//! requested presentation time** so the arrangement can show a reference video
//! next to the audio. It is not a playback engine — it produces no audio, owns
//! no clock, and never runs on a realtime thread.
//!
//! No FFmpeg / libav dependency exists on any platform. Each backend links the
//! media framework its platform already ships:
//!
//! | Platform | Backend                                     |
//! |----------|---------------------------------------------|
//! | Windows  | Media Foundation Source Reader              |
//! | macOS    | AVFoundation (`AVAssetImageGenerator`)      |
//! | Linux    | none yet — VAAPI is the intended backend    |
//!
//! Linux is deliberately unimplemented rather than wired to GStreamer/FFmpeg,
//! whose licensing this project does not want to take on. VAAPI (`libva`) is
//! the intended replacement, but unlike the other two it is a decode-only API:
//! it has no demuxer and no bitstream parser, so a VAAPI backend also needs a
//! permissively licensed container demuxer and slice-level H.264/HEVC parsing
//! (the `cros-libva`/`cros-codecs` stack, BSD-3-Clause, covers the latter). It
//! also has no software fallback — a machine with no working VAAPI driver
//! decodes nothing.
//!
//! Until that lands, Linux reports [`VideoError::UnsupportedPlatform`] and the
//! UI disables the player honestly instead of showing a fake frame. Any other
//! platform does the same.

use std::path::{Path, PathBuf};
use std::sync::Arc;

mod preview;
pub use preview::{PreviewStatus, VideoPreview, VideoPreviewFrame};

#[cfg(windows)]
mod windows_d3d;
#[cfg(windows)]
mod windows_mf;

#[cfg(target_os = "macos")]
mod macos_avf;

/// `true` when this build has a real decode backend compiled in.
macro_rules! has_backend {
    () => {
        cfg!(any(windows, target_os = "macos"))
    };
}

/// Container extensions the shipped backend is expected to open. Used for file
/// dialog filters and drag-and-drop acceptance; the decoder is still the
/// authority and may reject a file whose codec the OS cannot decode.
pub const SUPPORTED_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "avi", "wmv", "asf", "mkv", "webm", "mpg", "mpeg", "ts", "m2ts",
];

/// `true` when `path`'s extension is one this crate advertises support for.
pub fn is_supported_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| SUPPORTED_VIDEO_EXTENSIONS.contains(&e.as_str()))
}

/// Human-readable name of the compiled decode backend, for diagnostics and the
/// player window's status line.
pub fn backend_name() -> &'static str {
    #[cfg(windows)]
    {
        "media-foundation"
    }
    #[cfg(target_os = "macos")]
    {
        "avfoundation"
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        "unavailable"
    }
}

/// `true` when this build has a real decode backend.
pub fn backend_available() -> bool {
    has_backend!()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoError {
    /// This platform has no decode backend compiled in.
    UnsupportedPlatform,
    /// The file does not exist or could not be opened.
    NotFound(PathBuf),
    /// The container opened but carries no decodable video stream.
    NoVideoStream,
    /// The requested position is past the end of the media.
    EndOfStream,
    /// Backend-specific failure (HRESULT text, lock failure, …).
    Backend(String),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                write!(f, "video decoding is not supported on this platform")
            }
            Self::NotFound(path) => write!(f, "video file not found: {}", path.display()),
            Self::NoVideoStream => write!(f, "file contains no decodable video stream"),
            Self::EndOfStream => write!(f, "requested position is past the end of the video"),
            Self::Backend(message) => write!(f, "video decoder error: {message}"),
        }
    }
}

impl std::error::Error for VideoError {}

/// Static properties of an opened video stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Total presentation duration. `0.0` when the container does not report one.
    pub duration_seconds: f64,
    /// Nominal frame rate. `0.0` when unknown.
    pub frame_rate: f64,
}

impl VideoInfo {
    /// Display aspect ratio, or `16:9` when the stream reported no usable size.
    pub fn aspect_ratio(&self) -> f32 {
        if self.width == 0 || self.height == 0 {
            16.0 / 9.0
        } else {
            self.width as f32 / self.height as f32
        }
    }
}

/// One decoded frame in **BGRA8**, top-down, tightly packed (`width * 4` bytes
/// per row). BGRA is what GPUI's `RenderImage` uploads without conversion.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_seconds: f64,
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("timestamp_seconds", &self.timestamp_seconds)
            .field("bytes", &self.bgra.len())
            .finish()
    }
}

/// A single-stream, single-threaded video frame reader.
///
/// Not `Sync`. Own it from one worker thread (see [`VideoPreview`]) — never
/// from the GPUI render thread, because seek + decode can block for tens of
/// milliseconds on a long GOP.
pub struct VideoDecoder {
    path: PathBuf,
    #[cfg(any(windows, target_os = "macos"))]
    inner: PlatformDecoder,
}

#[cfg(windows)]
type PlatformDecoder = windows_mf::MediaFoundationDecoder;
#[cfg(target_os = "macos")]
type PlatformDecoder = macos_avf::AvFoundationDecoder;

impl VideoDecoder {
    /// Opens `path` and configures the first video stream for BGRA output.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VideoError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(VideoError::NotFound(path));
        }

        #[cfg(any(windows, target_os = "macos"))]
        {
            let inner = PlatformDecoder::open(&path)?;
            Ok(Self { path, inner })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = path;
            Err(VideoError::UnsupportedPlatform)
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> VideoInfo {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.info()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            unreachable!("VideoDecoder cannot be constructed without a backend")
        }
    }

    /// Decodes the frame presented at (or immediately before) `seconds`.
    ///
    /// Seeks only when the target is behind the reader or far ahead of it, so
    /// playback-rate stepping reads forward instead of re-seeking every frame.
    ///
    /// Returns the frame behind an `Arc`: a decoded frame is a full
    /// uncompressed image, and the backends hand the same one out repeatedly
    /// while the playhead stays inside it.
    pub fn frame_at(&mut self, seconds: f64) -> Result<Arc<VideoFrame>, VideoError> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.frame_at(seconds.max(0.0))
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = seconds;
            Err(VideoError::UnsupportedPlatform)
        }
    }
}

/// Probes a file for its dimensions/duration without keeping a decoder open.
pub fn probe(path: impl AsRef<Path>) -> Result<VideoInfo, VideoError> {
    Ok(VideoDecoder::open(path)?.info())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_supported_video_path(Path::new("C:/clips/Take 1.MP4")));
        assert!(is_supported_video_path(Path::new("/tmp/ref.mov")));
        assert!(!is_supported_video_path(Path::new("/tmp/mix.wav")));
        assert!(!is_supported_video_path(Path::new("/tmp/noextension")));
    }

    /// A path under a real directory, so the existence check is a local stat
    /// rather than a lookup on an absent drive letter (which can block for
    /// seconds on Windows).
    pub(crate) fn missing_video_path() -> PathBuf {
        std::env::temp_dir().join("futureboard-nonexistent-reference.mp4")
    }

    #[test]
    fn missing_file_reports_not_found_before_touching_a_backend() {
        let result = VideoDecoder::open(missing_video_path());
        assert!(matches!(result.err(), Some(VideoError::NotFound(_))));
    }

    #[test]
    fn aspect_ratio_falls_back_when_size_is_unknown() {
        let unknown = VideoInfo {
            width: 0,
            height: 0,
            duration_seconds: 0.0,
            frame_rate: 0.0,
        };
        assert!((unknown.aspect_ratio() - 16.0 / 9.0).abs() < 1.0e-6);

        let hd = VideoInfo {
            width: 1920,
            height: 1080,
            duration_seconds: 12.0,
            frame_rate: 30.0,
        };
        assert!((hd.aspect_ratio() - 16.0 / 9.0).abs() < 1.0e-6);
    }

    #[test]
    fn backend_name_matches_availability() {
        assert_eq!(backend_available(), backend_name() != "unavailable");
    }
}
