import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { libraryListArtists, libraryDeleteArtist, libraryRescan } from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { ArtistIcon, SyncIcon, TrashIcon } from "../components/icons";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { btnGhostSm, card, errorBox, okBox } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { SkeletonList } from "../components/Skeleton";

const PAGE_SIZE = 50;

export default function Library() {
  const [page, setPage] = useState(0);
  const offset = page * PAGE_SIZE;
  const qc = useQueryClient();
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const isManager = tier === "admin" || tier === "manager";
  const [rescanning, setRescanning] = useState(false);
  const [rescanResult, setRescanResult] = useState<string | null>(null);

  const q = useQuery({
    queryKey: ["library", "artists", page],
    queryFn: () => libraryListArtists({ limit: PAGE_SIZE, offset }),
    placeholderData: (prev) => prev,
  });

  async function delArtist(id: string, name: string) {
    if (!window.confirm(`Permanently delete artist "${name}" and all their albums/tracks from the server?`)) return;
    try {
      await libraryDeleteArtist(id);
      qc.invalidateQueries({ queryKey: ["library", "artists"] });
    } catch (e) {
      alert(formatError(e));
    }
  }

  async function doRescan() {
    setRescanning(true);
    setRescanResult(null);
    try {
      const r = await libraryRescan();
      setRescanResult(`Checked ${r.tracks_checked} tracks, updated ${r.tracks_updated} durations${r.errors > 0 ? `, ${r.errors} errors` : ""}.`);
      qc.invalidateQueries({ queryKey: ["library"] });
      broadcastInvalidate(["library"]);
    } catch (e) {
      setRescanResult(formatError(e));
    }
    setRescanning(false);
  }

  const items = q.data?.items ?? [];

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <header className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-[27px] font-semibold tracking-tight">Library</h1>
          <p className="mt-1 font-mono text-[11.5px] text-oct-subtle">
            {q.data?.total !== undefined ? `${q.data.total} artists` : "Artists"}
          </p>
        </div>
        <div className="flex items-center gap-3">
          {q.data && <SourceBadge source={q.data.source} />}
          {isManager && (
            <button onClick={doRescan} {...offlineAttrs(online, rescanning, "Re-measure durations for all tracks")} className={btnGhostSm}>
              <SyncIcon size={13} className={rescanning ? "animate-octspin" : ""} />
              {rescanning ? "Rescanning…" : "Rescan"}
            </button>
          )}
        </div>
      </header>

      {q.isLoading && <SkeletonList rows={10} />}
      {q.isError && <p className={errorBox}>{formatError(q.error)}</p>}
      {rescanResult && <p className={okBox}>{rescanResult}</p>}

      {q.data && (
        <>
          <div className={`${card} divide-y divide-oct-border`}>
            {items.length === 0 ? (
              <p className="p-4 text-sm text-oct-subtle">No artists.</p>
            ) : (
              items.map((a) => (
                <div key={a.id} className="group flex items-center gap-3 px-3 py-2.5 first:rounded-t-xl last:rounded-b-xl hover:bg-oct-elevated/50">
                  <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
                    <ArtistIcon size={16} />
                  </span>
                  <Link to={`/artists/${a.id}`} className="min-w-0 flex-1">
                    <span className="block truncate text-[13.5px] group-hover:text-white">{a.name}</span>
                    {a.sort_name && a.sort_name !== a.name && (
                      <span className="block truncate font-mono text-[10.5px] text-oct-faint">{a.sort_name}</span>
                    )}
                  </Link>
                  <DownloadedDot downloaded={a.downloaded} />
                  {isManager && (
                    <button
                      onClick={() => void delArtist(a.id, a.name)}
                      {...offlineAttrs(online, false, "Delete artist")}
                      className="text-oct-dim opacity-0 transition-opacity hover:text-oct-danger group-hover:opacity-100 disabled:cursor-not-allowed disabled:text-oct-faint"
                    >
                      <TrashIcon size={15} />
                    </button>
                  )}
                </div>
              ))
            )}
          </div>

          <nav className="flex items-center justify-between text-sm text-oct-subtle">
            <button disabled={page === 0} onClick={() => setPage((p) => Math.max(0, p - 1))} className={btnGhostSm}>
              ‹ Prev
            </button>
            <span className="font-mono text-[11px] text-oct-faint">
              {q.data.total !== undefined
                ? `${offset + 1}–${Math.min(offset + items.length, q.data.total)} of ${q.data.total}`
                : `${items.length} items`}
            </span>
            <button
              disabled={q.data.total !== undefined ? offset + PAGE_SIZE >= q.data.total : items.length < PAGE_SIZE}
              onClick={() => setPage((p) => p + 1)}
              className={btnGhostSm}
            >
              Next ›
            </button>
          </nav>
        </>
      )}
    </section>
  );
}
