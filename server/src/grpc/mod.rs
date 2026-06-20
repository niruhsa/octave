//! gRPC transport (primary).
//!
//! Mounts auth + library services behind the `AuthInterceptor`. Health is
//! intentionally unauthenticated.

pub mod auth_svc;
pub mod interceptor;
pub mod library_svc;
pub mod playlist_svc;
pub mod proto;

use std::net::SocketAddr;

use tonic::transport::Server;
use tracing::info;

use crate::auth::service::AuthService;
use crate::error::{AppError, Result};
use crate::services::{LibraryService, PlaylistService, ScanService};

pub use auth_svc::AuthServer;
pub use interceptor::AuthInterceptor;
pub use library_svc::LibraryServer;
pub use playlist_svc::PlaylistServer;

/// Build + run the gRPC server until shutdown.
pub async fn serve(
    addr: SocketAddr,
    auth: AuthService,
    library: LibraryService,
    scan: ScanService,
    playlists: PlaylistService,
) -> Result<()> {
    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<proto::auth::auth_service_server::AuthServiceServer<AuthServer>>()
        .await;
    health_reporter
        .set_serving::<proto::library::library_service_server::LibraryServiceServer<LibraryServer>>(
        )
        .await;
    health_reporter
        .set_serving::<proto::playlist::playlist_service_server::PlaylistServiceServer<PlaylistServer>>()
        .await;

    let interceptor = AuthInterceptor::new(auth.clone());
    let auth_server = AuthServer {
        auth: auth.clone(),
        interceptor: interceptor.clone(),
    }
    .into_service();
    let library_server = LibraryServer {
        library,
        scan,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let playlist_server = PlaylistServer {
        playlists,
        interceptor,
    }
    .into_service();

    info!(%addr, "gRPC server listening");

    Server::builder()
        .add_service(health_service)
        .add_service(auth_server)
        .add_service(library_server)
        .add_service(playlist_server)
        .serve_with_shutdown(addr, shutdown_signal())
        .await
        .map_err(|e| AppError::Internal(format!("gRPC server error: {e}")))?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("gRPC server received shutdown signal");
}
