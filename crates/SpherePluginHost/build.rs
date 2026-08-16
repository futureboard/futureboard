extern crate napi_build;

fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir
                .join("vst3backend/plugin_host_version.rc")
                .display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir
                .join("../../packages/shared/app/windows/app.manifest")
                .display()
        );
        embed_resource::compile(
            manifest_dir.join("vst3backend/plugin_host.rc"),
            embed_resource::NONE,
        )
        .manifest_required()
        .unwrap();
    }
    let sdk_root = manifest_dir.join("../../external/vst3sdk");
    let clap_root = manifest_dir.join("../../external/clap");
    let clap_helpers_root = manifest_dir.join("../../external/clap-helpers");
    let backend_root = manifest_dir.join("vst3backend");
    println!(
        "cargo:rerun-if-changed={}",
        backend_root.join("src/vst3_scanner.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        backend_root.join("src/plugin_editor_window.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        backend_root.join("src/plugin_editor_stub.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        backend_root.join("src/host_ui_mac.mm").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        backend_root.join("src/au_host.mm").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        backend_root.join("include/sphere_au_host.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        backend_root
            .join("include/sphere_plugin_host_vst3.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("../SphereDirectAudioEngine/vst2bridge/include/sphere_vst2_abi.h")
            .display()
    );

    // Baseline x64 only — do not add /arch:AVX2 or target-cpu=native here.
    // Distributed plugin host must run on CPUs without AVX2.
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++20")
        .flag_if_supported("/Zc:char8_t-")
        .flag_if_supported("/EHsc")
        .include(backend_root.join("include"))
        .include(&sdk_root)
        .include(sdk_root.join("pluginterfaces"))
        .include(sdk_root.join("public.sdk/source"))
        .include(clap_root.join("include"))
        .include(clap_helpers_root.join("include"))
        // Shared VST2 ABI header — same file the runtime bridge uses, so the
        // scanner and the host can never disagree about the AEffect layout.
        .include(manifest_dir.join("../SphereDirectAudioEngine/vst2bridge/include"))
        .file(backend_root.join("src/vst3_scanner.cpp"))
        .file(sdk_root.join("pluginterfaces/base/coreiids.cpp"))
        .file(sdk_root.join("pluginterfaces/base/funknown.cpp"))
        .file(sdk_root.join("pluginterfaces/base/ustring.cpp"))
        .file(sdk_root.join("public.sdk/source/common/commonstringconvert.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/utility/stringconvert.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/vstinitiids.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/hosting/hostclasses.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/hosting/pluginterfacesupport.cpp"))
        .file(sdk_root.join("public.sdk/source/vst/hosting/module.cpp"));

    apply_vst3_platform_config(&mut build, &sdk_root, &backend_root);

    if target_os_for_au() == "macos" {
        build
            .file(backend_root.join("src/au_scanner.mm"))
            // AppKit application + event pump for the host process, without
            // which a plug-in editor window never appears (see the file header).
            .file(backend_root.join("src/host_ui_mac.mm"))
            // Audio Unit runtime: hosted in this process like the built-ins, so
            // there is no engine-side in-process path for AU.
            .file(backend_root.join("src/au_host.mm"))
            .flag("-fobjc-arc");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=AppKit");
    } else {
        build
            .file(backend_root.join("src/au_scanner_stub.cpp"))
            .file(backend_root.join("src/au_host_stub.cpp"));
    }

    build.compile("sphere_plugin_host_vst3");
    napi_build::setup();
}

fn target_os_for_au() -> String {
    std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default()
}

fn apply_vst3_platform_config(
    build: &mut cc::Build,
    sdk_root: &std::path::Path,
    backend_root: &std::path::Path,
) {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    match target_os.as_str() {
        "windows" => {
            build.define("SMTG_OS_WINDOWS", "1");
            build.define("FB_PLATFORM_WINDOWS", None);
            build
                // Windows-only native editor window host (HWND). Must never be
                // compiled on other platforms — see the guard at the top of the file.
                .file(backend_root.join("src/plugin_editor_window.cpp"))
                .file(sdk_root.join("public.sdk/source/vst/hosting/module_win32.cpp"));
            println!("cargo:rustc-link-lib=ole32");
            println!("cargo:rustc-link-lib=user32");
            println!("cargo:rustc-link-lib=gdi32");
            println!("cargo:rustc-link-lib=dwmapi");
        }
        "macos" => {
            build.define("SMTG_OS_MACOS", "1");
            build.define("FB_PLATFORM_MACOS", None);
            build
                // No native editor host on macOS yet — stub reports Unsupported.
                .file(backend_root.join("src/plugin_editor_stub.cpp"))
                .flag("-fobjc-arc")
                .file(sdk_root.join("public.sdk/source/vst/hosting/module_mac.mm"));
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        "linux" => {
            build.define("SMTG_OS_LINUX", "1");
            build.define("FB_PLATFORM_LINUX", None);
            build
                // No native editor host on Linux yet — stub reports Unsupported.
                // plugin_editor_window.cpp is Windows-only (uses HWND) and must
                // not be compiled here.
                .file(backend_root.join("src/plugin_editor_stub.cpp"))
                .file(sdk_root.join("public.sdk/source/vst/hosting/module_linux.cpp"));
            println!("cargo:rustc-link-lib=dl");
        }
        _ => {
            build.define("FB_PLATFORM_UNKNOWN", None);
            build.file(backend_root.join("src/plugin_editor_stub.cpp"));
        }
    }
}
