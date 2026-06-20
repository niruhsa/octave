//! Phase 6 download-manager tests. The HTTP-streaming paths need a live
//! server, so these cover the cache-write + storage-accounting + delete-
//! prune logic that the manager delegates to and that runs regardless of
//! transport.

use music_app_lib::cache::model::{Album, AlbumArt, Artist, Track};
use music_app_lib::cache::repo;
use music_app_lib::db;

fn now() -> String {
    "2026-06-20T12:00:00.000Z".to_string()
}

async fn seed(pool: &sqlx::SqlitePool) {
    repo::upsert_artist(
        pool,
        &Artist {
            id: "a1".into(),
            name: "Artist".into(),
            sort_name: None,
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    repo::upsert_album(
        pool,
        &Album {
            id: "al1".into(),
            artist_id: "a1".into(),
            title: "Album".into(),
            release_year: Some(2024),
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    repo::upsert_album_art(
        pool,
        &AlbumArt {
            album_id: "al1".into(),
            local_cover_path: "/tmp/cover.jpg".into(),
            fetched_at: now(),
        },
    )
    .await
    .unwrap();
    for (id, sz) in [("t1", 1000_i64), ("t2", 2000)] {
        repo::upsert_track(
            pool,
            &Track {
                id: id.into(),
                album_id: "al1".into(),
                artist_id: "a1".into(),
                title: format!("Track {id}"),
                track_no: Some(1),
                disc_no: None,
                duration_ms: 60_000,
                codec: "flac".into(),
                bitrate_kbps: None,
                file_size: Some(sz),
                local_file_path: format!("/tmp/{id}.flac"),
                metadata_json: "{}".into(),
                downloaded_at: now(),
                updated_at: now(),
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn storage_usage_sums_file_sizes_and_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();
    seed(&pool).await;

    // 1000 + 2000 = 3000 bytes; 2 tracks; 1 cover.
    assert_eq!(repo::downloaded_bytes(&pool).await.unwrap(), 3000);
    assert_eq!(repo::count_downloaded_tracks(&pool).await.unwrap(), 2);
    assert_eq!(repo::downloaded_cover_count(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn settings_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    assert_eq!(repo::get_setting(&pool, "downloads_dir").await.unwrap(), None);
    repo::set_setting(&pool, "downloads_dir", "/srv/music").await.unwrap();
    assert_eq!(
        repo::get_setting(&pool, "downloads_dir").await.unwrap(),
        Some("/srv/music".into())
    );
    // Upsert overwrites.
    repo::set_setting(&pool, "downloads_dir", "/srv/other").await.unwrap();
    assert_eq!(
        repo::get_setting(&pool, "downloads_dir").await.unwrap(),
        Some("/srv/other".into())
    );
    repo::delete_setting(&pool, "downloads_dir").await.unwrap();
    assert_eq!(repo::get_setting(&pool, "downloads_dir").await.unwrap(), None);
}

#[tokio::test]
async fn count_tracks_for_album_drives_cover_prune() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();
    seed(&pool).await;

    // Two tracks under al1 → cover stays.
    assert_eq!(
        repo::count_downloaded_tracks_for_album(&pool, "al1").await.unwrap(),
        2
    );
    repo::delete_track(&pool, "t1").await.unwrap();
    assert_eq!(
        repo::count_downloaded_tracks_for_album(&pool, "al1").await.unwrap(),
        1
    );
    // Still has a track → cover + album row remain.
    assert!(repo::get_album_art(&pool, "al1").await.unwrap().is_some());
    assert!(repo::get_album(&pool, "al1").await.unwrap().is_some());

    repo::delete_track(&pool, "t2").await.unwrap();
    // Now empty → the manager's delete_track would prune cover + album.
    assert_eq!(
        repo::count_downloaded_tracks_for_album(&pool, "al1").await.unwrap(),
        0
    );
}
