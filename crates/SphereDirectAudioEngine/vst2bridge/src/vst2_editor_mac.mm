// macOS editor hosting for VST2 plug-ins.
//
// A Cocoa VST2 editor takes an `NSView*` as its `effEditOpen` parent, so this
// file owns a small NSWindow + container NSView per instance rather than
// reusing the Win32 `daux_editor_*` shell (which is HWND-based).
//
// Embedded mode receives the GPUI-provided `NSView*` directly and parents the
// container into it; standalone mode creates its own titled window.

#if !defined(__APPLE__)
#error "vst2_editor_mac.mm is macOS-only"
#endif

#include "vst2_processor_internal.hpp"

#import <Cocoa/Cocoa.h>

@interface DauxVst2EditorWindowDelegate : NSObject <NSWindowDelegate>
@property(nonatomic, assign) SphereDauxVst2Processor *processor;
@end

@implementation DauxVst2EditorWindowDelegate
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

/// Preferred editor size from the plug-in, falling back to the requested size.
void preferred_size(SphereDauxVst2Processor *p, int *width, int *height) {
  ERect *rect = nullptr;
  p->dispatch(effEditGetRect, 0, 0, &rect);
  if (rect) {
    const int w = rect->right - rect->left;
    const int h = rect->bottom - rect->top;
    if (w > 0 && h > 0) {
      *width = w;
      *height = h;
    }
  }
}

/// Create the container view the plug-in draws into, attach it, and record the
/// resulting size. Returns false when the plug-in produced no subview.
bool attach_into(SphereDauxVst2Processor *p, NSView *container, int width,
                 int height) {
  const auto result =
      p->dispatch(effEditOpen, 0, 0, (__bridge void *)container);
  if (container.subviews.count == 0) {
    std::fprintf(stderr,
                 "[vst2-editor] effEditOpen produced no subview (result=%lld)\n",
                 static_cast<long long>(result));
    p->dispatch(effEditClose);
    return false;
  }

  int w = width;
  int h = height;
  preferred_size(p, &w, &h);
  if (w > 0 && h > 0) {
    p->embed_content_w = w;
    p->embed_content_h = h;
    container.frame = NSMakeRect(0, 0, w, h);
    for (NSView *child in container.subviews) {
      child.frame = NSMakeRect(0, 0, w, h);
    }
  }

  p->editor_attached = true;
  std::fprintf(stderr, "[vst2-editor] attached instance=%s size=%dx%d\n",
               p->embed_instance_label.empty()
                   ? "<unknown>"
                   : p->embed_instance_label.c_str(),
               w, h);
  return true;
}

} // namespace

// ── Platform entry points used by vst2_processor.cpp ────────────────────────

unsigned long long vst2_embed_editor_mac(SphereDauxVst2Processor *p,
                                         unsigned long long parent_view,
                                         int x, int y, int width, int height) {
  if (!p || !p->effect || !p->has_editor) {
    vst2_set_last_error("VST2 embed editor: no editor on this plug-in");
    return 0;
  }
  NSView *parent = (__bridge NSView *)reinterpret_cast<void *>(
      static_cast<std::uintptr_t>(parent_view));
  if (!parent) {
    vst2_set_last_error("VST2 embed editor: invalid parent NSView");
    return 0;
  }

  if (p->editor_attached && p->editor_native_embed) {
    NSView *existing = (__bridge NSView *)p->editor_native_embed;
    existing.frame = NSMakeRect(x, y, width, height);
    return p->editor_handle;
  }

  int w = width > 0 ? width : 640;
  int h = height > 0 ? height : 480;
  preferred_size(p, &w, &h);

  NSView *container =
      [[NSView alloc] initWithFrame:NSMakeRect(x, y, w, h)];
  [parent addSubview:container];
  p->editor_native_embed = (__bridge_retained void *)container;
  p->embed_mode = true;
  p->embed_host_kind = 0; // child view

  if (!attach_into(p, container, w, h)) {
    [container removeFromSuperview];
    CFRelease(p->editor_native_embed);
    p->editor_native_embed = nullptr;
    p->embed_mode = false;
    vst2_set_last_error("VST2 embed editor: effEditOpen created no view");
    return 0;
  }

  p->editor_handle = vst2_next_editor_handle();
  return p->editor_handle;
}

void vst2_embed_set_bounds_mac(SphereDauxVst2Processor *p, int x, int y,
                               int width, int height) {
  if (!p || !p->editor_native_embed || width <= 0 || height <= 0)
    return;
  NSView *container = (__bridge NSView *)p->editor_native_embed;
  container.frame = NSMakeRect(x, y, width, height);
  p->embed_host_x = x;
  p->embed_host_y = y;
  p->embed_host_w = width;
  p->embed_host_h = height;
  if (p->editor_resizable) {
    for (NSView *child in container.subviews) {
      child.frame = NSMakeRect(0, 0, width, height);
    }
  }
}

unsigned long long vst2_open_editor_mac(SphereDauxVst2Processor *p,
                                        const char *window_id,
                                        const char *title, int width,
                                        int height) {
  if (!p || !p->effect || !p->has_editor) {
    vst2_set_last_error("VST2 editor: no editor on this plug-in");
    return 0;
  }
  p->editor_window_id = window_id ? window_id : "";
  if (title && *title)
    p->editor_title = title;

  if (p->editor_attached && p->editor_native_window) {
    NSWindow *existing = (__bridge NSWindow *)p->editor_native_window;
    [existing makeKeyAndOrderFront:nil];
    return p->editor_handle;
  }

  int w = width > 0 ? width : 640;
  int h = height > 0 ? height : 480;
  preferred_size(p, &w, &h);

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

  DauxVst2EditorWindowDelegate *delegate =
      [[DauxVst2EditorWindowDelegate alloc] init];
  delegate.processor = p;
  window.delegate = delegate;

  NSView *container = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, w, h)];
  window.contentView = container;

  p->editor_native_window = (__bridge_retained void *)window;
  p->editor_native_embed = (__bridge_retained void *)container;
  p->editor_native_delegate = (__bridge_retained void *)delegate;
  p->embed_mode = false;
  p->embed_host_kind = 2; // detached top-level

  if (!attach_into(p, container, w, h)) {
    vst2_close_editor_mac(p);
    vst2_set_last_error("VST2 editor: effEditOpen created no view");
    return 0;
  }

  [window setContentSize:NSMakeSize(p->embed_content_w, p->embed_content_h)];
  [window center];
  [window makeKeyAndOrderFront:nil];

  p->editor_handle = vst2_next_editor_handle();
  return p->editor_handle;
}

void vst2_close_editor_mac(SphereDauxVst2Processor *p) {
  if (!p)
    return;

  if (p->editor_attached && p->effect) {
    p->dispatch(effEditClose);
    p->editor_attached = false;
  }

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
    DauxVst2EditorWindowDelegate *delegate =
        (__bridge_transfer DauxVst2EditorWindowDelegate *)
            p->editor_native_delegate;
    delegate.processor = nullptr;
    p->editor_native_delegate = nullptr;
  }

  p->embed_mode = false;
  p->editor_handle = 0;
}

int vst2_focus_editor_mac(SphereDauxVst2Processor *p) {
  if (!p)
    return 0;
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

void vst2_editor_idle_mac(SphereDauxVst2Processor *p) {
  if (p && p->editor_attached)
    p->dispatch(effEditIdle);
}

// ── C API (macOS) ───────────────────────────────────────────────────────────

extern "C" {

unsigned long long sphere_daux_vst2_embed_editor(SphereDauxVst2Processor *p,
                                                 unsigned long long parent,
                                                 int x, int y, int width,
                                                 int height) {
  return vst2_embed_editor_mac(p, parent, x, y, width, height);
}

void sphere_daux_vst2_embed_set_bounds(SphereDauxVst2Processor *p, int x, int y,
                                       int width, int height) {
  vst2_embed_set_bounds_mac(p, x, y, width, height);
}

void sphere_daux_vst2_embed_refresh(SphereDauxVst2Processor *p) {
  vst2_editor_idle_mac(p);
}

unsigned long long
sphere_daux_vst2_embed_attach_hwnd(SphereDauxVst2Processor *p) {
  if (!p || !p->editor_native_embed)
    return 0;
  return static_cast<unsigned long long>(
      reinterpret_cast<std::uintptr_t>(p->editor_native_embed));
}

void sphere_daux_vst2_embed_detach(SphereDauxVst2Processor *p) {
  vst2_close_editor_mac(p);
}

int sphere_daux_vst2_embed_is_valid(SphereDauxVst2Processor *p) {
  return (p && p->embed_mode && p->editor_attached && p->editor_native_embed)
             ? 1
             : 0;
}

int sphere_daux_vst2_embed_has_visible_ui(SphereDauxVst2Processor *p) {
  if (!p || !p->editor_native_embed)
    return 0;
  NSView *container = (__bridge NSView *)p->editor_native_embed;
  return (container.subviews.count > 0 && !container.hiddenOrHasHiddenAncestor)
             ? 1
             : 0;
}

unsigned long long sphere_daux_vst2_open_editor(SphereDauxVst2Processor *p,
                                                const char *window_id,
                                                const char *title, int width,
                                                int height) {
  return vst2_open_editor_mac(p, window_id, title, width, height);
}

void sphere_daux_vst2_close_editor(SphereDauxVst2Processor *p) {
  vst2_close_editor_mac(p);
}

int sphere_daux_vst2_focus_editor(SphereDauxVst2Processor *p) {
  return vst2_focus_editor_mac(p);
}

// ── Host-owned view host ────────────────────────────────────────────────────
//
// Windows-only, exactly like the VST3 bridge's: the macOS editor is hosted in
// the bridge-owned NSWindow above. These exist so the shared C surface links.

int sphere_daux_vst2_view_attach(SphereDauxVst2Processor *, unsigned long long,
                                 int, int, int *, int *) {
  vst2_set_last_error("host-owned VST2 view is Windows-only");
  return 0;
}

void sphere_daux_vst2_view_detach(SphereDauxVst2Processor *) {}

int sphere_daux_vst2_view_is_attached(SphereDauxVst2Processor *) { return 0; }

int sphere_daux_vst2_view_set_size(SphereDauxVst2Processor *, int, int) {
  return 0;
}

int sphere_daux_vst2_view_get_size(SphereDauxVst2Processor *, int *, int *) {
  return 0;
}

int sphere_daux_vst2_view_can_resize(SphereDauxVst2Processor *) { return 0; }

int sphere_daux_vst2_view_constrain(SphereDauxVst2Processor *, int *, int *) {
  return 0;
}

int sphere_daux_vst2_view_take_resize_request(SphereDauxVst2Processor *, int *,
                                              int *) {
  return 0;
}

void sphere_daux_vst2_view_idle(SphereDauxVst2Processor *) {}

} // extern "C"
