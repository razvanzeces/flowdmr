//! Resolve the native TETRA ACELP codec link path when the real codec is used.
//!
//! Mirrors FlowStation's own build.rs: `pkg-config --libs tetra-codec` provides
//! the `-L` search path; the `#[link(name = "tetra-codec")]` attribute in
//! `src/codec.rs` provides the actual link directive. When the `codec-stub`
//! feature is active (and `real-codec` is not), no native library is needed.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=TETRA_CODEC_LIB_DIR");

    let real_codec = std::env::var("CARGO_FEATURE_REAL_CODEC").is_ok();
    let stub = std::env::var("CARGO_FEATURE_CODEC_STUB").is_ok();

    if !real_codec || stub {
        return;
    }

    // 1) Explicit override: point straight at the dir that holds libtetra-codec.{so,a}.
    //    Use this when there is no tetra-codec.pc on the device:
    //      TETRA_CODEC_LIB_DIR=/usr/local/lib cargo build --release --features flowdmr
    if let Ok(dir) = std::env::var("TETRA_CODEC_LIB_DIR") {
        if !dir.trim().is_empty() {
            println!("cargo:rustc-link-search=native={}", dir.trim());
            return;
        }
    }

    // 2) Otherwise resolve the -L path via pkg-config (same as FlowStation's asterisk build).
    {
        if let Ok(output) = Command::new("pkg-config").args(["--libs", "tetra-codec"]).output() {
            if output.status.success() {
                let flags = String::from_utf8_lossy(&output.stdout);
                for flag in flags.split_whitespace() {
                    if let Some(path) = flag.strip_prefix("-L") {
                        println!("cargo:rustc-link-search=native={path}");
                    }
                }
            } else {
                println!(
                    "cargo:warning=pkg-config could not find tetra-codec; \
                     build with --no-default-features --features codec-stub for a codec-less dev build"
                );
            }
        }
    }
}
