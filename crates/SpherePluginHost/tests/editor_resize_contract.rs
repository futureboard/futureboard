//! End-to-end smoke for the **generic VST3 editor resize contract** against a
//! real plugin (no vendor logic — any VST3 works as the test subject):
//!
//! - `EditorAttached` carries `resizable` (`IPlugView::canResize`).
//! - A `ResizeEditor` the plugin rejects/adjusts (fixed-size view, stepped
//!   sizes, …) produces an `EditorContentResize` snap-back so the wrapper
//!   never keeps blank/garbage area around the plugin content.
//! - The host child HWND client rect converges to the plugin-agreed size.
//!
//! Gated on a real plugin path so default CI stays plugin-free:
//!
//! ```text
//! FUTUREBOARD_SMOKE_VST3_PATH="C:\Program Files\Common Files\VST3\X.vst3" \
//! FUTUREBOARD_SMOKE_VST3_CLASS_ID=<32-hex class id> \
//! cargo test -p sphere-plugin-host --features plugin-host-bin \
//!   --test editor_resize_contract -- --nocapture
//! ```
#![cfg(all(windows, feature = "plugin-host-bin"))]

use std::time::{Duration, Instant};

use SpherePluginHost::ipc::HostEvent;
use SpherePluginHost::plugin_host_client::{ClientEvent, PluginHostClient};

mod win {
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetClientRect, PeekMessageW,
        TranslateMessage, CW_USEDEFAULT, MSG, PM_REMOVE, WINDOW_EX_STYLE, WS_CLIPCHILDREN,
        WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    pub fn hwnd_from(raw: u64) -> HWND {
        HWND(raw as *mut core::ffi::c_void)
    }

    /// Visible top-level parent standing in for the main-app wrapper content.
    pub fn create_parent_window() -> Option<u64> {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Futureboard Resize Contract Smoke"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                960,
                720,
                None,
                None,
                None,
                None,
            )
            .ok()?;
            Some(hwnd.0 as u64)
        }
    }

    pub fn destroy_window(raw: u64) {
        if raw != 0 {
            unsafe {
                let _ = DestroyWindow(hwnd_from(raw));
            }
        }
    }

    /// Drain this thread's message queue (the parent window lives here).
    pub fn pump_messages() {
        unsafe {
            let mut msg = MSG::default();
            let mut budget = 256;
            while budget > 0 && PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                budget -= 1;
            }
        }
    }

    /// Cross-process client rect of the host's embed child (HWNDs are global).
    pub fn client_size(raw: u64) -> Option<(i32, i32)> {
        if raw == 0 {
            return None;
        }
        let mut rc = RECT::default();
        unsafe { GetClientRect(hwnd_from(raw), &mut rc) }.ok()?;
        Some(((rc.right - rc.left).max(0), (rc.bottom - rc.top).max(0)))
    }
}

fn wait_for<F>(client: &PluginHostClient, timeout: Duration, mut pred: F) -> Option<HostEvent>
where
    F: FnMut(&HostEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        win::pump_messages();
        match client.try_recv_event() {
            Some(ClientEvent::Host(ev)) => {
                eprintln!("[smoke] event: {ev:?}");
                if pred(&ev) {
                    return Some(ev);
                }
            }
            Some(ClientEvent::Disconnected) => {
                panic!("host disconnected while waiting for an event");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    None
}

/// Pump + idle for `ms` so the host/editor settle between steps.
fn settle(ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        win::pump_messages();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn resize_contract_with_real_plugin() {
    let (Ok(plugin_path), Ok(class_id)) = (
        std::env::var("FUTUREBOARD_SMOKE_VST3_PATH"),
        std::env::var("FUTUREBOARD_SMOKE_VST3_CLASS_ID"),
    ) else {
        eprintln!(
            "[smoke] skipped: set FUTUREBOARD_SMOKE_VST3_PATH and \
             FUTUREBOARD_SMOKE_VST3_CLASS_ID to run against a real VST3"
        );
        return;
    };
    assert!(
        std::path::Path::new(&plugin_path).exists(),
        "FUTUREBOARD_SMOKE_VST3_PATH does not exist: {plugin_path}"
    );

    let instance = "smoke:resize1";
    let mut client = PluginHostClient::spawn().expect("spawn FutureboardPluginHostX64");
    assert!(
        wait_for(&client, Duration::from_secs(10), |ev| matches!(
            ev,
            HostEvent::Ready { .. }
        ))
        .is_some(),
        "no Ready from host"
    );

    client
        .load_plugin(instance, &plugin_path, &class_id, 48_000, 256, None)
        .expect("send LoadPlugin");
    assert!(
        wait_for(&client, Duration::from_secs(60), |ev| matches!(
            ev,
            HostEvent::PluginLoaded { .. } | HostEvent::PluginAlreadyLoaded { .. }
        ))
        .is_some(),
        "plugin did not load"
    );

    let parent = win::create_parent_window().expect("create parent window");

    client
        .open_editor(instance, &plugin_path, &class_id, parent, 600, 400, 96)
        .expect("send OpenEditorWithParentHwnd");
    let attached = wait_for(&client, Duration::from_secs(30), |ev| {
        matches!(
            ev,
            HostEvent::EditorAttached { .. } | HostEvent::EditorAttachFailed { .. }
        )
    })
    .expect("no EditorAttached/EditorAttachFailed");
    let (resizable, preferred, host_hwnd) = match attached {
        HostEvent::EditorAttached {
            resizable,
            preferred_width,
            preferred_height,
            host_hwnd,
            ..
        } => (resizable, (preferred_width, preferred_height), host_hwnd),
        other => panic!("editor attach failed: {other:?}"),
    };
    eprintln!(
        "[smoke] attached resizable={resizable} preferred={}x{} host_hwnd=0x{host_hwnd:x}",
        preferred.0, preferred.1
    );
    assert!(preferred.0 > 0 && preferred.1 > 0, "no preferred size");
    settle(1500);

    // ── Oversize request: way beyond anything the plugin asked for. ─────────
    let over = (preferred.0 + 600, preferred.1 + 500);
    client
        .resize_editor(instance, over.0, over.1, 96)
        .expect("send ResizeEditor oversize");
    if resizable {
        // The plugin may accept or constrain; both are valid. Just settle.
        let _ = wait_for(&client, Duration::from_secs(3), |ev| {
            matches!(ev, HostEvent::EditorContentResize { .. })
        });
    } else {
        // Fixed-size view MUST snap back to its own size.
        let snap = wait_for(&client, Duration::from_secs(5), |ev| {
            matches!(ev, HostEvent::EditorContentResize { .. })
        })
        .expect("fixed-size plugin: no EditorContentResize snap-back after oversize request");
        if let HostEvent::EditorContentResize { width, height, .. } = &snap {
            assert!(
                (*width, *height) != (over.0, over.1),
                "snap-back returned the rejected oversize"
            );
        }
    }
    settle(800);

    // The host child client rect must equal the plugin-agreed size — never the
    // blindly-requested wrapper size (this is the blank/garbage area bug).
    if let Some((cw, ch)) = win::client_size(host_hwnd) {
        eprintln!("[smoke] host child client after oversize: {cw}x{ch}");
        if !resizable {
            assert_eq!(
                (cw as u32, ch as u32),
                preferred,
                "fixed-size plugin child must stay at its preferred size"
            );
        } else {
            assert!(
                (cw as u32, ch as u32) != (0, 0),
                "host child lost its client area"
            );
        }
    }

    // ── Undersize request. ───────────────────────────────────────────────────
    client
        .resize_editor(instance, 220, 140, 96)
        .expect("send ResizeEditor undersize");
    let _ = wait_for(&client, Duration::from_secs(3), |ev| {
        matches!(ev, HostEvent::EditorContentResize { .. })
    });
    settle(800);
    if let (false, Some((cw, ch))) = (resizable, win::client_size(host_hwnd)) {
        assert_eq!(
            (cw as u32, ch as u32),
            preferred,
            "fixed-size plugin child must stay at its preferred size after undersize"
        );
    }

    client.close_editor(instance).expect("send CloseEditor");
    assert!(
        wait_for(&client, Duration::from_secs(10), |ev| matches!(
            ev,
            HostEvent::EditorClosed { .. }
        ))
        .is_some(),
        "no EditorClosed"
    );
    client.shutdown().ok();
    win::destroy_window(parent);
}
