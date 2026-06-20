# Music Server & App

A private, self-hosted music platform: a remote server hosts a lossless/downloaded music library and streams it to a cross-platform client app. The experience targets parity with apps like [Poweramp](https://powerampapp.com/) and Spotify, but self-owned — the server is the authority when online, and the client falls back to its offline cache when the server is unreachable.

## Repository Layout
- **[`server/`](./server/AGENTS.md)** — Rust backend. gRPC primary interface, REST fallback. Hosts the library, auth, streaming, ingest, recommendations, audit log. See [`server/AGENTS.md`](./server/AGENTS.md).
- **[`app/`](./app/AGENTS.md)** — [Tauri 2](https://tauri.app/) client (React 19 + TypeScript frontend, Rust native core). Cross-compiles to desktop + Android (iOS best effort). See [`app/AGENTS.md`](./app/AGENTS.md).

## Core Features
- **Streaming** of lossless/downloaded music from server to client.
- **Online/offline authority:** server is source of truth when reachable; client uses offline cache otherwise.
- **Recommendations** via fingerprint matching.
- **Playlists:** create / update / delete.
- **Offline use:** download and archive content for playback without the server.
- **Album artwork:** fetched automatically.
- **Metadata editing:** optional, manual, opt-in — both on upload and after upload completes.
- **Uploads:** push new music from clients individually or as archives (archive file, ISO, CD, and other popular music archiving formats).
- **Ingest folder:** a server-side folder where placed files (e.g. completed torrents) are **copied — not moved** — out and auto-organised into the library. Source files are never altered or removed.
- **Follow + notifications:** users follow artists and get notified of new releases.
- **Audit log + rollback:** every change is logged, viewable by managers/admins, and rollback-able (e.g. metadata edits).

## Architecture

```
┌─────────────────────────┐         gRPC (primary)          ┌──────────────────────────┐
│   Client App (app/)      │ ───────────────────────────────▶│   Server (server/)        │
│  Tauri 2 + React + TS    │         REST  (fallback)        │   Rust · gRPC + REST      │
│  desktop / Android / iOS │ ◀───────────────────────────────│   library · auth · ingest │
│  offline cache fallback  │                                 │   audit log · streaming   │
└─────────────────────────┘                                 └──────────────────────────┘
```

- **Transport:** gRPC is the primary interface; REST is a feature-parity fallback. Both share the same auth and permission enforcement. The client mirrors this gRPC-primary/REST-fallback preference.
- **Authority model:** server wins when online; client degrades gracefully to its offline cache.
- **Native work** (filesystem cache, secure credential storage, downloads) lives in the client's Rust side via `#[tauri::command]`; React handles UI/state.

## Authentication & Authorization
Both gRPC and REST use the same auth. Two mechanisms:
1. **`SECRET_KEY`** environment variable — pre-shared key. Changeable at runtime via the admin UI or by an authenticated admin client.
2. **Username/password** accounts — multiple accounts with tiered permissions.

Permission levels, each inheriting the level below:
- **Admin:** modify everything, including other accounts' permissions and levels.
- **Manager:** manage the music library (create, update, delete, change metadata, etc.). Cannot change settings or other users.
- **User:** read-only. Can still download and archive for offline use.

Permission checks are **enforced server-side** on every endpoint; the client only gates UI affordances.

## Server (summary)
Rust (edition 2024). gRPC primary, REST fallback. Owns library management, streaming, fingerprint recommendations, uploads, the copy-only ingest folder, follow notifications, and the rollback-able audit log. Ships an **optional admin UI** that is **not started by default** and enabled via an environment variable (hosts runtime settings, including `SECRET_KEY` rotation).

Full detail: [`server/AGENTS.md`](./server/AGENTS.md).

## Client (summary)
[Tauri 2](https://tauri.app/) ([Rust docs](https://docs.rs/tauri/2.11.3/tauri/)) with a React 19 + TypeScript frontend (Vite) and a Rust native core. Cross-compiles to **desktop and Android** at minimum, with **best-effort iOS**. Handles playback, offline cache, login, library browse/search, playlists, uploads, opt-in metadata editing, follow/notifications, and permission-gated admin/manager + audit/rollback UI.

Full detail: [`app/AGENTS.md`](./app/AGENTS.md).

## Project-Wide Conventions
- **Ingest = copy, never move/delete** the source.
- **Every mutating action is audited and rollback-able.**
- **gRPC primary, REST fallback** — keep them at feature parity on both server and client.
- **Server enforces permissions;** clients must not assume client-side checks are sufficient.
- Keep the per-component `AGENTS.md` files updated as features land.

## Documentation Maintenance
Docs are layered by domain ownership:
- [`server/AGENTS.md`](./server/AGENTS.md) owns server status; [`app/AGENTS.md`](./app/AGENTS.md) owns client status.
- This root file owns the high-level/cross-cutting picture.
- [`README.md`](./README.md) is the public-facing mirror.

**Rule:** when work changes a domain, update that domain's `AGENTS.md` first, then propagate any high-level or status change up to this file and to [`README.md`](./README.md). Keep all **Status** sections in sync.

## Status
Server through **Phase 7 (Metadata & Artwork)**: async bootstrap, gRPC + REST transports at feature parity, Postgres pool + portable schema, argon2 auth + session tokens + tier-inheriting `Identity`, gRPC proto files (`auth.proto` + `library.proto` + `playlist.proto` + `upload.proto`) under `server/proto/`, `LibraryService` with Manager+ gating + before/after audit writes, `ScanService` indexing on-disk files via `lofty`, `StreamingService` + `GET /tracks/:id/stream` with full RFC 7233 byte-range semantics (200/206/416), safe path resolution under `LIBRARY_PATH`, and `Accept-Ranges`/`Content-Range`/`Last-Modified` headers, and `PlaylistService` covering CRUD + 1-based contiguous track ordering (append, insert-with-shift, remove-with-shift, forward/backward reorder via tx-safe negate-shift) gated to playlist owner-or-`Manager+` with per-mutation `playlist.*` audit entries, surfaced over both `music.playlist.v1` gRPC and a `/playlists/...` REST tree.  Phase 6 adds a shared `tag` module for metadata extraction (`TagInfo` + `read_tags`), an `Organizer` for `Artist/Album/Track.ext` copy-only library layout with `sanitize()` path components, an `IngestService` orchestrating read-tags → upsert artist+album → copy → `ScanService.index_file`, a background `notify` folder watcher with per-path debounce (300 ms settle + in-flight `HashSet`), and two REST endpoints (`POST /upload` multipart with 500 MiB cap + `POST /ingest/scan`) gated to Manager+, plus archive ingest (`services::archive` extracting zip + `.tar`/`.tar.gz`/`.tar.bz2`/`.tar.xz` with zip-slip-guarded `safe_join`; ISO/CD recognised but stubbed) wired into `POST /upload` and a client-streaming `music.upload.v1` `UploadService` gRPC (`UploadInfo` + chunked bytes, 500 MiB cap). Phase 7 adds opt-in metadata editing (`MetadataService` wrapping audited `update_track` with optional `lofty` tag write-back gated by `WRITE_TAGS`), automatic album artwork (`ArtworkService` + `CoverArtSource` trait; `CoverArtArchive` resolves a MusicBrainz release MBID then pulls the Cover Art Archive front cover, caches it under `ARTWORK_PATH`, and updates the album `cover_path` via audited `update_album`, gated by `FETCH_ARTWORK`), surfaced over gRPC (`EditTrackMetadata` + `FetchAlbumArtwork`) and REST (`PATCH /tracks/:id/metadata` + `POST /albums/:id/artwork`). Client through **Phase 5 (Sync Engine)**: Tauri 2 + React 19 + Vite shell; embedded `sqlx` SQLite cache; gRPC-primary / REST-fallback `ServerClient` over the server's `auth.proto` + `library.proto`; `AuthManager` over OS-keychain (desktop) / app-private file (Android) secure store handling `SECRET_KEY` + username/password; `LibraryService` returning `MergedArtist`/`MergedAlbum`/`MergedTrack` rows with per-row `downloaded` flags + a `LibraryView { source, items, total? }` wrapper that falls back to the SQLite cache on transport errors or missing credentials; six `library_*` Tauri commands + mirrored TS bindings + `/library`, `/artists/:id`, `/albums/:id`, `/search` routes. Phase 4 adds a custom **`media://` URI-scheme protocol** that, per request, serves a downloaded local file with full RFC 7233 byte-range semantics or proxies the server's `GET /tracks/{id}/stream` with the auth header injected + `Range` forwarded (prefer-local-else-stream resolution lives in Rust; the session token stays out of the webview), a `player_media_url` command for the platform-correct URL, and a Zustand player store + persistent `PlayerBar` (queue, shuffle, repeat, seek, volume, click-to-play from Album/Search) wired into the OS Media Session API for media-key/lock-screen control. Phase 5 adds a **sync engine**: transport get-by-id (artist/album/track, 404→prune) + full playlist ops on both gRPC + REST, a `pending_ops` offline-edit outbox replayed FIFO on reconnect (local-id remap; server-authority conflict resolution), content-hash reconcile of cached entities (`updated_at` isn't on the wire) preserving client-owned download fields, and a missing-file prune — surfaced via `sync_now`/`sync_pending_count`/`sync_enqueue_op` and auto-triggered on online-regain + window focus. Downloads, playlists UI, and uploads/ingest still pending on the client side; full desktop-native now-playing (SMTC/MPRIS/macOS) and Android background-playback service are deferred/best-effort beyond Media Session.
