//! The provider abstraction — a metadata source the discography sync reconciles
//! against. `MusicBrainzDiscography` ([`super::musicbrainz`]) is the only impl
//! today; the trait keeps the service testable against a fake and lets a second
//! provider (Discogs) drop in later. Mirrors `CoverArtSource`.

use async_trait::async_trait;

use crate::error::Result;

/// A candidate provider artist for a name, from [`DiscographyProvider::resolve_artist`].
#[derive(Debug, Clone)]
pub struct ArtistCandidate {
    /// Provider artist id (a MusicBrainz artist MBID).
    pub provider_id: String,
    pub name: String,
    /// The provider's short disambiguation hint (e.g. "US rock band"), if any.
    pub disambiguation: Option<String>,
    /// Match score, 0–100 (higher is better).
    pub score: u8,
}

/// A release-group in a provider artist's discography.
#[derive(Debug, Clone)]
pub struct ProviderReleaseGroup {
    /// Provider release-group id (a MusicBrainz release-group MBID).
    pub provider_id: String,
    pub title: String,
    /// Mapped to our vocabulary: `album` / `ep` / `single` / `live` / `other`.
    /// The service reports only the types in `DISCOGRAPHY_INCLUDE_TYPES`.
    pub album_type: String,
    /// First-release year, when the provider exposes a date.
    pub year: Option<i32>,
}

/// One track of a release-group's canonical release.
#[derive(Debug, Clone)]
pub struct ProviderTrack {
    /// Recording id (a MusicBrainz recording MBID), when present — the stable
    /// key for a track ignore.
    pub provider_id: Option<String>,
    pub position: Option<i32>,
    pub disc_no: Option<i32>,
    pub title: String,
}

/// A metadata provider the discography sync reconciles the library against.
#[async_trait]
pub trait DiscographyProvider: Send + Sync {
    /// Stable id recorded in reports / status (e.g. `"musicbrainz"`).
    fn id(&self) -> &str;

    /// Candidate artist matches for `name`, best score first. `hint_titles` (a
    /// few local album titles) sharpens the provider's scoring. The confidence
    /// policy (auto-accept vs. needs-resolution) lives in the service, not here.
    async fn resolve_artist(
        &self,
        name: &str,
        hint_titles: &[String],
    ) -> Result<Vec<ArtistCandidate>>;

    /// Every release-group for a resolved provider artist id (paginated
    /// internally, rate-limited), each with its `album_type` already mapped.
    async fn release_groups(&self, provider_artist_id: &str) -> Result<Vec<ProviderReleaseGroup>>;

    /// The chosen canonical release's tracklist for a release-group.
    async fn tracklist(&self, provider_release_group_id: &str) -> Result<Vec<ProviderTrack>>;
}
