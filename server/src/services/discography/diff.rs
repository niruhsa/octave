//! The provider snapshot + the suppression filter (DISCOGRAPHY_SYNC.md §4.6–4.7).
//!
//! A `sync_artist` builds a [`ProviderSnapshot`] — the raw, pre-ignore diff:
//! every release-group, each tagged as owned (with its missing tracks) or
//! missing. [`apply_ignores`] then filters that snapshot against the artist's
//! suppression list to produce the report the UI sees. Because the snapshot is
//! persisted, ignore/unignore just re-run `apply_ignores` in memory — no network.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::models::{DiscographyIgnore, IncompleteAlbum, MissingRelease, MissingTrack};

/// One track missing from an owned album, as captured at sync time (pre-ignore).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapMissingTrack {
    pub title: String,
    pub position: Option<i32>,
    pub disc_no: Option<i32>,
    /// Recording MBID, when the provider supplied one.
    pub recording_id: Option<String>,
    /// Normalized title (§4.3) — the fallback ignore key.
    pub title_key: String,
}

/// One release-group in the snapshot. `matched_album_id` present ⇒ the library
/// owns it (and `missing_tracks` lists what's absent from that album); absent ⇒
/// the library is missing the whole release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapReleaseGroup {
    pub provider_id: String,
    pub title: String,
    pub album_type: String,
    pub year: Option<i32>,
    pub matched_album_id: Option<Uuid>,
    pub matched_album_title: Option<String>,
    #[serde(default)]
    pub missing_tracks: Vec<SnapMissingTrack>,
}

/// The full pre-ignore diff from one sync, persisted so suppression can
/// re-filter without re-hitting the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    pub release_groups: Vec<SnapReleaseGroup>,
}

/// Filter a snapshot against the suppression list → the `(missing_releases,
/// incomplete_albums)` the UI renders. Pure + network-free (DISCOGRAPHY_SYNC.md
/// §6): the single place the ignore filter lives, shared by sync/ignore/unignore.
pub fn apply_ignores(
    snapshot: &ProviderSnapshot,
    ignores: &[DiscographyIgnore],
) -> (Vec<MissingRelease>, Vec<IncompleteAlbum>) {
    // Release-scope: release-group ids to hide entirely.
    let release_ignored: HashSet<Uuid> = ignores
        .iter()
        .filter(|i| i.scope == "release")
        .map(|i| i.release_group_id)
        .collect();

    // Track-scope: per release-group, the set of track ignores to apply.
    let mut track_ignored: HashMap<Uuid, Vec<&DiscographyIgnore>> = HashMap::new();
    for i in ignores.iter().filter(|i| i.scope == "track") {
        track_ignored.entry(i.release_group_id).or_default().push(i);
    }

    let mut missing_releases = Vec::new();
    let mut incomplete_albums = Vec::new();

    for rg in &snapshot.release_groups {
        let rg_uuid = Uuid::parse_str(&rg.provider_id).ok();
        match rg.matched_album_id {
            // Missing release — unless the manager ignored it.
            None => {
                if let Some(u) = rg_uuid {
                    if release_ignored.contains(&u) {
                        continue;
                    }
                }
                missing_releases.push(MissingRelease {
                    title: rg.title.clone(),
                    album_type: rg.album_type.clone(),
                    year: rg.year,
                    provider_id: rg.provider_id.clone(),
                });
            }
            // Owned album — report the missing tracks that aren't ignored.
            Some(album_id) => {
                let ig = rg_uuid.and_then(|u| track_ignored.get(&u));
                let kept: Vec<MissingTrack> = rg
                    .missing_tracks
                    .iter()
                    .filter(|t| !is_track_ignored(t, ig))
                    .map(|t| MissingTrack {
                        title: t.title.clone(),
                        position: t.position,
                        disc_no: t.disc_no,
                        recording_id: t.recording_id.clone(),
                        title_key: t.title_key.clone(),
                    })
                    .collect();
                if !kept.is_empty() {
                    incomplete_albums.push(IncompleteAlbum {
                        album_id,
                        title: rg.matched_album_title.clone().unwrap_or_default(),
                        release_group_id: rg.provider_id.clone(),
                        missing_tracks: kept,
                    });
                }
            }
        }
    }

    (missing_releases, incomplete_albums)
}

/// Whether a snapshot track is suppressed by any of its release-group's track
/// ignores — by recording MBID when both sides have one, else by normalized
/// title key.
fn is_track_ignored(track: &SnapMissingTrack, ignores: Option<&Vec<&DiscographyIgnore>>) -> bool {
    let Some(list) = ignores else {
        return false;
    };
    let track_rec = track
        .recording_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());
    list.iter().any(|ig| {
        let by_recording = match (ig.recording_id, track_rec) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        let by_title = ig.title_key.as_deref() == Some(track.title_key.as_str());
        by_recording || by_title
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn ignore(scope: &str, rg: Uuid, rec: Option<Uuid>, title: Option<&str>) -> DiscographyIgnore {
        DiscographyIgnore {
            id: Uuid::new_v4(),
            artist_id: Uuid::new_v4(),
            scope: scope.to_string(),
            release_group_id: rg,
            recording_id: rec,
            title_key: title.map(|s| s.to_string()),
            label: "x".to_string(),
            created_at: OffsetDateTime::now_utc(),
        }
    }

    fn snapshot(rg_id: Uuid, album: Option<Uuid>) -> ProviderSnapshot {
        ProviderSnapshot {
            release_groups: vec![SnapReleaseGroup {
                provider_id: rg_id.to_string(),
                title: "Some Album".to_string(),
                album_type: "album".to_string(),
                year: Some(2000),
                matched_album_id: album,
                matched_album_title: album.map(|_| "Some Album".to_string()),
                missing_tracks: vec![SnapMissingTrack {
                    title: "Track One".to_string(),
                    position: Some(1),
                    disc_no: Some(1),
                    recording_id: None,
                    title_key: "track one".to_string(),
                }],
            }],
        }
    }

    #[test]
    fn release_ignore_hides_missing_release() {
        let rg = Uuid::new_v4();
        let snap = snapshot(rg, None);
        let (mr, _) = apply_ignores(&snap, &[]);
        assert_eq!(mr.len(), 1);
        let (mr, _) = apply_ignores(&snap, &[ignore("release", rg, None, None)]);
        assert!(mr.is_empty());
    }

    #[test]
    fn track_ignore_hides_missing_track_by_title() {
        let rg = Uuid::new_v4();
        let album = Uuid::new_v4();
        let snap = snapshot(rg, Some(album));
        let (_, ia) = apply_ignores(&snap, &[]);
        assert_eq!(ia.len(), 1);
        // Ignoring the only missing track drops the whole incomplete-album entry.
        let (_, ia) = apply_ignores(&snap, &[ignore("track", rg, None, Some("track one"))]);
        assert!(ia.is_empty());
    }

    #[test]
    fn track_ignore_matches_by_recording_id() {
        let rg = Uuid::new_v4();
        let album = Uuid::new_v4();
        let rec = Uuid::new_v4();
        let mut snap = snapshot(rg, Some(album));
        snap.release_groups[0].missing_tracks[0].recording_id = Some(rec.to_string());
        let (_, ia) = apply_ignores(&snap, &[ignore("track", rg, Some(rec), None)]);
        assert!(ia.is_empty());
    }
}
