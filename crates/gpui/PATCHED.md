# Patched GPUI

This vendored GPUI library has been modified for Futureboard Studio.

## Device-Scale Anti-Aliasing

Windows HLSL and WGPU WGSL SDF edges use fragment derivatives (`fwidth`) so
coverage follows the actual device-pixel footprint at fractional scale factors.
The compositing helper also avoids undefined RGB values for fully transparent
pixels.

The Futureboard workspace enables WGPU's DirectX 12 backend for Windows GPU
surfaces.

## macOS Traffic-Light Guard

`MacWindowState::move_traffic_light` now returns early when any of the three
standard window buttons is missing. AppKit removes all of them while a window is
presented as a sheet, and a style mask without `.miniaturizable` never creates
the minimize button, so the previous unconditional `frame` message was sent to a
nil button and aborted the process. Futureboard opens non-minimizable windows for
dialogs and session transactions, so this guard is required.

## Frame Profile Hook

`gpui::frame_profile` publishes the duration of the two phases an embedder
cannot otherwise see — `Window::draw` (element tree build, layout, prepaint,
paint) and `Window::present` (handing the scene to the platform) — as relaxed
atomics from the most recent frame.

Futureboard's Profiler overlay times its own `render` functions, but those cover
only element construction. Without this hook a 40 ms frame containing 0.2 ms of
app work is indistinguishable from a broken profiler, and there is nothing to
optimize against.

It also splits the draw into its phases — prepaint (element tree build plus
layout), paint, and accessibility-tree rebuild — and reports the readings that
say *why* a phase is expensive: microseconds spent shaping text that missed the
two-frame line-layout cache (`text_system/line_layout.rs`), the layout node
count (`taffy.rs`), and the primitive count of the finished scene. Layout nodes
matter more than primitives here: containers lay out without drawing, so a frame
can walk a large tree while emitting few primitives.

Cost is a handful of `Instant::now()` calls per frame plus one per shaping cache
miss; no behavior change.

## Maintenance Notes

When updating GPUI from upstream, preserve this Futureboard patch or port it
forward deliberately.
