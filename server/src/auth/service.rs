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
use crate::db::models::{NewAuditEntry, NewSession, NewUser, PermissionLevel};
use crate::db::repo::{AuditRepo, SessionRepo, UserRepo};
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
    audit: Arc<dyn AuditRepo>,
}

impl AuthService {
    pub fn new(
        secret_key: String,
        users: Arc<dyn UserRepo>,
        sessions: Arc<dyn SessionRepo>,
        audit: Arc<dyn AuditRepo>,
    ) -> Self {
        Self {
            secret_key: Arc::new(secret_key),
            users,
            sessions,
            audit,
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

    /// Change a user's password.
    ///
    /// Authorization:
    ///   * `SECRET_KEY` or an Admin caller may reset **any** user's
    ///     password without supplying the old one (admin reset).
    ///   * A non-admin caller may change only **their own** password and
    ///     must supply + verify `old_password`.
    ///   * Any other case → `PermissionDenied`.
    ///
    /// `new_password` must be ≥ 8 chars (same rule as `register`). The new
    /// hash is written via `UserRepo::update_password` and a `user.password_change`
    /// audit entry is recorded (before/after omitted — hashes are
    /// one-way + sensitive, so the audit row carries only the action +
    /// target id for traceability).
    pub async fn change_password(
        &self,
        caller: &Identity,
        target_id: Uuid,
        old_password: Option<&str>,
        new_password: &str,
    ) -> Result<()> {
        let is_admin = caller.level() == PermissionLevel::Admin; // incl. SecretKey
        let is_self = caller.user_id() == Some(target_id);

        if !is_admin && !is_self {
            return Err(AppError::PermissionDenied(
                "can only change your own password (admin/secret-key may reset any)".into(),
            ));
        }

        // Self-change by a non-admin requires the old password.
        if is_self && !is_admin {
            let old = old_password.unwrap_or("");
            let user = self.users.get(target_id).await?.ok_or_else(|| {
                AppError::NotFound("user not found".into())
            })?;
            if !password::verify(old, &user.password_hash)? {
                return Err(AppError::Unauthenticated("old password incorrect".into()));
            }
        }

        if new_password.len() < 8 {
            return Err(AppError::InvalidArgument(
                "new password must be \u{2265} 8 chars".into(),
            ));
        }

        // Confirm the target exists (so an admin reset of a bad id is a 404,
        // not a silent no-op).
        if self.users.get(target_id).await?.is_none() {
            return Err(AppError::NotFound("user not found".into()));
        }

        let hash = password::hash(new_password)?;
        self.users.update_password(target_id, &hash).await?;

        // Audit. No before/after payload — password hashes are sensitive
        // and not rollback-able (one-way). The row still records who reset
        // whose password and when, which is the useful trace.
        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: "user.password_change".to_string(),
                entity_type: "user".to_string(),
                entity_id: Some(target_id),
                before_json: None,
                after_json: None,
            })
            .await?;
        Ok(())
    }

    /// List every registered user. Admin-gated (caller must be Admin or
    /// `SECRET_KEY`); returns each user's id, username, and tier — no
    /// password hashes. Clients use this to populate a dropdown when an
    /// admin resets a password.
    pub async fn list_users(&self, caller: &Identity) -> Result<Vec<UserInfo>> {
        caller.require(PermissionLevel::Admin)?;
        let rows = self.users.list().await?;
        Ok(rows
            .into_iter()
            .map(|u| UserInfo {
                id: u.id,
                username: u.username,
                level: u.permission_level,
            })
            .collect())
    }

    /// Delete a user account. Admin-gated (caller must be Admin or
    /// `SECRET_KEY`). Cascade: the user's sessions, playlists, and
    /// follows are dropped by the DB's `ON DELETE CASCADE`; audit-log
    /// rows pointing at this user get `actor_id = NULL`. A
    /// `user.delete` audit entry is recorded with the user's info
    /// (username + level, no hash) in `before_json` so the deletion is
    /// traceable.
    pub async fn delete_user(&self, caller: &Identity, target_id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::Admin)?;

        let user = self.users.get(target_id).await?.ok_or_else(|| {
            AppError::NotFound("user not found".into())
        })?;

        // Capture before-image for audit before the row is destroyed.
        let before = serde_json::json!({
            "username": user.username,
            "level": user.permission_level,
        })
        .to_string();

        self.users.delete(target_id).await?;

        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: "user.delete".to_string(),
                entity_type: "user".to_string(),
                entity_id: Some(target_id),
                before_json: Some(before),
                after_json: None,
            })
            .await?;
        Ok(())
    }

    /// Revoke a previously-issued session token.
    pub async fn logout(&self, bearer: &str) -> Result<()> {
        self.sessions.revoke(bearer).await
    }
}

/// Public-safe user summary — what `list_users` returns. Deliberately does
/// NOT carry the `password_hash`.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub level: PermissionLevel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{AuditEntry, NewAuditEntry, Session, User};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use time::OffsetDateTime;

    // ---- minimal fakes ----

    #[derive(Default)]
    struct FakeUsers {
        users: Mutex<HashMap<Uuid, User>>,
    }

    #[async_trait]
    impl UserRepo for FakeUsers {
        async fn create(&self, new: NewUser) -> Result<User> {
            let id = Uuid::new_v4();
            let now = OffsetDateTime::now_utc();
            let u = User {
                id,
                username: new.username,
                password_hash: new.password_hash,
                permission_level: new.permission_level,
                created_at: now,
                updated_at: now,
            };
            self.users.lock().unwrap().insert(id, u.clone());
            Ok(u)
        }
        async fn get(&self, id: Uuid) -> Result<Option<User>> {
            Ok(self.users.lock().unwrap().get(&id).cloned())
        }
        async fn find_by_username(&self, _: &str) -> Result<Option<User>> {
            Ok(None)
        }
        async fn update_permission(&self, _: Uuid, _: PermissionLevel) -> Result<()> {
            Ok(())
        }
        async fn update_password(&self, id: Uuid, hash: &str) -> Result<()> {
            let mut g = self.users.lock().unwrap();
            if let Some(u) = g.get_mut(&id) {
                u.password_hash = hash.to_string();
            }
            Ok(())
        }
        async fn list(&self) -> Result<Vec<User>> {
            Ok(self.users.lock().unwrap().values().cloned().collect())
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeSessions;
    #[async_trait]
    impl SessionRepo for FakeSessions {
        async fn create(&self, new: NewSession) -> Result<Session> {
            Ok(Session {
                token: new.token,
                user_id: new.user_id,
                created_at: OffsetDateTime::now_utc(),
                expires_at: new.expires_at,
                revoked_at: None,
            })
        }
        async fn get(&self, _: &str) -> Result<Option<Session>> {
            Ok(None)
        }
        async fn revoke(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeAudit {
        entries: Mutex<Vec<NewAuditEntry>>,
    }
    #[async_trait]
    impl AuditRepo for FakeAudit {
        async fn record(&self, e: NewAuditEntry) -> Result<AuditEntry> {
            let row = AuditEntry {
                id: Uuid::new_v4(),
                actor_id: e.actor_id,
                action: e.action.clone(),
                entity_type: e.entity_type.clone(),
                entity_id: e.entity_id,
                before_json: e.before_json.clone(),
                after_json: e.after_json.clone(),
                created_at: OffsetDateTime::now_utc(),
            };
            self.entries.lock().unwrap().push(e);
            Ok(row)
        }
        async fn list_for_entity(&self, _: &str, _: Uuid) -> Result<Vec<AuditEntry>> {
            Ok(vec![])
        }
    }

    fn svc() -> (AuthService, Arc<FakeUsers>, Arc<FakeAudit>) {
        let users = Arc::new(FakeUsers::default());
        let audit = Arc::new(FakeAudit::default());
        let svc = AuthService::new(
            "secret".into(),
            users.clone(),
            Arc::new(FakeSessions),
            audit.clone(),
        );
        (svc, users, audit)
    }

    async fn make_user(users: &FakeUsers, level: PermissionLevel, pw: &str) -> Uuid {
        let hash = password::hash(pw).unwrap();
        users
            .create(NewUser {
                username: "u".into(),
                password_hash: hash,
                permission_level: level,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn self_change_requires_correct_old_password() {
        let (svc, users, _) = svc();
        let uid = make_user(&users, PermissionLevel::User, "oldpass12").await;
        let caller = Identity::User {
            id: uid,
            username: "u".into(),
            level: PermissionLevel::User,
        };
        // Wrong old → rejected.
        let err = svc
            .change_password(&caller, uid, Some("wrong"), "newpass12")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthenticated(_)));
        // Right old → ok.
        svc.change_password(&caller, uid, Some("oldpass12"), "newpass12")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn non_admin_cannot_change_others_password() {
        let (svc, users, _) = svc();
        let a = make_user(&users, PermissionLevel::User, "passaaaa1").await;
        let b = make_user(&users, PermissionLevel::User, "passbbbb1").await;
        let caller = Identity::User {
            id: a,
            username: "a".into(),
            level: PermissionLevel::User,
        };
        let err = svc
            .change_password(&caller, b, Some("passbbbb1"), "newpass12")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn admin_resets_any_user_without_old_password() {
        let (svc, users, audit) = svc();
        let admin = make_user(&users, PermissionLevel::Admin, "adminpass").await;
        let target = make_user(&users, PermissionLevel::User, "userpass1").await;
        let caller = Identity::User {
            id: admin,
            username: "admin".into(),
            level: PermissionLevel::Admin,
        };
        // No old_password supplied.
        svc.change_password(&caller, target, None, "brand-new-pw")
            .await
            .unwrap();
        // New password verifies, old doesn't.
        let u = users.get(target).await.unwrap().unwrap();
        assert!(password::verify("brand-new-pw", &u.password_hash).unwrap());
        assert!(!password::verify("userpass1", &u.password_hash).unwrap());
        // Audit recorded.
        let entries = audit.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "user.password_change");
        assert_eq!(entries[0].entity_id, Some(target));
    }

    #[tokio::test]
    async fn secret_key_resets_any_user_without_old_password() {
        let (svc, users, _) = svc();
        let target = make_user(&users, PermissionLevel::User, "userpass1").await;
        svc.change_password(&Identity::SecretKey, target, None, "brand-new-pw")
            .await
            .unwrap();
        let u = users.get(target).await.unwrap().unwrap();
        assert!(password::verify("brand-new-pw", &u.password_hash).unwrap());
    }

    #[tokio::test]
    async fn short_new_password_rejected() {
        let (svc, users, _) = svc();
        let uid = make_user(&users, PermissionLevel::User, "oldpass12").await;
        let caller = Identity::User {
            id: uid,
            username: "u".into(),
            level: PermissionLevel::User,
        };
        let err = svc
            .change_password(&caller, uid, Some("oldpass12"), "short")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn admin_reset_of_missing_user_is_not_found() {
        let (svc, _, _) = svc();
        let err = svc
            .change_password(
                &Identity::SecretKey,
                Uuid::new_v4(),
                None,
                "brand-new-pw",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    // ---- delete_user ----

    #[tokio::test]
    async fn non_admin_cannot_delete_user() {
        let (svc, users, _) = svc();
        let uid = make_user(&users, PermissionLevel::User, "pass1").await;
        let caller = Identity::User {
            id: uid,
            username: "u".into(),
            level: PermissionLevel::User,
        };
        let target = make_user(&users, PermissionLevel::User, "pass2").await;
        let err = svc.delete_user(&caller, target).await.unwrap_err();
        assert!(matches!(err, AppError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn admin_deletes_user_and_audits() {
        let (svc, users, audit) = svc();
        let admin = make_user(&users, PermissionLevel::Admin, "apass").await;
        let target = make_user(&users, PermissionLevel::Manager, "mpass").await;
        let caller = Identity::User {
            id: admin,
            username: "admin".into(),
            level: PermissionLevel::Admin,
        };
        svc.delete_user(&caller, target).await.unwrap();
        let entries = audit.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "user.delete");
        assert_eq!(entries[0].entity_id, Some(target));
        assert!(entries[0].before_json.as_ref().unwrap().contains("username"));
    }

    #[tokio::test]
    async fn secret_key_deletes_user() {
        let (svc, users, _) = svc();
        let target = make_user(&users, PermissionLevel::User, "pass1").await;
        svc.delete_user(&Identity::SecretKey, target)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_missing_user_is_not_found() {
        let (svc, _, _) = svc();
        let err = svc
            .delete_user(&Identity::SecretKey, Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }
}
