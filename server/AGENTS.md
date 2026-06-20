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
- `WRITE_TAGS` — `1`/`true` to write metadata edits back to file audio tags via `lofty`. Default off (DB stays authoritative; files untouched).
- `FETCH_ARTWORK` — `1`/`true` to enable automatic album-artwork fetch from the Cover Art Archive. Default off.
- `ARTWORK_PATH` — directory where fetched cover images are cached. Path-typed. Defaults to `<LIBRARY_PATH>/.artwork` when unset.
- `ENV_FILE` — explicit override for the `.env` file location.

### `.env` loading & path resolution
`Config::from_env` auto-locates a `.env` file at startup and seeds the
process environment from it. Search order:
1. `ENV_FILE` (if set).
2. Walk upward from the current working directory looking for `.env`.
3. `CARGO_MANIFEST_DIR/.env` (compile-time anchor; works for `cargo run`).
4. None — process env is used as-is.

The directory containing the loaded `.env` becomes the **config anchor**.
All path-typed env vars (`LIBRARY_PATH`, `INGEST_PATH`, `ARTWORK_PATH`) resolve relative
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
**Phases 0–7 complete (Scaffold, Persistence & Schema, Auth & Authorization, Library Management CRUD, Streaming, Playlists, Uploads & Ingest, Metadata & Artwork).**

**Password change (inter-phase).** Self-service + admin reset:
- `UserRepo::update_password(id, hash)` trait method + Postgres impl (`UPDATE users SET password_hash, updated_at`). The `FakeUsers` test stub implements it too.
- `AuthService::change_password(caller, target_id, old_password: Option<&str>, new_password)` — authorization: `SECRET_KEY` or an Admin caller may reset **any** user without the old password; a non-admin caller may change only **their own** password and must supply + verify the old one (`password::verify`); anything else → `PermissionDenied`. Enforces the ≥ 8-char rule (same as `register`). Confirms the target exists (admin reset of a bad id → `NotFound`, not a silent no-op). Audits a `user.password_change` entry (actor = caller, entity = target user) — before/after omitted because hashes are sensitive + not rollback-able; the row still records who reset whose password and when.
- `AuthService` now holds an `Arc<dyn AuditRepo>` (constructor signature changed; `main.rs` updated to pass the `PgRepos` audit handle). `register` is still unaudited (unchanged).
- gRPC parity ([`grpc/auth_svc.rs`](./src/grpc/auth_svc.rs) + [`proto/auth.proto`](./proto/auth.proto)): `ChangePassword(ChangePasswordRequest{user_id, old_password, new_password})` → `ChangePasswordResponse{}`. `old_password` empty ⇒ admin reset; non-empty ⇒ self-change (verified server-side). Handler parses the target UUID (`invalid_argument` on a bad id).
- REST parity ([`rest/mod.rs`](./src/rest/mod.rs)): `PUT /users/:id/password` body `{ old_password, new_password }` (`old_password` defaulting empty ⇒ admin reset), behind the existing auth middleware, same service → same authorization + audit as gRPC.
- User list (`UserRepo::list`, `AuthService::list_users`, `ListUsers` gRPC + `GET /users` REST): admin-gated, returns each user's id/username/tier — no password hashes. The client uses this to populate the admin password-reset dropdown instead of pasting UUIDs.
- Account deletion (`AuthService::delete_user`, `DeleteUser` gRPC + `DELETE /users/:id` REST): admin-gated. Captures a before-image (`username` + `level`, no hash) in the audit entry (`user.delete`), then calls `UserRepo::delete` (cascades sessions/playlists/follows; `audit_log.actor_id → SET NULL`). Four additional tests in `auth/service.rs` (non-admin denied, admin deletes + audit, secret-key deletes, missing→NotFound).
- Tests: 6 new in-module tests in `auth/service.rs` against minimal `FakeUsers`/`FakeSessions`/`FakeAudit` fakes — self-change requires correct old (wrong → `Unauthenticated`, right → ok), non-admin can't change another's (`PermissionDenied`), admin resets any user without old + audit row written, `SECRET_KEY` resets any, short new → `InvalidArgument`, admin reset of missing user → `NotFound`. Total lib tests: **61 passing** (was 55). `cargo build` clean, `cargo clippy` 0 new warnings.

**Phase 7 — Metadata & Artwork (done)**
- Tag write-back ([`services/tag.rs`](./src/services/tag.rs)): new `TagWrite` struct (optional title/artist/album/track_no/disc_no/year) + `write_tags(path, &TagWrite)` — reads the file via `lofty::read_from_path`, inserts a primary tag of the format's primary type when none exists, applies only the set fields via the `Accessor` setters (`set_title`/`set_artist`/`set_album`/`set_track`/`set_disk`/`set_year`), and persists with `save_to_path(WriteOptions::default())`. Empty edit is a no-op. 1 unit test (`cover_ext_mapping` lives in artwork; tag write covered via metadata service).
- New `services::metadata` ([`services/metadata.rs`](./src/services/metadata.rs)): `MetadataService` wraps `LibraryService::update_track` (already audited) and, when `write_tags` is enabled, mirrors changed fields back to the file. DB-first: row updated + audited, then best-effort file sync; a write-back failure is surfaced (DB change already audited, file left as-is). `MetadataEdit` carries optional title/track_no/disc_no/metadata_json + a `year` that is write-back-only (not a track DB column). Manager+ enforced.
- New `services::artwork` ([`services/artwork.rs`](./src/services/artwork.rs)): `CoverArtSource` trait isolates the external lookup; `CoverArtArchive` impl searches MusicBrainz (`/ws/2/release?query=release:".." AND artist:".."`) for a release MBID then pulls `coverartarchive.org/release/{mbid}/front` (404 → `Ok(None)`). `ArtworkService` fetches → caches to `<ARTWORK_PATH>/<album_id>.<ext>` → updates the album's `cover_path` via `LibraryService::update_album` (audited). Manager+ enforced. `reqwest` sends a compliant `User-Agent`. 1 unit test (content-type → ext mapping).
- Config ([`config.rs`](./src/config.rs)): new `write_tags` (`WRITE_TAGS`), `fetch_artwork` (`FETCH_ARTWORK`), and `artwork_path` (`ARTWORK_PATH`, defaults to `<LIBRARY_PATH>/.artwork`). New `env_flag` helper for boolean-ish parsing.
- gRPC parity ([`grpc/library_svc.rs`](./src/grpc/library_svc.rs) + [`proto/library.proto`](./proto/library.proto)): `LibraryService` gains `EditTrackMetadata` (optional-scalar field presence → unset-vs-value) returning the updated `Track`, and `FetchAlbumArtwork` returning `{found, cover_path}`. `FetchAlbumArtwork` returns gRPC `FAILED_PRECONDITION` when artwork is disabled. `LibraryServer` now holds `metadata: MetadataService` + `artwork: Option<ArtworkService>`.
- REST parity ([`rest/library.rs`](./src/rest/library.rs)): `PATCH /tracks/:id/metadata` and `POST /albums/:id/artwork`, both behind the existing auth middleware, same service → same permission + audit as gRPC. `RestState` gains `metadata` + `artwork`.
- Cargo.toml: added `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }`.
- `main.rs` constructs `MetadataService` always and `ArtworkService` only when `FETCH_ARTWORK` is set; injects both into gRPC `serve(..)` and `RestState`.
- Tests: artwork content-type mapping + metadata edit-default tests added (48 after Phase 7; now **55 total** including the 7 archive tests from the Phase 6 completion pass).
- Audit: metadata edits flow through `track.update`; artwork updates flow through `album.update` — both carry full before/after JSON for rollback (Phase 8).

**Phase 6 — Uploads & Ingest (done)**
- New `services::tag` module ([`services/tag.rs`](./src/services/tag.rs)): shared `TagInfo` struct + `read_tags(path)` helper used by scanner, uploader, and watcher alike.  `is_audio_file` + `AUDIO_EXTS` constant centralise extension checks.  Tag extraction falls back to `Unknown` / filename for missing metadata; `lofty` errors propagate up so callers get a clear 500.  Two organising helpers live here too: **`primary_artist(raw)`** strips collaboration suffixes (`feat.` / `ft.` / `featuring` / `vs.` / `with`) and splits on the first hard separator (` & `, `, `, `; `, ` × `, ` + `, ` and `, ` x `) so one artist's catalog stays in one folder — `TPE2`/`AlbumArtist` is preferred over `TPE1`/`TrackArtist` for the same reason; **`infer_language(name)`** maps the Unicode script of the artist name to a folder label (kana → Japanese, Hangul → Korean, Han → Chinese, Cyrillic/Arabic/Hebrew/Greek/Devanagari/Thai → their language; default English).  Kana and Hangul are treated as language-exclusive so mixed names (`宇多田ヒカル`) bucket correctly.  An explicit `Language` / `TLAN` tag (ISO-639-1/2 code or English/native name) overrides script inference via `normalize_language()`.
- New `services::organizer` module ([`services/organizer.rs`](./src/services/organizer.rs)): `Organizer` owns the `LIBRARY_PATH` root, computes `<root>/<Language>/<Artist>/<Album>/<NN - Title>.<ext>` destinations with `sanitize()` (replaces `/\:*?"<>|` with `_`, strips leading/trailing dots+underscores, defaults to `Unknown` on empty), and copies the file there — **source is never moved or deleted**.  `Language` is the top-level folder so an artist's discography lives under their primary language; `Artist` is the **primary artist only** (collab suffixes stripped upstream).  Idempotent: same-size destination is skipped.  7 unit tests (sanitize passthrough, special chars, empty-fallback, basic destination, Japanese-script destination, unknown-metadata destination).
- Refactored `services::scan` ([`services/scan.rs`](./src/services/scan.rs)): `extract_tags` lifted into shared `tag` module; added `pub async fn index_file(caller, path) -> Result<Option<Uuid>>` so the ingest pipeline can index a single file after the organizer has copied it to its final location.  Existing `scan()` still works unchanged.
- New `services::ingest` ([`services/ingest.rs`](./src/services/ingest.rs)): `IngestService` holds `ScanService` + `Organizer` + optional `ingest_root`; `organize_and_index(caller, source)` is the single entry-point for both direct uploads and the watcher — reads tags, upserts artist+album via the library service, copies to library, indexes via `ScanService.index_file`.
- New `services::watch` ([`services/watch.rs`](./src/services/watch.rs)): background `notify::RecommendedWatcher` on `INGEST_PATH`; `Create(File)` and `Modify(Data)` events debounced per-path (300 ms settle + in-flight set via `tokio::sync::Mutex<HashSet<PathBuf>>`); `.uploading` staging files skipped; errors logged at `warn` (already-indexed at `debug`).  Returns the watcher handle so it stays alive in `main`.
- REST routes ([`rest/ingest.rs`](./src/rest/ingest.rs)) mounted at `POST /upload` (multipart, 500 MiB cap) and `POST /ingest/scan` (walks ingest folder, copies+indexes all unseen audio).  Both require Manager+.  Upload writes to `<INGEST_PATH>/.tmp/<uuid>.uploading`, renames to real ext, calls `organize_and_index`, cleans up.  Response: `201 Created` with `{track_id, path}`.
- `Config` already had `ingest_path: Option<PathBuf>` from Phase 1; `main.rs` now constructs `IngestService` when `library_path` is set, starts the watcher, and injects `ingest: Option<IngestService>` into `RestState`.
- Cargo.toml: added `notify = "6"`, enabled `axum/multipart`; archive upload adds `zip`/`tar`/`flate2`/`bzip2`/`xz2`.
- Tests: **55 lib+suite tests passing** (38 after the initial Phase 6 drop, +7 archive tests in this completion pass, +Phase 7).  New for ingest: 2 `tag::is_audio_file` tests, 5 organiser tests, 7 archive tests.
- **Archive upload + `UploadService` gRPC (done):** new `services::archive` ([`services/archive.rs`](./src/services/archive.rs)) with `ArchiveKind::detect` (zip; `.tar`/`.tar.gz`/`.tgz`/`.tar.bz2`/`.tbz2`/`.tar.xz`/`.txz`; ISO/CD `.iso`/`.img`/`.nrg`/`.bin`/`.cue` recognised) and `extract(source, kind, dest_dir)`. zip via `zip` crate (deflate/bzip2/lzma/zstd/xz members), tarballs via `tar` + `flate2`/`bzip2`/`xz2` decoders. **ISO/CD return `InvalidArgument` ("not yet supported")** — PLAN stub. Zip-slip guarded: `enclosed_name` + a `safe_join` that rejects `..`/absolute/root components. `IngestService::organize_archive` extracts to `<INGEST_PATH>/.tmp/extract-<uuid>` (blocking extraction on `spawn_blocking`), runs `organize_and_index` on each audio member (non-audio ignored), cleans up the temp tree, returns `ArchiveIngestResult { ingested, already_indexed, non_audio_skipped, errors, track_ids }`. REST `POST /upload` now detects archives by filename and returns an untagged `UploadResult` (single-file `{track_id,path}` or archive summary). New `proto/upload.proto` (`music.upload.v1`) + `UploadServer` ([`grpc/upload_svc.rs`](./src/grpc/upload_svc.rs)): **client-streaming** `Upload(stream UploadRequest)` — first message `UploadInfo{filename}`, then `chunk` bytes; stages to `<INGEST_PATH>/.tmp/<uuid>.<ext>` with a 500 MiB cap, then single-file or archive ingest. Manager+ (credential extracted from metadata before consuming the non-Sync stream via new `AuthInterceptor::resolve_credential`). Mounted alongside auth/library/playlist/health with a health-reporter entry. 7 archive tests (detect zip/tarballs/disc-images/non-archive, disc-image stub error, zip-slip guard, zip + tar.gz extraction round-trips).
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
