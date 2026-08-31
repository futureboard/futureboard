//! Main-app-owned **content child HWND** for separated-process plugin editor
//! hosting (`FUTUREBOARD_PLUGIN_EDITOR_OWNERSHIP=host_process`).
//!
//! Spec Part 2: when the editor lives in `FutureboardPluginHostX64.exe`, the
//! *window* still belongs to the GPUI main app. The GPUI plugin-editor window
//! supplies its top HWND; this module creates a real `WS_CHILD` content HWND
//! under it and hands the content HWND's handle to the host process over IPC.
//! The host attaches the VST3 `IPlugView` to that handle from its own COM STA
//! thread (cross-process embedding).
//!
//! VST3 editor hosting follows public.sdk/samples/vst-hosting/editorhost
//! lifecycle: the difference here is only *which process* owns the window
//! (main app) versus the view (host process).
//!
//! Hard requirements enforced here:
//! - `content_hwnd != top_hwnd` (a dedicated child, never the top window).
//! - content child styles: `WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS`.
//! - the child's parent is the supplied top HWND.
//!
//! On non-Windows targets every entry point is a no-op stub returning `None`,
//! so the crate still compiles cross-platform. (macOS NSView hosting is a later
//! slice.)

/// Physical-pixel rect (relative to the parent client area) for the content
/// child window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

fn debug_enabled() -> bool {
    std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").is_some()
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Once;

    use super::{debug_enabled, ContentRect};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateSolidBrush, FillRect, GetStockObject, BLACK_BRUSH, HBRUSH, HDC,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetParent, GetWindow,
        GetWindowLongPtrW, GetWindowRect, IsChild, IsWindow, RegisterClassW, SetWindowPos,
        GWL_STYLE, GW_CHILD, HMENU, SWP_NOACTIVATE, SWP_NOZORDER, WINDOW_EX_STYLE, WM_ERASEBKGND,
        WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_PARENTNOTIFY, WM_RBUTTONDOWN, WM_SETFOCUS,
        WM_XBUTTONDOWN, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
    };

    fn hwnd_from(handle: u64) -> HWND {
        HWND(handle as *mut core::ffi::c_void)
    }

    /// Dedicated window class for the content host. We do NOT use the predefined
    /// `STATIC` class: a blank static control fills its client area with a
    /// light/system background, which appears as the "blank white" the plugin
    /// editor showed before the host's view painted (spec Part 5). This class
    /// paints solid black instead, matching the host's embed child, so there is
    /// no white flash and any area outside the plugin's own view stays dark.
    const CONTENT_HOST_CLASS: PCWSTR = w!("SpherePluginContentHost");

    fn ensure_content_host_class() {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(content_host_wndproc),
                lpszClassName: CONTENT_HOST_CLASS,
                hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0),
                ..Default::default()
            };
            unsafe { RegisterClassW(&wc) };
        });
    }

    /// Fill colour for the host's uncovered area.
    ///
    /// Black in normal use so an editor never flashes light. Under the view
    /// trace it is magenta instead: "the panel is dark" cannot distinguish a
    /// host window that is composited but empty from one the parent painted
    /// straight over, and that distinction is the whole question when a native
    /// child does not appear.
    fn host_fill_brush() -> HBRUSH {
        if debug_enabled() {
            static MAGENTA: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
            let handle = *MAGENTA.get_or_init(|| {
                let brush = unsafe { CreateSolidBrush(COLORREF(0x00FF00FF)) };
                brush.0 as usize
            });
            if handle != 0 {
                return HBRUSH(handle as *mut core::ffi::c_void);
            }
        }
        HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0)
    }

    /// Route Win32 keyboard focus to the embedded child view (the CEF browser
    /// or a VST3 editor).
    ///
    /// The GPUI shell never hands keyboard focus to native children on its
    /// own: its top-level HWND keeps focus through activation, so a text
    /// field inside an embedded editor could be clicked but never typed into.
    /// Two signals close that gap:
    ///
    /// - `WM_PARENTNOTIFY` with a button-down event — the one notification
    ///   this host reliably receives when a click lands anywhere inside the
    ///   embedded child, even though the click itself is consumed by the
    ///   child's own window procedure.
    /// - `WM_SETFOCUS` — anything that focuses this host (tabbing, an
    ///   explicit `SetFocus` from the shell) is really meant for the view
    ///   inside it.
    ///
    /// Both forward focus to the first child; Chromium (and VST3 views)
    /// route it on to their inner widget from there.
    unsafe fn focus_embedded_child(hwnd: HWND) {
        if let Ok(child) = unsafe { GetWindow(hwnd, GW_CHILD) } {
            let _ = unsafe { SetFocus(Some(child)) };
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[plugin-content-hwnd] keyboard focus routed to embedded child=0x{:x}",
                    child.0 as u64
                );
            }
        }
    }

    /// Suppress the default white erase: fill the content host's client area
    /// black so the plugin's WS_CHILD view (created by the host process) is never
    /// preceded by a white flash, and any uncovered region stays dark. WS_CLIPCHILDREN
    /// on this window keeps us from painting over the plugin's own child.
    unsafe extern "system" fn content_host_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_PARENTNOTIFY {
            let event = (wparam.0 as u32) & 0xFFFF;
            if matches!(
                event,
                WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN | WM_XBUTTONDOWN
            ) {
                unsafe { focus_embedded_child(hwnd) };
            }
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        if msg == WM_SETFOCUS {
            unsafe { focus_embedded_child(hwnd) };
            return LRESULT(0);
        }
        if msg == WM_ERASEBKGND {
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let brush = host_fill_brush();
            unsafe { FillRect(hdc, &rc, brush) };
            static LOGGED: AtomicBool = AtomicBool::new(false);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "[plugin-content-hwnd] WM_ERASEBKGND suppressed=true fill={}",
                    if debug_enabled() { "magenta" } else { "black" }
                );
            }
            return LRESULT(1);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// A real `WS_CHILD` content window parented to a main-app top HWND. Drop
    /// destroys it. The handle ([`Self::hwnd`]) is what travels to the host
    /// process via `HostCommand::OpenEditorWithParentHwnd`.
    pub struct ContentChildHwnd {
        top_hwnd: u64,
        content_hwnd: u64,
    }

    /// Whether the native-child geometry trace is enabled.
    ///
    /// The docked ARA editor resizes with the panel drag, so this path runs
    /// per frame rather than once per window resize; `DESIGN.md` requires
    /// high-rate logs to be environment-gated.
    fn view_debug() -> bool {
        std::env::var_os("FUTUREBOARD_PLUGIN_VIEW_DEBUG").is_some()
    }

    impl ContentChildHwnd {
        fn log_diagnostics(&self, rect: ContentRect) {
            if !view_debug() {
                return;
            }
            unsafe {
                let top = hwnd_from(self.top_hwnd);
                let content = hwnd_from(self.content_hwnd);
                let parent = GetParent(content).map(|p| p.0 as u64).unwrap_or(0);
                let is_child = IsChild(top, content).as_bool();
                let style = GetWindowLongPtrW(content, GWL_STYLE);
                let mut shell_rect = windows::Win32::Foundation::RECT::default();
                let mut content_screen = windows::Win32::Foundation::RECT::default();
                let mut content_client = windows::Win32::Foundation::RECT::default();
                let _ = GetWindowRect(top, &mut shell_rect);
                let _ = GetWindowRect(content, &mut content_screen);
                let _ = GetClientRect(content, &mut content_client);
                eprintln!("[plugin-editor-window] shell_hwnd=0x{:x}", self.top_hwnd);
                eprintln!(
                    "[plugin-editor-window] content_hwnd=0x{:x}",
                    self.content_hwnd
                );
                eprintln!("[plugin-editor-window] GetParent(content_hwnd)=0x{parent:x}");
                eprintln!("[plugin-editor-window] content_is_child={is_child}");
                eprintln!(
                    "[plugin-editor-window] content_style=0x{style:08x} WS_CHILD={} WS_VISIBLE={}",
                    (style & WS_CHILD.0 as isize) != 0,
                    (style & WS_VISIBLE.0 as isize) != 0
                );
                eprintln!(
                    "[plugin-editor-window] shell_screen_rect=({},{},{},{})",
                    shell_rect.left, shell_rect.top, shell_rect.right, shell_rect.bottom
                );
                eprintln!(
                    "[plugin-editor-window] content_screen_rect=({},{},{},{})",
                    content_screen.left,
                    content_screen.top,
                    content_screen.right,
                    content_screen.bottom
                );
                eprintln!(
                    "[plugin-editor-window] content_client_rect=({}, {}, {}x{})",
                    rect.x, rect.y, rect.width, rect.height
                );
            }
        }

        /// Create the content child window under `top_hwnd`. Returns `None` if
        /// `top_hwnd` is not a window or window creation fails.
        pub fn create(top_hwnd: u64, rect: ContentRect) -> Option<Self> {
            if top_hwnd == 0 {
                return None;
            }
            let top = hwnd_from(top_hwnd);
            ensure_content_host_class();
            // Safety: all args are validated; the content host class is
            // registered above. The plugin paints its own child into this HWND
            // after the host attaches the view; this window only provides a
            // black, non-erasing backing (no white flash — spec Part 5).
            unsafe {
                if !IsWindow(Some(top)).as_bool() {
                    return None;
                }
                let content = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    CONTENT_HOST_CLASS,
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    rect.x,
                    rect.y,
                    rect.width.max(1),
                    rect.height.max(1),
                    Some(top),
                    None::<HMENU>,
                    None,
                    None,
                )
                .ok()?;

                let content_u64 = content.0 as u64;
                if content_u64 == top_hwnd {
                    // Must never happen with a child window; bail rather than
                    // letting the host attach to the top window.
                    let _ = DestroyWindow(content);
                    return None;
                }

                let result = Self {
                    top_hwnd,
                    content_hwnd: content_u64,
                };
                if debug_enabled() {
                    eprintln!("[PluginEditorWindow] content_hwnd != top_hwnd");
                }
                result.log_diagnostics(rect);
                Some(result)
            }
        }

        /// The content child HWND as a `u64` — the value passed to the host.
        pub fn hwnd(&self) -> u64 {
            self.content_hwnd
        }

        pub fn top_hwnd(&self) -> u64 {
            self.top_hwnd
        }

        /// Resize/reposition the content child (main app owns the geometry; it
        /// then tells the host to re-issue `onSize`).
        pub fn set_bounds(&self, rect: ContentRect) {
            unsafe {
                let _ = SetWindowPos(
                    hwnd_from(self.content_hwnd),
                    None,
                    rect.x,
                    rect.y,
                    rect.width.max(1),
                    rect.height.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if view_debug() {
                eprintln!(
                    "[plugin-editor-window] resize content_hwnd=0x{:x} rect=({},{},{}x{})",
                    self.content_hwnd, rect.x, rect.y, rect.width, rect.height
                );
            }
            self.log_diagnostics(rect);
        }

        /// True while the content HWND is still a valid window.
        pub fn is_valid(&self) -> bool {
            unsafe { IsWindow(Some(hwnd_from(self.content_hwnd))).as_bool() }
        }
    }

    impl Drop for ContentChildHwnd {
        fn drop(&mut self) {
            unsafe {
                if self.content_hwnd != 0 && IsWindow(Some(hwnd_from(self.content_hwnd))).as_bool()
                {
                    let _ = DestroyWindow(hwnd_from(self.content_hwnd));
                }
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::ContentRect;

    /// Non-Windows stub. Host-process editor embedding via NSView/X11 is a later
    /// slice; this keeps the crate compiling everywhere.
    pub struct ContentChildHwnd {
        _private: (),
    }

    impl ContentChildHwnd {
        pub fn create(_top_hwnd: u64, _rect: ContentRect) -> Option<Self> {
            None
        }
        pub fn hwnd(&self) -> u64 {
            0
        }
        pub fn top_hwnd(&self) -> u64 {
            0
        }
        pub fn set_bounds(&self, _rect: ContentRect) {}
        pub fn is_valid(&self) -> bool {
            false
        }
    }
}

pub use imp::ContentChildHwnd;
