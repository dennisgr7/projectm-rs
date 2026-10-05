use std::env;
use std::path::PathBuf;

mod build_bindgen;
use crate::build_bindgen::bindgen;

// Functions to determine feature flags
fn enable_playlist() -> &'static str {
    if cfg!(feature = "playlist") {
        "ON"
    } else {
        "OFF"
    }
}

// Are we linking to shared or static libraries?
fn build_shared_libs_flag() -> &'static str {
    if cfg!(feature = "static") {
        "OFF" // Disable shared libs to enable static linking
    } else {
        "ON" // Enable shared libs
    }
}

fn main() {
    // Path to the projectM source code
    let projectm_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("libprojectM");

    // Verify the existence of the libprojectM directory
    if !projectm_path.exists() {
        println!("cargo:warning=The libprojectM source code is missing.");
        println!(
            "cargo:warning=If you are building from a git clone, please run 'git submodule update --init --recursive'."
        );
        println!("cargo:warning=If you downloaded this crate from crates.io, please ensure that the crate was packaged correctly.");
        std::process::exit(1);
    }

    // Determine feature flags
    let enable_playlist_flag = enable_playlist();
    let build_shared_libs = build_shared_libs_flag();

    let dst;

    // Platform-specific CMake configurations
    if cfg!(target_os = "windows") {
        // Ensure VCPKG installation root is set
        let vcpkg_root = match env::var("VCPKG_INSTALLATION_ROOT") {
            Ok(val) => val,
            Err(_) => {
                println!("cargo:warning=VCPKG_INSTALLATION_ROOT is not set. Please set it to your VCPKG installation directory.");
                std::process::exit(1);
            }
        };

        let vcpkg_root = PathBuf::from(vcpkg_root);
        let vcpkg_toolchain = vcpkg_root
            .join("scripts")
            .join("buildsystems")
            .join("vcpkg.cmake");

        if !vcpkg_toolchain.exists() {
            println!(
                "cargo:warning=The vcpkg toolchain file was not found at: {}",
                vcpkg_toolchain.display()
            );
            std::process::exit(1);
        }

        // Set VCPKG_ROOT for CMake
        env::set_var("VCPKG_ROOT", &vcpkg_root);

        // Match libprojectM and its vcpkg dependencies to Rust's C runtime.
        // Release binaries can opt into `crt-static` and remain one file;
        // ordinary consumers retain the dynamic runtime they had before.
        let static_crt = env::var("CARGO_CFG_TARGET_FEATURE")
            .map(|features| features.split(',').any(|feature| feature == "crt-static"))
            .unwrap_or(false);
        // The target being built for, not the machine building it, so an
        // arm64 build gets arm64 dependencies from vcpkg.
        let vcpkg_arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("aarch64") => "arm64",
            Ok("x86") => "x86",
            _ => "x64",
        };
        let vcpkg_triplet = if static_crt {
            format!("{vcpkg_arch}-windows-static")
        } else {
            format!("{vcpkg_arch}-windows-static-md")
        };
        let vcpkg_triplet = vcpkg_triplet.as_str();
        let msvc_runtime = if static_crt {
            "MultiThreaded$<$<CONFIG:Debug>:Debug>"
        } else {
            "MultiThreaded$<$<CONFIG:Debug>:Debug>DLL"
        };
        let vcpkg_installed = vcpkg_root.join("installed").join(vcpkg_triplet);
        let vcpkg_installed_str = vcpkg_installed.to_str().unwrap();

        // Define projectM_Eval_DIR and store in a variable
        let projectm_eval_dir = projectm_path.join("vendor").join("projectm-eval");
        let projectm_eval_dir_str = projectm_eval_dir.to_str().unwrap();

        // Convert vcpkg_toolchain to string
        let vcpkg_toolchain_str = vcpkg_toolchain.to_str().unwrap();

        // Configure and build libprojectM using CMake for Windows
        let mut cmake_config = cmake::Config::new(&projectm_path);
        // Rust links the release C runtime on MSVC whatever profile it is
        // building, and a libprojectM built Debug wants the debug one.
        // The two disagree about `_CrtDbgReport` and about iterator
        // debugging, and the link fails on a wall of unresolved symbols.
        // Build it optimised there, which is what a visualiser wants
        // anyway: a debug libprojectM is too slow to watch.
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            cmake_config.profile("Release");
        }
        // Respect an explicit CMAKE_GENERATOR from the environment; the
        // Visual Studio 17 generator fails on machines that carry another
        // Visual Studio (GitHub's windows-latest images ship VS 2026).
        if std::env::var_os("CMAKE_GENERATOR").is_none() {
            cmake_config.generator("Visual Studio 17 2022");
        }
        cmake_config
            .define("CMAKE_TOOLCHAIN_FILE", vcpkg_toolchain_str)
            .define("VCPKG_TARGET_TRIPLET", vcpkg_triplet)
            .define("CMAKE_MSVC_RUNTIME_LIBRARY", msvc_runtime)
            .define("ENABLE_PLAYLIST", enable_playlist_flag)
            .define("projectM_Eval_DIR", projectm_eval_dir_str)
            .define("CMAKE_PREFIX_PATH", vcpkg_installed_str)
            .define("CMAKE_VERBOSE_MAKEFILE", "ON")
            .define("BUILD_TESTING", "OFF")
            .define("BUILD_EXAMPLES", "OFF")
            .define("BUILD_SHARED_LIBS", build_shared_libs); // static/dynamic

        dst = cmake_config.build();
    } else if cfg!(target_os = "emscripten") {
        // Configure and build libprojectM using CMake for Emscripten
        dst = cmake::Config::new(&projectm_path)
            .define("ENABLE_PLAYLIST", enable_playlist_flag)
            .define("BUILD_TESTING", "OFF")
            .define("BUILD_EXAMPLES", "OFF")
            .define("ENABLE_EMSCRIPTEN", "ON")
            .define("BUILD_SHARED_LIBS", build_shared_libs) // static/dynamic
            .build();
    } else {
        // Configure and build libprojectM using CMake for other platforms (Linux, macOS)
        dst = cmake::Config::new(&projectm_path)
            .define("ENABLE_PLAYLIST", enable_playlist_flag)
            .define("BUILD_TESTING", "OFF")
            .define("BUILD_EXAMPLES", "OFF")
            .define("BUILD_SHARED_LIBS", build_shared_libs) // static/dynamic
            .build();
    }

    // Specify the library search path
    println!("cargo:rustc-link-search=native={}/lib", dst.display());

    // Determine the build profile (release or debug)
    let profile = env::var("PROFILE").unwrap_or_else(|_| "release".to_string());

    // Platform and feature-specific library linking
    if cfg!(target_os = "windows") || cfg!(target_os = "emscripten") {
        // Where the library lands and whether it carries the debug postfix
        // both depend on the CMake generator; find what was actually made,
        // point the linker at its folder, and link it by the name it has.
        fn find_libs(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>, depth: usize) {
            if depth > 6 {
                return;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    find_libs(&path, found, depth + 1);
                } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let stem = name.trim_start_matches("lib");
                    if stem.starts_with("projectM-4")
                        && (name.ends_with(".lib") || name.ends_with(".a"))
                    {
                        found.push(path.clone());
                    }
                }
            }
        }
        let mut libs = Vec::new();
        find_libs(&dst, &mut libs, 0);
        // The target being built for, not the machine building it: a
        // cross build from an MSVC host still links the GNU way.
        let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
        let kind = if cfg!(feature = "static") {
            "static"
        } else {
            "dylib"
        };
        let pick = |base: &str| -> Option<String> {
            let candidates: Vec<&std::path::PathBuf> = libs
                .iter()
                .filter(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(|stem| {
                            let stem = stem.trim_start_matches("lib");
                            stem == base || stem == format!("{base}d")
                        })
                        .unwrap_or(false)
                })
                .collect();
            let debug_first = profile != "release";
            let best = candidates
                .iter()
                .max_by_key(|path| {
                    let is_debug = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| stem.ends_with('d'));
                    is_debug == debug_first
                })
                .or(candidates.first())?;
            if let Some(parent) = best.parent() {
                println!("cargo:rustc-link-search=native={}", parent.display());
            }
            best.file_stem().and_then(|stem| stem.to_str()).map(|stem| {
                // MSVC links a library by the name of its file, `lib`
                // and all: what CMake wrote as `libprojectM-4d.lib`
                // has to be asked for as `libprojectM-4d`. Every
                // other linker puts the `lib` back itself, so there
                // it comes off. Stripping it everywhere sent the
                // Windows linker looking for a file that was never
                // made, right next to the one that was.
                if msvc {
                    stem.to_string()
                } else {
                    stem.trim_start_matches("lib").to_string()
                }
            })
        };
        match pick("projectM-4") {
            Some(name) => println!("cargo:rustc-link-lib={kind}={name}"),
            None => {
                // Fail loudly with the layout: a quiet fallback just moves
                // the error to the linker, where the listing is invisible.
                let mut listing = String::new();
                fn walk(dir: &std::path::Path, out: &mut String, depth: usize) {
                    if depth > 4 {
                        return;
                    }
                    let Ok(entries) = std::fs::read_dir(dir) else {
                        return;
                    };
                    for entry in entries.flatten() {
                        let path = entry.path();
                        out.push_str(&format!("{}{}\n", "  ".repeat(depth), path.display()));
                        if path.is_dir() {
                            walk(&path, out, depth + 1);
                        }
                    }
                }
                walk(&dst, &mut listing, 0);
                panic!(
                    "no projectM-4 library found under {}; the install laid out:\n{listing}",
                    dst.display()
                );
            }
        }
        if cfg!(feature = "playlist") {
            if let Some(name) = pick("projectM-4-playlist") {
                println!("cargo:rustc-link-lib={kind}={name}");
            }
        }
    } else {
        // For other platforms (Linux, macOS)
        if cfg!(feature = "static") {
            if profile == "release" {
                println!("cargo:rustc-link-lib=static=projectM-4");
                if cfg!(feature = "playlist") {
                    println!("cargo:rustc-link-lib=static=projectM-4-playlist");
                }
            } else {
                println!("cargo:rustc-link-lib=static=projectM-4d");
                if cfg!(feature = "playlist") {
                    println!("cargo:rustc-link-lib=static=projectM-4-playlistd");
                }
            }
        } else {
            if profile == "release" {
                println!("cargo:rustc-link-lib=dylib=projectM-4");
                if cfg!(feature = "playlist") {
                    println!("cargo:rustc-link-lib=dylib=projectM-4-playlist");
                }
            } else {
                println!("cargo:rustc-link-lib=dylib=projectM-4d");
                if cfg!(feature = "playlist") {
                    println!("cargo:rustc-link-lib=dylib=projectM-4-playlistd");
                }
            }
        }
    }

    // Platform-specific link flags for C++ and OpenGL
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=framework=OpenGL");
    }
    #[cfg(target_os = "linux")]
    {
        // On Linux, link stdc++ and GL.
        println!("cargo:rustc-link-lib=stdc++");
        println!("cargo:rustc-link-lib=GL");
        println!("cargo:rustc-link-lib=gomp");
    }
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rustc-link-lib=opengl32");
    }
    #[cfg(target_os = "emscripten")]
    {
        // Emscripten typically handles GL calls differently, so you might skip or rely on the
        // emscripten compiler for linking.
    }

    // Generate Rust bindings using bindgen
    bindgen();
}
