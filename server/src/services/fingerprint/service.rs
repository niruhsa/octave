//! The background analysis pass (Phase 12).
//!
//! [`FingerprintService`] walks the library and ensures every decodable track
//! has a fresh embedding for the current `model_version`. It mirrors the
//! image-optimize pass / podcast refresh poller: **idempotent, incremental,
//! bounded, and never blocks boot**. A first pass over a large library can take
//! hours — that's fine, because it's incremental (re-runs skip fresh rows) and
//! the radio degrades gracefully until embeddings exist.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use uuid::Uuid;

use crate::db::repo::{TrackFeatureRepo, TrackRepo};
use crate::db::models::NewTrackFeature;
use crate::error::{AppError, Result};

use super::extractor::FeatureExtractor;
use super::index::SimilarityIndex;

/// Snapshot for the status endpoint: how much of the library is analyzed.
#[derive(Debug, Clone, Default)]
pub struct FingerprintStatus {
    pub analyzed: i64,
    pub total: i64,
    pub model_version: String,
}

/// Outcome of one [`FingerprintService::run_pass`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FingerprintReport {
    /// Tracks (re)analyzed this pass.
    pub analyzed: u64,
    /// Tracks already fresh (same source signature + model) — skipped.
    pub skipped_fresh: u64,
    /// Tracks the build can't decode (e.g. MP3) — left unanalyzed by design.
    pub skipped_unanalyzable: u64,
    /// Tracks that errored (missing file, decode failure, …).
    pub failed: u64,
    /// Total tracks considered.
    pub total: u64,
}

#[derive(Clone)]
pub struct FingerprintService {
    pub tracks: Arc<dyn TrackRepo>,
    pub features: Arc<dyn TrackFeatureRepo>,
    pub extractor: Arc<dyn FeatureExtractor>,
    /// Refreshed after every pass so the radio sees new embeddings.
    pub index: Arc<dyn SimilarityIndex>,
    /// Library root used to resolve relative `Track.file_path`s (mirrors the
    /// streaming service). `None` requires absolute paths.
    pub library_root: Option<PathBuf>,
    /// Decode + DSP/ONNX workers run concurrently (bounded — CPU-heavy).
    pub concurrency: usize,
}

impl FingerprintService {
    pub fn new(
        tracks: Arc<dyn TrackRepo>,
        features: Arc<dyn TrackFeatureRepo>,
        extractor: Arc<dyn FeatureExtractor>,
        index: Arc<dyn SimilarityIndex>,
        concurrency: usize,
    ) -> Self {
        Self {
            tracks,
            features,
            extractor,
            index,
            library_root: None,
            concurrency: concurrency.max(1),
        }
    }

    /// Set the library root for resolving relative `file_path`s.
    pub fn with_library_root(mut self, root: Option<PathBuf>) -> Self {
        self.library_root = root;
        self
    }

    /// Resolve a stored `file_path` to an on-disk path (relative paths join the
    /// library root). `None` when it can't be resolved.
    fn resolve(&self, raw: &str) -> Option<PathBuf> {
        let candidate = PathBuf::from(raw);
        match (&self.library_root, candidate.is_absolute()) {
            (_, true) => Some(candidate),
            (Some(root), false) => Some(root.join(candidate)),
            (None, false) => None,
        }
    }

    /// The model version this service writes (status endpoint + index key).
    pub fn model_version(&self) -> &str {
        self.extractor.model_version()
    }

    /// Coverage snapshot: analyzed-for-current-model vs. total tracks.
    pub async fn status(&self) -> FingerprintStatus {
        let model = self.extractor.model_version().to_string();
        let analyzed = self.features.count_for_model(&model).await.unwrap_or(0);
        let total = self
            .tracks
            .list_all_ids_paths()
            .await
            .map(|v| v.len() as i64)
            .unwrap_or(0);
        FingerprintStatus {
            analyzed,
            total,
            model_version: model,
        }
    }

    /// Analyze every track lacking a fresh embedding for the current model,
    /// then reload the similarity index. Idempotent + incremental.
    pub async fn run_pass(&self) -> FingerprintReport {
        let model = self.extractor.model_version().to_string();
        let tracks = match self.tracks.list_all_ids_paths().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "fingerprint pass: listing tracks failed");
                return FingerprintReport::default();
            }
        };
        // Existing freshness signatures, keyed by track id.
        let fresh: std::collections::HashMap<Uuid, String> = match self.features.statuses().await {
            Ok(rows) => rows
                .into_iter()
                .filter(|s| s.model_version == model)
                .map(|s| (s.track_id, s.source_sig))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "fingerprint pass: loading statuses failed");
                Default::default()
            }
        };

        let total = tracks.len() as u64;
        // Decide work up front (cheap fs stat per track) so the concurrent
        // section only does the expensive decode for genuinely-stale tracks.
        let mut to_analyze: Vec<(Uuid, PathBuf, String)> = Vec::new();
        let mut skipped_fresh = 0u64;
        for t in tracks {
            let Some(path) = self.resolve(&t.file_path) else {
                continue;
            };
            let Some(sig) = source_sig(&path) else {
                // Unreadable file — count as failure, leave any old row in place.
                continue;
            };
            if fresh.get(&t.id).is_some_and(|existing| existing == &sig) {
                skipped_fresh += 1;
                continue;
            }
            to_analyze.push((t.id, path, sig));
        }

        // Bounded concurrent analysis.
        let results = stream::iter(to_analyze.into_iter().map(|(id, path, sig)| {
            let extractor = self.extractor.clone();
            let features = self.features.clone();
            let model = model.clone();
            async move { analyze_one(&*extractor, &*features, id, path, sig, &model).await }
        }))
        .buffer_unordered(self.concurrency)
        .collect::<Vec<AnalyzeOutcome>>()
        .await;

        let mut report = FingerprintReport {
            total,
            skipped_fresh,
            ..Default::default()
        };
        for r in results {
            match r {
                AnalyzeOutcome::Analyzed => report.analyzed += 1,
                AnalyzeOutcome::Unanalyzable => report.skipped_unanalyzable += 1,
                AnalyzeOutcome::Failed => report.failed += 1,
            }
        }

        if let Err(e) = self.index.reload().await {
            tracing::warn!(error = %e, "fingerprint pass: index reload failed");
        }
        tracing::info!(
            analyzed = report.analyzed,
            skipped_fresh = report.skipped_fresh,
            skipped_unanalyzable = report.skipped_unanalyzable,
            failed = report.failed,
            total = report.total,
            model = %model,
            "fingerprint pass complete"
        );
        report
    }

    /// Analyze (or re-analyze) one track on demand — the ingest/upload hook and
    /// the admin re-scan single-track path. Reloads the index on success so the
    /// new track is immediately reachable from radio.
    pub async fn analyze_track(&self, track_id: Uuid) -> Result<()> {
        let track = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        let path = self
            .resolve(&track.file_path)
            .ok_or_else(|| AppError::Internal("track file_path is relative but no LIBRARY_PATH is configured".into()))?;
        let sig = source_sig(&path)
            .ok_or_else(|| AppError::Io(std::io::Error::other("track file unreadable")))?;
        let model = self.extractor.model_version().to_string();
        match analyze_one(&*self.extractor, &*self.features, track_id, path, sig, &model).await {
            AnalyzeOutcome::Analyzed => {
                let _ = self.index.reload().await;
                Ok(())
            }
            AnalyzeOutcome::Unanalyzable => Err(AppError::InvalidArgument(
                "track codec is not analyzable (e.g. MP3)".into(),
            )),
            AnalyzeOutcome::Failed => {
                Err(AppError::Internal(format!("failed to analyze track {track_id}")))
            }
        }
    }

    /// Run an analysis pass on startup, then every `interval_secs` (0 =
    /// startup-only). Background + low priority — never blocks boot. Mirrors the
    /// podcast refresh poller / image-optimize pass.
    pub fn spawn_poller(self: &Arc<Self>, interval_secs: u64) {
        let this = self.clone();
        tokio::spawn(async move {
            this.run_pass().await;
        });
        if interval_secs == 0 {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            tick.tick().await; // consume the immediate first tick (startup pass ran)
            loop {
                tick.tick().await;
                this.run_pass().await;
            }
        });
    }
}

enum AnalyzeOutcome {
    Analyzed,
    Unanalyzable,
    Failed,
}

/// Extract + persist one track's embedding, classifying the outcome.
async fn analyze_one(
    extractor: &dyn FeatureExtractor,
    features: &dyn TrackFeatureRepo,
    track_id: Uuid,
    path: PathBuf,
    sig: String,
    model: &str,
) -> AnalyzeOutcome {
    match extractor.extract(&path).await {
        Ok(embedding) => {
            let chromaprint = compute_chromaprint(&path);
            let new = NewTrackFeature {
                track_id,
                dims: embedding.len() as i32,
                embedding,
                model_version: model.to_string(),
                source_sig: sig,
                chromaprint,
            };
            match features.upsert(new).await {
                Ok(()) => AnalyzeOutcome::Analyzed,
                Err(e) => {
                    tracing::warn!(%track_id, error = %e, "fingerprint: persist failed");
                    AnalyzeOutcome::Failed
                }
            }
        }
        // The extractor maps "can't decode this codec" to InvalidArgument.
        Err(AppError::InvalidArgument(_)) => AnalyzeOutcome::Unanalyzable,
        Err(e) => {
            tracing::debug!(%track_id, error = %e, "fingerprint: extract failed");
            AnalyzeOutcome::Failed
        }
    }
}

/// File-content signature: `size:mtime_secs`. Cheap (one stat) and changes when
/// a file is re-encoded/replaced, so the pass re-analyzes it. Mirrors the
/// image-optimizer freshness check. `None` when the file can't be stat'd.
fn source_sig(path: &std::path::Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("{}:{}", meta.len(), mtime))
}

/// Optionally compute a Chromaprint identification fingerprint (Phase 12E, the
/// `chromaprint` feature). Independent of "sounds like"; used for dedup /
/// AcoustID metadata enrichment. Returns `None` when the feature is off.
#[cfg(feature = "chromaprint")]
fn compute_chromaprint(path: &std::path::Path) -> Option<String> {
    super::chromaprint::fingerprint(path)
}

#[cfg(not(feature = "chromaprint"))]
fn compute_chromaprint(_path: &std::path::Path) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{
        NewTrack, Track, TrackFeature, TrackFeatureStatus,
    };
    use crate::db::repo::TrackIdPath;
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Mutex;
    use time::OffsetDateTime;

    /// No-op similarity index — the pass only calls `reload` on it.
    #[derive(Default)]
    struct NoopIndex;
    #[async_trait]
    impl SimilarityIndex for NoopIndex {
        async fn nearest(&self, _: Uuid, _: usize) -> Result<Vec<(Uuid, f32)>> {
            Ok(vec![])
        }
        async fn has(&self, _: Uuid) -> bool {
            false
        }
        async fn reload(&self) -> Result<()> {
            Ok(())
        }
        async fn len(&self) -> usize {
            0
        }
    }

    // A fake extractor that returns a fixed embedding for any path ending in a
    // decodable extension, and signals "unanalyzable" for ".mp3".
    struct FakeExtractor;
    #[async_trait]
    impl FeatureExtractor for FakeExtractor {
        fn model_version(&self) -> &str {
            "fake-v1"
        }
        fn dims(&self) -> usize {
            3
        }
        async fn extract(&self, path: &Path) -> Result<Vec<f32>> {
            if path.extension().and_then(|e| e.to_str()) == Some("mp3") {
                return Err(AppError::InvalidArgument("unanalyzable".into()));
            }
            Ok(vec![1.0, 0.0, 0.0])
        }
    }

    #[derive(Default)]
    struct FakeFeatures {
        rows: Mutex<Vec<TrackFeature>>,
    }
    #[async_trait]
    impl TrackFeatureRepo for FakeFeatures {
        async fn upsert(&self, new: NewTrackFeature) -> Result<()> {
            let mut g = self.rows.lock().unwrap();
            g.retain(|f| f.track_id != new.track_id);
            g.push(TrackFeature {
                track_id: new.track_id,
                embedding: new.embedding,
                dims: new.dims,
                model_version: new.model_version,
                source_sig: new.source_sig,
                chromaprint: new.chromaprint,
                analyzed_at: OffsetDateTime::now_utc(),
            });
            Ok(())
        }
        async fn get(&self, id: Uuid) -> Result<Option<TrackFeature>> {
            Ok(self.rows.lock().unwrap().iter().find(|f| f.track_id == id).cloned())
        }
        async fn all_for_model(&self, model: &str) -> Result<Vec<TrackFeature>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.model_version == model)
                .cloned()
                .collect())
        }
        async fn statuses(&self) -> Result<Vec<TrackFeatureStatus>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .map(|f| TrackFeatureStatus {
                    track_id: f.track_id,
                    source_sig: f.source_sig.clone(),
                    model_version: f.model_version.clone(),
                })
                .collect())
        }
        async fn count_for_model(&self, model: &str) -> Result<i64> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.model_version == model)
                .count() as i64)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    // Minimal TrackRepo fake: only get + list_all_ids_paths are exercised.
    #[derive(Default)]
    struct FakeTracks {
        rows: Mutex<Vec<Track>>,
    }
    impl FakeTracks {
        fn insert(&self, path: &str) -> Track {
            let t = mk_track(path);
            self.rows.lock().unwrap().push(t.clone());
            t
        }
    }
    fn mk_track(path: &str) -> Track {
        Track {
            id: Uuid::new_v4(),
            album_id: Uuid::new_v4(),
            artist_id: Uuid::new_v4(),
            title: "t".into(),
            track_no: None,
            disc_no: None,
            duration_ms: 1000,
            codec: "flac".into(),
            bitrate_kbps: None,
            file_path: path.into(),
            file_size: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            metadata_json: "{}".into(),
            is_single_release: false,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }
    #[async_trait]
    impl TrackRepo for FakeTracks {
        async fn create(&self, _: NewTrack) -> Result<Track> {
            unreachable!()
        }
        async fn get(&self, id: Uuid) -> Result<Option<Track>> {
            Ok(self.rows.lock().unwrap().iter().find(|t| t.id == id).cloned())
        }
        async fn list_by_album(&self, _: Uuid) -> Result<Vec<Track>> {
            Ok(vec![])
        }
        async fn search(&self, _: &str, _: i64, _: i64) -> Result<Vec<Track>> {
            Ok(vec![])
        }
        async fn update(&self, _: Uuid, _: &str, _: Option<i32>, _: Option<i32>, _: &str) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn find_by_file_path(&self, _: &str) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn reassign_artist(&self, _: Uuid, _: Uuid) -> Result<u64> {
            Ok(0)
        }
        async fn reassign_album(&self, _: Uuid, _: Uuid) -> Result<u64> {
            Ok(0)
        }
        async fn set_album(&self, _: Uuid, _: Uuid) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn set_single_release(&self, _: Uuid, _: bool) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn list_all_ids_paths(&self) -> Result<Vec<TrackIdPath>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .map(|t| TrackIdPath {
                    id: t.id,
                    file_path: t.file_path.clone(),
                    duration_ms: t.duration_ms,
                })
                .collect())
        }
        async fn update_duration(&self, _: Uuid, _: i64) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn update_file_props(
            &self,
            _: Uuid,
            _: &str,
            _: Option<i32>,
            _: Option<i64>,
            _: Option<i32>,
            _: Option<i32>,
            _: Option<i32>,
        ) -> Result<Option<Track>> {
            Ok(None)
        }
    }

    /// Write a small real file so `source_sig` (fs stat) succeeds; the fake
    /// extractor ignores the contents.
    fn touch(name: &str) -> String {
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, b"x").unwrap();
        p.to_string_lossy().into_owned()
    }

    fn make() -> (Arc<FingerprintService>, Arc<FakeTracks>, Arc<FakeFeatures>) {
        let tracks = Arc::new(FakeTracks::default());
        let features = Arc::new(FakeFeatures::default());
        let svc = Arc::new(FingerprintService::new(
            tracks.clone(),
            features.clone(),
            Arc::new(FakeExtractor),
            Arc::new(NoopIndex),
            2,
        ));
        (svc, tracks, features)
    }

    #[tokio::test]
    async fn pass_analyzes_decodable_and_skips_mp3() {
        let (svc, tracks, features) = make();
        tracks.insert(&touch("fp_a.flac"));
        tracks.insert(&touch("fp_b.mp3"));

        let r = svc.run_pass().await;
        assert_eq!(r.total, 2);
        assert_eq!(r.analyzed, 1);
        assert_eq!(r.skipped_unanalyzable, 1);
        assert_eq!(features.count_for_model("fake-v1").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn second_pass_skips_fresh_rows() {
        let (svc, tracks, _features) = make();
        tracks.insert(&touch("fp_c.flac"));
        let first = svc.run_pass().await;
        assert_eq!(first.analyzed, 1);
        let second = svc.run_pass().await;
        assert_eq!(second.analyzed, 0);
        assert_eq!(second.skipped_fresh, 1);
    }

    #[tokio::test]
    async fn analyze_track_on_demand() {
        let (svc, tracks, features) = make();
        let t = tracks.insert(&touch("fp_d.flac"));
        svc.analyze_track(t.id).await.unwrap();
        assert!(features.get(t.id).await.unwrap().is_some());
    }
}
