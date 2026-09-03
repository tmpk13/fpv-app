// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Builds devourer, the C shim over it, and - on Android - libusb.
//!
//! Nothing is downloaded here. Both C dependencies are git submodules pinned
//! to exact commits, which is what makes the GPL obligation this project
//! carries satisfiable: the complete corresponding source of a binary built
//! from this tree is this tree.
//!
//! The platform split is only in where libusb comes from. A desktop has one
//! and devourer's own CMake finds it through pkg-config; Android has none, so
//! the vendored copy is compiled here and a pkg-config file written pointing
//! at it, which is the smallest change that leaves devourer's build untouched.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=shim/devourer_shim.cpp");
    println!("cargo:rerun-if-changed=shim/devourer_shim.h");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_FEATURE_RADIO").is_err() {
        // Built without the radio: no C dependencies at all, and the result
        // is MIT rather than GPL-2.0.
        return;
    }

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let devourer = root.join("third_party/devourer");
    let libusb = root.join("third_party/libusb");
    require_submodule(&devourer, "third_party/devourer");

    let android = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android");
    if android {
        export_android_toolchain();
    }
    let libusb_include = if android {
        require_submodule(&libusb, "third_party/libusb");
        Some(build_libusb(&libusb))
    } else {
        None
    };

    let devourer_lib = build_devourer(&devourer, libusb_include.as_deref());
    build_shim(&root, &devourer, libusb_include.as_deref());

    println!("cargo:rustc-link-search=native={}", devourer_lib.display());
    println!("cargo:rustc-link-lib=static=devourer");

    if android {
        if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("arm") {
            println!(
                "cargo:rustc-link-arg=-Wl,--version-script={}",
                libc_version_node().display()
            );
        }
        println!("cargo:rustc-link-lib=static=usb-1.0");
        println!("cargo:rustc-link-lib=log");
        // The NDK's static C++ runtime. Rust's linker invocation does not
        // pass the sysroot's own library directory, so the path has to be
        // named as well as the libraries: without it the link fails on
        // c++_static, which reads as a missing dependency rather than a
        // missing search path.
        println!(
            "cargo:rustc-link-search=native={}",
            android_sysroot_lib().display()
        );
        println!("cargo:rustc-link-lib=static=c++_static");
        println!("cargo:rustc-link-lib=static=c++abi");
    } else {
        let found = pkg_config::Config::new()
            .probe("libusb-1.0")
            .expect("libusb-1.0 development files (install libusb-1.0-0-dev)");
        for path in &found.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
        for lib in &found.libs {
            println!("cargo:rustc-link-lib={lib}");
        }
        println!("cargo:rustc-link-lib=stdc++");
    }
}

/// Fail with something actionable when a submodule was never checked out.
fn require_submodule(path: &Path, name: &str) {
    if path.join("CMakeLists.txt").exists() || path.join("libusb").exists() {
        return;
    }
    panic!(
        "{name} is empty. It is a git submodule pinned to the revision this \
         build is tested against:\n\n    git submodule update --init {name}\n\n\
         Or build without the radio, which needs neither:\n\n    \
         cargo build --no-default-features\n"
    );
}

/// Compile devourer's static library for the target.
fn build_devourer(source: &Path, libusb_include: Option<&Path>) -> PathBuf {
    let mut cmake = cmake::Config::new(source);
    cmake
        .define("CMAKE_BUILD_TYPE", "Release")
        .build_target("devourer");

    for (option, on) in chip_options() {
        cmake.define(option, if on { "ON" } else { "OFF" });
    }

    if let Some(include) = libusb_include {
        // devourer asks pkg-config for libusb. On a target with no pkg-config
        // metadata, write some: it is five lines, and the alternative is
        // patching a submodule the licence wants distributable as published.
        let pc_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets this")).join("pkgconfig");
        std::fs::create_dir_all(&pc_dir).expect("creating the pkg-config directory");
        std::fs::write(
            pc_dir.join("libusb-1.0.pc"),
            format!(
                "Name: libusb-1.0\n\
                 Description: vendored, for a target with no system libusb\n\
                 Version: 1.0.27\n\
                 Libs: -lusb-1.0\n\
                 Cflags: -I{}\n",
                include.display()
            ),
        )
        .expect("writing the pkg-config file");

        cmake
            .env("PKG_CONFIG_LIBDIR", &pc_dir)
            .env("PKG_CONFIG_PATH", &pc_dir)
            // Without this a cross pkg-config prefixes every path with the
            // sysroot, and the include directory silently disappears.
            .env("PKG_CONFIG_SYSROOT_DIR", "");

        configure_android(&mut cmake);
    }

    // `build_target` leaves the artifact in the build tree rather than
    // installing it.
    cmake.build().join("build")
}

/// The chip backends compiled in.
///
/// Every backend brings its own firmware blob and PHY tables, and turning
/// them all on multiplies the library's size by four for hardware nobody here
/// owns. The default set is what FPV actually flies: the RTL8812AU family,
/// the 4T4R RTL8814AU, and the RTL8812EU whose product id an RTL8812AU-VS
/// shares. The `radio-all-chips` feature turns the rest back on.
fn chip_options() -> Vec<(&'static str, bool)> {
    let all = env::var("CARGO_FEATURE_RADIO_ALL_CHIPS").is_ok();
    vec![
        ("DEVOURER_JAGUAR1", true),
        ("DEVOURER_8814", true),
        ("DEVOURER_JAGUAR3_8822E", true),
        ("DEVOURER_JAGUAR2_8822B", all),
        ("DEVOURER_JAGUAR2_8821C", all),
        ("DEVOURER_JAGUAR3_8822C", all),
        ("DEVOURER_8733B", all),
        ("DEVOURER_KESTREL_8852B", all),
        ("DEVOURER_KESTREL_8852C", all),
        ("DEVOURER_PCIE", false),
    ]
}

/// Point CMake at the NDK toolchain for the target Cargo is building.
fn configure_android(cmake: &mut cmake::Config) {
    let ndk = android_ndk();
    let abi = android_target().abi;

    cmake
        .define(
            "CMAKE_TOOLCHAIN_FILE",
            ndk.join("build/cmake/android.toolchain.cmake"),
        )
        .define("ANDROID_ABI", abi)
        .define("ANDROID_PLATFORM", format!("android-{}", MIN_SDK))
        // Static, because the APK ships one native library and nothing else
        // would carry a shared C++ runtime into it.
        .define("ANDROID_STL", "c++_static");
}

/// Compile the vendored libusb for Android.
///
/// The upstream tree has no CMake build, and its Android support is an
/// ndk-build makefile naming ten source files and a config header. Compiling
/// those ten directly is less machinery than driving ndk-build from here, and
/// it uses the compiler Cargo has already chosen for the target.
fn build_libusb(source: &Path) -> PathBuf {
    let src = source.join("libusb");
    let mut build = cc::Build::new();
    build
        .include(&src)
        .include(src.join("os"))
        // The Android build config upstream ships, unmodified.
        .include(source.join("android"))
        .flag_if_supported("-fvisibility=hidden")
        .warnings(false);

    for file in [
        "core.c",
        "descriptor.c",
        "hotplug.c",
        "io.c",
        "sync.c",
        "strerror.c",
        "os/linux_usbfs.c",
        "os/events_posix.c",
        "os/threads_posix.c",
        "os/linux_netlink.c",
    ] {
        build.file(src.join(file));
    }

    build.compile("usb-1.0");
    src
}

/// Build the shim that gives devourer's C++ interface a C surface.
fn build_shim(root: &Path, devourer: &Path, libusb_include: Option<&Path>) {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        // `cc` would name the C++ runtime itself, and on Android its choice
        // is the shared one - which an APK built this way does not ship, so
        // the library loads on no device at all. The static runtime is named
        // explicitly in main() instead.
        .cpp_link_stdlib(None)
        .std("c++20")
        .file(root.join("shim/devourer_shim.cpp"))
        .include(root.join("shim"))
        .include(devourer.join("src"))
        .warnings(false);

    match libusb_include {
        Some(include) => {
            build.include(include);
        }
        None => {
            let found = pkg_config::Config::new()
                .probe("libusb-1.0")
                .expect("libusb-1.0 development files (install libusb-1.0-0-dev)");
            for path in &found.include_paths {
                build.include(path);
            }
        }
    }

    build.compile("devourer_shim");
}

/// Name the NDK's compiler in the environment, for everything below.
///
/// `cc`'s default guess is `<triple>-clang`, and the NDK has not shipped one
/// of those since r19: the wrappers carry the API level in their name, which
/// is what selects the right sysroot and unified headers. Without this the
/// build fails with "failed to find tool" naming a compiler that has not
/// existed for years, which reads as a broken NDK rather than a wrong guess.
///
/// Set in the environment rather than on each `cc::Build`, because the `cmake`
/// crate makes its own probe and would warn about the same missing tool.
/// These affect this build script and the tools it spawns, nothing else.
fn export_android_toolchain() {
    let bin = android_ndk()
        .join("toolchains/llvm/prebuilt")
        .join(ndk_host_tag())
        .join("bin");
    let tool = android_target().tool;
    let target = env::var("TARGET").expect("cargo sets this");

    let cc = bin.join(format!("{tool}{MIN_SDK}-clang"));
    let cxx = bin.join(format!("{tool}{MIN_SDK}-clang++"));
    // One llvm-ar for every target, rather than the per-triple `ar` that went
    // away at the same time as the per-triple clang.
    let ar = bin.join("llvm-ar");

    if !cc.exists() {
        panic!(
            "no {} in the NDK at {}. Set ANDROID_NDK_HOME to an NDK that has \
             one, or lower the API level the build targets.",
            cc.display(),
            android_ndk().display()
        );
    }

    // Both spellings: `cc` looks for the triple as written and with its
    // dashes turned into underscores, and which one it finds first is not
    // something to depend on.
    for name in [target.clone(), target.replace('-', "_")] {
        env::set_var(format!("CC_{name}"), &cc);
        env::set_var(format!("CXX_{name}"), &cxx);
        env::set_var(format!("AR_{name}"), &ar);
    }
}

/// Declare the `LIBC_N` symbol version, and return the script that does.
///
/// 32-bit ARM only, and only once C code is in the link. The NDK's libc stub
/// exports `__aeabi_memcpy` and its family under the version `LIBC_N`;
/// Rust's prebuilt `compiler_builtins` defines the same symbols weakly. When
/// C objects pull them in, the linker binds the compiler_builtins copies and
/// carries the version across with them - and then rejects the result,
/// because `LIBC_N` is not declared in the version script rustc generates.
///
/// Declaring an empty `LIBC_N` node satisfies that check without changing
/// which definition wins or what is exported. It is one line, and the
/// alternatives are patching a prebuilt standard library or dropping the ABI.
fn libc_version_node() -> PathBuf {
    let path =
        PathBuf::from(env::var("OUT_DIR").expect("cargo sets this")).join("libc-version.map");
    std::fs::write(&path, "LIBC_N { };\n").expect("writing the version script");
    path
}

/// The NDK sysroot directory holding this target's libraries.
fn android_sysroot_lib() -> PathBuf {
    android_ndk()
        .join("toolchains/llvm/prebuilt")
        .join(ndk_host_tag())
        .join("sysroot/usr/lib")
        .join(android_target().sysroot)
}

/// The three names one Android target goes by.
///
/// They are not the same, and 32-bit ARM is where that bites: Rust calls it
/// `armv7-linux-androideabi`, the compiler wrapper is
/// `armv7a-linux-androideabi21-clang`, the sysroot directory is
/// `arm-linux-androideabi`, and the ABI is `armeabi-v7a`. Using any one of
/// them where another belongs fails at a different stage, so they are named
/// once here rather than derived at each use.
struct AndroidNames {
    /// Prefix of the clang wrapper in the NDK's bin directory.
    tool: &'static str,
    /// Directory under the sysroot holding this target's libraries.
    sysroot: &'static str,
    /// What the APK and CMake call it.
    abi: &'static str,
}

fn android_target() -> AndroidNames {
    let target = env::var("TARGET").expect("cargo sets this");
    match target.as_str() {
        "aarch64-linux-android" => AndroidNames {
            tool: "aarch64-linux-android",
            sysroot: "aarch64-linux-android",
            abi: "arm64-v8a",
        },
        "armv7-linux-androideabi" => AndroidNames {
            tool: "armv7a-linux-androideabi",
            sysroot: "arm-linux-androideabi",
            abi: "armeabi-v7a",
        },
        "x86_64-linux-android" => AndroidNames {
            tool: "x86_64-linux-android",
            sysroot: "x86_64-linux-android",
            abi: "x86_64",
        },
        "i686-linux-android" => AndroidNames {
            tool: "i686-linux-android",
            sysroot: "i686-linux-android",
            abi: "x86",
        },
        other => panic!("no Android toolchain known for {other}"),
    }
}

/// The NDK's directory for the machine doing the building.
fn ndk_host_tag() -> &'static str {
    match env::var("HOST").expect("cargo sets this").as_str() {
        host if host.contains("apple-darwin") => "darwin-x86_64",
        host if host.contains("windows") => "windows-x86_64",
        _ => "linux-x86_64",
    }
}

/// The NDK an Android build uses.
fn android_ndk() -> PathBuf {
    for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"] {
        println!("cargo:rerun-if-env-changed={var}");
        if let Ok(path) = env::var(var) {
            return PathBuf::from(path);
        }
    }
    panic!("set ANDROID_NDK_HOME to the NDK an Android build should use");
}

/// The API level the native code targets.
///
/// Must match the manifest's `min_sdk_version`: this is what devourer and
/// libusb are compiled against, and a native library built for a newer API
/// than the manifest claims fails to load on exactly the devices the manifest
/// promised it would run on.
///
/// `AMediaCodec`'s buffer API is API 21 and the USB host API is much older,
/// so nothing here needs more.
const MIN_SDK: u32 = 21;
