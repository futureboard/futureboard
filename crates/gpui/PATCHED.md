# Patched GPUI

This vendored GPUI library has been modified for Futureboard Studio.

## Custom Cursor Patch

Futureboard added app-specific cursor styles to `gpui::CursorStyle`:

- `FutureboardArrow`
- `FutureboardSelect`
- `FutureboardMarquee`
- `FutureboardMove`
- `FutureboardFadeIn`
- `FutureboardFadeOut`
- `FutureboardResizeHorizon`
- `FutureboardResizeLeft`
- `FutureboardResizeRight`

On Windows, these styles are rendered as native `HCURSOR` handles decoded from
bundled PNG assets in `packages/shared/cursors`. Runtime cursor selection uses
the `@0.5x` assets as the default size so the cursors match DAW chrome density.
The standard `CursorStyle::Arrow` is also mapped to the Futureboard custom Arrow
cursor on Windows.

On macOS and Linux, the same styles currently fall back to the closest native
system cursor so the API remains cross-platform.

## macOS Traffic-Light Guard

`MacWindowState::move_traffic_light` now returns early when any of the three
standard window buttons is missing. AppKit removes all of them while a window is
presented as a sheet, and a style mask without `.miniaturizable` never creates
the minimize button, so the previous unconditional `frame` message was sent to a
nil button and aborted the process. Futureboard opens non-minimizable windows for
dialogs and session transactions, so this guard is required.

## Maintenance Notes

When updating GPUI from upstream, preserve these Futureboard patches or port
them forward deliberately. Do not remove the custom cursor variants unless
Futureboard has a replacement cursor pipeline.
