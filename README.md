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
Server through **Phase 6 (Uploads & Ingest)**. Client through **Phase 3 (Library Browse & Search)**.

| Domain | Status | Tracked in |
|--------|--------|-----------|
| Server | Phases 0–6 in progress — async bootstrap, gRPC + REST at feature parity, Postgres schema + repos, argon2 auth + tier inheritance, `auth.proto` + `library.proto` + `playlist.proto`, full Library CRUD/search/list + audit writes, `lofty`-based library scan, `GET /tracks/:id/stream` with RFC 7233 byte-range (200/206/416) + safe path resolution under `LIBRARY_PATH`, `PlaylistService` with owner-or-Manager+ gating + 1-based contiguous track ordering (insert/remove/reorder via tx-safe negate-shift) + per-mutation audit, shared `tag` module for metadata extraction, `organizer` for `Artist/Album/Track.ext` copy-only layout, `IngestService` orchestrating read-tags/upsert/copy/index, background `notify` folder watcher, `POST /upload` (multipart, 500 MiB) + `POST /ingest/scan` REST endpoints behind Manager+ auth. No archive/ISO/CD upload yet, no artwork/metadata write-back. | [`server/AGENTS.md`](./server/AGENTS.md) |
| Client | Phases 0–3 done — Tauri 2 + React 19 + Vite shell, embedded SQLite cache, gRPC-primary/REST-fallback `ServerClient` + `AuthManager` (Keychain on desktop / app-private file on Android) with `SECRET_KEY` + username/password, `LibraryService` with downloaded-flag merging and offline cache fallback, six `library_*` Tauri commands + mirrored TS bindings, `/library`, `/artists/:id`, `/albums/:id`, `/search` routes. | [`app/AGENTS.md`](./app/AGENTS.md) |
| High-level | Server Phase 6 in progress + client Phase 3 complete | [`AGENTS.md`](./AGENTS.md) |

## Documentation Map
This repo keeps documentation layered so each domain owns its own status:

- **[`AGENTS.md`](./AGENTS.md)** — whole-system overview, architecture, cross-cutting conventions.
- **[`server/AGENTS.md`](./server/AGENTS.md)** — server architecture, responsibilities, env vars, status.
- **[`app/AGENTS.md`](./app/AGENTS.md)** — client architecture, platforms, build, status.

**Maintenance rule:** when work changes a domain, update that domain's `AGENTS.md` first, then propagate high-level/status changes up to the root [`AGENTS.md`](./AGENTS.md) and this `README.md`. Keep the **Status** sections here and in each `AGENTS.md` in sync.
