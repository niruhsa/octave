//! Business-logic services.
//!
//! Each domain owns permission enforcement (defense in depth over the
//! transport layer) and audit-log writes for mutations.

pub mod ingest;
pub mod library;
pub mod organizer;
pub mod playlist;
pub mod scan;
pub mod streaming;
pub mod tag;
pub mod watch;

pub use ingest::IngestService;
pub use library::LibraryService;
pub use playlist::{PlaylistService, PlaylistWithTracks};
pub use scan::{ScanReport, ScanService};
pub use streaming::{ResolvedStream, StreamingService};
