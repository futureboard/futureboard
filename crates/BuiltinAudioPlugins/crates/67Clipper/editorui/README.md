# 67Clipper editor

Embedded editor UI for the `clipper67` built-in plugin. Svelte 5 + Vite,
bundled to a single `dist/index.html` by `vite-plugin-singlefile`, then
embedded into the library and served by the native CEF host at
`mikoplugin://clipper67/index.html`.

## Character

Flatline-style stage: a full-height scrolling waveform (muted blue input body,
red gain-reduction/clip overlay from the top), a floating rounded control
panel over the bottom-left of the stage (mode pushbuttons, Threshold / Shape /
Ceiling), thin In/Out/GR bar meters on the right, and a bottom strip for
Mix, DC Filter, and Stereo Link. Dark charcoal surfaces, one neon-blue signal
accent, soft red reserved for clipping.

## Wire contract

Mirrors `crates/67Clipper/src/ipc.rs` exactly:

```
power, mode, thresholdDb, shape, ceilingDb, mix, stereoLink, dcFilter
```

`mode` wire values: `clip = 0`, `hybrid = 1`, `limit = 2`. `params.ts` and
`rust.ts` keep the editor's copy pinned to the Rust source via
`params.test.ts`.

## Develop

```bash
bun install
bun run dev
bun run build
bun test
```

After a production build:

```bash
cargo build -p clipper67
```
