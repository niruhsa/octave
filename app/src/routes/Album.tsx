import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import {
  downloadAlbum,
  downloadDelete,
  downloadTrack,
  libraryListTracksByAlbum,
} from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatDuration } from "../lib/format";
import { formatError } from "../lib/error";
import { usePlayerStore } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import type { MergedTrack } from "../ipc";

export default function Album() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["library", "tracks-by-album", id],
    queryFn: () => libraryListTracksByAlbum(id),
    enabled: !!id,
  });
  const playTrack = usePlayerStore((s) => s.playTrack);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);

  const playFrom = (track: MergedTrack) => {
    const items = q.data?.items ?? [];
    playTrack(track, items);
  };

  async function dlTrack(track: MergedTrack) {
    try {
      await downloadTrack(track.id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }

  async function dlAlbum() {
    try {
      await downloadAlbum(id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }

  async function removeTrack(track: MergedTrack) {
    try {
      await downloadDelete(track.id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }

  const anyDownloaded = q.data?.items.some((t) => t.downloaded) ?? false;

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <Link to="/library" className="text-sm text-blue-400 hover:underline">
            ← Library
          </Link>
          <h1 className="text-2xl font-semibold">Tracks</h1>
          <p className="text-xs text-neutral-500">album {id}</p>
        </div>
        {q.data && <SourceBadge source={q.data.source} />}
      </header>

      {q.data && q.data.items.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={() => playQueue(q.data!.items, 0)}
            className="rounded bg-blue-600 px-3 py-1.5 text-sm text-white hover:bg-blue-500"
          >
            ▶ Play album
          </button>
          <button
            onClick={dlAlbum}
            className="rounded border border-neutral-700 px-3 py-1.5 text-sm hover:bg-neutral-800"
          >
            ⬇ Download album
          </button>
          {anyDownloaded && (
            <Link
              to="/downloads"
              className="text-xs text-blue-400 underline"
            >
              manage downloads
            </Link>
          )}
        </div>
      )}

      {q.isLoading && <p className="text-sm text-neutral-400">Loading…</p>}
      {q.isError && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(q.error)}
        </p>
      )}

      {q.data && (
        <ol className="divide-y divide-neutral-800 rounded border border-neutral-800">
          {q.data.items.length === 0 ? (
            <li className="p-3 text-sm text-neutral-500">No tracks.</li>
          ) : (
            q.data.items.map((t, i) => (
              <li
                key={t.id}
                className="flex cursor-pointer items-center gap-3 p-3 text-sm hover:bg-neutral-800/50"
                onClick={() => playFrom(t)}
              >
                <span className="w-6 text-right text-neutral-500">
                  {t.track_no ?? i + 1}
                </span>
                <DownloadedDot downloaded={t.downloaded} />
                <span className="flex-1 truncate">{t.title}</span>
                <span className="text-xs text-neutral-500">{t.codec}</span>
                <span className="w-12 text-right tabular-nums text-neutral-500">
                  {formatDuration(t.duration_ms)}
                </span>
                <span className="flex gap-1" onClick={(e) => e.stopPropagation()}>
                  {t.downloaded ? (
                    <button
                      onClick={() => void removeTrack(t)}
                      className="rounded border border-neutral-700 px-1.5 py-0.5 text-xs hover:bg-neutral-800"
                      title="Remove download"
                    >
                      ✕
                    </button>
                  ) : (
                    <button
                      onClick={() => void dlTrack(t)}
                      className="rounded border border-neutral-700 px-1.5 py-0.5 text-xs hover:bg-neutral-800"
                      title="Download for offline"
                    >
                      ⬇
                    </button>
                  )}
                </span>
              </li>
            ))
          )}
        </ol>
      )}
    </section>
  );
}
