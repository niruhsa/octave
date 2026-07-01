//! Tauri commands for library browse + search.
//!
//! All calls go through `LibraryService`, which decides per-call whether
//! to hit the server or fall back to the cache. The frontend never has to
//! ask "am I online?" — the returned `LibraryView` carries a `source` tag.

use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_fs::{FilePath, FsExt};

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::library::{LibraryView, MergedAlbum, MergedArtist, MergedTrack};
use crate::library::service::LibraryService;
use crate::transport::{
    ArtistStoragePaths, LibraryStorage, MetadataEdit, RelocateReport, RescanReport,
};
use crate::AppStateHandle;

/// Server default page sizes mirror the server's cap (200) / default (50).
const DEFAULT_LIMIT: i64 = 50;

async fn service<'a>(state: &'a State<'a, AppStateHandle>) -> AppResult<LibraryService<'a>> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    Ok(LibraryService::new(&state.pool, auth))
}

fn normalise_limit(limit: Option<i64>) -> i64 {
    let l = limit.unwrap_or(DEFAULT_LIMIT);
    if l <= 0 { DEFAULT_LIMIT } else { l.min(200) }
}

fn normalise_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

// ---------------------------------------------------------------------------
// artists
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_list_artists(
    state: State<'_, AppStateHandle>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedArtist>> {
    let svc = service(&state).await?;
    svc.list_artists(normalise_limit(limit), normalise_offset(offset)).await
}

#[tauri::command]
pub async fn library_search_artists(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedArtist>> {
    let svc = service(&state).await?;
    svc.search_artists(&query, normalise_limit(limit), normalise_offset(offset)).await
}

// ---------------------------------------------------------------------------
// storage
// ---------------------------------------------------------------------------

/// The server's library-storage breakdown for the homepage widget. Online-only
/// (a live server view); errors when offline so the UI can show "—".
#[tauri::command]
pub async fn library_get_storage(
    state: State<'_, AppStateHandle>,
) -> AppResult<LibraryStorage> {
    let svc = service(&state).await?;
    svc.get_library_storage().await
}

// ---------------------------------------------------------------------------
// albums
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_list_albums_by_artist(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<LibraryView<MergedAlbum>> {
    let svc = service(&state).await?;
    svc.list_albums_by_artist(&artist_id).await
}

#[tauri::command]
pub async fn library_search_albums(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedAlbum>> {
    let svc = service(&state).await?;
    svc.search_albums(&query, normalise_limit(limit), normalise_offset(offset)).await
}

// ---------------------------------------------------------------------------
// tracks
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_list_tracks_by_album(
    state: State<'_, AppStateHandle>,
    album_id: String,
) -> AppResult<LibraryView<MergedTrack>> {
    let svc = service(&state).await?;
    svc.list_tracks_by_album(&album_id).await
}

#[tauri::command]
pub async fn library_search_tracks(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedTrack>> {
    let svc = service(&state).await?;
    svc.search_tracks(&query, normalise_limit(limit), normalise_offset(offset)).await
}

// ---------------------------------------------------------------------------
// metadata edit (Phase 9; Manager+ gated server-side)
// ---------------------------------------------------------------------------

/// Apply an opt-in metadata edit to a track. Returns the refreshed
/// `MergedTrack` (with its `downloaded` flag) so the UI updates in one
/// round-trip; the edit is mirrored into the offline cache for downloaded
/// items by `LibraryService::edit_track_metadata`.
#[tauri::command]
pub async fn library_edit_track_metadata(
    state: State<'_, AppStateHandle>,
    id: String,
    edit: MetadataEdit,
) -> AppResult<MergedTrack> {
    let svc = service(&state).await?;
    svc.edit_track_metadata(&id, &edit).await
}

/// Fetch a single artist (server-first, with its alias set; cache fallback).
#[tauri::command]
pub async fn library_get_artist(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<MergedArtist> {
    let svc = service(&state).await?;
    svc.get_artist(&id).await
}

/// Fetch a single album (server-first, with its alias set; cache fallback).
#[tauri::command]
pub async fn library_get_album(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<MergedAlbum> {
    let svc = service(&state).await?;
    svc.get_album(&id).await
}

// ---------------------------------------------------------------------------
// merge + aliases (Phase 10 — Manager+ gated server-side)
//
// Merge folds a duplicate artist/album into a survivor (re-pointing its
// catalog, preserving every spelling as an alias). `library_move_track` moves
// a track into another album, optionally flagging it a single release. Alias
// commands edit the preserved-spelling set + which one displays.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_merge_artists(
    state: State<'_, AppStateHandle>,
    survivor_id: String,
    duplicate_id: String,
) -> AppResult<MergedArtist> {
    let svc = service(&state).await?;
    svc.merge_artists(&survivor_id, &duplicate_id).await
}

#[tauri::command]
pub async fn library_list_artist_library_paths(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<ArtistStoragePaths> {
    let svc = service(&state).await?;
    svc.list_artist_library_paths(&artist_id).await
}

#[tauri::command]
pub async fn library_set_artist_language(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    target_language: String,
    target_folder: Option<String>,
) -> AppResult<RelocateReport> {
    let svc = service(&state).await?;
    svc.set_artist_language(&artist_id, &target_language, target_folder.as_deref())
        .await
}

#[tauri::command]
pub async fn library_merge_albums(
    state: State<'_, AppStateHandle>,
    survivor_id: String,
    duplicate_id: String,
) -> AppResult<MergedAlbum> {
    let svc = service(&state).await?;
    svc.merge_albums(&survivor_id, &duplicate_id).await
}

#[tauri::command]
pub async fn library_move_track(
    state: State<'_, AppStateHandle>,
    track_id: String,
    album_id: String,
    single_release: bool,
) -> AppResult<MergedTrack> {
    let svc = service(&state).await?;
    svc.move_track(&track_id, &album_id, single_release).await
}

#[tauri::command]
pub async fn library_set_track_single_release(
    state: State<'_, AppStateHandle>,
    track_id: String,
    single_release: bool,
) -> AppResult<MergedTrack> {
    let svc = service(&state).await?;
    svc.set_track_single_release(&track_id, single_release).await
}

#[tauri::command]
pub async fn library_add_artist_alias(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    name: String,
    sort_name: Option<String>,
    language: Option<String>,
) -> AppResult<MergedArtist> {
    let svc = service(&state).await?;
    svc.add_artist_alias(&artist_id, &name, sort_name.as_deref(), language.as_deref()).await
}

#[tauri::command]
pub async fn library_remove_artist_alias(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    alias_id: String,
) -> AppResult<MergedArtist> {
    let svc = service(&state).await?;
    svc.remove_artist_alias(&artist_id, &alias_id).await
}

#[tauri::command]
pub async fn library_set_primary_artist_alias(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    alias_id: String,
) -> AppResult<MergedArtist> {
    let svc = service(&state).await?;
    svc.set_primary_artist_alias(&artist_id, &alias_id).await
}

#[tauri::command]
pub async fn library_add_album_alias(
    state: State<'_, AppStateHandle>,
    album_id: String,
    title: String,
    language: Option<String>,
) -> AppResult<MergedAlbum> {
    let svc = service(&state).await?;
    svc.add_album_alias(&album_id, &title, language.as_deref()).await
}

#[tauri::command]
pub async fn library_remove_album_alias(
    state: State<'_, AppStateHandle>,
    album_id: String,
    alias_id: String,
) -> AppResult<MergedAlbum> {
    let svc = service(&state).await?;
    svc.remove_album_alias(&album_id, &alias_id).await
}

#[tauri::command]
pub async fn library_set_primary_album_alias(
    state: State<'_, AppStateHandle>,
    album_id: String,
    alias_id: String,
) -> AppResult<MergedAlbum> {
    let svc = service(&state).await?;
    svc.set_primary_album_alias(&album_id, &alias_id).await
}

// ---------------------------------------------------------------------------
// rescan (Manager+ gated server-side)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_rescan(
    state: State<'_, AppStateHandle>,
) -> AppResult<RescanReport> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .clone()
            .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))?
    };
    auth.rescan_library().await
}

// ---------------------------------------------------------------------------
// delete (Manager+ gated server-side)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_delete_artist(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<()> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .clone()
            .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))?
    };
    auth.delete_artist(&id).await
}

#[tauri::command]
pub async fn library_delete_album(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<()> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .clone()
            .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))?
    };
    auth.delete_album(&id).await
}

#[tauri::command]
pub async fn library_delete_track(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<()> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .clone()
            .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))?
    };
    auth.delete_track(&id).await
}

// ---------------------------------------------------------------------------
// image upload (Phase 9 — Manager+ gated server-side)
//
// The picked image file is read **natively** (desktop path or Android
// `content://` URI) — never through the WebView — then pushed to the server
// over REST (`POST /albums/:id/cover` / `POST /artists/:id/image`). The
// server caches it under ARTWORK_PATH + updates the row (audited).
// ---------------------------------------------------------------------------

/// Generous client-side cap, matching the server's `MAX_IMAGE_BYTES`.
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

async fn auth_handle(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    state
        .auth
        .read()
        .await
        .clone()
        .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))
}

/// Read a picked image file's bytes natively (off the WebView), capped.
async fn read_image_file(app: AppHandle, path: &str) -> AppResult<Vec<u8>> {
    let fp: FilePath = path.parse().expect("FilePath::from_str is infallible");
    let bytes = tokio::task::spawn_blocking(move || app.fs().read(fp))
        .await
        .map_err(|e| AppError::Internal(format!("read task: {e}")))?
        .map_err(|e| AppError::Internal(format!("read image file: {e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Internal("picked image is empty".into()));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AppError::Internal(format!(
            "image is too large ({} MiB); max is {} MiB",
            bytes.len() / (1024 * 1024),
            MAX_IMAGE_BYTES / (1024 * 1024)
        )));
    }
    Ok(bytes)
}

/// Best-effort image content-type: magic-byte sniff first (robust for Android
/// `content://` URIs with opaque names), falling back to the file extension,
/// then `image/jpeg`.
fn sniff_image_content_type(bytes: &[u8], path: &str) -> &'static str {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return "image/jpeg";
    }
    if bytes.len() >= 8 && bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return "image/png";
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return "image/gif";
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

#[tauri::command]
pub async fn library_upload_album_cover(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    album_id: String,
    path: String,
) -> AppResult<()> {
    let bytes = read_image_file(app, &path).await?;
    let content_type = sniff_image_content_type(&bytes, &path);
    let auth = auth_handle(&state).await?;
    auth.upload_album_cover(&album_id, bytes, content_type).await
}

#[tauri::command]
pub async fn library_upload_artist_image(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    artist_id: String,
    path: String,
) -> AppResult<()> {
    let bytes = read_image_file(app, &path).await?;
    let content_type = sniff_image_content_type(&bytes, &path);
    let auth = auth_handle(&state).await?;
    auth.upload_artist_image(&artist_id, bytes, content_type).await
}

#[cfg(test)]
mod tests {
    use super::sniff_image_content_type;

    #[test]
    fn sniffs_by_magic_bytes() {
        assert_eq!(sniff_image_content_type(&[0xFF, 0xD8, 0xFF, 0x00], "x"), "image/jpeg");
        assert_eq!(
            sniff_image_content_type(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], "x"),
            "image/png"
        );
        assert_eq!(sniff_image_content_type(b"GIF89a....", "x"), "image/gif");
        let webp = b"RIFF\x00\x00\x00\x00WEBP....";
        assert_eq!(sniff_image_content_type(webp, "x"), "image/webp");
    }

    #[test]
    fn falls_back_to_extension_then_jpeg() {
        // Opaque bytes + a .png name → png by extension.
        assert_eq!(sniff_image_content_type(&[0, 1, 2, 3], "/a/b/c.PNG"), "image/png");
        // No magic, no usable extension → jpeg.
        assert_eq!(sniff_image_content_type(&[0, 1, 2, 3], "msf%3A13974"), "image/jpeg");
    }
}
