import { coverUrl } from "../ipc";
import type { MergedAlbum } from "../ipc";

/**
 * Renders an album cover via the `cover://` protocol, which resolves a
 * downloaded local cover first (works offline) and otherwise proxies the
 * server's auth-gated `GET /albums/:id/cover` so online albums still show
 * their artwork. Falls back to a neutral placeholder when neither exists
 * (the protocol returns 404 → `<img onerror>` hides the image).
 *
 * The cover container is responsive: `w-full` with `aspectRatio: "1 / 1"`
 * and optional `maxWidth` (defaults to 160px). The image always fills the
 * container with `object-cover`, ensuring square covers fit without
 * overflow.
 */
export function Cover({
  album,
  size = 160,
}: {
  album: { id?: string; local_cover_path: string | null; cover_path: string | null };
  size?: number;
}) {
  // Use the protocol whenever a cover exists on either side (local or
  // server) and we have an album id to address it by.
  const id = (album as MergedAlbum).id;
  const hasCover = Boolean(album.local_cover_path || album.cover_path);
  const src = id && hasCover ? coverUrl(id) : null;
  return (
    <div
      className="relative w-full overflow-hidden rounded bg-neutral-800"
      style={{ aspectRatio: "1 / 1", maxWidth: size }}
    >
      {src ? (
        <img
          src={src}
          alt="cover"
          className="h-full w-full object-cover"
          loading="lazy"
          onError={(e) => {
            (e.currentTarget as HTMLImageElement).style.display = "none";
          }}
        />
      ) : (
        <div className="flex h-full w-full items-center justify-center text-neutral-600">
          ♪
        </div>
      )}
    </div>
  );
}
