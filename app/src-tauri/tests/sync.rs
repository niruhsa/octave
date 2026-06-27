//! Phase 5 sync-engine tests that exercise the SQLite outbox + reconcile
//! helpers against a real DB. The server-talking paths (`sync_now`) need a
//! live server, so these focus on the persistence + remap logic that runs
//! regardless of transport.

use octave_lib::cache::model::{Artist, Playlist, PlaylistTrack, Track};
use octave_lib::cache::repo;
use octave_lib::db;
use octave_lib::sync::PendingOpKind;

fn now() -> String {
    "2026-06-20T12:00:00.000Z".to_string()
}

#[tokio::test]
async fn outbox_is_fifo_and_clears_per_op() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    let a = PendingOpKind::PlaylistCreate {
        local_id: "local:1".into(),
        name: "Mix".into(),
    };
    let b = PendingOpKind::PlaylistRename {
        playlist_id: "local:1".into(),
        name: "Renamed".into(),
    };
    let id_a = repo::enqueue_op(&pool, a.op_type(), &a.to_payload_json().unwrap())
        .await
        .unwrap();
    repo::enqueue_op(&pool, b.op_type(), &b.to_payload_json().unwrap())
        .await
        .unwrap();

    assert_eq!(repo::count_pending_ops(&pool).await.unwrap(), 2);

    let ops = repo::list_pending_ops(&pool).await.unwrap();
    assert_eq!(ops.len(), 2);
    // FIFO: first enqueued comes first.
    assert_eq!(ops[0].id, id_a);
    assert_eq!(ops[0].op_type, "playlist.create");
    assert_eq!(ops[1].op_type, "playlist.rename");

    repo::delete_pending_op(&pool, ops[0].id).await.unwrap();
    assert_eq!(repo::count_pending_ops(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn mark_failed_bumps_attempts_and_records_error() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    let op = PendingOpKind::PlaylistDelete {
        playlist_id: "p1".into(),
    };
    let id = repo::enqueue_op(&pool, op.op_type(), &op.to_payload_json().unwrap())
        .await
        .unwrap();

    repo::mark_op_failed(&pool, id, "transport error: refused")
        .await
        .unwrap();
    repo::mark_op_failed(&pool, id, "transport error: refused again")
        .await
        .unwrap();

    let ops = repo::list_pending_ops(&pool).await.unwrap();
    assert_eq!(ops[0].attempts, 2);
    assert_eq!(
        ops[0].last_error.as_deref(),
        Some("transport error: refused again")
    );
}

#[tokio::test]
async fn local_id_rewrite_cascades_to_playlist_tracks() {
    // Simulates the cache-row half of `remap_local_id`: a locally-created
    // playlist (temp id) plus its entries, rewritten to the server id.
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    // A track must exist for FK-free playlist_tracks (loose ref), but we
    // still need a playlist row.
    repo::upsert_playlist(
        &pool,
        &Playlist {
            id: "local:abc".into(),
            owner_id: "u1".into(),
            name: "Offline mix".into(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    repo::replace_playlist_tracks(
        &pool,
        "local:abc",
        &[PlaylistTrack {
            playlist_id: "local:abc".into(),
            track_id: "t1".into(),
            position: 1,
            added_at: now(),
        }],
    )
    .await
    .unwrap();

    // Apply the same transactional remap the engine runs (insert-new →
    // repoint-children → delete-old; a straight id UPDATE would trip the
    // playlist_tracks FK).
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO playlists (id, owner_id, name, updated_at)
         SELECT ?2, owner_id, name, updated_at FROM playlists WHERE id = ?1",
    )
    .bind("local:abc")
    .bind("server-uuid")
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE playlist_tracks SET playlist_id = ?2 WHERE playlist_id = ?1")
        .bind("local:abc")
        .bind("server-uuid")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM playlists WHERE id = ?1")
        .bind("local:abc")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let pls = repo::list_playlists(&pool).await.unwrap();
    assert_eq!(pls.len(), 1);
    assert_eq!(pls[0].id, "server-uuid");
    let entries = repo::list_playlist_tracks(&pool, "server-uuid")
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track_id, "t1");
}

#[tokio::test]
async fn delete_sync_state_removes_only_targeted_row() {
    use octave_lib::cache::model::SyncState;
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    for id in ["a1", "a2"] {
        repo::upsert_sync_state(
            &pool,
            &SyncState {
                entity_type: "artist".into(),
                entity_id: id.into(),
                server_version: None,
                server_etag: Some("hash".into()),
                last_synced_at: now(),
            },
        )
        .await
        .unwrap();
    }

    repo::delete_sync_state(&pool, "artist", "a1")
        .await
        .unwrap();
    assert!(repo::get_sync_state(&pool, "artist", "a1")
        .await
        .unwrap()
        .is_none());
    assert!(repo::get_sync_state(&pool, "artist", "a2")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn artist_and_track_rows_persist_for_reconcile() {
    // Sanity: the entities the pull loops walk are listable.
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    repo::upsert_artist(
        &pool,
        &Artist {
            id: "ar1".into(),
            name: "X".into(),
            sort_name: None,
            storage_bytes: 0,
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    // album required by tracks FK
    repo::upsert_album(
        &pool,
        &octave_lib::cache::model::Album {
            id: "al1".into(),
            artist_id: "ar1".into(),
            title: "A".into(),
            release_year: None,
            storage_bytes: 0,
            updated_at: now(),
        },
    )
    .await
    .unwrap();
    repo::upsert_track(
        &pool,
        &Track {
            id: "tr1".into(),
            album_id: "al1".into(),
            artist_id: "ar1".into(),
            title: "song".into(),
            track_no: Some(1),
            disc_no: None,
            duration_ms: 1000,
            codec: "flac".into(),
            bitrate_kbps: None,
            file_size: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            local_file_path: "/tmp/does-not-exist.flac".into(),
            metadata_json: "{}".into(),
            downloaded_at: now(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();

    assert_eq!(repo::list_artists(&pool).await.unwrap().len(), 1);
    assert_eq!(repo::list_downloaded_tracks(&pool).await.unwrap().len(), 1);
}
