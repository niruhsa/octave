//! Offline downloads (Phase 6).
//!
//! Fetches track files + album art from the server into the app's storage,
//! writes the cache rows that make them playable offline, and supports bulk
//! album / playlist downloads with resume, delete, and storage accounting.
//!
//! Layout:
//!   * [`paths`]   — sanitisation + canonical `Artist/Album/Track.ext` layout.
//!   * [`artwork`] — best-effort Cover Art Archive fetch (see module docs).
//!   * [`manager`] — the `DownloadManager` + result/event types.

pub mod artwork;
pub mod manager;
pub mod paths;

pub use manager::{
    BatchDownloadResult, BatchKind, DownloadManager, ProgressEvent, ProgressPhase, ProgressScope,
    StorageUsage, TrackDownloadResult, PROGRESS_EVENT,
};
