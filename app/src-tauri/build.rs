//! Build script: run Tauri's codegen and compile the server's `.proto` files
//! into Rust client stubs.
//!
//! The protos live in `../../server/proto/` — single source of truth shared
//! between server and client. We compile the *client* side only (the server
//! crate compiles the server side).

use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();

    // Generated gRPC clients are part of every clean app build. Use the same
    // vendored compiler as the server so Windows/macOS/Android builders do not
    // depend on a separately installed `protoc` executable.
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc unavailable");
    // SAFETY: build scripts run before this process starts any worker threads.
    unsafe { std::env::set_var("PROTOC", protoc) };

    // `gen/android` is regeneration-sensitive. Fail an Android build loudly
    // if `tauri android init` has dropped either half of the native EQ route
    // bridge; otherwise automatic switching would silently degrade to manual.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        for path in [
            "gen/android/app/src/main/java/dev/niruhsa/octave/AudioRoutePlugin.kt",
            "gen/android/app/src/main/java/dev/niruhsa/octave/AudioRouteBridge.kt",
        ] {
            println!("cargo:rerun-if-changed={path}");
            assert!(
                Path::new(path).is_file(),
                "missing Android EQ route bridge: {path}"
            );
        }
    }

    let proto_dir = PathBuf::from("../../server/proto");
    println!("cargo:rerun-if-changed=../../server/proto");

    if !proto_dir.exists() {
        // Allows out-of-tree builds (e.g. CI artifact tarball without the
        // server crate) to still compile — runtime gRPC just won't work.
        println!("cargo:warning=server/proto not found; skipping gRPC codegen");
        return;
    }

    let protos = collect_protos(&proto_dir);
    if protos.is_empty() {
        return;
    }

    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        .compile_protos(&protos, &[proto_dir])
        .expect("tonic-build failed");
}

fn collect_protos(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("read proto dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("proto"))
        .collect()
}
