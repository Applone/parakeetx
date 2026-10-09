use std::{env, path::PathBuf, process::Command};

fn checked(command: &mut Command) -> String {
    let output = command.output().expect("Cannot run the macOS build tools; install Xcode Command Line Tools");
    assert!(output.status.success(), "macOS capture bridge build failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("Build tool output is not UTF-8").trim().to_owned()
}

fn main() {
    println!("cargo:rerun-if-changed=native/screencapturekit.m");
    println!("cargo:rerun-if-changed=native/screencapturekit.h");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Target OS is missing");
    if target_os == "linux" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
    if target_os != "macos" { return; }
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("Unsupported macOS architecture"),
    };
    let sdk = checked(Command::new("xcrun").args(["--sdk", "macosx", "--show-sdk-path"]));
    let clang = checked(Command::new("xcrun").args(["--find", "clang"]));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is missing"));
    let object = out.join("screencapturekit.o");
    checked(Command::new(clang).args([
        "-c", "native/screencapturekit.m", "-o",
    ]).arg(&object).args([
        "-arch", arch, "-isysroot", &sdk, "-mmacosx-version-min=13.0",
        "-fobjc-arc", "-fblocks", "-O2", "-Wall", "-Wextra", "-Werror",
    ]));
    let archive = out.join("libparakeetx_capture.a");
    checked(Command::new("xcrun").arg("ar").arg("crs").arg(archive).arg(object));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=parakeetx_capture");
    println!("cargo:rustc-link-lib=objc");
    for framework in ["ScreenCaptureKit", "Foundation", "CoreMedia", "CoreAudio", "CoreGraphics"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
