# Ara2Bridge — vendored ARA 2 bindings

Source: [`futureboard/ara2-bridge`](https://github.com/futureboard/ara2-bridge)
at `cbee810f80028676b66cd36d522019831d7c7bda` (workspace version `0.3.0`), itself
a fork of [`entrepeneur4lyf/ara2-bridge`](https://github.com/entrepeneur4lyf/ara2-bridge).

Licensed `MIT OR Apache-2.0`; see `LICENSE-MIT`, `LICENSE-APACHE`, and
`LICENSES/`. `UPSTREAM-README.md` is upstream's README, kept for provenance.

This is a **vendored copy, not a submodule**: it carries local changes (below)
that upstream does not have, and Futureboard's build depends on them. Editing
these crates in place is expected. If a change is worth upstreaming, send it to
the fork separately and re-vendor.

## What was kept

Only the host-side crates Futureboard uses:

| Crate | Why |
| --- | --- |
| `ara2-bridge-sys` | Pregenerated, target-selected raw ARA ABI |
| `ara2-bridge-core` | Shared safe types, validation, registries, dispatch |
| `ara2-bridge-host` | Host services, document graph, plug-in dispatch |
| `ara2-bridge-companion` | VST3 (and Audio Unit) ARA adapters |

Dropped: `ara2-bridge-plugin` and the `ara2-bridge` facade (Futureboard is an ARA
*host*, never an ARA plug-in), `ara2-bridge-testkit`, `xtask`, `fuzz`, examples,
docs, CI, and the maintainer probe/provenance data. The companion's `plugin`
feature went with the plug-in runtime; `host`, `vst3`, `clap`, and
`audio-unit-v2` remain.

## Local changes

Both close real gaps in upstream 0.3.0 — without them an out-of-crate ARA host
cannot be written at all.

1. **`ara2-bridge-host/src/services/mod.rs`** — the `opaque_id!` macro gained
   `as_usize()` and `from_address()`. Service callbacks name graph objects by the
   address of the host record that created them, but the ids were constructible
   only inside the crate, so a host could not map an `AudioSourceId` back to its
   own asset, and `HostServices::revoke_audio_source_readers` was uncallable from
   outside.

2. **`ara2-bridge-host/src/document/mod.rs`** — added
   `DocumentSession::controller_ref()` and `generation()`. A companion API must
   bind a processor to the document controller *before* `bind_extension` can
   validate the resulting extension instance, and that binding call needs the
   controller reference, which had no public accessor.

3. **`ara2-bridge-companion/build.rs`** — rewritten. Upstream resolves both SDKs
   from environment variables and then verifies each checkout against a locked
   commit, tree, and clean working state with `git`, because a published crate
   cannot know what a consumer points it at. Here the SDKs are submodules of this
   repository (`external/ARA_SDK`, `external/vst3sdk`) pinned by the parent repo,
   which is the same guarantee by a stronger mechanism — and `git` verification
   would fail outright in a vendored tree with no `.git`. Environment overrides
   are still honoured.

4. **`ara2-bridge-host/src/document/edit.rs`** — added `audio_source_ref()`,
   `audio_modification_ref()` and `playback_region_ref()` to the edit scope,
   alongside the `musical_context_ref()` and `region_sequence_ref()` upstream
   already exposes there. A plug-in may call back into the host synchronously
   from inside a create call — Melodyne asks for an audio reader from within
   `createAudioSource` — so a host that can only resolve object identities after
   `endEditing()` refuses the plug-in's first request and, since plug-ins do not
   retry, never delivers any audio at all.

5. **Manifests** — `version`/`edition`/`license`/`repository` are spelled out
   instead of inheriting from upstream's `[workspace.package]`, since these crates
   are now members of the Futureboard workspace (which is edition 2024 while these
   are edition 2021). Third-party dependency versions come from Futureboard's
   `[workspace.dependencies]` where one already exists. All four are
   `publish = false`.

## Upgrading

Re-clone the fork at the new revision, copy `src/`, `native/`, and the licence
files over, then re-apply changes 1–4. Change 3 is a whole-file replacement;
1 and 2 are small additive edits marked by their doc comments.
