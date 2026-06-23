//! Phase 2 unit-ish tests: secure store round-trip + transport config
//! parsing. End-to-end auth against a live server is exercised manually
//! against the running gRPC/REST instance.
//!
//! We deliberately use `FileStore` here even on desktop — testing the
//! keychain in CI would require an unlocked login session, and the file
//! store is the same trait surface.

use music_app_lib::auth::store::{
    FileStore, SecureStore, StoredCredential, StoredCredentialKind,
};
use music_app_lib::transport::{PermissionTier, ServerConfig};

fn cred_bearer(token: &str) -> StoredCredential {
    StoredCredential {
        kind: StoredCredentialKind::Bearer,
        secret: token.into(),
        rest_url: None,
        grpc_url: None,
        grpc_explicit: None,
        user_id: Some("aaaa-bbbb".into()),
        username: Some("dr".into()),
        tier: Some(PermissionTier::Manager),
        expires_at: Some("2026-12-31T00:00:00Z".into()),
    }
}

fn cred_secret_key(key: &str) -> StoredCredential {
    StoredCredential {
        kind: StoredCredentialKind::SecretKey,
        secret: key.into(),
        rest_url: None,
        grpc_url: None,
        grpc_explicit: None,
        user_id: None,
        username: None,
        tier: Some(PermissionTier::Admin),
        expires_at: None,
    }
}

#[tokio::test]
async fn file_store_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let store = FileStore::new(tmp.path().join("cred.json"));

    assert!(store.load().await.unwrap().is_none(), "empty store reads None");

    let cred = cred_bearer("opaque-token-xyz");
    store.save(&cred).await.unwrap();

    let loaded = store.load().await.unwrap().unwrap();
    assert_eq!(loaded.kind, StoredCredentialKind::Bearer);
    assert_eq!(loaded.secret, "opaque-token-xyz");

    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none(), "clear wipes");
}

#[tokio::test]
async fn secret_key_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let store = FileStore::new(tmp.path().join("cred.json"));
    let cred = cred_secret_key("pre-shared-secret");
    store.save(&cred).await.unwrap();

    let loaded = store.load().await.unwrap().unwrap();
    assert_eq!(loaded.kind, StoredCredentialKind::SecretKey);
    assert_eq!(loaded.tier, Some(PermissionTier::Admin));
    assert!(loaded.user_id.is_none());
}

#[test]
fn server_config_rejects_missing_scheme() {
    assert!(ServerConfig::new("localhost:8080", "localhost:50051").is_err());
}

#[test]
fn server_config_parses_valid_urls() {
    let c = ServerConfig::new("http://localhost:8080", "http://localhost:50051").unwrap();
    assert_eq!(c.rest_root(), "http://localhost:8080");
    assert_eq!(c.grpc_endpoint(), "http://localhost:50051");
}

#[test]
fn server_config_strips_trailing_slash() {
    let c = ServerConfig::new("http://host:8080/", "http://host:50051/").unwrap();
    assert_eq!(c.rest_root(), "http://host:8080");
    assert_eq!(c.grpc_endpoint(), "http://host:50051");
}

#[test]
fn server_config_derives_grpc_port_from_rest() {
    let c = ServerConfig::from_rest_only("http://localhost:8080").unwrap();
    assert_eq!(c.rest_root(), "http://localhost:8080");
    assert_eq!(c.grpc_endpoint(), "http://localhost:50051");
}

#[test]
fn permission_tier_from_proto() {
    use music_app_lib::transport::PermissionTier;
    assert_eq!(PermissionTier::from_proto(3), PermissionTier::Admin);
    assert_eq!(PermissionTier::from_proto(2), PermissionTier::Manager);
    assert_eq!(PermissionTier::from_proto(1), PermissionTier::User);
    assert_eq!(PermissionTier::from_proto(0), PermissionTier::User); // unknown
    assert_eq!(PermissionTier::from_proto(99), PermissionTier::User);
}
