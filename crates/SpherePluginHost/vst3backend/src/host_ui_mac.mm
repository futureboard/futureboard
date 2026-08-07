// host_ui_mac.mm — AppKit application + event pump for the plugin host process.
//
// VST3 editors on macOS are NSViews the plug-in draws into a window this
// process owns (see editor_mac.mm in the audio engine's vst3bridge). AppKit
// only delivers events, timers, and Core Animation commits to a process that
// has an NSApplication whose event queue is being drained, so without the
// functions below a plug-in editor window is created but never appears, never
// paints, and never responds to input.
//
// The host's UI thread (its main thread, which also owns plug-in lifecycle) is
// an IPC loop, not `[NSApp run]`, so it drains the queue cooperatively:
//
//   sphere_plugin_host_mac_ui_init()   once, on the main thread, at startup
//   sphere_plugin_host_mac_ui_pump()   non-blocking drain each iteration
//   sphere_plugin_host_mac_ui_wait()   bounded wait that also runs the run loop
//   sphere_plugin_host_mac_ui_wake()   from any thread, to cut a wait short
//
// Compiled as Objective-C++ with -fobjc-arc (see build.rs).

#import <AppKit/AppKit.h>
#import <dispatch/dispatch.h>

#include <cstdio>
#include <cstdint>

namespace {

/// Marks the application-defined event `wake()` posts. Anything else with this
/// type belongs to AppKit or a plug-in and is forwarded normally.
constexpr short kWakeEventSubtype = 0x5742; // 'WB'

/// Upper bound on events drained per non-blocking pump so one busy editor can
/// never starve the IPC loop.
constexpr unsigned int kMaxEventsPerPump = 64;

/// `kVK_Space` from Carbon's Events.h, without pulling Carbon in.
constexpr unsigned short kSpaceKeyCode = 49;

bool ui_ready() { return NSApp != nil; }

/// Transport-key presses claimed from plug-in editors, not yet reported to the
/// main app. Written and drained on the UI thread only.
unsigned int g_transport_toggle_requests = 0;

/// True when the key window is currently editing text, so Space belongs to the
/// caret rather than the transport (preset name fields, search boxes).
bool key_window_is_editing_text() {
  NSWindow *window = NSApp.keyWindow;
  if (window == nil) {
    return false;
  }
  NSResponder *responder = window.firstResponder;
  if ([responder isKindOfClass:[NSTextView class]]) {
    // A field editor stands in for the NSTextField that owns it; either way a
    // caret is live in this window.
    return true;
  }
  return [responder isKindOfClass:[NSTextField class]];
}

/// Claim a bare Space keyDown for the DAW transport. Returns true when the
/// event was consumed and must not reach the plug-in.
///
/// Deliberately narrow, matching the Windows pump: no Command / Control /
/// Option held, and no text field editing in the key window.
bool claim_transport_key(NSEvent *event) {
  if (event.type != NSEventTypeKeyDown || event.keyCode != kSpaceKeyCode) {
    return false;
  }
  if (event.isARepeat) {
    return false; // one press, one toggle
  }
  NSEventModifierFlags blocking = NSEventModifierFlagCommand |
                                  NSEventModifierFlagControl |
                                  NSEventModifierFlagOption;
  if ((event.modifierFlags & blocking) != 0) {
    return false;
  }
  if (key_window_is_editing_text()) {
    return false;
  }
  g_transport_toggle_requests++;
  return true;
}

} // namespace

/// Create the NSApplication and put it in a state where windows can be shown.
/// Idempotent; must run on the process main thread. No-op off the main thread.
extern "C" void sphere_plugin_host_mac_ui_init(void) {
  static bool initialized = false;
  if (initialized || !NSThread.isMainThread) {
    return;
  }
  initialized = true;

  @autoreleasepool {
    NSApplication *app = [NSApplication sharedApplication];
    // Accessory, not Regular: plug-in editor windows must be able to become key
    // and take keyboard input, but this helper is not an application the user
    // launched — it gets no Dock tile and no menu bar of its own. The Studio
    // stays the app the user sees.
    [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
    [app finishLaunching];

    // Local monitor covers every key window in this process, including plug-in
    // views that never go through nextEventMatchingMask in our pump (CEF/JUCE
    // nested run loops). Returning nil swallows the event — same contract as
    // the Windows WH_KEYBOARD_LL path.
    [NSEvent
        addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown
                                     handler:^NSEvent *(NSEvent *event) {
                                       if (claim_transport_key(event)) {
                                         return nil;
                                       }
                                       return event;
                                     }];

    std::fprintf(stderr,
                 "[plugin-host-ui/mac] NSApplication ready policy=accessory "
                 "transport_key_monitor=on main_thread=%d\n",
                 (int)NSThread.isMainThread);
  }
}

/// Drain whatever is already queued without blocking. Returns the number of
/// events dispatched (the IPC loop's spin/pump-gap watchdog input).
extern "C" unsigned int sphere_plugin_host_mac_ui_pump(void) {
  if (!ui_ready()) {
    return 0;
  }
  unsigned int dispatched = 0;
  while (dispatched < kMaxEventsPerPump) {
    @autoreleasepool {
      NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny
                                          untilDate:NSDate.distantPast
                                             inMode:NSDefaultRunLoopMode
                                            dequeue:YES];
      if (!event) {
        break;
      }
      if (claim_transport_key(event)) {
        dispatched++;
        continue;
      }
      if (event.type != NSEventTypeApplicationDefined ||
          event.subtype != kWakeEventSubtype) {
        [NSApp sendEvent:event];
      }
      dispatched++;
    }
  }
  if (dispatched > 0) {
    @autoreleasepool {
      [NSApp updateWindows];
    }
  }
  return dispatched;
}

/// Wait up to `timeout_ms` for the next event, dispatching it if one arrives.
/// Returns 1 when the wait ended on an event rather than the timeout.
///
/// This is also what keeps an editor alive between IPC commands: blocking here
/// runs the main run loop, so plug-in timers, Core Animation commits and blocks
/// queued on the main dispatch queue all get serviced.
extern "C" int sphere_plugin_host_mac_ui_wait(unsigned int timeout_ms) {
  if (!ui_ready()) {
    return 0;
  }
  @autoreleasepool {
    NSDate *deadline =
        [NSDate dateWithTimeIntervalSinceNow:(double)timeout_ms / 1000.0];
    NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny
                                        untilDate:deadline
                                           inMode:NSDefaultRunLoopMode
                                          dequeue:YES];
    if (!event) {
      return 0;
    }
    if (claim_transport_key(event)) {
      return 1;
    }
    if (event.type != NSEventTypeApplicationDefined ||
        event.subtype != kWakeEventSubtype) {
      [NSApp sendEvent:event];
      [NSApp updateWindows];
    }
    return 1;
  }
}

/// Number of transport-key presses claimed from plug-in editors since the last
/// call. The IPC loop turns each into a `HostEvent::TransportToggleRequested`.
extern "C" unsigned int sphere_plugin_host_mac_ui_take_transport_toggles(void) {
  unsigned int taken = g_transport_toggle_requests;
  g_transport_toggle_requests = 0;
  return taken;
}

/// End an in-progress wait early. Safe to call from any thread — the IPC reader
/// uses it so a command never waits out the full timeout.
extern "C" void sphere_plugin_host_mac_ui_wake(void) {
  if (!ui_ready()) {
    return;
  }
  @autoreleasepool {
    NSEvent *wake = [NSEvent otherEventWithType:NSEventTypeApplicationDefined
                                       location:NSZeroPoint
                                  modifierFlags:0
                                      timestamp:0
                                   windowNumber:0
                                        context:nil
                                        subtype:kWakeEventSubtype
                                          data1:0
                                          data2:0];
    [NSApp postEvent:wake atStart:YES];
  }
}

/// Bring an existing host-owned editor NSWindow to the front. `handle` is an
/// NSWindow* bit-cast to u64 (AU path). Returns 1 on success, 0 if the handle
/// is not a live window owned by this process.
extern "C" int sphere_plugin_host_mac_ui_focus_window(unsigned long long handle) {
  if (handle == 0 || !ui_ready()) {
    return 0;
  }
  if (![NSThread isMainThread]) {
    __block int result = 0;
    dispatch_sync(dispatch_get_main_queue(), ^{
      result = sphere_plugin_host_mac_ui_focus_window(handle);
    });
    return result;
  }
  @autoreleasepool {
    NSWindow *window = (__bridge NSWindow *)(void *)(uintptr_t)handle;
    if (window == nil || ![NSApp.windows containsObject:window]) {
      return 0;
    }
    [window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
    return 1;
  }
}
