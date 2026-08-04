//! Windows video frame reader built on the Media Foundation Source Reader.
//!
//! Media Foundation is part of the operating system, so this backend adds no
//! FFmpeg/libav dependency and no redistributable codec binaries — it decodes
//! whatever the machine's installed codecs already handle (H.264/AAC MP4 and
//! MOV out of the box, HEVC/VP9 when the corresponding OS codec is present).
//!
//! Threading: `IMFSourceReader` is apartment-bound and this type is neither
//! `Send` nor `Sync`. Construct and use it from exactly one worker thread —
//! `crate::preview::VideoPreview` owns that thread. `ReadSample` blocks, so it
//! must never run on the GPUI render thread.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFSTARTUP_NOSOCKET, MFStartup,
    MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VT_I8;
use windows::core::HSTRING;

use crate::{VideoError, VideoFrame, VideoInfo};

/// Media Foundation timestamps and durations are in 100-nanosecond units.
const HNS_PER_SECOND: f64 = 10_000_000.0;

/// Reading forward is much cheaper than a seek (a seek lands on the preceding
/// key frame and re-decodes the GOP). When the requested position is ahead of
/// the reader by no more than this, step forward instead of seeking.
const FORWARD_SCAN_WINDOW_SECONDS: f64 = 2.0;

/// Upper bound on samples consumed by one `frame_at` call, so a pathological
/// stream cannot pin the worker thread indefinitely. On overrun the most
/// recently decoded frame is returned rather than an error.
const MAX_SAMPLES_PER_REQUEST: u32 = 480;

fn hresult_error(context: &str, err: windows::core::Error) -> VideoError {
    VideoError::Backend(format!("{context}: {err}"))
}

/// `MFStartup` is process-global and reference-counted by the OS. Calling it
/// once for the process lifetime is intentional: the preview worker can be
/// stopped and restarted while the app runs, and matching every start with a
/// shutdown would tear down Media Foundation under a still-open reader.
fn ensure_media_foundation_started() -> Result<(), VideoError> {
    use std::sync::OnceLock;
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();

    STARTED
        .get_or_init(|| unsafe {
            // The reader is used from a plain worker thread, so it needs COM in
            // the multithreaded apartment. `RPC_E_CHANGED_MODE` means the thread
            // already joined an apartment — harmless here.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).map_err(|err| err.to_string())
        })
        .clone()
        .map_err(VideoError::Backend)
}

/// Frame geometry as the reader currently reports it. Re-read whenever the
/// source reader signals a mid-stream media type change.
#[derive(Debug, Clone, Copy)]
struct OutputFormat {
    width: u32,
    height: u32,
    /// Row pitch in bytes. Negative means the frame is stored bottom-up.
    stride: i32,
}

impl OutputFormat {
    fn row_bytes(&self) -> usize {
        self.width as usize * 4
    }

    fn abs_stride(&self) -> usize {
        self.stride.unsigned_abs() as usize
    }
}

pub(crate) struct MediaFoundationDecoder {
    reader: IMFSourceReader,
    format: OutputFormat,
    info: VideoInfo,
    /// Presentation time of the frame currently held in `last_frame`.
    last_frame: Option<VideoFrame>,
    /// `true` once the reader has produced end-of-stream and has not been
    /// seeked since; further forward reads would return nothing.
    at_end: bool,
}

impl MediaFoundationDecoder {
    pub(crate) fn open(path: &Path) -> Result<Self, VideoError> {
        ensure_media_foundation_started()?;

        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)
                .map_err(|e| hresult_error("MFCreateAttributes", e))?;
            let attributes =
                attributes.ok_or_else(|| VideoError::Backend("null reader attributes".into()))?;
            // Lets the Source Reader insert the OS Video Processor MFT, which is
            // what converts the decoder's native NV12/YUV output to RGB32.
            attributes
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .map_err(|e| hresult_error("enable video processing", e))?;

            let url = HSTRING::from(path.as_os_str());
            let reader = MFCreateSourceReaderFromURL(&url, &attributes)
                .map_err(|e| hresult_error("MFCreateSourceReaderFromURL", e))?;

            let video_stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(|e| hresult_error("deselect streams", e))?;
            // A container with no video stream fails here rather than later with
            // an opaque read error.
            reader
                .SetStreamSelection(video_stream, true)
                .map_err(|_| VideoError::NoVideoStream)?;

            let requested =
                MFCreateMediaType().map_err(|e| hresult_error("MFCreateMediaType", e))?;
            requested
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| hresult_error("set major type", e))?;
            requested
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| hresult_error("set RGB32 subtype", e))?;
            reader
                .SetCurrentMediaType(video_stream, None, &requested)
                .map_err(|e| hresult_error("negotiate RGB32 output", e))?;

            let format = read_output_format(&reader)?;
            let info = VideoInfo {
                width: format.width,
                height: format.height,
                duration_seconds: read_duration_seconds(&reader),
                frame_rate: read_frame_rate(&reader),
            };

            Ok(Self {
                reader,
                format,
                info,
                last_frame: None,
                at_end: false,
            })
        }
    }

    pub(crate) fn info(&self) -> VideoInfo {
        self.info
    }

    /// Nominal duration of one frame, used to decide whether the cached frame
    /// still covers the requested position.
    fn frame_duration_seconds(&self) -> f64 {
        if self.info.frame_rate > 0.0 {
            1.0 / self.info.frame_rate
        } else {
            1.0 / 30.0
        }
    }

    pub(crate) fn frame_at(&mut self, seconds: f64) -> Result<VideoFrame, VideoError> {
        let frame_duration = self.frame_duration_seconds();

        // Already showing the right frame — most scrub/tick requests land here.
        if let Some(frame) = &self.last_frame {
            let start = frame.timestamp_seconds;
            if seconds >= start && seconds < start + frame_duration {
                return Ok(frame.clone());
            }
        }

        let needs_seek = match &self.last_frame {
            None => true,
            Some(frame) => {
                seconds < frame.timestamp_seconds
                    || seconds > frame.timestamp_seconds + FORWARD_SCAN_WINDOW_SECONDS
            }
        };

        if needs_seek {
            self.seek(seconds)?;
        } else if self.at_end {
            // Forward reads cannot produce anything new past end-of-stream;
            // hold the last frame instead of spinning.
            return self.last_frame.clone().ok_or(VideoError::EndOfStream);
        }

        self.read_forward_to(seconds, frame_duration)
    }

    fn seek(&mut self, seconds: f64) -> Result<(), VideoError> {
        let hns = (seconds * HNS_PER_SECOND).round().max(0.0) as i64;
        unsafe {
            let mut position: windows::Win32::System::Com::StructuredStorage::PROPVARIANT =
                std::mem::zeroed();
            // `PROPVARIANT`'s payload sits behind a `ManuallyDrop` union field,
            // so the writes are explicit derefs — the zeroed value it replaces
            // owns nothing and must not be dropped.
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = hns;
            self.reader
                .SetCurrentPosition(&windows::core::GUID::zeroed(), &position)
                .map_err(|e| hresult_error("SetCurrentPosition", e))?;
        }
        self.last_frame = None;
        self.at_end = false;
        Ok(())
    }

    /// Consumes samples until one covers `target_seconds`, then returns it.
    fn read_forward_to(
        &mut self,
        target_seconds: f64,
        frame_duration: f64,
    ) -> Result<VideoFrame, VideoError> {
        let video_stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

        for _ in 0..MAX_SAMPLES_PER_REQUEST {
            let mut flags = 0u32;
            let mut timestamp_hns = 0i64;
            let mut sample = None;

            unsafe {
                self.reader
                    .ReadSample(
                        video_stream,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut timestamp_hns),
                        Some(&mut sample),
                    )
                    .map_err(|e| hresult_error("ReadSample", e))?;
            }

            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                self.format = read_output_format(&self.reader)?;
                self.info.width = self.format.width;
                self.info.height = self.format.height;
            }

            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.at_end = true;
                // Past the end, the last decoded frame is the honest answer for
                // a reference view; only a stream that produced nothing errors.
                return self.last_frame.clone().ok_or(VideoError::EndOfStream);
            }

            // Gaps and stream ticks carry no sample; keep reading.
            let Some(sample) = sample else {
                continue;
            };

            let timestamp_seconds = timestamp_hns as f64 / HNS_PER_SECOND;
            let frame = convert_sample(&sample, self.format, timestamp_seconds)?;
            let reached_target = timestamp_seconds + frame_duration > target_seconds;
            self.last_frame = Some(frame);
            if reached_target {
                return Ok(self.last_frame.clone().expect("frame was just stored"));
            }
        }

        // Budget exhausted (very long GOP or a badly timestamped stream): return
        // what was decoded rather than blocking the worker for another pass.
        self.last_frame.clone().ok_or(VideoError::EndOfStream)
    }
}

/// Reads width/height/stride from the reader's *current* output type. Called
/// after negotiation and again on every media-type change.
fn read_output_format(reader: &IMFSourceReader) -> Result<OutputFormat, VideoError> {
    unsafe {
        let media_type = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .map_err(|e| hresult_error("GetCurrentMediaType", e))?;

        // MF_MT_FRAME_SIZE packs width in the high 32 bits, height in the low.
        let packed = media_type
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| hresult_error("MF_MT_FRAME_SIZE", e))?;
        let width = (packed >> 32) as u32;
        let height = (packed & 0xFFFF_FFFF) as u32;
        if width == 0 || height == 0 {
            return Err(VideoError::NoVideoStream);
        }

        // Absent on some transforms; a positive packed stride is the safe
        // default because the video processor emits top-down RGB32 there.
        let stride = media_type
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map(|value| value as i32)
            .unwrap_or((width * 4) as i32);

        Ok(OutputFormat {
            width,
            height,
            stride,
        })
    }
}

fn read_frame_rate(reader: &IMFSourceReader) -> f64 {
    unsafe {
        let Ok(media_type) =
            reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
        else {
            return 0.0;
        };
        let Ok(packed) = media_type.GetUINT64(&MF_MT_FRAME_RATE) else {
            return 0.0;
        };
        let numerator = (packed >> 32) as u32;
        let denominator = (packed & 0xFFFF_FFFF) as u32;
        if denominator == 0 {
            0.0
        } else {
            numerator as f64 / denominator as f64
        }
    }
}

fn read_duration_seconds(reader: &IMFSourceReader) -> f64 {
    unsafe {
        let Ok(value) =
            reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
        else {
            return 0.0;
        };
        // MF_PD_DURATION is a VT_UI8 count of 100ns units; no heap payload to
        // release, so the PROPVARIANT can be read and dropped as plain data.
        let hns = value.Anonymous.Anonymous.Anonymous.uhVal;
        hns as f64 / HNS_PER_SECOND
    }
}

/// Copies one RGB32 sample into a tightly packed, top-down BGRA buffer.
fn convert_sample(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
    format: OutputFormat,
    timestamp_seconds: f64,
) -> Result<VideoFrame, VideoError> {
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| hresult_error("ConvertToContiguousBuffer", e))?;

        let mut data: *mut u8 = std::ptr::null_mut();
        let mut current_len = 0u32;
        buffer
            .Lock(&mut data, None, Some(&mut current_len))
            .map_err(|e| hresult_error("IMFMediaBuffer::Lock", e))?;

        let row_bytes = format.row_bytes();
        let stride = format.abs_stride();
        let height = format.height as usize;
        let mut bgra = vec![0u8; row_bytes * height];

        let result =
            if data.is_null() || stride < row_bytes || (current_len as usize) < stride * height {
                Err(VideoError::Backend(format!(
                    "unexpected frame layout: {}x{} stride={} bytes={current_len}",
                    format.width, format.height, format.stride
                )))
            } else {
                for row in 0..height {
                    // A negative default stride means the frame is stored bottom-up:
                    // buffer row 0 is the image's last row.
                    let source_row = if format.stride < 0 {
                        height - 1 - row
                    } else {
                        row
                    };
                    let source = data.add(source_row * stride);
                    let destination = bgra.as_mut_ptr().add(row * row_bytes);
                    std::ptr::copy_nonoverlapping(source, destination, row_bytes);
                    // RGB32's fourth channel is undefined padding; make it opaque so
                    // the frame composites correctly in the UI.
                    for pixel in 0..format.width as usize {
                        *destination.add(pixel * 4 + 3) = 0xFF;
                    }
                }
                Ok(())
            };

        let _ = buffer.Unlock();
        result?;

        Ok(VideoFrame {
            width: format.width,
            height: format.height,
            timestamp_seconds,
            bgra,
        })
    }
}
