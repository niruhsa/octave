//! Business-logic services.
//!
//! Each domain owns permission enforcement (defense in depth over the
//! transport layer) and audit-log writes for mutations.

pub mod archive;
pub mod artwork;
pub mod duration;
pub mod ingest;
pub mod library;
pub mod metadata;
pub mod mp3;
pub mod organizer;
pub mod playlist;
pub mod scan;
pub mod streaming;
pub mod tag;
pub mod uploads;
pub mod watch;

pub use archive::{extract as extract_archive, ArchiveKind};
pub use artwork::{ArtworkService, CoverArtArchive, CoverArtSource, CoverImage};
pub use ingest::{ArchiveIngestResult, IngestService};
pub use library::LibraryService;
pub use metadata::{MetadataEdit, MetadataService};
pub use playlist::{PlaylistService, PlaylistWithTracks};
pub use scan::{ScanReport, ScanService};
pub use streaming::{ResolvedStream, StreamingService};
pub use uploads::{
    can_see, ChunkAck, ChunkInit, ChunkView, FileInit, UploadEvent, UploadFileView, UploadHub,
    UploadSummary, UploadView, UploadsService,
};
