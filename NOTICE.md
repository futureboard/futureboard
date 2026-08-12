Futureboard includes vendored and patched third-party code.

GPUI is vendored under `crates/gpui` from the Zed project and is licensed
under Apache-2.0. Futureboard carries local patches on top of that upstream
library for native DAW integration.

Current Futureboard GPUI patches include a macOS traffic-light safety guard for
sheet and non-minimizable windows.

See `crates/gpui/PATCHED.md` for the GPUI-specific patch record.
