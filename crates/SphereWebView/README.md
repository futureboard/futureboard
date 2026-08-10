# SphereWebView

CEF hosting for built-in Futureboard plugin editors. Editors use windowless
rendering on every platform. Windows takes the accelerated OSR path and copies
CEF D3D11 shared textures into GPUI-owned GPU textures; Linux and macOS retain
the software framebuffer path.

Normal workspace builds do not download the pinned SDK. Install it explicitly:

```sh
cargo run -p SphereWebView --example install_cef --features installer
```

Pass `-- --force` to replace the current host installation under
`build/cef/150.0.11/<platform>`.

`cef-dll-sys` reads `CEF_PATH` during compilation. Export the path for the Cargo
target before invoking Cargo; the repository deliberately has no global
platform default:

```sh
# Linux x86_64
export CEF_PATH="$PWD/build/cef/150.0.11/cef_linux_x86_64"
```

CI resolves the equivalent Windows, Linux, Intel macOS, and Apple Silicon macOS
paths in `.github/workflows/set-cef-path.sh`. macOS bindgen builds must also set
`LIBCLANG_PATH` to the installed LLVM library directory. Windows ASIO builds
using the repository-local LLVM tools should set `LIBCLANG_PATH` to the absolute
`.bin/bin` directory. This is intentionally not a global Cargo setting because
that Windows path breaks bindgen discovery on other hosts.

The executable owns process dispatch and must:

1. create the scheme `CefApp`;
2. pass that exact object to `runtime::execute_subprocess` before any normal app
   startup;
3. exit immediately when CEF returns a subprocess code;
4. transfer the same object to browser-process `CefRuntime::initialize`;
5. drive `CefRuntime::do_message_loop_work` from the initializing UI thread.

Futureboard uses CEF's integrated subprocess model. The
`browser_subprocess_path` setting is intentionally empty, which means CEF
re-launches the current executable and enters the dispatch gate above. A
separate helper path belongs to packaging and must not be selected unless that
helper is actually shipped.

Set `FUTUREBOARD_PLUGIN_VIEW_DEBUG=1` for detailed browser lifecycle, resource,
reference-count, and console diagnostics. Initialization failures, browser
creation failures, load failures, blocked navigation, renderer termination, and
shutdown remain visible without debug logging.
