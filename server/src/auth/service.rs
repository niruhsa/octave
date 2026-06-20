//! Auth service: register, login, resolve credentials → [`Identity`].
//!
//! Composes the `UserRepo` + `SessionRepo` repositories with password hashing
//! and opaque token generation. The transport layer (gRPC interceptor / axum
//! middleware) calls [`AuthService::resolve`] on every incoming request.

use std::sync::Arc;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::identity::Identity;
use crate::auth::{password, token};
use crate::db::models::{NewSession, NewUser, PermissionLevel};
use crate::db::repo::{SessionRepo, UserRepo};
use crate::error::{AppError, Result};

/// Session lifetime issued on login.
const SESSION_TTL_DAYS: i64 = 30;

/// Parsed credential extracted from the transport layer.
#[derive(Debug, Clone)]
pub enum Credential {
    /// `Authorization: SecretKey <key>` (or `X-Secret-Key: <key>`).
    SecretKey(String),
    /// `Authorization: Bearer <token>` issued by [`AuthService::login`].
    Bearer(String),
}

/// What `login` returns to the caller.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub token: String,
    pub user_id: Uuid,
    pub level: PermissionLevel,
    pub expires_at: OffsetDateTime,
}

/// Auth service. Cheap to clone (just wraps `Arc`s).
#[derive(Clone)]
pub struct AuthService {
    secret_key: Arc<String>,
    users: Arc<dyn UserRepo>,
    sessions: Arc<dyn SessionRepo>,
}

impl AuthService {
    pub fn new(
        secret_key: String,
        users: Arc<dyn UserRepo>,
        sessions: Arc<dyn SessionRepo>,
    ) -> Self {
        Self {
            secret_key: Arc::new(secret_key),
            users,
            sessions,
        }
    }

    /// Resolve a credential to an [`Identity`], or fail with
    /// [`AppError::Unauthenticated`].
    pub async fn resolve(&self, cred: Credential) -> Result<Identity> {
        match cred {
            Credential::SecretKey(presented) => {
                if token::constant_time_eq(&presented, self.secret_key.as_str()) {
                    Ok(Identity::SecretKey)
                } else {
                    Err(AppError::Unauthenticated("invalid secret key".into()))
                }
            }
            Credential::Bearer(t) => {
                let session = self
                    .sessions
                    .get(&t)
                    .await?
                    .ok_or_else(|| AppError::Unauthenticated("unknown token".into()))?;

                if session.revoked_at.is_some() {
                    return Err(AppError::Unauthenticated("token revoked".into()));
                }
                if session.expires_at < OffsetDateTime::now_utc() {
                    return Err(AppError::Unauthenticated("token expired".into()));
                }

                let user = self
                    .users
                    .get(session.user_id)
                    .await?
                    .ok_or_else(|| AppError::Unauthenticated("user not found".into()))?;

                Ok(Identity::User {
                    id: user.id,
                    username: user.username,
                    level: user.permission_level,
                })
            }
        }
    }

    /// Register a new account. Authorization is the **caller's responsibility**
    /// — only `SECRET_KEY` or an existing Admin should be able to invoke this.
    /// `caller.require(PermissionLevel::Admin)` must be checked upstream.
    pub async fn register(
        &self,
        caller: &Identity,
        username: &str,
        password: &str,
        level: PermissionLevel,
    ) -> Result<Uuid> {
        // Defense in depth: re-check here even though the transport gated it.
        caller.require(PermissionLevel::Admin)?;

        if username.is_empty() || password.len() < 8 {
            return Err(AppError::InvalidArgument(
                "username required; password must be \u{2265} 8 chars".into(),
            ));
        }
        if self.users.find_by_username(username).await?.is_some() {
            return Err(AppError::InvalidArgument("username already exists".into()));
        }

        let hash = password::hash(password)?;
        let user = self
            .users
            .create(NewUser {
                username: username.to_string(),
                password_hash: hash,
                permission_level: level,
            })
            .await?;
        Ok(user.id)
    }

    /// Verify username/password and issue a session token.
    pub async fn login(&self, username: &str, password: &str) -> Result<LoginOutcome> {
        let user = self
            .users
            .find_by_username(username)
            .await?
            .ok_or_else(|| AppError::Unauthenticated("invalid credentials".into()))?;

        if !password::verify(password, &user.password_hash)? {
            return Err(AppError::Unauthenticated("invalid credentials".into()));
        }

        let token = token::generate();
        let expires_at = OffsetDateTime::now_utc() + Duration::days(SESSION_TTL_DAYS);
        let session = self
            .sessions
            .create(NewSession {
                token: token.clone(),
                user_id: user.id,
                expires_at,
            })
            .await?;

        Ok(LoginOutcome {
            token: session.token,
            user_id: user.id,
            level: user.permission_level,
            expires_at: session.expires_at,
        })
    }

    /// Revoke a previously-issued session token.
    pub async fn logout(&self, bearer: &str) -> Result<()> {
        self.sessions.revoke(bearer).await
    }
}
