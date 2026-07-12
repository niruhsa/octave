// Typed thin wrapper over Tauri's `invoke`. Every Rust command exposed via
// `#[tauri::command]` should have a typed entry here so React callers never
// touch raw strings or `any`.

import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------

export type AppInfo = {
  name: string;
  version: string;
  tauri_version: string;
};

/** Fetch build/runtime info. Used as the Phase 0 IPC smoke test. */
export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

// ---------------------------------------------------------------------------
// auth + transport (Phase 2)
// ---------------------------------------------------------------------------

export type PermissionTier = "admin" | "manager" | "user";
export type StoredCredentialKind = "secret_key" | "bearer";

export type AuthSession = {
  kind: StoredCredentialKind;
  user_id: string | null;
  username: string | null;
  tier: PermissionTier;
  expires_at: string | null;
};

/**
 * Point the client at a server. Required before any other auth call.
 *
 * `restUrl` is the URL the user knows (e.g. `http://localhost:8080`).
 * `grpcUrl` is optional: when omitted, Rust derives it (swaps dev port
 * 8080 → 50051; otherwise reuses `restUrl`, assuming a reverse proxy).
 */
export const authConfigureServer = (restUrl: string, grpcUrl?: string) =>
  invoke<void>("auth_configure_server", { restUrl, grpcUrl: grpcUrl ?? null });

/** The server URLs the client is currently pointed at (null if unconfigured).
 *  `grpc_explicit` is true when the gRPC URL is a user override (vs derived
 *  from REST) — the UI only prefills/reveals the gRPC field when so. */
export type ServerInfo = {
  rest_url: string;
  grpc_url: string;
  grpc_explicit: boolean;
};

/** Read the active server config — used to pre-fill the change-server form. */
export const authServerConfig = () =>
  invoke<ServerInfo | null>("auth_server_config");

/**
 * Re-point the app at a (possibly different) server while signed in. Persists
 * the new URLs. Returns the live session when the current credential is still
 * valid on the new server, or `null` when it was rejected (re-login required).
 * Works offline (the session is kept optimistically until the server answers).
 */
export const authChangeServer = (restUrl: string, grpcUrl?: string) =>
  invoke<AuthSession | null>("auth_change_server", { restUrl, grpcUrl: grpcUrl ?? null });

/** Username/password login. Stores the bearer token securely. */
export const authLogin = (username: string, password: string) =>
  invoke<AuthSession>("auth_login", { username, password });

/** Install a pre-shared `SECRET_KEY`. Verified server-side before persist. */
export const authSetSecretKey = (secretKey: string) =>
  invoke<AuthSession>("auth_set_secret_key", { secretKey });

/** Cached session (no network). Returns null if no credential is stored. */
export const authSession = () => invoke<AuthSession | null>("auth_session");

/** Resolve current credential against the server; updates cached tier. */
export const authWhoami = () => invoke<AuthSession>("auth_whoami");

/** Best-effort server logout + wipe local credential store. */
export const authLogout = () => invoke<void>("auth_logout");

/** `/health` ping. Drives the online/offline indicator. */
export const authRefreshOnline = () => invoke<boolean>("auth_refresh_online");

/** Per-transport reachability — gRPC (primary) and REST (fallback). The app is
 *  "online" when either is up (calls fall back gRPC → REST automatically). */
export type TransportHealth = { rest: boolean; grpc: boolean };

/** Probe both transports; drives the per-transport status indicator. */
export const authRefreshTransports = () =>
  invoke<TransportHealth>("auth_refresh_transports");

/**
 * Register a new account. Server-gated to Admin callers (or `SECRET_KEY`,
 * which is effective Admin); the active credential authorizes the call.
 * Returns the new user id. The new account is not logged in locally —
 * the admin stays signed in. Server enforces: username non-empty,
 * password ≥ 8 chars, username unique.
 */
export const authRegister = (
  username: string,
  password: string,
  tier: PermissionTier,
) => invoke<string>("auth_register", { username, password, tier });

/**
 * Change (or admin-reset) a user's password. `oldPassword` is empty for
 * admin/secret-key resets; required + verified server-side for non-admin
 * self-changes. `targetUserId` is the user whose password changes
 * (self-change uses the session's own id). The current session stays
 * valid. Server enforces: new password ≥ 8 chars.
 */
export const authChangePassword = (
  targetUserId: string,
  oldPassword: string,
  newPassword: string,
) =>
  invoke<void>("auth_change_password", {
    targetUserId,
    oldPassword,
    newPassword,
  });

export type UserEntry = {
  id: string;
  username: string;
  level: PermissionTier;
};

/** List every registered user (admin-gated). Used to populate the admin
 *  password-reset dropdown. */
export const authListUsers = () =>
  invoke<UserEntry[]>("auth_list_users");

/** Delete a user account (admin-gated server-side). */
export const authDeleteUser = (targetUserId: string) =>
  invoke<void>("auth_delete_user", { targetUserId });

// ---------------------------------------------------------------------------
// library browse + search (Phase 3)
//
// Each list/search call returns a `LibraryView<T>` with:
//   - `source`: "server" or "cache" — lets UI show an offline badge.
//   - `items`: merged rows carrying a `downloaded` flag per item.
//   - `total`: server-reported total when paginating list endpoints.
// ---------------------------------------------------------------------------

export type LibrarySource = "server" | "cache";

export type LibraryView<T> = {
  source: LibrarySource;
  items: T[];
  total?: number;
};

/** One known spelling of an artist/album, preserved across merges. `name` is
 * the spelling (artist name or album title); `sort_name` is artist-only. */
export type AliasInfo = {
  id: string;
  name: string;
  sort_name: string | null;
  language: string | null;
  is_primary: boolean;
};

export type MergedArtist = {
  id: string;
  name: string;
  sort_name: string | null;
  /** Server-side artist image path when set; drives whether the UI renders
   * the image (served via `artistImageUrl`). `null` for cache-only rows. */
  image_path: string | null;
  /** Every known spelling (e.g. Korean + English). Populated on single-entity
   * reads (the Artist route); empty for list/search/cache rows. */
  aliases: AliasInfo[];
  /** Sum of the on-disk bytes of every track by this artist (server-side). */
  storage_bytes: number;
  downloaded: boolean;
};

/** Album classification. A `single` album always has at least one track
 * flagged `is_single_release` (enforced server-side); `album`/`ep`/`live` are
 * unrestricted. */
export type AlbumType = "album" | "ep" | "single" | "live";

export type MergedAlbum = {
  id: string;
  artist_id: string;
  title: string;
  release_year: number | null;
  album_type: AlbumType;
  /** True when any track on this album is explicit. */
  is_explicit: boolean;
  cover_path: string | null;
  local_cover_path: string | null;
  /** Every known title spelling. See `MergedArtist.aliases`. */
  aliases: AliasInfo[];
  /** Sum of the on-disk bytes of every track on this album (server-side). */
  storage_bytes: number;
  downloaded: boolean;
};

export type MergedTrack = {
  id: string;
  album_id: string;
  artist_id: string;
  title: string;
  track_no: number | null;
  disc_no: number | null;
  duration_ms: number;
  codec: string;
  bitrate_kbps: number | null;
  file_path: string;
  file_size: number | null;
  /** Audio-quality detail probed server-side; `null` when unknown. */
  sample_rate_hz: number | null;
  bit_depth: number | null;
  channels: number | null;
  local_file_path: string | null;
  /** `true` when this track is a single release within its album. */
  is_single_release: boolean;
  /** `true` when this track is explicit (independent of the title text). */
  is_explicit: boolean;
  /** Loudness normalization (Phase 16): integrated loudness (LUFS) + sample peak
   *  + the owning album's loudness. `null` until measured. The player derives a
   *  per-track gain from these. */
  loudness_lufs: number | null;
  loudness_peak: number | null;
  album_loudness_lufs: number | null;
  /** Every known title spelling (populated on single-track reads only). */
  aliases: AliasInfo[];
  downloaded: boolean;
};

/** The server's library-storage breakdown (homepage widget). `misc` shown in
 * the UI is `artwork_bytes + other_bytes`. Online-only read. */
export type LibraryStorage = {
  music_bytes: number;
  podcast_bytes: number;
  artwork_bytes: number;
  other_bytes: number;
  total_bytes: number;
  track_count: number;
  album_count: number;
  artist_count: number;
  podcast_count: number;
  episode_count: number;
  computed_at: string;
};

export type Page = { limit?: number; offset?: number };

export const libraryListArtists = (page: Page = {}) =>
  invoke<LibraryView<MergedArtist>>("library_list_artists", page);

export const librarySearchArtists = (query: string, page: Page = {}) =>
  invoke<LibraryView<MergedArtist>>("library_search_artists", { query, ...page });

export const libraryListAlbumsByArtist = (artistId: string) =>
  invoke<LibraryView<MergedAlbum>>("library_list_albums_by_artist", { artistId });

export const librarySearchAlbums = (query: string, page: Page = {}) =>
  invoke<LibraryView<MergedAlbum>>("library_search_albums", { query, ...page });

export const libraryListTracksByAlbum = (albumId: string) =>
  invoke<LibraryView<MergedTrack>>("library_list_tracks_by_album", { albumId });

/** The server's library-storage breakdown for the homepage widget. Online-only
 * (a live server view) — rejects when offline so the UI can show "—". */
export const getLibraryStorage = () =>
  invoke<LibraryStorage>("library_get_storage");

/** Fetch a single artist/album with its alias set (server-first; cache
 * fallback returns empty `aliases`). Used by the Artist/Album routes. */
export const libraryGetArtist = (id: string) =>
  invoke<MergedArtist>("library_get_artist", { id });
export const libraryGetAlbum = (id: string) =>
  invoke<MergedAlbum>("library_get_album", { id });

export const librarySearchTracks = (query: string, page: Page = {}) =>
  invoke<LibraryView<MergedTrack>>("library_search_tracks", { query, ...page });

/** Delete an artist, album, or track. Manager+ gated server-side. */
export const libraryDeleteArtist = (id: string) =>
  invoke<void>("library_delete_artist", { id });
export const libraryDeleteAlbum = (id: string) =>
  invoke<void>("library_delete_album", { id });
export const libraryDeleteTrack = (id: string) =>
  invoke<void>("library_delete_track", { id });

export type RescanReport = {
  tracks_checked: number;
  tracks_updated: number;
  errors: number;
};

/** Re-measure actual durations for all tracks. Manager+ gated. */
export const libraryRescan = () => invoke<RescanReport>("library_rescan");

/**
 * One opt-in metadata edit for a track (Phase 9). Every field is optional;
 * omit a field to leave it unchanged server-side. `year` is written to the
 * file's audio tag only (not a DB column) and takes effect only when the
 * server has tag write-back (`WRITE_TAGS`) enabled.
 */
export type MetadataEdit = {
  title?: string;
  track_no?: number;
  disc_no?: number;
  metadata_json?: string;
  year?: number;
};

/**
 * Apply a metadata edit to a track. Manager+ gated server-side. Returns the
 * refreshed track; the change is mirrored into the offline cache for
 * downloaded items and reconciled into the cache for everyone on next sync.
 */
export const libraryEditTrackMetadata = (id: string, edit: MetadataEdit) =>
  invoke<MergedTrack>("library_edit_track_metadata", { id, edit });

// ---------------------------------------------------------------------------
// merge + aliases (Phase 10 — Manager+ gated server-side)
//
// Merge folds a duplicate artist/album into a survivor, preserving every
// spelling as an alias; the survivor's display name follows the server's
// PRIMARY_LANGUAGE. `libraryMoveTrack` moves a track into another album and
// optionally flags it a single release. All require a live server.
// ---------------------------------------------------------------------------

export const libraryMergeArtists = (survivorId: string, duplicateId: string) =>
  invoke<MergedArtist>("library_merge_artists", { survivorId, duplicateId });

// ---------------------------------------------------------------------------
// Artist storage location (per-artist language folder)
//
// An artist's tracks can end up split across several `<Language>/<Artist>`
// folders on disk (different language tags / spellings at ingest). These let a
// manager see the split and consolidate every track under one language folder.
// ---------------------------------------------------------------------------

/** One distinct `<Language>/<Artist>` directory an artist occupies on disk. */
export type ArtistLibraryPath = {
  language: string;
  artist_folder: string;
  /** `"<language>/<artist_folder>"` — the group key shown to the user. */
  relative_dir: string;
  track_count: number;
  storage_bytes: number;
};

export type ArtistStoragePaths = {
  paths: ArtistLibraryPath[];
  /** Language folders already present at the top of the library. */
  library_languages: string[];
};

export type RelocateReport = {
  moved: number;
  skipped: number;
  target_relative_dir: string;
};

/** List the on-disk language/artist folders an artist occupies (server-only). */
export const libraryListArtistLibraryPaths = (id: string) =>
  invoke<ArtistStoragePaths>("library_list_artist_library_paths", { artistId: id });

/**
 * Move all of an artist's tracks under a single `<targetLanguage>/…` folder.
 * `targetFolder` pins the on-disk artist-folder spelling; omit it to let the
 * server resolve one (existing folder in that language → alias in that language
 * → current folder). Manager+ gated server-side.
 */
export const librarySetArtistLanguage = (
  id: string,
  targetLanguage: string,
  targetFolder?: string,
) =>
  invoke<RelocateReport>("library_set_artist_language", {
    artistId: id,
    targetLanguage,
    targetFolder: targetFolder ?? null,
  });

/** An album's current on-disk folder + the name a "match title" rename yields. */
export type AlbumFolderInfo = {
  /** Current album-folder name on disk, or null when unresolvable / no root. */
  current_folder: string | null;
  /** `"<language>/<artist>/<album>"` full relative dir, for display. */
  relative_dir: string | null;
  /** Sanitized album title — what "rename to match title" would produce. */
  suggested_folder: string;
  track_count: number;
};

/** Inspect an album's on-disk folder (server-only; Manager+ acts on it). */
export const libraryAlbumFolder = (albumId: string) =>
  invoke<AlbumFolderInfo>("library_album_folder", { albumId });

/**
 * Rename an album's on-disk folder, physically moving its track files. Pass a
 * `folderName` to pin the name, or omit it to match the album's title. Applies
 * to every album type (album/EP/single/live). Manager+ gated server-side.
 */
export const libraryRenameAlbumFolder = (albumId: string, folderName?: string) =>
  invoke<RelocateReport>("library_rename_album_folder", {
    albumId,
    folderName: folderName ?? null,
  });

export const libraryMergeAlbums = (survivorId: string, duplicateId: string) =>
  invoke<MergedAlbum>("library_merge_albums", { survivorId, duplicateId });

export const libraryMoveTrack = (
  trackId: string,
  albumId: string,
  singleRelease: boolean,
) => invoke<MergedTrack>("library_move_track", { trackId, albumId, singleRelease });

export const librarySetTrackSingleRelease = (trackId: string, singleRelease: boolean) =>
  invoke<MergedTrack>("library_set_track_single_release", { trackId, singleRelease });

/** Toggle a track's explicit flag; the album's explicit rollup recomputes server-side. */
export const librarySetTrackExplicit = (trackId: string, explicit: boolean) =>
  invoke<MergedTrack>("library_set_track_explicit", { trackId, explicit });

/** Set an album's classification. When setting `single`, pass `singleTrackId`
 * to flag the main single (required unless the album already has one). */
export const librarySetAlbumType = (
  albumId: string,
  albumType: AlbumType,
  singleTrackId?: string,
) =>
  invoke<MergedAlbum>("library_set_album_type", { albumId, albumType, singleTrackId });

export const libraryAddArtistAlias = (
  artistId: string,
  name: string,
  sortName?: string,
  language?: string,
) =>
  invoke<MergedArtist>("library_add_artist_alias", {
    artistId,
    name,
    sortName: sortName ?? null,
    language: language ?? null,
  });

export const libraryRemoveArtistAlias = (artistId: string, aliasId: string) =>
  invoke<MergedArtist>("library_remove_artist_alias", { artistId, aliasId });

export const librarySetPrimaryArtistAlias = (artistId: string, aliasId: string) =>
  invoke<MergedArtist>("library_set_primary_artist_alias", { artistId, aliasId });

export const libraryAddAlbumAlias = (albumId: string, title: string, language?: string) =>
  invoke<MergedAlbum>("library_add_album_alias", {
    albumId,
    title,
    language: language ?? null,
  });

export const libraryRemoveAlbumAlias = (albumId: string, aliasId: string) =>
  invoke<MergedAlbum>("library_remove_album_alias", { albumId, aliasId });

export const librarySetPrimaryAlbumAlias = (albumId: string, aliasId: string) =>
  invoke<MergedAlbum>("library_set_primary_album_alias", { albumId, aliasId });

/** List a track's alternate title spellings (single-track read). */
export const libraryListTrackAliases = (trackId: string) =>
  invoke<AliasInfo[]>("library_list_track_aliases", { trackId });

export const libraryAddTrackAlias = (trackId: string, title: string, language?: string) =>
  invoke<MergedTrack>("library_add_track_alias", {
    trackId,
    title,
    language: language ?? null,
  });

export const libraryRemoveTrackAlias = (trackId: string, aliasId: string) =>
  invoke<MergedTrack>("library_remove_track_alias", { trackId, aliasId });

export const librarySetPrimaryTrackAlias = (trackId: string, aliasId: string) =>
  invoke<MergedTrack>("library_set_primary_track_alias", { trackId, aliasId });

/**
 * Upload a cover image (album) / image (artist) from a locally-picked file
 * path. The Rust side reads the file natively (off the WebView) and POSTs the
 * bytes to the server (Manager+ gated). After success, bust the cached image
 * URL with a new `version` (see `coverUrl` / `artistImageUrl`).
 */
export const libraryUploadAlbumCover = (albumId: string, path: string) =>
  invoke<void>("library_upload_album_cover", { albumId, path });

export const libraryUploadArtistImage = (artistId: string, path: string) =>
  invoke<void>("library_upload_artist_image", { artistId, path });

// ---------------------------------------------------------------------------
// follows & notifications (Phase 10)
//
// Server-authoritative + online-only (no offline cache path). Only a
// logged-in *user* (bearer session) can follow — a `SECRET_KEY` session has
// no user and is rejected server-side.
// ---------------------------------------------------------------------------

/** A slim followed-artist row (from `list_following`). */
export type FollowedArtist = {
  id: string;
  name: string;
  sort_name: string | null;
  image_path: string | null;
};

/** One delivered notification. `kind` is `"new_release"` today; `artist_id`/
 *  `album_id` are null when the referenced entity was since deleted. */
export type AppNotification = {
  id: string;
  kind: string;
  artist_id: string | null;
  album_id: string | null;
  /** Set on a `"new_episode"` notification (the podcast / episode it's about). */
  podcast_id: string | null;
  episode_id: string | null;
  title: string;
  body: string | null;
  read: boolean;
  created_at: string;
};

/** A page of notifications + the total unread count (for a badge). */
export type NotificationPage = {
  notifications: AppNotification[];
  total: number;
  unread_count: number;
};

/** Follow an artist. Returns the resulting follow state (`true`). */
export const followArtist = (artistId: string) =>
  invoke<boolean>("follow_artist", { artistId });

/** Unfollow an artist. Returns the resulting follow state (`false`). */
export const unfollowArtist = (artistId: string) =>
  invoke<boolean>("unfollow_artist", { artistId });

/** Whether the caller currently follows `artistId`. */
export const isFollowing = (artistId: string) =>
  invoke<boolean>("is_following", { artistId });

/** The artists the caller follows. */
export const listFollowing = () => invoke<FollowedArtist[]>("list_following");

/** A page of the caller's notifications (newest first) + unread count. */
export const notificationsList = (
  unreadOnly?: boolean,
  limit?: number,
  offset?: number,
) =>
  invoke<NotificationPage>("notifications_list", {
    unreadOnly: unreadOnly ?? false,
    limit: limit ?? null,
    offset: offset ?? null,
  });

/** The caller's unread notification count (for the badge). */
export const notificationsUnreadCount = () =>
  invoke<number>("notifications_unread_count");

/** Mark one notification read. */
export const notificationsMarkRead = (id: string) =>
  invoke<void>("notifications_mark_read", { id });

/** Mark every unread notification read. Returns the count flipped. */
export const notificationsMarkAllRead = () =>
  invoke<number>("notifications_mark_all_read");

// ---------------------------------------------------------------------------
// play history (Phase 11)
//
// Recording is offline-first: `playHistoryRecord` queues a play locally; the
// sync scheduler (and an opportunistic `playHistoryFlush` right after) pushes
// the backlog to the server. Reads (`playHistoryList` / `playStats`) are
// server-authoritative + online-only, like the notifications feed.
// ---------------------------------------------------------------------------

/** One recorded play. Entity refs are null when the catalog row was deleted. */
export type PlayEvent = {
  id: string;
  track_id: string | null;
  artist_id: string | null;
  album_id: string | null;
  track_title: string;
  artist_name: string;
  ms_played: number;
  completed: boolean;
  played_at: string;
};

export type PlayHistoryPage = { events: PlayEvent[]; total: number };

export type TrackStat = {
  track_id: string | null;
  track_title: string;
  artist_name: string;
  plays: number;
};

export type ArtistStat = {
  artist_id: string | null;
  artist_name: string;
  plays: number;
};

export type ListeningStats = {
  top_tracks: TrackStat[];
  top_artists: ArtistStat[];
  total_plays: number;
  total_ms: number;
};

/** Queue a play locally (offline-safe). Flushed to the server separately. */
export const playHistoryRecord = (
  trackId: string,
  msPlayed: number,
  completed: boolean,
) =>
  invoke<void>("play_history_record", {
    trackId,
    msPlayed: Math.max(0, Math.round(msPlayed)),
    completed,
  });

/** Flush the queued plays to the server. Returns the count recorded. */
export const playHistoryFlush = () => invoke<number>("play_history_flush");

/** A page of the caller's plays, newest first. */
export const playHistoryList = (limit?: number, offset?: number) =>
  invoke<PlayHistoryPage>("play_history_list", {
    limit: limit ?? null,
    offset: offset ?? null,
  });

/** Listening stats over a window (`windowDays` 0/omitted = all time). */
export const playStats = (windowDays?: number, limit?: number) =>
  invoke<ListeningStats>("play_history_stats", {
    windowDays: windowDays ?? null,
    limit: limit ?? null,
  });

// ---------------------------------------------------------------------------
// favorites (Phase 11)
//
// Per-user likes on tracks/albums/artists. Server-authoritative; the UI toggles
// optimistically and reverts on error. Bearer-user only (a SECRET_KEY session
// has no user — server-rejected). List reads return full entities.
// ---------------------------------------------------------------------------

export type FavoriteKind = "track" | "album" | "artist";

/** A favorited track (server's entity shape; lacks the cache `downloaded`/
 *  `local_file_path` fields — playback resolves local-or-stream in Rust). */
export type FavoriteTrack = {
  id: string;
  album_id: string;
  artist_id: string;
  title: string;
  track_no: number | null;
  disc_no: number | null;
  duration_ms: number;
  codec: string;
  bitrate_kbps: number | null;
  file_path: string;
  file_size: number | null;
  sample_rate_hz: number | null;
  bit_depth: number | null;
  channels: number | null;
  metadata_json: string;
  is_single_release: boolean;
  /** Loudness normalization (Phase 16); `null` until measured. */
  loudness_lufs: number | null;
  loudness_peak: number | null;
  album_loudness_lufs: number | null;
};

export type FavoriteAlbum = {
  id: string;
  artist_id: string;
  title: string;
  release_year: number | null;
  cover_path: string | null;
  aliases: AliasInfo[];
  storage_bytes: number;
};

export type FavoriteArtist = {
  id: string;
  name: string;
  sort_name: string | null;
  image_path: string | null;
  aliases: AliasInfo[];
  storage_bytes: number;
};

/** Favorite an entity. Returns the resulting state (`true`). */
export const favoritesFavorite = (kind: FavoriteKind, entityId: string) =>
  invoke<boolean>("favorites_favorite", { kind, entityId });

/** Unfavorite an entity. Returns the resulting state (`false`). */
export const favoritesUnfavorite = (kind: FavoriteKind, entityId: string) =>
  invoke<boolean>("favorites_unfavorite", { kind, entityId });

/** Whether the caller has favorited `entityId`. */
export const favoritesIsFavorite = (kind: FavoriteKind, entityId: string) =>
  invoke<boolean>("favorites_is_favorite", { kind, entityId });

export const favoritesListTracks = () =>
  invoke<FavoriteTrack[]>("favorites_list_tracks");
export const favoritesListAlbums = () =>
  invoke<FavoriteAlbum[]>("favorites_list_albums");
export const favoritesListArtists = () =>
  invoke<FavoriteArtist[]>("favorites_list_artists");

/** Just the favorited track ids — for bulk heart-state hydration. */
export const favoritesTrackIds = () => invoke<string[]>("favorites_track_ids");

// ---------------------------------------------------------------------------
// discover / recommendations (Phase 11)
//
// Behavioral home shelves + seeded radio, derived from play history + favorites
// + the library graph. Server-authoritative + online-only; `discoverHome` is
// bearer-user personalized.
// ---------------------------------------------------------------------------

/** An album in a discover shelf (server entity shape; no cache fields). */
export type DiscoverAlbum = {
  id: string;
  artist_id: string;
  title: string;
  release_year: number | null;
  cover_path: string | null;
  aliases: AliasInfo[];
  storage_bytes: number;
};

/** A titled album shelf. */
export type DiscoverSection = {
  id: string;
  title: string;
  albums: DiscoverAlbum[];
};

/** Personalized home shelves (only the non-empty ones). */
export const discoverHome = () => invoke<DiscoverSection[]>("discover_home");

/** A radio track queue seeded from an artist, album, or track (pass exactly
 *  one). A `seedTrackId` uses acoustic "sounds like" similarity (Phase 12),
 *  falling back to behavioral radio when the track has no embedding yet.
 *  Returns the server track shape (same as a favorited track). */
export const discoverRadio = (
  seedArtistId?: string,
  seedAlbumId?: string,
  seedTrackId?: string,
) =>
  invoke<FavoriteTrack[]>("discover_radio", {
    seedArtistId: seedArtistId ?? null,
    seedAlbumId: seedAlbumId ?? null,
    seedTrackId: seedTrackId ?? null,
  });

/** Acoustic "sounds like this" — the seed track's nearest neighbors (Phase 12).
 *  Falls back to the track's other same-artist tracks when no embedding yet. */
export const discoverSimilar = (trackId: string, limit?: number) =>
  invoke<FavoriteTrack[]>("discover_similar", {
    trackId,
    limit: limit ?? null,
  });

// ---------------------------------------------------------------------------
// lyrics (Phase 15)
// ---------------------------------------------------------------------------

/** One lyric line: `ms` from the start of the track + its text. */
export type LyricLine = { ms: number; text: string };

/** A track's parsed lyrics. `found=false` means none/pending — render nothing
 *  (or a placeholder). `synced` distinguishes time-aligned lines (with `ms`)
 *  from a plain-text dump; `instrumental` is a positive "no lyrics". */
export type Lyrics = {
  found: boolean;
  synced: boolean;
  instrumental: boolean;
  source: string | null;
  lines: LyricLine[];
  plain: string;
};

/** Fetch a track's parsed lyrics. Server-first; falls back to the offline
 *  SQLite mirror for downloaded/previously-viewed tracks. */
export const getLyrics = (trackId: string) =>
  invoke<Lyrics>("get_lyrics", { trackId });

/** Manager: force a re-resolve of a track's lyrics (sidecar → embedded → LRCLIB). */
export const refetchLyrics = (trackId: string) =>
  invoke<Lyrics>("refetch_lyrics", { trackId });

/** Manager: set lyrics from an uploaded `.lrc`/text blob. */
export const setLyrics = (trackId: string, lrc: string) =>
  invoke<Lyrics>("set_lyrics", { trackId, lrc });

/** Manager: clear a track's lyrics. */
export const clearLyrics = (trackId: string) =>
  invoke<Lyrics>("clear_lyrics", { trackId });

/** Spotify-style playlist recommendations (Phase 12). Pass the playlist's
 *  current track ids as seeds; results are based on + exclude them — so the more
 *  songs the playlist has, the better the recommendations. Falls back to
 *  same-artist suggestions when no seed has an embedding yet. */
export const discoverPlaylistRecommendations = (
  seedTrackIds: string[],
  limit?: number,
) =>
  invoke<FavoriteTrack[]>("discover_playlist_recommendations", {
    seedTrackIds,
    limit: limit ?? null,
  });

/** Acoustic-fingerprint analysis coverage (Phase 12). `enabled` is false when
 *  the server has `FINGERPRINT_ENABLED` off. */
export type FingerprintStatus = {
  analyzed: number;
  total: number;
  model_version: string;
  enabled: boolean;
};

/** Read fingerprint analysis coverage from the server. */
export const fingerprintStatus = () =>
  invoke<FingerprintStatus>("fingerprint_status");

// ---------------------------------------------------------------------------
// discography sync (Phase 14) — Manager-only. Reconcile an artist against
// MusicBrainz to surface missing releases + missing tracks. See
// DISCOGRAPHY_SYNC.md. Server enforces Manager; the panel also hides itself.
// ---------------------------------------------------------------------------

export type MissingRelease = {
  title: string;
  album_type: string;
  year: number | null;
  provider_id: string;
};

export type MissingTrack = {
  title: string;
  position: number | null;
  disc_no: number | null;
  recording_id: string | null;
  title_key: string;
};

export type IncompleteAlbum = {
  album_id: string;
  title: string;
  release_group_id: string;
  missing_tracks: MissingTrack[];
};

export type DiscographyReport = {
  artist_id: string;
  provider: string;
  missing_releases: MissingRelease[];
  incomplete_albums: IncompleteAlbum[];
  missing_release_count: number;
  incomplete_album_count: number;
  generated_at: string;
};

export type DiscographyCandidate = {
  provider_id: string;
  name: string;
  disambiguation: string | null;
  score: number;
};

export type DiscographyIgnore = {
  id: string;
  artist_id: string;
  scope: "release" | "track";
  release_group_id: string;
  recording_id: string | null;
  title_key: string | null;
  label: string;
  created_at: string;
};

export type DiscographySyncResult = {
  status: "report" | "needs_resolution";
  report: DiscographyReport | null;
  candidates: DiscographyCandidate[];
};

/** The cached gap report, or null when the artist has never been synced. */
export const discographyReport = (artistId: string) =>
  invoke<DiscographyReport | null>("discography_report", { artistId });

/** Trigger a sync (slow — hits the provider). Returns a report, or a candidate
 *  list to disambiguate when the artist can't be auto-matched. */
export const discographySync = (artistId: string) =>
  invoke<DiscographySyncResult>("discography_sync", { artistId });

/** Provider artist candidates for the disambiguation dialog. */
export const discographyCandidates = (artistId: string) =>
  invoke<DiscographyCandidate[]>("discography_candidates", { artistId });

/** Pin the artist ↔ MusicBrainz match, or ignore the artist (omit `mbid`). */
export const discographyResolve = (artistId: string, mbid?: string) =>
  invoke<void>("discography_resolve", { artistId, mbid: mbid ?? null });

/** The artist's suppression list (the "Ignored" management view). */
export const discographyIgnores = (artistId: string) =>
  invoke<DiscographyIgnore[]>("discography_ignores", { artistId });

/** Suppress a release (scope "release") or a track (scope "track"); returns the
 *  re-filtered report. */
export const discographyAddIgnore = (
  artistId: string,
  scope: "release" | "track",
  releaseGroupId: string,
  opts?: { recordingId?: string; titleKey?: string; label?: string },
) =>
  invoke<DiscographyReport>("discography_add_ignore", {
    artistId,
    scope,
    releaseGroupId,
    recordingId: opts?.recordingId ?? null,
    titleKey: opts?.titleKey ?? null,
    label: opts?.label ?? "",
  });

/** Un-ignore a suppression; returns the re-filtered report. */
export const discographyRemoveIgnore = (artistId: string, ignoreId: string) =>
  invoke<DiscographyReport>("discography_remove_ignore", { artistId, ignoreId });

/** Library-wide coverage (Manager). `enabled` is false when the server has
 *  `DISCOGRAPHY_ENABLED` off. */
export type DiscographyStatus = {
  enabled: boolean;
  provider: string;
  artists_total: number;
  matched: number;
  unresolved: number;
  ignored: number;
};

/** Summary of a library-wide `sync-all` pass. */
export type DiscographySyncAll = {
  synced: number;
  skipped_fresh: number;
  failed: number;
  total: number;
};

/** Read library-wide discography coverage. */
export const discographyStatus = () =>
  invoke<DiscographyStatus>("discography_status");

/** Re-sync every matched artist (rate-limited — can take a while). */
export const discographySyncAll = () =>
  invoke<DiscographySyncAll>("discography_sync_all");

/**
 * Enable the **background** notification poll (Android only; no-op on desktop).
 * Reads the active bearer token natively and schedules a ~15-min WorkManager
 * job that surfaces new-release notifications while the app is closed. Safe to
 * call on every session change; disables itself for a `SECRET_KEY` session.
 */
export const notifBackgroundSyncEnable = () =>
  invoke<void>("notif_background_sync_enable");

/** Disable the background notification poll (logout / no eligible user). */
export const notifBackgroundSyncDisable = () =>
  invoke<void>("notif_background_sync_disable");

// ---------------------------------------------------------------------------
// Podcasts
// ---------------------------------------------------------------------------

/** A directory search result (enough to subscribe to a feed + display it). */
export type PodcastCandidate = {
  feed_url: string;
  title: string;
  author: string | null;
  description: string | null;
  image_url: string | null;
  categories: string[];
  itunes_id: number | null;
  podcastindex_id: number | null;
};

/** A podcast show + whether the user is subscribed + downloaded-episode count. */
export type MergedPodcast = {
  id: string;
  feed_url: string;
  title: string;
  author: string | null;
  description: string | null;
  image_url: string | null;
  link: string | null;
  language: string | null;
  categories: string[];
  itunes_id: number | null;
  podcastindex_id: number | null;
  auto_download: number;
  last_refreshed_at: string | null;
  subscribed: boolean;
  downloaded_count: number;
  /** Sum of the on-disk bytes of every downloaded episode of this show (server-side). */
  storage_bytes: number;
};

/** An episode + its offline state. `downloaded` = the client has the file;
 *  `server_downloaded` = the server has it cached (its stream endpoint serves it). */
export type MergedEpisode = {
  id: string;
  podcast_id: string;
  guid: string;
  title: string;
  description: string | null;
  enclosure_url: string;
  enclosure_type: string | null;
  episode_no: number | null;
  season_no: number | null;
  duration_ms: number | null;
  codec: string | null;
  bitrate_kbps: number | null;
  file_size: number | null;
  image_url: string | null;
  published_at: string | null;
  local_file_path: string | null;
  server_downloaded: boolean;
  downloaded: boolean;
  /** Last playback position in ms (0 = not started). Drives "resume". */
  position_ms: number;
  /** Played to (near) the end — shown as "listened". */
  completed: boolean;
};

/** Outcome of a feed refresh. */
export type RefreshReport = {
  podcast_id: string;
  new_episodes: number;
  not_modified: boolean;
};

/** Search the directory (iTunes / PodcastIndex) for shows. Online only. */
export const podcastSearch = (query: string, limit?: number) =>
  invoke<PodcastCandidate[]>("podcast_search", { query, limit: limit ?? null });

/** The shows the user is subscribed to (server when online, cache offline). */
export const podcastList = () =>
  invoke<LibraryView<MergedPodcast>>("podcast_list");

/** A single show (server-first, cache fallback). */
export const podcastGet = (id: string) =>
  invoke<MergedPodcast>("podcast_get", { id });

/** A show's episodes, newest-first (server-first, cache fallback). */
/** All of a show's episodes, newest-first (the Tauri command pages through the
 *  whole feed; the server caps a single response at 200). */
export const podcastListEpisodes = (podcastId: string) =>
  invoke<LibraryView<MergedEpisode>>("podcast_list_episodes", { podcastId });

/** Record playback progress for an episode (last position + whether finished).
 *  Writes the local cache immediately and pushes to the server best-effort. */
export const podcastRecordProgress = (
  episodeId: string,
  positionMs: number,
  completed: boolean,
) =>
  invoke<void>("podcast_record_progress", {
    episodeId,
    positionMs: Math.max(0, Math.round(positionMs)),
    completed,
  });

/** Subscribe a feed to the catalog by feed URL, or by an iTunes id to resolve.
 *  Manager+ server-side. */
export const podcastSubscribeFeed = (feedUrl?: string, itunesId?: number) =>
  invoke<MergedPodcast>("podcast_subscribe_feed", {
    feedUrl: feedUrl ?? null,
    itunesId: itunesId ?? null,
  });

/** Subscribe the current user to a show (for new-episode alerts). */
export const podcastSubscribe = (id: string) =>
  invoke<MergedPodcast>("podcast_subscribe", { id });

/** Unsubscribe the current user from a show. */
export const podcastUnsubscribe = (id: string) =>
  invoke<MergedPodcast>("podcast_unsubscribe", { id });

/** Manually refresh a show's feed (Manager+). Returns the new-episode count. */
export const podcastRefresh = (id: string) =>
  invoke<RefreshReport>("podcast_refresh", { id });

/** Set a show's newest-N auto-download policy (Manager+; 0 = metadata only). */
export const podcastSetAutoDownload = (id: string, autoDownload: number) =>
  invoke<MergedPodcast>("podcast_set_auto_download", { id, autoDownload });

/**
 * Register this device for **real-time FCM push** (Android). Fetches the FCM
 * token natively and registers it with the server (bearer token read in Rust,
 * never the WebView). Returns `true` when FCM is available + registered;
 * `false` on desktop / no Google Play Services / no bearer session — the caller
 * then falls back to the WorkManager background poll.
 */
export const pushRegister = () => invoke<boolean>("push_register");

/** Unregister this device from FCM push (call on sign-out, before clearing the
 *  session). No-op on desktop. */
export const pushUnregister = () => invoke<void>("push_unregister");

// ---------------------------------------------------------------------------
// playlists (Phase 7)
//
// `playlist_list` / `playlist_get` follow the same server-then-cache
// fallback as the library calls. Mutations either land on the server
// immediately (cache mirrored) or, when offline / the playlist is a
// client-minted `local:` placeholder, are recorded as a `PendingOp` and
// applied optimistically to the cache. `PlaylistDetailView.entries` reuse
// `MergedTrack` so they drop straight into the player queue.
// ---------------------------------------------------------------------------

export type MergedPlaylist = {
  id: string;
  owner_id: string;
  name: string;
  /** True for client-minted `local:` ids whose create op is still queued. */
  local: boolean;
};

export type MergedPlaylistEntry = {
  /** 1-based contiguous position. */
  position: number;
  added_at: string;
  track: MergedTrack;
};

export type PlaylistDetailView = {
  source: LibrarySource;
  playlist: MergedPlaylist;
  entries: MergedPlaylistEntry[];
};

export const playlistList = () =>
  invoke<LibraryView<MergedPlaylist>>("playlist_list");

export const playlistGet = (playlistId: string) =>
  invoke<PlaylistDetailView | null>("playlist_get", { playlistId });

export const playlistCreate = (name: string) =>
  invoke<MergedPlaylist>("playlist_create", { name });

export const playlistRename = (playlistId: string, name: string) =>
  invoke<MergedPlaylist>("playlist_rename", { playlistId, name });

export const playlistDelete = (playlistId: string) =>
  invoke<void>("playlist_delete", { playlistId });

/** `position = 0` ⇒ append; `position ≥ 1` ⇒ 1-based insert with shift. */
export const playlistAddTrack = (
  playlistId: string,
  trackId: string,
  position: number,
) => invoke<PlaylistDetailView>("playlist_add_track", { playlistId, trackId, position });

export const playlistRemoveTrack = (playlistId: string, position: number) =>
  invoke<PlaylistDetailView>("playlist_remove_track", { playlistId, position });

export const playlistReorderTrack = (
  playlistId: string,
  fromPosition: number,
  toPosition: number,
) =>
  invoke<PlaylistDetailView>("playlist_reorder_track", {
    playlistId,
    fromPosition,
    toPosition,
  });

// ---------------------------------------------------------------------------
// playback (Phase 4)
//
// `player_media_url` returns the platform-correct URL for the webview's
// `<audio>` element. The `media://` protocol (registered in Rust) serves a
// cached local file (range-aware) or proxies the server stream with auth
// injected — so the frontend never branches on online/offline.
// ---------------------------------------------------------------------------

export const playerMediaUrl = (trackId: string, kind?: "track" | "episode") =>
  invoke<string>("player_media_url", { trackId, kind: kind ?? null });

/** Loopback URL for an album's cover art (fetchable by native code). */
export const playerCoverUrl = (albumId: string) =>
  invoke<string>("player_cover_url", { albumId });

/** Loopback base the native notification posts transport presses to. */
export const playerActionUrlBase = () =>
  invoke<string>("player_action_url_base");

/**
 * Ask Rust to prefetch a track to a local temp file (idempotent, fire-and-forget).
 * Lets a streamed queue advance with the screen off: the next track is fetched
 * to disk while the current plays, so the hidden WebView loads a local file at
 * the boundary instead of a network stream (which it won't start). See
 * `player::prefetch` on the Rust side.
 */
export const playerPrefetch = (trackId: string) =>
  invoke<void>("player_prefetch", { trackId });

/**
 * True when the prefetcher holds a *complete* local copy of `trackId`. The
 * playback deck checks this before arming its standby `<audio>` element for a
 * streamed track — arming earlier would proxy-stream the track in parallel
 * with the prefetch download (see `GAPLESS_CROSSFADE.md`).
 */
export const playerPrefetchIsReady = (trackId: string) =>
  invoke<boolean>("player_prefetch_is_ready", { trackId });

/**
 * Subscribe to prefetch completions (payload = track id). The playback deck
 * re-arms its standby element on each — this is the async half of
 * `playerPrefetchIsReady`. Returns an unlisten fn.
 */
export async function onPrefetchReady(
  cb: (trackId: string) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<string>("player-prefetch-ready", (e) => cb(e.payload));
}

// ---------------------------------------------------------------------------
// native media session (Android system notification + lock screen)
//
// A bare WebView doesn't surface the Web Media Session API to Android's system
// notification, so the native side (Kotlin `MediaSessionPlugin` + a foreground
// service) owns a MediaSession + MediaStyle notification. The frontend pushes
// now-playing state via these commands and receives transport-button presses
// via `onMediaSessionAction`. All no-ops on desktop (handle never bound).
// ---------------------------------------------------------------------------

export type MediaInfo = {
  title: string;
  artist: string;
  album: string;
  /** Loopback cover URL (`playerCoverUrl`) or null when art is unknown. */
  artworkUrl: string | null;
  /** Loopback base (`playerActionUrlBase`) for native transport presses. */
  actionBaseUrl: string;
  durationMs: number;
  positionMs: number;
  playing: boolean;
};

export type PlaybackInfo = {
  positionMs: number;
  durationMs: number;
  playing: boolean;
};

/** Full now-playing push (track change / metadata). */
export const mediaSessionUpdate = (info: MediaInfo) =>
  invoke<void>("media_session_update", { info });

/** Lightweight play/pause + position push. */
export const mediaSessionSetPlayback = (info: PlaybackInfo) =>
  invoke<void>("media_session_set_playback", { info });

/** Tear down the native session + notification. */
export const mediaSessionClear = () => invoke<void>("media_session_clear");

export type MediaSessionAction = {
  action: "play" | "pause" | "playpause" | "next" | "prev" | "stop" | "seek";
  /** Present for `seek` — target position in ms. */
  positionMs?: number | null;
};

/**
 * Subscribe to native transport-button presses (notification / lock screen /
 * Bluetooth / headset). The native side posts these to the in-app loopback
 * server, which re-emits them as the `media-session-action` Tauri event (the
 * plugin event channel is ACL-gated, so we use a normal event). Returns an
 * unlisten fn.
 */
export async function onMediaSessionAction(
  cb: (a: MediaSessionAction) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<MediaSessionAction>("media-session-action", (e) => cb(e.payload));
}

// ---------------------------------------------------------------------------
// sync engine (Phase 5)
//
// `syncNow` runs push (replay offline-edit outbox) → pull/reconcile cached
// entities → prune missing files, and returns a `SyncReport`. Offline
// playlist edits are recorded via `syncEnqueueOp` and replayed on reconnect.
// ---------------------------------------------------------------------------

export type SyncReport = {
  ops_pushed: number;
  ops_conflicted: number;
  ops_deferred: number;
  entities_updated: number;
  entities_pruned: number;
  files_missing: number;
  conflicts: string[];
};

/**
 * One queued offline edit. The `kind` discriminant matches the Rust
 * `PendingOpKind` (serde `tag = "kind"`, snake_case). Locally-created
 * playlists use a `local:`-prefixed placeholder id until their create op
 * replays and the engine learns the server id.
 */
export type PendingOp =
  | { kind: "playlist_create"; local_id: string; name: string }
  | { kind: "playlist_rename"; playlist_id: string; name: string }
  | { kind: "playlist_delete"; playlist_id: string }
  | {
      kind: "playlist_add_track";
      playlist_id: string;
      track_id: string;
      position: number;
    }
  | { kind: "playlist_remove_track"; playlist_id: string; position: number }
  | {
      kind: "playlist_reorder_track";
      playlist_id: string;
      from_position: number;
      to_position: number;
    };

/** Full reconcile cycle. Requires a configured server + credential. */
export const syncNow = () => invoke<SyncReport>("sync_now");

/** Count of queued offline edits awaiting sync. */
export const syncPendingCount = () => invoke<number>("sync_pending_count");

/** Append a typed op to the offline-edit outbox. Returns the new op id. */
export const syncEnqueueOp = (op: PendingOp) =>
  invoke<number>("sync_enqueue_op", { op });

// ---------------------------------------------------------------------------
// downloads (Phase 6)
//
// `download_track` / `download_album` / `download_playlist` fetch files +
// cover art into app storage and write the cache rows that make them
// playable offline. Progress is reported via the `download-progress` Tauri
// event (see `onDownloadProgress`). `download_delete` removes a file + its
// cache row (and prunes the album cover when the album is emptied).
// ---------------------------------------------------------------------------

export type TrackDownloadResult = {
  track_id: string;
  local_path: string;
  bytes: number;
  skipped: boolean;
};

export type BatchKind = "album" | "playlist";

export type BatchDownloadResult = {
  id: string;
  kind: BatchKind;
  total: number;
  succeeded: number;
  skipped: number;
  failed: number;
  errors: string[];
};

export type StorageUsage = {
  /** Grand total across tracks + episodes. */
  bytes: number;
  track_count: number;
  cover_count: number;
  /** Downloaded podcast episodes + their byte total (included in `bytes`). */
  episode_count: number;
  episode_bytes: number;
};

export type ProgressScope = "track" | "batch";
export type ProgressPhase = "start" | "progress" | "done" | "error";

export type ProgressEvent = {
  scope: ProgressScope;
  id: string;
  phase: ProgressPhase;
  received?: number;
  total?: number;
  track_id?: string;
  index?: number;
  total_tracks?: number;
  message?: string;
};

export const downloadTrack = (trackId: string) =>
  invoke<TrackDownloadResult>("download_track", { trackId });
export const downloadAlbum = (albumId: string) =>
  invoke<BatchDownloadResult>("download_album", { albumId });
export const downloadPlaylist = (playlistId: string) =>
  invoke<BatchDownloadResult>("download_playlist", { playlistId });
export const downloadDelete = (trackId: string) =>
  invoke<void>("download_delete", { trackId });
export const downloadsStorageUsage = () =>
  invoke<StorageUsage>("downloads_storage_usage");
export const downloadsDir = () => invoke<string>("downloads_dir");
export const downloadsSetDir = (path: string) =>
  invoke<void>("downloads_set_dir", { path });
export const downloadsWifiOnly = () => invoke<boolean>("downloads_wifi_only");
export const downloadsSetWifiOnly = (on: boolean) =>
  invoke<void>("downloads_set_wifi_only", { on });

/** Download one podcast episode for offline use (reuses the track pipeline). */
export const podcastDownloadEpisode = (episodeId: string) =>
  invoke<TrackDownloadResult>("podcast_download_episode", { episodeId });
/** Download the newest N undownloaded episodes of a show (default 10). */
export const podcastDownloadShow = (podcastId: string, newestN?: number) =>
  invoke<BatchDownloadResult>("podcast_download_show", {
    podcastId,
    newestN: newestN ?? null,
  });
/** Remove a downloaded episode (file + cache row). */
export const podcastDeleteEpisode = (episodeId: string) =>
  invoke<void>("podcast_delete_episode", { episodeId });

/**
 * Subscribe to download-progress events. Returns an unlisten fn.
 * Callers aggregate by `id` to render a per-track / per-batch bar.
 */
export async function onDownloadProgress(
  cb: (e: ProgressEvent) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<ProgressEvent>("download-progress", (e) => cb(e.payload));
}

// Windows/Android don't allow custom URI schemes in the WebView, so they use
// the `http://<scheme>.localhost` form (mirrors `player_media_url`'s split).
function coverScheme(
  path: string,
  opts?: { version?: string | number; lowres?: boolean },
): string {
  const isWinLike =
    typeof navigator !== "undefined" && /Windows|Android/i.test(navigator.userAgent);
  const base = isWinLike ? `http://cover.localhost/${path}` : `cover://localhost/${path}`;
  // `?lowres=1` selects the tiny placeholder variant; `?v=` busts the cache
  // after a re-upload. Order is fixed so the native cache key stays stable.
  const params: string[] = [];
  if (opts?.lowres) params.push("lowres=1");
  if (opts?.version != null) params.push(`v=${encodeURIComponent(String(opts.version))}`);
  return params.length ? `${base}?${params.join("&")}` : base;
}

/**
 * Platform-correct `cover://` URL for an album cover (local-then-server,
 * cached + optimized by the native side). `lowres` requests the tiny
 * placeholder variant for blur-up.
 */
export function coverUrl(albumId: string, version?: string | number, lowres = false): string {
  return coverScheme(encodeURIComponent(albumId), { version, lowres });
}

/** Platform-correct URL for an artist image (server-proxied; no offline copy). */
export function artistImageUrl(artistId: string, version?: string | number, lowres = false): string {
  return coverScheme(`artist/${encodeURIComponent(artistId)}`, { version, lowres });
}

// ---------------------------------------------------------------------------
// offline cache (Phase 1) — types mirror `cache::model` 1:1
// ---------------------------------------------------------------------------

export type Artist = {
  id: string;
  name: string;
  sort_name: string | null;
  updated_at: string;
};

export type Album = {
  id: string;
  artist_id: string;
  title: string;
  release_year: number | null;
  updated_at: string;
};

export type AlbumArt = {
  album_id: string;
  local_cover_path: string;
  fetched_at: string;
};

export type Track = {
  id: string;
  album_id: string;
  artist_id: string;
  title: string;
  track_no: number | null;
  disc_no: number | null;
  duration_ms: number;
  codec: string;
  bitrate_kbps: number | null;
  file_size: number | null;
  local_file_path: string;
  metadata_json: string;
  downloaded_at: string;
  updated_at: string;
};

export type Playlist = {
  id: string;
  owner_id: string;
  name: string;
  updated_at: string;
};

export type PlaylistTrack = {
  playlist_id: string;
  track_id: string;
  position: number;
  added_at: string;
};

export type SyncEntityType = "artist" | "album" | "track" | "playlist" | "album_art";

export type SyncState = {
  entity_type: SyncEntityType;
  entity_id: string;
  server_version: string | null;
  server_etag: string | null;
  last_synced_at: string;
};

// --- artists ---------------------------------------------------------------

export const cacheUpsertArtist = (artist: Artist) =>
  invoke<void>("cache_upsert_artist", { artist });
export const cacheGetArtist = (id: string) =>
  invoke<Artist | null>("cache_get_artist", { id });
export const cacheListArtists = () => invoke<Artist[]>("cache_list_artists");
export const cacheDeleteArtist = (id: string) =>
  invoke<void>("cache_delete_artist", { id });

// --- albums ----------------------------------------------------------------

export const cacheUpsertAlbum = (album: Album) =>
  invoke<void>("cache_upsert_album", { album });
export const cacheGetAlbum = (id: string) =>
  invoke<Album | null>("cache_get_album", { id });
export const cacheListAlbumsByArtist = (artistId: string) =>
  invoke<Album[]>("cache_list_albums_by_artist", { artistId });
export const cacheDeleteAlbum = (id: string) =>
  invoke<void>("cache_delete_album", { id });

// --- album_art -------------------------------------------------------------

export const cacheUpsertAlbumArt = (art: AlbumArt) =>
  invoke<void>("cache_upsert_album_art", { art });
export const cacheGetAlbumArt = (albumId: string) =>
  invoke<AlbumArt | null>("cache_get_album_art", { albumId });
export const cacheDeleteAlbumArt = (albumId: string) =>
  invoke<void>("cache_delete_album_art", { albumId });

// --- tracks ----------------------------------------------------------------

export const cacheUpsertTrack = (track: Track) =>
  invoke<void>("cache_upsert_track", { track });
export const cacheGetTrack = (id: string) =>
  invoke<Track | null>("cache_get_track", { id });
export const cacheListTracksByAlbum = (albumId: string) =>
  invoke<Track[]>("cache_list_tracks_by_album", { albumId });
export const cacheListDownloadedTracks = () =>
  invoke<Track[]>("cache_list_downloaded_tracks");
export const cacheDeleteTrack = (id: string) =>
  invoke<void>("cache_delete_track", { id });

/** A downloaded podcast episode + its show's display fields (Downloads view). */
export type DownloadedEpisode = {
  id: string;
  podcast_id: string;
  podcast_title: string;
  image_url: string | null;
  title: string;
  duration_ms: number | null;
  file_size: number | null;
};
/** Every downloaded episode across all shows, newest-downloaded first. */
export const cacheListDownloadedEpisodes = () =>
  invoke<DownloadedEpisode[]>("cache_list_downloaded_episodes");

// --- playlists -------------------------------------------------------------

export const cacheUpsertPlaylist = (playlist: Playlist) =>
  invoke<void>("cache_upsert_playlist", { playlist });
export const cacheListPlaylists = () => invoke<Playlist[]>("cache_list_playlists");
export const cacheDeletePlaylist = (id: string) =>
  invoke<void>("cache_delete_playlist", { id });
export const cacheReplacePlaylistTracks = (
  playlistId: string,
  entries: PlaylistTrack[],
) => invoke<void>("cache_replace_playlist_tracks", { playlistId, entries });
export const cacheListPlaylistTracks = (playlistId: string) =>
  invoke<PlaylistTrack[]>("cache_list_playlist_tracks", { playlistId });

// --- sync_state ------------------------------------------------------------

export const cacheUpsertSyncState = (sync: SyncState) =>
  invoke<void>("cache_upsert_sync_state", { sync });
export const cacheGetSyncState = (entityType: SyncEntityType, entityId: string) =>
  invoke<SyncState | null>("cache_get_sync_state", { entityType, entityId });

// ---------------------------------------------------------------------------
// uploads (v2) — session-oriented uploads + reports + live stream
//
// `uploadFiles` / `uploadFolder` start a background job in Rust that bundles
// every picked file into ONE upload session, computing a per-chunk SHA-256 the
// server verifies on arrival. Progress arrives via `upload-progress` and a
// final `upload-complete` event (+ an OS notification). Reports are queryable
// (`uploadsList` / `uploadsGet`), cancellable (`uploadsCancel`), and broadcast
// live to admins/owners over `upload-event` (`uploadsSubscribe`).
//
// On Android the picker returns `content://` URIs which the Upload route hands
// here as `UploadItem`s; bytes are read natively in Rust (never the WebView).
// ---------------------------------------------------------------------------

export type UploadLifecycle =
  | "initialized"
  | "uploading"
  | "paused"
  | "completed"
  | "cancelled";

/** A resolved upload source — a desktop path or an Android `content://` URI. */
export type UploadItem = { path: string };

/** Start a background upload of resolved files (one session). Returns the job id. */
export const uploadFiles = (items: UploadItem[]) =>
  invoke<string>("upload_files", { items });

/** Start a background folder upload (desktop, one session). Returns the job id. */
export const uploadFolder = (dirPath: string) =>
  invoke<string>("upload_folder", { dirPath });

// ----- Reports -----

export type UploadChunkView = {
  index: number;
  start: number;
  end: number;
  hash: string;
  received: boolean;
};

export type UploadFileView = {
  file_index: number;
  filename: string;
  file_hash: string;
  total_size: number;
  chunk_size: number;
  total_chunks: number;
  received_chunks: number;
  state: string;
  error: string | null;
  chunks: UploadChunkView[];
};

export type UploadReport = {
  id: string;
  user_id: string | null;
  state: UploadLifecycle;
  total_files: number;
  total_bytes: number;
  bytes_received: number;
  created_at: string;
  updated_at: string;
  error: string | null;
  // Aggregated ingest report once completed: { files, tracks_ingested, files_failed }.
  report: Record<string, unknown> | null;
  files: UploadFileView[];
};

export type UploadSummary = {
  id: string;
  user_id: string | null;
  state: UploadLifecycle;
  total_files: number;
  total_bytes: number;
  created_at: string;
  updated_at: string;
  error: string | null;
};

export type UploadListFilter = {
  user_id?: string | null;
  state?: string | null;
  limit?: number | null;
  offset?: number | null;
};

/** List upload reports (own by default; admins may pass `user_id`). */
export const uploadsList = (filter?: UploadListFilter) =>
  invoke<UploadSummary[]>("uploads_list", { filter: filter ?? null });

/** Fetch one upload report with per-file/per-chunk detail. */
export const uploadsGet = (id: string) =>
  invoke<UploadReport>("uploads_get", { id });

/** Cancel an in-flight upload (owner/admin); staged chunks are cleaned up. */
export const uploadsCancel = (id: string) =>
  invoke<UploadReport>("uploads_cancel", { id });

/** Pause an in-flight upload (owner/admin); the session stays staged + resumable. */
export const uploadsPause = (id: string) =>
  invoke<UploadReport>("uploads_pause", { id });

/** Resume a paused upload (owner/admin); a chunk landing also auto-resumes. */
export const uploadsResume = (id: string) =>
  invoke<UploadReport>("uploads_resume", { id });

/**
 * Set how many file chunks upload concurrently (Settings → Networking). Clamped
 * server-side to `1..=16`; takes effect immediately, resizing an in-flight
 * upload's concurrency on the fly. Returns the value actually applied.
 */
export const uploadsSetConcurrency = (concurrency: number) =>
  invoke<number>("uploads_set_concurrency", { concurrency });

/**
 * Resume an upload left in flight by a previous app session (after an accidental
 * kill). No-ops if there's nothing to resume or an upload is already running.
 * Returns true if a resume was started; progress arrives over the usual events.
 */
export const uploadsResumePending = () =>
  invoke<boolean>("uploads_resume_pending");

/**
 * Whether the app has full "All files access" (Android `MANAGE_EXTERNAL_STORAGE`).
 * Always true on desktop / Android ≤10.
 */
export const storageHasAllFilesAccess = () =>
  invoke<boolean>("storage_has_all_files_access");

/** Open the system "All files access" settings screen to grant it (Android 11+). */
export const storageRequestAllFilesAccess = () =>
  invoke<void>("storage_request_all_files_access");

/** Open the live `uploads` channel; events arrive via `onUploadEvent`. */
export const uploadsSubscribe = () => invoke<void>("uploads_subscribe");

// ----- Live events -----

/** A live event broadcast over the `uploads` channel (gRPC stream / WS). */
export type UploadLiveEvent = {
  kind:
    | "initialized"
    | "progress"
    | "paused"
    | "resumed"
    | "completed"
    | "cancelled";
  upload_id: string;
  owner_id: string | null;
  state: UploadLifecycle;
  file_index: number | null;
  total_files: number;
  bytes_received: number;
  total_bytes: number;
  chunks_received: number;
  total_chunks: number;
  bytes_per_sec: number | null;
  report: Record<string, unknown> | null;
};

/** The active uploader's own per-chunk progress (local job events). */
export type UploadProgressEvent = {
  jobId: string;
  uploadId: string | null;
  phase: "scanning" | "uploading" | "finalizing" | "done";
  current: number;
  total: number;
  file: string | null;
  received: number | null;
  bytesTotal: number | null;
  sessionReceived: number | null;
  sessionTotal: number | null;
  bytesPerSec: number | null;
  ok: boolean | null;
  message: string | null;
  /** Pause transition: `true` = paused, `false` = resumed, `null` = unchanged. */
  paused: boolean | null;
  /** Why it paused: "manual" | "stalled" (set alongside `paused: true`). */
  pauseReason: string | null;
};

export type UploadCompleteEvent = {
  jobId: string;
  uploadId: string | null;
  state: string;
  totalFiles: number;
  tracksIngested: number;
  filesFailed: number;
  skipped: number;
  errors: string[];
};

/** Subscribe to the active job's per-chunk progress. Returns an unlisten fn. */
export async function onUploadProgress(
  cb: (e: UploadProgressEvent) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<UploadProgressEvent>("upload-progress", (e) => cb(e.payload));
}

/** Subscribe to the active job's completion. Returns an unlisten fn. */
export async function onUploadComplete(
  cb: (e: UploadCompleteEvent) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<UploadCompleteEvent>("upload-complete", (e) => cb(e.payload));
}

/** Subscribe to the live `uploads` broadcast (other users / admins). */
export async function onUploadEvent(
  cb: (e: UploadLiveEvent) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<UploadLiveEvent>("upload-event", (e) => cb(e.payload));
}
