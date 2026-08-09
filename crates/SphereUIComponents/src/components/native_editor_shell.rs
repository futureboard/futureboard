//! Native, **main-owned** plugin editor shell with **custom dark chrome**
//! (`PluginEditorShellKind::NativeBorderless`).
//!
//! Why native (not GPUI): GPUI windows present with a DXGI **flip-model** swap
//! chain (`CreateSwapChainForHwnd` + `DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`), which
//! composites over the entire client area and cannot let a foreign `WS_CHILD`
//! show through. So a plugin `IPlugView` parented under a GPUI window never
//! paints. This shell is a real native Win32 window (no swap chain over the
//! content), so the separated `FutureboardPluginHostX64.exe` can attach the
//! VST3 view into the content HWND and it paints.
//!
//! Why borderless + custom chrome: the OS title bar is suppressed
//! (`WM_NCCALCSIZE`) and a dark Futureboard-style title bar (plugin name +
//! minimize / maximize / close) is drawn by this window's `WndProc`. Drag and
//! resize are handled via `WM_NCHITTEST` (`HTCAPTION` / `HTLEFT` …). The result
//! is no standard Windows title bar, matching the Futureboard dark DAW look,
//! while keeping the reliable native painting (spec Part 1/2/5/8 — "Native
//! borderless + custom chrome").
//!
//! Still **main-owned**: created and controlled by `FutureboardNative.exe`.
//! NOT `host_detached` (PluginHost-owned) and NOT in-process hosting.
//! This is now a legacy/fallback shell; the default VST3 editor backend is the
//! host-owned C++ shell (`FUTUREBOARD_VST3_EDITOR_BACKEND=cpp_shell`).
//!
//! Non-Windows targets get no-op stubs so the crate compiles everywhere.

/// Events polled from the shell's message loop (the shell shares the GPUI main
/// thread's message pump, so its `WndProc` runs as messages dispatch).
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeShellPoll {
    /// User clicked the custom close button. The owner tears the session down
    /// (send `CloseEditor`, drop the shell).
    pub close_requested: bool,
    /// New **content** (client-below-titlebar) size since the last poll, if the
    /// window was resized/maximized. The owner forwards it as `ResizeEditor`.
    pub resized: Option<(i32, i32)>,
    /// Bare Space presses claimed by the shell's own chrome since the last poll.
    ///
    /// The shell is a raw Win32 window in the main process, so a key pressed
    /// while *it* holds focus reaches neither GPUI (which only routes keys for
    /// its own windows) nor the plug-in host process (which claims Space for the
    /// windows it owns). Without this, Space did nothing whenever focus sat on
    /// the editor's titlebar or frame instead of inside the plug-in's view.
    pub transport_toggles: u32,
}

/// Default plugin editor shell content size before the plug-in reports its
/// preferred size (spec Part 1). Single source of truth — do not scatter 640×320.
#[derive(Debug, Clone, Copy)]
pub struct PluginShellDefaults {
    pub default_content_width: i32,
    pub default_content_height: i32,
}

impl PluginShellDefaults {
    pub const fn new() -> Self {
        Self {
            default_content_width: 640,
            default_content_height: 320,
        }
    }
}

/// Centralized opening defaults for the native plugin editor shell.
pub fn shell_defaults() -> PluginShellDefaults {
    PluginShellDefaults::new()
}

/// Paint instrumentation counters (spec Part 6).
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeShellPaintStats {
    pub shell_paint_count: u32,
    pub content_paint_count: u32,
    pub content_erase_count: u32,
    pub size_count: u32,
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
    use std::sync::{Arc, Mutex, Once};

    use super::{NativeShellPaintStats, NativeShellPoll};
    use windows::core::{w, BOOL, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
        FillRect, GetStockObject, InvalidateRect, LineTo, MonitorFromWindow, MoveToEx,
        SelectObject, SetBkMode, SetTextColor, UpdateWindow, BLACK_BRUSH, CLIP_DEFAULT_PRECIS,
        DEFAULT_CHARSET, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
        FF_DONTCARE, FONT_QUALITY, FW_MEDIUM, HBRUSH, HDC, MONITOR_DEFAULTTONEAREST,
        MONITOR_DEFAULTTOPRIMARY, OUT_TT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
    };
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, ScreenToClient, MONITORINFO};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Controls::WM_MOUSELEAVE;
    use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE,
        TRACKMOUSEEVENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        ChildWindowFromPoint, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetClassNameW, GetClientRect, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
        IsDialogMessageW, IsWindow, IsZoomed, LoadCursorW, PeekMessageW, RegisterClassW,
        SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
        GWLP_HWNDPARENT, GWLP_USERDATA, GWL_EXSTYLE, GWL_STYLE, HMENU, HTBOTTOM, HTBOTTOMLEFT,
        HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
        HWND_TOP, IDC_ARROW, MA_ACTIVATE, MINMAXINFO, MSG, PM_REMOVE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_MAXIMIZE,
        SW_MINIMIZE, SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_CLOSE,
        WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCDESTROY,
        WM_NCHITTEST, WM_NCMOUSEMOVE, WM_NCPAINT, WM_PAINT, WM_SIZE, WM_SYSKEYDOWN, WNDCLASSW,
        WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_DLGFRAME,
        WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
        WS_THICKFRAME, WS_VISIBLE,
    };

    const SHELL_CLASS: PCWSTR = w!("SpherePluginEditorShell");
    const CONTENT_CLASS: PCWSTR = w!("SpherePluginEditorContent");

    // Hovered-button ids.
    const BTN_NONE: u8 = 0;
    const BTN_MIN: u8 = 1;
    const BTN_MAX: u8 = 2;
    const BTN_CLOSE: u8 = 3;

    /// Centralized chrome theme (spec Part 5). One place to adjust the plugin
    /// editor shell's look: colors, metrics, font. Native GDI/DirectWrite cannot
    /// read `theme.ts`, so this is the single source of truth for the shell;
    /// values track the Futureboard dark DAW palette. Metrics are logical pixels
    /// (scaled by window DPI at use).
    pub(crate) struct PluginShellTheme {
        pub titlebar_bg: COLORREF,
        pub content_bg: COLORREF,
        pub border: COLORREF,
        pub title_text: COLORREF,
        pub status_text: COLORREF,
        pub error_text: COLORREF,
        pub glyph: COLORREF,
        pub glyph_active: COLORREF,
        pub button_hover: COLORREF,
        pub close_hover: COLORREF,
        pub titlebar_h: i32,
        /// Futureboard shell border thickness (logical px, scaled at paint).
        pub border_px: i32,
        pub button_w: i32,
        pub resize_grab: i32,
        pub title_pad: i32,
        pub title_em: f32,
        pub status_em: f32,
        pub font: crate::components::plugin_shell_text::PluginShellFontTheme,
    }

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
    }

    /// The one shell theme. Edit here to restyle the whole chrome.
    pub(crate) fn theme() -> PluginShellTheme {
        let font = crate::components::plugin_shell_text::shell_font_theme();
        PluginShellTheme {
            titlebar_bg: rgb(24, 25, 28),
            content_bg: rgb(0, 0, 0),
            border: rgb(44, 46, 51),
            title_text: rgb(220, 221, 225),
            status_text: rgb(150, 152, 158),
            error_text: rgb(229, 115, 115),
            glyph: rgb(205, 206, 210),
            glyph_active: rgb(245, 246, 248),
            button_hover: rgb(45, 47, 53),
            close_hover: rgb(196, 43, 43),
            titlebar_h: 32,
            border_px: 1,
            button_w: 46,
            resize_grab: 6,
            title_pad: 12,
            title_em: crate::theme::typography::PLUGIN_TITLE,
            status_em: crate::theme::typography::UI_SM,
            font,
        }
    }

    fn hwnd_from(handle: u64) -> HWND {
        HWND(handle as *mut core::ffi::c_void)
    }

    fn black_brush() -> HBRUSH {
        HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0)
    }

    fn dpi_scale(hwnd: HWND) -> f32 {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }

    fn scaled(hwnd: HWND, logical: i32) -> i32 {
        (logical as f32 * dpi_scale(hwnd)).round() as i32
    }

    fn titlebar_h(hwnd: HWND) -> i32 {
        scaled(hwnd, theme().titlebar_h)
    }

    /// Logical title line height for vertically centered single-line chrome text.
    fn title_line_height_logical() -> f32 {
        16.0
    }

    /// Hit-test trace: WM_NCHITTEST fires on every mouse move, so this is
    /// debug-gated AND throttled (max 2/sec) — never an unbounded flood.
    fn log_hit_test(kind: &str, x: i32, y: i32) {
        static HIT_TEST_RATE: crate::forensic_trace::LogRateLimiter =
            crate::forensic_trace::LogRateLimiter::new(2);
        if crate::forensic_trace::plugin_trace_enabled() && HIT_TEST_RATE.allow() {
            eprintln!("[PluginEditor] hit_test {kind} x={x} y={y} (throttled 2/sec)");
        }
    }

    /// Click-path diagnostic on the content window (spec item 9): where did
    /// the click land, what would hit-test resolve to, and who holds focus and
    /// capture. Throttled to 4/sec; kept in safe mode (it is the minimal
    /// "focus/capture summary on click").
    fn log_click_path(content: HWND, x: i32, y: i32) {
        static CLICK_RATE: crate::forensic_trace::LogRateLimiter =
            crate::forensic_trace::LogRateLimiter::new(4);
        if !CLICK_RATE.allow() {
            return;
        }
        use windows::Win32::Graphics::Gdi::ClientToScreen;
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, GetFocus, IsWindowEnabled};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, WindowFromPoint,
            GA_ROOT,
        };
        fn class_of(hwnd: HWND) -> String {
            if hwnd.0.is_null() {
                return String::new();
            }
            let mut buf = [0u16; 64];
            let len = unsafe { GetClassNameW(hwnd, &mut buf) };
            if len > 0 {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            }
        }
        unsafe {
            let mut screen = POINT { x, y };
            let _ = ClientToScreen(content, &mut screen);
            let wfp = WindowFromPoint(screen);
            let child = ChildWindowFromPoint(content, POINT { x, y });
            let top = GetAncestor(content, GA_ROOT);
            let focus = GetFocus();
            let capture = GetCapture();
            let mut wfp_pid = 0u32;
            let wfp_tid = GetWindowThreadProcessId(wfp, Some(&mut wfp_pid));
            eprintln!(
                "[PluginClickPath][shell] client=({x},{y}) screen=({},{}) content=0x{:x} \
                 top=0x{:x} child_from_point=0x{:x} child_class='{}'",
                screen.x,
                screen.y,
                content.0 as u64,
                top.0 as u64,
                child.0 as u64,
                class_of(child),
            );
            eprintln!(
                "[PluginClickPath][shell] window_from_point=0x{:x} wfp_class='{}' wfp_enabled={} \
                 wfp_visible={} wfp_tid={wfp_tid} wfp_pid={wfp_pid} our_pid={} focus=0x{:x} \
                 capture=0x{:x}",
                wfp.0 as u64,
                class_of(wfp),
                IsWindowEnabled(wfp).as_bool(),
                IsWindowVisible(wfp).as_bool(),
                std::process::id(),
                focus.0 as u64,
                capture.0 as u64,
            );
        }
    }

    fn focus_deepest_plugin_child(content: HWND, x: i32, y: i32) {
        unsafe {
            let pt = POINT { x, y };
            let mut target = ChildWindowFromPoint(content, pt);
            if target == content || target.0.is_null() {
                target = content;
            }
            let _ = SetFocus(Some(target));
            eprintln!(
                "[PluginEditor] forwarding/focus plugin hwnd=0x{:x}",
                target.0 as u64
            );
        }
    }

    /// True if `hwnd` is a real Win32 dialog (class `#32770`). The shell and
    /// content windows are NOT dialogs; running `IsDialogMessageW` against them
    /// swallows Tab/arrow/Enter/Escape keystrokes meant for plugin controls.
    fn is_dialog_class(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
        if hwnd.0.is_null() {
            return false;
        }
        let mut buf = [0u16; 16];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == "#32770"
    }

    /// Nearest `#32770` dialog in the parent chain (including `hwnd` itself).
    /// `IsDialogMessageW` may only run against this — a message targeting a
    /// dialog *descendant* (e.g. a button) still needs dialog routing, while a
    /// non-dialog target must never be fed to IsDialogMessage.
    fn dialog_ancestor(hwnd: HWND) -> Option<HWND> {
        use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_PARENT};
        let mut cur = hwnd;
        let mut depth = 0;
        while !cur.0.is_null() && depth < 32 {
            if is_dialog_class(cur) {
                return Some(cur);
            }
            cur = unsafe { GetAncestor(cur, GA_PARENT) };
            depth += 1;
        }
        None
    }

    fn pump_shell_messages(top: HWND, content: HWND) {
        let _ = content;
        // Bounded drain: a message storm can never wedge the GPUI main thread
        // here; the caller re-pumps next tick.
        const MAX_PUMP_PER_CALL: u32 = 256;
        unsafe {
            let mut msg = MSG::default();
            let mut pumped = 0u32;
            while pumped < MAX_PUMP_PER_CALL
                && PeekMessageW(&mut msg, Some(top), 0, 0, PM_REMOVE).as_bool()
            {
                let _ = TranslateMessage(&msg);
                if let Some(dialog) = dialog_ancestor(msg.hwnd) {
                    if IsDialogMessageW(dialog, &msg).as_bool() {
                        pumped += 1;
                        continue;
                    }
                }
                DispatchMessageW(&msg);
                pumped += 1;
            }
            if pumped > 0 {
                static PUMP_TICK: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let n = PUMP_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if crate::forensic_trace::shell_layout_trace_enabled() && n % 600 == 0 {
                    eprintln!("[PluginEditor] modal/dialog message pump active drained={pumped}");
                }
            }
        }
    }

    fn border_w(hwnd: HWND) -> i32 {
        scaled(hwnd, theme().border_px).max(1)
    }

    /// Desired shell **client** size from plugin content + chrome (spec Part 2).
    fn shell_client_size(
        content_w: i32,
        content_h: i32,
        titlebar_h: i32,
        border: i32,
    ) -> (i32, i32) {
        (
            content_w.max(1),
            (content_h.max(1) + titlebar_h + border).max(1),
        )
    }

    /// Map a target client rect to outer window dimensions (DPI-aware).
    fn outer_size_for_client(hwnd: HWND, client_w: i32, client_h: i32) -> (i32, i32) {
        let style = unsafe { WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32) };
        let ex_style = WINDOW_EX_STYLE(unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 });
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let dpi = if dpi == 0 { 96 } else { dpi };
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: client_w.max(1),
            bottom: client_h.max(1),
        };
        unsafe {
            if AdjustWindowRectExForDpi(&mut rect, style, false, ex_style, dpi).is_ok() {
                return (
                    (rect.right - rect.left).max(1),
                    (rect.bottom - rect.top).max(1),
                );
            }
        }
        (client_w.max(1), client_h.max(1))
    }

    /// Authoritative layout for the native borderless shell (spec Part 1).
    #[derive(Debug, Clone, Copy)]
    struct ShellLayout {
        client_w: i32,
        client_h: i32,
        titlebar_h: i32,
        border: i32,
        content_x: i32,
        content_y: i32,
        content_w: i32,
        content_h: i32,
    }

    /// Authoritative shell layout: content rect below titlebar (spec Bug 3).
    fn compute_plugin_shell_layout(
        shell_client_w: i32,
        shell_client_h: i32,
        titlebar_h: i32,
        border: i32,
    ) -> (i32, i32, i32, i32) {
        let content_x = 0;
        let content_y = titlebar_h;
        let content_w = shell_client_w.max(1);
        let content_h = (shell_client_h - titlebar_h - border).max(1);
        (content_x, content_y, content_w, content_h)
    }

    fn compute_shell_layout(top: HWND) -> ShellLayout {
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(top, &mut client);
        }
        let client_w = client.right.max(0);
        let client_h = client.bottom.max(0);
        let th = titlebar_h(top);
        let bw = border_w(top);
        let (content_x, content_y, content_w, content_h) =
            compute_plugin_shell_layout(client_w, client_h, th, bw);
        ShellLayout {
            client_w,
            client_h,
            titlebar_h: th,
            border: bw,
            content_x,
            content_y,
            content_w,
            content_h,
        }
    }

    fn content_screen_rect(content: HWND) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetWindowRect(content, &mut rc);
        }
        rc
    }

    /// Recompute `content_rect`, position the content HWND, and queue resize.
    unsafe fn apply_shell_layout(top: HWND, inner: &ShellInner, reason: &str) -> (i32, i32) {
        let layout = compute_shell_layout(top);
        let content_raw = inner.content_hwnd.load(Ordering::Relaxed);
        let prev_x = inner.last_layout_x.load(Ordering::Relaxed);
        let prev_y = inner.last_layout_y.load(Ordering::Relaxed);
        let prev_w = inner.last_layout_w.load(Ordering::Relaxed);
        let prev_h = inner.last_layout_h.load(Ordering::Relaxed);
        let content_y = layout.titlebar_h.max(layout.content_y);
        let changed = prev_x != layout.content_x
            || prev_y != content_y
            || prev_w != layout.content_w
            || prev_h != layout.content_h;
        // Plugin child HWND must live strictly below the custom titlebar.
        // Only touch the window when the computed layout actually changed:
        // SetWindowPos on a window with a cross-process child subtree is a
        // synchronous send into the host UI thread. SWP_NOZORDER keeps normal
        // layout updates from re-raising; the one explicit raise happens on
        // attach (`mark_attached`).
        if content_raw != 0 && changed {
            let content = hwnd_from(content_raw);
            let _ = SetWindowPos(
                content,
                None,
                layout.content_x,
                content_y,
                layout.content_w,
                layout.content_h,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            eprintln!(
                "[plugin-editor-window] SetWindowPos content_hwnd=0x{content_raw:x} \
                 x={} y={} w={} h={}",
                layout.content_x, content_y, layout.content_w, layout.content_h
            );
        }
        if changed || crate::forensic_trace::shell_layout_trace_enabled() {
            eprintln!("[PluginEditor] titlebar_h={}", layout.titlebar_h);
            eprintln!(
                "[PluginEditor] plugin_child_rect x={} y={} w={} h={}",
                layout.content_x, content_y, layout.content_w, layout.content_h
            );
            eprintln!(
                "[PluginEditor] top_client={}x{}",
                layout.client_w, layout.client_h
            );
            eprintln!(
                "[PluginEditorResize] wrapper_client={}x{} titlebar_h={} content={}x{} \
                 child_pos=({},{})",
                layout.client_w,
                layout.client_h,
                layout.titlebar_h,
                layout.content_w,
                layout.content_h,
                layout.content_x,
                content_y,
            );
        }
        inner
            .last_layout_x
            .store(layout.content_x, Ordering::Relaxed);
        inner.last_layout_y.store(content_y, Ordering::Relaxed);
        inner
            .last_layout_w
            .store(layout.content_w, Ordering::Relaxed);
        inner
            .last_layout_h
            .store(layout.content_h, Ordering::Relaxed);
        inner.resize_w.store(layout.content_w, Ordering::Relaxed);
        inner.resize_h.store(layout.content_h, Ordering::Relaxed);
        if changed {
            inner.resize_pending.store(true, Ordering::Relaxed);
        }
        if changed || crate::forensic_trace::shell_layout_trace_enabled() {
            let shell_raw = top.0 as u64;
            let mut content_client_w = 0i32;
            let mut content_client_h = 0i32;
            let screen = if content_raw != 0 {
                let content = hwnd_from(content_raw);
                let mut cr = RECT::default();
                let _ = GetClientRect(content, &mut cr);
                content_client_w = (cr.right - cr.left).max(0);
                content_client_h = (cr.bottom - cr.top).max(0);
                let scr = content_screen_rect(content);
                format!("({},{},{},{})", scr.left, scr.top, scr.right, scr.bottom)
            } else {
                "none".to_string()
            };
            if crate::forensic_trace::shell_layout_trace_enabled() || changed {
                eprintln!("[plugin-shell-layout] reason={reason}");
                eprintln!("[plugin-shell-layout] shell_hwnd=0x{shell_raw:x}");
                eprintln!(
                    "[plugin-shell-layout] shell_client=(0,0,{},{})",
                    layout.client_w, layout.client_h
                );
                eprintln!("[plugin-shell-layout] titlebar_h={}", layout.titlebar_h);
                eprintln!("[plugin-shell-layout] border={}", layout.border);
                eprintln!(
                    "[plugin-shell-layout] computed_content=({},{},{},{})",
                    layout.content_x, layout.content_y, layout.content_w, layout.content_h
                );
                eprintln!("[plugin-shell-layout] content_hwnd=0x{content_raw:x}");
                eprintln!(
                    "[plugin-shell-layout] content_client=({content_client_w},{content_client_h})"
                );
                eprintln!("[plugin-shell-layout] content_screen={screen}");
            }
            let attached = inner.attached.load(Ordering::Relaxed);
            if changed {
                eprintln!(
                    "[plugin-shell-render] titlebar_only=true content_paint={}",
                    !attached
                );
            }
        }
        (layout.content_w, layout.content_h)
    }

    /// Strip classic OS frame styles and keep only borderless popup chrome.
    unsafe fn apply_borderless_styles(hwnd: HWND, owner: Option<HWND>) {
        let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
        let frame_bits = WS_CAPTION
            | WS_THICKFRAME
            | WS_BORDER
            | WS_DLGFRAME
            | WS_SYSMENU
            | WS_MINIMIZEBOX
            | WS_MAXIMIZEBOX;
        let borderless = WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS | WS_VISIBLE;
        let new_style = WINDOW_STYLE((style.0 & !frame_bits.0) | borderless.0);
        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style.0 as isize);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let new_ex = if owner.is_some() {
            (ex & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0
        } else {
            ex | WS_EX_APPWINDOW.0
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex as isize);
        if let Some(owner) = owner {
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner.0 as isize);
            eprintln!(
                "[plugin-editor-window] owner_applied owner_hwnd=0x{:x}",
                owner.0 as u64
            );
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        eprintln!(
            "[plugin-editor-window] shell_styles=borderless owner={}",
            owner.is_some()
        );
    }

    fn validated_owner(owner_hwnd: Option<u64>) -> Option<HWND> {
        let hwnd = owner_hwnd.map(hwnd_from)?;
        if hwnd.0.is_null() {
            return None;
        }
        if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            Some(hwnd)
        } else {
            eprintln!(
                "[plugin-editor-window] owner_hwnd invalid hwnd=0x{:x}",
                hwnd.0 as u64
            );
            None
        }
    }

    unsafe fn paint_shell_border(hdc: HDC, cw: i32, ch: i32, bw: i32, color: COLORREF) {
        // Themed window border: delegate the GDI strip-fill to the engine's
        // reusable software painter.
        sphere_graphic_engine::SoftwarePainter::frame_inset(hdc, cw, ch, bw, color);
    }
    fn button_w(hwnd: HWND) -> i32 {
        scaled(hwnd, theme().button_w)
    }

    /// Right-aligned button rects (min, max, close) for client width `cw`.
    fn button_rects(hwnd: HWND, cw: i32) -> [RECT; 3] {
        let bw = button_w(hwnd);
        let th = titlebar_h(hwnd);
        let close = RECT {
            left: cw - bw,
            top: 0,
            right: cw,
            bottom: th,
        };
        let max = RECT {
            left: cw - 2 * bw,
            top: 0,
            right: cw - bw,
            bottom: th,
        };
        let min = RECT {
            left: cw - 3 * bw,
            top: 0,
            right: cw - 2 * bw,
            bottom: th,
        };
        [min, max, close]
    }

    fn point_in(rc: &RECT, x: i32, y: i32) -> bool {
        x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
    }

    struct EnumFirstChild {
        found: u64,
    }

    unsafe extern "system" fn enum_first_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut EnumFirstChild);
        if ctx.found == 0 {
            ctx.found = hwnd.0 as u64;
            return BOOL(0);
        }
        BOOL(1)
    }

    /// Shared shell state, reachable from both `WndProc`s via `GWLP_USERDATA`.
    struct ShellInner {
        content_hwnd: AtomicU64,
        attached: AtomicBool,
        close_requested: AtomicBool,
        resize_pending: AtomicBool,
        resize_w: AtomicI32,
        resize_h: AtomicI32,
        last_layout_x: AtomicI32,
        last_layout_y: AtomicI32,
        last_layout_w: AtomicI32,
        last_layout_h: AtomicI32,
        shell_paint: AtomicU32,
        content_paint: AtomicU32,
        content_erase: AtomicU32,
        size_count: AtomicU32,
        hover_btn: AtomicU8,
        pressed_btn: AtomicU8,
        title: Mutex<String>,
        /// Overlay line drawn over the content area until the plugin attaches:
        /// `(text, is_error)`. Empty text = nothing drawn.
        status: Mutex<(String, bool)>,
        /// Main Futureboard window HWND at open time — used to recenter on the
        /// same monitor / over the app until the user moves the shell.
        owner_hwnd: AtomicU64,
        /// User dragged or resized via the custom chrome (spec Part 4).
        has_user_moved: AtomicBool,
        has_user_resized: AtomicBool,
        initial_auto_size_done: AtomicBool,
        /// `IPlugView::canResize` from the host. While false the wrapper is
        /// size-locked: no resize hit-test edges, and WM_GETMINMAXINFO pins
        /// min = max = current size so nothing (drag, snap, maximize) can open
        /// blank area around a fixed-size plugin view.
        resizable: AtomicBool,
        /// True while the shell itself drives SetWindowPos (plugin-requested
        /// resize, DPI change). Exempts programmatic resizes from the
        /// fixed-size min/max lock above.
        programmatic_resize: AtomicBool,
        /// Bare Space presses seen by the shell chrome, drained by `poll`.
        transport_toggles: AtomicU32,
    }

    impl ShellInner {
        fn new(title: String, status: String, owner_hwnd: Option<u64>) -> Arc<Self> {
            Arc::new(Self {
                content_hwnd: AtomicU64::new(0),
                attached: AtomicBool::new(false),
                close_requested: AtomicBool::new(false),
                resize_pending: AtomicBool::new(false),
                resize_w: AtomicI32::new(0),
                resize_h: AtomicI32::new(0),
                last_layout_x: AtomicI32::new(-1),
                last_layout_y: AtomicI32::new(-1),
                last_layout_w: AtomicI32::new(-1),
                last_layout_h: AtomicI32::new(-1),
                shell_paint: AtomicU32::new(0),
                content_paint: AtomicU32::new(0),
                content_erase: AtomicU32::new(0),
                size_count: AtomicU32::new(0),
                hover_btn: AtomicU8::new(BTN_NONE),
                pressed_btn: AtomicU8::new(BTN_NONE),
                title: Mutex::new(title),
                status: Mutex::new((status, false)),
                owner_hwnd: AtomicU64::new(owner_hwnd.unwrap_or(0)),
                has_user_moved: AtomicBool::new(false),
                has_user_resized: AtomicBool::new(false),
                initial_auto_size_done: AtomicBool::new(false),
                resizable: AtomicBool::new(true),
                programmatic_resize: AtomicBool::new(false),
                transport_toggles: AtomicU32::new(0),
            })
        }
    }

    fn monitor_work_area_for(reference: HWND) -> RECT {
        let mon = unsafe {
            if reference.0.is_null() {
                MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY)
            } else {
                MonitorFromWindow(reference, MONITOR_DEFAULTTONEAREST)
            }
        };
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
            mi.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        }
    }

    fn center_in_rect(area: RECT, outer_w: i32, outer_h: i32) -> (i32, i32) {
        let aw = area.right - area.left;
        let ah = area.bottom - area.top;
        (
            area.left + (aw - outer_w) / 2,
            area.top + (ah - outer_h) / 2,
        )
    }

    fn clamp_shell_position(x: i32, y: i32, outer_w: i32, outer_h: i32, work: RECT) -> (i32, i32) {
        let mut x = x;
        let mut y = y;
        if x < work.left {
            x = work.left;
        }
        if y < work.top {
            y = work.top;
        }
        if x + outer_w > work.right {
            x = (work.right - outer_w).max(work.left);
        }
        if y + outer_h > work.bottom {
            y = (work.bottom - outer_h).max(work.top);
        }
        (x, y)
    }

    /// Compute outer shell position centered over `owner_hwnd` (if valid) and
    /// clamped to the monitor work area (spec Part 2).
    fn center_shell_open_position(
        outer_w: i32,
        outer_h: i32,
        owner_hwnd: Option<u64>,
    ) -> (i32, i32, RECT) {
        let owner = owner_hwnd.map(hwnd_from).filter(|hwnd| !hwnd.0.is_null());
        let reference = owner.unwrap_or_default();
        let work = monitor_work_area_for(reference);
        let (mut x, mut y) = if let Some(owner) = owner {
            let mut owner_rect = RECT::default();
            if unsafe { GetWindowRect(owner, &mut owner_rect) }.is_ok() {
                let ow = owner_rect.right - owner_rect.left;
                let oh = owner_rect.bottom - owner_rect.top;
                (
                    owner_rect.left + (ow - outer_w) / 2,
                    owner_rect.top + (oh - outer_h) / 2,
                )
            } else {
                center_in_rect(work, outer_w, outer_h)
            }
        } else {
            center_in_rect(work, outer_w, outer_h)
        };
        (x, y) = clamp_shell_position(x, y, outer_w, outer_h, work);
        (x, y, work)
    }

    fn log_content_rect(top: HWND) {
        let layout = compute_shell_layout(top);
        eprintln!(
            "[plugin-editor-window] content_rect x={} y={} w={} h={}",
            layout.content_x, layout.content_y, layout.content_w, layout.content_h
        );
    }

    unsafe fn inner_ref<'a>(hwnd: HWND) -> Option<&'a ShellInner> {
        let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if raw == 0 {
            None
        } else {
            Some(unsafe { &*(raw as *const ShellInner) })
        }
    }

    unsafe fn reclaim_inner(hwnd: HWND) {
        let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if raw != 0 {
            unsafe {
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Arc::from_raw(raw as *const ShellInner));
            }
        }
    }

    unsafe fn install_inner(hwnd: HWND, inner: &Arc<ShellInner>) {
        let raw = Arc::into_raw(inner.clone()) as isize;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw);
        }
    }

    fn loword(lp: LPARAM) -> i32 {
        (lp.0 & 0xFFFF) as u16 as i16 as i32
    }
    fn hiword(lp: LPARAM) -> i32 {
        ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32
    }

    fn invalidate_titlebar(hwnd: HWND) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let th = titlebar_h(hwnd);
        let bar = RECT {
            left: 0,
            top: 0,
            right: rc.right,
            bottom: th,
        };
        unsafe {
            let _ = InvalidateRect(Some(hwnd), Some(&bar), false);
        }
    }

    fn button_at(hwnd: HWND, x: i32, y: i32) -> u8 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        if y >= titlebar_h(hwnd) {
            return BTN_NONE;
        }
        let [min, max, close] = button_rects(hwnd, rc.right);
        if point_in(&close, x, y) {
            BTN_CLOSE
        } else if point_in(&max, x, y) {
            BTN_MAX
        } else if point_in(&min, x, y) {
            BTN_MIN
        } else {
            BTN_NONE
        }
    }

    unsafe fn paint_title_gdi(
        hdc: HDC,
        trc: RECT,
        title: &str,
        em_px: f32,
        fg: COLORREF,
        dpi_scale: f32,
    ) {
        let mut buf: Vec<u16> = title.encode_utf16().collect();
        let mut g = trc;
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, fg);
        let height = (-(em_px * dpi_scale).round() as i32).max(-1);
        let family: Vec<u16> = crate::theme::SYSTEM_UI_FONT_FAMILY
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let font = CreateFontW(
            height,
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            FONT_QUALITY(0),
            FF_DONTCARE.0 as u32,
            PCWSTR(family.as_ptr()),
        );
        let old_font = if font.is_invalid() {
            None
        } else {
            Some(SelectObject(hdc, font.into()))
        };
        let _ = DrawTextW(
            hdc,
            &mut buf,
            &mut g,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        if let Some(old) = old_font {
            let _ = SelectObject(hdc, old);
        }
        if !font.is_invalid() {
            let _ = DeleteObject(font.into());
        }
    }

    fn paint_shell_titlebar(hwnd: HWND, inner: &ShellInner) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let cw = rc.right;
        let ch = rc.bottom;
        let th = titlebar_h(hwnd);
        let bw = border_w(hwnd);
        let scale = dpi_scale(hwnd);
        let t = theme();
        let active = unsafe { GetForegroundWindow() == hwnd };
        let title_fg = if active { t.title_text } else { t.status_text };

        // Futureboard border on all edges (no classic OS frame).
        unsafe {
            paint_shell_border(hdc, cw, ch, bw, t.border);
        }

        // Titlebar background.
        let bar = RECT {
            left: 0,
            top: 0,
            right: cw,
            bottom: th,
        };
        unsafe {
            let bg = CreateSolidBrush(t.titlebar_bg);
            FillRect(hdc, &bar, bg);
            let _ = DeleteObject(bg.into());
        }

        // Title text — DirectWrite (spec Part 2), Segoe UI GDI fallback (Part 9).
        let title = inner.title.lock().map(|t| t.clone()).unwrap_or_default();
        if !title.is_empty() {
            let pad = (t.title_pad as f32 * scale).round() as i32;
            let button_area = 3 * button_w(hwnd);
            let trc = RECT {
                left: pad,
                top: 0,
                right: (cw - button_area - pad).max(pad + 1),
                bottom: th,
            };
            // Font size is logical px; DirectWrite scales via pixels_per_dip.
            let em = t.title_em;
            static TITLE_LOG: Once = Once::new();
            TITLE_LOG.call_once(|| {
                eprintln!("[PluginEditor] title_font_family={}", t.font.family_primary);
                eprintln!("[PluginEditor] title_font_size={em}");
                eprintln!("[PluginEditor] title_text={title}");
            });
            let drew = crate::components::plugin_shell_text::draw_text_with_line_height(
                hdc,
                trc,
                &title,
                t.font.family_primary,
                t.font.weight_title,
                em,
                t.titlebar_bg,
                title_fg,
                crate::components::plugin_shell_text::TextAlign::LeftMiddle,
                scale,
                title_line_height_logical(),
            );
            if !drew {
                unsafe {
                    paint_title_gdi(hdc, trc, &title, em, title_fg, scale);
                }
            }
        }

        // Buttons.
        let hover = inner.hover_btn.load(Ordering::Relaxed);
        let [minr, maxr, closer] = button_rects(hwnd, cw);
        let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
        unsafe {
            paint_button(hdc, &minr, BTN_MIN, hover, false, scale, &t);
            paint_button(hdc, &maxr, BTN_MAX, hover, maximized, scale, &t);
            paint_button(hdc, &closer, BTN_CLOSE, hover, false, scale, &t);
        }

        // Bottom hairline last so text/buttons never overpaint it.
        unsafe {
            let border = RECT {
                left: 0,
                top: th - 1,
                right: cw,
                bottom: th,
            };
            let bb = CreateSolidBrush(t.border);
            FillRect(hdc, &border, bb);
            let _ = DeleteObject(bb.into());
        }

        let _ = unsafe { EndPaint(hwnd, &ps) };
    }

    unsafe fn paint_button(
        hdc: HDC,
        rc: &RECT,
        btn: u8,
        hover: u8,
        maximized: bool,
        scale: f32,
        t: &PluginShellTheme,
    ) {
        if hover == btn {
            let hb = unsafe {
                CreateSolidBrush(if btn == BTN_CLOSE {
                    t.close_hover
                } else {
                    t.button_hover
                })
            };
            unsafe {
                FillRect(hdc, rc, hb);
                let _ = DeleteObject(hb.into());
            }
        }
        let glyph_col = if hover == BTN_CLOSE && btn == BTN_CLOSE {
            t.glyph_active
        } else {
            t.glyph
        };
        let cx = (rc.left + rc.right) / 2;
        let cy = (rc.top + rc.bottom) / 2;
        let r = (5.0 * scale).round() as i32;
        unsafe {
            let pen = CreatePen(PS_SOLID, (1.0 * scale).max(1.0) as i32, glyph_col);
            let old = SelectObject(hdc, pen.into());
            match btn {
                BTN_MIN => {
                    let _ = MoveToEx(hdc, cx - r, cy, None);
                    let _ = LineTo(hdc, cx + r, cy);
                }
                BTN_MAX => {
                    if maximized {
                        // restore glyph: two offset rects
                        draw_rect_outline(hdc, cx - r + 2, cy - r, cx + r, cy + r - 2);
                        draw_rect_outline(hdc, cx - r, cy - r + 2, cx + r - 2, cy + r);
                    } else {
                        draw_rect_outline(hdc, cx - r, cy - r, cx + r, cy + r);
                    }
                }
                BTN_CLOSE => {
                    let _ = MoveToEx(hdc, cx - r, cy - r, None);
                    let _ = LineTo(hdc, cx + r + 1, cy + r + 1);
                    let _ = MoveToEx(hdc, cx + r, cy - r, None);
                    let _ = LineTo(hdc, cx - r - 1, cy + r + 1);
                }
                _ => {}
            }
            SelectObject(hdc, old);
            let _ = DeleteObject(pen.into());
        }
    }

    unsafe fn draw_rect_outline(hdc: HDC, l: i32, t: i32, r: i32, b: i32) {
        unsafe {
            let _ = MoveToEx(hdc, l, t, None);
            let _ = LineTo(hdc, r, t);
            let _ = LineTo(hdc, r, b);
            let _ = LineTo(hdc, l, b);
            let _ = LineTo(hdc, l, t);
        }
    }

    /// Claim a bare Space press for the DAW transport, mirroring the rule the
    /// plug-in host process applies to the windows it owns
    /// (`futureboard_plugin_host::platform::is_transport_toggle_key`): a fresh,
    /// unmodified Space only, and never one aimed at a text caret.
    ///
    /// Returns true when the key was claimed and must not reach `DefWindowProc`.
    unsafe fn claim_transport_key(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> bool {
        const VK_SPACE_W: usize = 0x20;
        const KEY_REPEAT_BIT: isize = 1 << 30;
        if wparam.0 != VK_SPACE_W || lparam.0 & KEY_REPEAT_BIT != 0 {
            return false;
        }
        unsafe {
            let held = |vk: i32| GetKeyState(vk) < 0;
            // VK_CONTROL / VK_MENU / VK_LWIN / VK_RWIN.
            if held(0x11) || held(0x12) || held(0x5B) || held(0x5C) {
                return false;
            }
            let focus = GetFocus();
            if !focus.0.is_null() && focus != hwnd {
                let mut class = [0u16; 128];
                let len = GetClassNameW(focus, &mut class);
                if len > 0 {
                    let name =
                        String::from_utf16_lossy(&class[..len as usize]).to_ascii_lowercase();
                    // A caret owns Space — renaming a preset must not start playback.
                    if name == "edit"
                        || name == "combobox"
                        || name.starts_with("richedit")
                        || name.contains("textbox")
                    {
                        return false;
                    }
                }
            }
            if let Some(inner) = inner_ref(hwnd) {
                inner.transport_toggles.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    unsafe extern "system" fn shell_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN if unsafe { claim_transport_key(hwnd, wparam, lparam) } => {
                LRESULT(0)
            }
            // Borderless: client area fills the whole window (no OS titlebar).
            WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),
            // Suppress classic non-client painting / activation frame.
            WM_NCPAINT => LRESULT(0),
            WM_NCACTIVATE => LRESULT(1),
            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: loword(lparam),
                    y: hiword(lparam),
                };
                unsafe {
                    let _ = ScreenToClient(hwnd, &mut pt);
                }
                let mut rc = RECT::default();
                unsafe {
                    let _ = GetClientRect(hwnd, &mut rc);
                }
                let (cw, ch) = (rc.right, rc.bottom);
                let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
                // Fixed-size editors (IPlugView::canResize == false) expose no
                // resize edges at all — dragging must never open blank area
                // around the plugin content (resize contract, spec item 1/8).
                let resizable = unsafe { inner_ref(hwnd) }
                    .map(|inner| inner.resizable.load(Ordering::Relaxed))
                    .unwrap_or(true);
                if !maximized && resizable {
                    let grab = scaled(hwnd, theme().resize_grab);
                    let l = pt.x < grab;
                    let r = pt.x >= cw - grab;
                    let t = pt.y < grab;
                    let b = pt.y >= ch - grab;
                    if t && l {
                        return LRESULT(HTTOPLEFT as isize);
                    }
                    if t && r {
                        return LRESULT(HTTOPRIGHT as isize);
                    }
                    if b && l {
                        return LRESULT(HTBOTTOMLEFT as isize);
                    }
                    if b && r {
                        return LRESULT(HTBOTTOMRIGHT as isize);
                    }
                    if l {
                        return LRESULT(HTLEFT as isize);
                    }
                    if r {
                        return LRESULT(HTRIGHT as isize);
                    }
                    if t {
                        return LRESULT(HTTOP as isize);
                    }
                    if b {
                        return LRESULT(HTBOTTOM as isize);
                    }
                }
                let th = titlebar_h(hwnd);
                if pt.y < th {
                    log_hit_test("titlebar", pt.x, pt.y);
                    if button_at(hwnd, pt.x, pt.y) != BTN_NONE {
                        return LRESULT(HTCLIENT as isize); // we handle button clicks
                    }
                    return LRESULT(HTCAPTION as isize); // drag + dbl-click maximize
                }
                log_hit_test("content", pt.x, pt.y);
                // Plugin content HWND owns input below the titlebar.
                LRESULT(HTCLIENT as isize)
            }
            WM_ACTIVATE => {
                // Repaint chrome, then let DefWindowProc run its default
                // activation handling (focus management) — the shell does not
                // own this message.
                invalidate_titlebar(hwnd);
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_ENTERSIZEMOVE => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    inner.has_user_moved.store(true, Ordering::Relaxed);
                    inner.has_user_resized.store(true, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                // Fixed-size editor: pin min = max = current outer size so no
                // path (edge drag, Aero snap, maximize, dbl-click caption) can
                // resize the wrapper away from the plugin view size. Shell-
                // driven resizes (plugin resizeView, DPI change) set
                // `programmatic_resize` and bypass the lock.
                if lparam.0 != 0 {
                    if let Some(inner) = unsafe { inner_ref(hwnd) } {
                        if !inner.resizable.load(Ordering::Relaxed)
                            && !inner.programmatic_resize.load(Ordering::Relaxed)
                        {
                            let mut wr = RECT::default();
                            if unsafe { GetWindowRect(hwnd, &mut wr) }.is_ok() {
                                let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                                let size = POINT {
                                    x: (wr.right - wr.left).max(1),
                                    y: (wr.bottom - wr.top).max(1),
                                };
                                mmi.ptMinTrackSize = size;
                                mmi.ptMaxTrackSize = size;
                                mmi.ptMaxSize = size;
                                return LRESULT(0);
                            }
                        }
                    }
                }
                // Resizable: maximize to the work area (don't cover the
                // taskbar) and set a sensible minimum.
                if lparam.0 != 0 {
                    let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
                    let mut mi = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
                        let work = mi.rcWork;
                        let mon_rc = mi.rcMonitor;
                        mmi.ptMaxPosition.x = work.left - mon_rc.left;
                        mmi.ptMaxPosition.y = work.top - mon_rc.top;
                        mmi.ptMaxSize.x = work.right - work.left;
                        mmi.ptMaxSize.y = work.bottom - work.top;
                    }
                    let scale = dpi_scale(hwnd);
                    mmi.ptMinTrackSize.x = (320.0 * scale) as i32;
                    mmi.ptMinTrackSize.y = (160.0 * scale) as i32;
                }
                LRESULT(0)
            }
            WM_SIZE => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    inner.size_count.fetch_add(1, Ordering::Relaxed);
                    let reason = if unsafe { IsZoomed(hwnd) }.as_bool() {
                        "WM_SIZE:maximize"
                    } else {
                        "WM_SIZE"
                    };
                    unsafe {
                        apply_shell_layout(hwnd, inner, reason);
                    }
                    invalidate_titlebar(hwnd);
                }
                LRESULT(0)
            }
            0x0003 /* WM_MOVE */ => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    unsafe {
                        apply_shell_layout(hwnd, inner, "WM_MOVE");
                    }
                    invalidate_titlebar(hwnd);
                }
                LRESULT(0)
            }
            0x0047 /* WM_WINDOWPOSCHANGED */ | 0x0018 /* WM_SHOWWINDOW */ => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    let reason = if msg == 0x0047 {
                        "WM_WINDOWPOSCHANGED"
                    } else {
                        "WM_SHOWWINDOW"
                    };
                    unsafe {
                        apply_shell_layout(hwnd, inner, reason);
                    }
                    invalidate_titlebar(hwnd);
                }
                // Not host-owned: always fall through to default handling.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            0x02E0 /* WM_DPICHANGED */ => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    let suggested = unsafe { *(lparam.0 as *const RECT) };
                    // System-driven resize: exempt from the fixed-size lock
                    // (titlebar height and outer bounds are DPI-dependent).
                    inner.programmatic_resize.store(true, Ordering::Relaxed);
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            suggested.left,
                            suggested.top,
                            suggested.right - suggested.left,
                            suggested.bottom - suggested.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        apply_shell_layout(hwnd, inner, "WM_DPICHANGED");
                    }
                    inner.programmatic_resize.store(false, Ordering::Relaxed);
                    invalidate_titlebar(hwnd);
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                let layout = compute_shell_layout(hwnd);
                let t = theme();
                unsafe {
                    if let Some(inner) = inner_ref(hwnd) {
                        if inner.content_hwnd.load(Ordering::Relaxed) != 0 {
                            paint_shell_border(hdc, layout.client_w, layout.client_h, layout.border, t.border);
                            let bar = RECT {
                                left: 0,
                                top: 0,
                                right: layout.client_w,
                                bottom: layout.titlebar_h,
                            };
                            let bg = CreateSolidBrush(t.titlebar_bg);
                            FillRect(hdc, &bar, bg);
                            let _ = DeleteObject(bg.into());
                        } else {
                            let mut rc = RECT::default();
                            let _ = GetClientRect(hwnd, &mut rc);
                            FillRect(hdc, &rc, black_brush());
                        }
                    } else {
                        let mut rc = RECT::default();
                        let _ = GetClientRect(hwnd, &mut rc);
                        FillRect(hdc, &rc, black_brush());
                    }
                }
                LRESULT(1)
            }
            WM_PAINT => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    inner.shell_paint.fetch_add(1, Ordering::Relaxed);
                    paint_shell_titlebar(hwnd, inner);
                } else {
                    return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    let btn = button_at(hwnd, loword(lparam), hiword(lparam));
                    if inner.hover_btn.swap(btn, Ordering::Relaxed) != btn {
                        invalidate_titlebar(hwnd);
                    }
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = unsafe { TrackMouseEvent(&mut tme) };
                }
                LRESULT(0)
            }
            WM_NCMOUSEMOVE => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    if inner.hover_btn.swap(BTN_NONE, Ordering::Relaxed) != BTN_NONE {
                        invalidate_titlebar(hwnd);
                    }
                }
                // Non-client move is not host-owned (resize/caption tracking).
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_MOUSELEAVE => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    if inner.hover_btn.swap(BTN_NONE, Ordering::Relaxed) != BTN_NONE {
                        invalidate_titlebar(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    let btn = button_at(hwnd, loword(lparam), hiword(lparam));
                    if btn != BTN_NONE {
                        // Chrome button press: this is the ONLY place the
                        // shell takes mouse capture, and only for the duration
                        // of the button press (released on WM_LBUTTONUP).
                        inner.pressed_btn.store(btn, Ordering::Relaxed);
                        unsafe {
                            let _ = SetCapture(hwnd);
                        }
                        return LRESULT(0);
                    }
                }
                // Not on a chrome button — not host-owned; don't consume.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_LBUTTONUP => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    let pressed = inner.pressed_btn.swap(BTN_NONE, Ordering::Relaxed);
                    if pressed != BTN_NONE {
                        unsafe {
                            let _ = ReleaseCapture();
                        }
                        let over = button_at(hwnd, loword(lparam), hiword(lparam));
                        if pressed == over {
                            match pressed {
                                BTN_MIN => unsafe {
                                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                                },
                                BTN_MAX => unsafe {
                                    // Fixed-size editors cannot maximize —
                                    // it would only add blank area.
                                    if inner.resizable.load(Ordering::Relaxed) {
                                        if IsZoomed(hwnd).as_bool() {
                                            let _ = ShowWindow(hwnd, SW_RESTORE);
                                        } else {
                                            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
                                        }
                                    }
                                },
                                BTN_CLOSE => {
                                    inner.close_requested.store(true, Ordering::Relaxed);
                                }
                                _ => {}
                            }
                        }
                        return LRESULT(0);
                    }
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_CLOSE => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    inner.close_requested.store(true, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_NCDESTROY => {
                unsafe { reclaim_inner(hwnd) };
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    unsafe extern "system" fn content_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            // The content surface can hold focus before the plug-in's own view
            // takes it (and after the view is destroyed), so it claims Space too.
            WM_KEYDOWN | WM_SYSKEYDOWN if unsafe { claim_transport_key(hwnd, wparam, lparam) } => {
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                if let Some(inner) = unsafe { inner_ref(hwnd) } {
                    inner.content_erase.fetch_add(1, Ordering::Relaxed);
                    if inner.attached.load(Ordering::Relaxed) {
                        // Plugin attached: fill with the editor wrapper
                        // background so any area the plugin view does NOT
                        // cover (constrained/fixed-size view smaller than the
                        // wrapper for a frame, snap-back in flight) shows
                        // clean background instead of stale garbage. The
                        // plugin's own HWND sits above this surface
                        // (WS_CLIPCHILDREN child or overlay), so this never
                        // paints over plugin content.
                        let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                        let mut rc = RECT::default();
                        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
                        unsafe {
                            let bg = CreateSolidBrush(theme().content_bg);
                            FillRect(hdc, &rc, bg);
                            let _ = DeleteObject(bg.into());
                        }
                        static ERASE_RATE: crate::forensic_trace::LogRateLimiter =
                            crate::forensic_trace::LogRateLimiter::new(1);
                        if ERASE_RATE.allow() {
                            eprintln!(
                                "[plugin-content-hwnd] WM_ERASEBKGND fill=content_bg attached=true"
                            );
                        }
                        return LRESULT(1);
                    }
                }
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                let mut rc = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut rc) };
                unsafe { FillRect(hdc, &rc, black_brush()) };
                LRESULT(1)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE => {
                let x = loword(lparam);
                let y = hiword(lparam);
                if msg == WM_LBUTTONDOWN {
                    log_hit_test("content", x, y);
                    // Focus/capture + hit-test summary on click (kept in safe
                    // mode; throttled).
                    log_click_path(hwnd, x, y);
                    if !crate::forensic_trace::plugin_editor_safe_mode() {
                        focus_deepest_plugin_child(hwnd, x, y);
                    }
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_PAINT => {
                let inner = unsafe { inner_ref(hwnd) };
                let attached = inner
                    .map(|i| {
                        i.content_paint.fetch_add(1, Ordering::Relaxed);
                        i.attached.load(Ordering::Relaxed)
                    })
                    .unwrap_or(false);
                let mut ps = PAINTSTRUCT::default();
                let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
                if attached {
                    // Clean background under/around the plugin view — exposed
                    // strips after a resize must never keep stale pixels.
                    let t = theme();
                    let mut rc = RECT::default();
                    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
                    unsafe {
                        let bg = CreateSolidBrush(t.content_bg);
                        FillRect(hdc, &rc, bg);
                        let _ = DeleteObject(bg.into());
                    }
                } else {
                    let t = theme();
                    let mut rc = RECT::default();
                    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
                    unsafe {
                        let bg = CreateSolidBrush(t.content_bg);
                        FillRect(hdc, &rc, bg);
                        let _ = DeleteObject(bg.into());
                    }
                    // Loading / error overlay text — DirectWrite, GDI fallback.
                    if let Some(inner) = inner {
                        let (text, is_error) =
                            inner.status.lock().map(|s| s.clone()).unwrap_or_default();
                        if !text.is_empty() {
                            let fg = if is_error {
                                t.error_text
                            } else {
                                t.status_text
                            };
                            let scale = dpi_scale(hwnd);
                            let em = t.status_em;
                            let drew = crate::components::plugin_shell_text::draw_text(
                                hdc,
                                rc,
                                &text,
                                t.font.family_primary,
                                t.font.weight_body,
                                em,
                                t.content_bg,
                                fg,
                                crate::components::plugin_shell_text::TextAlign::Center,
                                scale,
                            );
                            if !drew {
                                let lines: Vec<&str> = text.lines().collect();
                                let line_h = (em * scale * 1.45).round().max(16.0) as i32;
                                let total_h = line_h * lines.len().max(1) as i32;
                                let start_y = rc.top + ((rc.bottom - rc.top) - total_h) / 2;
                                unsafe {
                                    SetBkMode(hdc, TRANSPARENT);
                                    SetTextColor(hdc, fg);
                                    for (line_index, line) in lines.iter().enumerate() {
                                        let mut buf: Vec<u16> = line.encode_utf16().collect();
                                        let mut g = RECT {
                                            left: rc.left,
                                            top: start_y + line_h * line_index as i32,
                                            right: rc.right,
                                            bottom: start_y + line_h * (line_index as i32 + 1),
                                        };
                                        let _ = DrawTextW(
                                            hdc,
                                            &mut buf,
                                            &mut g,
                                            DT_SINGLELINE
                                                | DT_VCENTER
                                                | DT_NOPREFIX
                                                | windows::Win32::Graphics::Gdi::DT_CENTER,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                let _ = unsafe { EndPaint(hwnd, &ps) };
                LRESULT(0)
            }
            WM_NCDESTROY => {
                unsafe { reclaim_inner(hwnd) };
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn ensure_classes() {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| unsafe {
            let hinstance = GetModuleHandleW(None).unwrap_or_default();
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
            let shell = WNDCLASSW {
                lpfnWndProc: Some(shell_wndproc),
                lpszClassName: SHELL_CLASS,
                hInstance: hinstance.into(),
                hCursor: cursor,
                hbrBackground: black_brush(),
                ..Default::default()
            };
            RegisterClassW(&shell);
            let content = WNDCLASSW {
                lpfnWndProc: Some(content_wndproc),
                lpszClassName: CONTENT_CLASS,
                hInstance: hinstance.into(),
                hCursor: cursor,
                hbrBackground: black_brush(),
                ..Default::default()
            };
            RegisterClassW(&content);
        });
    }

    /// Apply DWM window polish (spec Part 4) via the engine's reusable window
    /// effects. Each attribute is best-effort and runtime-guarded; unsupported
    /// attributes on older Windows are ignored.
    fn apply_dwm_polish(hwnd: HWND, t: &PluginShellTheme) {
        use sphere_graphic_engine::{CornerPreference, DwmChromeOptions, DwmWindowEffects};
        let result = DwmWindowEffects::apply(
            hwnd,
            &DwmChromeOptions {
                dark_mode: true,
                corner: CornerPreference::Round,
                border_color: Some(t.border.0),
                caption_color: Some(t.titlebar_bg.0),
                disable_nc_rendering: true,
            },
        );
        eprintln!(
            "[plugin-shell-dwm] dark_mode={} rounded_corner={} border_color=theme available={}",
            result.dark_ok,
            if result.rounded { "round" } else { "none" },
            result.available
        );
        eprintln!(
            "[plugin-shell-text] renderer=DirectWriteCustomRenderer d2d=false font={}",
            t.font.family_primary
        );
    }

    pub struct NativeEditorShell {
        inner: Arc<ShellInner>,
        top_hwnd: u64,
        content_hwnd: u64,
    }

    impl NativeEditorShell {
        /// Create a borderless, custom-chrome top-level window plus a `WS_CHILD`
        /// content window filling the area below the drawn titlebar.
        /// `content_w`/`h` is the desired initial content (below-titlebar) size.
        /// `owner_hwnd` is the main Futureboard window used to center on open.
        pub fn create(
            title: &str,
            content_w: i32,
            content_h: i32,
            owner_hwnd: Option<u64>,
        ) -> Option<Self> {
            ensure_classes();
            let status = format!("Loading Plugin\n{title}");
            let inner = ShellInner::new(title.to_string(), status, owner_hwnd);
            let scale = owner_hwnd.map(hwnd_from).map(dpi_scale).unwrap_or(1.0);
            let th = (theme().titlebar_h as f32 * scale).round() as i32;
            let bw = (theme().border_px as f32 * scale).round().max(1.0) as i32;
            // Borderless: outer client = content + titlebar + bottom border strip.
            let (client_w, client_h) = shell_client_size(content_w, content_h, th, bw);
            let win_w = client_w;
            let win_h = client_h;
            let (pos_x, pos_y, work) = center_shell_open_position(win_w, win_h, owner_hwnd);
            eprintln!(
                "[plugin-editor-window] center_on_open monitor=work_area=({},{},{},{})",
                work.left, work.top, work.right, work.bottom
            );
            eprintln!(
                "[plugin-editor-window] initial_size content={content_w}x{content_h} shell={win_w}x{win_h}"
            );
            eprintln!("[plugin-editor-window] positioned x={pos_x} y={pos_y} w={win_w} h={win_h}");
            let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();

            unsafe {
                let owner = validated_owner(owner_hwnd);
                let ex_style = if owner.is_some() {
                    WS_EX_TOOLWINDOW
                } else {
                    WS_EX_APPWINDOW
                };
                let top = CreateWindowExW(
                    ex_style,
                    SHELL_CLASS,
                    PCWSTR(title_w.as_ptr()),
                    // Borderless custom chrome — no WS_CAPTION/WS_THICKFRAME/WS_BORDER.
                    // Resize via WM_NCHITTEST; min/max/close via drawn buttons.
                    WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    pos_x,
                    pos_y,
                    win_w,
                    win_h,
                    owner,
                    None::<HMENU>,
                    None,
                    None,
                )
                .ok()?;
                apply_borderless_styles(top, owner);
                let (outer_w, outer_h) = outer_size_for_client(top, client_w, client_h);
                if outer_w != win_w || outer_h != win_h {
                    let _ = SetWindowPos(
                        top,
                        None,
                        pos_x,
                        pos_y,
                        outer_w,
                        outer_h,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                install_inner(top, &inner);
                // Native window polish: immersive dark mode, rounded corners,
                // themed border (spec Part 4). Runtime-guarded — older Windows
                // simply ignores unknown attributes.
                apply_dwm_polish(top, &theme());
                // Apply WM_NCCALCSIZE now so the client fills the window.
                let _ = SetWindowPos(
                    top,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );

                let layout = compute_shell_layout(top);

                let content = match CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    CONTENT_CLASS,
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    layout.content_x,
                    layout.content_y,
                    layout.content_w,
                    layout.content_h,
                    Some(top),
                    None::<HMENU>,
                    None,
                    None,
                ) {
                    Ok(c) => c,
                    Err(_) => {
                        let _ = DestroyWindow(top);
                        return None;
                    }
                };
                install_inner(content, &inner);
                inner
                    .content_hwnd
                    .store(content.0 as u64, Ordering::Relaxed);
                apply_shell_layout(top, &inner, "initial_open");

                let _ = ShowWindow(top, SW_SHOW);
                let _ = UpdateWindow(top);

                // The click that opened the editor may have left mouse capture
                // on a GPUI surface of this thread; a stale capture steals the
                // next click intended for the plugin content. Release it.
                {
                    use windows::Win32::UI::Input::KeyboardAndMouse::GetCapture;
                    let captured = GetCapture();
                    if !captured.0.is_null() {
                        let _ = ReleaseCapture();
                        eprintln!(
                            "[PluginEditorInput] released_stale_capture=0x{:x}",
                            captured.0 as u64
                        );
                    }
                }

                eprintln!("[plugin-editor-window] shell_kind=native_borderless");
                let top_style = GetWindowLongPtrW(top, GWL_STYLE);
                let child_style = GetWindowLongPtrW(content, GWL_STYLE);
                eprintln!(
                    "[PluginEditor] window styles top=0x{top_style:08x} child=0x{child_style:08x}"
                );
                eprintln!(
                    "[plugin-editor-window] shell_hwnd=0x{:x} content_hwnd=0x{:x} content_parent=shell_hwnd titlebar_h={} content={}x{}",
                    top.0 as u64,
                    content.0 as u64,
                    layout.titlebar_h,
                    layout.content_w,
                    layout.content_h
                );
                eprintln!(
                    "[PluginEditor] child rect=({},{},{},{})",
                    layout.content_x,
                    layout.content_y,
                    layout.content_x + layout.content_w,
                    layout.content_y + layout.content_h
                );
                eprintln!("[plugin-editor-window] native_content_region_reserved=true");
                log_content_rect(top);
                let dpi = GetDpiForWindow(top);
                let dpi_scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
                eprintln!("[UI] dpi_scale={dpi_scale:.3}");
                eprintln!("[UI] default_font={}", crate::theme::FONT_FAMILY);
                eprintln!(
                    "[UI] default_font_size={}",
                    crate::theme::typography::UI_SM as u32
                );
                eprintln!(
                    "[PluginEditor] wrapper_title_font_size={}",
                    crate::theme::typography::PLUGIN_TITLE
                );
                eprintln!("[PluginEditor] dpi={dpi}");
                eprintln!("[PluginEditor] titlebar_height={}", layout.titlebar_h);
                eprintln!("[PluginEditor] title_text={title}");
                eprintln!("[Fonts] default_ui_font={}", crate::theme::FONT_FAMILY);
                eprintln!(
                    "[PluginEditor] title_font_family={}",
                    theme().font.family_primary
                );
                eprintln!(
                    "[PluginEditor] title_font_size={}",
                    crate::theme::typography::PLUGIN_TITLE
                );
                eprintln!(
                    "[PluginEditor] plugin_child_rect x={} y={} w={} h={}",
                    layout.content_x, layout.content_y, layout.content_w, layout.content_h
                );
                eprintln!(
                    "[PluginEditor] top_client={}x{}",
                    layout.client_w, layout.client_h
                );

                Some(Self {
                    inner,
                    top_hwnd: top.0 as u64,
                    content_hwnd: content.0 as u64,
                })
            }
        }

        /// Host-owned (detached) mode: the bridge plugin-host process owns the
        /// real top-level editor window, so the main app creates NO window at
        /// all — there is no main-thread HWND that the foreign plugin view is
        /// ever parented under. This is what breaks the cross-process
        /// input-queue coupling that froze the GPUI main thread.
        ///
        /// The returned shell is a *proxy*: `top_hwnd`/`content_hwnd` are 0 and
        /// every window method degrades to a harmless no-op (Win32 calls on a
        /// null HWND fail without side effects). It exists only so the existing
        /// session/state machinery can keep operating unchanged; the visible
        /// window and all of its input live entirely in the host process.
        pub fn host_owned_proxy(title: &str) -> Self {
            let inner = ShellInner::new(title.to_string(), format!("Opening: {title}"), None);
            Self {
                inner,
                top_hwnd: 0,
                content_hwnd: 0,
            }
        }

        /// True for a [`host_owned_proxy`] shell (no main-owned window).
        pub fn is_host_owned_proxy(&self) -> bool {
            self.top_hwnd == 0
        }

        pub fn top_hwnd(&self) -> u64 {
            self.top_hwnd
        }
        pub fn content_hwnd(&self) -> u64 {
            self.content_hwnd
        }

        pub fn content_size(&self) -> (i32, i32) {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd_from(self.content_hwnd), &mut rc);
            }
            ((rc.right - rc.left).max(1), (rc.bottom - rc.top).max(1))
        }

        /// Mark the plugin view attached: stop the loading/black fill and force
        /// the content child visible + on top + repainted (spec Part 4/7).
        pub fn mark_attached(&self) {
            self.inner.attached.store(true, Ordering::Relaxed);
            if let Ok(mut s) = self.inner.status.lock() {
                *s = (String::new(), false);
            }
            eprintln!("[plugin-editor-window] loading_overlay_visible=false");
            invalidate_titlebar(hwnd_from(self.top_hwnd));
            // Single explicit raise of the content child at attach time; normal
            // layout updates use SWP_NOZORDER (see `apply_shell_layout`).
            unsafe {
                let _ = SetWindowPos(
                    hwnd_from(self.content_hwnd),
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            self.ensure_visible_zorder();
        }

        /// Recompute and apply the authoritative content rect (spec Part 1/2).
        pub fn apply_content_layout(&self) -> (i32, i32) {
            let top = hwnd_from(self.top_hwnd);
            unsafe { apply_shell_layout(top, &self.inner, "apply_content_layout") }
        }

        /// Gap diagnostics between shell content, host HWND, and plugin child (spec Part 3).
        pub fn log_black_gap_check(&self, host_hwnd: u64) {
            let top = hwnd_from(self.top_hwnd);
            let layout = compute_shell_layout(top);
            let content = hwnd_from(self.content_hwnd);
            let mut host_w = 0i32;
            let mut host_h = 0i32;
            let mut plugin_child_w = 0i32;
            let mut plugin_child_h = 0i32;
            let mut gap_left = 0i32;
            let mut gap_top = 0i32;
            let mut gap_right = 0i32;
            let mut gap_bottom = 0i32;
            if host_hwnd != 0 {
                let host = hwnd_from(host_hwnd);
                let mut rc = RECT::default();
                if unsafe { GetClientRect(host, &mut rc) }.is_ok() {
                    host_w = (rc.right - rc.left).max(0);
                    host_h = (rc.bottom - rc.top).max(0);
                }
                let mut host_screen = RECT::default();
                if unsafe { GetWindowRect(host, &mut host_screen) }.is_ok() {
                    let mut origin = POINT {
                        x: host_screen.left,
                        y: host_screen.top,
                    };
                    let _ = unsafe { ScreenToClient(content, &mut origin) };
                    gap_left = origin.x.max(0);
                    gap_top = origin.y.max(0);
                    gap_right = (layout.content_w - gap_left - host_w).max(0);
                    gap_bottom = (layout.content_h - gap_top - host_h).max(0);
                }
                let mut child_ctx = EnumFirstChild { found: 0 };
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;
                    let _ = EnumChildWindows(
                        Some(host),
                        Some(enum_first_child),
                        LPARAM(&mut child_ctx as *mut EnumFirstChild as isize),
                    );
                }
                if child_ctx.found != 0 {
                    let mut rc = RECT::default();
                    if unsafe { GetClientRect(hwnd_from(child_ctx.found), &mut rc) }.is_ok() {
                        plugin_child_w = (rc.right - rc.left).max(0);
                        plugin_child_h = (rc.bottom - rc.top).max(0);
                    }
                }
            }
            eprintln!("[plugin-shell-gap-check] content_w={}", layout.content_w);
            eprintln!("[plugin-shell-gap-check] content_h={}", layout.content_h);
            eprintln!("[plugin-shell-gap-check] host_w={host_w}");
            eprintln!("[plugin-shell-gap-check] host_h={host_h}");
            eprintln!("[plugin-shell-gap-check] child_w={plugin_child_w}");
            eprintln!("[plugin-shell-gap-check] child_h={plugin_child_h}");
            eprintln!("[plugin-shell-gap-check] gap_left={gap_left}");
            eprintln!("[plugin-shell-gap-check] gap_top={gap_top}");
            eprintln!("[plugin-shell-gap-check] gap_right={gap_right}");
            eprintln!("[plugin-shell-gap-check] gap_bottom={gap_bottom}");
            if host_hwnd != 0 {
                let host_fits = host_w == layout.content_w && host_h == layout.content_h;
                let child_fits =
                    plugin_child_w == 0 || (plugin_child_w == host_w && plugin_child_h == host_h);
                eprintln!(
                    "[vst3-editor-audit] child hwnd rect matches host={}",
                    host_fits && child_fits
                );
            }
        }

        /// Update the content overlay shown until attach (loading / error text).
        pub fn set_status(&self, text: &str, is_error: bool) {
            if is_error {
                self.inner.attached.store(false, Ordering::Relaxed);
            }
            if let Ok(mut s) = self.inner.status.lock() {
                *s = (text.to_string(), is_error);
            }
            unsafe {
                let _ = InvalidateRect(Some(hwnd_from(self.content_hwnd)), None, false);
            }
        }

        pub fn ensure_visible_zorder(&self) {
            let (cw, ch) = self.apply_content_layout();
            let content = hwnd_from(self.content_hwnd);
            // Async invalidation only. No UpdateWindow / RDW_UPDATENOW /
            // RDW_ALLCHILDREN here: the content child hosts a cross-process
            // (host-owned) subtree, and a synchronous repaint from the main
            // thread blocks until the host pump services WM_PAINT. The host
            // repaints its own subtree from its own pump.
            unsafe {
                let _ = ShowWindow(content, SW_SHOW);
                let _ = InvalidateRect(Some(content), None, false);
            }
            eprintln!(
                "[plugin-editor-window] ensure_visible_zorder content_hwnd=0x{:x} size={cw}x{ch}",
                self.content_hwnd
            );
        }

        pub fn has_user_moved(&self) -> bool {
            self.inner.has_user_moved.load(Ordering::Relaxed)
        }

        pub fn owner_hwnd(&self) -> Option<u64> {
            let raw = self.inner.owner_hwnd.load(Ordering::Relaxed);
            if raw == 0 {
                None
            } else {
                Some(raw)
            }
        }

        /// Clamp a preferred content size to the monitor work area minus titlebar.
        pub fn clamp_content_to_work_area(
            &self,
            content_w: i32,
            content_h: i32,
        ) -> (i32, i32, bool) {
            let top = hwnd_from(self.top_hwnd);
            let reference = self.owner_hwnd().map(hwnd_from).unwrap_or(top);
            let work = monitor_work_area_for(reference);
            let th = titlebar_h(top);
            let bw = border_w(top);
            let max_w = (work.right - work.left).max(1);
            let max_h = ((work.bottom - work.top) - th - bw).max(1);
            let cw = content_w.clamp(1, max_w);
            let ch = content_h.clamp(1, max_h);
            let clamped = cw != content_w || ch != content_h;
            (cw, ch, clamped)
        }

        pub fn shell_outer_size(&self) -> (i32, i32) {
            let top = hwnd_from(self.top_hwnd);
            let mut outer = RECT::default();
            unsafe {
                let _ = GetWindowRect(top, &mut outer);
            }
            (
                (outer.right - outer.left).max(1),
                (outer.bottom - outer.top).max(1),
            )
        }

        pub fn shell_dpi(&self) -> u32 {
            let top = hwnd_from(self.top_hwnd);
            let dpi = unsafe { GetDpiForWindow(top) };
            if dpi == 0 {
                96
            } else {
                dpi
            }
        }

        /// Resize the window so the content area equals `content_w x content_h`
        /// (window height adds the titlebar). Recenters when `recenter` is true
        /// and the user has not moved the shell (spec Part 4/6).
        pub fn resize_to_content(&self, content_w: i32, content_h: i32, recenter: bool) {
            let top = hwnd_from(self.top_hwnd);
            let th = titlebar_h(top);
            let bw = border_w(top);
            let (client_w, client_h) = shell_client_size(content_w, content_h, th, bw);
            let (win_w, win_h) = outer_size_for_client(top, client_w, client_h);
            // Shell-driven resize (plugin resizeView / preferred size / snap-
            // back): exempt from the fixed-size min/max lock for its duration.
            self.inner
                .programmatic_resize
                .store(true, Ordering::Relaxed);
            unsafe {
                let _ = SetWindowPos(
                    top,
                    None,
                    0,
                    0,
                    win_w,
                    win_h,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                if recenter && !self.inner.has_user_moved.load(Ordering::Relaxed) {
                    let (outer_w, outer_h) = (win_w, win_h);
                    let (x, y, _) = center_shell_open_position(outer_w, outer_h, self.owner_hwnd());
                    let _ = SetWindowPos(
                        top,
                        None,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            self.inner
                .programmatic_resize
                .store(false, Ordering::Relaxed);
            self.inner
                .initial_auto_size_done
                .store(true, Ordering::Relaxed);
            let _ = self.apply_content_layout();
            invalidate_titlebar(top);
            log_content_rect(top);
        }

        /// Apply the host-reported `IPlugView::canResize` (spec item 1/8).
        /// `false` locks the wrapper: resize edges disappear, min = max =
        /// current size, maximize is ignored. `true` restores normal resizing.
        pub fn set_resizable(&self, resizable: bool) {
            let was = self.inner.resizable.swap(resizable, Ordering::Relaxed);
            if was != resizable {
                eprintln!(
                    "[PluginEditorResize] wrapper resizable={resizable} (IPlugView::canResize)"
                );
                // Re-evaluate the frame so the cursor stops offering resize
                // arrows on the (now disabled) edges.
                unsafe {
                    let _ = SetWindowPos(
                        hwnd_from(self.top_hwnd),
                        None,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                    );
                }
            }
        }

        pub fn pump_messages(&self) {
            // Host-owned proxy: no window. PeekMessage with a null HWND would
            // drain the GPUI main thread's own message queue — never do that.
            if self.top_hwnd == 0 {
                return;
            }
            pump_shell_messages(hwnd_from(self.top_hwnd), hwnd_from(self.content_hwnd));
        }

        pub fn poll(&self) -> NativeShellPoll {
            let resized = if self.inner.resize_pending.swap(false, Ordering::Relaxed) {
                Some((
                    self.inner.resize_w.load(Ordering::Relaxed),
                    self.inner.resize_h.load(Ordering::Relaxed),
                ))
            } else {
                None
            };
            NativeShellPoll {
                close_requested: self.inner.close_requested.load(Ordering::Relaxed),
                resized,
                transport_toggles: self.inner.transport_toggles.swap(0, Ordering::Relaxed),
            }
        }

        pub fn paint_stats(&self) -> NativeShellPaintStats {
            NativeShellPaintStats {
                shell_paint_count: self.inner.shell_paint.load(Ordering::Relaxed),
                content_paint_count: self.inner.content_paint.load(Ordering::Relaxed),
                content_erase_count: self.inner.content_erase.load(Ordering::Relaxed),
                size_count: self.inner.size_count.load(Ordering::Relaxed),
            }
        }

        pub fn focus(&self) {
            unsafe {
                let _ = SetForegroundWindow(hwnd_from(self.top_hwnd));
            }
        }

        pub fn set_title(&self, title: &str) {
            if let Ok(mut t) = self.inner.title.lock() {
                *t = title.to_string();
            }
            invalidate_titlebar(hwnd_from(self.top_hwnd));
        }
    }

    impl Drop for NativeEditorShell {
        fn drop(&mut self) {
            // Host-owned proxy owns no window; the host process destroys the
            // real editor window on CloseEditor.
            if self.top_hwnd == 0 {
                return;
            }
            unsafe {
                let _ = DestroyWindow(hwnd_from(self.top_hwnd));
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{NativeShellPaintStats, NativeShellPoll};

    pub struct NativeEditorShell {
        _private: (),
    }

    impl NativeEditorShell {
        pub fn create(
            _title: &str,
            _content_w: i32,
            _content_h: i32,
            _owner_hwnd: Option<u64>,
        ) -> Option<Self> {
            None
        }
        pub fn host_owned_proxy(_title: &str) -> Self {
            Self { _private: () }
        }
        pub fn is_host_owned_proxy(&self) -> bool {
            true
        }
        pub fn top_hwnd(&self) -> u64 {
            0
        }
        pub fn content_hwnd(&self) -> u64 {
            0
        }
        pub fn content_size(&self) -> (i32, i32) {
            (0, 0)
        }
        pub fn mark_attached(&self) {}
        pub fn set_status(&self, _text: &str, _is_error: bool) {}
        pub fn ensure_visible_zorder(&self) {}
        pub fn has_user_moved(&self) -> bool {
            false
        }
        pub fn owner_hwnd(&self) -> Option<u64> {
            None
        }
        pub fn clamp_content_to_work_area(&self, w: i32, h: i32) -> (i32, i32, bool) {
            (w, h, false)
        }
        pub fn shell_outer_size(&self) -> (i32, i32) {
            (0, 0)
        }
        pub fn shell_dpi(&self) -> u32 {
            96
        }
        pub fn apply_content_layout(&self) -> (i32, i32) {
            (0, 0)
        }
        pub fn log_black_gap_check(&self, _host_hwnd: u64) {}
        pub fn resize_to_content(&self, _content_w: i32, _content_h: i32, _recenter: bool) {}
        pub fn set_resizable(&self, _resizable: bool) {}
        pub fn pump_messages(&self) {}
        pub fn poll(&self) -> NativeShellPoll {
            NativeShellPoll::default()
        }
        pub fn paint_stats(&self) -> NativeShellPaintStats {
            NativeShellPaintStats::default()
        }
        pub fn focus(&self) {}
        pub fn set_title(&self, _title: &str) {}
    }
}

pub use imp::NativeEditorShell;
