// Track subtitle formatting — the "Artist • Album" line shown under a song
// title across every track listing. Either part may be missing (a stream-only
// track whose names haven't resolved yet, a single with no album), so the
// helper falls back to whichever piece is present and yields `null` when
// neither is — letting callers omit the line entirely.

/** The mid-dot separator used between artist and album everywhere. */
export const META_SEP = "•";

/**
 * Join an artist name and album title into one subtitle line, e.g.
 * `Stereolab • Dots and Loops`. Returns whichever side is present when only
 * one resolved, or `null` when neither did.
 */
export function trackMetaLine(
  artistName: string | null | undefined,
  albumTitle: string | null | undefined,
): string | null {
  const a = artistName?.trim() || null;
  const b = albumTitle?.trim() || null;
  if (a && b) return `${a} ${META_SEP} ${b}`;
  return a || b || null;
}
