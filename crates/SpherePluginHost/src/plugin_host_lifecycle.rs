//! Windows Job Object, AppUserModelID, and coordinated plugin-host shutdown.
//!
//! Implementation lives in [`crate::process_manager`] and [`crate::platform`].

pub use crate::plugin_host_spawn_config::PluginHostSpawnConfig;
pub use crate::process_manager::{
    init_plugin_host_job, shutdown_host_client, shutdown_host_client_with_timeout,
    BridgeHostManager, BridgeHostRecord, HostLifecycleState, PluginHostHandle, PluginHostId,
    PluginHostProcessManager, HOST_SHUTDOWN_TIMEOUT,
};

/// Shared Windows shell identity for FutureboardNative and PluginHost.
pub const APP_USER_MODEL_ID: &str = "studio.futureboard.Futureboard";

/// Set the process-wide explicit AppUserModelID so plugin-host and editor
/// windows group under the DAW shell identity.
pub fn set_futureboard_app_user_model_id() {
    set_app_user_model_id();
}

#[cfg(windows)]
pub fn set_app_user_model_id() {
    use windows::core::w;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    // SAFETY: `w!` is a 'static NUL-terminated UTF-16 literal.
    let result =
        unsafe { SetCurrentProcessExplicitAppUserModelID(w!("studio.futureboard.Futureboard")) };
    match result {
        Ok(()) => eprintln!(
            "[app-id] SetCurrentProcessExplicitAppUserModelID id={APP_USER_MODEL_ID} ok=true"
        ),
        Err(error) => eprintln!(
            "[app-id] SetCurrentProcessExplicitAppUserModelID id={APP_USER_MODEL_ID} ok=false error={error}"
        ),
    }
}

#[cfg(not(windows))]
pub fn set_app_user_model_id() {}

/// Takes this process's own console window off screen and off the taskbar.
///
/// The plug-in host is a service of the DAW, not an app: it has no window of
/// its own any more — the editor's window belongs to the main process — so the
/// only thing it can put on the taskbar is its console, which a debug build
/// still gets. That console is a blank window sitting between the DAW's own,
/// and its output already goes to the host log and up the pipe to the parent,
/// so nothing is lost by hiding it.
///
/// Only ever hides a console this process owns. A host spawned with the
/// parent's console attached would otherwise hide the DAW's.
#[cfg(windows)]
pub fn hide_own_console_window() {
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, ShowWindow, SW_HIDE};

    // SAFETY: plain Win32 queries; every handle is checked before use.
    unsafe {
        let console = GetConsoleWindow();
        if console.is_invalid() {
            return;
        }
        let mut owner_pid = 0u32;
        GetWindowThreadProcessId(console, Some(&mut owner_pid));
        if owner_pid != GetCurrentProcessId() {
            eprintln!("[plugin-host] console belongs to another process; left alone");
            return;
        }
        let _ = ShowWindow(console, SW_HIDE);
        eprintln!("[plugin-host] own console window hidden (kept off the taskbar)");
    }
}

#[cfg(not(windows))]
pub fn hide_own_console_window() {}
