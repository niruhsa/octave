//! Acoustic fingerprinting — content-based "sounds like" radio (Phase 12).
//!
//! Two distinct concepts live here (don't conflate them):
//!
//!   * **Similarity embedding** ([`FeatureExtractor`] → [`SimilarityIndex`]) —
//!     a fixed-length vector per track whose cosine distance ≈ perceptual
//!     similarity. This powers "sounds like" radio.
//!   * **Identification fingerprint** ([`chromaprint`], optional) — a Chromaprint
//!     hash answering "is this the *same recording*?" for dedup / AcoustID
//!     metadata. It does **not** power "sounds like".
//!
//! The whole subsystem is **server-only** (the server has the files + CPU) and
//! gated behind `FINGERPRINT_ENABLED` — the server boots and behaves exactly as
//! before when it's off (radio stays purely behavioral).
//!
//! Pipeline: [`FingerprintService`] decodes each track via a [`FeatureExtractor`]
//! into the `track_features` table; [`BruteForceIndex`] holds the embeddings for
//! nearest-neighbor search; `RecommendationService` turns a seed track's
//! neighbors into a diversified radio queue (with a behavioral fallback when an
//! embedding isn't ready yet).

mod decode;
mod extractor;
mod index;
mod service;

#[cfg(feature = "chromaprint")]
mod chromaprint;

pub mod onnx;

pub use extractor::{DspExtractor, FeatureExtractor};
pub use index::{cosine_similarity, BruteForceIndex, PgVectorIndex, SimilarityIndex};
pub use service::{FingerprintReport, FingerprintService, FingerprintStatus};

use std::sync::Arc;

use crate::config::IndexKind;
use crate::db::repo::TrackFeatureRepo;

/// Build the configured [`FeatureExtractor`]: the learned ONNX encoder when a
/// model path is set (and the `onnx` feature is built), else the DSP baseline.
/// Centralizes the §4 "selection is config-driven" rule.
pub fn build_extractor(model_path: Option<&std::path::Path>) -> Arc<dyn FeatureExtractor> {
    match model_path {
        Some(path) => match onnx::try_build(path) {
            Some(ext) => {
                tracing::info!(model = %path.display(), "fingerprint: using ONNX extractor");
                ext
            }
            None => {
                tracing::warn!(
                    model = %path.display(),
                    "FINGERPRINT_MODEL is set but ONNX support is unavailable; \
                     falling back to the DSP extractor"
                );
                Arc::new(DspExtractor::new())
            }
        },
        None => Arc::new(DspExtractor::new()),
    }
}

/// Build the configured similarity index for the given extractor's model.
/// `BruteForce` holds embeddings in memory; `PgVector` delegates to a Postgres
/// ANN index (sized to `dims`). Both start unloaded — call
/// [`SimilarityIndex::reload`] (the analysis pass does, and `main` does once at
/// startup) to populate / prepare them.
pub fn build_index(
    kind: IndexKind,
    features: Arc<dyn TrackFeatureRepo>,
    model_version: &str,
    dims: usize,
) -> Arc<dyn SimilarityIndex> {
    match kind {
        IndexKind::BruteForce => Arc::new(BruteForceIndex::new(features, model_version)),
        IndexKind::PgVector => Arc::new(PgVectorIndex::new(features, model_version, dims)),
    }
}
