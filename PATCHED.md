# Futureboard Patch Record

This repository contains local changes to vendored libraries. These changes are
intentional and should be preserved when updating dependencies.

## GPUI

Path: `crates/gpui`

Reason: Futureboard Studio needs custom native DAW cursors and app-specific
desktop behavior that is not present in upstream GPUI.

Current patches:

- Added Futureboard cursor styles to `gpui::CursorStyle`.
- Added Windows PNG-to-HCURSOR loading for bundled cursor assets.
- Mapped the default Windows Arrow cursor to Futureboard's custom Arrow cursor.
- Set custom cursor rendering to use `@0.5x` assets as the default size.
- Added macOS/Linux fallback mappings for the Futureboard cursor styles.
- Guarded macOS traffic-light repositioning against missing standard window
  buttons, which AppKit removes for sheets and for non-minimizable style masks.

When rebasing GPUI, keep this file and `crates/gpui/PATCHED.md` updated with
the exact local changes that remain.
