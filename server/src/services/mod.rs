//! Business-logic services.
//!
//! Each domain owns permission enforcement (defense in depth over the
//! transport layer) and audit-log writes for mutations.

pub mod archive;
pub mod artwork;
pub mod ingest;
pub mod library;
pub mod metadata;
pub mod organizer;
pub mod playlist;
pub mod scan;
pub mod streaming;
pub mod tag;
pub mod watch;

pub use archive::{extract as extract_archive, ArchiveKind};
pub use artwork::{ArtworkService, CoverArtArchive, CoverArtSource, CoverImage};
pub use ingest::{ArchiveIngestResult, IngestService};
pub use library::LibraryService;
pub use metadata::{MetadataEdit, MetadataService};
pub use playlist::{PlaylistService, PlaylistWithTracks};
pub use scan::{ScanReport, ScanService};
pub use streaming::{ResolvedStream, StreamingService};
