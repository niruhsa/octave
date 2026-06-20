//! Phase 7 playlist tests.
//!
//! The `PlaylistService` online/offline decision needs a live server + an
//! `AuthManager`, so these tests pin down the persistence-layer invariants
//! the service relies on: `replace_playlist_tracks` contiguity + ordering,
//! the optimistic splice/remove/renumber the service applies, and the
//! dependent-op pruning the local-id delete path runs.

use music_app_lib::cache::model::{Playlist, PlaylistTrack};
use music_app_lib::cache::repo;
use music_app_lib::db;
use music_app_lib::sync::ops::{is_local_id, PendingOpKind};

fn now() -> String {
    "2026-06-20T12:00:00.000Z".to_string()
}

fn entry(pid: &str, tid: &str, pos: i64) -> PlaylistTrack {
    PlaylistTrack {
        playlist_id: pid.into(),
        track_id: tid.into(),
        position: pos,
        added_at: now(),
    }
}

/// `replace_playlist_tracks` must wipe the old list and insert the new one
/// in the given order with the given positions — the service relies on
/// this atomicity for every optimistic mutation.
#[tokio::test]
async fn replace_playlist_tracks_is_atomic_and_ordered() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();
    repo::upsert_playlist(
        &pool,
        &Playlist {
            id: "p1".into(),
            owner_id: "u1".into(),
            name: "Mix".into(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();

    // Seed three entries.
    repo::replace_playlist_tracks(
        &pool,
        "p1",
        &[
            entry("p1", "a", 1),
            entry("p1", "b", 2),
            entry("p1", "c", 3),
        ],
    )
    .await
    .unwrap();

    // Replace with a reordered subset — the old rows must be gone.
    repo::replace_playlist_tracks(
        &pool,
        "p1",
        &[entry("p1", "c", 1), entry("p1", "a", 2)],
    )
    .await
    .unwrap();

    let rows = repo::list_playlist_tracks(&pool, "p1").await.unwrap();
    assert_eq!(
        rows.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
        [("c", 1), ("a", 2)]
    );
}

/// Mirrors `PlaylistService::optimistic_add` + `optimistic_remove`: read,
/// splice, renumber 1..N, replace. Verifies the cache stays contiguous
/// after an insert-in-middle then a remove.
#[tokio::test]
async fn optimistic_insert_then_remove_keeps_contiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();
    repo::upsert_playlist(
        &pool,
        &Playlist {
            id: "p2".into(),
            owner_id: "u1".into(),
            name: "Mix".into(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();

    // Start: [a, b, c].
    let mut rows = vec![entry("p2", "a", 1), entry("p2", "b", 2), entry("p2", "c", 3)];
    repo::replace_playlist_tracks(&pool, "p2", &rows).await.unwrap();

    // Insert x at position 2 (1-based) → [a, x, b, c].
    rows.insert(1, entry("p2", "x", 0));
    for (i, r) in rows.iter_mut().enumerate() {
        r.position = (i + 1) as i64;
    }
    repo::replace_playlist_tracks(&pool, "p2", &rows).await.unwrap();
    let after_add = repo::list_playlist_tracks(&pool, "p2").await.unwrap();
    assert_eq!(
        after_add.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
        [("a", 1), ("x", 2), ("b", 3), ("c", 4)]
    );

    // Remove position 3 (b) → [a, x, c].
    let mut rows = repo::list_playlist_tracks(&pool, "p2").await.unwrap();
    rows.remove(2);
    for (i, r) in rows.iter_mut().enumerate() {
        r.position = (i + 1) as i64;
    }
    repo::replace_playlist_tracks(&pool, "p2", &rows).await.unwrap();
    let after_rm = repo::list_playlist_tracks(&pool, "p2").await.unwrap();
    assert_eq!(
        after_rm.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
        [("a", 1), ("x", 2), ("c", 3)]
    );
    // Contiguity invariant: positions are exactly 1..=N.
    for (i, r) in after_rm.iter().enumerate() {
        assert_eq!(r.position as usize, i + 1);
    }
}

/// Mirrors `PlaylistService::delete_offline_local`: deleting a `local:`
/// playlist must also drop every queued op that still references it,
/// otherwise the engine would replay a create for a discarded playlist.
#[tokio::test]
async fn delete_offline_local_drops_dependent_ops() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    let local_id = "local:deadbeef";
    repo::upsert_playlist(
        &pool,
        &Playlist {
            id: local_id.into(),
            owner_id: "u1".into(),
            name: "Discarded".into(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();

    // Queue a create + a couple of dependent ops.
    let ops = [
        PendingOpKind::PlaylistCreate {
            local_id: local_id.into(),
            name: "Discarded".into(),
        },
        PendingOpKind::PlaylistAddTrack {
            playlist_id: local_id.into(),
            track_id: "t1".into(),
            position: 0,
        },
        // An unrelated op for a different playlist must survive.
        PendingOpKind::PlaylistRename {
            playlist_id: "server-uuid".into(),
            name: "Keep me".into(),
        },
    ];
    for op in &ops {
        repo::enqueue_op(&pool, op.op_type(), &op.to_payload_json().unwrap())
            .await
            .unwrap();
    }
    assert_eq!(repo::count_pending_ops(&pool).await.unwrap(), 3);

    // Replicate the service's pruning loop.
    for row in repo::list_pending_ops(&pool).await.unwrap() {
        if let Ok(kind) = PendingOpKind::from_payload_json(&row.payload_json) {
            if kind.references_playlist(local_id) {
                repo::delete_pending_op(&pool, row.id).await.unwrap();
            }
        }
    }
    // Drop the cache row too.
    repo::delete_playlist(&pool, local_id).await.unwrap();

    let remaining = repo::list_pending_ops(&pool).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        PendingOpKind::from_payload_json(&remaining[0].payload_json).unwrap().op_type(),
        "playlist.rename"
    );
    assert!(repo::list_playlists(&pool).await.unwrap().is_empty());
}

/// A locally-minted playlist id round-trips through the cache and is
/// detectable by `is_local_id` — the flag `MergedPlaylist.local` depends on.
#[tokio::test]
async fn local_id_playlist_round_trips_and_is_detectable() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    let local = "local:abc123";
    repo::upsert_playlist(
        &pool,
        &Playlist {
            id: local.into(),
            owner_id: "u1".into(),
            name: "Offline".into(),
            updated_at: now(),
        },
    )
    .await
    .unwrap();

    let listed = repo::list_playlists(&pool).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(is_local_id(&listed[0].id));
    assert!(!is_local_id("550e8400-e29b-41d4-a716-446655440000"));
}
