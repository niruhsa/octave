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

mod diff;
mod r#match;
mod musicbrainz;
mod provider;
mod service;

pub use provider::{ArtistCandidate, DiscographyProvider, ProviderReleaseGroup, ProviderTrack};
pub use service::{
    DiscographyCfg, DiscographyPassReport, DiscographyService, DiscographyStatus, IgnoreRequest,
    SyncOutcome,
};

use std::sync::Arc;

use crate::config::DiscographyConfig;

/// Build the configured metadata provider. Only MusicBrainz today;
/// `DISCOGRAPHY_PROVIDER` is reserved for a future Discogs backend.
pub fn build_provider(cfg: &DiscographyConfig) -> Arc<dyn DiscographyProvider> {
    Arc::new(musicbrainz::MusicBrainzDiscography::new(cfg.contact.clone()))
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
