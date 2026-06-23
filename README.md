# Music Server & App

A private, self-hosted music platform. A remote **server** hosts a lossless/downloaded music library and streams it to a cross-platform **client app**. The goal is parity with apps like [Poweramp](https://powerampapp.com/) and Spotify — but self-owned: the server is the authority when online, and the client falls back to its offline cache when the server is unreachable.

## Features
- **Streaming** of lossless/downloaded music from server to client.
- **Online/offline authority** — server is source of truth when reachable; client uses its offline cache otherwise.
- **Fingerprint-based recommendations.**
- **Playlists** — create / update / delete.
- **Offline use** — download and archive content for playback without the server.
- **Automatic album artwork.**
- **Opt-in metadata editing** — manual, both on upload and afterwards.
- **Uploads** — push new music individually or as archives (archive file, ISO, CD, and other popular formats).
- **Ingest folder** — files placed there (e.g. completed torrents) are **copied, not moved**, then auto-organised into the library; sources are never altered.
- **Follow + notifications** — follow artists and get notified of new releases.
- **Audit log + rollback** — every change is logged, viewable by managers/admins, and reversible.

## Architecture

```
┌─────────────────────────┐         gRPC (primary)          ┌──────────────────────────┐
│   Client App (app/)      │ ───────────────────────────────▶│   Server (server/)        │
│  Tauri 2 + React + TS    │         REST  (fallback)        │   Rust · gRPC + REST      │
│  desktop / Android / iOS │ ◀───────────────────────────────│   library · auth · ingest │
│  offline cache fallback  │                                 │   audit log · streaming   │
└─────────────────────────┘                                 └──────────────────────────┘
```

| Component | Path | Stack | Details |
|-----------|------|-------|---------|
| **Server** | [`server/`](./server) | Rust (edition 2024), gRPC primary + REST fallback | [`server/AGENTS.md`](./server/AGENTS.md) |
| **Client** | [`app/`](./app) | Tauri 2, React 19 + TypeScript, Vite | [`app/AGENTS.md`](./app/AGENTS.md) |

## Authentication
Both gRPC and REST share one auth layer with two mechanisms:
1. **`SECRET_KEY`** env var — pre-shared key, runtime-changeable via the admin UI or an authenticated admin client.
2. **Username/password** accounts with tiered permissions (each inherits the level below):
   - **Admin** — modify everything, including other accounts.
   - **Manager** — manage the library (create/update/delete, metadata). Cannot change settings or users.
   - **User** — read-only; can still download and archive for offline use.

Permissions are **enforced server-side**; the client only gates UI.

## Getting Started

### Server
```sh
cd server
cargo build
cargo run
cargo test
```
Optional admin UI is **not started by default**; enable it via environment variable.

### Client
```sh
cd app
npm install
npm run tauri dev      # full desktop app
npm run dev            # frontend only (browser)
npm run tauri build    # production build
```
Mobile (after `tauri android/ios init`):
```sh
npm run tauri android dev
npm run tauri ios dev   # best effort
```

## Status
Server through **Phase 7 (Metadata & Artwork)**. Client through **Phase 9 (Metadata Editing)**.

| Domain | Status | Tracked in |
|--------|--------|-----------|
| Server | Phases 0–7 done — async bootstrap, gRPC + REST at feature parity, Postgres schema + repos, argon2 auth + tier inheritance, `auth.proto` + `library.proto` + `playlist.proto`, full Library CRUD/search/list + audit writes, `lofty`-based library scan, `GET /tracks/:id/stream` with RFC 7233 byte-range (200/206/416) + safe path resolution under `LIBRARY_PATH`, `PlaylistService` with owner-or-Manager+ gating + 1-based contiguous track ordering (insert/remove/reorder via tx-safe negate-shift) + per-mutation audit, shared `tag` module for metadata extraction, `organizer` for `Artist/Album/Track.ext` copy-only layout, `IngestService` orchestrating read-tags/upsert/copy/index, background `notify` folder watcher, `POST /upload` (multipart, 500 MiB) + `POST /ingest/scan` REST endpoints behind Manager+ auth, opt-in metadata editing (`EditTrackMetadata` / `PATCH /tracks/:id/metadata`) with optional `lofty` tag write-back (`WRITE_TAGS`), automatic album artwork via MusicBrainz + Cover Art Archive cached under `ARTWORK_PATH` (`FetchAlbumArtwork` / `POST /albums/:id/artwork`, `FETCH_ARTWORK`), archive ingest (zip + `.tar`/`.tar.gz`/`.tar.bz2`/`.tar.xz`, zip-slip-guarded; ISO/CD stubbed) via `POST /upload` + a client-streaming `UploadService` gRPC. **Uploads v2** adds a DB-backed, session-oriented subsystem (migration `20260201000000`): `POST /uploads/init` declares a session — a *list* of files, each with a recombined-file hash + per-chunk hashes the server re-verifies on arrival (a corrupt chunk is rejected); chunked `POST /uploads/:id/files/:fi/chunks/:ci`; queryable reports (`GET /uploads` own-by-default/admin-all + `?user_id=&state=`, `GET /uploads/:id` with per-track ingest detail); `POST /uploads/:id/cancel` (cleans staged chunks); one active upload per user; and live progress over gRPC `StreamUploads` + a REST `GET /uploads/stream` WebSocket (per-listener filtered). Inter-phase: **password change** (self-service + admin reset, audited `user.password_change`), **user list** (`GET /users`, admin-gated), and **account deletion** (`DELETE /users/:id`, admin-gated, cascades sessions/playlists/follows, `audit_log.actor_id → SET NULL`, audited `user.delete`). **Manual artwork upload** (Manager+) extends Phase 7: `artists.image_path` (migration + `Artist` proto/DTO), `ArtworkService` decoupled from `FETCH_ARTWORK`, and REST `POST /albums/:id/cover` + `POST /artists/:id/image` + `GET /artists/:id/image` (binary uploads REST-only). Now **84 lib tests**. | [`server/AGENTS.md`](./server/AGENTS.md) |
| Client | Phases 0–9 done — Tauri 2 + React 19 + Vite shell, embedded SQLite cache, gRPC-primary/REST-fallback `ServerClient` + `AuthManager` (Keychain on desktop / app-private file on Android) with `SECRET_KEY` + username/password, `LibraryService` with downloaded-flag merging and offline cache fallback, six `library_*` Tauri commands + mirrored TS bindings, `/library`, `/artists/:id`, `/albums/:id`, `/search` routes, **playback** via a custom `media://` protocol (range-aware local files / auth-injected server-stream proxy) + a Zustand player store and persistent `PlayerBar` (queue, shuffle, repeat, seek, volume, Media Session integration), a **sync engine** (get-by-id + playlist ops on both transports, `pending_ops` offline-edit outbox replayed FIFO on reconnect with server-authority conflicts, content-hash reconcile, missing-file prune, auto-triggered on online-regain + focus), and **offline downloads** — a `DownloadManager` streaming from `GET /tracks/{id}/stream` into `.part` files with Range-based resume + atomic rename, single/album/playlist batches (downloaded tracks skip), best-effort client-side Cover Art Archive cover fetch, delete flow pruning files + cache rows + empty-album covers, storage accounting, configurable downloads root + Wi-Fi-only toggle (new `settings` table / migration `0003`), `download-progress` Tauri events, a second `cover://` protocol serving local covers, nine `download_*`/`downloads_*` commands, a `/downloads` route, and per-track Download/Remove buttons + album covers in the Artist grid and now-playing bar. Phase 7 adds **playlists** — a `PlaylistService` with per-mutation online/offline routing (server push + cache mirror, or outbox + optimistic cache splice for `Transport`/`AuthNotConfigured`/`local:` ids), `local:`-id lifecycle with dependent-op pruning on delete, `PlaylistDetailView` entries reusing `MergedTrack`, eight `playlist_*` commands, and `/playlists` + `/playlists/:id` routes (create/rename/delete, Play-all, Download-all, inline track-search-add, ↑/↓ reorder + remove, owner-or-Manager+ gating). Desktop-native now-playing (SMTC/MPRIS/macOS) and Android background-playback service deferred/best-effort. Inter-phase: **account management** (password change, user-list dropdown, account deletion — all admin-gated, server-audited), **session persistence** (username+password sessions auto-restore on app restart from keychain, `SECRET_KEY` excluded), a **permanent sidebar** (flex-rail nav on every authenticated route), and a **sync scheduler** with 1 s reconnect probe + pending-change detection + 30 s floor sync. **Uploads v2**: a pick becomes one session (per-chunk SHA-256 + whole-file hash, gRPC-primary / REST-fallback `init`/`put_chunk`/`get`/`list`/`cancel` + a `subscribe` over gRPC-stream-or-WebSocket); upload state moved to a global store so it survives tab switches, with a global completion listener that refreshes the library (fixing "uploads never appear"), one-at-a-time gating, and an `/uploads` reports route (list + per-file/per-chunk detail + ingest report). Phase 9 adds **opt-in metadata editing** (Manager+, client-only — the Phase-7 `EditTrackMetadata` / `PATCH /tracks/:id/metadata` server contract already covers it): a `MetadataEdit` transport type + `edit_track_metadata` across gRPC/REST/`ServerClient`/`AuthManager`, a `LibraryService.edit_track_metadata` pushing to the server then mirroring into the offline cache for downloaded items (reconcile-on-sync already handled by the Phase-5 track content-hash), the `library_edit_track_metadata` command, and a reusable `MetadataEditor` modal — **single-track** (mobile bottom-sheet) and **batch** (desktop table + bulk helpers, only changed rows saved) — wired into the Album route (per-track ✏ + desktop "Edit tags") and the upload-report ingest list (on-upload), sending only the touched fields. **Artwork upload** adds manual album-cover + artist-image upload (Manager+): files read natively (content-type sniffed, 16 MiB cap) + pushed over REST, the `cover://` scheme extended to proxy artist images, and a reusable `ImageUploader` modal on the Album hero cover + a new Artist-route image hero. | [`app/AGENTS.md`](./app/AGENTS.md) |
| High-level | Server Phase 7 + client Phase 9 complete · **Uploads v2** (DB-backed sessions, per-chunk verification, reports, gRPC-stream + WebSocket live updates) · **opt-in metadata editing** (Manager+, single + batch) · **artwork upload** (manual album-cover + artist-image upload, Manager+) | [`AGENTS.md`](./AGENTS.md) |

## Documentation Map
This repo keeps documentation layered so each domain owns its own status:

- **[`AGENTS.md`](./AGENTS.md)** — whole-system overview, architecture, cross-cutting conventions.
- **[`server/AGENTS.md`](./server/AGENTS.md)** — server architecture, responsibilities, env vars, status.
- **[`app/AGENTS.md`](./app/AGENTS.md)** — client architecture, platforms, build, status.

**Maintenance rule:** when work changes a domain, update that domain's `AGENTS.md` first, then propagate high-level/status changes up to the root [`AGENTS.md`](./AGENTS.md) and this `README.md`. Keep the **Status** sections here and in each `AGENTS.md` in sync.
