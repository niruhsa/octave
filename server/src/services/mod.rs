//! Business-logic services.
//!
//! Each domain owns permission enforcement (defense in depth over the
//! transport layer) and audit-log writes for mutations.

pub mod archive;
pub mod artwork;
pub mod discography;
pub mod duration;
pub mod favorites;
pub mod fcm;
pub mod feed;
pub mod fingerprint;
pub mod musicbrainz;
pub mod image_opt;
pub mod ingest;
pub mod library;
pub mod metadata;
pub mod mp3;
pub mod notification;
pub mod organizer;
pub mod playhistory;
pub mod playlist;
pub mod podcast;
pub mod rec_cache;
pub mod recommendation;
pub mod podcast_dir;
pub mod scan;
pub mod storage;
pub mod streaming;
pub mod tag;
pub mod uploads;
pub mod watch;

pub use archive::{extract as extract_archive, ArchiveKind};
pub use artwork::{ArtworkService, CoverArtArchive, CoverArtSource, CoverImage};
pub use discography::{
    build_provider as build_discography_provider, ArtistCandidate, DiscographyCfg,
    DiscographyProvider, DiscographyService, DiscographyStatus, IgnoreRequest, SyncOutcome,
};
pub use favorites::FavoritesService;
pub use fcm::{FcmSender, PushOutcome, PushSender};
pub use feed::{parse_feed, ParsedEpisode, ParsedFeed};
pub use fingerprint::{
    build_extractor, build_index, BruteForceIndex, FeatureExtractor, FingerprintReport,
    FingerprintService, FingerprintStatus, SimilarityIndex,
};
pub use image_opt::{run_optimize_pass, ImageOptimizer, Variant};
pub use ingest::{ArchiveIngestResult, IngestService};
pub use library::LibraryService;
pub use metadata::{MetadataEdit, MetadataService};
pub use notification::NotificationService;
pub use playhistory::{ListeningStats, PlayHistoryService, PlayInput};
pub use playlist::{PlaylistService, PlaylistWithTracks};
pub use podcast::{PodcastService, RefreshReport};
pub use rec_cache::{
    DebouncedWarmer, PlaylistRecWarmer, RecommendationCache, REC_CACHE_MAX, REC_CACHE_TTL,
    REC_WARM_DEBOUNCE,
};
pub use recommendation::{DiscoverSection, RecommendationService};
pub use podcast_dir::{ItunesDirectory, PodcastCandidate, PodcastDirectory, PodcastIndexDirectory};
pub use scan::{ScanReport, ScanService};
pub use storage::StorageService;
pub use streaming::{ResolvedStream, StreamingService};
pub use uploads::{
    can_see, ChunkAck, ChunkInit, ChunkView, FileInit, UploadEvent, UploadFileView, UploadHub,
    UploadSummary, UploadView, UploadsService,
};
