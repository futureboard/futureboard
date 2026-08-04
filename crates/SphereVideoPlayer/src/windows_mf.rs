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
use std::sync::Arc;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFDXGIDeviceManager, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_D3D_MANAGER, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video, MFSTARTUP_NOSOCKET,
    MFStartup, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{HSTRING, Interface};

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

/// D3D11 device manager handed to the Source Reader so it can pick a hardware
/// (DXVA) decoder MFT. Built once per process and shared by every decoder: the
/// device is created multithread-protected, which is exactly what lets several
/// preview workers submit to it.
///
/// `None` when this machine has no usable D3D11 video device (no GPU, a remote
/// session, a driver that refuses `VIDEO_SUPPORT`). Callers fall back to the
/// software Source Reader configuration rather than failing to open the file —
/// a soft picture monitor is better than no picture monitor.
fn dxgi_device_manager() -> Option<IMFDXGIDeviceManager> {
    use std::sync::OnceLock;
    static MANAGER: OnceLock<Option<DxgiManager>> = OnceLock::new();

    MANAGER
        .get_or_init(|| {
            let manager = create_dxgi_device_manager();
            if manager.is_none() {
                tracing::info!(
                    "video: no D3D11 video device; decoding reference video in software"
                );
            }
            manager.map(DxgiManager)
        })
        .as_ref()
        .map(|manager| manager.0.clone())
}

/// `IMFDXGIDeviceManager` is `Send`/`Sync`-hostile in the `windows` crate's
/// view because it is a raw COM pointer, but the underlying manager is
/// documented as thread-safe once the device is multithread-protected, which is
/// what `create_dxgi_device_manager` guarantees before this wrapper is built.
struct DxgiManager(IMFDXGIDeviceManager);

// SAFETY: the wrapped manager is only ever handed out by cloning the COM
// pointer (an interlocked AddRef), and the D3D11 device behind it has
// `ID3D11Multithread::SetMultithreadProtected(true)` applied at creation, so
// concurrent use from several preview worker threads is serialized by D3D
// itself.
unsafe impl Send for DxgiManager {}
unsafe impl Sync for DxgiManager {}

fn create_dxgi_device_manager() -> Option<IMFDXGIDeviceManager> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        // `VIDEO_SUPPORT` is what makes the device usable by a decoder MFT;
        // `BGRA_SUPPORT` lets the video processor emit the RGB32 this backend
        // asks for without a second conversion pass.
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .ok()?;
        let device = device?;

        // Media Foundation drives this device from its own decoder threads, so
        // it must be multithread-protected before the manager publishes it.
        let multithread: ID3D11Multithread = device.cast().ok()?;
        // Returns the *previous* setting, not a status — nothing to check.
        let _ = multithread.SetMultithreadProtected(true);

        let mut reset_token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut reset_token, &mut manager).ok()?;
        let manager = manager?;
        manager.ResetDevice(&device, reset_token).ok()?;
        Some(manager)
    }
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
    /// Most recently decoded frame. Behind an `Arc` because it is handed out on
    /// every cache hit and every end-of-stream hold — cloning the `VideoFrame`
    /// itself would memcpy the whole uncompressed image (8 MB at 1080p) several
    /// times per transport tick.
    last_frame: Option<Arc<VideoFrame>>,
    /// `true` once the reader has produced end-of-stream and has not been
    /// seeked since; further forward reads would return nothing.
    at_end: bool,
}

impl MediaFoundationDecoder {
    pub(crate) fn open(path: &Path) -> Result<Self, VideoError> {
        ensure_media_foundation_started()?;

        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)
                .map_err(|e| hresult_error("MFCreateAttributes", e))?;
            let attributes =
                attributes.ok_or_else(|| VideoError::Backend("null reader attributes".into()))?;

            // Hardware decode. Without a D3D manager the Source Reader only ever
            // instantiates *software* decoder MFTs, which is what made a 4K/long-GOP
            // reference video crawl: every seek re-decoded a GOP on the CPU.
            //
            // The two attributes go together — `HARDWARE_TRANSFORMS` lets the
            // reader consider hardware MFTs at all, and `D3D_MANAGER` gives them
            // the device to decode onto. `ADVANCED_VIDEO_PROCESSING` is required
            // instead of the plain flag once samples live in D3D surfaces,
            // because the plain Video Processor cannot read them.
            let hardware = dxgi_device_manager();
            if let Some(manager) = &hardware {
                attributes
                    .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                    .map_err(|e| hresult_error("enable hardware transforms", e))?;
                attributes
                    .SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, manager)
                    .map_err(|e| hresult_error("set D3D manager", e))?;
                attributes
                    .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
                    .map_err(|e| hresult_error("enable advanced video processing", e))?;
            } else {
                // Software path: lets the Source Reader insert the OS Video
                // Processor MFT, which converts the decoder's native NV12/YUV
                // output to RGB32.
                attributes
                    .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                    .map_err(|e| hresult_error("enable video processing", e))?;
            }

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

            let native = read_native_frame_size(&reader);

            let requested =
                MFCreateMediaType().map_err(|e| hresult_error("MFCreateMediaType", e))?;
            requested
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| hresult_error("set major type", e))?;
            requested
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| hresult_error("set RGB32 subtype", e))?;

            // Ask the video processor to downscale oversized sources. A 4K frame
            // is 33 MB of BGRA; every one of those is read back from the GPU,
            // copied, and uploaded as a texture for a window that is a few
            // hundred pixels wide. Scaling on the GPU before readback cuts all
            // three costs by the square of the ratio.
            //
            // Only the advanced (hardware) video processor can resize — the
            // basic one converts format only — so this is attempted, not
            // required, and negotiation retries at native size if it is refused.
            let scaled = native
                .filter(|_| hardware.is_some())
                .and_then(|(width, height)| preview_frame_size(width, height));
            let negotiated_scaled = if let Some((width, height)) = scaled {
                requested
                    .SetUINT64(&MF_MT_FRAME_SIZE, pack_frame_size(width, height))
                    .map_err(|e| hresult_error("set preview frame size", e))?;
                reader
                    .SetCurrentMediaType(video_stream, None, &requested)
                    .is_ok()
            } else {
                false
            };
            if !negotiated_scaled {
                if scaled.is_some() {
                    // Put the native size back so the retry is a plain
                    // format-only negotiation.
                    if let Some((width, height)) = native {
                        let _ =
                            requested.SetUINT64(&MF_MT_FRAME_SIZE, pack_frame_size(width, height));
                    }
                }
                reader
                    .SetCurrentMediaType(video_stream, None, &requested)
                    .map_err(|e| hresult_error("negotiate RGB32 output", e))?;
            }

            let format = read_output_format(&reader)?;
            // `info` describes the *source*, so it keeps the native dimensions
            // even when the preview decodes smaller.
            let (info_width, info_height) = native.unwrap_or((format.width, format.height));
            let info = VideoInfo {
                width: info_width,
                height: info_height,
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

    pub(crate) fn frame_at(&mut self, seconds: f64) -> Result<Arc<VideoFrame>, VideoError> {
        let frame_duration = self.frame_duration_seconds();

        // Already showing the right frame — most scrub/tick requests land here.
        if let Some(frame) = &self.last_frame {
            let start = frame.timestamp_seconds;
            if seconds >= start && seconds < start + frame_duration {
                return Ok(Arc::clone(frame));
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
    ) -> Result<Arc<VideoFrame>, VideoError> {
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
                // Output geometry only. `info` reports the source's own size,
                // which a change in the decoded preview size does not alter.
                self.format = read_output_format(&self.reader)?;
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
            let frame = Arc::new(convert_sample(&sample, self.format, timestamp_seconds)?);
            let reached_target = timestamp_seconds + frame_duration > target_seconds;
            self.last_frame = Some(Arc::clone(&frame));
            if reached_target {
                return Ok(frame);
            }
        }

        // Budget exhausted (very long GOP or a badly timestamped stream): return
        // what was decoded rather than blocking the worker for another pass.
        self.last_frame.clone().ok_or(VideoError::EndOfStream)
    }
}

/// `MF_MT_FRAME_SIZE` packs width in the high 32 bits and height in the low.
fn pack_frame_size(width: u32, height: u32) -> u64 {
    ((width as u64) << 32) | height as u64
}

fn unpack_frame_size(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

/// The stream's own frame size, before any output negotiation.
fn read_native_frame_size(reader: &IMFSourceReader) -> Option<(u32, u32)> {
    unsafe {
        let media_type = reader
            .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
            .ok()?;
        let (width, height) = unpack_frame_size(media_type.GetUINT64(&MF_MT_FRAME_SIZE).ok()?);
        (width > 0 && height > 0).then_some((width, height))
    }
}

/// Longest edge the preview decodes at. A picture monitor is watched at a few
/// hundred points across, so decoding a 4K source at full size buys nothing and
/// costs 33 MB per frame in readback, copy, and texture upload.
const PREVIEW_MAX_EDGE: u32 = 1280;

/// The size to decode `width`x`height` at, or `None` when it already fits.
///
/// Never upscales, and keeps the aspect ratio. Both edges are rounded to even
/// numbers because chroma-subsampled sources scale to odd sizes badly.
fn preview_frame_size(width: u32, height: u32) -> Option<(u32, u32)> {
    let longest = width.max(height);
    if longest <= PREVIEW_MAX_EDGE || width == 0 || height == 0 {
        return None;
    }
    let scale = PREVIEW_MAX_EDGE as f64 / longest as f64;
    let even = |value: u32| ((value as f64 * scale).round() as u32).max(2) & !1;
    Some((even(width), even(height)))
}

/// Reads width/height/stride from the reader's *current* output type. Called
/// after negotiation and again on every media-type change.
fn read_output_format(reader: &IMFSourceReader) -> Result<OutputFormat, VideoError> {
    unsafe {
        let media_type = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .map_err(|e| hresult_error("GetCurrentMediaType", e))?;

        let packed = media_type
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .map_err(|e| hresult_error("MF_MT_FRAME_SIZE", e))?;
        let (width, height) = unpack_frame_size(packed);
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
///
/// Two lock paths, because the hardware path changes where the pixels live.
/// A D3D-backed sample's mapped row pitch is chosen by the driver and need not
/// match `MF_MT_DEFAULT_STRIDE`, so trusting the media type's stride there
/// would skew the picture. `IMF2DBuffer::Lock2D` reports the real pitch and is
/// tried first; the plain `Lock` path stays for system-memory buffers that do
/// not implement it.
fn convert_sample(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
    format: OutputFormat,
    timestamp_seconds: f64,
) -> Result<VideoFrame, VideoError> {
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| hresult_error("ConvertToContiguousBuffer", e))?;

        let row_bytes = format.row_bytes();
        let height = format.height as usize;
        let mut bgra = vec![0u8; row_bytes * height];

        let two_d = buffer.cast::<IMF2DBuffer>().ok();
        let mut scanline0: *mut u8 = std::ptr::null_mut();
        let mut pitch = 0i32;
        let mut locked_len = 0u32;

        // `Lock2D` hands back the first scanline *of the image* and a signed
        // pitch, so a bottom-up surface is walked by stepping backwards rather
        // than by flipping the row index.
        let (base, step) = if let Some(two_d) = &two_d {
            two_d
                .Lock2D(&mut scanline0, &mut pitch)
                .map_err(|e| hresult_error("IMF2DBuffer::Lock2D", e))?;
            (scanline0, pitch as isize)
        } else {
            buffer
                .Lock(&mut scanline0, None, Some(&mut locked_len))
                .map_err(|e| hresult_error("IMFMediaBuffer::Lock", e))?;
            let stride = format.abs_stride();
            if format.stride < 0 && !scanline0.is_null() {
                // Bottom-up: buffer row 0 is the image's last row.
                (
                    scanline0.add(stride * height.saturating_sub(1)),
                    -(stride as isize),
                )
            } else {
                (scanline0, stride as isize)
            }
        };

        let usable_pitch = step.unsigned_abs();
        let enough_bytes = two_d.is_some() || (locked_len as usize) >= usable_pitch * height;
        let result = if base.is_null() || usable_pitch < row_bytes || !enough_bytes {
            Err(VideoError::Backend(format!(
                "unexpected frame layout: {}x{} pitch={step} bytes={locked_len}",
                format.width, format.height
            )))
        } else {
            for row in 0..height {
                let source = base.offset(step * row as isize);
                let destination = bgra.as_mut_ptr().add(row * row_bytes);
                std::ptr::copy_nonoverlapping(source, destination, row_bytes);
            }
            // RGB32's fourth channel is undefined padding; force it opaque so
            // the frame composites correctly in the UI. One flat pass over the
            // packed buffer, outside the row loop: the old per-row `add`
            // arithmetic recomputed a base pointer for every pixel and blocked
            // vectorization.
            for pixel in bgra.chunks_exact_mut(4) {
                pixel[3] = 0xFF;
            }
            Ok(())
        };

        if let Some(two_d) = &two_d {
            let _ = two_d.Unlock2D();
        } else {
            let _ = buffer.Unlock();
        }
        result?;

        Ok(VideoFrame {
            width: format.width,
            height: format.height,
            timestamp_seconds,
            bgra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_packing_round_trips() {
        assert_eq!(unpack_frame_size(pack_frame_size(3840, 2160)), (3840, 2160));
    }

    #[test]
    fn a_source_that_already_fits_is_not_rescaled() {
        assert_eq!(preview_frame_size(1280, 720), None);
        assert_eq!(preview_frame_size(640, 480), None);
        // Never upscales a small source.
        assert_eq!(preview_frame_size(320, 240), None);
    }

    #[test]
    fn oversized_sources_scale_down_keeping_aspect() {
        // 4K UHD, 16:9 -> capped long edge, height follows.
        assert_eq!(preview_frame_size(3840, 2160), Some((1280, 720)));
        // Portrait: the cap applies to the long edge, which is the height.
        assert_eq!(preview_frame_size(2160, 3840), Some((720, 1280)));
    }

    #[test]
    fn scaled_sizes_are_even() {
        for (width, height) in [(3840u32, 2158u32), (2000, 1333), (4096, 1717)] {
            let (scaled_width, scaled_height) =
                preview_frame_size(width, height).expect("oversized");
            assert_eq!(scaled_width % 2, 0, "{width}x{height} width");
            assert_eq!(scaled_height % 2, 0, "{width}x{height} height");
            assert!(scaled_width.max(scaled_height) <= PREVIEW_MAX_EDGE);
        }
    }

    #[test]
    fn degenerate_sizes_do_not_scale() {
        assert_eq!(preview_frame_size(0, 0), None);
        assert_eq!(preview_frame_size(3840, 0), None);
    }
}
