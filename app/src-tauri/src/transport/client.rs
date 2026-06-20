//! `ServerClient` — the single surface the rest of the app talks to.
//!
//! gRPC primary, REST fallback. On a transport-level failure (channel
//! refused, codec, timeout — anything that smells like "the gRPC layer
//! itself isn't working"), we fall back to REST for the same call. We do
//! NOT fall back on auth errors (`Unauthenticated`/`Forbidden`) — those are
//! the server speaking, and the answer is the same on both transports.

use serde::{Deserialize, Serialize};

use super::grpc::GrpcClient;
use super::rest::RestClient;
use super::{Album, Artist, Credential, PermissionTier, ServerConfig, Track};
use crate::error::{AppError, AppResult};

/// Aggregate client. Cheap to clone-by-`Arc` once placed in Tauri state.
pub struct ServerClient {
    config: ServerConfig,
    rest: RestClient,
}

impl ServerClient {
    pub fn new(config: ServerConfig) -> AppResult<Self> {
        let rest = RestClient::new(&config)?;
        Ok(Self { config, rest })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Username/password login. Tries gRPC first; falls back to REST on
    /// any non-auth transport failure.
    pub async fn login(&self, username: &str, password: &str) -> AppResult<LoginOutcome> {
        let grpc_attempt = try_grpc(&self.config).await;
        if let Ok(client) = &grpc_attempt {
            match client.login(username, password).await {
                Ok(o) => {
                    return Ok(LoginOutcome {
                        token: o.token,
                        user_id: o.user_id,
                        tier: o.tier,
                        expires_at: o.expires_at,
                        transport: TransportUsed::Grpc,
                    });
                }
                Err(e) if is_transport_error(&e) => {
                    tracing::info!(err = %e, "gRPC login unavailable; falling back to REST");
                }
                Err(e) => return Err(e),
            }
        } else if let Err(e) = &grpc_attempt {
            tracing::info!(err = %e, "gRPC connect failed; falling back to REST");
        }
        let o = self.rest.login(username, password).await?;
        Ok(LoginOutcome {
            token: o.token,
            user_id: o.user_id,
            tier: o.tier,
            expires_at: o.expires_at,
            transport: TransportUsed::Rest,
        })
    }

    /// Resolve a credential to a server identity (and therefore tier).
    pub async fn whoami(&self, cred: &Credential) -> AppResult<WhoAmI> {
        if let Ok(client) = try_grpc(&self.config).await {
            match client.whoami(cred).await {
                Ok(w) => {
                    return Ok(WhoAmI {
                        kind: w.kind,
                        user_id: w.user_id,
                        username: w.username,
                        tier: w.tier,
                        transport: TransportUsed::Grpc,
                    });
                }
                Err(e) if is_transport_error(&e) => {
                    tracing::info!(err = %e, "gRPC whoami unavailable; falling back to REST");
                }
                Err(e) => return Err(e),
            }
        }
        let w = self.rest.whoami(cred).await?;
        Ok(WhoAmI {
            kind: w.kind,
            user_id: w.user_id,
            username: w.username,
            tier: w.tier,
            transport: TransportUsed::Rest,
        })
    }

    /// Revoke a session.
    pub async fn logout(&self, cred: &Credential) -> AppResult<()> {
        if let Ok(client) = try_grpc(&self.config).await {
            match client.logout(cred).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => {
                    tracing::info!(err = %e, "gRPC logout unavailable; falling back to REST");
                }
                Err(e) => return Err(e),
            }
        }
        self.rest.logout(cred).await
    }

    /// Liveness probe used by the online/offline detector. We only use REST
    /// `/health` here — the server exposes it cheaply and tonic-health
    /// would require a separate proto wiring for marginal benefit.
    pub async fn health(&self) -> AppResult<bool> {
        self.rest.health().await
    }

    // ----- Library reads -------------------------------------------------

    pub async fn list_artists(
        &self,
        cred: &Credential,
        limit: i64,
        offset: i64,
    ) -> AppResult<(Vec<Artist>, i64)> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_artists(cred, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_artists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_artists(cred, limit, offset).await
    }

    pub async fn search_artists(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Artist>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_artists(cred, query, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_artists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_artists(cred, query, limit, offset).await
    }

    pub async fn list_albums_by_artist(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Vec<Album>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_albums_by_artist(cred, artist_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_albums_by_artist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_albums_by_artist(cred, artist_id).await
    }

    pub async fn search_albums(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Album>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_albums(cred, query, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_albums", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_albums(cred, query, limit, offset).await
    }

    pub async fn list_tracks_by_album(
        &self,
        cred: &Credential,
        album_id: &str,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_tracks_by_album(cred, album_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_tracks_by_album", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_tracks_by_album(cred, album_id).await
    }

    pub async fn search_tracks(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_tracks(cred, query, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_tracks", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_tracks(cred, query, limit, offset).await
    }

    /// Open a gRPC channel for one logical operation. Returns `None` (not
    /// `Err`) when the channel can't be opened so the call sites can just
    /// fall through to REST without nested matches.
    async fn try_grpc(&self) -> Option<GrpcClient> {
        match GrpcClient::connect(&self.config).await {
            Ok(c) => Some(c),
            Err(e) => {
                fallback_log("connect", &e);
                None
            }
        }
    }
}

fn fallback_log(op: &str, err: &AppError) {
    tracing::info!(op, err = %err, "gRPC unavailable; falling back to REST");
}

async fn try_grpc(config: &ServerConfig) -> AppResult<GrpcClient> {
    GrpcClient::connect(config).await
}

fn is_transport_error(err: &AppError) -> bool {
    matches!(err, AppError::Transport(_))
}

/// Which transport actually serviced a successful call. Surfaced to the UI
/// for diagnostics; not part of the auth contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportUsed {
    Grpc,
    Rest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginOutcome {
    pub token: String,
    pub user_id: String,
    pub tier: PermissionTier,
    pub expires_at: String,
    pub transport: TransportUsed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoAmI {
    pub kind: String,
    pub user_id: String,
    pub username: String,
    pub tier: PermissionTier,
    pub transport: TransportUsed,
}
