//! Playlist management (Phase 7).
//!
//! Reuses the Phase 5 transport surface (`ServerClient::*_playlist*`) and
//! the offline-edit outbox (`sync::ops`). This module is the *user-facing*
//! orchestrator: it decides per-call whether to push the mutation straight
//! to the server (and mirror it into the cache) or — when the server is
//! unreachable, no credential is configured, or the playlist is a
//! client-minted `local:` placeholder whose create op is still queued —
//! record the edit as a `PendingOpKind` and apply it optimistically to the
//! cache so the UI reflects the change immediately.
//!
//! Reads (`list_my_playlists`, `get_playlist`) follow the same
//! server-then-cache fallback as `library::service`, so the frontend never
//! branches on online/offline.
//!
//! `MergedPlaylistEntry` reuses `library::MergedTrack` so the player store
//! can queue a playlist's entries without a shape conversion.

pub mod merged;
pub mod service;

pub use merged::{MergedPlaylist, MergedPlaylistEntry, PlaylistDetailView};
pub use service::PlaylistService;
