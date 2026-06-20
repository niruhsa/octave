//! Metadata editing (opt-in, manual) with optional file tag write-back.
//!
//! Wraps [`LibraryService::update_track`] (which already audits before/after)
//! and, when `write_tags` is enabled, mirrors the edited fields back into the
//! file's audio tags via [`crate::services::tag::write_tags`].
//!
//! The DB stays authoritative: the row is updated first, then the file is
//! best-effort synced. A write-back failure is surfaced to the caller (the
//! DB change is already audited), but the on-disk file is left as-is.

use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{PermissionLevel, Track};
use crate::error::Result;
use crate::services::library::LibraryService;
use crate::services::tag::{self, TagWrite};

/// A single metadata edit. `None` fields are left unchanged.
#[derive(Debug, Clone, Default)]
pub struct MetadataEdit {
    pub title: Option<String>,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    /// Free-form JSON blob stored on the track row. `None` leaves the
    /// existing `metadata_json` untouched.
    pub metadata_json: Option<String>,
    /// Year to write back to the file tag (not a DB column on tracks).
    /// Ignored when `write_tags` is disabled.
    pub year: Option<i32>,
}

#[derive(Clone)]
pub struct MetadataService {
    pub library: LibraryService,
    /// When `true`, edited fields are written back to the file's audio tags.
    pub write_tags: bool,
}

impl MetadataService {
    pub fn new(library: LibraryService, write_tags: bool) -> Self {
        Self {
            library,
            write_tags,
        }
    }

    /// Apply `edit` to track `id`. Manager+ (enforced by `update_track`).
    ///
    /// Updates the DB row (audited), then optionally writes the changed tag
    /// fields back to the file when server-side write-back is enabled.
    pub async fn edit_track(
        &self,
        caller: &Identity,
        id: Uuid,
        edit: MetadataEdit,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;

        // Load current row so unspecified fields keep their values and we have
        // the file_path for write-back.
        let before = self.library.get_track(caller, id).await?;

        let title = edit.title.clone().unwrap_or_else(|| before.title.clone());
        let track_no = edit.track_no.or(before.track_no);
        let disc_no = edit.disc_no.or(before.disc_no);
        let metadata_json = edit
            .metadata_json
            .clone()
            .unwrap_or_else(|| before.metadata_json.clone());

        let updated = self
            .library
            .update_track(caller, id, &title, track_no, disc_no, &metadata_json)
            .await?;

        if self.write_tags {
            let tw = TagWrite {
                title: edit.title,
                artist: None,
                album: None,
                track_no: edit.track_no,
                disc_no: edit.disc_no,
                year: edit.year,
            };
            if !tw.is_empty() {
                match tag::write_tags(std::path::Path::new(&updated.file_path), &tw) {
                    Ok(()) => debug!(track_id = %id, "metadata: tags written back to file"),
                    Err(e) => {
                        warn!(track_id = %id, error = %e, "metadata: tag write-back failed");
                        return Err(e);
                    }
                }
            }
        }

        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_defaults_are_none() {
        let e = MetadataEdit::default();
        assert!(e.title.is_none());
        assert!(e.track_no.is_none());
        assert!(e.year.is_none());
    }
}
