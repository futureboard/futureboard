// macOS editor hosting for CLAP plug-ins.
//
// A Cocoa CLAP GUI is parented to an `NSView` through `clap_window.cocoa`, so
// this file owns a small NSWindow + container NSView per instance rather than
// reusing the Win32 `daux_editor_*` shell (which is HWND-based).
//
// Embedded mode receives the GPUI-provided `NSView*` directly and parents the
// container into it; standalone mode creates its own titled window.

#if !defined(__APPLE__)
#error "clap_editor_mac.mm is macOS-only"
#endif

#include "clap_processor_internal.hpp"

#import <Cocoa/Cocoa.h>

@interface DauxClapEditorWindowDelegate : NSObject <NSWindowDelegate>
@property(nonatomic, assign) SphereDauxClapProcessor *processor;
@end

@implementation DauxClapEditorWindowDelegate
- (BOOL)windowShouldClose:(NSWindow *)sender {
  (void)sender;
  if (self.processor) {
    // Report the user close; the host tears the editor down and keeps the
    // audio instance alive.
    self.processor->embed_user_closed.store(true, std::memory_order_release);
  }
  return NO;
}
@end

namespace {

/// Run the `clap.gui` attach sequence into `container`.
bool attach_into(SphereDauxClapProcessor *p, NSView *container, int *width,
                 int *height) {
  if (!p || !p->plugin || !p->ext_gui || !container) {
    return false;
  }

  if (!p->gui_created) {
    if (!p->ext_gui->is_api_supported ||
        !p->ext_gui->is_api_supported(p->plugin, CLAP_WINDOW_API_COCOA,
                                      false)) {
      clap_set_last_error("CLAP plug-in does not support an embedded Cocoa GUI");
      return false;
    }
    if (!p->ext_gui->create ||
        !p->ext_gui->create(p->plugin, CLAP_WINDOW_API_COCOA, false)) {
      clap_set_last_error("clap_plugin_gui->create() returned false");
      return false;
    }
    p->gui_created = true;
  }

  if (p->ext_gui->set_scale) {
    const double scale =
        container.window ? container.window.backingScaleFactor : 1.0;
    p->ext_gui->set_scale(p->plugin, scale);
  }

  p->preferred_gui_size(width, height);

  clap_window_t window{};
  window.api = CLAP_WINDOW_API_COCOA;
  window.cocoa = (__bridge void *)container;
  if (!p->ext_gui->set_parent || !p->ext_gui->set_parent(p->plugin, &window)) {
    clap_set_last_error("clap_plugin_gui->set_parent() returned false");
    return false;
  }

  if (p->ext_gui->show) {
    p->ext_gui->show(p->plugin);
  }

  // Re-query after show: some plug-ins only settle their size once visible.
  p->preferred_gui_size(width, height);
  if (*width > 0 && *height > 0) {
    p->embed_content_w = *width;
    p->embed_content_h = *height;
    container.frame =
        NSMakeRect(container.frame.origin.x, container.frame.origin.y, *width,
                   *height);
    for (NSView *child in container.subviews) {
      child.frame = NSMakeRect(0, 0, *width, *height);
    }
  }

  p->editor_attached = true;
  std::fprintf(stderr, "[clap-editor] attached instance=%s size=%dx%d\n",
               p->embed_instance_label.empty()
                   ? "<unknown>"
                   : p->embed_instance_label.c_str(),
               *width, *height);
  return true;
}

} // namespace

// ── Platform entry points ───────────────────────────────────────────────────

unsigned long long clap_embed_editor_mac(SphereDauxClapProcessor *p,
                                         unsigned long long parent_view, int x,
                                         int y, int width, int height) {
  if (!p || !p->plugin || !p->ext_gui) {
    clap_set_last_error("CLAP embed editor: plug-in exposes no GUI");
    return 0;
  }
  NSView *parent = (__bridge NSView *)reinterpret_cast<void *>(
      static_cast<std::uintptr_t>(parent_view));
  if (!parent) {
    clap_set_last_error("CLAP embed editor: invalid parent NSView");
    return 0;
  }

  if (p->editor_attached && p->editor_native_embed) {
    NSView *existing = (__bridge NSView *)p->editor_native_embed;
    existing.frame = NSMakeRect(x, y, width, height);
    return p->editor_handle;
  }

  int w = width > 0 ? width : 640;
  int h = height > 0 ? height : 480;

  NSView *container = [[NSView alloc] initWithFrame:NSMakeRect(x, y, w, h)];
  [parent addSubview:container];
  p->editor_native_embed = (__bridge_retained void *)container;
  p->embed_mode = true;
  p->embed_host_kind = 0; // child view

  if (!attach_into(p, container, &w, &h)) {
    [container removeFromSuperview];
    CFRelease(p->editor_native_embed);
    p->editor_native_embed = nullptr;
    p->embed_mode = false;
    return 0;
  }

  p->editor_handle = clap_next_editor_handle();
  return p->editor_handle;
}

void clap_embed_set_bounds_mac(SphereDauxClapProcessor *p, int x, int y,
                               int width, int height) {
  if (!p || !p->editor_native_embed || width <= 0 || height <= 0) {
    return;
  }
  NSView *container = (__bridge NSView *)p->editor_native_embed;
  container.frame = NSMakeRect(x, y, width, height);
  p->embed_host_x = x;
  p->embed_host_y = y;
  p->embed_host_w = width;
  p->embed_host_h = height;
  if (!p->editor_resizable || !p->gui_created || !p->ext_gui) {
    return;
  }
  // Let the plug-in snap the request to a size it accepts before applying it.
  auto w = static_cast<uint32_t>(width);
  auto h = static_cast<uint32_t>(height);
  if (p->ext_gui->adjust_size) {
    p->ext_gui->adjust_size(p->plugin, &w, &h);
  }
  if (p->ext_gui->set_size) {
    p->ext_gui->set_size(p->plugin, w, h);
  }
  p->embed_content_w = static_cast<int>(w);
  p->embed_content_h = static_cast<int>(h);
  for (NSView *child in container.subviews) {
    child.frame = NSMakeRect(0, 0, static_cast<int>(w), static_cast<int>(h));
  }
}

unsigned long long clap_open_editor_mac(SphereDauxClapProcessor *p,
                                        const char *window_id,
                                        const char *title, int width,
                                        int height) {
  if (!p || !p->plugin || !p->ext_gui) {
    clap_set_last_error("CLAP editor: plug-in exposes no GUI");
    return 0;
  }
  p->editor_window_id = window_id ? window_id : "";
  if (title && *title) {
    p->editor_title = title;
  }

  if (p->editor_attached && p->editor_native_window) {
    NSWindow *existing = (__bridge NSWindow *)p->editor_native_window;
    [existing makeKeyAndOrderFront:nil];
    return p->editor_handle;
  }

  int w = width > 0 ? width : 640;
  int h = height > 0 ? height : 480;

  NSWindow *window = [[NSWindow alloc]
      initWithContentRect:NSMakeRect(0, 0, w, h)
                styleMask:(NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                           NSWindowStyleMaskMiniaturizable)
                  backing:NSBackingStoreBuffered
                    defer:NO];
  window.title = [NSString stringWithUTF8String:p->editor_title.empty()
                                                    ? "Plug-in Editor"
                                                    : p->editor_title.c_str()];
  window.releasedWhenClosed = NO;

  DauxClapEditorWindowDelegate *delegate =
      [[DauxClapEditorWindowDelegate alloc] init];
  delegate.processor = p;
  window.delegate = delegate;

  NSView *container = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, w, h)];
  window.contentView = container;

  p->editor_native_window = (__bridge_retained void *)window;
  p->editor_native_embed = (__bridge_retained void *)container;
  p->editor_native_delegate = (__bridge_retained void *)delegate;
  p->embed_mode = false;
  p->embed_host_kind = 2; // detached top-level

  if (!attach_into(p, container, &w, &h)) {
    clap_close_editor_mac(p);
    return 0;
  }

  [window setContentSize:NSMakeSize(p->embed_content_w, p->embed_content_h)];
  [window center];
  [window makeKeyAndOrderFront:nil];

  p->editor_handle = clap_next_editor_handle();
  return p->editor_handle;
}

void clap_close_editor_mac(SphereDauxClapProcessor *p) {
  if (!p) {
    return;
  }

  if (p->gui_created && p->plugin && p->ext_gui) {
    if (p->ext_gui->hide) {
      p->ext_gui->hide(p->plugin);
    }
    if (p->ext_gui->destroy) {
      p->ext_gui->destroy(p->plugin);
    }
    p->gui_created = false;
  }
  p->editor_attached = false;

  if (p->editor_native_embed) {
    NSView *container = (__bridge_transfer NSView *)p->editor_native_embed;
    [container removeFromSuperview];
    p->editor_native_embed = nullptr;
  }
  if (p->editor_native_window) {
    NSWindow *window = (__bridge_transfer NSWindow *)p->editor_native_window;
    window.delegate = nil;
    [window orderOut:nil];
    [window close];
    p->editor_native_window = nullptr;
  }
  if (p->editor_native_delegate) {
    DauxClapEditorWindowDelegate *delegate =
        (__bridge_transfer DauxClapEditorWindowDelegate *)
            p->editor_native_delegate;
    delegate.processor = nullptr;
    p->editor_native_delegate = nullptr;
  }

  p->embed_mode = false;
  p->editor_handle = 0;
}

int clap_focus_editor_mac(SphereDauxClapProcessor *p) {
  if (!p) {
    return 0;
  }
  if (p->editor_native_window) {
    NSWindow *window = (__bridge NSWindow *)p->editor_native_window;
    [window makeKeyAndOrderFront:nil];
    return 1;
  }
  if (p->editor_native_embed) {
    NSView *container = (__bridge NSView *)p->editor_native_embed;
    [container.window makeFirstResponder:container];
    return 1;
  }
  return 0;
}

// ── C API (macOS) ───────────────────────────────────────────────────────────

extern "C" {

unsigned long long sphere_daux_clap_embed_editor(SphereDauxClapProcessor *p,
                                                 unsigned long long parent,
                                                 int x, int y, int width,
                                                 int height) {
  return clap_embed_editor_mac(p, parent, x, y, width, height);
}

void sphere_daux_clap_embed_set_bounds(SphereDauxClapProcessor *p, int x, int y,
                                       int width, int height) {
  clap_embed_set_bounds_mac(p, x, y, width, height);
}

void sphere_daux_clap_embed_refresh(SphereDauxClapProcessor *) {
  // CLAP GUIs drive their own repaint through clap.timer-support; there is no
  // host idle call to make.
}

unsigned long long
sphere_daux_clap_embed_attach_hwnd(SphereDauxClapProcessor *p) {
  if (!p || !p->editor_native_embed) {
    return 0;
  }
  return static_cast<unsigned long long>(
      reinterpret_cast<std::uintptr_t>(p->editor_native_embed));
}

void sphere_daux_clap_embed_detach(SphereDauxClapProcessor *p) {
  clap_close_editor_mac(p);
}

int sphere_daux_clap_embed_is_valid(SphereDauxClapProcessor *p) {
  return (p && p->embed_mode && p->editor_attached && p->editor_native_embed)
             ? 1
             : 0;
}

int sphere_daux_clap_embed_has_visible_ui(SphereDauxClapProcessor *p) {
  if (!p || !p->editor_native_embed) {
    return 0;
  }
  NSView *container = (__bridge NSView *)p->editor_native_embed;
  return (container.subviews.count > 0 && !container.hiddenOrHasHiddenAncestor)
             ? 1
             : 0;
}

unsigned long long sphere_daux_clap_open_editor(SphereDauxClapProcessor *p,
                                                const char *window_id,
                                                const char *title, int width,
                                                int height) {
  return clap_open_editor_mac(p, window_id, title, width, height);
}

void sphere_daux_clap_close_editor(SphereDauxClapProcessor *p) {
  clap_close_editor_mac(p);
}

int sphere_daux_clap_focus_editor(SphereDauxClapProcessor *p) {
  return clap_focus_editor_mac(p);
}

} // extern "C"
