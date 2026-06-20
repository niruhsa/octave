//! Sync engine (Phase 5).
//!
//! Keeps the SQLite offline cache consistent with the server. Three jobs,
//! run in order by [`SyncEngine::sync_now`]:
//!
//! 1. **Push** — replay the offline-edit outbox (`pending_ops`) against the
//!    server in FIFO order. Server authority wins: an op the server rejects
//!    on its merits (403 / not-found / invalid) is dropped and surfaced as
//!    a conflict; a transport failure leaves the op queued for next time.
//! 2. **Pull / reconcile** — for every cached entity (artists, albums,
//!    tracks, playlists) fetch the server's current row and upsert it when
//!    it differs. Rows the server no longer has (404) are pruned locally.
//! 3. **Prune** — drop cached tracks whose downloaded file vanished from
//!    disk (and their now-dangling sync_state rows).
//!
//! Versioning: the server's REST/gRPC DTOs don't expose `updated_at`, so we
//! can't diff timestamps. Instead we hash the server row's content and
//! compare it to the hash we stored in `sync_state.server_etag` on the last
//! sync. Identical hash → skip the write; different → upsert + restamp. This
//! keeps writes (and `updated_at` churn) proportional to *actual* changes.
//!
//! The engine is deliberately conservative: it only ever pulls entities the
//! cache already knows about (the offline-cache principle — we never mirror
//! the full catalog), and it treats the server as the source of truth for
//! every conflict.

pub mod engine;
pub mod ops;

pub use engine::{SyncEngine, SyncReport};
pub use ops::PendingOpKind;
