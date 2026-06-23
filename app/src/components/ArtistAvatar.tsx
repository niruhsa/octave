import { artistImageUrl } from "../ipc";
import { ArtistIcon } from "./icons";
import { FallbackImg } from "./FallbackImg";

/**
 * Circular artist thumbnail for browse/search lists. Shows the uploaded
 * artist image (served via the `cover://artist/<id>` proxy) when one exists,
 * layered over an `ArtistIcon` placeholder — so a missing image, a 404, or a
 * load error all fall back to the icon via `<img onerror>`.
 */
export function ArtistAvatar({
  id,
  imagePath,
  size = 36,
  version,
}: {
  id: string;
  /** Server image path (presence decides whether to attempt the image). */
  imagePath?: string | null;
  size?: number;
  /** Cache-bust token — bump after a re-upload to force a reload. */
  version?: string | number;
}) {
  return (
    <span
      className="relative grid shrink-0 place-items-center overflow-hidden rounded-full bg-oct-elevated text-oct-subtle"
      style={{ width: size, height: size }}
    >
      <ArtistIcon size={Math.round(size * 0.44)} />
      {/* low-res variant: ~64px is plenty for a 32–36px avatar, and loads
          instantly from the native image cache (no jarring pop-in). */}
      <FallbackImg
        src={imagePath ? artistImageUrl(id, version, true) : null}
        className="absolute inset-0 h-full w-full object-cover"
      />
    </span>
  );
}
