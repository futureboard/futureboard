//! Application startup state machine and lightweight boot tasks.
//!
//! Splash covers early boot only — no VST scanning, no project restore I/O.
//! Heavy session loads use [`crate::loading_session`] later.

use std::path::PathBuf;
use std::time::Duration;

use gpui::AsyncApp;

use crate::paths::FutureboardPaths;
use crate::project::RecentProjectsStore;
use crate::settings::SettingsSchema;

/// Where the app should route once splash boot completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupRoute {
    Welcome,
    /// Blank unsaved studio workspace (start screen disabled, no restore target).
    EmptyWorkspace,
    OpenProject(PathBuf),
    RestoreLastProject(PathBuf),
}

/// Coarse startup phases for logging and future splash status hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupPhase {
    Starting,
    LoadingConfig,
    LoadingTheme,
    PreparingUserData,
    ResolvingStartupRoute,
    OpeningWelcome,
    OpeningStudio,
    Done,
}

impl StartupPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::LoadingConfig => "Loading configuration",
            Self::LoadingTheme => "Loading theme",
            Self::PreparingUserData => "Preparing user data",
            Self::ResolvingStartupRoute => "Resolving startup route",
            Self::OpeningWelcome => "Opening welcome",
            Self::OpeningStudio => "Opening studio",
            Self::Done => "Done",
        }
    }
}

/// Resolved boot destination plus whether the Welcome start screen is enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupPlan {
    pub route: StartupRoute,
    /// When false the app skips Welcome and opens an empty studio workspace.
    pub show_welcome_screen: bool,
}

impl StartupPlan {
    pub fn resolve() -> Self {
        let schema = SettingsSchema::load_from_disk();
        let show_welcome_screen = schema.general.show_start_screen;
        let route = if show_welcome_screen {
            StartupRoute::Welcome
        } else if let Some(path) = restore_last_project_candidate() {
            StartupRoute::RestoreLastProject(path)
        } else {
            StartupRoute::EmptyWorkspace
        };
        Self {
            route,
            show_welcome_screen,
        }
    }
}

fn restore_last_project_candidate() -> Option<PathBuf> {
    let mut recent = RecentProjectsStore::load();
    recent.refresh_missing();
    recent
        .entries()
        .iter()
        .find(|entry| !entry.missing)
        .map(|entry| entry.path.clone())
}

pub fn log_startup_phase(phase: StartupPhase) {
    crate::boot::log(&format!("startup phase: {}", phase.label()));
}

/// Result of the startup GPU enumeration.
#[derive(Debug, Clone)]
pub struct GpuProbe {
    /// Names of detected hardware GPUs (software/CPU adapters excluded).
    pub devices: Vec<String>,
    /// One-line status suitable for the splash screen.
    pub summary: String,
    pub has_gpu: bool,
}

/// Enumerate GPU adapters (wgpu) and record availability for the audio stack so
/// stem extraction can automatically prefer GPU inference. Safe to call without
/// the `gpu-renderer` feature (returns "no GPU"). Never panics — enumeration is
/// already `catch_unwind`-guarded.
pub fn probe_gpus() -> GpuProbe {
    let devices: Vec<String> = crate::components::timeline::render::list_available_gpu_devices()
        .into_iter()
        // wgpu reports software/WARP/llvmpipe fallbacks as `Cpu`; exclude those.
        .filter(|d| d.device_type.as_deref() != Some("Cpu"))
        .map(|d| d.name)
        .collect();
    let has_gpu = !devices.is_empty();

    // Authoritative signal for `SphereAudioProcessor::gpu_available()`.
    SphereAudioProcessor::set_gpu_detected(has_gpu);

    let summary = if has_gpu {
        format!("GPU: {}", devices.join(", "))
    } else {
        "GPU: none detected — using CPU".to_string()
    };
    crate::boot::log(&format!("startup GPU probe: {summary}"));
    GpuProbe {
        devices,
        summary,
        has_gpu,
    }
}

/// Lightweight boot work shared by Welcome and direct-to-studio launches.
/// Runs while the splash window is visible.
pub async fn run_lightweight_boot(cx: &mut AsyncApp) -> StartupPlan {
    let executor = cx.background_executor().clone();

    log_startup_phase(StartupPhase::Starting);
    executor.timer(Duration::from_millis(1)).await;

    log_startup_phase(StartupPhase::LoadingConfig);
    let _schema = SettingsSchema::load_from_disk();
    executor.timer(Duration::from_millis(40)).await;

    log_startup_phase(StartupPhase::LoadingTheme);
    executor.timer(Duration::from_millis(40)).await;

    log_startup_phase(StartupPhase::PreparingUserData);
    let paths = FutureboardPaths::resolve();
    let _ = paths.ensure_user_dirs();
    let mut recent = RecentProjectsStore::load();
    recent.refresh_missing();
    executor.timer(Duration::from_millis(40)).await;

    log_startup_phase(StartupPhase::ResolvingStartupRoute);
    let plan = StartupPlan::resolve();
    executor.timer(Duration::from_millis(40)).await;

    crate::boot::log("[Startup] phase=ScanAudio");
    executor
        .spawn(async {
            crate::device_registry::scan_audio();
        })
        .await;

    crate::boot::log("[Startup] phase=ScanMidi");
    executor
        .spawn(async {
            crate::device_registry::scan_midi_resilient();
        })
        .await;
    if crate::device_registry::cached_midi_devices().is_empty() {
        // USB class drivers and platform MIDI services can appear after the
        // splash scan has completed. Retry once after startup without delaying
        // Welcome/Studio; the regular hardware sync will consume the new cache.
        let retry_timer = executor.clone();
        executor
            .spawn(async move {
                retry_timer.timer(Duration::from_secs(2)).await;
                crate::device_registry::scan_midi_resilient();
            })
            .detach();
    }

    executor.timer(Duration::from_millis(80)).await;
    cx.update(|_app| {
        let warm = crate::layout::warm_up_renderer_status();
        crate::boot::log(&format!(
            "renderer warm-up: {} [{}]",
            warm.status_text(),
            warm.backend_label
        ));
    });

    log_startup_phase(StartupPhase::Done);
    plan
}
