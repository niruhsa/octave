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

/** Platform-correct `cover://` URL for a downloaded album cover. */
export function coverUrl(albumId: string): string {
  // Mirrors `player_media_url`'s platform split: Windows/Android need the
  // `http://<scheme>.localhost` form because they don't allow custom schemes.
  const isWinLike =
    typeof navigator !== "undefined" && /Windows|Android/i.test(navigator.userAgent);
  return isWinLike
    ? `http://cover.localhost/${encodeURIComponent(albumId)}`
    : `cover://localhost/${encodeURIComponent(albumId)}`;
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
// uploads (Phase 8)
//
// `uploadFile` takes a local file path (from a native dialog picker),
// reads the bytes in Rust, and pushes them to the server via gRPC
// (client-streaming) or REST (multipart) fallback. Manager+ gated.
// ---------------------------------------------------------------------------

export type SingleUploadResult = {
  track_id: string;
  path: string;
};

export type ArchiveUploadResult = {
  kind: string;
  ingested: number;
  already_indexed: number;
  non_audio_skipped: number;
  errors: number;
  track_ids: string[];
};

export type UploadResult =
  | { variant: "single"; data: SingleUploadResult }
  | { variant: "archive"; data: ArchiveUploadResult };

/** Upload a single file (audio track or archive) from a local path. */
export const uploadFile = (path: string) =>
  invoke<UploadResult>("upload_file", { path });

export type FolderUploadResult = {
  total: number;
  succeeded: number;
  failed: number;
  skipped: number;
  errors: string[];
};

/** Upload every audio + archive file in a directory tree. */
export const uploadFolder = (dirPath: string) =>
  invoke<FolderUploadResult>("upload_folder", { dirPath });

export type UploadProgressEvent = {
  phase: "scanning" | "uploading" | "done";
  current: number;
  total: number;
  file?: string;
  ok?: boolean;
  message?: string;
};

/** Subscribe to upload-progress events. Returns an unlisten fn. */
export async function onUploadProgress(
  cb: (e: UploadProgressEvent) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<UploadProgressEvent>("upload-progress", (e) => cb(e.payload));
}
