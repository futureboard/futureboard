use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::window::{studio_window_options, welcome_window_options};
use gpui::{App, AppContext, BorrowAppContext, Global, WindowHandle};
use sphere_ui_components::app_state::{AppMode, AppSessionGate};
use sphere_ui_components::assets;
use sphere_ui_components::boot;
use sphere_ui_components::components::progress_dialog::ProgressBarValue;
use sphere_ui_components::layout::{
    PendingCloseAction, PreparedWorkspaceFinish, ProjectOpenOptions, StudioLayout,
};
use sphere_ui_components::loading_session::{
    begin_pre_studio_workspace_prepare, begin_project_session_load,
    begin_studio_project_session_load, begin_studio_session_shutdown, complete_project_lifecycle,
    show_loading_session_error, update_loading_session_progress, LoadFailedContext,
    LoadedSessionPackage, ProjectLifecycleTarget, SessionRollbackSnapshot, SessionShutdownReason,
};
use sphere_ui_components::project::{FutureboardProject, ProjectTemplate};
use sphere_ui_components::splash::SplashWindowHandle;
use sphere_ui_components::startup::{
    log_startup_phase, run_lightweight_boot, StartupPhase, StartupRoute,
};
#[cfg(feature = "exclusive")]
use sphere_ui_components::welcome::WelcomeFooterAction;
use sphere_ui_components::welcome::{WelcomeAction, WelcomeCallbacks, WelcomeWindow};

static DISCORD_RPC: OnceLock<sphere_discord_rpc::DiscordRpcHandle> = OnceLock::new();

pub fn install_discord_rpc(handle: sphere_discord_rpc::DiscordRpcHandle) {
    let _ = DISCORD_RPC.set(handle);
}

pub fn shutdown_discord_rpc() {
    if let Some(rpc) = DISCORD_RPC.get() {
        rpc.request_shutdown();
    }
}

fn set_discord_enabled(enabled: bool) {
    if let Some(rpc) = DISCORD_RPC.get() {
        rpc.set_enabled(enabled);
    }
}

fn set_discord_presence(presence: sphere_discord_rpc::Presence) {
    if let Some(rpc) = DISCORD_RPC.get() {
        rpc.set_presence(presence);
    }
}

/// Retains the studio window handle at app scope so GPUI never drops the last
/// window during LoadingSession → Studio transitions.
#[derive(Default)]
struct NativeShellWindows {
    studio: Option<WindowHandle<StudioLayout>>,
}

impl Global for NativeShellWindows {}

pub fn setup(cx: &mut App) {
    boot::log("app boot start");
    cx.set_global(AppSessionGate {
        mode: AppMode::Welcome,
    });
    cx.set_global(NativeShellWindows::default());
    sphere_ui_components::settings::install_settings_change_listener(Arc::new(|settings| {
        set_discord_enabled(settings.general.discord_rpc_enabled);
    }));

    let theme_report = sphere_ui_components::theme::initialize_theme_system();
    let saved_theme = sphere_ui_components::settings::SettingsSchema::load_from_disk()
        .appearance
        .theme;
    let _ = sphere_ui_components::theme::activate_theme_by_id(&saved_theme);
    boot::log(&format!(
        "theme initialized: {} ({})",
        theme_report.active_id, theme_report.active_name
    ));

    // Fonts must be registered before the first native view renders.
    assets::register_fonts(cx);
    boot::log("fonts registered");

    // On macOS CEF must install its integrated CFRunLoop observer during
    // applicationDidFinishLaunching, before GPUI creates the first native
    // window. Deferring initialization to the first main-queue turn is too
    // late and causes the pinned CEF framework to assert in its run-loop
    // observer. The probe under SphereWebView covers this ordering.
    #[cfg(all(feature = "builtin-plugin-editor", target_os = "macos"))]
    {
        use sphere_ui_components::components::builtin_plugin_editor;
        match builtin_plugin_editor::init_at_boot() {
            Ok(()) => {
                boot::log("builtin plugin editor host (CEF) initialized before first macOS window");
                builtin_plugin_editor::preload();
            }
            Err(err) => boot::log(&format!("builtin plugin editor host unavailable: {err}")),
        }
    }

    // Prewarm CEF on the GPUI UI thread, but outside this active App update.
    // CEF may synchronously dispatch Win32 messages while initializing; running
    // it as foreground work prevents those messages from re-entering a borrowed
    // AppCell. A failure stays non-fatal and is surfaced by the editor window.
    #[cfg(not(all(feature = "builtin-plugin-editor", target_os = "macos")))]
    cx.spawn(async move |cx| {
        use sphere_ui_components::components::builtin_plugin_editor;
        match builtin_plugin_editor::init_at_boot() {
            Ok(()) => {
                boot::log("builtin plugin editor host (CEF) initialized");
                // Warm-up browser: spawns Chromium's helper processes (GPU,
                // network, renderer) behind the loading screen so the first
                // editor open only pays for its own page.
                builtin_plugin_editor::preload();
                // Drive CEF briefly — with no editor window open nothing else
                // pumps its message loop, and the warm-up needs it to finish
                // launching those processes.
                for _ in 0..150 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(16))
                        .await;
                    builtin_plugin_editor::pump();
                }
                boot::log("builtin plugin editor warm-up pump finished");
            }
            Err(err) => boot::log(&format!("builtin plugin editor host unavailable: {err}")),
        }
    })
    .detach();

    // macOS initialization and browser creation happened synchronously above
    // at the proven AppKit lifecycle point. Drive the warm-up after this App
    // update is released; CEF remains owned by the same main thread.
    #[cfg(all(feature = "builtin-plugin-editor", target_os = "macos"))]
    cx.spawn(async move |cx| {
        use sphere_ui_components::components::builtin_plugin_editor;
        for _ in 0..150 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
            builtin_plugin_editor::pump();
        }
        boot::log("builtin plugin editor warm-up pump finished");
    })
    .detach();

    // Apply the saved renderer preference now (before any window) so the GPU
    // renderer can be warmed during the loading screen rather than stalling the
    // first studio frame.
    sphere_ui_components::layout::apply_saved_renderer_preference(cx);
    boot::log("renderer preference applied");

    // Splash first — before Welcome/Studio windows or heavy UI initialization.
    let splash = match SplashWindowHandle::open(cx) {
        Ok(handle) => Some(handle),
        Err(error) => {
            boot::log(&format!("splash open failed: {error}"));
            None
        }
    };

    cx.spawn(async move |cx| {
        // Reading the GPU list first: enumerate adapters and record availability
        // so stem extraction can auto-select GPU (DirectML on Windows).
        if let Some(splash) = splash.as_ref() {
            cx.update(|app| splash.set_status(app, "Reading GPU list…"));
        }
        let gpu_summary = cx.update(|_app| sphere_ui_components::startup::probe_gpus().summary);
        if let Some(splash) = splash.as_ref() {
            cx.update(|app| splash.set_status(app, gpu_summary.clone()));
        }

        let plan = run_lightweight_boot(cx).await;
        boot::log(&format!(
            "startup route resolved: {:?} (show_welcome={})",
            plan.route, plan.show_welcome_screen
        ));

        // Open the next surface before closing splash so Windows does not quit
        // on LastWindowClosed while transitioning.
        if let Some(splash) = splash.as_ref() {
            let opening = match plan.route {
                StartupRoute::Welcome => "Opening Welcome…",
                _ => "Opening Studio…",
            };
            cx.update(|app| splash.set_status(app, opening));
        }
        match plan.route {
            StartupRoute::Welcome => {
                log_startup_phase(StartupPhase::OpeningWelcome);
                cx.update(open_welcome_window);
            }
            StartupRoute::EmptyWorkspace => {
                log_startup_phase(StartupPhase::OpeningStudio);
                cx.update(|app| {
                    begin_workspace_session(
                        "Preparing Workspace…",
                        FutureboardProject::new("Untitled Project"),
                        PreparedWorkspaceFinish::EmptyUntitled,
                        app,
                    );
                });
            }
            StartupRoute::RestoreLastProject(path) => {
                log_startup_phase(StartupPhase::OpeningStudio);
                cx.update(|app| {
                    begin_load_project_from_welcome(
                        path,
                        ProjectOpenOptions { from_recent: true },
                        app,
                    );
                });
            }
            StartupRoute::OpenProject(path) => {
                log_startup_phase(StartupPhase::OpeningStudio);
                cx.update(|app| {
                    begin_load_project_from_welcome(path, ProjectOpenOptions::default(), app);
                });
            }
        }

        if let Some(splash) = splash {
            cx.update(|app| splash.close(app));
        }

        // First-run EULA gate (Exclusive Edition). Opens on top of the first
        // surface; declining quits. No-op once accepted for this version.
        #[cfg(feature = "exclusive")]
        {
            let _ = cx.update(|app| crate::exclusive_edition::show_eula_if_needed(app));
        }
    })
    .detach();
}

fn set_app_mode(cx: &mut App, mode: AppMode) {
    let from = cx
        .try_global::<AppSessionGate>()
        .map(|gate| gate.mode)
        .unwrap_or(AppMode::Welcome);
    if from != mode {
        sphere_ui_components::window_lifecycle::log_app_mode_change(from, mode, "app_shell");
        match mode {
            AppMode::Welcome | AppMode::LoadFailed => {
                set_discord_presence(sphere_discord_rpc::Presence::Welcome)
            }
            AppMode::LoadingSession => set_discord_presence(sphere_discord_rpc::Presence::Loading),
            AppMode::Studio => {}
        }
    }
    cx.update_global::<AppSessionGate, _>(|gate, _| gate.mode = mode);
}

fn studio_shell_alive(cx: &App) -> bool {
    cx.try_global::<NativeShellWindows>()
        .map(|shell| shell.studio.is_some())
        .unwrap_or(false)
}

fn log_window_registry(cx: &App, stage: &str) {
    let studio_alive = studio_shell_alive(cx);
    let loader_alive = sphere_ui_components::loading_session::is_loading_session_window_open(cx);
    let app_mode = cx
        .try_global::<AppSessionGate>()
        .map(|gate| gate.mode.label())
        .unwrap_or("unknown");
    sphere_ui_components::window_lifecycle::log_shell_window_registry(
        stage,
        studio_alive,
        loader_alive,
        app_mode,
    );
}

fn store_studio_window_handle(cx: &mut App, handle: WindowHandle<StudioLayout>) {
    cx.update_global::<NativeShellWindows, _>(|shell, _| {
        shell.studio = Some(handle);
    });
    eprintln!("[StudioOpen] stored studio window handle");
}

fn finish_loading_to_studio(cx: &mut App) {
    log_window_registry(cx, "before loading ui close");
    eprintln!("[ProjectSwitch] close loading ui");
    // Never close the loading window synchronously from a path that may still be
    // inside a LoadingSessionWindow update (GPUI double-lease). Activate the
    // studio only after the loader is gone so users never stare at an empty shell.
    cx.defer(|cx| {
        if !studio_shell_alive(cx) {
            eprintln!("[WindowLifecycle] refused to close loader — studio shell not alive");
            return;
        }
        complete_project_lifecycle(cx, ProjectLifecycleTarget::Studio);
        if let Some(studio) = cx
            .try_global::<NativeShellWindows>()
            .and_then(|shell| shell.studio)
        {
            let _ = studio.update(cx, |_layout, window, _cx| {
                window.activate_window();
            });
        }
        log_window_registry(cx, "after loading ui close");
        eprintln!("[SessionLoad] studio ready");
    });
}

fn finish_loading_to_welcome(cx: &mut App) {
    log_window_registry(cx, "before loading ui close (welcome)");
    eprintln!("[ProjectClose] lifecycle completing target=welcome");
    complete_project_lifecycle(cx, ProjectLifecycleTarget::Welcome);
    log_window_registry(cx, "after loading ui close (welcome)");
    eprintln!("[ProjectClose] close-to-welcome complete");
}

fn transition_loaded_package_to_existing_studio(
    studio: gpui::WindowHandle<StudioLayout>,
    package: LoadedSessionPackage,
    cx: &mut App,
) {
    let project_name = package.project.name.clone();
    log_window_registry(cx, "before install into existing studio");
    eprintln!("[ProjectSwitch] installing loaded session");
    eprintln!("[AppMode] LoadingSession -> Studio");
    set_app_mode(cx, AppMode::Studio);
    store_studio_window_handle(cx, studio);
    let install_result = studio.update(cx, |layout, _window, cx| {
        layout.install_loaded_session(package, cx);
        if !layout.has_self_window() {
            layout.set_self_window(studio);
        }
        cx.notify();
    });
    if install_result.is_err() {
        eprintln!("[ProjectSwitch] install into existing studio failed");
        set_app_mode(cx, AppMode::LoadFailed);
        show_loading_session_error(cx, "Could not install the loaded project into the studio.");
        return;
    }
    eprintln!("[SessionLoad] install complete");
    set_discord_presence(sphere_discord_rpc::Presence::editing(project_name));
    eprintln!("[ProjectSwitch] notify root window");
    finish_loading_to_studio(cx);
    log_window_registry(cx, "after switch install");
    eprintln!("[ProjectSwitch] complete");
}

/// Open the Welcome window (start screen). Used after splash boot and when
/// returning from Close Project — never shows the splash window again.
fn open_welcome_window(cx: &mut App) {
    set_app_mode(cx, AppMode::Welcome);
    let callbacks = WelcomeCallbacks {
        on_action: Arc::new(|action, welcome_window, cx| match action {
            // Always open the LoadingSession shell before retiring Welcome.
            // On Linux, QuitMode::LastWindowClosed would otherwise quit the app
            // the moment Welcome closes with no replacement window yet.
            WelcomeAction::OpenProjectFile(path) => {
                begin_load_project_from_welcome(path, ProjectOpenOptions::default(), cx);
                welcome_window.remove_window();
            }
            WelcomeAction::OpenRecent(path) => {
                begin_load_project_from_welcome(path, ProjectOpenOptions { from_recent: true }, cx);
                welcome_window.remove_window();
            }
            other => {
                open_studio_for_action(other, cx);
                welcome_window.remove_window();
            }
        }),
        footer_action: {
            #[cfg(feature = "exclusive")]
            {
                Some(WelcomeFooterAction {
                    label: "License",
                    icon: assets::ICON_FILE_PATH,
                    on_click: Arc::new(|welcome_window, cx| {
                        let activation = crate::exclusive_edition::configured_license_activator(
                            env!("CARGO_PKG_VERSION"),
                        );
                        if let Err(error) = crate::exclusive_edition::open_license_activation_window(
                            Some(welcome_window.bounds()),
                            activation,
                            cx,
                        ) {
                            eprintln!("[LicenseActivation] failed to open dialog: {error}");
                        }
                    }),
                })
            }
            #[cfg(not(feature = "exclusive"))]
            {
                None
            }
        },
    };
    let _welcome = cx
        .open_window(welcome_window_options(cx), |_window, cx| {
            cx.new(|cx| WelcomeWindow::new(callbacks, cx.focus_handle()))
        })
        .expect("failed to open welcome window");
    boot::log("welcome window shown");
}

fn transition_loaded_package_to_studio(package: LoadedSessionPackage, cx: &mut App) {
    eprintln!("[AppMode] LoadingSession -> Studio");
    set_app_mode(cx, AppMode::Studio);
    update_loading_session_progress(cx, "Opening studio", ProgressBarValue::value(0.98));
    cx.defer(
        move |cx| match open_studio_workspace(WorkspaceInit::Loaded(package), cx) {
            Ok(()) => {
                eprintln!("[StudioMount] mounted after ready");
                finish_loading_to_studio(cx);
            }
            Err(error) => {
                eprintln!("[StudioOpen] studio window open failed: {error}");
                set_app_mode(cx, AppMode::LoadFailed);
                show_loading_session_error(
                    cx,
                    format!("Could not open the studio workspace.\n\nDetails: {error}"),
                );
            }
        },
    );
}

fn transition_prepared_package_to_studio(
    package: LoadedSessionPackage,
    follow_up: PreparedWorkspaceFinish,
    cx: &mut App,
) {
    eprintln!("[AppMode] LoadingSession -> Studio (prepared workspace)");
    set_app_mode(cx, AppMode::Studio);
    update_loading_session_progress(cx, "Opening studio", ProgressBarValue::value(0.98));
    let init = WorkspaceInit::Prepared { package, follow_up };
    cx.defer(move |cx| match open_studio_workspace(init, cx) {
        Ok(()) => {
            eprintln!("[StudioMount] mounted after workspace prepare");
            finish_loading_to_studio(cx);
        }
        Err(error) => {
            eprintln!("[StudioOpen] studio window open failed: {error}");
            set_app_mode(cx, AppMode::LoadFailed);
            show_loading_session_error(
                cx,
                format!("Could not open the studio workspace.\n\nDetails: {error}"),
            );
        }
    });
}

fn begin_close_project_session(
    reason: SessionShutdownReason,
    owner_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    studio: gpui::WindowHandle<StudioLayout>,
    cx: &mut App,
) {
    let on_complete = Arc::new(move |cx: &mut App| {
        let _ = studio.update(cx, |layout, window, cx| {
            sphere_ui_components::window_position::persist_studio_window_from_window(window, cx);
            window.remove_window();
            let _ = layout;
        });
        cx.update_global::<NativeShellWindows, _>(|shell, _| {
            shell.studio = None;
        });
        set_app_mode(cx, AppMode::Welcome);
        open_welcome_window(cx);
        finish_loading_to_welcome(cx);
    });
    begin_studio_session_shutdown(reason, studio, owner_bounds, true, on_complete, cx);
}

fn begin_load_project_from_welcome(path: PathBuf, open_options: ProjectOpenOptions, cx: &mut App) {
    set_discord_presence(sphere_discord_rpc::Presence::Loading);
    let on_success = Arc::new(|package: LoadedSessionPackage, cx: &mut App| {
        transition_loaded_package_to_studio(package, cx);
    });
    let on_failure = Arc::new(|ctx: LoadFailedContext, cx: &mut App| {
        handle_load_failed(ctx, cx);
    });
    begin_project_session_load(
        path,
        open_options,
        None,
        None,
        None,
        on_success,
        on_failure,
        cx,
    );
}

fn begin_workspace_session(
    heading: &str,
    project: FutureboardProject,
    follow_up: PreparedWorkspaceFinish,
    cx: &mut App,
) {
    set_discord_presence(sphere_discord_rpc::Presence::Loading);
    let on_success = Arc::new(move |package: LoadedSessionPackage, cx: &mut App| {
        transition_prepared_package_to_studio(package, follow_up.clone(), cx);
    });
    let on_failure = Arc::new(|ctx: LoadFailedContext, cx: &mut App| {
        handle_load_failed(ctx, cx);
    });
    begin_pre_studio_workspace_prepare(heading, project, on_success, on_failure, cx);
}

fn handle_project_switch_load_failed(
    studio: gpui::WindowHandle<StudioLayout>,
    ctx: LoadFailedContext,
    cx: &mut App,
) {
    eprintln!(
        "[SessionLoad] project switch failed: {} — {}",
        ctx.title, ctx.message
    );
    if let Some(snapshot) = ctx.rollback {
        let project_name = snapshot.session.name.clone();
        set_app_mode(cx, AppMode::Studio);
        store_studio_window_handle(cx, studio);
        let _ = studio.update(cx, |layout, _window, cx| {
            layout.restore_session_rollback_snapshot(snapshot, cx);
            layout.show_project_open_failed_dialog(
                &ctx.title,
                &ctx.message,
                ctx.detail.clone(),
                ctx.path,
                ctx.open_options,
                cx,
            );
            cx.notify();
        });
        set_discord_presence(sphere_discord_rpc::Presence::editing(project_name));
        finish_loading_to_studio(cx);
        log_window_registry(cx, "after switch failure rollback");
        return;
    }
    handle_load_failed(ctx, cx);
}

fn request_project_switch_in_studio(
    studio: gpui::WindowHandle<StudioLayout>,
    path: PathBuf,
    open_options: ProjectOpenOptions,
    cx: &mut App,
) {
    // Never call `studio.update` synchronously here — this hook is invoked from
    // inside an active `StudioLayout` update (e.g. File → Open Project).
    cx.defer(move |cx| {
        log_window_registry(cx, "before switch");
        eprintln!("[ProjectSwitch] begin target={}", path.display());
        eprintln!(
            "[ProjectSwitch] AppMode before={}",
            cx.try_global::<AppSessionGate>()
                .map(|gate| gate.mode.label())
                .unwrap_or("unknown")
        );

        let prepared = studio.update(cx, |layout, _window, cx| {
            layout.prepare_for_in_studio_project_switch_transaction(cx)
        });
        let Ok((rollback, owner_bounds)) = prepared else {
            eprintln!("[ProjectSwitch] failed to prepare studio for in-studio switch");
            return;
        };

        eprintln!("[SessionShutdown] begin reason=project_switch");
        log_window_registry(cx, "after prepare before load");
        set_app_mode(cx, AppMode::LoadingSession);
        eprintln!("[ProjectSwitch] AppMode -> LoadingSession");

        // Keep the studio shell alive — on Windows GPUI quits when the last
        // window closes (QuitMode::LastWindowClosed).
        let studio_for_success = studio;
        let studio_for_failure = studio;
        let on_success = Arc::new(move |package: LoadedSessionPackage, cx: &mut App| {
            transition_loaded_package_to_existing_studio(studio_for_success, package, cx);
        });
        let on_failure = Arc::new(move |ctx: LoadFailedContext, cx: &mut App| {
            handle_project_switch_load_failed(studio_for_failure, ctx, cx);
        });
        eprintln!("[SessionLoad] begin target={}", path.display());
        begin_studio_project_session_load(
            path,
            open_options,
            rollback,
            studio,
            owner_bounds,
            on_success,
            on_failure,
            cx,
        );
    });
}

fn handle_load_failed(ctx: LoadFailedContext, cx: &mut App) {
    eprintln!("[SessionLoad] open failed: {} — {}", ctx.title, ctx.message);
    if let Some(snapshot) = ctx.rollback {
        let project_name = snapshot.session.name.clone();
        set_app_mode(cx, AppMode::Studio);
        if let Some(studio) = cx
            .try_global::<NativeShellWindows>()
            .and_then(|shell| shell.studio)
        {
            log_window_registry(cx, "rollback onto existing studio");
            let _ = studio.update(cx, |layout, _window, cx| {
                layout.restore_session_rollback_snapshot(snapshot, cx);
                cx.notify();
            });
            set_discord_presence(sphere_discord_rpc::Presence::editing(project_name));
            finish_loading_to_studio(cx);
            return;
        }
        match open_studio_workspace(WorkspaceInit::Rollback(snapshot), cx) {
            Ok(()) => finish_loading_to_studio(cx),
            Err(error) => {
                eprintln!("[StudioOpen] rollback studio open failed: {error}");
                set_app_mode(cx, AppMode::LoadFailed);
                show_loading_session_error(
                    cx,
                    format!(
                        "{}\n\n{}\n\nRollback failed: {error}",
                        ctx.title, ctx.message
                    ),
                );
            }
        }
        return;
    }
    set_app_mode(cx, AppMode::LoadFailed);
    open_welcome_window(cx);
    complete_project_lifecycle(cx, ProjectLifecycleTarget::Welcome);
}

/// What the freshly opened workspace should do once its window exists.
enum WorkspaceInit {
    /// Install a decoded project that was loaded before studio mounted.
    Loaded(LoadedSessionPackage),
    /// Pre-studio handoff plus a lightweight workspace finish hook.
    Prepared {
        package: LoadedSessionPackage,
        follow_up: PreparedWorkspaceFinish,
    },
    /// Restore a rollback snapshot after a failed in-studio replace.
    Rollback(SessionRollbackSnapshot),
}

impl WorkspaceInit {
    fn project_name(&self) -> &str {
        match self {
            Self::Loaded(package) | Self::Prepared { package, .. } => &package.project.name,
            Self::Rollback(snapshot) => &snapshot.session.name,
        }
    }
}

fn open_studio_for_action(action: WelcomeAction, cx: &mut App) {
    match action {
        WelcomeAction::OpenProjectFile(_) | WelcomeAction::OpenRecent(_) => (),
        WelcomeAction::EmptyProject | WelcomeAction::OpenEmptyWorkspace => {
            begin_workspace_session(
                "Preparing Workspace…",
                FutureboardProject::new("Untitled Project"),
                PreparedWorkspaceFinish::EmptyUntitled,
                cx,
            );
        }
        WelcomeAction::MidiComposer => begin_workspace_session(
            "Preparing Workspace…",
            FutureboardProject::new("Untitled Project"),
            PreparedWorkspaceFinish::Template(ProjectTemplate::BeatMaking),
            cx,
        ),
        WelcomeAction::AudioSession => begin_workspace_session(
            "Preparing Workspace…",
            FutureboardProject::new("Untitled Project"),
            PreparedWorkspaceFinish::Template(ProjectTemplate::Recording),
            cx,
        ),
        WelcomeAction::MixTemplate => begin_workspace_session(
            "Preparing Workspace…",
            FutureboardProject::new("Untitled Project"),
            PreparedWorkspaceFinish::Template(ProjectTemplate::Mixing),
            cx,
        ),
        WelcomeAction::OpenProject => begin_workspace_session(
            "Preparing Workspace…",
            FutureboardProject::new("Untitled Project"),
            PreparedWorkspaceFinish::OpenDialog,
            cx,
        ),
        WelcomeAction::CreateProject(options) => begin_workspace_session(
            "Preparing Workspace…",
            FutureboardProject::new(&options.name),
            PreparedWorkspaceFinish::CreateProject(options),
            cx,
        ),
    }
}

fn open_studio_workspace(init: WorkspaceInit, cx: &mut App) -> Result<(), String> {
    if !cx
        .try_global::<AppSessionGate>()
        .map(|g| g.mode.allows_studio_mount())
        .unwrap_or(false)
    {
        eprintln!("[StudioMount] blocked because session not ready");
        boot::log("studio mount blocked — app mode is not Studio");
        return Err("app mode is not Studio".to_string());
    }

    let project_name = init.project_name().to_string();
    eprintln!("[StudioOpen] opening studio window");
    let studio_options = studio_window_options(cx);
    let studio = cx
        .open_window(studio_options, |window, cx| {
            boot::log("build StudioLayout");
            let layout = cx.new(StudioLayout::new);
            boot::log("StudioLayout built");

            // WCO / OS window close → quit the app, but go through the
            // unsaved-changes guard first. Returning `false` always prevents
            // GPUI's default close: `request_quit` drives `cx.quit()` only once
            // the user confirms, so Cancel keeps the app open and the close
            // never routes to Welcome.
            let studio_entity = layout.clone();
            window.on_window_should_close(cx, move |window, cx| {
                sphere_ui_components::window_position::persist_studio_window_from_window(
                    window, cx,
                );
                let bounds = window.bounds();
                studio_entity.update(cx, |studio, cx| {
                    studio.request_close(PendingCloseAction::QuitApp, Some(bounds), cx);
                });
                // Always veto the platform close; confirmed quit runs via `do_quit`.
                false
            });

            layout
        })
        .map_err(|e| e.to_string())?;
    eprintln!("[StudioOpen] studio window opened");

    let studio_handle = studio;
    store_studio_window_handle(cx, studio_handle);

    studio
        .update(cx, move |layout, _window, cx| {
            // Wire the workspace lifecycle: its own window handle (so Close Project
            // can close it) and the hook that re-opens Welcome.
            layout.set_self_window(studio_handle);
            layout.set_request_welcome_callback(Arc::new(open_welcome_window));
            layout.set_request_session_shutdown_callback(Arc::new({
                move |reason, owner_bounds, studio, cx| {
                    begin_close_project_session(reason, owner_bounds, studio, cx);
                }
            }));
            layout.set_request_project_load_callback(Arc::new({
                let studio_handle = studio_handle;
                move |path, open_options, cx| {
                    request_project_switch_in_studio(studio_handle, path, open_options, cx);
                }
            }));

            match init {
                WorkspaceInit::Loaded(package) => layout.install_loaded_session(package, cx),
                WorkspaceInit::Prepared { package, follow_up } => {
                    layout.install_prepared_workspace(package, follow_up, cx);
                }
                WorkspaceInit::Rollback(snapshot) => {
                    layout.restore_session_rollback_snapshot(snapshot, cx)
                }
            }
        })
        .map_err(|e| e.to_string())?;

    set_discord_presence(sphere_discord_rpc::Presence::editing(project_name));

    // The studio window is created hidden (see `platform_chrome::studio_window_options`,
    // `show: false`) so the OS never displays its empty black client area while the
    // heavy first layout / workspace install runs. Reveal it after the first frame
    // has painted — `activate_window` applies the stored initial placement and shows
    // the window with real content instead of a black flash.
    let _ = studio.update(cx, |_layout, window, _cx| {
        window.on_next_frame(|window, _cx| window.activate_window());
    });

    boot::log("workspace entered");
    Ok(())
}
