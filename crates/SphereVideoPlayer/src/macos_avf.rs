//! macOS video frame reader built on AVFoundation.
//!
//! AVFoundation ships with the operating system, so this backend adds no
//! FFmpeg/libav dependency — it decodes whatever VideoToolbox already handles
//! (H.264/HEVC/ProRes MP4 and MOV out of the box).
//!
//! `AVAssetImageGenerator` is the right tool for this crate's job: it is built
//! for "give me the frame at time T" rather than continuous playback, and it
//! handles seeking and the preferred track transform (rotation metadata) itself.
//! Requesting zero time tolerance makes it decode the exact frame instead of the
//! nearest key frame, which is what a reference view against audio needs.
//!
//! Threading: `copyCGImageAtTime` blocks for as long as the decode takes, so
//! this type must stay on the `crate::preview::VideoPreview` worker thread and
//! never touch the GPUI render thread.

use std::path::Path;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_av_foundation::{AVAssetImageGenerator, AVMediaTypeVideo, AVURLAsset};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
};
use objc2_core_media::{CMTimeGetSeconds, CMTimeMakeWithSeconds, kCMTimeZero};
use objc2_foundation::{NSString, NSURL};

use crate::{VideoError, VideoFrame, VideoInfo};

/// CMTime timescale used when converting seconds to a requested position.
/// 600 is the classic QuickTime timescale — it divides evenly by 24, 25, 30 and
/// 60, so common frame rates land on exact tick boundaries.
const TIME_SCALE: i32 = 600;

pub(crate) struct AvFoundationDecoder {
    generator: Retained<AVAssetImageGenerator>,
    info: VideoInfo,
}

impl AvFoundationDecoder {
    pub(crate) fn open(path: &Path) -> Result<Self, VideoError> {
        let path_string = path.to_string_lossy();

        unsafe {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path_string));
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);

            let media_type = AVMediaTypeVideo
                .ok_or_else(|| VideoError::Backend("AVMediaTypeVideo unavailable".into()))?;
            #[allow(deprecated)]
            let tracks = asset.tracksWithMediaType(media_type);
            let track = tracks.firstObject().ok_or(VideoError::NoVideoStream)?;

            let natural_size = track.naturalSize();
            let frame_rate = track.nominalFrameRate() as f64;
            let duration_seconds = CMTimeGetSeconds(asset.duration());

            let generator =
                AVAssetImageGenerator::initWithAsset(AVAssetImageGenerator::alloc(), &asset);
            // Honour rotation metadata so portrait phone footage is not shown
            // sideways in the reference view.
            generator.setAppliesPreferredTrackTransform(true);
            // Exact-frame decoding. Without this AVFoundation is free to return
            // the nearest key frame, which drifts against the audio timeline.
            generator.setRequestedTimeToleranceBefore(kCMTimeZero);
            generator.setRequestedTimeToleranceAfter(kCMTimeZero);

            let width = natural_size.width.max(0.0) as u32;
            let height = natural_size.height.max(0.0) as u32;
            if width == 0 || height == 0 {
                return Err(VideoError::NoVideoStream);
            }

            Ok(Self {
                generator,
                info: VideoInfo {
                    width,
                    height,
                    duration_seconds: if duration_seconds.is_finite() {
                        duration_seconds.max(0.0)
                    } else {
                        0.0
                    },
                    frame_rate: if frame_rate.is_finite() {
                        frame_rate.max(0.0)
                    } else {
                        0.0
                    },
                },
            })
        }
    }

    pub(crate) fn info(&self) -> VideoInfo {
        self.info
    }

    pub(crate) fn frame_at(&mut self, seconds: f64) -> Result<VideoFrame, VideoError> {
        if self.info.duration_seconds > 0.0 && seconds > self.info.duration_seconds {
            return Err(VideoError::EndOfStream);
        }

        unsafe {
            let requested = CMTimeMakeWithSeconds(seconds, TIME_SCALE);
            let mut actual = kCMTimeZero;
            #[allow(deprecated)]
            let image = self
                .generator
                .copyCGImageAtTime_actualTime_error(requested, &mut actual)
                .map_err(|err| VideoError::Backend(err.localizedDescription().to_string()))?;

            let timestamp_seconds = {
                let actual_seconds = CMTimeGetSeconds(actual);
                if actual_seconds.is_finite() {
                    actual_seconds.max(0.0)
                } else {
                    seconds
                }
            };

            render_cgimage_to_bgra(&image, timestamp_seconds)
        }
    }
}

/// Draws a `CGImage` into a tightly packed, top-down BGRA8 buffer.
///
/// The image comes back at its transformed size (rotation applied), so its own
/// dimensions — not the track's natural size — are authoritative here.
fn render_cgimage_to_bgra(
    image: &CGImage,
    timestamp_seconds: f64,
) -> Result<VideoFrame, VideoError> {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    if width == 0 || height == 0 {
        return Err(VideoError::Backend("decoded frame has zero size".into()));
    }

    let row_bytes = width * 4;
    let mut bgra = vec![0u8; row_bytes * height];

    let color_space = CGColorSpace::new_device_rgb()
        .ok_or_else(|| VideoError::Backend("could not create sRGB color space".into()))?;

    // Premultiplied-first + little-endian byte order is Core Graphics' spelling
    // of "BGRA in memory", which is the layout GPUI uploads without conversion.
    let bitmap_info = CGImageAlphaInfo::PremultipliedFirst.0 | CGBitmapInfo::ByteOrder32Little.0;

    unsafe {
        let context = CGBitmapContextCreate(
            bgra.as_mut_ptr().cast(),
            width,
            height,
            8,
            row_bytes,
            Some(&color_space),
            bitmap_info,
        )
        .ok_or_else(|| VideoError::Backend("could not create bitmap context".into()))?;

        // A bitmap context's backing store is written top row first even though
        // its drawing origin is bottom-left, so drawing the image over the full
        // context rect leaves `bgra` in the top-down order this crate promises —
        // no row flip needed.
        CGContext::draw_image(
            Some(&context),
            CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(width as f64, height as f64),
            ),
            Some(image),
        );
    }

    Ok(VideoFrame {
        width: width as u32,
        height: height as u32,
        timestamp_seconds,
        bgra,
    })
}
