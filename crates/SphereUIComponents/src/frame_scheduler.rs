//! Deterministic, display-synced frame scheduler.
//!
//! GPUI repaints on demand (a `cx.notify()` / `app.notify(id)` schedules one
//! frame); there is no continuous render loop. The only thing that drives
//! *continuous* repaints is the audio poll loop in
//! [`crate::layout`] (`spawn_audio_poll`), which historically slept a hardcoded
//! 16 ms (~60 Hz). This module replaces that with a cadence that is a **pure
//! function** of `(mode, detected refresh rate, frame class)`:
//!
//! * default to the monitor refresh rate ([`FrameRateMode::DisplaySync`]),
//! * offer fixed caps + a battery saver for settings/debug,
//! * never feed measured frame timing back into the interval, so the cadence
//!   cannot oscillate / jitter.
//!
//! Idle is unchanged: the poll loop only notifies on state change, so when
//! nothing is dirty no frames are scheduled regardless of the configured rate.
//!
//! The refresh rate is queried once from the OS and cached:
//! * Windows — `EnumDisplaySettingsW(...).dmDisplayFrequency`
//! * macOS — `CGDisplayModeGetRefreshRate`, then CoreVideo's nominal output
//!   period when the mode reports `0` (common on LCD / ProMotion panels)
//! * Linux — XRandR current rate (native X11 and XWayland); pure Wayland without
//!   XWayland falls back like any other query failure
//!
//! On any query failure it falls back to 60 Hz. The detected value is clamped to
//! `30..=240` Hz.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Frame rate behaviour. `DisplaySync` is the default and tracks the monitor
/// refresh rate; the fixed modes and battery saver are for settings/debug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FrameRateMode {
    /// Track the detected monitor refresh rate (clamped 30..=240 Hz).
    DisplaySync,
    Fixed60,
    Fixed120,
    Fixed144,
    /// As fast as the poll loop allows (1 ms floor). Debug only.
    Unlimited,
    /// 30 FPS, or refresh/2 when that is lower. Reduces power on laptops.
    BatterySaver,
}

impl Default for FrameRateMode {
    fn default() -> Self {
        FrameRateMode::DisplaySync
    }
}

impl FrameRateMode {
    pub fn label(self) -> &'static str {
        match self {
            FrameRateMode::DisplaySync => "Display Sync",
            FrameRateMode::Fixed60 => "60 FPS",
            FrameRateMode::Fixed120 => "120 FPS",
            FrameRateMode::Fixed144 => "144 FPS",
            FrameRateMode::Unlimited => "Unlimited",
            FrameRateMode::BatterySaver => "Battery Saver",
        }
    }

    /// All modes in display order (settings dropdown / round-trip tests).
    pub fn all() -> [FrameRateMode; 6] {
        [
            FrameRateMode::DisplaySync,
            FrameRateMode::Fixed60,
            FrameRateMode::Fixed120,
            FrameRateMode::Fixed144,
            FrameRateMode::Unlimited,
            FrameRateMode::BatterySaver,
        ]
    }

    /// Debug override via `FUTUREBOARD_FRAME_RATE_MODE`
    /// (`displaysync|fixed60|fixed120|fixed144|unlimited|battery`). Takes
    /// precedence over the persisted setting so a session can be pinned without
    /// touching the settings file.
    pub fn from_env() -> Option<FrameRateMode> {
        let raw = std::env::var("FUTUREBOARD_FRAME_RATE_MODE").ok()?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "displaysync" | "display" | "display-sync" | "refresh" => {
                Some(FrameRateMode::DisplaySync)
            }
            "fixed60" | "60" => Some(FrameRateMode::Fixed60),
            "fixed120" | "120" => Some(FrameRateMode::Fixed120),
            "fixed144" | "144" => Some(FrameRateMode::Fixed144),
            "unlimited" | "uncapped" | "max" => Some(FrameRateMode::Unlimited),
            "battery" | "batterysaver" | "battery-saver" | "saver" => {
                Some(FrameRateMode::BatterySaver)
            }
            other => {
                eprintln!(
                    "[frame-scheduler] ignoring unknown FUTUREBOARD_FRAME_RATE_MODE='{other}'"
                );
                None
            }
        }
    }
}

/// What kind of work a scheduled frame serves. Meters follow the display
/// cadence so high-refresh monitors animate smoothly; background work keeps a
/// lower cap so progress UI cannot drive continuous full-rate repainting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// Playback playhead, drag, scroll, zoom, active animations.
    Continuous,
    /// Meter / VU updates. Follows the continuous display cadence.
    Meter,
    /// Progress bars / background jobs. Capped to 30 Hz, region-invalidation.
    Background,
}

const MIN_REFRESH_HZ: u32 = 30;
const MAX_REFRESH_HZ: u32 = 240;
const FALLBACK_REFRESH_HZ: u32 = 60;
const BACKGROUND_CAP_HZ: u32 = 30;
/// Ceiling `DisplaySync` uses on a machine with no discrete GPU. A 120/144 Hz
/// panel driven by integrated graphics spends the extra frames on memory
/// bandwidth the CPU is also competing for, and a DAW's continuous motion —
/// playhead, meters, scroll — reads the same at 60. Only the automatic mode is
/// capped: picking a fixed rate in Settings is an explicit choice and wins.
const INTEGRATED_GPU_CAP_HZ: u32 = 60;
/// Floor for `Unlimited` so the poll loop never busy-spins.
const UNLIMITED_FLOOR: Duration = Duration::from_millis(1);

#[inline]
fn hz_to_interval(hz: u32) -> Duration {
    Duration::from_nanos(1_000_000_000u64 / hz.max(1) as u64)
}

/// The refresh rate `mode` should actually pace against on `class` hardware.
///
/// Pure policy, so it can be tested without a GPU: only `DisplaySync` on an
/// integrated-only machine is capped, and only downward — a 60 Hz panel is
/// already at the cap, and every explicit mode passes through untouched.
pub fn effective_refresh_hz(
    mode: FrameRateMode,
    refresh_hz: u32,
    class: crate::perf::GpuClass,
) -> u32 {
    if mode == FrameRateMode::DisplaySync && class == crate::perf::GpuClass::IntegratedOnly {
        refresh_hz.min(INTEGRATED_GPU_CAP_HZ)
    } else {
        refresh_hz
    }
}

/// Clamp a raw OS-reported refresh rate. `0` / `1` are the Windows "use
/// hardware default" sentinels and map to the fallback.
pub fn clamp_refresh(raw: u32) -> u32 {
    if raw <= 1 {
        FALLBACK_REFRESH_HZ
    } else {
        raw.clamp(MIN_REFRESH_HZ, MAX_REFRESH_HZ)
    }
}

/// Pure cadence function: the only inputs are the mode, the (already clamped)
/// refresh rate, and the frame class. No measured-timing feedback, so the
/// returned interval is stable for a given configuration.
pub fn frame_interval(mode: FrameRateMode, refresh_hz: u32, class: FrameClass) -> Duration {
    let refresh_hz = refresh_hz.clamp(MIN_REFRESH_HZ, MAX_REFRESH_HZ);
    let continuous = match mode {
        FrameRateMode::DisplaySync => hz_to_interval(refresh_hz),
        FrameRateMode::Fixed60 => hz_to_interval(60),
        FrameRateMode::Fixed120 => hz_to_interval(120),
        FrameRateMode::Fixed144 => hz_to_interval(144),
        FrameRateMode::Unlimited => UNLIMITED_FLOOR,
        // 30 FPS, or refresh/2 when that is slower (larger interval).
        FrameRateMode::BatterySaver => {
            hz_to_interval(BACKGROUND_CAP_HZ).max(hz_to_interval(refresh_hz) * 2)
        }
    };
    match class {
        FrameClass::Continuous => continuous,
        // Meters are lightweight region repaints and should match display
        // refresh instead of stepping at a fixed 60/30 Hz.
        FrameClass::Meter => continuous,
        FrameClass::Background => continuous.max(hz_to_interval(BACKGROUND_CAP_HZ)),
    }
}

/// Query the primary monitor refresh rate once and cache it. Clamped to
/// `30..=240`; falls back to 60 Hz on any query failure.
pub fn detect_refresh_hz() -> u32 {
    static CACHED: OnceLock<u32> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let raw = query_refresh_hz_os().unwrap_or(FALLBACK_REFRESH_HZ);
        let hz = clamp_refresh(raw);
        if frame_diag_enabled() {
            eprintln!("[frame-scheduler] detected refresh raw={raw}Hz -> clamped {hz}Hz");
        }
        hz
    })
}

/// Round a floating OS-reported refresh rate to an integer Hz. Values under
/// 1.0 (Windows "hardware default", CGDisplay LCD sentinel, XRandR zero) are
/// treated as unavailable so callers can try the next source.
pub fn round_refresh_hz(raw: f64) -> Option<u32> {
    if !raw.is_finite() || raw < 1.0 {
        None
    } else {
        // 59.94 / 119.88 must map to 60 / 120 so DisplaySync intervals land on
        // whole-frame boundaries rather than drifting.
        Some(raw.round() as u32)
    }
}

#[cfg(windows)]
fn query_refresh_hz_os() -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
    let mut devmode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // NULL device name → the current display device on the calling thread
    // (i.e. the primary monitor for the app).
    let ok = unsafe { EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut devmode) };
    if ok.as_bool() {
        Some(devmode.dmDisplayFrequency)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn query_refresh_hz_os() -> Option<u32> {
    // Prefer the active display mode when it reports a real rate. LCD and
    // ProMotion panels often return 0 from CGDisplayModeGetRefreshRate, so
    // fall through to CoreVideo's nominal output period which still yields a
    // usable cadence for DisplaySync.
    type CGDirectDisplayID = u32;
    type CGDisplayModeRef = *mut std::ffi::c_void;
    type CVDisplayLinkRef = *mut std::ffi::c_void;

    #[repr(C)]
    struct CVTime {
        time_value: i64,
        time_scale: i32,
        flags: i32,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> CGDirectDisplayID;
        fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
        fn CGDisplayModeGetRefreshRate(mode: CGDisplayModeRef) -> f64;
        fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    }

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        fn CVDisplayLinkCreateWithCGDisplay(
            display_id: CGDirectDisplayID,
            display_link_out: *mut CVDisplayLinkRef,
        ) -> i32;
        fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(display_link: CVDisplayLinkRef)
        -> CVTime;
        fn CVDisplayLinkRelease(display_link: CVDisplayLinkRef);
    }

    unsafe {
        let display = CGMainDisplayID();
        let mode = CGDisplayCopyDisplayMode(display);
        if !mode.is_null() {
            let rate = CGDisplayModeGetRefreshRate(mode);
            CGDisplayModeRelease(mode);
            if let Some(hz) = round_refresh_hz(rate) {
                return Some(hz);
            }
        }

        let mut link: CVDisplayLinkRef = std::ptr::null_mut();
        // kCVReturnSuccess == 0
        if CVDisplayLinkCreateWithCGDisplay(display, &mut link) != 0 || link.is_null() {
            return None;
        }
        let period = CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link);
        CVDisplayLinkRelease(link);
        if period.time_value <= 0 || period.time_scale <= 0 {
            return None;
        }
        let hz = period.time_scale as f64 / period.time_value as f64;
        round_refresh_hz(hz)
    }
}

#[cfg(target_os = "linux")]
fn query_refresh_hz_os() -> Option<u32> {
    // XRandR covers native X11 and Wayland sessions that still expose XWayland
    // (`DISPLAY` set). Pure Wayland without XWayland returns None → 60 Hz
    // fallback, matching the BPM cursor path.
    use x11_dl::xlib::{self, Display};
    use x11_dl::xrandr;

    let xlib = xlib::Xlib::open().ok()?;
    let xrandr = xrandr::Xrandr::open().ok()?;
    unsafe {
        let display: *mut Display = (xlib.XOpenDisplay)(std::ptr::null());
        if display.is_null() {
            return None;
        }
        let root = (xlib.XDefaultRootWindow)(display);

        // Prefer mode-table rate from the primary (or first connected) output —
        // more accurate than the legacy screen-config rate on multi-monitor.
        let hz = refresh_hz_from_xrandr_resources(&xrandr, display, root)
            .or_else(|| refresh_hz_from_xrandr_screen_config(&xrandr, display, root));

        (xlib.XCloseDisplay)(display);
        hz
    }
}

#[cfg(target_os = "linux")]
unsafe fn refresh_hz_from_xrandr_screen_config(
    xrandr: &x11_dl::xrandr::Xrandr,
    display: *mut x11_dl::xlib::Display,
    root: x11_dl::xlib::Window,
) -> Option<u32> {
    let config = unsafe { (xrandr.XRRGetScreenInfo)(display, root) };
    if config.is_null() {
        return None;
    }
    let rate = unsafe { (xrandr.XRRConfigCurrentRate)(config) };
    unsafe { (xrandr.XRRFreeScreenConfigInfo)(config) };
    round_refresh_hz(rate as f64)
}

#[cfg(target_os = "linux")]
unsafe fn refresh_hz_from_xrandr_resources(
    xrandr: &x11_dl::xrandr::Xrandr,
    display: *mut x11_dl::xlib::Display,
    root: x11_dl::xlib::Window,
) -> Option<u32> {
    use x11_dl::xrandr::{RRMode, RR_Connected, XRRModeInfo};

    let resources = unsafe { (xrandr.XRRGetScreenResourcesCurrent)(display, root) };
    if resources.is_null() {
        return None;
    }

    let primary = unsafe { (xrandr.XRRGetOutputPrimary)(display, root) };
    let noutput = unsafe { (*resources).noutput };
    let outputs = unsafe { (*resources).outputs };
    if noutput <= 0 || outputs.is_null() {
        unsafe { (xrandr.XRRFreeScreenResources)(resources) };
        return None;
    }

    let mut chosen_mode: RRMode = 0;
    // Prefer primary when connected; otherwise first connected output with a CRTC.
    let order = std::iter::once(primary).chain((0..noutput).map(|i| unsafe { *outputs.add(i as usize) }));
    for output in order {
        if output == 0 {
            continue;
        }
        let info = unsafe { (xrandr.XRRGetOutputInfo)(display, resources, output) };
        if info.is_null() {
            continue;
        }
        let connected = unsafe { (*info).connection } == RR_Connected as u16;
        let crtc = unsafe { (*info).crtc };
        unsafe { (xrandr.XRRFreeOutputInfo)(info) };
        if !connected || crtc == 0 {
            continue;
        }
        let crtc_info = unsafe { (xrandr.XRRGetCrtcInfo)(display, resources, crtc) };
        if crtc_info.is_null() {
            continue;
        }
        let mode = unsafe { (*crtc_info).mode };
        unsafe { (xrandr.XRRFreeCrtcInfo)(crtc_info) };
        if mode != 0 {
            chosen_mode = mode;
            break;
        }
    }

    let mut hz = None;
    if chosen_mode != 0 {
        let nmode = unsafe { (*resources).nmode };
        let modes = unsafe { (*resources).modes };
        if nmode > 0 && !modes.is_null() {
            for i in 0..nmode as usize {
                let mode: &XRRModeInfo = unsafe { &*modes.add(i) };
                if mode.id == chosen_mode {
                    hz = refresh_hz_from_xrr_mode(mode);
                    break;
                }
            }
        }
    }

    unsafe { (xrandr.XRRFreeScreenResources)(resources) };
    hz
}

/// Compute Hz from an XRRModeInfo: `dotClock / (hTotal * vTotal)`.
#[cfg(target_os = "linux")]
fn refresh_hz_from_xrr_mode(mode: &x11_dl::xrandr::XRRModeInfo) -> Option<u32> {
    let denom = (mode.hTotal as u64).saturating_mul(mode.vTotal as u64);
    if denom == 0 || mode.dotClock == 0 {
        return None;
    }
    // dotClock is kHz in XRandR.
    let hz = (mode.dotClock as f64 * 1000.0) / denom as f64;
    round_refresh_hz(hz)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn query_refresh_hz_os() -> Option<u32> {
    None
}

fn frame_diag_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_FRAME_DIAG").is_some())
}

/// Live scheduler used by the layout. Holds the resolved mode + cached refresh
/// rate and publishes the continuous interval through a lock-free `AtomicU64`
/// (nanoseconds) the detached poll loop reads each tick without locking the
/// entity.
pub struct FrameScheduler {
    mode: FrameRateMode,
    refresh_hz: u32,
    /// Set when `FUTUREBOARD_FRAME_RATE_MODE` is present; overrides settings.
    env_override: Option<FrameRateMode>,
    continuous_nanos: Arc<AtomicU64>,
}

impl FrameScheduler {
    pub fn new(settings_mode: FrameRateMode) -> Self {
        let refresh_hz = detect_refresh_hz();
        let env_override = FrameRateMode::from_env();
        let mode = env_override.unwrap_or(settings_mode);
        let continuous_nanos = Arc::new(AtomicU64::new(
            frame_interval(
                mode,
                effective_refresh_hz(mode, refresh_hz, crate::perf::gpu_class()),
                FrameClass::Continuous,
            )
            .as_nanos() as u64,
        ));
        let scheduler = Self {
            mode,
            refresh_hz,
            env_override,
            continuous_nanos,
        };
        if frame_diag_enabled() {
            eprintln!(
                "[frame-scheduler] init {} (continuous {:.2}ms, meter {:.2}ms, background {:.2}ms)",
                scheduler.describe(),
                scheduler.continuous_interval().as_secs_f32() * 1000.0,
                scheduler.meter_min_interval().as_secs_f32() * 1000.0,
                scheduler.background_interval().as_secs_f32() * 1000.0,
            );
        }
        scheduler
    }

    /// Lock-free handle to the continuous interval (nanoseconds) for the poll
    /// loop. The loop reads this each iteration so a mode change applies on the
    /// next tick.
    pub fn continuous_nanos_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.continuous_nanos)
    }

    /// Re-resolve the mode from the latest persisted setting (env override still
    /// wins) and republish the continuous interval. Cheap — call from `render`.
    pub fn refresh_from_settings(&mut self, settings_mode: FrameRateMode) {
        let mode = self.env_override.unwrap_or(settings_mode);
        if mode != self.mode {
            self.mode = mode;
            self.continuous_nanos.store(
                frame_interval(mode, self.paced_refresh_hz(), FrameClass::Continuous).as_nanos()
                    as u64,
                Ordering::Relaxed,
            );
            if frame_diag_enabled() {
                eprintln!("[frame-scheduler] mode -> {}", self.describe());
            }
        }
    }

    pub fn mode(&self) -> FrameRateMode {
        self.mode
    }

    pub fn refresh_hz(&self) -> u32 {
        self.refresh_hz
    }

    /// The rate the scheduler actually paces against — the detected refresh,
    /// capped on integrated-only hardware while in `DisplaySync`.
    fn paced_refresh_hz(&self) -> u32 {
        effective_refresh_hz(self.mode, self.refresh_hz, crate::perf::gpu_class())
    }

    pub fn continuous_interval(&self) -> Duration {
        frame_interval(self.mode, self.paced_refresh_hz(), FrameClass::Continuous)
    }

    pub fn meter_min_interval(&self) -> Duration {
        frame_interval(self.mode, self.paced_refresh_hz(), FrameClass::Meter)
    }

    pub fn background_interval(&self) -> Duration {
        frame_interval(self.mode, self.paced_refresh_hz(), FrameClass::Background)
    }

    /// Effective continuous FPS for the HUD (e.g. `144` for DisplaySync@144).
    pub fn effective_fps(&self) -> u32 {
        let nanos = self.continuous_interval().as_nanos().max(1) as u64;
        (1_000_000_000u64 / nanos) as u32
    }

    /// Status-bar / log label, e.g. `"Display Sync 144Hz"`.
    pub fn describe(&self) -> String {
        match self.mode {
            FrameRateMode::DisplaySync => {
                let paced = self.paced_refresh_hz();
                if paced < self.refresh_hz {
                    // Say why the app is not running at panel rate, so a capped
                    // machine never looks like a bug.
                    format!(
                        "Display Sync {paced}Hz (iGPU cap, panel {}Hz)",
                        self.refresh_hz
                    )
                } else {
                    format!("Display Sync {paced}Hz")
                }
            }
            FrameRateMode::Unlimited => "Unlimited".to_string(),
            other => format!("{} ({} FPS)", other.label(), self.effective_fps()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }

    #[test]
    fn clamp_refresh_bounds_and_sentinels() {
        assert_eq!(clamp_refresh(10), 30, "below floor clamps up");
        assert_eq!(clamp_refresh(300), 240, "above ceiling clamps down");
        assert_eq!(clamp_refresh(0), 60, "0 is the fallback sentinel");
        assert_eq!(clamp_refresh(1), 60, "1 is the fallback sentinel");
        assert_eq!(clamp_refresh(144), 144);
        assert_eq!(clamp_refresh(30), 30);
        assert_eq!(clamp_refresh(240), 240);
    }

    #[test]
    fn display_sync_is_capped_on_integrated_only_hardware() {
        use crate::perf::GpuClass;
        let sync = FrameRateMode::DisplaySync;
        assert_eq!(
            effective_refresh_hz(sync, 144, GpuClass::IntegratedOnly),
            60,
            "a 144Hz panel on an iGPU paces at the cap"
        );
        assert_eq!(
            effective_refresh_hz(sync, 60, GpuClass::IntegratedOnly),
            60,
            "the cap never raises a slower panel"
        );
        assert_eq!(effective_refresh_hz(sync, 30, GpuClass::IntegratedOnly), 30);
    }

    #[test]
    fn discrete_hardware_and_explicit_modes_are_never_capped() {
        use crate::perf::GpuClass;
        assert_eq!(
            effective_refresh_hz(FrameRateMode::DisplaySync, 144, GpuClass::Discrete),
            144
        );
        assert_eq!(
            effective_refresh_hz(FrameRateMode::DisplaySync, 144, GpuClass::Unknown),
            144,
            "undetected hardware keeps panel rate"
        );
        // Choosing a fixed rate in Settings is an explicit decision.
        assert_eq!(
            effective_refresh_hz(FrameRateMode::Fixed144, 144, GpuClass::IntegratedOnly),
            144
        );
        assert_eq!(
            effective_refresh_hz(FrameRateMode::Unlimited, 240, GpuClass::IntegratedOnly),
            240
        );
    }

    #[test]
    fn continuous_interval_per_mode() {
        let near = |a: Duration, want_ms: f64| (ms(a) - want_ms).abs() < 0.2;
        assert!(near(
            frame_interval(FrameRateMode::DisplaySync, 144, FrameClass::Continuous),
            1000.0 / 144.0
        ));
        assert!(near(
            frame_interval(FrameRateMode::DisplaySync, 60, FrameClass::Continuous),
            1000.0 / 60.0
        ));
        assert!(near(
            frame_interval(FrameRateMode::Fixed60, 144, FrameClass::Continuous),
            1000.0 / 60.0
        ));
        assert!(near(
            frame_interval(FrameRateMode::Fixed120, 60, FrameClass::Continuous),
            1000.0 / 120.0
        ));
        assert!(near(
            frame_interval(FrameRateMode::Fixed144, 60, FrameClass::Continuous),
            1000.0 / 144.0
        ));
        assert_eq!(
            frame_interval(FrameRateMode::Unlimited, 240, FrameClass::Continuous),
            UNLIMITED_FLOOR
        );
    }

    #[test]
    fn meter_follows_continuous_display_cadence() {
        // 144 Hz DisplaySync: meters follow continuous refresh (~6.9ms).
        let meter = frame_interval(FrameRateMode::DisplaySync, 144, FrameClass::Meter);
        assert!(
            (ms(meter) - 1000.0 / 144.0).abs() < 0.2,
            "meter was {}ms",
            ms(meter)
        );
        // Battery saver continuous (~33ms) still slows meters with the rest of the UI.
        let bs_meter = frame_interval(FrameRateMode::BatterySaver, 144, FrameClass::Meter);
        assert!(
            (ms(bs_meter) - 1000.0 / 30.0).abs() < 0.2,
            "bs meter was {}ms",
            ms(bs_meter)
        );
    }

    #[test]
    fn background_capped_at_30() {
        for mode in FrameRateMode::all() {
            let bg = frame_interval(mode, 144, FrameClass::Background);
            assert!(
                ms(bg) >= 1000.0 / 30.0 - 0.2,
                "{mode:?} background {}ms too fast",
                ms(bg)
            );
        }
    }

    #[test]
    fn battery_saver_is_30_or_refresh_over_two() {
        // 144 Hz: 30 FPS dominates (33ms > 13.9ms).
        let at_144 = frame_interval(FrameRateMode::BatterySaver, 144, FrameClass::Continuous);
        assert!((ms(at_144) - 1000.0 / 30.0).abs() < 0.2);
        // 50 Hz: refresh/2 = 25 FPS dominates (40ms > 33ms).
        let at_50 = frame_interval(FrameRateMode::BatterySaver, 50, FrameClass::Continuous);
        assert!(
            (ms(at_50) - 1000.0 / 25.0).abs() < 0.5,
            "bs@50 was {}ms",
            ms(at_50)
        );
    }

    #[test]
    fn detect_is_clamped_and_nonzero() {
        let hz = detect_refresh_hz();
        assert!((MIN_REFRESH_HZ..=MAX_REFRESH_HZ).contains(&hz));
    }

    #[test]
    fn round_refresh_maps_fractional_panel_rates() {
        assert_eq!(round_refresh_hz(59.94), Some(60));
        assert_eq!(round_refresh_hz(119.88), Some(120));
        assert_eq!(round_refresh_hz(144.0), Some(144));
        assert_eq!(round_refresh_hz(0.0), None, "LCD/ProMotion sentinel");
        assert_eq!(round_refresh_hz(-1.0), None);
        assert_eq!(round_refresh_hz(f64::NAN), None);
    }
}
