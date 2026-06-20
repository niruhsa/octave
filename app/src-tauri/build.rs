//! Build script: run Tauri's codegen and compile the server's `.proto` files
//! into Rust client stubs.
//!
//! The protos live in `../../server/proto/` — single source of truth shared
//! between server and client. We compile the *client* side only (the server
//! crate compiles the server side).

use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();

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
