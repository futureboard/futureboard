//! macOS host-owned VST3 editor, end to end against a real spawned host.
//!
//! AppKit has no public cross-process view embedding, so on macOS the plug-in
//! editor is a top-level NSWindow owned by the plug-in host process and the
//! main app passes no parent window (`parent_hwnd = 0`) — the same host-owned
//! model Linux uses. This test drives that path over real IPC:
//!
//!   spawn host -> Ready -> LoadPlugin -> PluginLoaded
//!               -> OpenEditorWithParentHwnd(parent 0) -> EditorAttached
//!               -> CloseEditor
//!
//! A real plug-in is required, so point the test at one:
//!
//!   FUTUREBOARD_TEST_VST3_PATH=/Library/Audio/Plug-Ins/VST3/Some.vst3 \
//!     cargo test -p sphere-plugin-host --features plugin-host-bin \
//!     --test macos_host_owned_editor -- --nocapture
//!
//! Set `FUTUREBOARD_TEST_VST3_HOLD_MS` to keep the editor on screen after the
//! attach, for visual inspection of a real run.
#![cfg(all(target_os = "macos", feature = "plugin-host-bin"))]

use std::time::{Duration, Instant};

use SpherePluginHost::ipc::HostEvent;
use SpherePluginHost::plugin_host_client::{ClientEvent, PluginHostClient};
use SpherePluginHost::scan_plugin_bundle;

const INSTANCE_ID: &str = "track1:insert1";

fn wait_for<T>(
    client: &PluginHostClient,
    timeout: Duration,
    mut accept: impl FnMut(HostEvent) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match client.try_recv_event() {
            Some(ClientEvent::Host(event)) => {
                if let Some(value) = accept(event) {
                    return Some(value);
                }
            }
            Some(other) => eprintln!("[test] client event {other:?}"),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    None
}

fn hold_after_attach() -> Duration {
    let ms = std::env::var("FUTUREBOARD_TEST_VST3_HOLD_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_millis(ms)
}

#[test]
fn host_owned_editor_attaches_without_a_parent_window() {
    let Ok(plugin_path) = std::env::var("FUTUREBOARD_TEST_VST3_PATH") else {
        eprintln!("skipping: set FUTUREBOARD_TEST_VST3_PATH to a .vst3 bundle");
        return;
    };

    let classes = scan_plugin_bundle(std::path::Path::new(&plugin_path))
        .unwrap_or_else(|e| panic!("scan {plugin_path}: {e}"));
    let plugin = classes
        .into_iter()
        .find(|info| info.class_id.is_some())
        .unwrap_or_else(|| panic!("{plugin_path} reported no class id"));
    let class_id = plugin.class_id.clone().expect("class id");
    eprintln!(
        "[test] plugin={} vendor={} class_id={class_id}",
        plugin.name, plugin.vendor
    );

    let mut client = PluginHostClient::spawn_bridge().expect("spawn plugin host");
    assert!(
        wait_for(&client, Duration::from_secs(15), |event| matches!(
            event,
            HostEvent::Ready { .. }
        )
        .then_some(()))
        .is_some(),
        "host never reported Ready"
    );

    client
        .load_plugin(INSTANCE_ID, &plugin_path, &class_id, 48_000, 512)
        .expect("send load_plugin");
    let loaded = wait_for(&client, Duration::from_secs(60), |event| match event {
        HostEvent::PluginLoaded { name, .. } | HostEvent::PluginAlreadyLoaded { name, .. } => {
            Some(Ok(name))
        }
        HostEvent::PluginLoadFailed { error, .. } => Some(Err(error)),
        _ => None,
    })
    .expect("host never answered LoadPlugin");
    let loaded = loaded.unwrap_or_else(|error| panic!("plugin load failed: {error}"));
    eprintln!("[test] loaded {loaded}");

    // parent 0: there is no cross-process parent view on macOS. The host must
    // still open its own editor window rather than rejecting the request.
    client
        .open_editor(INSTANCE_ID, &plugin_path, &class_id, 0, 900, 600, 96)
        .expect("send open_editor");
    let attached = wait_for(&client, Duration::from_secs(60), |event| match event {
        HostEvent::EditorAttached {
            preferred_width,
            preferred_height,
            host_hwnd,
            ..
        } => Some(Ok((preferred_width, preferred_height, host_hwnd))),
        HostEvent::EditorAttachFailed { error, .. } => Some(Err(error)),
        _ => None,
    })
    .expect("host never answered OpenEditorWithParentHwnd");
    let (width, height, host_handle) =
        attached.unwrap_or_else(|error| panic!("editor attach failed: {error}"));
    eprintln!("[test] editor attached size={width}x{height} host_handle=0x{host_handle:x}");

    assert!(
        host_handle != 0,
        "host reported no editor handle, so nothing owns the editor window"
    );
    assert!(
        width >= 32 && height >= 32,
        "editor reported an unusable size {width}x{height}"
    );

    // Report whatever the host says while the editor is up (EditorClosed when
    // the window is closed by hand, EditorUnresponsive if the pump stalls).
    let hold = hold_after_attach();
    if !hold.is_zero() {
        eprintln!("[test] holding the editor open for {}ms", hold.as_millis());
        let until = Instant::now() + hold;
        while Instant::now() < until {
            match client.try_recv_event() {
                Some(event) => eprintln!("[test] while open: {event:?}"),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    client.close_editor(INSTANCE_ID).expect("send close_editor");
    client.unload_plugin(INSTANCE_ID).ok();
    client.shutdown().ok();
}
