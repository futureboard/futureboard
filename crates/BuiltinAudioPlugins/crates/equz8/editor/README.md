# EQUZ8 Editor

Embedded CEF editor for the built-in EQUZ8 dynamic EQ.

Visual direction:

- **Pro-Q 4** — graph-first analyser stage, numbered band nodes, teal sum
  curve with d3 area fill, solo/audition gestures.
- **Serum-ish** — metallic knobs with coloured value arcs, dense bottom rack,
  graphite chassis.

Libraries:

- `d3` — log/linear scales, monotone curve + area paths
- `animejs` — intro choreography, band-select pulse, rack flash, knob snaps

```bash
bun install
bun run build
```

Assets are embedded by `equz8` `build.rs` via `builtin_ui_embed`.
