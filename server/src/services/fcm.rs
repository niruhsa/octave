//! Firebase Cloud Messaging push sender (HTTP v1 API) — Phase 10.
//!
//! Real-time delivery of new-release notifications to followers' devices. The
//! HTTP v1 API authenticates with a short-lived OAuth2 access token minted from
//! a Google **service-account** key: we sign a JWT assertion (RS256) with the
//! account's private key, exchange it at the token endpoint, and cache the
//! resulting token until shortly before it expires.
//!
//! The notification service depends on the [`PushSender`] trait (not the
//! concrete client) so it can be unit-tested with a fake and so the whole thing
//! is optional — push is only wired when `FCM_ENABLED`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::FcmConfig;
use crate::error::{AppError, Result};

/// FCM OAuth2 scope.
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

/// Outcome of sending to a single device token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Accepted by FCM for delivery.
    Delivered,
    /// The token is no longer valid (app uninstalled / token rotated) — the
    /// caller should prune it from the DB.
    Unregistered,
}

/// Backend that delivers a push to one device token. Abstracted so the
/// notification service can be tested with a fake and so push stays optional.
#[async_trait]
pub trait PushSender: Send + Sync {
    async fn send(
        &self,
        token: &str,
        title: &str,
        body: &str,
        data: &[(String, String)],
    ) -> Result<PushOutcome>;
}

/// The fields we need from the service-account JSON key.
#[derive(Debug, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Serialize)]
struct JwtClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// FCM HTTP v1 client.
pub struct FcmSender {
    project_id: String,
    account: ServiceAccount,
    encoding_key: jsonwebtoken::EncodingKey,
    http: reqwest::Client,
    cached: Mutex<Option<CachedToken>>,
}

impl FcmSender {
    /// Build from config: read + parse the service-account JSON and prepare the
    /// RS256 signing key. Fails fast (Config error) on a bad path / malformed
    /// key so a misconfigured `FCM_ENABLED` doesn't boot silently broken.
    pub fn from_config(cfg: &FcmConfig) -> Result<Self> {
        let bytes = std::fs::read(&cfg.credentials_path).map_err(|e| {
            AppError::Config(format!(
                "read FCM_CREDENTIALS {}: {e}",
                cfg.credentials_path.display()
            ))
        })?;
        let account: ServiceAccount = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Config(format!("parse FCM service-account JSON: {e}")))?;
        let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .map_err(|e| AppError::Config(format!("FCM service-account private_key: {e}")))?;
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| AppError::Internal(format!("fcm http client: {e}")))?;
        Ok(Self {
            project_id: cfg.project_id.clone(),
            account,
            encoding_key,
            http,
            cached: Mutex::new(None),
        })
    }

    /// A valid OAuth2 access token, minting + caching a fresh one when needed.
    async fn access_token(&self) -> Result<String> {
        // Fast path: a cached token that isn't near expiry. The lock is scoped
        // so the guard never crosses the `.await` below.
        {
            let guard = self.cached.lock().unwrap();
            if let Some(c) = guard.as_ref()
                && c.expires_at > Instant::now()
            {
                return Ok(c.token.clone());
            }
        }

        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let claims = JwtClaims {
            iss: &self.account.client_email,
            scope: FCM_SCOPE,
            aud: &self.account.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        let assertion = jsonwebtoken::encode(&header, &claims, &self.encoding_key)
            .map_err(|e| AppError::Internal(format!("fcm jwt sign: {e}")))?;

        let resp = self
            .http
            .post(&self.account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("fcm token request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Internal(format!(
                "fcm token http {status}: {body}"
            )));
        }
        let tok: TokenResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("fcm token decode: {e}")))?;

        // Cache with a 60s safety margin before the real expiry.
        let ttl = Duration::from_secs(tok.expires_in.max(60) as u64)
            .saturating_sub(Duration::from_secs(60));
        *self.cached.lock().unwrap() = Some(CachedToken {
            token: tok.access_token.clone(),
            expires_at: Instant::now() + ttl,
        });
        Ok(tok.access_token)
    }
}

#[async_trait]
impl PushSender for FcmSender {
    async fn send(
        &self,
        token: &str,
        title: &str,
        body: &str,
        data: &[(String, String)],
    ) -> Result<PushOutcome> {
        let access = self.access_token().await?;
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.project_id
        );
        let data_map: serde_json::Map<String, serde_json::Value> = data
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let payload = serde_json::json!({
            "message": {
                "token": token,
                "notification": { "title": title, "body": body },
                "data": data_map,
                "android": { "priority": "HIGH" }
            }
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&access)
            .json(&payload)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("fcm send: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(PushOutcome::Delivered);
        }
        let text = resp.text().await.unwrap_or_default();
        // A dead token: 404 NOT_FOUND, or an UNREGISTERED error code. Prune it.
        if status.as_u16() == 404
            || text.contains("UNREGISTERED")
            || text.contains("registration-token-not-registered")
        {
            return Ok(PushOutcome::Unregistered);
        }
        Err(AppError::Internal(format!(
            "fcm send http {status}: {text}"
        )))
    }
}
