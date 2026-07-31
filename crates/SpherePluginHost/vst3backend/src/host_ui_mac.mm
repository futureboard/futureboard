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

#include <cstdio>

namespace {

/// Marks the application-defined event `wake()` posts. Anything else with this
/// type belongs to AppKit or a plug-in and is forwarded normally.
constexpr short kWakeEventSubtype = 0x5742; // 'WB'

/// Upper bound on events drained per non-blocking pump so one busy editor can
/// never starve the IPC loop.
constexpr unsigned int kMaxEventsPerPump = 64;

bool ui_ready() { return NSApp != nil; }

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
    std::fprintf(stderr,
                 "[plugin-host-ui/mac] NSApplication ready policy=accessory "
                 "main_thread=%d\n",
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
    if (event.type != NSEventTypeApplicationDefined ||
        event.subtype != kWakeEventSubtype) {
      [NSApp sendEvent:event];
      [NSApp updateWindows];
    }
    return 1;
  }
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
