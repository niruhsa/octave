//! Tonic-generated client stubs.
//!
//! `tonic-build` writes one Rust module per proto `package`. Server's
//! `auth.proto` declares `package music.auth.v1`, so the generated module is
//! `music.auth.v1`. Re-export under stable Rust names.

#[allow(clippy::all, clippy::pedantic)]
pub mod auth {
    tonic::include_proto!("music.auth.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod library {
    tonic::include_proto!("music.library.v1");
}
