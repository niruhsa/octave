//! Transport configuration.
//!
//! The server runs gRPC and REST on **separate ports by default** (see
//! `server/src/config.rs`: `GRPC_ADDR=0.0.0.0:50051`, `REST_ADDR=0.0.0.0:8080`).
//! Production deployments often put both behind one reverse-proxy URL, but
//! the dev defaults split them — so we accept both endpoints separately.
//!
//! UX: the frontend asks the user for a REST URL (the obvious one to type
//! — it's the one they can `curl`) and optionally a gRPC URL. If the gRPC
//! URL is omitted we *derive* one from the REST URL by swapping the
//! conventional dev port (`8080` → `50051`) and otherwise reuse the REST
//! host. That covers both `localhost` dev and reverse-proxy prod with
//! minimal config.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Conventional ports we swap between when deriving one URL from the other.
const CONVENTIONAL_REST_PORT: u16 = 8080;
const CONVENTIONAL_GRPC_PORT: u16 = 50051;

/// Where the server lives. Always carries both URLs after construction so
/// downstream code never has to make guesses at call time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub rest_url: String,
    pub grpc_url: String,
    /// `true` when the gRPC URL was supplied by the user, `false` when it was
    /// derived from the REST URL. Persisted so a user's explicit override
    /// survives restarts (and isn't silently re-derived), while a derived URL
    /// keeps tracking the REST URL when that changes.
    #[serde(default)]
    pub grpc_explicit: bool,
}

impl ServerConfig {
    /// Parse explicit REST + gRPC URLs. Both must be http/https with a
    /// host; trailing slashes are stripped.
    pub fn new(rest_url: &str, grpc_url: &str) -> AppResult<Self> {
        Ok(Self {
            rest_url: validate_url(rest_url)?,
            grpc_url: validate_url(grpc_url)?,
            grpc_explicit: true,
        })
    }

    /// Parse a single REST URL and derive a gRPC URL from it. Swaps the
    /// dev REST port (8080) for the dev gRPC port (50051); otherwise hands
    /// the URL through unchanged (matches a reverse-proxy setup).
    pub fn from_rest_only(rest_url: &str) -> AppResult<Self> {
        let rest = validate_url(rest_url)?;
        let grpc = derive_grpc_url(&rest)?;
        Ok(Self {
            rest_url: rest,
            grpc_url: grpc,
            grpc_explicit: false,
        })
    }

    pub fn rest_root(&self) -> &str {
        &self.rest_url
    }

    /// gRPC endpoint string consumable by `tonic::transport::Endpoint`.
    pub fn grpc_endpoint(&self) -> &str {
        &self.grpc_url
    }
}

fn validate_url(raw: &str) -> AppResult<String> {
    let url = url::Url::parse(raw.trim())
        .map_err(|e| AppError::Internal(format!("invalid URL '{raw}': {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Internal(format!(
            "unsupported scheme '{}' in '{raw}': expected http or https",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(AppError::Internal(format!(
            "URL '{raw}' is missing a host"
        )));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn derive_grpc_url(rest: &str) -> AppResult<String> {
    let mut url = url::Url::parse(rest)
        .map_err(|e| AppError::Internal(format!("invalid REST URL '{rest}': {e}")))?;
    // Only swap when the user is on the conventional dev port; otherwise
    // they're behind a reverse proxy and the same URL is correct.
    if url.port() == Some(CONVENTIONAL_REST_PORT) {
        url.set_port(Some(CONVENTIONAL_GRPC_PORT))
            .map_err(|_| AppError::Internal("cannot set port on URL".into()))?;
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_grpc_port_for_dev_default() {
        let cfg = ServerConfig::from_rest_only("http://localhost:8080").unwrap();
        assert_eq!(cfg.rest_root(), "http://localhost:8080");
        assert_eq!(cfg.grpc_endpoint(), "http://localhost:50051");
        assert!(!cfg.grpc_explicit, "a derived gRPC URL is not explicit");
    }

    #[test]
    fn leaves_non_dev_port_alone() {
        let cfg = ServerConfig::from_rest_only("https://music.example.com").unwrap();
        assert_eq!(cfg.rest_root(), "https://music.example.com");
        assert_eq!(cfg.grpc_endpoint(), "https://music.example.com");
        assert!(!cfg.grpc_explicit);
    }

    #[test]
    fn accepts_explicit_pair() {
        let cfg = ServerConfig::new(
            "http://10.0.0.1:9000",
            "http://10.0.0.1:9001",
        )
        .unwrap();
        assert_eq!(cfg.rest_root(), "http://10.0.0.1:9000");
        assert_eq!(cfg.grpc_endpoint(), "http://10.0.0.1:9001");
        assert!(cfg.grpc_explicit, "a user-supplied gRPC URL is explicit");
    }

    #[test]
    fn rejects_garbage() {
        assert!(ServerConfig::from_rest_only("not-a-url").is_err());
        assert!(ServerConfig::from_rest_only("ftp://example.com").is_err());
        assert!(ServerConfig::new("http://a.example", "file:///etc").is_err());
    }
}
