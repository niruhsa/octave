//! Business-logic services.
//!
//! Each domain owns permission enforcement (defense in depth over the
//! transport layer) and audit-log writes for mutations.

pub mod archive;
pub mod artwork;
pub mod discography;
pub mod duration;
pub mod equalizer;
pub mod favorites;
pub mod fcm;
pub mod feed;
pub mod fingerprint;
pub mod image_opt;
pub mod ingest;
pub mod library;
pub mod lyrics;
pub mod metadata;
pub mod mp3;
pub mod musicbrainz;
pub mod notification;
pub mod organizer;
pub mod playhistory;
pub mod playlist;
pub mod podcast;
pub mod podcast_dir;
pub mod rec_cache;
pub mod recommendation;
pub mod scan;
pub mod storage;
pub mod streaming;
pub mod tag;
pub mod uploads;
pub mod watch;

pub use archive::{ArchiveKind, extract as extract_archive};
pub use artwork::{ArtworkService, CoverArtArchive, CoverArtSource, CoverImage};
pub use discography::{
    ArtistCandidate, AudioResolver, DiscographyCfg, DiscographyProvider, DiscographyService,
    DiscographyStatus, IgnoreRequest, NewReleaseNotifier, SyncOutcome, build_audio_resolver,
    build_provider as build_discography_provider,
};
pub use equalizer::{
    EqualizerChangesPage, EqualizerDeviceRuleInput, EqualizerProfileInput, EqualizerService,
    GetEqualizerStateOutcome,
};
pub use favorites::FavoritesService;
pub use fcm::{FcmSender, PushOutcome, PushSender};
pub use feed::{ParsedEpisode, ParsedFeed, parse_feed};
pub use fingerprint::{
    BruteForceIndex, FeatureExtractor, FingerprintReport, FingerprintService, FingerprintStatus,
    SimilarityIndex, build_extractor, build_index,
};
pub use image_opt::{ImageOptimizer, Variant, run_optimize_pass};
pub use ingest::{ArchiveIngestResult, IngestService};
pub use library::LibraryService;
pub use lyrics::{
    LrcLibSource, LyricResult, LyricsOutcome, LyricsReport, LyricsService, LyricsSource,
    LyricsStatus, LyricsView,
};
pub use metadata::{MetadataEdit, MetadataService};
pub use notification::NotificationService;
pub use playhistory::{ListeningStats, PlayHistoryService, PlayInput};
pub use playlist::{PlaylistService, PlaylistWithTracks};
pub use podcast::{PodcastService, RefreshReport};
pub use podcast_dir::{ItunesDirectory, PodcastCandidate, PodcastDirectory, PodcastIndexDirectory};
pub use rec_cache::{
    DebouncedWarmer, PlaylistRecWarmer, REC_CACHE_MAX, REC_CACHE_TTL, REC_WARM_DEBOUNCE,
    RecommendationCache,
};
pub use recommendation::{DiscoverSection, RecommendationService};
pub use scan::{ScanReport, ScanService};
pub use storage::StorageService;
pub use streaming::{ResolvedStream, StreamingService};
pub use uploads::{
    ChunkAck, ChunkInit, ChunkView, FileInit, UploadEvent, UploadFileView, UploadHub,
    UploadSummary, UploadView, UploadsService, can_see,
};
