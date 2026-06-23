-- Artist images (Phase 9 metadata-editing extension).
-- Adds an optional path to a manager-uploaded artist image, stored under
-- ARTWORK_PATH and served via GET /artists/:id/image. Nullable: artists
-- default to no image (the client renders a gradient placeholder).
ALTER TABLE artists ADD COLUMN image_path TEXT;
