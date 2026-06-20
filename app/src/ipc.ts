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
