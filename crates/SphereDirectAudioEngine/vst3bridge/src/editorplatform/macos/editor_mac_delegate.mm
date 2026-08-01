#include "editor_mac_internal.hpp"

#include <cmath>

@implementation DauxEditorWindowDelegate

- (void)windowDidResize:(NSNotification *)notification {
  if (self.applyingHostResize)
    return;
  NSWindow *window = notification.object;
  SphereDauxVst3Processor *proc = self.processor;
  if (!proc || !window)
    return;

  NSSize size = window.contentView.bounds.size;
  int width = (int)std::llround(size.width);
  int height = (int)std::llround(size.height);
  if (width <= 0 || height <= 0)
    return;

  int agreed_width = width;
  int agreed_height = height;
  if (!sphere_daux_editor_constrain_view_size(proc, &agreed_width,
                                               &agreed_height))
    return;
  if (agreed_width != width || agreed_height != height) {
    daux_resize_editor_content(proc, agreed_width, agreed_height, false,
                               "windowDidResize.constraint");
  }
  sphere_daux_editor_notify_resize(proc, agreed_width, agreed_height);
}

- (BOOL)windowShouldClose:(NSWindow *)sender {
  (void)sender;
  SphereDauxVst3Processor *proc = self.processor;
  if (proc) {
    // The plugin-host process polls this flag and reports EditorClosed, so the
    // main app drops its session instead of believing the editor is still open.
    sphere_daux_editor_signal_user_close(proc);
    // Detach + release — this zeroes editor_native_window so re-entrant
    // calls from windowWillClose: or any queued callbacks are no-ops.
    close_editor_mac(proc);
  }
  return NO; // we handle the close ourselves via close_editor_mac
}

@end
