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

#[allow(clippy::all, clippy::pedantic)]
pub mod playlist {
    tonic::include_proto!("music.playlist.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod upload {
    tonic::include_proto!("music.upload.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod notification {
    tonic::include_proto!("music.notification.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod podcast {
    tonic::include_proto!("music.podcast.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod playhistory {
    tonic::include_proto!("music.playhistory.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod favorite {
    tonic::include_proto!("music.favorite.v1");
}

#[allow(clippy::all, clippy::pedantic)]
pub mod discover {
    tonic::include_proto!("music.discover.v1");
}