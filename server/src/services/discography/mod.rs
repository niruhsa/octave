//! Discography sync (Phase 14) — reconcile each artist against an online
//! metadata provider (MusicBrainz) so managers can see which albums/EPs/singles
//! the library is missing and, for owned releases, which tracks are missing.
//!
//! Server-only + Manager-gated + gated behind `DISCOGRAPHY_ENABLED` (the server
//! boots + the endpoints report `enabled = false` when off). See
//! DISCOGRAPHY_SYNC.md for the full design.
//!
//! Pipeline: [`DiscographyService`] resolves an artist to a provider id (with a
//! confidence policy + manager disambiguation), fetches the discography via a
//! [`DiscographyProvider`], diffs it against the library (fuzzy title matching in
//! [`r#match`]), and persists a filtered gap report plus the raw snapshot so the
//! suppression list ([`diff::apply_ignores`]) can re-filter without re-hitting
//! the provider.

#[cfg(feature = "chromaprint")]
mod acoustid;
mod diff;
mod discogs;
mod r#match;
mod musicbrainz;
mod provider;
mod service;

pub use provider::{ArtistCandidate, DiscographyProvider, ProviderReleaseGroup, ProviderTrack};
pub use service::{
    AudioResolver, DiscographyCfg, DiscographyPassReport, DiscographyService, DiscographyStatus,
    IgnoreRequest, NewReleaseNotifier, SyncOutcome,
};

use std::sync::Arc;

use crate::config::DiscographyConfig;

/// Build the configured metadata provider (`DISCOGRAPHY_PROVIDER`): `discogs`
/// when selected (needs `DISCOGRAPHY_DISCOGS_TOKEN` for search), else the
/// MusicBrainz default.
pub fn build_provider(cfg: &DiscographyConfig) -> Arc<dyn DiscographyProvider> {
    match cfg.provider.as_str() {
        "discogs" => {
            if cfg.discogs_token.is_none() {
                tracing::warn!(
                    "DISCOGRAPHY_PROVIDER=discogs but DISCOGRAPHY_DISCOGS_TOKEN is unset — \
                     artist search will fail (401)"
                );
            }
            Arc::new(discogs::DiscogsDiscography::new(
                cfg.discogs_token.clone(),
                cfg.contact.clone(),
            ))
        }
        _ => Arc::new(musicbrainz::MusicBrainzDiscography::new(cfg.contact.clone())),
    }
}

/// Build the Phase-E audio-anchored resolver (AcoustID → MusicBrainz), or `None`
/// when it's not available: needs the `chromaprint` build feature, a
/// `DISCOGRAPHY_ACOUSTID_KEY`, and the MusicBrainz provider.
#[cfg(feature = "chromaprint")]
pub fn build_audio_resolver(cfg: &DiscographyConfig) -> Option<Arc<dyn AudioResolver>> {
    acoustid::build(cfg)
}

#[cfg(not(feature = "chromaprint"))]
pub fn build_audio_resolver(_cfg: &DiscographyConfig) -> Option<Arc<dyn AudioResolver>> {
    None
}

impl From<&DiscographyConfig> for DiscographyCfg {
    fn from(c: &DiscographyConfig) -> Self {
        DiscographyCfg {
            match_threshold: c.match_threshold,
            title_sim: c.title_sim,
            include_types: c.include_types.clone(),
            sync_interval_secs: c.sync_interval_secs,
        }
    }
}
