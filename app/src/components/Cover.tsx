import { coverUrl } from "../ipc";
import type { MergedAlbum } from "../ipc";
import { gradientFor } from "../lib/visual";

/**
 * Album-art tile in the OCTAVE style.
 *
 * Renders a deterministic gradient + concentric "vinyl" motif as the base
 * (so every album reads as distinct even with no artwork), and layers the
 * real cover on top when one exists. The cover is served via the `cover://`
 * protocol, which resolves a downloaded local cover first (works offline)
 * and otherwise proxies the server's auth-gated `GET /albums/:id/cover`. If
 * the protocol 404s, `<img onerror>` hides the image and the gradient
 * placeholder shows through.
 *
 * `badge` (top-left) and `quality` (top-right) are optional overlay slots
 * used by the album grid for SAVED/STREAM + hi-res chips.
 */
export function Cover({
  album,
  size = 160,
  radius = 9,
  badge,
  quality,
  className = "",
  tryCover = false,
}: {
  album: { id?: string; local_cover_path?: string | null; cover_path?: string | null };
  size?: number;
  radius?: number;
  badge?: React.ReactNode;
  quality?: React.ReactNode;
  className?: string;
  /** Attempt the cover by id even when cover paths are unknown. */
  tryCover?: boolean;
}) {
  const id = (album as MergedAlbum).id;
  const hasCover = Boolean(album.local_cover_path || album.cover_path);
  const src = id && (hasCover || tryCover) ? coverUrl(id) : null;

  return (
    <div
      className={`relative w-full overflow-hidden ${className}`}
      style={{
        aspectRatio: "1 / 1",
        maxWidth: size,
        borderRadius: radius,
        background: gradientFor(id),
      }}
    >
      {/* vinyl motif (shows when no cover, sits behind the cover when present) */}
      <div className="absolute inset-0 grid place-items-center">
        <div
          className="grid aspect-square w-[54%] place-items-center rounded-full"
          style={{ border: "1px solid rgba(255,255,255,0.16)" }}
        >
          <span
            className="aspect-square w-[14%] rounded-full"
            style={{ background: "rgba(255,255,255,0.22)" }}
          />
        </div>
      </div>

      {src && (
        <img
          src={src}
          alt=""
          className="absolute inset-0 h-full w-full object-cover"
          loading="lazy"
          onError={(e) => {
            (e.currentTarget as HTMLImageElement).style.display = "none";
          }}
        />
      )}

      {badge && <div className="absolute left-2.5 top-2.5 z-10">{badge}</div>}
      {quality && (
        <div className="absolute right-2.5 top-2.5 z-10 rounded-md bg-black/35 px-1.5 py-0.5 font-mono text-[9.5px] text-white/75">
          {quality}
        </div>
      )}
    </div>
  );
}

/**
 * Small square gradient "vinyl" thumbnail for player bar / queue rows where a
 * full Cover is overkill. Layers the real cover on top when available.
 */
export function Thumb({
  album,
  size,
  radius = 7,
  className = "",
  tryCover = false,
}: {
  album?: { id?: string; local_cover_path?: string | null; cover_path?: string | null } | null;
  size: number;
  radius?: number;
  className?: string;
  /** Attempt the cover by id even when cover paths are unknown (player bar). */
  tryCover?: boolean;
}) {
  const id = album?.id;
  const hasCover = Boolean(album?.local_cover_path || album?.cover_path);
  const src = id && (hasCover || tryCover) ? coverUrl(id) : null;
  return (
    <div
      className={`relative shrink-0 overflow-hidden ${className}`}
      style={{ width: size, height: size, borderRadius: radius, background: gradientFor(id) }}
    >
      <div className="absolute inset-0 grid place-items-center">
        <span
          className="aspect-square w-[46%] rounded-full"
          style={{ border: "1px solid rgba(255,255,255,0.18)" }}
        />
      </div>
      {src && (
        <img
          src={src}
          alt=""
          className="absolute inset-0 h-full w-full object-cover"
          loading="lazy"
          onError={(e) => {
            (e.currentTarget as HTMLImageElement).style.display = "none";
          }}
        />
      )}
    </div>
  );
}
