/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
use std::env;
use std::path::Path;

/*fn rerun_if_changed(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.to_str().unwrap());
}*/
fn link_search(path: &Path) {
    println!("cargo:rustc-link-search=native={}", path.to_str().unwrap());
}
fn link_lib(lib: &str) {
    if cfg!(feature = "static") {
        println!("cargo:rustc-link-lib=static={lib}");
    } else {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}

fn link_framework(framework: &str) {
    println!("cargo:rustc-link-lib=framework={framework}");
}

fn main() {
    let os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS was not set");
    if cfg!(feature = "static") {
        let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = package_root.join("../../..");

        let mut build = cmake::Config::new(workspace_root.join("vendor/openal-soft"));

        // Prevent any out dir re-use for Android. For some reason, this causes
        // weird linker errors when cross-compiling on macOS.
        let out_dir = Path::new(&env::var("OUT_DIR").unwrap()).join(&os);
        if os.eq_ignore_ascii_case("android") {
            let _ = std::fs::remove_dir_all(&out_dir);
        }
        build.out_dir(out_dir);

        build.define("LIBTYPE", "STATIC");

        // Don't build extras, we don't need them and they can encounter issues
        // when cross-compiling.
        build.define("ALSOFT_UTILS", "OFF");
        build.define("ALSOFT_NO_CONFIG_UTIL", "ON");
        build.define("ALSOFT_EXAMPLES", "OFF");

        // n72 fork (iPod touch 2G, Alpine): cross-built against the sysroot,
        // with ALSA as the only backend (loaded at run time, so nothing to
        // link). Alpine's own libopenal is built -Os with libstdc++'s
        // assertions on, which leaves a bounds-checked call per sample in
        // the mixer's and the resamplers' inner loops; this build takes the
        // profile's optimisation level (CMake's Release type would make it
        // -O3 whatever the profile says).
        let n72 = os.eq_ignore_ascii_case("linux")
            && env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "arm");
        if n72 {
            if let Ok(sysroot) = env::var("N72_SYSROOT") {
                build.define("CMAKE_SYSROOT", &sysroot);
                build.define("CMAKE_FIND_ROOT_PATH", &sysroot);
                build.define("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM", "NEVER");
                build.define("CMAKE_FIND_ROOT_PATH_MODE_LIBRARY", "ONLY");
                build.define("CMAKE_FIND_ROOT_PATH_MODE_INCLUDE", "ONLY");
                build.define("CMAKE_FIND_ROOT_PATH_MODE_PACKAGE", "ONLY");
            }
            // (the host's ar, which CMake would pick, cannot index ELF objects)
            if let Ok(ar) = env::var("AR_arm_unknown_linux_musleabihf") {
                build.define("CMAKE_RANLIB", format!("{}-ranlib", ar.trim_end_matches("-ar")));
                build.define("CMAKE_AR", ar);
            }
            // (the vendored release asks for a CMake older than current ones accept)
            build.define("CMAKE_POLICY_VERSION_MINIMUM", "3.5");
            build.define("ALSOFT_REQUIRE_ALSA", "ON");
            build.define("ALSOFT_DLOPEN", "ON");
            for backend in ["OSS", "PIPEWIRE", "SOLARIS", "SNDIO", "PORTAUDIO", "PULSEAUDIO", "JACK", "SDL2", "WAVE"] {
                build.define(&format!("ALSOFT_BACKEND_{backend}"), "OFF");
            }
            build.define("CMAKE_DISABLE_FIND_PACKAGE_SDL2", "ON");
            // (it relies on headers that newer libstdc++ no longer pulls in)
            build.cxxflag("-include cstdint");
            build.define("ALSOFT_RTKIT", "OFF");
            build.define("ALSOFT_CPUEXT_NEON", "OFF");
            build.define("ALSOFT_EMBED_HRTF_DATA", "OFF");
            build.define("ALSOFT_INSTALL_CONFIG", "OFF");
            build.define("ALSOFT_INSTALL_HRTF_DATA", "OFF");
            build.define("ALSOFT_INSTALL_AMBDEC_PRESETS", "OFF");
            build.define("ALSOFT_UPDATE_BUILD_VERSION", "OFF");
            if let Ok(level) = env::var("OPT_LEVEL") {
                let flags = format!("-O{level} -DNDEBUG");
                build.define("CMAKE_C_FLAGS_RELEASE", &flags);
                build.define("CMAKE_CXX_FLAGS_RELEASE", &flags);
            }
        }

        let openal_soft_out = build.build();

        link_search(&openal_soft_out.join("lib"));
        // some Linux systems
        link_search(&openal_soft_out.join("lib64"));

        // Some dependencies of OpenAL Soft.
        if os.eq_ignore_ascii_case("linux") && !n72 {
            // OpenAL on Linux depends on sndio, needs to be dynamically linked
            println!("cargo:rustc-link-lib=dylib=sndio");
        }
        if n72 {
            // (C++; libstdc++ comes from Alpine like the rest)
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
        if os.eq_ignore_ascii_case("android") {
            println!("cargo:rustc-link-lib=dylib=OpenSLES");
        }
        if os.eq_ignore_ascii_case("macos") {
            link_framework("AudioToolbox");
            link_framework("CoreAudio");
            link_framework("CoreFoundation");
        }
    }

    // see also src/audio/openal.rs
    link_lib(if os.eq_ignore_ascii_case("windows") {
        "OpenAL32"
    } else {
        "openal"
    });
    // rerun-if-changed seems to not work if pointed to a directory :(
    //rerun_if_changed(&workspace_root.join("vendor/openal-soft"));
}
