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

export type MergedArtist = {
  id: string;
  name: string;
  sort_name: string | null;
  /** Server-side artist image path when set; drives whether the UI renders
   * the image (served via `artistImageUrl`). `null` for cache-only rows. */
  image_path: string | null;
  downloaded: boolean;
};

export type MergedAlbum = {
  id: string;
  artist_id: string;
  title: string;
  release_year: number | null;
  cover_path: string | null;
  local_cover_path: string | null;
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
  local_file_path: string | null;
  downloaded: boolean;
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

export const playerMediaUrl = (trackId: string) =>
  invoke<string>("player_media_url", { trackId });

/** Loopback URL for an album's cover art (fetchable by native code). */
export const playerCoverUrl = (albumId: string) =>
  invoke<string>("player_cover_url", { albumId });

/** Loopback base the native notification posts transport presses to. */
export const playerActionUrlBase = () =>
  invoke<string>("player_action_url_base");

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
  bytes: number;
  track_count: number;
  cover_count: number;
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
