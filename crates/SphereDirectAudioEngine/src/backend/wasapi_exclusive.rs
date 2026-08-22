//! DAUx WASAPI Exclusive backend — Windows only.
//!
//! Uses raw Win32 WASAPI COM APIs to open a device in exclusive mode with
//! event-driven buffer filling and MMCSS "Pro Audio" thread priority.
//!
//! # Thread model
//!
//! A dedicated audio thread is spawned.  The thread:
//!   1. Calls `CoInitializeEx(COINIT_MULTITHREADED)` for COM.
//!   2. Sets MMCSS "Pro Audio" priority via `AvSetMmThreadCharacteristicsW`.
//!   3. Negotiates an exclusive-mode format with `IsFormatSupported`.
//!   4. Opens WASAPI device in exclusive, event-driven mode.
//!   5. Runs `WaitForMultipleObjects([buf_event, stop_event])` render loop.
//!   6. Calls `CoUninitialize` on exit.
//!
//! # Error behaviour
//!
//! All WASAPI failures are returned as `Err(String)` via the info channel —
//! never panicked, never silently swallowed.  The engine layer is responsible
//! for deciding whether to retry with a fallback backend.

#![allow(non_snake_case, clippy::too_many_arguments)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_NOPERSIST, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};

use crate::backend::render::{drain_commands, fill_output_f32, LocalAudioState};
use crate::backend::DauxDeviceConfig;
use crate::command::EngineCommand;
use crate::engine::SharedState;
use crate::error::SphereAudioError;
use crate::runtime::RuntimeProject;

// ── Raw extern for MMCSS (avrt.lib) ──────────────────────────────────────────

#[link(name = "avrt")]
extern "system" {
    fn AvSetMmThreadCharacteristicsW(task_name: *const u16, task_index: *mut u32) -> isize;
    fn AvRevertMmThreadCharacteristics(handle: isize) -> i32;
}

// ── WASAPI HRESULT error codes ────────────────────────────────────────────────

const E_AUDCLNT_DEVICE_IN_USE: i32 = 0x88890004u32 as i32;
const E_AUDCLNT_UNSUPPORTED_FORMAT: i32 = 0x88890008u32 as i32;
const E_AUDCLNT_EXCLUSIVE_MODE_NOT_ALLOWED: i32 = 0x8889000Eu32 as i32;
const E_AUDCLNT_DEVICE_INVALIDATED: i32 = 0x88890014u32 as i32;
const E_AUDCLNT_BUFFER_SIZE_NOT_ALIGNED: i32 = 0x88890019u32 as i32;

/// Map a WASAPI HRESULT to a human-readable error string.
fn classify_hresult(code: i32, context: &str) -> String {
    let detail = match code {
        E_AUDCLNT_DEVICE_IN_USE => {
            "Device is in use by another application (close other audio software)".to_string()
        }
        E_AUDCLNT_UNSUPPORTED_FORMAT => "Unsupported audio format for exclusive mode".to_string(),
        E_AUDCLNT_EXCLUSIVE_MODE_NOT_ALLOWED => {
            "Exclusive mode is not allowed — enable it in Windows Sound > Advanced".to_string()
        }
        E_AUDCLNT_DEVICE_INVALIDATED => "Audio device was disconnected or invalidated".to_string(),
        E_AUDCLNT_BUFFER_SIZE_NOT_ALIGNED => {
            "Buffer size is not aligned for this device. Try 256 or 512 samples.".to_string()
        }
        _ => format!("HRESULT 0x{:08X}", code as u32),
    };
    format!("WASAPI Exclusive {context}: {detail}")
}

// ─────────────────────────────────────────────────────────────────────────────

/// Handle to a running WASAPI Exclusive stream.
///
/// Dropping this handle signals the audio thread to stop immediately —
/// it sets `stop_flag` AND signals `stop_event` so the thread wakes from
/// `WaitForMultipleObjects` without waiting for the 2-second buffer timeout.
pub struct WasapiExclusiveHandle {
    pub cmd_tx: Sender<EngineCommand>,
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub device_name: String,
    stop_flag: Arc<AtomicBool>,
    /// Manual-reset event signaled to wake the audio thread on shutdown.
    stop_event: HANDLE,
    thread: Option<thread::JoinHandle<()>>,
}

// Safety: HANDLE (isize) is safe to send across threads for a kernel event object.
unsafe impl Send for WasapiExclusiveHandle {}

impl Drop for WasapiExclusiveHandle {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        unsafe {
            let _ = SetEvent(self.stop_event);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop_event);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

pub fn open(
    config: &DauxDeviceConfig,
    shared: Arc<SharedState>,
    initial_runtime: RuntimeProject,
    glitch_counter: Arc<AtomicU64>,
) -> Result<WasapiExclusiveHandle, SphereAudioError> {
    let output_device_id = config.output_device_id.clone();
    let requested_sr = config.sample_rate;
    let buf_frames = config
        .buffer_size
        .unwrap_or(if config.safe_mode { 512 } else { 256 });

    let (tx, rx) = bounded::<EngineCommand>(512);
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop_flag);
    let glitch2 = Arc::clone(&glitch_counter);

    let stop_event: HANDLE = unsafe {
        CreateEventW(None, false, false, None)
            .map_err(|e| SphereAudioError::StreamOpenFailed(format!("CreateEventW(stop): {e}")))?
    };
    let stop_event_usize = stop_event.0 as usize;

    let (info_tx, info_rx) = std::sync::mpsc::channel::<Result<(u32, u32, String), String>>();

    let t = thread::Builder::new()
        .name("daux-wasapi-excl".into())
        .spawn(move || unsafe {
            let stop_ev = HANDLE(stop_event_usize as *mut _);
            wasapi_thread(
                output_device_id,
                requested_sr,
                buf_frames,
                rx,
                shared,
                initial_runtime,
                glitch2,
                stop2,
                stop_ev,
                info_tx,
            );
        })
        .map_err(|e| {
            unsafe {
                let _ = CloseHandle(stop_event);
            }
            SphereAudioError::StreamOpenFailed(e.to_string())
        })?;

    let (sample_rate, buffer_size, device_name) = info_rx
        .recv_timeout(std::time::Duration::from_secs(8))
        .map_err(|e| {
            // Timeout or channel disconnect (thread panicked before sending).
            SphereAudioError::StreamOpenFailed(format!("WASAPI Exclusive thread init failed: {e}"))
        })
        .and_then(|r| r.map_err(SphereAudioError::StreamOpenFailed))?;

    Ok(WasapiExclusiveHandle {
        cmd_tx: tx,
        sample_rate,
        buffer_size,
        device_name,
        stop_flag,
        stop_event,
        thread: Some(t),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Audio thread
// ─────────────────────────────────────────────────────────────────────────────

unsafe fn wasapi_thread(
    device_id: Option<String>,
    requested_sr: Option<u32>,
    buf_frames: u32,
    cmd_rx: Receiver<EngineCommand>,
    shared: Arc<SharedState>,
    initial_runtime: RuntimeProject,
    glitch_counter: Arc<AtomicU64>,
    stop_flag: Arc<AtomicBool>,
    stop_event: HANDLE,
    info_tx: std::sync::mpsc::Sender<Result<(u32, u32, String), String>>,
) {
    // ── COM init ──────────────────────────────────────────────────────────────
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

    // ── MMCSS ─────────────────────────────────────────────────────────────────
    let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut task_idx = 0u32;
    let mmcss_h = AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut task_idx);
    if mmcss_h != 0 {
        eprintln!("[DAUx WASAPI Excl] MMCSS 'Pro Audio' set (index={task_idx})");
        shared.mmcss_active.store(true, Ordering::Relaxed);
    } else {
        eprintln!("[DAUx WASAPI Excl] MMCSS set failed (non-fatal)");
    }

    // ── Device enumerator ─────────────────────────────────────────────────────
    let enumerator: IMMDeviceEnumerator =
        match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            Ok(e) => e,
            Err(e) => {
                let _ = info_tx.send(Err(format!("CoCreateInstance(IMMDeviceEnumerator): {e}")));
                cleanup_mmcss(mmcss_h);
                CoUninitialize();
                return;
            }
        };

    let device: IMMDevice = match resolve_device(&enumerator, device_id.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            let _ = info_tx.send(Err(e));
            cleanup_mmcss(mmcss_h);
            CoUninitialize();
            return;
        }
    };

    let device_name = get_device_friendly_name(&device);
    eprintln!("[DAUx WASAPI Excl] Opening: {device_name}");

    // ── Open exclusive stream with format negotiation ─────────────────────────
    let _stream_completed = open_exclusive_stream(
        &device,
        requested_sr,
        buf_frames,
        &shared,
        &info_tx,
        &glitch_counter,
        &stop_flag,
        stop_event,
        &cmd_rx,
        initial_runtime,
        &device_name,
        mmcss_h,
    );

    cleanup_mmcss(mmcss_h);
    shared.mmcss_active.store(false, Ordering::Relaxed);
    CoUninitialize();
}

/// Opens the WASAPI exclusive stream, negotiates format and period, runs the
/// render loop, and handles BUFFER_SIZE_NOT_ALIGNED retry.
///
/// Returns `false` if initialization failed (error already sent on `info_tx`).
/// Returns `true` when the stream ran to completion (normal shutdown).
unsafe fn open_exclusive_stream(
    device: &IMMDevice,
    requested_sr: Option<u32>,
    buf_frames: u32,
    shared: &Arc<SharedState>,
    info_tx: &std::sync::mpsc::Sender<Result<(u32, u32, String), String>>,
    glitch_counter: &Arc<AtomicU64>,
    stop_flag: &Arc<AtomicBool>,
    stop_event: HANDLE,
    cmd_rx: &Receiver<EngineCommand>,
    initial_runtime: RuntimeProject,
    device_name: &str,
    _mmcss_h: isize,
) -> bool {
    // ── IAudioClient ──────────────────────────────────────────────────────────
    let client: IAudioClient = match device.Activate(CLSCTX_ALL, None) {
        Ok(c) => c,
        Err(e) => {
            let _ = info_tx.send(Err(format!("Activate(IAudioClient): {e}")));
            return false;
        }
    };

    // ── Mix format (device's native format) ───────────────────────────────────
    let mix_fmt = match client.GetMixFormat() {
        Ok(p) => p,
        Err(e) => {
            let _ = info_tx.send(Err(format!("GetMixFormat: {e}")));
            return false;
        }
    };
    // Safety: GetMixFormat returns a valid pointer allocated by CoTaskMemAlloc.
    if mix_fmt.is_null() {
        let _ = info_tx.send(Err("GetMixFormat returned null".into()));
        return false;
    }

    let native_sr = (*mix_fmt).nSamplesPerSec.max(1);
    let device_ch = (*mix_fmt).nChannels as usize;
    let block_align = (*mix_fmt).nBlockAlign as u32;

    // Negotiate the actual exclusive-mode sample rate. The device mix format is
    // initialized as-is unless a *different* rate was requested — in which case
    // it is only honoured when the device supports it in exclusive mode; we fall
    // back to the device's native rate otherwise. `sample_rate` is the rate
    // reported to the engine, so it must always equal the rate the hardware
    // actually runs at — storing the requested rate while initializing the device
    // at the native rate (the previous behaviour) desyncs transport/tempo/pitch.
    // Stamping `nSamplesPerSec` requires updating `nAvgBytesPerSec` too or some
    // drivers reject the format.
    let mut sample_rate = requested_sr.unwrap_or(native_sr).max(1);
    if sample_rate != native_sr {
        (*mix_fmt).nSamplesPerSec = sample_rate;
        (*mix_fmt).nAvgBytesPerSec = sample_rate.saturating_mul(block_align);
        let probe = client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, mix_fmt, None);
        if !probe.is_ok() {
            eprintln!(
                "[DAUx WASAPI Excl] Requested {sample_rate} Hz unsupported in exclusive mode — using device rate {native_sr} Hz"
            );
            sample_rate = native_sr;
            (*mix_fmt).nSamplesPerSec = native_sr;
            (*mix_fmt).nAvgBytesPerSec = native_sr.saturating_mul(block_align);
        }
    }

    // ── Query device periods ───────────────────────────────────────────────────
    // hnsMinimumDevicePeriod is the minimum exclusive-mode period.
    let mut _default_period: i64 = 0;
    let mut min_period_hns: i64 = 0;
    let _ = client.GetDevicePeriod(Some(&mut _default_period), Some(&mut min_period_hns));
    // Compute HNS period from the buffer size at the rate we will actually run at.
    let requested_hns = (buf_frames as i64 * 10_000_000i64) / sample_rate as i64;
    let hns = requested_hns.max(min_period_hns.max(1));

    // ── Check exclusive format support ────────────────────────────────────────
    // IsFormatSupported returns an HRESULT directly (not a Result).
    // For exclusive mode ppClosestMatch MUST be null.
    let fmt_hr = client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, mix_fmt, None);
    if !fmt_hr.is_ok() {
        let _ = info_tx.send(Err(classify_hresult(fmt_hr.0, "IsFormatSupported")));
        windows::Win32::System::Com::CoTaskMemFree(Some(mix_fmt as *const _ as *const _));
        return false;
    }

    // ── Initialize IAudioClient (exclusive event-driven) ──────────────────────
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_NOPERSIST;

    let init_result =
        client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, hns, hns, mix_fmt, None);

    // Handle BUFFER_SIZE_NOT_ALIGNED: re-create client with driver-aligned period.
    let client = match init_result {
        Ok(()) => client,
        Err(e) if e.code().0 == E_AUDCLNT_BUFFER_SIZE_NOT_ALIGNED => {
            eprintln!("[DAUx WASAPI Excl] Buffer size not aligned — querying aligned size");
            // GetBufferSize() after the failed Initialize returns the aligned frame count.
            let aligned_frames = match client.GetBufferSize() {
                Ok(f) => f,
                Err(e2) => {
                    let _ = info_tx.send(Err(format!(
                        "WASAPI Exclusive: buffer not aligned; GetBufferSize failed: {e2}"
                    )));
                    windows::Win32::System::Com::CoTaskMemFree(Some(
                        mix_fmt as *const _ as *const _,
                    ));
                    return false;
                }
            };
            let aligned_hns = ((aligned_frames as i64) * 10_000_000i64) / sample_rate as i64;
            eprintln!(
                "[DAUx WASAPI Excl] Aligned buffer: {aligned_frames} frames, {aligned_hns} hns"
            );
            // Per Windows docs: release and re-create the IAudioClient before retry.
            drop(client);
            let client2: IAudioClient = match device.Activate(CLSCTX_ALL, None) {
                Ok(c) => c,
                Err(e2) => {
                    let _ = info_tx.send(Err(format!(
                        "WASAPI Exclusive: Re-Activate for alignment retry failed: {e2}"
                    )));
                    windows::Win32::System::Com::CoTaskMemFree(Some(
                        mix_fmt as *const _ as *const _,
                    ));
                    return false;
                }
            };
            if let Err(e2) = client2.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                flags,
                aligned_hns,
                aligned_hns,
                mix_fmt,
                None,
            ) {
                let msg = classify_hresult(e2.code().0, "Initialize (aligned retry)");
                let _ = info_tx.send(Err(msg));
                windows::Win32::System::Com::CoTaskMemFree(Some(mix_fmt as *const _ as *const _));
                return false;
            }
            client2
        }
        Err(e) => {
            let msg = classify_hresult(e.code().0, "Initialize");
            let _ = info_tx.send(Err(msg));
            windows::Win32::System::Com::CoTaskMemFree(Some(mix_fmt as *const _ as *const _));
            return false;
        }
    };

    windows::Win32::System::Com::CoTaskMemFree(Some(mix_fmt as *const _ as *const _));

    // ── Actual buffer size ─────────────────────────────────────────────────────
    let actual_buf = match client.GetBufferSize() {
        Ok(f) => f,
        Err(e) => {
            let _ = info_tx.send(Err(format!("GetBufferSize: {e}")));
            return false;
        }
    };

    // ── Buffer-ready event ────────────────────────────────────────────────────
    let buf_event: HANDLE = match CreateEventW(None, false, false, None) {
        Ok(h) => h,
        Err(e) => {
            let _ = info_tx.send(Err(format!("CreateEventW(buf): {e}")));
            return false;
        }
    };

    if let Err(e) = client.SetEventHandle(buf_event) {
        let _ = info_tx.send(Err(format!("SetEventHandle: {e}")));
        let _ = CloseHandle(buf_event);
        return false;
    }

    // ── IAudioRenderClient ────────────────────────────────────────────────────
    let render: IAudioRenderClient = match client.GetService() {
        Ok(s) => s,
        Err(e) => {
            let _ = info_tx.send(Err(format!("GetService(IAudioRenderClient): {e}")));
            let _ = CloseHandle(buf_event);
            return false;
        }
    };

    if let Err(e) = client.Start() {
        let _ = info_tx.send(Err(format!("IAudioClient::Start: {e}")));
        let _ = CloseHandle(buf_event);
        return false;
    }

    shared.sample_rate.store(sample_rate, Ordering::Relaxed);
    let _ = info_tx.send(Ok((sample_rate, actual_buf, device_name.to_string())));
    eprintln!(
        "[DAUx WASAPI Excl] Stream ready: device='{}' sr={} buf={} ch={}",
        device_name, sample_rate, actual_buf, device_ch
    );

    // ── Runtime ───────────────────────────────────────────────────────────────
    let mut runtime = initial_runtime;
    runtime.retarget_sample_rate(sample_rate);
    let mut local = LocalAudioState::with_monitor_capacity(sample_rate as f64, actual_buf as usize);
    let mut scratch = vec![0.0f32; actual_buf as usize * device_ch];

    // ── Render loop ───────────────────────────────────────────────────────────
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        let wait_handles = [buf_event, stop_event];
        let wait = WaitForMultipleObjects(&wait_handles, false, 2000);
        if wait == WAIT_OBJECT_0 {
            // buf_event signaled — fall through to render below.
        } else {
            // Index 1 = stop_event. Timeout or other = glitch. An unexpected
            // exit while not stopping means the device went away — flag it for
            // the control thread to surface DeviceLost and recover.
            if wait.0 != 1 && !stop_flag.load(Ordering::Relaxed) {
                glitch_counter.fetch_add(1, Ordering::Relaxed);
                shared.device_lost.store(true, Ordering::Relaxed);
            }
            break;
        }
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        drain_commands(cmd_rx, &mut runtime, shared, &mut local, sample_rate);

        let padding = client.GetCurrentPadding().unwrap_or(actual_buf);
        let frames = actual_buf.saturating_sub(padding);
        if frames == 0 {
            continue;
        }

        let buf_ptr = match render.GetBuffer(frames) {
            Ok(p) => p,
            Err(_) => {
                glitch_counter.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        let out_len = frames as usize * device_ch;
        // `scratch` is preallocated to `actual_buf * device_ch` and `frames`
        // can never exceed `actual_buf` (`saturating_sub` above), so `out_len`
        // never exceeds `scratch.len()` here. Clamp defensively instead of
        // growing — growing would allocate on the audio thread.
        let total = out_len.min(scratch.len());
        let s = &mut scratch[..total];
        // `fill_output_f32` fully overwrites every sample of `s` on every
        // reachable path (see `backend/render.rs`) — no pre-zero needed.
        fill_output_f32(s, device_ch, &mut runtime, shared, &mut local);

        let out: &mut [f32] = std::slice::from_raw_parts_mut(buf_ptr as *mut f32, out_len);
        out[..total].copy_from_slice(s);
        if total < out_len {
            // Unreachable in practice (see above) — silence rather than leave
            // the tail of the exclusive-mode buffer holding stale samples.
            out[total..].fill(0.0);
        }

        if let Err(e) = render.ReleaseBuffer(frames, 0) {
            eprintln!("[DAUx WASAPI Excl] ReleaseBuffer: {e}");
            glitch_counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    let _ = client.Stop();
    let _ = CloseHandle(buf_event);
    eprintln!("[DAUx WASAPI Excl] Stopped: {device_name}");
    true
}

// ─────────────────────────────────────────────────────────────────────────────

unsafe fn resolve_device(
    enumerator: &IMMDeviceEnumerator,
    name: Option<&str>,
) -> Result<IMMDevice, String> {
    match name {
        None => enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|e| format!("GetDefaultAudioEndpoint: {e}")),
        Some(wanted) => {
            use windows::Win32::Media::Audio::IMMDeviceCollection;
            let coll: IMMDeviceCollection = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| format!("EnumAudioEndpoints: {e}"))?;
            let count = coll.GetCount().map_err(|e| format!("GetCount: {e}"))?;
            for i in 0..count {
                let dev = coll.Item(i).map_err(|e| format!("Item({i}): {e}"))?;
                if get_device_friendly_name(&dev) == wanted {
                    return Ok(dev);
                }
            }
            eprintln!("[DAUx WASAPI Excl] Device '{wanted}' not found, using default");
            enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .map_err(|e| format!("GetDefaultAudioEndpoint (fallback): {e}"))
        }
    }
}

unsafe fn get_device_friendly_name(device: &IMMDevice) -> String {
    use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::STGM_READ;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

    let store: IPropertyStore = match device.OpenPropertyStore(STGM_READ) {
        Ok(s) => s,
        Err(_) => return "Unknown Device".into(),
    };

    let key = &DEVPKEY_Device_FriendlyName as *const _ as *const PROPERTYKEY;
    let mut prop = match store.GetValue(key) {
        Ok(p) => p,
        Err(_) => return "Unknown Device".into(),
    };

    #[repr(C)]
    struct RawPropVariant {
        vt: u16,
        _pad: [u16; 3],
        pwsz: *mut u16,
    }
    let raw = &mut prop as *mut _ as *mut RawPropVariant;
    if (*raw).vt == 31 {
        let ptr = (*raw).pwsz;
        if !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let s = String::from_utf16_lossy(slice).to_string();
            windows::Win32::System::Com::CoTaskMemFree(Some(ptr as *const _));
            (*raw).pwsz = std::ptr::null_mut();
            (*raw).vt = 0; // VT_EMPTY
            return s;
        }
    }
    "Unknown Device".into()
}

unsafe fn cleanup_mmcss(handle: isize) {
    if handle != 0 {
        AvRevertMmThreadCharacteristics(handle);
    }
}
