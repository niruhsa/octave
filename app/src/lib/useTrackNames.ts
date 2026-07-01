// Batch artist-name + album-title resolution for track listings.
//
// Server tracks (`MergedTrack` / `FavoriteTrack` / the player's `QueueItem`)
// carry `artist_id` + `album_id` but no human names. We resolve each *unique*
// id once via the server-first, cache-fallback library getters — keyed
// identically to the Album/Artist routes (`["library","album"|"artist",id]`)
// so every lookup dedupes app-wide and still resolves offline from the cache.
//
// Returns a getter (not one hook per row), so a list renders `Artist • Album`
// per track without violating the rules of hooks.

import { useQueries } from "@tanstack/react-query";
import { libraryGetAlbum, libraryGetArtist } from "../ipc";

type TrackIds = { artist_id?: string | null; album_id?: string | null };

export type TrackNames = { artistName: string | null; albumTitle: string | null };

const STALE = 5 * 60 * 1000;

const uniq = (xs: (string | null | undefined)[]): string[] => [
  ...new Set(xs.filter((x): x is string => !!x)),
];

export function useTrackNames(tracks: TrackIds[]): (t: TrackIds) => TrackNames {
  const albumIds = uniq(tracks.map((t) => t.album_id));
  const artistIds = uniq(tracks.map((t) => t.artist_id));

  const albumQs = useQueries({
    queries: albumIds.map((id) => ({
      queryKey: ["library", "album", id],
      queryFn: () => libraryGetAlbum(id),
      staleTime: STALE,
    })),
  });
  const artistQs = useQueries({
    queries: artistIds.map((id) => ({
      queryKey: ["library", "artist", id],
      queryFn: () => libraryGetArtist(id),
      staleTime: STALE,
    })),
  });

  const albumMap = new Map<string, string>();
  albumIds.forEach((id, i) => {
    const title = albumQs[i]?.data?.title;
    if (title) albumMap.set(id, title);
  });
  const artistMap = new Map<string, string>();
  artistIds.forEach((id, i) => {
    const name = artistQs[i]?.data?.name;
    if (name) artistMap.set(id, name);
  });

  return (t) => ({
    artistName: t.artist_id ? artistMap.get(t.artist_id) ?? null : null,
    albumTitle: t.album_id ? albumMap.get(t.album_id) ?? null : null,
  });
}
