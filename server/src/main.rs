//! Server entry point.

use std::sync::Arc;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use server::auth::AuthService;
use server::config::Config;
use server::db::{self, pg::PgRepos};
use server::error::{AppError, Result};
use server::rest::RestState;
use server::services::{
    ArtworkService, CoverArtArchive, IngestService, LibraryService, MetadataService,
    PlaylistService, ScanService, StreamingService,
};
use server::services::organizer::Organizer;
use server::services::watch as ingest_watcher;
use server::{grpc, rest};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Config::from_env()?;
    info!(
        grpc = %config.grpc_addr,
        rest = %config.rest_addr,
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
    );
    let library = LibraryService::new(
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
    );
    let scan = ScanService::new(library.clone(), config.library_path.clone());
    let streaming = StreamingService::new(Arc::new(repos.clone()), config.library_path.clone());
    let playlists = PlaylistService::new(
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
        Arc::new(repos.clone()),
    );

    let metadata = MetadataService::new(library.clone(), config.write_tags);
    let artwork = if config.fetch_artwork {
        Some(ArtworkService::new(
            library.clone(),
            Arc::new(CoverArtArchive::new()),
            config.artwork_path.clone(),
        ))
    } else {
        None
    };

    let ingest = match config.library_path.clone() {
        Some(ref root) => {
            let organizer = Organizer::new(root.clone());
            Some(IngestService::new(
                scan.clone(),
                organizer,
                config.ingest_path.clone(),
            ))
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

    let rest_state = RestState {
        auth: auth.clone(),
        library: library.clone(),
        scan: scan.clone(),
        streaming,
        playlists: playlists.clone(),
        ingest: ingest.clone(),
        metadata: metadata.clone(),
        artwork: artwork.clone(),
    };

    let grpc_task = tokio::spawn(grpc::serve(
        config.grpc_addr,
        auth.clone(),
        library,
        scan,
        metadata,
        artwork,
        playlists,
        ingest,
    ));
    let rest_task = tokio::spawn(rest::serve(config.rest_addr, rest_state));

    tokio::select! {
        res = grpc_task => unwrap_join("grpc", res)?,
        res = rest_task => unwrap_join("rest", res)?,
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
