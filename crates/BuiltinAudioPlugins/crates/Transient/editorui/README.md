# Transient editor

Embedded editor UI for the `transient` built-in plugin. Svelte 5 + Vite,
bundled to a single `dist/index.html` by `vite-plugin-singlefile`, then
embedded into the library and served by the native CEF host at
`mikoplugin://transient/index.html`.

## Character

Attack / sustain shaper stage: scrolling waveform with teal signal accent,
floating control panel (Attack / Sustain / Speed), In/Out/Shape meters, and a
footer for Mix + Stereo Link. Dark charcoal surfaces shared with 67Clipper /
BurnLimit, distinct teal accent.

## Wire contract

Mirrors `crates/Transient/src/ipc.rs` exactly:

```
power, attack, sustain, speed, mix, stereoLink
```

`params.ts` and `rust.ts` keep the editor's copy pinned to the Rust source via
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
cargo build -p transient
```
