//! Podcast cache tests. The HTTP/transport paths need a live server, so these
//! cover the cache-write + storage-accounting + subscription-flag logic the
//! `PodcastService` / `DownloadManager` delegate to, plus that the widened
//! `sync_state` CHECK (migration 0004) accepts the two new entity types.

use octave_lib::cache::model::{Podcast, PodcastEpisode, SyncState};
use octave_lib::cache::repo;
use octave_lib::db;

fn now() -> String {
    "2026-06-26T12:00:00.000Z".to_string()
}

fn show(id: &str, subscribed: i64) -> Podcast {
    Podcast {
        id: id.into(),
        feed_url: format!("https://feeds.example.com/{id}"),
        title: "Test Show".into(),
        author: Some("Host".into()),
        description: None,
        image_url: Some("https://art.example.com/x.jpg".into()),
        language: Some("en".into()),
        categories: "[\"Tech\"]".into(),
        subscribed,
        updated_at: now(),
    }
}

fn episode(id: &str, podcast_id: &str, size: i64) -> PodcastEpisode {
    PodcastEpisode {
        id: id.into(),
        podcast_id: podcast_id.into(),
        guid: format!("guid-{id}"),
        title: format!("Episode {id}"),
        description: None,
        enclosure_url: format!("https://cdn.example.com/{id}.mp3"),
        episode_no: Some(1),
        season_no: None,
        duration_ms: Some(1_800_000),
        codec: Some("mp3".into()),
        bitrate_kbps: Some(128),
        file_size: Some(size),
        local_file_path: Some(format!("/dl/Podcasts/Test Show/{id}.mp3")),
        image_path: None,
        published_at: Some(now()),
        metadata_json: "{}".into(),
        downloaded_at: Some(now()),
        updated_at: now(),
    }
}

#[tokio::test]
async fn subscribe_flag_drives_the_offline_list() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    repo::upsert_podcast(&pool, &show("p1", 1)).await.unwrap();
    repo::upsert_podcast(&pool, &show("p2", 0)).await.unwrap();

    // Only the subscribed show appears in the offline subscription list.
    let subs = repo::list_subscribed_podcasts(&pool).await.unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].id, "p1");
    // …but both are cached (downloaded-episode shows are reconciled too).
    assert_eq!(repo::list_all_podcasts(&pool).await.unwrap().len(), 2);

    // Flipping the flag updates the list without touching the rest of the row.
    repo::set_podcast_subscribed(&pool, "p2", true).await.unwrap();
    assert_eq!(repo::list_subscribed_podcasts(&pool).await.unwrap().len(), 2);
    let p2 = repo::get_podcast(&pool, "p2").await.unwrap().unwrap();
    assert_eq!(p2.title, "Test Show");
    assert_eq!(p2.subscribed, 1);
}

#[tokio::test]
async fn episode_upsert_counts_and_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    repo::upsert_podcast(&pool, &show("p1", 1)).await.unwrap();
    repo::upsert_episode(&pool, &episode("e1", "p1", 5_000_000)).await.unwrap();
    repo::upsert_episode(&pool, &episode("e2", "p1", 7_000_000)).await.unwrap();

    assert_eq!(
        repo::count_downloaded_episodes_for_podcast(&pool, "p1").await.unwrap(),
        2
    );
    assert_eq!(repo::count_downloaded_episodes(&pool).await.unwrap(), 2);
    assert_eq!(repo::downloaded_episode_bytes(&pool).await.unwrap(), 12_000_000);
    assert_eq!(repo::list_episodes_for_podcast(&pool, "p1").await.unwrap().len(), 2);

    // Deleting one episode drops the count; deleting the show cascades.
    repo::delete_episode(&pool, "e1").await.unwrap();
    assert_eq!(
        repo::count_downloaded_episodes_for_podcast(&pool, "p1").await.unwrap(),
        1
    );
    repo::delete_podcast(&pool, "p1").await.unwrap();
    assert!(repo::get_podcast(&pool, "p1").await.unwrap().is_none());
    assert_eq!(repo::count_downloaded_episodes(&pool).await.unwrap(), 0);
}

#[tokio::test]
async fn sync_state_accepts_podcast_entity_types() {
    // Migration 0004 widened the sync_state CHECK; the reconcile stamps these.
    let tmp = tempfile::tempdir().unwrap();
    let pool = db::open(&tmp.path().join("c.sqlite")).await.unwrap();

    for entity_type in ["podcast", "podcast_episode"] {
        repo::upsert_sync_state(
            &pool,
            &SyncState {
                entity_type: entity_type.into(),
                entity_id: "x".into(),
                server_version: None,
                server_etag: Some("hash".into()),
                last_synced_at: now(),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("sync_state rejected '{entity_type}': {e}"));
        let got = repo::get_sync_state(&pool, entity_type, "x").await.unwrap();
        assert_eq!(got.unwrap().server_etag.as_deref(), Some("hash"));
    }
}
