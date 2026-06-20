//! Library browse + search service.
//!
//! Behaviour:
//!   * Online — read from the server (`ServerClient`), then enrich each row
//!     with a `downloaded: bool` flag pulled from the local SQLite cache.
//!     The server is authoritative for catalog state; the cache is
//!     authoritative for "is this available offline?".
//!   * Offline — fall back entirely to the cache and present only what
//!     has been downloaded. The result types are the same so the UI can
//!     render either source uniformly.
//!
//! "Offline" here means we have no usable credential / no reachable
//! server. We decide that at call time (a single `whoami`-cheap probe via
//! the cached online flag in `AuthManager`) rather than per-row.

pub mod merged;
pub mod service;

pub use merged::{MergedAlbum, MergedArtist, MergedTrack};
pub use service::{LibrarySource, LibraryView};
