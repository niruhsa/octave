//! Server entry point.

use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use server::auth::AuthService;
use server::config::Config;
use server::db::{self, pg::PgRepos};
use server::error::{AppError, Result};
use server::rest::RestState;
use server::services::{
    build_discography_provider, build_extractor, build_index, run_optimize_pass, ArtworkService,
    CoverArtArchive, CoverArtSource, DebouncedWarmer, DiscographyCfg, DiscographyService,
    FavoritesService, FcmSender, FingerprintService, ImageOptimizer, IngestService,
    ItunesDirectory, LibraryService, MetadataService, NotificationService, PlaylistRecWarmer,
    PlayHistoryService, PlaylistService, PodcastDirectory, PodcastIndexDirectory, PodcastService,
    PushSender, RecommendationCache, RecommendationService, ScanService, StorageService,
    StreamingService, UploadHub, UploadsService, REC_CACHE_MAX, REC_CACHE_TTL, REC_WARM_DEBOUNCE,
};
use server::db::repo::TrackFeatureRepo;
use server::auth::Identity;
use server::services::organizer::Organizer;
use server::services::watch as ingest_watcher;
use server::{grpc, rest};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Config::from_env()?;
    info!(
        grpc = %config.grpc_addr,
        grpc_tls = config.grpc_tls.is_some(),
        rest = %config.rest_addr,
        rest_tls = config.rest_tls.is_some(),
        admin_ui = config.enable_admin_ui,
        "starting music server"
    );

    let database_url = config.database_url.as_deref().ok_or_else(|| {
        AppError::Config("DATABASE_URL is required (see PLAN.md Phase 1)".into())
    })?;
    let pool = db::connect(database_url).await?;
    db::run_migrations(&pool).await?;
    let repos = PgRepos::new(pool);

    let auth = AuthService::new(
        config.secret_key.clone(),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
    );
    // Optional FCM push backend (Phase 10 — real-time notifications). Built only
    // when FCM_ENABLED; a bad credential path/key is a hard startup error so a
    // misconfigured push setup never boots silently broken.
    let push: Option<Arc<dyn PushSender>> = match &config.fcm {
        Some(cfg) => {
            let sender = FcmSender::from_config(cfg)?;
            info!(project = %cfg.project_id, "FCM push enabled");
            Some(Arc::new(sender))
        }
        None => None,
    };

    // Follows & notifications (Phase 10). Constructed before the library so the
    // library's `create_album` can fan out new-release notifications to
    // followers via this service.
    let notifications = NotificationService::new(
        Arc::new(repos.clone()), // follows
        Arc::new(repos.clone()), // notifications
        Arc::new(repos.clone()), // artists
        Arc::new(repos.clone()), // audit
        Arc::new(repos.clone()), // device_tokens
    )
    .with_push(push);
    let library = LibraryService::new(
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
    )
    .with_library_root(config.library_path.clone())
    .with_primary_language(config.primary_language.clone())
    .with_notifications(notifications.clone());
    // Library-storage accounting (homepage widget + per-entity rollups). Reads
    // the same artwork/library/podcast roots the rest of the system uses.
    let storage = StorageService::new(
        Arc::new(repos.clone()),
        config.library_path.clone(),
        config.artwork_path.clone(),
        config.podcast.as_ref().map(|pc| pc.path.clone()),
    );
    let scan = ScanService::new(library.clone(), config.library_path.clone())
        .with_storage(storage.clone());
    // Ensure the podcast storage dir exists so the streaming path can canonicalize
    // it (episode files live here). Best-effort — created on demand otherwise.
    if let Some(pc) = &config.podcast
        && let Err(e) = std::fs::create_dir_all(&pc.path)
    {
        warn!(path = %pc.path.display(), error = %e, "failed to create PODCAST_PATH");
    }
    let mut streaming = StreamingService::new(Arc::new(repos.clone()), config.library_path.clone());
    if let Some(pc) = &config.podcast {
        streaming = streaming.with_podcasts(Arc::new(repos.clone()), Some(pc.path.clone()));
    }
    let playlists = PlaylistService::new(
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
    );

    // Play history (Phase 11) — records plays + serves recently-played/stats.
    let play_history = PlayHistoryService::new(
        Arc::new(repos.clone()), // play_events
        Arc::new(repos.clone()), // tracks (hydration)
        Arc::new(repos.clone()), // artists (hydration)
    );

    // Favorites (Phase 11) — per-user likes on tracks/albums/artists.
    let favorites = FavoritesService::new(
        Arc::new(repos.clone()), // favorites
        Arc::new(repos.clone()), // tracks (hydration + existence)
        Arc::new(repos.clone()), // albums
        Arc::new(repos.clone()), // artists
        Arc::new(repos.clone()), // audit
    );

    // Recommendations / Discover (Phase 11) — behavioral shelves + radio,
    // derived from play history + favorites + the library graph.
    let discover = RecommendationService::new(
        Arc::new(repos.clone()), // play_history
        Arc::new(repos.clone()), // favorites
        Arc::new(repos.clone()), // tracks
        Arc::new(repos.clone()), // albums
        Arc::new(repos.clone()), // artists
    );

    // Acoustic fingerprinting (Phase 12) — optional "sounds like" radio. When
    // enabled it builds an embedding extractor + an in-memory similarity index,
    // augments `discover` with true acoustic neighbors, and starts a background
    // analysis pass. When disabled, the radio stays purely behavioral (the
    // server boots + behaves exactly as before).
    let (discover, fingerprint) = if let Some(fp_cfg) = config.fingerprint.clone() {
        let features: Arc<dyn TrackFeatureRepo> = Arc::new(repos.clone());
        let extractor = build_extractor(fp_cfg.model_path.as_deref());
        let index = build_index(
            fp_cfg.index_kind,
            features.clone(),
            extractor.model_version(),
            extractor.dims(),
        );
        // Prepare the index once up front so the first request is usable before
        // the background pass finishes: bruteforce loads existing embeddings;
        // pgvector ensures the derived column + HNSW index exist and backfills.
        if let Err(e) = index.reload().await {
            tracing::warn!(error = %e, "fingerprint: initial index reload failed");
        }
        let fp = FingerprintService::new(
            Arc::new(repos.clone()), // tracks
            features,
            extractor,
            index.clone(),
            fp_cfg.concurrency,
        )
        .with_library_root(config.library_path.clone());
        info!(
            model = %fp.model_version(),
            interval_secs = fp_cfg.interval_secs,
            concurrency = fp_cfg.concurrency,
            "acoustic fingerprinting enabled"
        );
        // Background analysis pass: startup + interval (0 = startup-only).
        std::sync::Arc::new(fp.clone()).spawn_poller(fp_cfg.interval_secs);
        (discover.with_similarity(index), Some(fp))
    } else {
        (discover, None)
    };

    // Playlist-recommendation cache + warm-on-change (Phase 3). The cache tag
    // is the active fingerprint model (so re-analysis busts warm pools), or
    // "behavioral" when fingerprinting is off. Attaching the warmer to the
    // playlist service means a membership change schedules a debounced recompute
    // so the pool is warm by open time.
    let rec_cache = std::sync::Arc::new(RecommendationCache::new(REC_CACHE_TTL, REC_CACHE_MAX));
    let rec_model = fingerprint
        .as_ref()
        .map(|fp| fp.model_version().to_string())
        .unwrap_or_else(|| "behavioral".to_string());
    let discover = discover
        .with_rec_cache(rec_cache.clone(), rec_model)
        .with_rec_persistence(Arc::new(repos.clone()));
    let warmer: Arc<dyn PlaylistRecWarmer> = Arc::new(DebouncedWarmer::new(
        Arc::new(discover.clone()),
        Arc::new(repos.clone()),
        REC_WARM_DEBOUNCE,
    ));
    let playlists = playlists.with_warmer(warmer);

    let metadata = MetadataService::new(library.clone(), config.write_tags);
    // Construct the artwork service whenever there's somewhere to cache images
    // (ARTWORK_PATH) OR auto-fetch is enabled. The external CAA source is only
    // wired when FETCH_ARTWORK is on — manual cover/image uploads need only the
    // cache dir, so they work regardless of the auto-fetch toggle.
    let artwork = if config.artwork_path.is_some() || config.fetch_artwork {
        let source: Option<Arc<dyn CoverArtSource>> =
            config.fetch_artwork.then(|| Arc::new(CoverArtArchive::new()) as Arc<dyn CoverArtSource>);
        Some(ArtworkService::new(
            library.clone(),
            source,
            config.artwork_path.clone(),
        ))
    } else {
        None
    };

    // Image optimizer: available whenever there's an artwork cache dir.
    // Serves downscaled cover/artist images, generating them on demand.
    let optimizer = config
        .artwork_path
        .clone()
        .map(|dir| ImageOptimizer::new(dir, config.image_max_dim, config.image_quality));

    let ingest = match config.library_path.clone() {
        Some(ref root) => {
            let organizer = Organizer::new(root.clone());
            Some(
                IngestService::new(
                    scan.clone(),
                    organizer,
                    config.ingest_path.clone(),
                    artwork.clone().map(Arc::new),
                )
                // Analyze newly-uploaded tracks on ingest (Phase 12) so "sounds
                // like" works promptly; no-op when fingerprinting is disabled.
                .with_fingerprint(fingerprint.clone()),
            )
        }
        None => None,
    };

    // Start the ingest-folder watcher (background, non-blocking).
    let _watcher = match &ingest {
        Some(ingest_svc) => Some(ingest_watcher::start(ingest_svc.clone()).map_err(|e| {
            AppError::Internal(format!("ingest watcher: {e}"))
        })?),
        None => None,
    };

    // Uploads v2: shared hub (live broadcast) + DB-backed session service.
    // The service needs an ingest pipeline to stage/organise files, so it is
    // only available when a library/ingest root is configured.
    let upload_hub = UploadHub::new();
    let uploads = ingest.clone().map(|ing| {
        UploadsService::new(Arc::new(repos.clone()), ing, upload_hub.clone())
    });
    // Server-side stall sweeper: autonomously pause an active upload that has
    // received no chunk for ≥1 min, so the server reflects `paused` even when the
    // client can't deliver its own pause (network down / app killed).
    if let Some(up) = &uploads {
        up.spawn_stall_sweeper();
    }

    // Podcasts (optional — gated on PODCAST_PATH/LIBRARY_PATH). The directory is
    // PodcastIndex when keyed (with an iTunes fallback), else iTunes-only. The
    // service shares the new-episode fan-out with `notifications`.
    let podcasts = config.podcast.as_ref().map(|pc| {
        let directory: Arc<dyn PodcastDirectory> = match &pc.podcastindex {
            Some(creds) => Arc::new(PodcastIndexDirectory::new(
                creds.api_key.clone(),
                creds.api_secret.clone(),
                ItunesDirectory::new(),
            )),
            None => Arc::new(ItunesDirectory::new()),
        };
        info!(
            path = %pc.path.display(),
            podcastindex = pc.podcastindex.is_some(),
            refresh_secs = pc.refresh_interval_secs,
            "podcasts enabled"
        );
        PodcastService::new(
            Arc::new(repos.clone()),
            Arc::new(repos.clone()),
            Arc::new(repos.clone()),
            Arc::new(repos.clone()),
            directory,
            pc.path.clone(),
        )
        .with_notifications(notifications.clone())
        .with_auto_download_default(pc.auto_download_default)
        .with_refresh_interval(pc.refresh_interval_secs)
    });
    // Background feed refresh poller (0 disables) — mirrors the optimize pass /
    // upload stall sweeper.
    if let (Some(p), Some(pc)) = (&podcasts, &config.podcast)
        && pc.refresh_interval_secs > 0
    {
        p.spawn_refresh_poller();
    }

    // Image-optimization background work: optimize everything once on startup,
    // then on a timer. New/changed images are also handled on upload + on
    // demand at serve time — this pass is the catch-all (e.g. ingest-created
    // covers) and a self-heal for a wiped optimized cache.
    if let Some(opt) = optimizer.clone() {
        let repos_for_opt = Arc::new(repos.clone());
        // Startup pass (background — never blocks boot).
        {
            let opt = opt.clone();
            let repos_for_opt = repos_for_opt.clone();
            tokio::spawn(async move {
                run_optimize_pass(&*repos_for_opt, &*repos_for_opt, &opt).await;
            });
        }
        // Periodic pass (0 disables).
        if config.image_optimize_interval_secs > 0 {
            let period = std::time::Duration::from_secs(config.image_optimize_interval_secs);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(period);
                tick.tick().await; // consume the immediate first tick (startup pass ran)
                loop {
                    tick.tick().await;
                    run_optimize_pass(&*repos_for_opt, &*repos_for_opt, &opt).await;
                }
            });
        }
    }

    // Library-storage background job: a one-shot startup recompute (so the
    // homepage widget has data immediately), then a periodic light refresh
    // (index new files, prune missing, recompute) every STORAGE_REFRESH_SECS
    // (0 disables the periodic pass). Gated on a configured library path —
    // there's nothing to account for without one.
    if config.library_path.is_some() {
        storage.spawn_refresh_job(
            scan.clone(),
            Identity::SecretKey,
            config.storage_refresh_interval_secs,
        );
    }

    // Discography sync (Phase 14) — optional external metadata reconciliation.
    // Manager-gated; when disabled the endpoints report `enabled = false`.
    let discography = config.discography.as_ref().map(|dc| {
        let provider = build_discography_provider(dc);
        info!(
            provider = %dc.provider,
            interval_secs = dc.sync_interval_secs,
            "discography sync enabled"
        );
        DiscographyService::new(
            Arc::new(repos.clone()), // artists
            Arc::new(repos.clone()), // albums
            Arc::new(repos.clone()), // tracks
            Arc::new(repos.clone()), // aliases (album/track spellings for matching)
            Arc::new(repos.clone()), // discography state / reports / ignores
            provider,
            DiscographyCfg::from(dc),
        )
    });
    // Background sync-all poller (0 = manual only; no startup pass so we never
    // hammer the provider at ~1 req/s on boot).
    if let (Some(d), Some(dc)) = (&discography, &config.discography) {
        Arc::new(d.clone()).spawn_poller(dc.sync_interval_secs);
    }

    // One shutdown signal fans out to both transports and the live streams.
    // A dedicated listener flips it on the first SIGINT/SIGTERM.
    let (shutdown_tx, shutdown_rx) = server::shutdown::channel();
    tokio::spawn(async move {
        server::shutdown::wait_for_signal().await;
        info!("shutdown signal received; draining transports");
        let _ = shutdown_tx.send(true);
    });

    let rest_state = RestState {
        auth: auth.clone(),
        library: library.clone(),
        scan: scan.clone(),
        storage: storage.clone(),
        streaming,
        playlists: playlists.clone(),
        notifications: notifications.clone(),
        play_history: play_history.clone(),
        favorites: favorites.clone(),
        discover: discover.clone(),
        fingerprint: fingerprint.clone(),
        discography: discography.clone(),
        podcasts: podcasts.clone(),
        ingest: ingest.clone(),
        metadata: metadata.clone(),
        artwork: artwork.clone(),
        optimizer: optimizer.clone(),
        uploads: uploads.clone(),
        upload_hub: upload_hub.clone(),
        shutdown: shutdown_rx.clone(),
    };

    let mut grpc_task = tokio::spawn(grpc::serve(
        config.grpc_addr,
        config.grpc_tls.clone(),
        auth.clone(),
        library,
        scan,
        storage,
        metadata,
        artwork,
        playlists,
        notifications,
        play_history,
        favorites,
        discover,
        fingerprint,
        discography,
        podcasts,
        ingest,
        uploads,
        upload_hub,
        shutdown_rx.clone(),
    ));
    let mut rest_task = tokio::spawn(rest::serve(config.rest_addr, config.rest_tls.clone(), rest_state));

    // Run until a transport exits on its own (bind error / panic) or the
    // shutdown signal fans out to both. If one transport dies, stop the other.
    tokio::select! {
        res = &mut grpc_task => {
            rest_task.abort();
            return unwrap_join("grpc", res);
        }
        res = &mut rest_task => {
            grpc_task.abort();
            return unwrap_join("rest", res);
        }
        _ = server::shutdown::wait_for_shutdown(shutdown_rx) => {}
    }

    // Shutdown requested: both transports are draining. Bound the wait so a
    // connection that refuses to drain can't wedge the process open (which
    // previously required a force-kill) — abort and exit if it overruns.
    const DRAIN_GRACE: Duration = Duration::from_secs(10);
    let drained = tokio::time::timeout(DRAIN_GRACE, async {
        let _ = tokio::join!(&mut grpc_task, &mut rest_task);
    })
    .await;
    if drained.is_err() {
        warn!(
            secs = DRAIN_GRACE.as_secs(),
            "transports did not drain in time; forcing shutdown"
        );
        grpc_task.abort();
        rest_task.abort();
    }

    info!("music server shut down cleanly");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn unwrap_join(
    name: &str,
    res: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    match res {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            error!(transport = name, error = %e, "transport exited with error");
            Err(e)
        }
        Err(join_err) => {
            error!(transport = name, error = %join_err, "transport task panicked");
            Err(AppError::Internal(format!(
                "{name} task join error: {join_err}"
            )))
        }
    }
}
