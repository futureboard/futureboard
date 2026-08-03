# Architecture

> Status: **pre-alpha**. This document describes the intended shape of the
> project and how its parts relate today. Names, boundaries, and crate
> membership are still moving.

This file is the high-level map. For per-crate detail, read the crate READMEs;
for contribution rules, see [CONTRIBUTING.md](CONTRIBUTING.md) and
[DESIGN.md](DESIGN.md).

---

## Surfaces

Futureboard has one primary surface and a set of secondary ones. They are not
peers — development targets the native app first, and the others follow.

### Futureboard Studio — native (primary)

- **Location:** [`apps/native`](apps/native) + the GPUI UI kit in
  [`crates/SphereUIComponents`](crates/SphereUIComponents).
- **Stack:** Rust + [GPUI](https://www.gpui.rs) (the rendering framework behind
  the Zed editor), driving the in-process Rust audio engine directly.
- **Role:** This *is* Futureboard Studio. It is the canonical UI, the surface
  new features land on first, and the only one with first-class access to the
  native audio engine and plugin host. When this document says "the app"
  unqualified, it means the native app.

### Futureboard Lite — web (secondary)

- **Location:** [`apps/web`](apps/web) (React + TypeScript + Vite).
- **Stack:** Browser SPA backed by a WebAssembly DSP core
  ([`crates/SphereWebAudioCore`](crates/SphereWebAudioCore)) running in an
  AudioWorklet.
- **Role:** A reduced, browser-deliverable surface. It mirrors concepts from the
  native app but lags it in feature coverage and does not host native plugins.
  Maintained, but secondary.

### Futureboard Express — Electron (secondary / legacy)

- **Location:** [`apps/electron`](apps/electron).
- **Stack:** Electron wrapper around the web frontend, bridged to native audio
  components over an N-API control bridge.
- **Role:** Reference/legacy desktop path. The native app is the recommended
  desktop experience; Express is not where new work goes.

### Collaboration server

- **Location:** [`apps/server`](apps/server) — stream sync and file hosting for
  collaboration. Supporting infrastructure, not a UI surface.

---

## Audio engine ownership

Audio is owned by Rust on every surface. There are two engine implementations
that deliberately do **not** share a runtime:

| Engine | Crate | Owns | Used by |
|---|---|---|---|
| **Direct audio engine (DAUx)** | [`crates/SphereDirectAudioEngine`](crates/SphereDirectAudioEngine) | The realtime graph, transport, mixer, device I/O (WASAPI professional + MMCSS / CoreAudio / ALSA via `cpal`), and the realtime render thread. | Native app (primary). Also exposes a C/N-API surface for the Electron bridge. |
| **Web audio core** | [`crates/SphereWebAudioCore`](crates/SphereWebAudioCore) | A web-compatible transport, flat audio graph (tracks → master), mixer, and meters, compiled to WASM and run in an AudioWorklet. | Web app (Lite). |

Rules of ownership:

- **The engine owns realtime state.** UI code never mutates DSP state on the
  audio thread. Inserts are owned by the engine as `RuntimeInsert { kind,
  params, dsp }`; the UI sends intent, the engine applies it. See
  [`crates/SphereAudioPlugins`](crates/SphereAudioPlugins) for the realtime-safe
  DSP contract (no allocation, locks, I/O, or logging on the render path).
- **Native plugins are hosted, not embedded ad hoc.**
  [`crates/SpherePluginHost`](crates/SpherePluginHost) wraps the C++ VST3/CLAP
  SDKs (`external/vst3sdk`, `external/clap`) and embeds the native editor inside
  the GPUI window. This path is native-only.
- **The two engines are intentionally separate.** Do not try to unify DAUx and
  the web core into one runtime; they target different constraints (professional
  low-latency native I/O vs. a sandboxed AudioWorklet). Shared *concepts* live in
  descriptors and IDs (e.g. stable plugin IDs like `sphere.eq8`), not in shared
  realtime code.

> Stabilization note: the audio engine is treated as load-bearing and is **not**
> rewritten as part of routine cleanup. Changes to realtime code are deliberate
> and reviewed against the realtime-safety rules above.

---

## Crate map (`crates/`)

Active, workspace-member crates:

- **SphereDirectAudioEngine** — native realtime audio engine (DAUx). *(see above)*
- **SphereWebAudioCore** — WASM DSP core for the web surface.
- **SphereUIComponents** — GPUI/Skia native UI kit and CoreUI for the native app.
- **SpherePluginHost** — VST3/CLAP host wrapper, native editor embedding.
- **SphereAudioPlugins** — built-in realtime-safe stock DSP (EQ, comp, delay…).
- **SphereAudioEditor**, **SphereVoiceTune** — feature crates consumed by the
  native app.

Extension templates live outside the workspace in [`extensions/`](extensions)
(`template`, `template-react`, `template-vue`) and are built independently.

---

## Experimental crates policy

Some crates under `crates/` are **placeholders** — a name and a `main.rs` stub
with no real implementation yet (e.g. encoder, media utils, noise remover, stem
extractor, settings, extension host). To keep the build, CI, and dependency
graph honest, these are **quarantined**: listed under `exclude` in the workspace
[`Cargo.toml`](Cargo.toml) rather than built as members.

What this means in practice:

- A quarantined crate is **not** compiled, linted, or tested by CI, and does not
  appear in `Cargo.lock`. Its directory stays in the tree as a reserved name and
  a place to start.
- **Do not** add a placeholder crate to the workspace members until it has real,
  compiling, tested content. Promote it by removing its line from the `exclude`
  list (and running `cargo build` to relock).
- **Do not** let other crates depend on a quarantined crate — by definition it
  has nothing to offer yet.
- New experimental work should either start quarantined (stub + `exclude` entry)
  or live on a branch until it builds and has at least a smoke test. The default
  is: if it is in the workspace, it must pass `fmt`, `clippy -D warnings`, and
  `cargo test`.

This policy keeps the green-CI surface equal to the *maintained* surface, which
is the point of the stabilization pass that introduced it.

---

## CI gates

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) enforces correctness on
every push and pull request (release/packaging stays in `build-desktop.yml` and
`daily-build.yml`):

- **Rust format** — `cargo fmt --all --check`.
- **Rust clippy + tests** — `cargo clippy --workspace --all-targets -- -D
  warnings` and `cargo test --workspace` (with audio SDK submodules and Linux
  system deps initialized).
- **Web** — `tsc -b` typecheck and `eslint` lint for `apps/web`.

Because quarantined crates are excluded from the workspace, these gates cover
exactly the maintained code and nothing that is known-incomplete.
