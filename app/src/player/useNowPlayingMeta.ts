// Best-effort display names for the now-playing track.
//
// `MergedTrack` carries `album_id` + `artist_id` but no human names, so we
// resolve the album title + artist name from the offline cache (the same
// source the Album route uses). Names are present for downloaded/cached
// content; stream-only-and-uncached tracks resolve to `null` and the UI
// degrades gracefully (title-only, like the rest of the app).
//
// Queried via React Query and keyed identically to `Album.tsx`'s album query
// (`["cache","album",id]`) so the lookups dedupe across the app.

import { useQuery } from "@tanstack/react-query";
import { cacheGetAlbum, cacheGetArtist } from "../ipc";
import type { MergedTrack } from "../ipc";

export type NowPlayingMeta = {
  albumTitle: string | null;
  artistName: string | null;
};

export function useNowPlayingMeta(track: MergedTrack | null): NowPlayingMeta {
  const albumId = track?.album_id;
  const artistId = track?.artist_id;

  const album = useQuery({
    queryKey: ["cache", "album", albumId],
    queryFn: () => cacheGetAlbum(albumId!),
    enabled: !!albumId,
    staleTime: 5 * 60 * 1000,
  });
  const artist = useQuery({
    queryKey: ["cache", "artist", artistId],
    queryFn: () => cacheGetArtist(artistId!),
    enabled: !!artistId,
    staleTime: 5 * 60 * 1000,
  });

  return {
    albumTitle: album.data?.title ?? null,
    artistName: artist.data?.name ?? null,
  };
}
