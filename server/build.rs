//! Compile `proto/*.proto` into Rust via `tonic-build`.

use std::{fs, path::PathBuf};

fn main() {
    // Keep clean builds independent of a system-wide protobuf installation.
    // In Rust 2024, mutating the build process environment is explicitly
    // unsafe; this happens before any worker threads are spawned.
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc unavailable");
    unsafe { std::env::set_var("PROTOC", protoc) };

    let proto_dir = PathBuf::from("proto");
    println!("cargo:rerun-if-changed=proto");

    if !proto_dir.exists() {
        return;
    }

    let protos: Vec<PathBuf> = fs::read_dir(&proto_dir)
        .expect("failed to read proto/ directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("proto"))
        .collect();

    if protos.is_empty() {
        return;
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&protos, &[proto_dir])
        .expect("tonic-build failed");
}
