import { useQuery } from "@tanstack/react-query";

import { discoverSimilar } from "../ipc";
import { serverTrackToQueueItem, usePlayerStore } from "../player/store";
import { trackMetaLine } from "../lib/trackMeta";
import { useTrackNames } from "../lib/useTrackNames";
import { Cover } from "./Cover";
import { RadioIcon } from "./icons";

/**
 * Acoustic "Sounds like this" shelf (Phase 12).
 *
 * Given a seed track, fetches its nearest acoustic neighbors from the server
 * (`discover_similar`) and renders them as a horizontal shelf. Clicking a tile
 * plays the shelf as a queue starting there. The server gracefully falls back
 * to the seed's same-artist tracks when it has no embedding yet, so this is
 * always safe to render; it **self-hides** when the seed resolves to nothing
 * (e.g. an empty library or an offline transport error).
 */
export function SoundsLikeShelf({
  seedTrackId,
  title = "Sounds like this",
  limit = 12,
}: {
  seedTrackId: string;
  title?: string;
  limit?: number;
}) {
  const playQueue = usePlayerStore((s) => s.playQueue);
  const q = useQuery({
    queryKey: ["discover_similar", seedTrackId, limit],
    queryFn: () => discoverSimilar(seedTrackId, limit),
    enabled: Boolean(seedTrackId),
    // Neighbors are stable until the catalog/analysis changes — cache a while.
    staleTime: 5 * 60_000,
    retry: false,
  });

  const tracks = q.data ?? [];
  const trackNames = useTrackNames(tracks);
  if (tracks.length === 0) return null;

  return (
    <div>
      <h2 className="mb-3 flex items-center gap-2 font-mono text-[11px] tracking-[0.14em] text-oct-faint">
        <RadioIcon size={13} /> {title.toUpperCase()}
      </h2>
      <div className="grid grid-cols-3 gap-4 sm:grid-cols-4 lg:grid-cols-6">
        {tracks.map((t, i) => {
          const m = trackNames(t);
          const sub = trackMetaLine(m.artistName, m.albumTitle);
          return (
            <button
              key={t.id}
              onClick={() => playQueue(tracks.map(serverTrackToQueueItem), i)}
              className="group flex flex-col gap-2 text-left"
              title={`Play "${t.title}"`}
            >
              <Cover album={{ id: t.album_id }} tryCover className="w-full" />
              <span className="min-w-0">
                <span className="block truncate text-[12.5px] font-medium group-hover:text-white">
                  {t.title}
                </span>
                {sub && <span className="block truncate text-[11px] text-oct-subtle">{sub}</span>}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
