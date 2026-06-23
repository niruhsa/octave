import { useEffect, useState } from "react";
import { FallbackImg } from "./FallbackImg";

/**
 * Blur-up image: shows a tiny low-resolution placeholder immediately (blurred),
 * then cross-fades the full image in once it loads. This kills the "blank for a
 * second or two, then the cover pops in" jank — the low-res variant is ~1–3 KB
 * and (via the native image cache) loads near-instantly, so the user always
 * sees *something* in place.
 *
 * Both layers are absolutely positioned (`absolute inset-0 …` passed in via
 * `className`) over whatever placeholder the parent renders (gradient/vinyl/
 * icon), so a low-res miss falls through to that, and a full-image miss simply
 * leaves the blurred low-res showing.
 */
export function BlurUpImage({
  lowSrc,
  fullSrc,
  className = "",
}: {
  lowSrc: string | null;
  fullSrc: string | null;
  className?: string;
}) {
  const [fullLoaded, setFullLoaded] = useState(false);
  // Reset the fade whenever the full source changes (new id / re-upload bump).
  useEffect(() => setFullLoaded(false), [fullSrc]);

  return (
    <>
      {/* low-res placeholder: blurred + slightly scaled so the blur bleeds past
          the edges (the parent clips with overflow-hidden). Fades out as the
          full image fades in, so there's no double-exposure flash. */}
      <FallbackImg
        src={lowSrc}
        loading="eager"
        className={`${className} scale-105 blur-[6px] transition-opacity duration-500 ${
          fullLoaded ? "opacity-0" : "opacity-100"
        }`}
      />
      {/* full image: fades in on load, on top of the placeholder. */}
      <FallbackImg
        src={fullSrc}
        onLoad={() => setFullLoaded(true)}
        className={`${className} transition-opacity duration-500 ${
          fullLoaded ? "opacity-100" : "opacity-0"
        }`}
      />
    </>
  );
}
