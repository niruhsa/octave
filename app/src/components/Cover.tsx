import { coverUrl } from "../ipc";
import type { MergedAlbum } from "../ipc";

/**
 * Renders an album cover. Prefers the downloaded local cover (via the
 * `cover://` protocol) so it shows offline; falls back to the server's
 * `cover_path` when online and no local copy exists; falls back to a
 * neutral placeholder otherwise.
 *
 * `size` is the square edge in px.
 */
export function Cover({
  album,
  size = 160,
}: {
  album: { local_cover_path: string | null; cover_path: string | null };
  size?: number;
}) {
  const src = album.local_cover_path ? coverUrl((album as MergedAlbum).id) : null;
  return (
    <div
      className="relative overflow-hidden rounded bg-neutral-800"
      style={{ width: size, height: size }}
    >
      {src ? (
        <img
          src={src}
          width={size}
          height={size}
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
