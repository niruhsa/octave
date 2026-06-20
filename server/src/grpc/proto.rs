//! Generated proto modules (via `tonic-build` in `build.rs`).

pub mod auth {
    tonic::include_proto!("music.auth.v1");
}

pub mod library {
    tonic::include_proto!("music.library.v1");
}

pub mod playlist {
    tonic::include_proto!("music.playlist.v1");
}

pub mod upload {
    tonic::include_proto!("music.upload.v1");
}
