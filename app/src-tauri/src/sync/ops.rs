//! Offline-edit op payloads.
//!
//! Each variant maps to one server mutation. The op is serialized to JSON
//! and stored in `pending_ops.payload_json`; on reconnect the engine
//! decodes it and calls the matching `ServerClient` method.
//!
//! Locally-created playlists use a **temporary client-side id** (prefixed
//! `local:`) so the UI can reference them before the server has issued a
//! real UUID. When the queued `playlist.create` op replays, the engine
//! learns the server id and rewrites any later queued ops + cache rows that
//! referenced the temp id (see `engine::remap_local_id`).

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Prefix marking a client-minted placeholder playlist id.
pub const LOCAL_ID_PREFIX: &str = "local:";

pub fn is_local_id(id: &str) -> bool {
    id.starts_with(LOCAL_ID_PREFIX)
}

/// The op-type discriminant stored in `pending_ops.op_type`. Kept as
/// explicit string constants so they match the migration's CHECK list.
pub mod op_type {
    pub const CREATE: &str = "playlist.create";
    pub const RENAME: &str = "playlist.rename";
    pub const DELETE: &str = "playlist.delete";
    pub const ADD_TRACK: &str = "playlist.add_track";
    pub const REMOVE_TRACK: &str = "playlist.remove_track";
    pub const REORDER_TRACK: &str = "playlist.reorder_track";
}

/// Decoded representation of one queued op. Carries everything the engine
/// needs to call the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingOpKind {
    /// Create a playlist. `local_id` is the placeholder the UI used; the
    /// server returns the real id on replay.
    PlaylistCreate {
        local_id: String,
        name: String,
    },
    PlaylistRename {
        playlist_id: String,
        name: String,
    },
    PlaylistDelete {
        playlist_id: String,
    },
    PlaylistAddTrack {
        playlist_id: String,
        track_id: String,
        /// 0 = append; otherwise 1-based insert position.
        position: i32,
    },
    PlaylistRemoveTrack {
        playlist_id: String,
        position: i32,
    },
    PlaylistReorderTrack {
        playlist_id: String,
        from_position: i32,
        to_position: i32,
    },
}

impl PendingOpKind {
    /// The `op_type` string this kind serializes under.
    pub fn op_type(&self) -> &'static str {
        match self {
            PendingOpKind::PlaylistCreate { .. } => op_type::CREATE,
            PendingOpKind::PlaylistRename { .. } => op_type::RENAME,
            PendingOpKind::PlaylistDelete { .. } => op_type::DELETE,
            PendingOpKind::PlaylistAddTrack { .. } => op_type::ADD_TRACK,
            PendingOpKind::PlaylistRemoveTrack { .. } => op_type::REMOVE_TRACK,
            PendingOpKind::PlaylistReorderTrack { .. } => op_type::REORDER_TRACK,
        }
    }

    pub fn to_payload_json(&self) -> AppResult<String> {
        serde_json::to_string(self).map_err(|e| AppError::Internal(format!("encode op: {e}")))
    }

    pub fn from_payload_json(s: &str) -> AppResult<Self> {
        serde_json::from_str(s).map_err(|e| AppError::Internal(format!("decode op: {e}")))
    }

    /// Does this op reference `id` (as the playlist target)? Used to rewrite
    /// queued ops after a `playlist.create` resolves its temp id.
    pub fn references_playlist(&self, id: &str) -> bool {
        match self {
            PendingOpKind::PlaylistCreate { local_id, .. } => local_id == id,
            PendingOpKind::PlaylistRename { playlist_id, .. }
            | PendingOpKind::PlaylistDelete { playlist_id }
            | PendingOpKind::PlaylistAddTrack { playlist_id, .. }
            | PendingOpKind::PlaylistRemoveTrack { playlist_id, .. }
            | PendingOpKind::PlaylistReorderTrack { playlist_id, .. } => playlist_id == id,
        }
    }

    /// Replace references to `old_id` with `new_id` (temp → server id).
    pub fn remap_playlist(&mut self, old_id: &str, new_id: &str) {
        let swap = |s: &mut String| {
            if s == old_id {
                *s = new_id.to_string();
            }
        };
        match self {
            PendingOpKind::PlaylistCreate { local_id, .. } => swap(local_id),
            PendingOpKind::PlaylistRename { playlist_id, .. }
            | PendingOpKind::PlaylistDelete { playlist_id }
            | PendingOpKind::PlaylistAddTrack { playlist_id, .. }
            | PendingOpKind::PlaylistRemoveTrack { playlist_id, .. }
            | PendingOpKind::PlaylistReorderTrack { playlist_id, .. } => swap(playlist_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let op = PendingOpKind::PlaylistAddTrack {
            playlist_id: "p1".into(),
            track_id: "t1".into(),
            position: 0,
        };
        let json = op.to_payload_json().unwrap();
        let back = PendingOpKind::from_payload_json(&json).unwrap();
        assert!(matches!(back, PendingOpKind::PlaylistAddTrack { .. }));
        assert_eq!(back.op_type(), op_type::ADD_TRACK);
    }

    #[test]
    fn detects_local_ids() {
        assert!(is_local_id("local:abc"));
        assert!(!is_local_id("01234567-89ab"));
    }

    #[test]
    fn remaps_playlist_id() {
        let mut op = PendingOpKind::PlaylistRename {
            playlist_id: "local:x".into(),
            name: "n".into(),
        };
        op.remap_playlist("local:x", "real-id");
        assert!(op.references_playlist("real-id"));
        assert!(!op.references_playlist("local:x"));
    }
}
