#pragma once
// Internal C bridge between the platform editor implementations
// (editor_mac.mm, editor_linux.cpp) and the VST3 processor core
// (vst3_processor.cpp).
//
// Using a pure-C API means the platform files never need to see the full
// SphereDauxVst3Processor struct definition or any VST3 SDK headers.
// All functions must be called on the UI / main thread unless noted.

#ifdef __cplusplus
extern "C" {
#endif

struct SphereDauxVst3Processor; // opaque forward declaration

// ── Error / diagnostics ───────────────────────────────────────────────────────

/// Write to the thread-local last-error string returned by
/// sphere_daux_vst3_last_error().
void sphere_daux_editor_set_error(const char* msg);

/// Return and increment the global editor-handle counter (1-based, never 0).
unsigned long long sphere_daux_editor_next_handle(void);

/// Mark that the user closed the (host-owned) editor window via its own
/// titlebar. The external plugin-host process polls this via
/// sphere_daux_vst3_embed_take_user_close() to report EditorClosed. Safe no-op
/// when the processor has no such state. Called from the GTK/AppKit UI thread.
void sphere_daux_editor_signal_user_close(SphereDauxVst3Processor* proc);

// ── IPlugView lifecycle ───────────────────────────────────────────────────────

/// Ask the IEditController to create a native view and query the plugin's
/// preferred window size.
///
/// platform_type  "NSView" on macOS | "X11EmbedWindowID" on Linux.
/// out_width / out_height  updated to the plugin's preferred size when
///   available; left unchanged if the plugin has no preference.
///
/// Returns 1 on success, 0 if the view could not be created or the platform
/// type is not supported.  On failure the view pointer is released internally.
int sphere_daux_editor_create_view(
    SphereDauxVst3Processor* proc,
    const char*              platform_type,
    int*                     out_width,
    int*                     out_height);

/// Attach the previously created IPlugView to a platform-native parent handle.
///
/// native_handle  NSView* (cast to void*) on macOS;
///                X11 Window / XID (cast via (void*)(uintptr_t)xid) on Linux.
/// platform_type  same value passed to sphere_daux_editor_create_view().
///
/// Returns 1 on success.  On failure the view is detached and released.
int sphere_daux_editor_attach_view(
    SphereDauxVst3Processor* proc,
    void*                    native_handle,
    const char*              platform_type);

/// Forward a host-side resize to IPlugView::onSize().
/// Call this when the native window's client area changes.
void sphere_daux_editor_notify_resize(
    SphereDauxVst3Processor* proc,
    int                      width,
    int                      height);

/// Query the current IPlugView size. Unlike the initial create-view result,
/// this reflects editors that choose their final dimensions in attached().
int sphere_daux_editor_get_view_size(
    SphereDauxVst3Processor* proc,
    int*                     out_width,
    int*                     out_height);

/// Return whether the current view supports host/user resizing.
int sphere_daux_editor_can_resize(SphereDauxVst3Processor* proc);

/// Apply the VST3 size contract to a proposed content size. Fixed-size views
/// snap to getSize(); resizable views run checkSizeConstraint().
int sphere_daux_editor_constrain_view_size(
    SphereDauxVst3Processor* proc,
    int*                     io_width,
    int*                     io_height);

/// Store the native editor content size used by the host-owned macOS/Linux
/// window so the IPC EditorAttached event reports the real size.
void sphere_daux_editor_set_content_size(
    SphereDauxVst3Processor* proc,
    int                      width,
    int                      height);

#if defined(__APPLE__)
/// Resize the AppKit content area for a plug-in initiated resizeView request.
int sphere_daux_editor_apply_plugin_resize(
    SphereDauxVst3Processor* proc,
    int                      width,
    int                      height);
#endif

/// Detach and release the IPlugView (calls IPlugView::removed()).
/// Safe to call even when no view is currently attached.
void sphere_daux_editor_detach_view(SphereDauxVst3Processor* proc);

/// Install (frame != NULL) or clear (frame == NULL) the host IPlugFrame on the
/// current IPlugView via IPlugView::setFrame().
///
/// `frame` is an opaque Steinberg::IPlugFrame* (passed as void* to keep this
/// bridge free of SDK headers). On Linux the frame MUST also implement
/// Steinberg::Linux::IRunLoop — without it most VST3 editors never repaint.
///
/// Contract: call with a valid frame AFTER create_view and BEFORE attach_view;
/// call with NULL before detach_view (mirrors the SDK editorhost teardown).
/// Returns 1 on success, 0 if there is no view or setFrame() was rejected.
int sphere_daux_editor_set_frame(SphereDauxVst3Processor* proc, void* frame);

// ── Native window state storage ───────────────────────────────────────────────

/// Store all platform native pointers and metadata in the processor.
///
/// native_window    NSWindow* / GtkWidget* (GtkWindow)
/// native_embed     NSView* used as the IPlugView parent (macOS only; NULL on Linux)
/// native_delegate  DauxEditorWindowDelegate* (macOS NSWindowDelegate; NULL on Linux)
/// handle           value from sphere_daux_editor_next_handle()
void sphere_daux_editor_store_native(
    SphereDauxVst3Processor* proc,
    void*                    native_window,
    void*                    native_embed,
    void*                    native_delegate,
    unsigned long long       handle,
    const char*              window_id,
    const char*              title,
    int                      requested_width,
    int                      requested_height);

/// Zero all native window fields in the processor (called after the
/// platform window has been destroyed / hidden).
void sphere_daux_editor_clear_native(SphereDauxVst3Processor* proc);

// ── Getters (read back stored state) ─────────────────────────────────────────

void*              sphere_daux_editor_get_native_window(SphereDauxVst3Processor* proc);
void*              sphere_daux_editor_get_native_embed(SphereDauxVst3Processor* proc);
void*              sphere_daux_editor_get_native_delegate(SphereDauxVst3Processor* proc);
unsigned long long sphere_daux_editor_get_handle(SphereDauxVst3Processor* proc);
const char*        sphere_daux_editor_get_window_id(SphereDauxVst3Processor* proc);
const char*        sphere_daux_editor_get_title(SphereDauxVst3Processor* proc);
int                sphere_daux_editor_get_requested_width(SphereDauxVst3Processor* proc);
int                sphere_daux_editor_get_requested_height(SphereDauxVst3Processor* proc);

#ifdef __cplusplus
} // extern "C"
#endif
