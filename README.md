<div align="center">

<img width="2111" height="684" alt="Futureboard Studio banner" src="packages/assets/banner.png" />

**The Community Edition of a modern open-source Digital Audio Workstation, built with Rust, GPUI, TypeScript, WebAssembly, and native audio/plugin infrastructure.**

[![CI](https://img.shields.io/github/actions/workflow/status/futureboard/Futureboard/ci.yml?branch=main&style=for-the-badge&label=CI&logo=github&logoColor=white&color=22c55e&labelColor=0f172a)](https://github.com/futureboard/Futureboard/actions/workflows/ci.yml)
[![Status](https://img.shields.io/badge/status-pre--alpha-f59e0b?style=for-the-badge&labelColor=0f172a)](ARCHITECTURE.md)
[![License](https://img.shields.io/badge/license-MIT-22c55e?style=for-the-badge&labelColor=0f172a)](LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-38bdf8?style=for-the-badge&labelColor=0f172a)](CONTRIBUTING.md)
[![Translate on Crowdin](https://img.shields.io/badge/Translate-Crowdin-2e3340?style=for-the-badge&logo=crowdin&logoColor=white&labelColor=0f172a)](https://crowdin.com/project/futureboard-studio)

[![Rust](https://img.shields.io/badge/Rust-2024-f97316?style=for-the-badge&logo=rust&logoColor=white&labelColor=0f172a)](https://rustup.rs)
[![TypeScript](https://img.shields.io/badge/TypeScript-5.x-3b82f6?style=for-the-badge&logo=typescript&logoColor=white&labelColor=0f172a)](https://www.typescriptlang.org)
[![Bun](https://img.shields.io/badge/Bun-runtime-fbf0df?style=for-the-badge&logo=bun&logoColor=black&labelColor=0f172a)](https://bun.sh)
[![WebAssembly](https://img.shields.io/badge/WebAssembly-DSP-7c3aed?style=for-the-badge&logo=webassembly&logoColor=white&labelColor=0f172a)](https://webassembly.org)

[![GPUI](https://img.shields.io/badge/UI-GPUI-06b6d4?style=for-the-badge&labelColor=0f172a)](https://www.gpui.rs)
[![VST3](https://img.shields.io/badge/Plugins-VST3-f97316?style=for-the-badge&labelColor=0f172a)](https://steinbergmedia.github.io/vst3_dev_portal/)
[![CLAP](https://img.shields.io/badge/Plugins-CLAP-a855f7?style=for-the-badge&labelColor=0f172a)](https://cleveraudio.org)
[![Platforms](https://img.shields.io/badge/Platforms-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Web-14b8a6?style=for-the-badge&labelColor=0f172a)](#getting-started)

[Architecture](#architectural-overview) ·
[Getting Started](#getting-started) ·
[Build](#building-the-native-app) ·
[Debugging](#debugging--diagnostics) ·
[Translations](#translations) ·
[Contributing](#contributing) ·
[Forks & Trademarks](#third-party-forks--trademarks)

</div>

---

## Preview

<table>
  <tr>
    <td width="25%" align="center">
      <img src="packages/assets/preview_midi_editor.png" alt="Futureboard Studio MIDI editor" />
      <br />
      <sub>MIDI Editor</sub>
    </td>
    <td width="25%" align="center">
      <img src="packages/assets/preview_mixer.png" alt="Futureboard Studio mixer" />
      <br />
      <sub>Mixer</sub>
    </td>
    <td width="25%" align="center">
      <img src="packages/assets/preview_mainwindow.png" alt="Futureboard Studio workspace preview" />
      <br />
      <sub>Workspace</sub>
    </td>
  </tr>
</table>

---

> [!WARNING]
> **Pre-alpha.** Under active early development — expect breaking changes, incomplete features, and no persistence guarantees. Not ready for production; don't trust it with irreplaceable projects. Nightly builds are test snapshots only.

---

## Community and Professional Editions

This public repository contains **Futureboard Community Edition** source code,
licensed under the [MIT License](LICENSE). A normal Community build does not
contain the private Professional Edition crate, ASIO provider, license activation
client, or other separately licensed commercial components.

Authorized private checkouts may add the Git-ignored
`crates/ExclusiveEdition` source and use [build-private.ps1](build-private.ps1).
The presence of shared extension hooks or the public `professional` build-feature
name does not grant access to, a license for, or redistribution rights in
Professional Edition.

```bash
# Public Community Edition
cargo build -p futureboard_native

# Authorized private checkout only (PowerShell)
.\build-private.ps1

# Equivalent direct Cargo command
cargo build --release -p futureboard_native --features professional

# Fully temporary LLVM + ASIO SDK setup (downloads about 822 MiB)
python .\build-private-temporary.py --accept-asio-license

# Build all default workspace targets with Professional enabled for Studio
python .\build-private-temporary.py --accept-asio-license --workspace
```

---

## Architectural Overview

Futureboard Studio is a Digital Audio Workstation whose primary maintained surface is a **native Rust application built on [GPUI](https://www.gpui.rs)** (the rendering framework behind the Zed editor), driving an in-process Rust audio engine. A secondary **web** (WASM DSP) surface shares layout and engine concepts, but the native app is the main development target.

| Surface              | Path          | Stack                                | Status                 |
| -------------------- | ------------- | ------------------------------------ | ---------------------- |
| **Native** (primary) | `apps/native` | Rust · GPUI · direct audio engine    | Main dev target        |
| Web                  | `apps/web`    | React · TypeScript · Vite · WASM DSP | Tracks native, may lag |

### Core crates

| Crate                     | Purpose                                                    |
| ------------------------- | ---------------------------------------------------------- |
| `SphereDirectAudioEngine` | Native low-latency engine (WASAPI · CoreAudio · ALSA)      |
| `SphereWebAudioCore`      | Web WASM audio core — transport, graph, mixer, meters, DSP |
| `SphereUIComponents`      | Native GPUI UI kit, styling, and layout primitives         |
| `SpherePluginHost`        | Plugin scanning & hosting (VST3, CLAP, AU, VST2 legacy)    |
| `SphereAudioPlugins`      | Built-in real-time DSP (EQ, compression, delay, …)         |

Also: [`plugins/`](plugins/) (stock-plugin editors), [`modules/`](modules/) (noise removal, stem extraction), [`extensions/`](extensions/) (extension templates), [`packages/`](packages/) (shared fonts/icons/assets), [`external/`](external/) (vendored SDKs). See [ARCHITECTURE.md](ARCHITECTURE.md) for the full breakdown.

---

## Getting Started

**Prerequisites:** [Bun](https://bun.sh) · [Rust](https://rustup.rs) 1.85+ (edition 2024) with the `wasm32-unknown-unknown` target · [CMake](https://cmake.org) 3.20+ · a C++ toolchain (MSVC / Xcode CLT / GCC / Clang).

> [!IMPORTANT]
> Vendored SDKs (`external/vst3sdk`, `external/clap`, …) are **git submodules** — clone with `--recursive` (or run `git submodule update --init --recursive` afterwards).

```bash
git clone --recursive https://github.com/futureboard/Futureboard
cd Futureboard
bun install                               # JS workspace dependencies
rustup target add wasm32-unknown-unknown  # web audio core target
```

Run a surface:

```bash
bun run dev:native   # native GPUI client   (= cargo run -p futureboard_native)
bun run dev:web      # React web app
bun run dev:server   # collaboration server
```

---

## Building the Native App

The native client is a Rust binary linking the GPUI UI kit, the direct audio engine, and the plugin host (CMake + a C++ toolchain are required for the native plugin/SDK bridge). The `bun run` scripts wrap the equivalent `cargo` commands.

```bash
bun run build:native:debug   # debug    (= cargo build -p futureboard_native)
bun run build:native         # release  (= cargo build --release -p futureboard_native)
```

The release binary is emitted to `target/release/FutureboardNative` (`.exe` on Windows).

Package distributables (scripts in `packaging/native/`):

```bash
bun run bundle:native:mac       # macOS .app
bun run bundle:native:mac:dmg   # macOS .dmg installer
bun run bundle:native:win       # Windows portable / installer
bun run build:all               # all surfaces (WASM + native)
```

#### macOS universal (Apple Silicon + Intel)

CEF publishes `macosx64` and `macosarm64` as separate distributions — there is no
universal one — so a universal app is produced by packaging each architecture and
merging the two trees with `lipo`:

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo run -p SphereWebView --example install_cef --features installer -- --target universal-macos

for triple in x86_64-apple-darwin aarch64-apple-darwin; do
  cargo run -p xtask -- package --profile release --edition community --plugin all --target "$triple"
done

bash packaging/native/merge-universal-macos.sh \
  out/release/community/macos-x64 out/release/community/macos-arm64 \
  out/release/community/macos-universal

bash packaging/native/bundle-macos.sh out/release/community/macos-universal
bash packaging/native/bundle-macos-dmg.sh
```

`bundle-macos.sh` prefers `macos-universal` when no package directory is passed,
reports the architectures it shipped, and fails on a single-architecture bundle
when `FUTUREBOARD_REQUIRE_UNIVERSAL=1` (release CI sets it). The DMG filename
carries the architecture: `…-macos-universal.dmg`, `…-macos-arm64.dmg`, or
`…-macos-x86_64.dmg`.

### Platform notes

| Platform | Audio backend                           | Setup                                                         |
| -------- | --------------------------------------- | ------------------------------------------------------------- |
| Windows  | WASAPI (exclusive mode / MMCSS planned) | `rustup default stable-msvc`                                  |
| macOS    | CoreAudio                               | `xcode-select --install`                                      |
| Linux    | ALSA (PipeWire/JACK later)              | `sudo apt install libasound2-dev` · `sudo pacman -S alsa-lib` |

---

## Bun Scripts Reference

| Script                                                                          | Description                                   |
| ------------------------------------------------------------------------------- | --------------------------------------------- |
| `dev:web` · `dev:native` · `dev:server`                                         | Run a surface in dev                          |
| `build:web` · `build:wasm` · `build:native[:debug]`                             | Production / debug builds                     |
| `build:audio:plugins`                                                           | Check stock plugin crate + extension template |
| `bundle:native:mac[:dmg]` · `bundle:native:win`                                 | Package distributables                        |
| `cargo:check` · `cargo:build` · `cargo:release` · `cargo:test` · `cargo:clippy` | Rust workspace passthroughs                   |
| `cargo:fmt[:check]` · `check` · `lint` · `fmt`                                  | Format & combined checks                      |

---

## Debugging & Diagnostics

Several subsystems expose verbose logging and debug behavior through environment variables — set the logging variables to `1` to enable them.

| Variable                          | Effect                                                       |
| --------------------------------- | ------------------------------------------------------------ |
| `FUTUREBOARD_PLUGIN_DEBUG`        | Insert add/set/remove/bypass mutations + engine-sync details |
| `FUTUREBOARD_PLUGIN_VIEW_DEBUG`   | Native plugin editor lifecycle and view attachment           |
| `FUTUREBOARD_ROUTING_DEBUG`       | Send, return, and bus routing graph diagnostics              |
| `GPUI_DISABLE_DIRECT_COMPOSITION` | Windows composition workaround for native plugin UI          |
| `FUTUREBOARD_PLUGIN_EDITOR_MODE`  | Plugin editor mode selection                                 |

```bash
# bash
FUTUREBOARD_PLUGIN_VIEW_DEBUG=1 cargo run -p futureboard_native
# PowerShell
$env:FUTUREBOARD_PLUGIN_VIEW_DEBUG=1; cargo run -p futureboard_native
```

---

## Repository Layout

```text
Futureboard
├─ apps/         native · web
├─ crates/       SphereDirectAudioEngine · SphereWebAudioCore · SphereUIComponents · SpherePluginHost · SphereAudioPlugins
├─ packages/     assets · shared
├─ plugins/      modules/      extensions/
├─ external/     vendored SDKs
└─ packaging/    native bundle scripts
```

---

## Roadmap

Toward a usable native DAW foundation: a stable native GPUI shell, audio clip editing, timeline & MIDI editing, mixer routing, native plugin hosting (VST3 editor embedding, CLAP support), a project file format, automation lanes, audio export, and cross-platform packaging. See [ARCHITECTURE.md](ARCHITECTURE.md) for current status.

---

## Translations

Help translate Futureboard Studio through the
[Futureboard Studio project on Crowdin](https://crowdin.com/project/futureboard-studio).
The English source catalog and downloaded locale files live under
`packages/shared/locales`; see the [translation guide](packages/shared/locales/translation.md)
for the catalog format and maintainer workflow.

---

## Contributing

Contributions are welcome — bug reports, build testing, documentation, UI fixes, plugin-hosting and audio-engine work, and platform support. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request; UI work also follows [DESIGN.md](DESIGN.md) and [AGENTS.md](AGENTS.md).

---

## Third-Party Forks & Trademarks

The MIT License permits forks and modified distributions of covered Community
Edition source code. It does not authorize third parties to present modified
software as an official Futureboard product or to imply endorsement.

Third-party forks that refer to Futureboard in product-facing materials must use
distinct product branding and prominently display this notice:

> This project is an independent third-party fork of Futureboard Community
> Edition. It is not affiliated with, endorsed by, sponsored by, or supported
> by the Futureboard project or its maintainers.

Factual statements such as "based on Futureboard Community Edition" are allowed
when accurate and when the fork's own branding is more prominent. See
[TRADEMARKS.md](TRADEMARKS.md) for the complete naming, logo, redistribution,
fork, and Professional Edition policy.

---

## License

Futureboard Community Edition source code is licensed under the
[MIT License](LICENSE). Required third-party notices and licenses remain
applicable; see [NOTICE.md](NOTICE.md) and the relevant dependency directories.

The Futureboard names, logos, product identity, and trade dress are not licensed
for use as the branding of modified products merely because the source code is
open source. See [TRADEMARKS.md](TRADEMARKS.md). Futureboard Professional Edition
components are separately licensed and are not part of the public Community
Edition repository.
