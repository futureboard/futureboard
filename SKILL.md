# Futureboard Native Studio

---
Read @.claude/skills too
---

Use this skill to make controlled, production-real changes to Futureboard
Studio. Read `CLAUDE.md` first, then read `DESIGN.md` for any UI-facing task.

## Establish scope

Treat the native Rust/GPUI application as the product:

```txt
apps/native/studio                 native executable and startup
crates/SphereUIComponents         GPUI shell and shared native UI
crates/SphereDirectAudioEngine    realtime engine and graph
crates/SpherePluginHost           scanner, hosting, bridge, editor attach
crates/SphereWebView              CEF host used by built-in plugin editors
crates/BuiltinAudioPlugins        stock DSP and embedded editor assets
```

Treat Electron and the general-purpose Web UI as retired. Do not:

- add features or fixes to them;
- use their behavior or appearance as product authority;
- validate routine changes with their build commands;
- preserve cross-surface abstractions merely for those retired clients.

Only touch retired code when the user explicitly requests cleanup, removal, or
historical investigation.

Keep one narrow exception: built-in plugins may have React/Vite/Tailwind editor
bundles under `crates/BuiltinAudioPlugins/crates/*/editor` or `editorui`. These
compile into embedded static assets and run inside the native CEF host. Treat
them as plugin editor implementations, never as an application-wide Web UI.

## Work from evidence

1. Inspect `git status --short` before editing.
2. Read the smallest relevant source files and trace the real call path.
3. Identify state ownership and thread/FFI boundaries.
4. Classify touched functions as realtime callback, audio control, plugin
   producer, UI/control, scanner/offline, build-time, or test-only.
5. Implement the smallest patch that fully connects the requested behavior.
6. Validate the smallest relevant package before broadening checks.
7. Report compile/test and manual/runtime/visual evidence separately.

Preserve unrelated user changes. Do not infer permission to rewrite a nearby
system, redesign the UI, or complete a whole roadmap.

## Require real behavior

- Reuse existing models, commands, providers, stores, and registries.
- Connect controls to real state and runtime behavior.
- Disable or label incomplete behavior instead of faking success.
- Preserve save/load and undo semantics when state changes.
- Reuse the verified edition/license state; do not add parallel entitlement
  checks.
- Maintain stable identifiers across UI, project persistence, engine, bridge,
  and plugin instances.
- Add diagnostics that observe the real path, and keep expensive diagnostics
  gated and off realtime threads.

## Protect realtime paths

Ban these operations from audio callbacks and producer-hot steady-state code:

- heap allocation and buffer growth;
- filesystem, network, scanning, or serialization work;
- JSON parsing or string-keyed map lookup per block;
- logging or formatting;
- blocking locks, waits, sleeps, or unbounded queues;
- UI/entity updates;
- panics or unwinding across FFI.

Prefer:

- preallocated buffers and immutable runtime snapshots;
- compact IDs/enums resolved before playback;
- atomics and bounded SPSC/lock-free queues;
- event-driven producer wakeups;
- diagnostic rings drained by non-realtime workers;
- explicit overload/drop policy and monotonic counters.

Do not mask xruns with arbitrary debounce, sleeps, larger stale windows, or
removed freshness guards. Trace request publication, wake, processing, response
publication, and consumption; then fix the slow or incorrectly synchronized
stage.

## Preserve plugin correctness

For every hosted or bridged plugin:

- Route MIDI, automation, parameter changes, state, audio, and responses to the
  exact `instance_id`/track/insert mapping.
- Persist VST3 component and controller state when available as opaque blobs.
- Restore state after instantiation and before playback or editor use.
- Derive process context from the real transport and tempo/time-signature maps.
- Deliver parameter automation end to end, not merely into an unused queue.
- Include plugin and bridge latency in graph delay compensation.
- Keep scanner work and editor lifecycle off the audio thread.
- Isolate plugin failure from the Studio process wherever the architecture
  provides isolation.

For external editor windows, keep the GPUI shell and plugin-owned child view
separate. Measure the client rectangle, handle DPI and resize requests, forward
focus/input correctly, and detach the child before destroying its parent.

## Build native UI deliberately

Follow `DESIGN.md` as the visual and interaction contract.

- Inspect the nearest polished GPUI component before creating a new one.
- Reuse shared components, command routing, and semantic theme tokens.
- Name the layout owner, scroll owner, clip owner, coordinate space, and state
  owner before changing geometry.
- Use a shared transform for drawing and hit-testing.
- Virtualize or cull large track, mixer, MIDI, waveform, and automation views.
- Keep playhead, meters, and other high-frequency visuals isolated from broad
  declarative rerenders.
- Defer parent updates when a child callback would produce nested GPUI entity
  mutation.
- Verify compact, resized, maximized, high-DPI, overflow, focus, and keyboard
  states relevant to the change.

## Implement built-in plugin editor Web UI as an embedded view

Keep the boundary explicit:

```txt
Rust DSP/parameter schema/state
  -> native parameter bridge
  -> embedded editor UI
  -> user gesture
  -> native bridge
  -> exact Rust plugin instance
```

Rules:

- Make Rust the authority for parameter IDs, ranges, defaults, normalization,
  DSP, state, and persistence.
- Keep editor-local state transient and derived from authoritative values.
- Coalesce high-rate gestures without dropping the final committed value.
- Build deterministic static output that can be embedded by
  `builtin_ui_embed`.
- Load through the native custom scheme; require no dev server, remote CDN, or
  network service at runtime.
- Handle CEF create/attach/resize/focus/close lifecycle in native code.
- Keep plugin-specific visual identity inside its editor bounds while following
  the Futureboard interaction and accessibility contract.
- Do not reuse embedded Web UI components in the native application shell.

When changing an editor, inspect its `package.json`, build script, Rust `build.rs`,
embedded asset table, parameter schema, and native host together.

## Choose validation by risk

Start narrow:

```bash
cargo fmt --all -- --check
cargo check -p futureboard_native
cargo check -p sphere_ui_components
cargo check -p sphere-plugin-host
cargo test -p sphere-plugin-host
cargo check -p BuiltinAudioPlugins
```

Broaden only when the change crosses package boundaries:

```bash
cargo check --workspace
cargo test --workspace
```

For a built-in editor, run its own scripts, for example:

```bash
bun run --cwd crates/BuiltinAudioPlugins/crates/rodharerist/editorui build
```

To build a runnable native debug app, build both Studio and its mandatory helper
binaries:

```bash
cargo build -p futureboard_native
cargo build -p sphere-plugin-host --bins
```

Do not treat a successful Rust compile as proof that gestures, audio output,
editor embedding, window lifecycle, project restore, or visual layout work at
runtime. Test those paths explicitly when the task requires them.

## Report precisely

Include:

- the behavior delivered;
- files or packages changed;
- validation commands and exact outcomes;
- manual/runtime/visual checks actually performed;
- anything still unverified or blocked.

Never claim success from a command that was not run or a runtime path that was
not exercised.
