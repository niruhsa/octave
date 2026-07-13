//! Nearest-neighbor search over embeddings (Phase 12).
//!
//! [`SimilarityIndex`] is a trait so the search backend can be swapped by
//! config. [`BruteForceIndex`] holds every embedding in memory and does a
//! cosine scan — microseconds-to-low-ms even for ~100k tracks (a 512-d f32
//! embedding is ~2 KB, so ~200 MB at 100k), which is plenty for a self-hosted
//! library. A `pgvector`-backed ANN index is the documented scale-out path
//! (Phase D) behind the same trait.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::db::repo::TrackFeatureRepo;
use crate::error::Result;

/// k-nearest-neighbor search over the analyzed catalog.
#[async_trait]
pub trait SimilarityIndex: Send + Sync {
    /// The `k` nearest track ids to `seed` by cosine distance, nearest first,
    /// excluding the seed itself. Returns an empty vec when the seed has no
    /// embedding loaded.
    async fn nearest(&self, seed: Uuid, k: usize) -> Result<Vec<(Uuid, f32)>>;
    /// Whether `seed` currently has a loaded embedding (drives the radio's
    /// fall-back-to-behavioral decision without a full search).
    async fn has(&self, seed: Uuid) -> bool;
    /// Reload all embeddings from the repo (called after an analysis pass).
    async fn reload(&self) -> Result<()>;
    /// Number of embeddings currently loaded.
    async fn len(&self) -> usize;

    /// Single-query recommendation over a set of `seeds` via their **centroid**
    /// (the `k` nearest to the averaged seed vector, seeds excluded, nearest
    /// first). Returns `Ok(None)` when this backend has no efficient centroid
    /// path — the caller then falls back to per-seed aggregation. Default:
    /// `None`. The `pgvector` backend overrides it to collapse a whole playlist
    /// into one ANN query (O(1) queries regardless of playlist size).
    async fn centroid_nearest(&self, seeds: &[Uuid], k: usize) -> Result<Option<Vec<(Uuid, f32)>>> {
        let _ = (seeds, k);
        Ok(None)
    }
}

/// Cosine similarity of two equal-length, finite vectors. Embeddings are stored
/// unit-normalized, so this is effectively a dot product — but we normalize
/// defensively so a stray non-unit vector can't dominate the ranking.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// In-memory brute-force cosine index, refreshed from the repo for one
/// `model_version`.
pub struct BruteForceIndex {
    features: Arc<dyn TrackFeatureRepo>,
    model_version: String,
    rows: RwLock<Vec<(Uuid, Vec<f32>)>>,
}

impl BruteForceIndex {
    /// Build an empty index for `model_version`. Call [`reload`](SimilarityIndex::reload)
    /// (or [`load`](Self::load)) to populate it.
    pub fn new(features: Arc<dyn TrackFeatureRepo>, model_version: impl Into<String>) -> Self {
        Self {
            features,
            model_version: model_version.into(),
            rows: RwLock::new(Vec::new()),
        }
    }

    /// Build + populate in one step.
    pub async fn load(
        features: Arc<dyn TrackFeatureRepo>,
        model_version: impl Into<String>,
    ) -> Result<Self> {
        let idx = Self::new(features, model_version);
        idx.reload().await?;
        Ok(idx)
    }
}

#[async_trait]
impl SimilarityIndex for BruteForceIndex {
    async fn nearest(&self, seed: Uuid, k: usize) -> Result<Vec<(Uuid, f32)>> {
        let rows = self.rows.read().await;
        let Some((_, seed_vec)) = rows.iter().find(|(id, _)| *id == seed) else {
            return Ok(Vec::new());
        };
        let mut scored: Vec<(Uuid, f32)> = rows
            .iter()
            .filter(|(id, _)| *id != seed)
            .map(|(id, v)| (*id, cosine_similarity(seed_vec, v)))
            .collect();
        // Highest similarity first; total order via the partial cmp with NaN last.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    async fn has(&self, seed: Uuid) -> bool {
        self.rows.read().await.iter().any(|(id, _)| *id == seed)
    }

    async fn reload(&self) -> Result<()> {
        let loaded = self.features.all_for_model(&self.model_version).await?;
        let rows: Vec<(Uuid, Vec<f32>)> = loaded
            .into_iter()
            .map(|f| (f.track_id, f.embedding))
            .collect();
        *self.rows.write().await = rows;
        Ok(())
    }

    async fn len(&self) -> usize {
        self.rows.read().await.len()
    }
}

/// Postgres/pgvector-backed similarity index (Phase 13). Postgres *is* the
/// index, so there's nothing to hold in memory: every method delegates to the
/// [`TrackFeatureRepo`]'s ANN queries. [`reload`](SimilarityIndex::reload)
/// ensures the derived `vector` column + HNSW index exist (sized to `dims`) and
/// backfills them from the BYTEA source of truth — so it stays in sync after
/// each analysis pass without a separate write path.
pub struct PgVectorIndex {
    features: Arc<dyn TrackFeatureRepo>,
    model_version: String,
    dims: usize,
}

impl PgVectorIndex {
    pub fn new(
        features: Arc<dyn TrackFeatureRepo>,
        model_version: impl Into<String>,
        dims: usize,
    ) -> Self {
        Self {
            features,
            model_version: model_version.into(),
            dims,
        }
    }

    /// Average the present seed embeddings into one unit-normalized centroid.
    /// `None` when no seed has a usable embedding.
    async fn centroid(&self, seeds: &[Uuid]) -> Result<Option<Vec<f32>>> {
        let mut sum: Vec<f32> = Vec::new();
        let mut count = 0usize;
        for &seed in seeds {
            if let Some(feat) = self.features.get(seed).await? {
                if feat.model_version != self.model_version {
                    continue;
                }
                if sum.is_empty() {
                    sum = vec![0.0; feat.embedding.len()];
                }
                if sum.len() == feat.embedding.len() {
                    for (s, v) in sum.iter_mut().zip(&feat.embedding) {
                        *s += *v;
                    }
                    count += 1;
                }
            }
        }
        if count == 0 {
            return Ok(None);
        }
        for s in sum.iter_mut() {
            *s /= count as f32;
        }
        let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for s in sum.iter_mut() {
                *s /= norm;
            }
        }
        Ok(Some(sum))
    }
}

#[async_trait]
impl SimilarityIndex for PgVectorIndex {
    async fn nearest(&self, seed: Uuid, k: usize) -> Result<Vec<(Uuid, f32)>> {
        self.features.nearest(seed, &self.model_version, k).await
    }

    async fn has(&self, seed: Uuid) -> bool {
        self.features
            .has_vector(seed, &self.model_version)
            .await
            .unwrap_or(false)
    }

    async fn reload(&self) -> Result<()> {
        // Postgres is the index; "reload" means keep the derived vector column +
        // HNSW index in sync with the BYTEA source of truth.
        self.features
            .prepare_vector_index(&self.model_version, self.dims)
            .await
    }

    async fn len(&self) -> usize {
        self.features
            .count_for_model(&self.model_version)
            .await
            .unwrap_or(0) as usize
    }

    async fn centroid_nearest(&self, seeds: &[Uuid], k: usize) -> Result<Option<Vec<(Uuid, f32)>>> {
        let Some(centroid) = self.centroid(seeds).await? else {
            return Ok(None);
        };
        // Over-fetch so seed rows can be filtered out without shrinking the pool.
        let exclude: std::collections::HashSet<Uuid> = seeds.iter().copied().collect();
        let ranked = self
            .features
            .nearest_to_vector(&centroid, &self.model_version, k + exclude.len())
            .await?;
        let out: Vec<(Uuid, f32)> = ranked
            .into_iter()
            .filter(|(id, _)| !exclude.contains(id))
            .take(k)
            .collect();
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{NewTrackFeature, TrackFeature, TrackFeatureStatus};
    use std::sync::Mutex;
    use time::OffsetDateTime;

    #[derive(Default)]
    struct FakeFeatures {
        rows: Mutex<Vec<TrackFeature>>,
    }
    impl FakeFeatures {
        fn insert(&self, id: Uuid, embedding: Vec<f32>) {
            self.rows.lock().unwrap().push(TrackFeature {
                track_id: id,
                dims: embedding.len() as i32,
                embedding,
                model_version: "dsp-v1".into(),
                source_sig: "sig".into(),
                chromaprint: None,
                analyzed_at: OffsetDateTime::now_utc(),
            });
        }
    }
    #[async_trait]
    impl TrackFeatureRepo for FakeFeatures {
        async fn upsert(&self, _: NewTrackFeature) -> Result<()> {
            Ok(())
        }
        async fn get(&self, id: Uuid) -> Result<Option<TrackFeature>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|f| f.track_id == id)
                .cloned())
        }
        async fn all_for_model(&self, _: &str) -> Result<Vec<TrackFeature>> {
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn statuses(&self) -> Result<Vec<TrackFeatureStatus>> {
            Ok(vec![])
        }
        async fn count_for_model(&self, _: &str) -> Result<i64> {
            Ok(self.rows.lock().unwrap().len() as i64)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0); // mismatched len
    }

    #[tokio::test]
    async fn nearest_orders_by_similarity_and_excludes_seed() {
        let feats = Arc::new(FakeFeatures::default());
        let seed = Uuid::new_v4();
        let near = Uuid::new_v4();
        let far = Uuid::new_v4();
        feats.insert(seed, vec![1.0, 0.0, 0.0]);
        feats.insert(near, vec![0.9, 0.1, 0.0]);
        feats.insert(far, vec![0.0, 0.0, 1.0]);

        let idx = BruteForceIndex::load(feats, "dsp-v1").await.unwrap();
        assert_eq!(idx.len().await, 3);
        assert!(idx.has(seed).await);

        let out = idx.nearest(seed, 10).await.unwrap();
        assert_eq!(out.len(), 2, "seed excluded");
        assert_eq!(out[0].0, near, "nearest first");
        assert_eq!(out[1].0, far);
    }

    #[tokio::test]
    async fn nearest_empty_when_seed_unknown() {
        let feats = Arc::new(FakeFeatures::default());
        feats.insert(Uuid::new_v4(), vec![1.0, 0.0]);
        let idx = BruteForceIndex::load(feats, "dsp-v1").await.unwrap();
        assert!(idx.nearest(Uuid::new_v4(), 5).await.unwrap().is_empty());
        assert!(!idx.has(Uuid::new_v4()).await);
    }

    // --- PgVectorIndex (Phase 13) — logic verified over the in-memory fake,
    //     which inherits the default (brute-force) repo ANN impls. ---

    #[tokio::test]
    async fn pgvector_delegates_nearest_and_has() {
        let feats = Arc::new(FakeFeatures::default());
        let seed = Uuid::new_v4();
        let near = Uuid::new_v4();
        let far = Uuid::new_v4();
        feats.insert(seed, vec![1.0, 0.0, 0.0]);
        feats.insert(near, vec![0.9, 0.1, 0.0]);
        feats.insert(far, vec![0.0, 0.0, 1.0]);

        let idx = PgVectorIndex::new(feats, "dsp-v1", 3);
        // reload prepares/backfills the (fake) index — a no-op here, must succeed.
        idx.reload().await.unwrap();
        assert_eq!(idx.len().await, 3);
        assert!(idx.has(seed).await);
        assert!(!idx.has(Uuid::new_v4()).await);

        let out = idx.nearest(seed, 10).await.unwrap();
        assert_eq!(out.len(), 2, "seed excluded");
        assert_eq!(out[0].0, near, "nearest first");
        assert_eq!(out[1].0, far);
    }

    #[tokio::test]
    async fn pgvector_centroid_nearest_excludes_seeds_and_ranks_by_centroid() {
        let feats = Arc::new(FakeFeatures::default());
        // Two seeds sit on the x and y axes; their centroid points into the
        // xy-diagonal, so the diagonal candidate outranks the z-axis one.
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        let diag = Uuid::new_v4();
        let zaxis = Uuid::new_v4();
        feats.insert(s1, vec![1.0, 0.0, 0.0]);
        feats.insert(s2, vec![0.0, 1.0, 0.0]);
        feats.insert(diag, vec![0.7, 0.7, 0.0]);
        feats.insert(zaxis, vec![0.0, 0.0, 1.0]);

        let idx = PgVectorIndex::new(feats, "dsp-v1", 3);
        let ranked = idx
            .centroid_nearest(&[s1, s2], 10)
            .await
            .unwrap()
            .expect("pgvector supports centroid queries");
        // Seeds excluded; the diagonal candidate ranks ahead of the z-axis one.
        assert!(!ranked.iter().any(|(id, _)| *id == s1 || *id == s2));
        assert_eq!(ranked[0].0, diag, "closest to the centroid first");
        assert_eq!(ranked[1].0, zaxis);
    }

    #[tokio::test]
    async fn pgvector_centroid_none_when_no_seed_has_embedding() {
        let feats = Arc::new(FakeFeatures::default());
        feats.insert(Uuid::new_v4(), vec![1.0, 0.0]);
        let idx = PgVectorIndex::new(feats, "dsp-v1", 2);
        // Unknown seeds → no centroid → None (caller falls back).
        assert!(
            idx.centroid_nearest(&[Uuid::new_v4()], 5)
                .await
                .unwrap()
                .is_none()
        );
    }
}
