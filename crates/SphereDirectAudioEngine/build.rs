// Required by napi-build to generate platform-specific .def / linker files
// for the native Node.js addon (.node output).
extern crate napi_build;

fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let sdk_root = manifest_dir.join("../../external/vst3sdk");
    let bridge_root = manifest_dir.join("vst3bridge");

    // Trigger rebuilds when any bridge source or header changes.
    for name in &[
        "include/sphere_daux_vst3_processor.h",
        "include/sphere_daux_editor_bridge.h",
        "include/editor_windows.hpp",
        "src/vst3_processor.cpp",
        "src/editor_windows.cpp",
        "src/editorplatform/windows/editor_windows_api.cpp",
        "src/editorplatform/windows/editor_windows_common.cpp",
        "src/editorplatform/windows/editor_windows_create.cpp",
        "src/editorplatform/windows/editor_windows_internal.hpp",
        "src/editorplatform/windows/editor_windows_rendering.cpp",
        "src/editorplatform/windows/editor_windows_titlebar.cpp",
        "src/editorplatform/windows/editor_windows_utils.cpp",
        "src/editorplatform/windows/editor_windows_windowproc.cpp",
        "src/editorplatform/macos/editor_mac.mm",
        "src/editorplatform/macos/editor_mac_delegate.mm",
        "src/editorplatform/macos/editor_mac_helpers.mm",
        "src/editorplatform/macos/editor_mac_internal.hpp",
        "src/editor_linux.cpp",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            bridge_root.join(name).display()
        );
    }

    // Baseline x64 VST3 bridge — no /arch:AVX2 or target-cpu=native.
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++20")
        .flag_if_supported("/Zc:char8_t-")
        .flag_if_supported("/EHsc")
        .include(bridge_root.join("include"))
        .include(&sdk_root)
        .include(sdk_root.join("pluginterfaces"))
        .include(sdk_root.join("public.sdk/source"))
        .file(bridge_root.join("src/vst3_processor.cpp"))
        .file(bridge_root.join("src/editor_windows.cpp"))
        .file(sdk_root.join("pluginterfaces/base/coreiids.cpp"))
        .file(sdk_root.join("pluginterfaces/base/funknown.cpp"))
        .file(sdk_root.join("pluginterfaces/base/ustring.cpp"))
        .file(sdk_root.join("public.sdk/source/common/commonstringconvert.cpp"))
        .file(sdk_root.join("public.sdk/source/common/memorystream.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/utility/stringconvert.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/vstinitiids.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/hosting/hostclasses.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/hosting/pluginterfacesupport.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/hosting/module.cpp"));

    apply_vst3_platform_config(&mut build, &sdk_root, &bridge_root);

    build.compile("sphere_daux_vst3_processor");
    napi_build::setup();
}

fn apply_vst3_platform_config(
    build: &mut cc::Build,
    sdk_root: &std::path::Path,
    bridge_root: &std::path::Path,
) {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    match target_os.as_str() {
        "windows" => {
            build.define("SMTG_OS_WINDOWS", "1");
            build.file(sdk_root.join("public.sdk/source/vst/hosting/module_win32.cpp"));
            for source in &[
                "src/editorplatform/windows/editor_windows_api.cpp",
                "src/editorplatform/windows/editor_windows_common.cpp",
                "src/editorplatform/windows/editor_windows_create.cpp",
                "src/editorplatform/windows/editor_windows_rendering.cpp",
                "src/editorplatform/windows/editor_windows_titlebar.cpp",
                "src/editorplatform/windows/editor_windows_utils.cpp",
                "src/editorplatform/windows/editor_windows_windowproc.cpp",
            ] {
                build.file(bridge_root.join(source));
            }
            println!("cargo:rustc-link-lib=ole32");
            println!("cargo:rustc-link-lib=user32");
            println!("cargo:rustc-link-lib=gdi32");
            println!("cargo:rustc-link-lib=dwmapi");
            println!("cargo:rustc-link-lib=dwrite");
        }
        "macos" => {
            build.define("SMTG_OS_MACOS", "1");
            build.flag("-fobjc-arc");
            build.file(sdk_root.join("public.sdk/source/vst/hosting/module_mac.mm"));
            for source in &[
                "src/editorplatform/macos/editor_mac.mm",
                "src/editorplatform/macos/editor_mac_delegate.mm",
                "src/editorplatform/macos/editor_mac_helpers.mm",
            ] {
                build.file(bridge_root.join(source));
            }
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=AppKit");
        }
        "linux" => {
            build.define("SMTG_OS_LINUX", "1");
            build.file(sdk_root.join("public.sdk/source/vst/hosting/module_linux.cpp"));
            build.file(bridge_root.join("src/editor_linux.cpp"));

            let gtk4 = pkg_config::probe_library("gtk4").expect(
                "GTK4 not found — install libgtk-4-dev (Debian/Ubuntu) or gtk4-devel (Fedora)",
            );
            for path in &gtk4.include_paths {
                build.include(path);
            }
            for (key, val) in &gtk4.defines {
                build.define(key, val.as_deref());
            }

            println!("cargo:rustc-link-lib=dl");
            // editor_linux.cpp uses XGrabKey so Space reaches the host even when
            // an XEmbed plug-in child holds focus.
            println!("cargo:rustc-link-lib=X11");
        }
        _ => {}
    }
}
