//! High-level cache repository on top of `db::SqlitePool`.
//!
//! Two distinct concerns live behind this module:
//!   * Metadata rows (artists / albums / tracks / playlists / sync_state) —
//!     this file.
//!   * On-disk media + cover blobs under the app data dir — landed in
//!     Phase 6 (Offline Downloads).
//!
//! All functions here are async and take `&SqlitePool`. They return
//! `AppResult` so callers can pass errors straight through the Tauri bridge.

pub mod model;
pub mod repo;
