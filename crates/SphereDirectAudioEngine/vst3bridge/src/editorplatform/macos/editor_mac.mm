// editor_mac.mm — macOS NSWindow + NSView IPlugView embedding
// Compiled as Objective-C++ (.mm) with -fobjc-arc.
//
// Plug-in GUI lifecycle (macOS VST3):
//   open_editor_mac()   → create NSWindow + NSView
//                        → sphere_daux_editor_create_view("NSView")
//                        → sphere_daux_editor_attach_view(NSView*)
//                        → [NSWindow makeKeyAndOrderFront]
//   close_editor_mac()  → sphere_daux_editor_detach_view()
//                        → [NSWindow orderOut] + release retained objects
//   focus_editor_mac()  → [NSWindow makeKeyAndOrderFront]
//   shutdown_editor_mac() → close_editor_mac() (called from dtor / destroy)
//
// ObjC objects (NSWindow, NSView, delegate) are retained via __bridge_retained
// when stored as void* in the processor struct, and released via
// __bridge_transfer when the window is closed.

#include "editor_mac_internal.hpp"

#import <dispatch/dispatch.h>

#include <cstdio>

/// Open a floating NSWindow containing an NSView that hosts the plugin's GUI.
/// May be called from any thread; dispatches to the main thread synchronously.
unsigned long long open_editor_mac(SphereDauxVst3Processor *proc,
                                   const char *window_id, const char *title,
                                   int width, int height) {
  if (!proc)
    return 0;

  // All Cocoa work must happen on the main thread.
  if (!NSThread.isMainThread) {
    __block unsigned long long result = 0;
    dispatch_sync(dispatch_get_main_queue(), ^{
      result = open_editor_mac(proc, window_id, title, width, height);
    });
    return result;
  }

  // Already open? Bring it to front and return the existing handle.
  void *existing_win = sphere_daux_editor_get_native_window(proc);
  if (existing_win) {
    NSWindow *win = (__bridge NSWindow *)existing_win;
    [win makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
    return sphere_daux_editor_get_handle(proc);
  }

  // ── Step 1: Create the IPlugView and query preferred size ────────────────

  int editor_width = width > 0 ? width : 820;
  int editor_height = height > 0 ? height : 560;

  if (!sphere_daux_editor_create_view(proc, "NSView", &editor_width,
                                      &editor_height)) {
    std::fprintf(stderr, "[SphereVST3/mac] create_view('NSView') failed\n");
    return 0;
  }

  // ── Step 2: Create NSWindow ───────────────────────────────────────────────

  NSRect content_rect = NSMakeRect(0.0, 0.0, (CGFloat)editor_width,
                                   (CGFloat)editor_height);
  NSWindowStyleMask style = NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                            NSWindowStyleMaskResizable |
                            NSWindowStyleMaskMiniaturizable;

  NSWindow *window = [[NSWindow alloc] initWithContentRect:content_rect
                                                 styleMask:style
                                                   backing:NSBackingStoreBuffered
                                                     defer:NO];

  NSString *ns_title = [NSString
      stringWithUTF8String:(title && *title) ? title : "Plugin Editor"];
  [window setTitle:ns_title];
  [window setBackgroundColor:daux_bg_color()];
  [window setLevel:NSFloatingWindowLevel];
  [window center];

  // ── Step 3: Create embed NSView (IPlugView parent) ────────────────────────

  NSView *embed = [[NSView alloc] initWithFrame:content_rect];
  embed.wantsLayer = YES;
  embed.layer.backgroundColor = daux_bg_color().CGColor;
  [window setContentView:embed];

  // ── Step 4: Attach NSWindowDelegate ──────────────────────────────────────

  DauxEditorWindowDelegate *delegate = [[DauxEditorWindowDelegate alloc] init];
  delegate.processor = proc;
  [window setDelegate:delegate];

  // ── Step 5: Attach IPlugView to NSView ────────────────────────────────────

  // Retain all ObjC objects as void* BEFORE calling attach so the store happens
  // atomically and close_editor_mac can release them even if attach fails
  // mid-way (we'll clear them on failure).
  void *win_retained = (__bridge_retained void *)window;
  void *embed_retained = (__bridge_retained void *)embed;
  void *delegate_retained = (__bridge_retained void *)delegate;

  unsigned long long handle = sphere_daux_editor_next_handle();

  // Store now so that close_editor_mac can run safely if attach fails.
  sphere_daux_editor_store_native(
      proc, win_retained, embed_retained, delegate_retained, handle,
      window_id ? window_id : "", (title && *title) ? title : "Plugin Editor",
      width, height);

  if (!sphere_daux_editor_attach_view(proc, (__bridge void *)embed, "NSView")) {
    std::fprintf(
        stderr,
        "[SphereVST3/mac] attach_view('NSView') failed; handle=%llu\n",
        handle);
    sphere_daux_editor_clear_native(proc);
    // Release retained objects (ARC releases when the local variables go out of
    // scope).
    {
      NSWindow *w = (__bridge_transfer NSWindow *)win_retained;
      [w setDelegate:nil];
      (void)w;
    }
    {
      NSView *v = (__bridge_transfer NSView *)embed_retained;
      (void)v;
    }
    {
      DauxEditorWindowDelegate *d =
          (__bridge_transfer DauxEditorWindowDelegate *)delegate_retained;
      (void)d;
    }
    return 0;
  }

  // ── Step 6: Resize window to plugin's post-attach preferred size ──────────

  // Some plugins resize themselves inside attached(); re-query and adjust.
  // sphere_daux_editor_notify_resize already calls IPlugView::onSize so we only
  // need to resize the NSWindow frame here.
  {
    // (view size is set by the plugin; we match the NS window to it)
    NSRect embed_frame = embed.frame;
    if (embed_frame.size.width > 0 && embed_frame.size.height > 0) {
      // The embed frame is view-local, so its origin is (0,0) — the screen's
      // bottom-left corner in window coordinates. Take only the size and
      // re-center, or the window jumps into the corner of the display.
      NSRect content_rect =
          NSMakeRect(0.0, 0.0, embed_frame.size.width, embed_frame.size.height);
      NSRect window_frame = [window frameRectForContentRect:content_rect];
      window_frame.origin = window.frame.origin;
      [window setFrame:window_frame display:NO];
      [window center];
    }
  }

  // ── Step 7: Show the window ───────────────────────────────────────────────

  [window makeKeyAndOrderFront:nil];
  [NSApp activateIgnoringOtherApps:YES];

  std::fprintf(stderr,
               "[SphereVST3/mac] editor opened handle=%llu windowId=%s w=%d "
               "h=%d\n",
               handle, window_id ? window_id : "", editor_width, editor_height);
  return handle;
}

/// Detach the IPlugView and destroy the NSWindow.
/// May be called from any thread; dispatches to the main thread asynchronously
/// (or synchronously if already on the main thread).
void close_editor_mac(SphereDauxVst3Processor *proc) {
  if (!proc)
    return;

  if (!NSThread.isMainThread) {
    dispatch_async(dispatch_get_main_queue(), ^{ close_editor_mac(proc); });
    return;
  }

  // Grab and immediately clear the native pointers to prevent re-entrancy
  // (e.g. windowShouldClose: → close_editor_mac → [win orderOut] → ... ).
  void *win_ptr = sphere_daux_editor_get_native_window(proc);
  void *embed_ptr = sphere_daux_editor_get_native_embed(proc);
  void *delegate_ptr = sphere_daux_editor_get_native_delegate(proc);

  if (!win_ptr)
    return; // already closed

  sphere_daux_editor_clear_native(proc); // zero the struct fields first
  sphere_daux_editor_detach_view(proc);  // IPlugView::removed()

  // Release retained ObjC objects via __bridge_transfer.
  // Order: clear delegate first so no callbacks fire during window close.
  if (win_ptr) {
    NSWindow *win = (__bridge_transfer NSWindow *)win_ptr;
    [win setDelegate:nil];
    [win orderOut:nil]; // hide without triggering windowShouldClose:
    // ARC releases win here (our __bridge_retained +1 is consumed)
  }
  if (embed_ptr) {
    NSView *v = (__bridge_transfer NSView *)embed_ptr;
    [v removeFromSuperview];
    (void)v; // ARC releases
  }
  if (delegate_ptr) {
    DauxEditorWindowDelegate *d =
        (__bridge_transfer DauxEditorWindowDelegate *)delegate_ptr;
    (void)d; // ARC releases
  }

  std::fprintf(stderr, "[SphereVST3/mac] editor closed\n");
}

/// Bring the plugin editor window to the front.
int focus_editor_mac(SphereDauxVst3Processor *proc) {
  if (!proc)
    return 0;
  void *win_ptr = sphere_daux_editor_get_native_window(proc);
  if (!win_ptr)
    return 0;

  if (!NSThread.isMainThread) {
    dispatch_async(dispatch_get_main_queue(), ^{ focus_editor_mac(proc); });
    return 1;
  }

  NSWindow *win = (__bridge NSWindow *)win_ptr;
  [win makeKeyAndOrderFront:nil];
  [NSApp activateIgnoringOtherApps:YES];
  return 1;
}

/// Called from SphereDauxVst3Processor::shutdown() on macOS.
void shutdown_editor_mac(SphereDauxVst3Processor *proc) { close_editor_mac(proc); }
