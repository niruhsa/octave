# Music App (Client) — Agent Guide

Client-side of the Music Server & App project. See the root [`AGENTS.md`](../AGENTS.md) for whole-system context and [`server/AGENTS.md`](../server/AGENTS.md) for the backend. This file covers the client app only.

## Purpose
Cross-platform music player that streams from the remote server when online and falls back to a local offline cache when the server is unreachable. Functionality comparable to [Poweramp](https://powerampapp.com/) / Spotify, with the server acting as authority when available.

## Tech Stack
- **Framework:** [Tauri 2](https://tauri.app/) ([Rust docs](https://docs.rs/tauri/2.11.3/tauri/)).
- **Frontend:** React 19 + TypeScript, bundled with Vite 8.
- **Rust core (`src-tauri/`):** native shell, plugins, and `#[tauri::command]` handlers bridging frontend ↔ native.
- **Package name:** `music-app` (npm) / `music_app_lib` (Rust lib).
- **App identifier:** `dev.niruhsa.music.app`.

## Target Platforms
- **Minimum:** desktop (Windows/macOS/Linux) and Android — must cross-compile to both.
- **Best effort:** iOS.

## Layout
- `src/` — React/TS frontend. Entry: [`src/main.tsx`](./src/main.tsx) → [`src/App.tsx`](./src/App.tsx).
- `src-tauri/` — Rust native side. Entry: [`src-tauri/src/lib.rs`](./src-tauri/src/lib.rs) (`run()`), [`src-tauri/src/main.rs`](./src-tauri/src/main.rs).
- `src-tauri/tauri.conf.json` — app config (window, bundle, dev/build commands, identifier).
- `src-tauri/capabilities/default.json` — permission/capability grants.
- `public/` — static assets served as-is.
- `index.html` — Vite HTML entry.

## Responsibilities (client side)
- **Streaming playback** from the server with gapless/queue UX.
- **Offline cache (SQLite):** a *partial local copy* of the server DB holding **only downloaded items** — track/album/artist metadata, playlist references, and album-art cover paths for downloaded content. Never the full catalog. Play from cache when the server is offline; server (PostgreSQL) is authority when online.
- **Auth:** log in via `SECRET_KEY` or username/password; store credentials securely; respect permission tier (Admin / Manager / User) returned by server.
- **Library browse/search:** artists, albums, tracks, playlists.
- **Playlists:** create / update / delete (synced through server when online).
- **Uploads:** send new music to the server — individually or as archives (archive file, ISO, CD, other popular formats).
- **Metadata editing:** optional, manual, opt-in UI for managers/admins.
- **Follow + notifications:** follow artists; surface new-release notifications.
- **Admin/manager UI:** expose settings & user management only when the logged-in account has permission.
- **Audit/rollback views:** managers/admins can view change logs and trigger rollbacks (server-enforced).

## Server Communication
- Backend is **gRPC primary, REST fallback**. Mirror that preference in the client transport layer.
- All requests carry auth (`SECRET_KEY` or session from username/password).
- Treat server as source of truth when reachable; degrade gracefully to offline cache otherwise.
- Heavy/native work (filesystem cache, secure credential storage, downloads) belongs in the Rust side via `#[tauri::command]`; keep the React layer for UI/state.

## Conventions
- Frontend in TypeScript with `strict` mode (see [`tsconfig.json`](./tsconfig.json)) — no unused locals/params, no fallthrough cases.
- Expose native capabilities through explicit Tauri commands + capability grants in `src-tauri/capabilities/`; do not widen permissions beyond need.
- Permission-gate every admin/manager UI affordance; never assume client-side checks are sufficient — server enforces.
- Keep dev server on fixed port `1420` (Tauri requirement; see `vite.config.ts`).

## Build & Run
```sh
cd app
npm install

# Frontend only (browser dev)
npm run dev

# Full Tauri app (desktop)
npm run tauri dev
npm run tauri build

# Type-check + frontend build
npm run build
```
Mobile (after `tauri android/ios init`):
```sh
npm run tauri android dev
npm run tauri android build
npm run tauri ios dev      # best effort
```

### Prerequisites
- **Android:** JDK 17 (AGP 8.11 / Gradle 8.14 do not support JDK 22+). Pin per-shell with:
  ```sh
  export JAVA_HOME="$(/usr/libexec/java_home -v 17)"
  export PATH="$JAVA_HOME/bin:$PATH"
  ```
  Plus `ANDROID_HOME` pointing at a real SDK install and an installed NDK (the Tauri Android tooling auto-detects `$ANDROID_HOME/ndk/<version>`).

## Documentation Maintenance
This file owns the **client's** status and detail. When client work changes behaviour, platforms, build, or scope:
1. Update this file first (responsibilities, layout, build, status).
2. Propagate any high-level/status change up to the root [`AGENTS.md`](../AGENTS.md) and [`README.md`](../README.md).

Keep the **Status** section below in sync with the root docs.

## Status
**Phase 5 — Sync Engine: complete.**

- Transport extended on **both** gRPC + REST (feature parity, gRPC→REST fallback via the existing `try_grpc` path): get-by-id for artists/albums/tracks (404 / `NotFound` → `Ok(None)` so a missing server row means "prune locally"), and full playlist surface (`list_my_playlists`, `get_playlist`, `create`/`rename`/`delete`, `add`/`remove`/`reorder` track). New shared `transport::{Playlist, PlaylistTrack, PlaylistWithTracks}` models; client `playlist.proto` stubs added to `transport::proto`. gRPC playlist mutation errors map by code (`PermissionDenied`→`Forbidden`, `NotFound`/`InvalidArgument`/`FailedPrecondition`→permanent `Internal` so the engine drops the op).
- New migration [`migrations/0002_sync.sql`](./src-tauri/migrations/0002_sync.sql): a `pending_ops` **offline-edit outbox** (FIFO, `op_type` CHECK list + opaque `payload_json` + `attempts`/`last_error`). Repo gains `enqueue_op` / `list_pending_ops` / `count_pending_ops` / `delete_pending_op` / `mark_op_failed` and `delete_sync_state`.
- New module [`src-tauri/src/sync/`](./src-tauri/src/sync): `ops.rs` (`PendingOpKind` typed payloads, serde `tag="kind"` snake_case, `local:`-prefixed placeholder ids + `remap_playlist`/`references_playlist`) and `engine.rs` (`SyncEngine` + `SyncReport`). `sync_now` runs **push → pull/reconcile → prune** in order:
  1. **Push** replays the outbox FIFO; a `playlist.create` resolves its temp id and `remap_local_id` rewrites later queued ops + cache rows (transactional insert-new→repoint-children→delete-old to dodge the `playlist_tracks` FK). Server-rejected ops are dropped as **conflicts** (server authority wins); transport failures stay queued (`ops_deferred`).
  2. **Pull/reconcile** walks only cached entities (offline-cache principle — never the full catalog), fetches each server row, and upserts when changed. Versioning uses a **content hash** stored in `sync_state.server_etag` (the wire DTOs drop `updated_at`, so we hash significant fields and skip writes when unchanged). Client-owned track fields (`local_file_path`, `downloaded_at`) are preserved across reconcile; server-404 rows are pruned with their `sync_state`.
  3. **Prune** drops downloaded tracks whose local file vanished from disk.
- Three Tauri commands [`commands/sync_commands.rs`](./src-tauri/src/commands/sync_commands.rs): `sync_now` (→ `SyncReport`), `sync_pending_count`, `sync_enqueue_op`. Frontend: TS bindings + `PendingOp`/`SyncReport` types in [`src/ipc.ts`](./src/ipc.ts); a Zustand sync store + `useSyncScheduler` ([`src/sync/useSync.ts`](./src/sync/useSync.ts)) that runs sync on **online-regain** and **window focus / app foreground** (mobile defers big syncs to those moments) and tracks a "N unsynced" badge; Home shows sync status + a manual **sync now** button + a compact push/update/prune/conflict summary.
- Tests: 6 in-module unit tests (op JSON round-trip, local-id detection, remap, content-hash order-sensitivity + field-boundary, local-id guard) + 5 integration tests in [`tests/sync.rs`](./src-tauri/tests/sync.rs) (outbox FIFO + per-op clear, `mark_op_failed` attempts/error, transactional local-id remap cascade, targeted `delete_sync_state`, reconcile-list sanity). `cargo test` — **35 passed** (7 suites). `cargo build` clean (0 warnings), `npm run build` clean.
- **Outstanding (manual verification against a live server):** offline→online round-trip pushing real playlist edits, conflict surfacing when the server rejects a queued op, and metadata reconcile after a server-side edit.

**Phase 4 — Playback: complete.**

- New native module [`src-tauri/src/player/`](./src-tauri/src/player): a custom **`media://` URI-scheme protocol** (`stream.rs`) the webview's `<audio>` element loads directly, plus a `resolver.rs` URL helper.
  - Per request the protocol resolves the track id: **local cache hit** → serves `local_file_path` from disk with full RFC 7233 byte-range semantics (200 / 206 / 416, `Accept-Ranges`/`Content-Range`/`Content-Length`, MIME by extension); **cache miss** → proxies `GET /tracks/{id}/stream` from the server, injecting the active `Authorization` header and forwarding the incoming `Range` header, relaying the server's status + stream headers; **offline + not cached** → 502 so the element surfaces an error.
  - This keeps the session token out of the webview (the `<audio>` element can't set auth headers) and means the frontend never branches on online/offline — source resolution (prefer-local-else-stream) lives entirely in Rust. A process-wide `OnceLock<reqwest::Client>` backs the proxy so range requests don't rebuild TLS state.
  - Registered in `lib.rs::run()` via `register_asynchronous_uri_scheme_protocol("media", …)`; the handler is generic over the runtime and dispatches each request onto the async runtime.
- One Tauri command [`commands/player_commands.rs`](./src-tauri/src/commands/player_commands.rs): `player_media_url(track_id)` returns the platform-correct URL string (`media://localhost/<id>` on macOS/Linux/iOS, `http://media.localhost/<id>` on Windows/Android), defensively percent-encoded.
- Frontend: a Zustand [`src/player/store.ts`](./src/player/store.ts) owning the queue, current index, play state, position/duration, volume, **shuffle** (Fisher–Yates with the chosen track pinned first) and **repeat** (`off`/`all`/`one`); it binds to a single persistent `<audio>` element and drives `src`/play/pause/seek. A persistent [`src/components/PlayerBar.tsx`](./src/components/PlayerBar.tsx) renders now-playing + transport (play/pause, prev/next with 3 s restart semantics, shuffle, repeat, seek slider, volume) and mirrors state into the OS **Media Session API** (`navigator.mediaSession`) so media keys / lock-screen / Bluetooth controls drive the same store. `Album` and `Search` track rows are click-to-play and queue the surrounding list; `Album` gains a **Play album** button.
  - **Bug fixed during impl:** the `<audio>` element must be mounted unconditionally — gating it behind a non-empty queue meant the store never bound to a live element and `loadAndPlay` silently aborted, so nothing ever played. The bar chrome is now the only conditionally-rendered part.
- Tests: 4 new player-protocol unit tests (range parsing incl. open-ended + rejects, percent-decode, track-id path stripping) + 2 resolver tests. `cargo test` — **24 passed** (2 cache + 5 auth + 1 config + 5 in-module config + 4 library + 2 resolver + 4 stream... see suites). `cargo build` clean (0 warnings), `npm run build` clean.
- **Deferred / best-effort:** full desktop-native now-playing (SMTC / MPRIS / macOS Now Playing) beyond what Media Session provides, and a true Android background-playback foreground service + audio-focus handling — both layered in later polish. Cover art in the player bar lands with Phase 6 downloads.

**Phase 3 — Library Browse & Search: complete.**

- gRPC + REST clients extended with `LibraryService` reads: `list_artists` (paged), `search_artists`, `list_albums_by_artist`, `search_albums`, `list_tracks_by_album`, `search_tracks`. Both transports map their wire DTOs into a shared `transport::{Artist, Album, Track}` model so callers don't branch on transport.
- gRPC → REST fallback factored into a single `fallback_log` helper; the gRPC connect attempt is bundled into a `try_grpc()` helper so each library call is a flat match → fallback path.
- New module [`src-tauri/src/library/`](./src-tauri/src/library): `MergedArtist` / `MergedAlbum` / `MergedTrack` (server row + `downloaded: bool` + optional `local_file_path` / `local_cover_path`) and a `LibraryService` that:
  - Online: hits the server, then enriches each row by checking the SQLite cache (IN-list lookups for `tracks.artist_id`, `album_art.album_id`, `tracks.id`).
  - Offline (transport error OR no credential): falls back to the cache and returns the same `MergedX` shapes (downloaded = `true` by construction).
  - Wraps the result in `LibraryView<T> { source, items, total? }` so the UI can render an offline badge in one branch.
- Six new Tauri commands in [`commands/library_commands.rs`](./src-tauri/src/commands/library_commands.rs): `library_list_artists`, `library_search_artists`, `library_list_albums_by_artist`, `library_search_albums`, `library_list_tracks_by_album`, `library_search_tracks`. All clamp `limit` to the server's 200 cap, default 50, and clamp `offset >= 0`.
- Frontend: TypeScript bindings in [`src/ipc.ts`](./src/ipc.ts) (`MergedArtist`/`MergedAlbum`/`MergedTrack`/`LibraryView`); new shared helpers [`src/lib/format.ts`](./src/lib/format.ts) + [`src/lib/error.ts`](./src/lib/error.ts); a [`SourceBadge`](./src/components/SourceBadge.tsx) + `DownloadedDot` chip pair. Four new routes:
  - `/library` — paginated artist list, prev/next, source badge, downloaded dot per row.
  - `/artists/:id` — album grid for one artist.
  - `/albums/:id` — track list with codec + duration + downloaded dot.
  - `/search` — unified artist/album/track search, each in its own query so a slow section doesn't block the rest.
- Home gains `Library` + `Search` shortcut buttons when a session is active.
- Tests in [`src-tauri/tests/library.rs`](./src-tauri/tests/library.rs) cover the downloaded-detection SQL, cover/track-path lookups, the empty-input guard, and the offline title filter — 17 tests pass total (2 cache + 5 auth + 1 config + 5 in-module config + 4 library).
- `cargo build` clean, `npm run build` clean.

**Phase 2 — Server Transport & Auth: complete.**

- gRPC + REST client deps in place: `tonic` 0.12 (transport + TLS), `prost` 0.13, `tonic-build` 0.12, `reqwest` 0.12 (rustls + json + http2), `url`, `async-trait`. `keyring` 3 is desktop-only via `cfg(not(target_os = "android"))`.
- `build.rs` compiles `../../server/proto/*.proto` (single source of truth) into client stubs via `tonic-build`.
- [`src-tauri/src/transport/`](./src-tauri/src/transport): `ServerConfig` (URL parser), `proto` (generated `music.auth.v1` + `music.library.v1`), `GrpcClient` (tonic), `RestClient` (reqwest, `/auth/*` + `/health`), and `ServerClient` orchestrating **gRPC primary → REST fallback** on transport-level failures (auth rejections do NOT fall back — server is authority on either path). `Credential::{SecretKey, Bearer}` mirrors the server middleware (`Authorization: SecretKey ...` / `Bearer ...`).
- [`src-tauri/src/auth/`](./src-tauri/src/auth): `SecureStore` trait with `KeyringStore` (macOS Keychain / Windows Credential Manager / libsecret via `keyring`) on desktop and `FileStore` (0600 file in app-private storage) on Android. `AuthManager` holds the active credential + cached `WhoAmI` snapshot, exposes `login` / `set_secret_key` / `whoami` / `logout` / `refresh_online`.
- New Tauri commands in [`commands/auth_commands.rs`](./src-tauri/src/commands/auth_commands.rs): `auth_configure_server`, `auth_login`, `auth_set_secret_key`, `auth_whoami`, `auth_session`, `auth_logout`, `auth_refresh_online`. Tauri state extended to `AppStateHandle { pool, auth: RwLock<Option<Arc<AuthManager>>> }`.
- `AppError` extended with `Transport`, `Unauthenticated`, `Forbidden`, `AuthNotConfigured`, `SecureStorage` variants so the frontend can distinguish auth failures from connection failures.
- TypeScript mirror in [`src/ipc.ts`](./src/ipc.ts) (`AuthSession`, `authLogin`, `authSetSecretKey`, etc.) and a new [`src/routes/Login.tsx`](./src/routes/Login.tsx) supporting both username/password and `SECRET_KEY` flows. `App.tsx` adds a `/login` route and re-hydrates the session from Rust on boot. Zustand store now carries `session` + `tier` + `online` + `serverConfigured`.
- Tests ([`src-tauri/tests/auth.rs`](./src-tauri/tests/auth.rs)): `FileStore` round-trip for both credential kinds, `ServerConfig` URL parsing + rejection, `PermissionTier::from_proto` fallback. `cargo test` — 7 passed (2 cache + 5 auth).
- `cargo build` clean (0 warnings) + `npm run build` clean.

**Outstanding for full Phase 2 completion (manual verification against a live server):**
- Boot the server (`server/`) locally and verify desktop login (`username/password` and `SECRET_KEY`) round-trips successfully, the keychain persists the credential, and the cached tier renders on relaunch.
- Verify the same on Android emulator (`FileStore` path).
- Verify gRPC → REST fallback by binding only one transport on the server side and re-running login.

**Phase 1 — Local SQLite Cache (Offline Store): complete.**

- `sqlx` (sqlite, runtime-tokio, migrate, macros) + `uuid` + `time` added to `src-tauri/Cargo.toml`.
- Embedded migration [`src-tauri/migrations/0001_init.sql`](./src-tauri/migrations/0001_init.sql) creates the **offline-only** schema: `artists`, `albums`, `album_art`, `tracks` (with `local_file_path` + `downloaded_at`), `playlists`, `playlist_tracks`, `sync_state` (per-entity `server_version` + `server_etag` + `last_synced_at`). IDs are TEXT and equal the server's UUIDs byte-for-byte so Phase 5 sync can reconcile.
- `db::open()` in [`src-tauri/src/db/mod.rs`](./src-tauri/src/db/mod.rs) opens/creates the DB at the OS app-data path with WAL + `synchronous=NORMAL` + `foreign_keys=ON`, runs embedded migrations, and returns a `SqlitePool`. Resolved via `app.path().app_data_dir()` so it lands in app-private storage on desktop **and** Android.
- Typed cache repository under [`src-tauri/src/cache/`](./src-tauri/src/cache) (`model.rs` row structs, `repo.rs` upsert/get/list/delete + idempotent UPSERTs + transactional `replace_playlist_tracks`).
- `AppStateHandle { pool }` registered as Tauri managed state in `lib.rs::run()`.
- `#[tauri::command]` surface in [`commands/cache_commands.rs`](./src-tauri/src/commands/cache_commands.rs) — 24 commands covering artists, albums, album_art, tracks, playlists, playlist_tracks, sync_state. All registered in `invoke_handler!`.
- Mirrored TypeScript types and typed `invoke` wrappers in [`src/ipc.ts`](./src/ipc.ts); [`src/routes/Home.tsx`](./src/routes/Home.tsx) now reads `cache_list_downloaded_tracks` as a live UI smoke test of the bridge.
- Integration tests in [`src-tauri/tests/cache.rs`](./src-tauri/tests/cache.rs): full round-trip (upsert/read/cascade) + cross-reopen persistence. `cargo test --test cache` — 2 passed.
- `cargo check` + `npm run build` clean.

**Phase 0 — Project Scaffold & Tooling: complete.**

- Frontend deps in place: React 19 + Vite 8, Zustand, TanStack Query, React Router, Tailwind v4 (`@tailwindcss/vite`).
- Rust deps in place: `tokio` (full), `serde`, `tracing` + `tracing-subscriber`, `anyhow`, `thiserror`.
- Native module layout staked out under [`src-tauri/src/`](./src-tauri/src): `commands/`, `db/`, `transport/`, `cache/`, plus `error.rs` (`AppError` / `AppResult`).
- Typed `#[tauri::command]` bridge pattern established — `app_info` (replacing the template `greet`) returns `AppInfo` via `AppResult<T>`, wired through a typed wrapper in [`src/ipc.ts`](./src/ipc.ts) and rendered by [`src/routes/Home.tsx`](./src/routes/Home.tsx).
- Frontend shell: `QueryClientProvider` + `RouterProvider`, global Zustand store with `online` + `tier` placeholders, Tailwind base styling.
- Capability grants kept minimal — still just `core:default` + `opener:default` in [`src-tauri/capabilities/default.json`](./src-tauri/capabilities/default.json); widen per phase.
- `npm run build` (tsc + vite) and `cargo check` both clean.
- `npm run tauri dev` boots the desktop shell with the new IPC wired through. ✅
- `npm run tauri android dev` builds and launches the shell on an Android emulator (Pixel_XL_API_36) with JDK 17 + NDK 30.0.14904198. ✅
- iOS init (`npm run tauri ios init`) — best effort, deferred.

Next: **Phase 1 — Local SQLite Cache (Offline Store)**.

No streaming, auth, SQLite cache, or server transport implemented yet.
