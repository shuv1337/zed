#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

use std::{env, path::PathBuf, process::Command};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    println!("cargo:rerun-if-changed=src/bindings.h");
    println!("cargo:rerun-if-env-changed=SDKROOT");

    let sdk_path = match env::var("SDKROOT") {
        Ok(value) if !value.trim().is_empty() => value,
        _ if cfg!(target_os = "macos") => {
            let output = Command::new("xcrun")
                .args(["--sdk", "macosx", "--show-sdk-path"])
                .output()
                .expect("failed to run xcrun to locate the macOS SDK");
            let stdout =
                String::from_utf8(output.stdout).expect("xcrun output was not valid UTF-8");
            let sdk_path = stdout.trim();
            if sdk_path.is_empty() {
                panic!("xcrun returned an empty SDK path; set SDKROOT to a macOS SDK path");
            }
            sdk_path.to_string()
        }
        _ => {
            panic!("SDKROOT is not set; set SDKROOT to a macOS SDK path to build bindings");
        }
    };

    let framework_path = PathBuf::from(&sdk_path)
        .join("System")
        .join("Library")
        .join("Frameworks");
    let include_path = PathBuf::from(&sdk_path).join("usr").join("include");

    let bindings = bindgen::Builder::default()
        .header("src/bindings.h")
        .clang_arg("-isysroot")
        .clang_arg(&sdk_path)
        .clang_arg("-isystem")
        .clang_arg(include_path.to_string_lossy())
        .clang_arg("-F")
        .clang_arg(framework_path.to_string_lossy())
        .clang_arg("-iframework")
        .clang_arg(framework_path.to_string_lossy())
        .clang_arg("-xobjective-c")
        .allowlist_type("CMItemIndex")
        .allowlist_type("CMSampleTimingInfo")
        .allowlist_type("CMVideoCodecType")
        .allowlist_type("VTEncodeInfoFlags")
        .allowlist_function("CMTimeMake")
        .allowlist_var("kCVPixelFormatType_.*")
        .allowlist_var("kCVReturn.*")
        .allowlist_var("VTEncodeInfoFlags_.*")
        .allowlist_var("kCMVideoCodecType_.*")
        .allowlist_var("kCMTime.*")
        .allowlist_var("kCMSampleAttachmentKey_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .layout_tests(false)
        .generate()
        .expect("unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("could not write bindings");
}
