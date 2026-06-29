//! gRPC transport (primary).
//!
//! Mounts auth + library services behind the `AuthInterceptor`. Health is
//! intentionally unauthenticated.

pub mod auth_svc;
pub mod discover_svc;
pub mod favorite_svc;
pub mod interceptor;
pub mod library_svc;
pub mod notification_svc;
pub mod playhistory_svc;
pub mod playlist_svc;
pub mod podcast_svc;
pub mod proto;
pub mod upload_svc;

use std::net::SocketAddr;

use tonic::transport::{Identity, Server, ServerTlsConfig};
use tracing::info;

use crate::auth::service::AuthService;
use crate::config::TlsConfig;
use crate::error::{AppError, Result};
use crate::shutdown::{wait_for_shutdown, ShutdownRx};
use crate::services::{
    ArtworkService, FavoritesService, IngestService, LibraryService, MetadataService,
    NotificationService, PlayHistoryService, PlaylistService, PodcastService, RecommendationService,
    ScanService, StorageService, UploadHub, UploadsService,
};

pub use auth_svc::AuthServer;
pub use discover_svc::DiscoverServer;
pub use favorite_svc::FavoriteServer;
pub use interceptor::AuthInterceptor;
pub use library_svc::LibraryServer;
pub use notification_svc::NotificationServer;
pub use playhistory_svc::PlayHistoryServer;
pub use playlist_svc::PlaylistServer;
pub use podcast_svc::PodcastServer;
pub use upload_svc::UploadServer;

/// Build + run the gRPC server until shutdown.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    addr: SocketAddr,
    tls: Option<TlsConfig>,
    auth: AuthService,
    library: LibraryService,
    scan: ScanService,
    storage: StorageService,
    metadata: MetadataService,
    artwork: Option<ArtworkService>,
    playlists: PlaylistService,
    notifications: NotificationService,
    play_history: PlayHistoryService,
    favorites: FavoritesService,
    discover: RecommendationService,
    podcasts: Option<PodcastService>,
    ingest: Option<IngestService>,
    uploads: Option<UploadsService>,
    upload_hub: UploadHub,
    shutdown: ShutdownRx,
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
    health_reporter
        .set_serving::<proto::upload::upload_service_server::UploadServiceServer<UploadServer>>()
        .await;
    health_reporter
        .set_serving::<proto::notification::notification_service_server::NotificationServiceServer<NotificationServer>>()
        .await;
    health_reporter
        .set_serving::<proto::podcast::podcast_service_server::PodcastServiceServer<PodcastServer>>()
        .await;
    health_reporter
        .set_serving::<proto::playhistory::play_history_service_server::PlayHistoryServiceServer<PlayHistoryServer>>()
        .await;
    health_reporter
        .set_serving::<proto::favorite::favorite_service_server::FavoriteServiceServer<FavoriteServer>>()
        .await;
    health_reporter
        .set_serving::<proto::discover::discover_service_server::DiscoverServiceServer<DiscoverServer>>()
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
        storage,
        metadata,
        artwork,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let playlist_server = PlaylistServer {
        playlists,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let notification_server = NotificationServer {
        notifications,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let podcast_server = PodcastServer {
        podcasts,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let play_history_server = PlayHistoryServer {
        plays: play_history,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let favorite_server = FavoriteServer {
        favorites,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let discover_server = DiscoverServer {
        discover,
        interceptor: interceptor.clone(),
    }
    .into_service();
    let upload_server = UploadServer {
        ingest,
        uploads,
        hub: upload_hub,
        interceptor,
        shutdown: shutdown.clone(),
    }
    .into_service();

    let mut builder = Server::builder();
    if let Some(tls) = &tls {
        builder = builder
            .tls_config(server_tls_config(tls)?)
            .map_err(|e| AppError::Config(format!("invalid gRPC TLS config: {e}")))?;
        info!(%addr, "gRPC server listening (TLS enabled, HTTP/2 over ALPN)");
    } else {
        info!(%addr, "gRPC server listening (plaintext h2c)");
    }

    let shutdown_signal = async move {
        wait_for_shutdown(shutdown).await;
        info!("gRPC server received shutdown signal");
    };

    builder
        .add_service(health_service)
        .add_service(auth_server)
        .add_service(library_server)
        .add_service(playlist_server)
        .add_service(notification_server)
        .add_service(podcast_server)
        .add_service(play_history_server)
        .add_service(favorite_server)
        .add_service(discover_server)
        .add_service(upload_server)
        .serve_with_shutdown(addr, shutdown_signal)
        .await
        .map_err(|e| AppError::Internal(format!("gRPC server error: {e}")))?;

    Ok(())
}

/// Build the gRPC server TLS config from the configured PEM paths.
///
/// Reads the cert + key at call time so a bad path fails fast (and loudly,
/// never silently downgrading to plaintext). tonic then serves gRPC over
/// HTTP/2 only and, on the TLS handshake, advertises `h2` via ALPN
/// automatically — so the endpoint satisfies the gRPC-over-TLS contract
/// (TLS + HTTP/2, `h2` ALPN, `application/grpc` content-type).
fn server_tls_config(tls: &TlsConfig) -> Result<ServerTlsConfig> {
    let cert = std::fs::read(&tls.cert_path).map_err(|e| {
        AppError::Config(format!("read GRPC_TLS_CERT {}: {e}", tls.cert_path.display()))
    })?;
    let key = std::fs::read(&tls.key_path).map_err(|e| {
        AppError::Config(format!("read GRPC_TLS_KEY {}: {e}", tls.key_path.display()))
    })?;
    Ok(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))
}

#[cfg(test)]
mod tls_tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tls")
            .join(name)
    }

    /// End-to-end proof of the gRPC-over-TLS requirements: a real gRPC health
    /// `Check` succeeds over a TLS channel built from our `server_tls_config`.
    /// That single success covers all three:
    ///   1. **TLS + HTTP/2** — the call rides a TLS-wrapped HTTP/2 connection;
    ///   2. **`h2` over ALPN** — tonic's client *requires* the server to
    ///      negotiate `h2` via ALPN or the handshake fails (it isn't
    ///      `assume_http2`), so completing the RPC proves it was advertised;
    ///   3. **`application/grpc`** — gRPC framing wouldn't decode otherwise.
    #[tokio::test]
    async fn grpc_over_tls_negotiates_h2_and_serves() {
        let tls = TlsConfig {
            cert_path: fixture("cert.pem"),
            key_path: fixture("key.pem"),
        };
        let server_cfg = server_tls_config(&tls).expect("build server TLS config");

        // Reserve an ephemeral loopback port, then serve a health-only gRPC
        // server (the overall "" service defaults to SERVING) over TLS on it.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let (_reporter, health_service) = tonic_health::server::health_reporter();
        let server = tokio::spawn(async move {
            Server::builder()
                .tls_config(server_cfg)
                .unwrap()
                .add_service(health_service)
                .serve(addr)
                .await
                .unwrap();
        });

        // Client trusts the self-signed fixture as its CA and validates the
        // cert against its `localhost` SAN.
        let ca = std::fs::read(fixture("cert.pem")).unwrap();
        let client_tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca))
            .domain_name("localhost");
        let endpoint = Endpoint::from_shared(format!("https://127.0.0.1:{}", addr.port()))
            .unwrap()
            .tls_config(client_tls)
            .unwrap()
            .connect_timeout(Duration::from_secs(5));

        // Retry until the spawned server is accepting.
        let mut channel = None;
        let mut last_err = String::new();
        for _ in 0..50 {
            match endpoint.connect().await {
                Ok(c) => {
                    channel = Some(c);
                    break;
                }
                Err(e) => last_err = format!("{e:?}"),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let channel = channel.unwrap_or_else(|| panic!("connect over TLS failed: {last_err}"));

        let resp = HealthClient::new(channel)
            .check(HealthCheckRequest {
                service: String::new(),
            })
            .await
            .expect("gRPC health Check over TLS")
            .into_inner();
        assert_eq!(resp.status, ServingStatus::Serving as i32);

        server.abort();
    }
}
