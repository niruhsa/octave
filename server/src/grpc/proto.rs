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

pub mod notification {
    tonic::include_proto!("music.notification.v1");
}

pub mod podcast {
    tonic::include_proto!("music.podcast.v1");
}

pub mod playhistory {
    tonic::include_proto!("music.playhistory.v1");
}

pub mod favorite {
    tonic::include_proto!("music.favorite.v1");
}

pub mod discover {
    tonic::include_proto!("music.discover.v1");
}

pub mod upload {
    tonic::include_proto!("music.upload.v1");
}
