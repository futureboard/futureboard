# VerbSpace editor

Embedded editor UI for the `verbspace` built-in plugin. Svelte 5 + Vite,
bundled to a single self-contained `dist/index.html` that `build.rs` embeds into
the plugin library and the native CEF host serves at
`mikoplugin://verbspace/index.html`.

This is a plugin view, not an application. It has no router, no dev server
dependency at runtime, and no network access — a strict single-file bundle is
the deliverable.

## Commands

```bash
bun install
bun run build      # svelte-check, then the embedded single-file bundle
bun test           # schema/model checks (also run in the Rust-side workflow)
bun run dev        # standalone preview; the bridge no-ops without a host
```

After `bun run build`, rebuild the crate so the new assets are embedded:

```bash
cargo build -p verbspace
```

## Where authority lives

Rust owns everything that decides what a value *means*:

| Concern | Owner |
| --- | --- |
| parameter ids and wire indices | `../src/ipc.rs` (`UI_PARAM_IDS`) |
| ranges, clamping, defaults | `../src/ipc.rs`, `../src/lib.rs` |
| DSP, persistence, state blob | `../src/lib.rs`, `../src/ipc.rs` |
| taper, layout, formatting | `src/params.ts` |

`src/params.ts` and `src/model.ts` duplicate a handful of Rust constants so the
UI can lay out a control and draw the decay display. `src/params.test.ts` and
`src/model.test.ts` parse the real `.rs` files and compare, so a change on
either side that is not mirrored fails the tests rather than shipping a UI that
quietly disagrees with the DSP.

## The decay display

`DecayView` draws a model computed from the current parameters — the RT60
envelope for the low, mid and damped-high bands, the pre-delay gap, and the
tank's line arrivals. It is not measured audio and is labelled as such in the
UI; no analyser feed exists for this plugin.

## Bridge

`src/bridge.ts` speaks the shared built-in editor protocol: the host pushes
`futureboard.selectInstance` with the authoritative state, the page answers
`futureboard.instanceReady`, and gestures go back as `futureboard.setParams`
batched per animation frame and tagged with the binding they were made against.
Stale batches are dropped by the host, so an edit made against a torn-down
instance can never land on its replacement.
