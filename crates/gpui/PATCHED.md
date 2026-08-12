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

## Maintenance Notes

When updating GPUI from upstream, preserve this Futureboard patch or port it
forward deliberately.
