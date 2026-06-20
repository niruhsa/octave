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

#[tokio::test]
async fn file_store_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let store = FileStore::new(tmp.path().join("cred.json"));

    assert!(store.load().await.unwrap().is_none(), "empty store reads None");

    let cred = StoredCredential {
        kind: StoredCredentialKind::Bearer,
        secret: "opaque-token-xyz".into(),
        user_id: Some("aaaa-bbbb".into()),
        username: Some("dr".into()),
        tier: Some(PermissionTier::Manager),
        expires_at: Some("2026-12-31T00:00:00Z".into()),
    };
    store.save(&cred).await.unwrap();

    let loaded = store.load().await.unwrap().unwrap();
    assert_eq!(loaded.kind, StoredCredentialKind::Bearer);
    assert_eq!(loaded.secret, "opaque-token-xyz");
    assert_eq!(loaded.tier, Some(PermissionTier::Manager));
    assert_eq!(loaded.username.as_deref(), Some("dr"));

    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none(), "after clear reads None");

    // Clearing a non-existent store is a no-op, not an error.
    store.clear().await.unwrap();
}

#[tokio::test]
async fn file_store_persists_secret_key_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let store = FileStore::new(tmp.path().join("cred.json"));
    let cred = StoredCredential {
        kind: StoredCredentialKind::SecretKey,
        secret: "shared-key".into(),
        user_id: None,
        username: None,
        tier: Some(PermissionTier::Admin),
        expires_at: None,
    };
    store.save(&cred).await.unwrap();
    let loaded = store.load().await.unwrap().unwrap();
    assert_eq!(loaded.kind, StoredCredentialKind::SecretKey);
    assert_eq!(loaded.tier, Some(PermissionTier::Admin));
    assert!(loaded.expires_at.is_none());
}

#[test]
fn server_config_derives_grpc_port_for_dev_default() {
    // Dev server splits ports: REST 8080, gRPC 50051.
    let cfg = ServerConfig::from_rest_only("http://localhost:8080").unwrap();
    assert_eq!(cfg.rest_root(), "http://localhost:8080");
    assert_eq!(cfg.grpc_endpoint(), "http://localhost:50051");
}

#[test]
fn server_config_leaves_non_dev_port_alone() {
    // Reverse-proxy URL — same URL for both transports.
    let cfg = ServerConfig::from_rest_only("https://music.example.com/").unwrap();
    assert_eq!(cfg.rest_root(), "https://music.example.com");
    assert_eq!(cfg.grpc_endpoint(), "https://music.example.com");
}

#[test]
fn server_config_accepts_explicit_pair() {
    let cfg = ServerConfig::new("http://10.0.0.1:9000", "http://10.0.0.1:9001").unwrap();
    assert_eq!(cfg.rest_root(), "http://10.0.0.1:9000");
    assert_eq!(cfg.grpc_endpoint(), "http://10.0.0.1:9001");
}

#[test]
fn server_config_rejects_garbage() {
    assert!(ServerConfig::from_rest_only("not-a-url").is_err());
    assert!(ServerConfig::from_rest_only("ftp://example.com").is_err());
    assert!(ServerConfig::from_rest_only("").is_err());
    assert!(ServerConfig::new("http://a.example", "file:///etc").is_err());
}

#[test]
fn permission_tier_from_proto_falls_back_to_user() {
    assert_eq!(PermissionTier::from_proto(3), PermissionTier::Admin);
    assert_eq!(PermissionTier::from_proto(2), PermissionTier::Manager);
    assert_eq!(PermissionTier::from_proto(1), PermissionTier::User);
    // Unknown / zero / unspecified → least privilege.
    assert_eq!(PermissionTier::from_proto(0), PermissionTier::User);
    assert_eq!(PermissionTier::from_proto(99), PermissionTier::User);
}
