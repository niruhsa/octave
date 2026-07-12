//! End-to-end exercise of the offline cache against a real SQLite file.
//!
//! Verifies the Phase 1 "Done when" criterion:
//!   *the app can persist + read downloaded-item metadata and cover paths
//!    offline.*
//!
//! Uses a tempdir DB so each run starts clean and nothing leaks into the
//! actual app-data dir.

use octave_lib::cache::model::{
    Album, AlbumArt, Artist, Playlist, PlaylistTrack, SyncState, Track,
};
use octave_lib::cache::repo;
use octave_lib::db;

fn now() -> String {
    "2026-06-19T12:00:00.000Z".to_string()
}

#[tokio::test]
async fn cache_roundtrip_persists_downloaded_items() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("cache.sqlite");
    let pool = db::open(&db_path).await.expect("open cache db");

    // --- artist + album + cover + track ---------------------------------

    let artist = Artist {
        id: "11111111-1111-1111-1111-111111111111".into(),
        name: "Boards of Canada".into(),
        sort_name: Some("Boards of Canada".into()),
        storage_bytes: 0,
        updated_at: now(),
    };
    repo::upsert_artist(&pool, &artist).await.unwrap();

    let album = Album {
        id: "22222222-2222-2222-2222-222222222222".into(),
        artist_id: artist.id.clone(),
        title: "Music Has the Right to Children".into(),
        release_year: Some(1998),
        storage_bytes: 0,
        updated_at: now(),
    };
    repo::upsert_album(&pool, &album).await.unwrap();

    let art = AlbumArt {
        album_id: album.id.clone(),
        local_cover_path: "/tmp/covers/mhtrtc.jpg".into(),
        fetched_at: now(),
    };
    repo::upsert_album_art(&pool, &art).await.unwrap();

    let track = Track {
        id: "33333333-3333-3333-3333-333333333333".into(),
        album_id: album.id.clone(),
        artist_id: artist.id.clone(),
        title: "Roygbiv".into(),
        track_no: Some(8),
        disc_no: Some(1),
        duration_ms: 144_000,
        codec: "flac".into(),
        bitrate_kbps: Some(900),
        file_size: Some(15_000_000),
        sample_rate_hz: Some(44_100),
        bit_depth: Some(16),
        channels: Some(2),
        loudness_lufs: None,
        loudness_peak: None,
        album_loudness_lufs: None,
        local_file_path: "/tmp/tracks/roygbiv.flac".into(),
        metadata_json: "{}".into(),
        downloaded_at: now(),
        updated_at: now(),
    };
    repo::upsert_track(&pool, &track).await.unwrap();

    // --- reads ----------------------------------------------------------

    let fetched_artist = repo::get_artist(&pool, &artist.id).await.unwrap().unwrap();
    assert_eq!(fetched_artist.name, artist.name);

    let by_artist = repo::list_albums_by_artist(&pool, &artist.id)
        .await
        .unwrap();
    assert_eq!(by_artist.len(), 1);
    assert_eq!(by_artist[0].title, album.title);

    let cover = repo::get_album_art(&pool, &album.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cover.local_cover_path, art.local_cover_path);

    let by_album = repo::list_tracks_by_album(&pool, &album.id).await.unwrap();
    assert_eq!(by_album.len(), 1);
    assert_eq!(by_album[0].local_file_path, track.local_file_path);

    // --- upsert idempotency --------------------------------------------

    let mut updated = track.clone();
    updated.title = "Roygbiv (alt mix)".into();
    repo::upsert_track(&pool, &updated).await.unwrap();
    let again = repo::get_track(&pool, &track.id).await.unwrap().unwrap();
    assert_eq!(again.title, "Roygbiv (alt mix)");
    assert_eq!(
        repo::list_tracks_by_album(&pool, &album.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // --- playlists + tracks --------------------------------------------

    let playlist = Playlist {
        id: "44444444-4444-4444-4444-444444444444".into(),
        owner_id: "55555555-5555-5555-5555-555555555555".into(),
        name: "ambient".into(),
        updated_at: now(),
    };
    repo::upsert_playlist(&pool, &playlist).await.unwrap();
    repo::replace_playlist_tracks(
        &pool,
        &playlist.id,
        &[PlaylistTrack {
            playlist_id: playlist.id.clone(),
            track_id: track.id.clone(),
            position: 0,
            added_at: now(),
        }],
    )
    .await
    .unwrap();
    let entries = repo::list_playlist_tracks(&pool, &playlist.id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track_id, track.id);

    // --- sync_state -----------------------------------------------------

    let sync = SyncState {
        entity_type: "track".into(),
        entity_id: track.id.clone(),
        server_version: Some("v7".into()),
        server_etag: Some("abc123".into()),
        last_synced_at: now(),
    };
    repo::upsert_sync_state(&pool, &sync).await.unwrap();
    let fetched_sync = repo::get_sync_state(&pool, "track", &track.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched_sync.server_etag.as_deref(), Some("abc123"));

    // --- cascade on album delete ---------------------------------------
    // Deleting an album cascades to its tracks and album_art (album_id
    // references). Deleting an artist would NOT cascade through tracks
    // because `tracks.artist_id` is ON DELETE RESTRICT (mirrors the
    // server's portable schema) — callers must clear tracks first.

    repo::delete_album(&pool, &album.id).await.unwrap();
    assert!(repo::get_album(&pool, &album.id).await.unwrap().is_none());
    assert!(repo::get_track(&pool, &track.id).await.unwrap().is_none());
    assert!(repo::get_album_art(&pool, &album.id)
        .await
        .unwrap()
        .is_none());

    // Artist now safe to delete (no tracks left).
    repo::delete_artist(&pool, &artist.id).await.unwrap();
    assert!(repo::get_artist(&pool, &artist.id).await.unwrap().is_none());
}

#[tokio::test]
async fn cache_open_is_idempotent_across_reopens() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("cache.sqlite");

    let pool1 = db::open(&db_path).await.expect("first open");
    repo::upsert_artist(
        &pool1,
        &Artist {
            id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            name: "Aphex Twin".into(),
            sort_name: None,
            storage_bytes: 0,
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    pool1.close().await;

    let pool2 = db::open(&db_path).await.expect("reopen");
    let listed = repo::list_artists(&pool2).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Aphex Twin");
}
