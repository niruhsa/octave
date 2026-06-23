//! Image optimization — downscale + re-encode cover/artist artwork to small
//! JPEGs so the client loads them fast.
//!
//! Optimized variants live in `<ARTWORK_PATH>/.optimized/<key>.jpg` (a parallel
//! cache; the pristine source is left untouched, so re-optimizing is idempotent
//! and never re-compresses a JPEG-of-a-JPEG). A variant is considered fresh
//! while its mtime is `>=` the source's, so a re-upload (which rewrites the
//! source) transparently invalidates it.
//!
//! Trigger points (all funnel through [`ensure_optimized`] / [`optimize_file`]):
//!   * **on-demand** — the serve endpoints optimize a not-yet-optimized image
//!     at request time, then serve + cache it.
//!   * **on upload** — the upload handlers warm the cache for the new image.
//!   * **on startup + on a schedule** — [`run_optimize_pass`] walks every album
//!     cover + artist image and ensures each is optimized.

use std::path::{Path, PathBuf};

use tokio::fs;
use uuid::Uuid;

use crate::db::repo::{AlbumRepo, ArtistRepo};
use crate::error::{AppError, Result};

/// Low-resolution placeholder dimensions/quality. Tiny (~1–3 KB) so it loads
/// near-instantly and doubles as a blur-up placeholder for large covers and a
/// native-res image for small avatars.
const LOW_DIM: u32 = 64;
const LOW_QUALITY: u8 = 58;

/// Which optimized variant to produce/serve: the full optimized image or the
/// tiny low-resolution placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Full,
    Low,
}

impl Variant {
    /// Filename suffix before `.jpg` (`""` for full, `.lq` for low-res).
    fn suffix(self) -> &'static str {
        match self {
            Variant::Full => "",
            Variant::Low => ".lq",
        }
    }
}

/// Cheap to clone — just the cache dir + two encode knobs.
#[derive(Clone)]
pub struct ImageOptimizer {
    optimized_dir: PathBuf,
    max_dim: u32,
    quality: u8,
}

impl ImageOptimizer {
    /// `artwork_dir` is `ARTWORK_PATH`; optimized variants go in its
    /// `.optimized` subdirectory.
    pub fn new(artwork_dir: PathBuf, max_dim: u32, quality: u8) -> Self {
        Self {
            optimized_dir: artwork_dir.join(".optimized"),
            max_dim,
            quality,
        }
    }

    pub fn album_key(id: Uuid) -> String {
        format!("album-{id}")
    }
    pub fn artist_key(id: Uuid) -> String {
        format!("artist-{id}")
    }

    fn optimized_path(&self, key: &str, variant: Variant) -> PathBuf {
        self.optimized_dir
            .join(format!("{key}{}.jpg", variant.suffix()))
    }

    fn dim_quality(&self, variant: Variant) -> (u32, u8) {
        match variant {
            Variant::Full => (self.max_dim, self.quality),
            Variant::Low => (LOW_DIM, LOW_QUALITY),
        }
    }

    /// Return a path to serve for `source` in the requested `variant`: the
    /// cached variant when it exists and is fresh, otherwise generate it now.
    /// **Never errors** — any failure (decode error, unreadable source, etc.)
    /// falls back to the original `source` so the caller still serves
    /// *something* (only sensible for `Full`; a `Low` failure is rare).
    pub async fn ensure_optimized(&self, key: &str, source: &Path, variant: Variant) -> PathBuf {
        let opt = self.optimized_path(key, variant);
        if is_fresh(&opt, source).await {
            return opt;
        }
        match self.optimize_file(key, source, variant).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(key, ?variant, error = %e, "image optimize failed; serving original");
                source.to_path_buf()
            }
        }
    }

    /// Read `source`, optimize it into `variant`, and write the cached file for
    /// `key`. Returns the cached path on success.
    pub async fn optimize_file(&self, key: &str, source: &Path, variant: Variant) -> Result<PathBuf> {
        let bytes = fs::read(source).await.map_err(AppError::Io)?;
        let bytes_in = bytes.len();
        let (dim, quality) = self.dim_quality(variant);
        // Decode + resize + encode is CPU-bound — keep it off the async runtime.
        let optimized = tokio::task::spawn_blocking(move || encode_optimized(&bytes, dim, quality))
            .await
            .map_err(|e| AppError::Internal(format!("optimize task join: {e}")))??;
        fs::create_dir_all(&self.optimized_dir).await.map_err(AppError::Io)?;
        let opt = self.optimized_path(key, variant);
        let bytes_out = optimized.len();
        fs::write(&opt, &optimized).await.map_err(AppError::Io)?;
        tracing::debug!(key, ?variant, bytes_in, bytes_out, "image optimized");
        Ok(opt)
    }

    /// Generate both the full + low-res variants for `key` (best-effort). Used
    /// by the startup/scheduled pass and the on-upload cache warm.
    pub async fn ensure_all(&self, key: &str, source: &Path) {
        self.ensure_optimized(key, source, Variant::Full).await;
        self.ensure_optimized(key, source, Variant::Low).await;
    }
}

/// `opt` is fresh iff it exists and its mtime is `>=` the source's.
async fn is_fresh(opt: &Path, source: &Path) -> bool {
    let (Ok(om), Ok(sm)) = (fs::metadata(opt).await, fs::metadata(source).await) else {
        return false;
    };
    match (om.modified(), sm.modified()) {
        (Ok(omt), Ok(smt)) => omt >= smt,
        // Platform without mtime — treat an existing optimized file as fresh.
        _ => true,
    }
}

/// Decode `bytes`, downscale so the longest side is `<= max_dim` (preserving
/// aspect ratio; never upscales), and re-encode as JPEG at `quality`.
fn encode_optimized(bytes: &[u8], max_dim: u32, quality: u8) -> Result<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::DynamicImage;

    let img = image::load_from_memory(bytes)
        .map_err(|e| AppError::Internal(format!("decode image: {e}")))?;
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > max_dim {
        // `thumbnail` fits within the box preserving aspect ratio.
        img.thumbnail(max_dim, max_dim)
    } else {
        img
    };
    // JPEG has no alpha channel — flatten to RGB8.
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| AppError::Internal(format!("encode jpeg: {e}")))?;
    Ok(out)
}

/// Ensure every album cover + artist image is optimized. Idempotent + cheap on
/// repeat (already-fresh variants are skipped). Errors are logged, never fatal —
/// this runs on startup + on a timer.
pub async fn run_optimize_pass(albums: &dyn AlbumRepo, artists: &dyn ArtistRepo, opt: &ImageOptimizer) {
    let mut count = 0u64;
    match albums.all_cover_paths().await {
        Ok(rows) => {
            for (id, path) in rows {
                opt.ensure_all(&ImageOptimizer::album_key(id), Path::new(&path)).await;
                count += 1;
            }
        }
        Err(e) => tracing::warn!(error = %e, "optimize pass: listing album covers failed"),
    }
    match artists.all_image_paths().await {
        Ok(rows) => {
            for (id, path) in rows {
                opt.ensure_all(&ImageOptimizer::artist_key(id), Path::new(&path)).await;
                count += 1;
            }
        }
        Err(e) => tracing::warn!(error = %e, "optimize pass: listing artist images failed"),
    }
    tracing::info!(images = count, "image optimize pass complete");
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    /// Encode a solid-colour `w`×`h` PNG into memory.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, image::Rgb([10, 120, 200])));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn downscales_large_images_and_emits_jpeg() {
        let src = png_bytes(2000, 1500); // larger than the 800 cap
        let out = encode_optimized(&src, 800, 82).unwrap();
        // Decodes back as a valid image…
        let decoded = image::load_from_memory(&out).unwrap();
        // …downscaled so the longest side is the cap, aspect preserved.
        assert_eq!(decoded.width(), 800);
        assert_eq!(decoded.height(), 600);
        // …and is a JPEG (SOI marker), much smaller than the source PNG.
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
        assert!(out.len() < src.len());
    }

    #[test]
    fn leaves_small_images_at_native_size() {
        let src = png_bytes(300, 300); // smaller than the cap → not upscaled
        let out = encode_optimized(&src, 800, 82).unwrap();
        let decoded = image::load_from_memory(&out).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (300, 300));
    }

    #[test]
    fn rejects_non_image_bytes() {
        assert!(encode_optimized(b"not an image", 800, 82).is_err());
    }

    #[test]
    fn low_res_variant_is_tiny() {
        let src = png_bytes(2000, 1500);
        let full = encode_optimized(&src, 800, 82).unwrap();
        let low = encode_optimized(&src, LOW_DIM, LOW_QUALITY).unwrap();
        let decoded = image::load_from_memory(&low).unwrap();
        // Low-res longest side is capped at LOW_DIM, aspect preserved, and the
        // byte payload is far smaller than the full optimized image.
        assert_eq!(decoded.width(), LOW_DIM);
        assert_eq!(decoded.height(), LOW_DIM * 3 / 4);
        assert!(low.len() < full.len() / 4, "low {} vs full {}", low.len(), full.len());
    }

    #[test]
    fn variant_suffix_distinguishes_paths() {
        let opt = ImageOptimizer::new(std::path::PathBuf::from("/tmp/art"), 800, 82);
        let full = opt.optimized_path("album-x", Variant::Full);
        let low = opt.optimized_path("album-x", Variant::Low);
        assert!(full.to_string_lossy().ends_with("album-x.jpg"));
        assert!(low.to_string_lossy().ends_with("album-x.lq.jpg"));
        assert_ne!(full, low);
    }

    #[test]
    fn keys_are_namespaced() {
        let id = Uuid::nil();
        assert!(ImageOptimizer::album_key(id).starts_with("album-"));
        assert!(ImageOptimizer::artist_key(id).starts_with("artist-"));
        assert_ne!(ImageOptimizer::album_key(id), ImageOptimizer::artist_key(id));
    }
}
