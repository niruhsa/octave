# Music Server — Agent Guide

Server-side of the Music Server & App project. See the root [`AGENTS.md`](../AGENTS.md) for whole-system context. This file covers the server only.

## Purpose
Authoritative backend that hosts lossless/downloaded music and streams it to desktop/mobile clients. When online, the server is the source of truth; clients fall back to their offline cache when it is unreachable.

## Tech Stack
- **Language:** Rust (edition 2024).
- **Primary interface:** gRPC.
- **Fallback interface:** REST.
- **Crate name:** `server` (see [`Cargo.toml`](./Cargo.toml)).
- Entry point: [`src/main.rs`](./src/main.rs).

## Responsibilities
- **Streaming:** serve lossless/downloaded audio to authenticated clients.
- **Library management:** create / update / delete tracks, albums, artists, metadata.
- **Recommendations:** fingerprint-matching based music recommendation.
- **Playlists:** create / update / delete.
- **Offline support:** allow clients to download and archive content for offline use.
- **Artwork:** automatic album artwork fetching.
- **Metadata editing:** optional, manual, opt-in — on upload and after upload completes.
- **Uploads:** accept new music from clients individually or as archives (archive file, ISO, CD, and other popular music archiving formats).
- **Ingest folder:** watch a server-side ingest dir. Files placed there (e.g. completed torrents) are **copied — not moved** — out and auto-organised into the library. Never delete or move the source.
- **Follow notifications:** notify users when a followed artist has a new release.
- **Audit log:** record every change with viewer access for managers/admins. Changes must be **rollback-able** (e.g. metadata edits).

## Authentication & Authorization
Both gRPC and REST use the same auth. Two mechanisms:
1. **`SECRET_KEY`** env var — pre-shared key. Changeable at runtime via the admin UI or by an authenticated admin client.
2. **Username/password** accounts — multiple accounts, tiered permissions.

Permission levels (each inherits the level below):
- **Admin:** modify everything, including other accounts' permissions/levels.
- **Manager:** manage the music library (create, update, delete, metadata changes). Cannot change settings or other users.
- **User:** read-only. Can still download and archive for offline use.

## Admin UI
- Optional. **Not started by default.**
- Enabled via an environment variable.
- Hosts runtime settings, including `SECRET_KEY` rotation.

## Environment Variables
- `SECRET_KEY` — pre-shared auth key (runtime-changeable). **Required.**
- `DATABASE_URL` — PostgreSQL connection. **Required from Phase 2 onward.**
- `GRPC_ADDR` — gRPC bind address. Default `0.0.0.0:50051`.
- `REST_ADDR` — REST bind address. Default `0.0.0.0:8080`.
- `ENABLE_ADMIN_UI` — `1`/`true` to start the optional admin UI.
- `LIBRARY_PATH` — organised library root. Path-typed (see below).
- `INGEST_PATH` — copy-only ingest folder. Path-typed (see below).
- `ENV_FILE` — explicit override for the `.env` file location.

### `.env` loading & path resolution
`Config::from_env` auto-locates a `.env` file at startup and seeds the
process environment from it. Search order:
1. `ENV_FILE` (if set).
2. Walk upward from the current working directory looking for `.env`.
3. `CARGO_MANIFEST_DIR/.env` (compile-time anchor; works for `cargo run`).
4. None — process env is used as-is.

The directory containing the loaded `.env` becomes the **config anchor**.
All path-typed env vars (`LIBRARY_PATH`, `INGEST_PATH`) resolve relative
to that anchor when not absolute, so the server behaves the same
regardless of which directory it is launched from. Copy [`.env.example`](./.env.example)
to `.env` to get started; `.env` is gitignored.

Deployment is via Docker Compose (server + PostgreSQL); see [`PLAN.md`](./PLAN.md) Phase 13.

> Document each env var here as it is added.

## Conventions
- Ingest = **copy**, never move/delete source files.
- Every mutating action writes an audit log entry that supports rollback.
- gRPC is primary; keep REST as a feature-parity fallback for the same auth and operations.
- Permission checks enforced server-side on every gRPC and REST endpoint.

## Build & Run
```sh
cd server
cargo build
cargo run
cargo test
```

## Documentation Maintenance
This file owns the **server's** status and detail. When server work changes behaviour, config, or scope:
1. Update this file first (responsibilities, env vars, status).
2. Propagate any high-level/status change up to the root [`AGENTS.md`](../AGENTS.md) and [`README.md`](../README.md).

Keep the **Status** section below in sync with the root docs.

## Status
**Phases 0–5 complete (Scaffold, Persistence & Schema, Auth & Authorization, Library Management CRUD, Streaming, Playlists).**

**Phase 6 — Uploads & Ingest (in progress)**
- New `services::tag` module ([`services/tag.rs`](./src/services/tag.rs)): shared `TagInfo` struct + `read_tags(path)` helper used by scanner, uploader, and watcher alike.  `is_audio_file` + `AUDIO_EXTS` constant centralise extension checks.  Tag extraction falls back to `Unknown` / filename for missing metadata; `lofty` errors propagate up so callers get a clear 500.  Two organising helpers live here too: **`primary_artist(raw)`** strips collaboration suffixes (`feat.` / `ft.` / `featuring` / `vs.` / `with`) and splits on the first hard separator (` & `, `, `, `; `, ` × `, ` + `, ` and `, ` x `) so one artist's catalog stays in one folder — `TPE2`/`AlbumArtist` is preferred over `TPE1`/`TrackArtist` for the same reason; **`infer_language(name)`** maps the Unicode script of the artist name to a folder label (kana → Japanese, Hangul → Korean, Han → Chinese, Cyrillic/Arabic/Hebrew/Greek/Devanagari/Thai → their language; default English).  Kana and Hangul are treated as language-exclusive so mixed names (`宇多田ヒカル`) bucket correctly.  An explicit `Language` / `TLAN` tag (ISO-639-1/2 code or English/native name) overrides script inference via `normalize_language()`.
- New `services::organizer` module ([`services/organizer.rs`](./src/services/organizer.rs)): `Organizer` owns the `LIBRARY_PATH` root, computes `<root>/<Language>/<Artist>/<Album>/<NN - Title>.<ext>` destinations with `sanitize()` (replaces `/\:*?"<>|` with `_`, strips leading/trailing dots+underscores, defaults to `Unknown` on empty), and copies the file there — **source is never moved or deleted**.  `Language` is the top-level folder so an artist's discography lives under their primary language; `Artist` is the **primary artist only** (collab suffixes stripped upstream).  Idempotent: same-size destination is skipped.  7 unit tests (sanitize passthrough, special chars, empty-fallback, basic destination, Japanese-script destination, unknown-metadata destination).
- Refactored `services::scan` ([`services/scan.rs`](./src/services/scan.rs)): `extract_tags` lifted into shared `tag` module; added `pub async fn index_file(caller, path) -> Result<Option<Uuid>>` so the ingest pipeline can index a single file after the organizer has copied it to its final location.  Existing `scan()` still works unchanged.
- New `services::ingest` ([`services/ingest.rs`](./src/services/ingest.rs)): `IngestService` holds `ScanService` + `Organizer` + optional `ingest_root`; `organize_and_index(caller, source)` is the single entry-point for both direct uploads and the watcher — reads tags, upserts artist+album via the library service, copies to library, indexes via `ScanService.index_file`.
- New `services::watch` ([`services/watch.rs`](./src/services/watch.rs)): background `notify::RecommendedWatcher` on `INGEST_PATH`; `Create(File)` and `Modify(Data)` events debounced per-path (300 ms settle + in-flight set via `tokio::sync::Mutex<HashSet<PathBuf>>`); `.uploading` staging files skipped; errors logged at `warn` (already-indexed at `debug`).  Returns the watcher handle so it stays alive in `main`.
- REST routes ([`rest/ingest.rs`](./src/rest/ingest.rs)) mounted at `POST /upload` (multipart, 500 MiB cap) and `POST /ingest/scan` (walks ingest folder, copies+indexes all unseen audio).  Both require Manager+.  Upload writes to `<INGEST_PATH>/.tmp/<uuid>.uploading`, renames to real ext, calls `organize_and_index`, cleans up.  Response: `201 Created` with `{track_id, path}`.
- `Config` already had `ingest_path: Option<PathBuf>` from Phase 1; `main.rs` now constructs `IngestService` when `library_path` is set, starts the watcher, and injects `ingest: Option<IngestService>` into `RestState`.
- Cargo.toml: added `notify = { version = "6", default-features = false, features = ["macos_kqueue"] }`, enabled `axum/multipart`.
- Tests: 38 lib tests passing (was 31 after Phase 5).  New: 2 `tag::is_audio_file` tests, 5 organiser tests (sanitize, destination, unknown metadata).
- **Still TODO for Phase 6:** archive upload (zip/ISO/CD — stub for now); full gRPC parity (PlaylistService-style `UploadService` gRPC server); write-back of metadata tags to file via `lofty`; album-artwork fetch hook (Phase 7).
- New [`services::PlaylistService`](./src/services/playlist.rs): full CRUD + track-ordering surface, audit-logged on every mutation. Permission model: any authed user may read playlists / list playlist tracks; mutations require the playlist's owner *or* `Manager+`; `SECRET_KEY` is rejected from `create` because it has no `user_id` to own the row. `list_for_owner` is gated to owner-or-`Manager+` so a `User` cannot enumerate someone else's library, while `list_mine` always works for users (it uses the caller's own id). Name validation: trimmed, non-empty, ≤200 chars.
- Track ordering: positions are **1-based contiguous integers**. `add_track` appends at `max(position)+1`; `insert_track(pos)` shifts existing rows up; `remove_track_at(pos)` shifts later rows down; `reorder(from, to)` moves a row in either direction. Repo impls execute every shift inside a single tx, parking the affected rows in the negative position space so the unique `(playlist_id, position)` PK never collides mid-op.
- New `PlaylistRepo` surface: `update_name`, `insert_track_at`, `remove_track_at`, `move_track`, `next_position`, `get_track_at`, plus the legacy `list_tracks`. Postgres impl in [`db::pg`](./src/db/pg.rs) uses the negate-shift pattern for all row movements (preserves `added_at`).
- New `proto/playlist.proto` (`music.playlist.v1`) + `PlaylistServer` ([`grpc::playlist_svc`](./src/grpc/playlist_svc.rs)) mounted alongside auth/library/health. RPCs: `CreatePlaylist`, `GetPlaylist` (returns `PlaylistWithTracks`), `RenamePlaylist`, `DeletePlaylist`, `ListMyPlaylists`, `ListPlaylistsForOwner`, `ListPlaylistTracks`, `AddPlaylistTrack` (`position=0` ⇒ append), `RemovePlaylistTrack`, `ReorderPlaylistTrack`.
- REST parity ([`rest::playlist`](./src/rest/playlist.rs)): `POST /playlists`, `GET /playlists`, `GET /users/:owner_id/playlists`, `GET|PUT|DELETE /playlists/:id`, `GET|POST /playlists/:id/tracks`, `DELETE|PUT /playlists/:id/tracks/:position` — all behind the existing auth middleware, all backed by the same service so permission + audit are identical to gRPC.
- Audit actions: `playlist.create`, `playlist.update`, `playlist.delete`, `playlist.track.add`, `playlist.track.remove`, `playlist.track.reorder`; before/after JSON includes the full row(s) (the reorder entry's before/after carry the source and destination `PlaylistTrack`).
- Tests: 10 service-level unit tests against in-memory repo fakes (`FakePlaylists`, `FakeTracks`, etc.) covering create+audit, secret-key rejection, name validation, owner-vs-manager mutation gating, append + insert-with-shift, unknown-track 404, forward+backward reorder, remove+shift, owner-only `list_for_owner`, and create/delete audit sequencing. Total lib tests: **31 passing** (was 21 in Phase 4).

**Phase 4 — Streaming**
- New [`services::StreamingService`](./src/services/streaming.rs): resolves a track id to a safe on-disk path, gated by `Identity` (any authed user). Path resolution policy: `Track.file_path` is treated as absolute or relative-to-`LIBRARY_PATH`; `canonicalize` is applied and the result **must** live under the canonical library root or the request is denied (`PermissionDenied`). With no `LIBRARY_PATH` configured, only absolute `file_path` values are accepted. Missing files map to `NotFound`. Extension→MIME table covers flac/mp3/ogg/opus/m4a/wav/alac/wv/ape; everything else streams as `application/octet-stream`.
- REST endpoint `GET /tracks/:id/stream` (+ `HEAD`) in [`rest::streaming`](./src/rest/streaming.rs):
  - `200 OK` for full-body requests; `206 Partial Content` for any satisfiable `Range:` header.
  - `416 Range Not Satisfiable` (with `Content-Range: bytes */<size>`) when the range parses but is out of bounds.
  - Malformed `Range` is ignored and the full body is served (RFC 7233 §3.1).
  - Always emits `Content-Type`, `Accept-Ranges: bytes`, `Content-Length`, and `Last-Modified`; `Content-Range` on `206`.
  - Body is built from `tokio_util::io::ReaderStream` over a `seek`+`take(len)`'d file — no buffering of the whole file in memory.
- Pure [`rest::range::parse_range`](./src/rest/range.rs) handles `bytes=A-B`, `bytes=A-`, `bytes=-N`, whitespace, upper-bound clipping per RFC, and explicitly rejects multi-range (`bytes=0-9,20-29`) as malformed (we don't serve `multipart/byteranges`).
- Transcoder stub trait (PLAN §4) added but unimplemented.
- Tests: 7 range-parser unit tests + 5 streaming-service tests (relative-without-root rejection, relative-inside-root resolution, dotdot traversal blocked, missing file → NotFound, MIME mapping). All 21 lib tests pass.
- Verified end-to-end against the running server: `200`/`206`/`416`/`401` all correct, byte content matches expected pattern at multiple offsets.
- **Fixed a latent routing bug** discovered during Phase 4 smoke testing: every path-parameter route in `rest/library.rs` (and the new streaming route) used axum 0.8 `{id}` syntax, but the crate is pinned to axum 0.7 / matchit 0.7 which expects `:id`. Migrated all path params from `{id}` → `:id`. `/tracks/:id`, `/albums/:id/...`, `/artists/:id/...` etc. now actually match.

**Phases 0–3**
- Async bootstrap (`tokio`), structured logging (`tracing`), env-driven config.
- gRPC server (`tonic` + `tonic-health`) and REST `/health` (`axum`) both boot and shut down on Ctrl-C.
- Module layout: `config`, `error`, `db/` (`models`, `repo`, `pg`, `pool`), `services/`, `grpc/`, `rest/`.
- `build.rs` is wired for `tonic-build` (no-op until protos land).
- Postgres connection pool + embedded migrations (`migrations/20260101000000_init.sql`) covering `users`, `artists`, `albums`, `tracks`, `playlists` (+ `playlist_tracks`), `follows`, `audit_log`, `sessions`. Schema kept SQLite-portable for the client's offline cache.
- Async repository traits per entity (`UserRepo`, `ArtistRepo`, `AlbumRepo`, `TrackRepo`, `PlaylistRepo`, `FollowRepo`, `AuditRepo`, `SessionRepo`) with a `PgRepos` Postgres implementation.
- Bootstrap runs migrations on startup; `DATABASE_URL` is **required** from Phase 2 onward (auth/sessions live in the DB).
- Auth module (`src/auth/`): argon2id password hashing, opaque base64url session tokens (32 bytes, 30-day TTL), constant-time `SECRET_KEY` compare via `subtle`, `Identity` enum (`SecretKey` → effective Admin / `User` w/ tier), `Identity::require(tier)` inheritance check.
- `AuthService::{resolve, register, login, logout}` composed over `UserRepo` + `SessionRepo`; `register` requires Admin (re-checked inside service — defense in depth).
- REST: `POST /auth/login` (public), `/auth/whoami`, `/auth/register`, `/auth/logout` behind an axum middleware that resolves `Authorization: Bearer <token>` or `SecretKey <key>` (also `X-Secret-Key: <key>`) into `Identity`. `AppError` maps to proper HTTP statuses (401/403/400/404/500).
- gRPC: `AuthInterceptor` synchronously checks credential metadata is present + well-formed; future RPC handlers call `interceptor.resolve(&req).await` then `identity.require(tier)`.
- Unit tests for tier inheritance, secret-key=Admin, argon2 round-trip + per-call uniqueness, token uniqueness, constant-time eq (6 passing).
- **gRPC proto files** under `proto/`: `auth.proto` (AuthService: Login/Logout/WhoAmI/Register) + `library.proto` (LibraryService: CRUD/search/list/scan for artists/albums/tracks). Generated by `build.rs` via `tonic-build`.
- **LibraryService** (`src/services/library.rs`): Manager+-gated CRUD for artists/albums/tracks, ILIKE search, paginated list (cap 200, default 50), FK validation, before/after JSON audit-log write on every mutation.
- **ScanService** (`src/services/scan.rs`): walks a directory with `walkdir`, probes audio files via `lofty` (mp3/flac/wav/ogg/opus/m4a/mp4/aac/aiff/wv/ape), best-effort tag extraction (title/artist/album/track-no/disc-no/year), upserts artist+album, inserts track. Idempotent via `tracks.file_path` uniqueness. Manager+.
- **gRPC services**: `AuthServer` + `LibraryServer` mounted alongside health; handlers call `interceptor.resolve(&req).await` then `identity.require(tier)`. `AppError` → `tonic::Status` mapping.
- **REST routes**: `/auth/*` (existing) + `/artists`, `/artists/{id}`, `/artists/{id}/albums`, `/artists/search`; `/albums`, `/albums/{id}`, `/albums/{id}/tracks`, `/albums/search`; `/tracks`, `/tracks/{id}`, `/tracks/search`; `/library/scan` — all behind the auth middleware, at feature parity with gRPC.
- Repository traits extended with `count`/`search`/`update`/lookup-by-name helpers; Postgres impls updated.

No streaming, playlists, uploads/ingest, or audit-log read API yet — those land in Phase 4+.
