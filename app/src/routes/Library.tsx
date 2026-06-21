import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { libraryListArtists, libraryDeleteArtist, libraryRescan } from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";

const PAGE_SIZE = 50;

export default function Library() {
  const [page, setPage] = useState(0);
  const offset = page * PAGE_SIZE;
  const qc = useQueryClient();
  const tier = useAppStore((s) => s.tier);
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
    } catch (e) { alert(formatError(e)); }
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

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Library</h1>
          <p className="text-sm text-neutral-400">Artists</p>
        </div>
        <div className="flex items-center gap-3">
          {q.data && <SourceBadge source={q.data.source} />}
          {isManager && (
            <button
              onClick={doRescan}
              disabled={rescanning}
              className="rounded border border-amber-800 px-2 py-1 text-xs text-amber-400 hover:bg-amber-900/20 disabled:opacity-50"
              title="Re-measure durations for all tracks"
            >
              {rescanning ? "↻ Rescanning…" : "⟳ Rescan"}
            </button>
          )}
          <Link to="/search" className="text-sm text-blue-400 hover:underline">
            Search
          </Link>
          <Link to="/" className="text-sm text-blue-400 hover:underline">
            Home
          </Link>
        </div>
      </header>

      {q.isLoading && <p className="text-sm text-neutral-400">Loading…</p>}
      {q.isError && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(q.error)}
        </p>
      )}
      {rescanResult && (
        <p className="rounded border border-amber-800 bg-amber-900/20 p-2 text-xs text-amber-200">
          {rescanResult}
        </p>
      )}

      {q.data && (
        <>
          <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
            {q.data.items.length === 0 ? (
              <li className="p-3 text-sm text-neutral-500">No artists.</li>
            ) : (
              q.data.items.map((a) => (
                <li key={a.id} className="flex items-center gap-3 p-3">
                  <DownloadedDot downloaded={a.downloaded} />
                  <Link
                    to={`/artists/${a.id}`}
                    className="flex-1 text-sm hover:underline"
                  >
                    {a.name}
                  </Link>
                  {a.sort_name && a.sort_name !== a.name && (
                    <span className="text-xs text-neutral-500">
                      {a.sort_name}
                    </span>
                  )}
                  {isManager && (
                    <button
                      onClick={() => void delArtist(a.id, a.name)}
                      className="rounded border border-red-800 px-1.5 py-0.5 text-xs text-red-400 hover:bg-red-900/20"
                      title="Delete artist"
                    >
                      🗑
                    </button>
                  )}
                </li>
              ))
            )}
          </ul>

          <nav className="flex items-center justify-between text-sm">
            <button
              disabled={page === 0}
              onClick={() => setPage((p) => Math.max(0, p - 1))}
              className="rounded border border-neutral-700 px-2 py-1 disabled:opacity-50"
            >
              ‹ Prev
            </button>
            <span className="text-neutral-500">
              {q.data.total !== undefined
                ? `${offset + 1}–${Math.min(offset + q.data.items.length, q.data.total)} of ${q.data.total}`
                : `${q.data.items.length} items`}
            </span>
            <button
              disabled={
                q.data.total !== undefined
                  ? offset + PAGE_SIZE >= q.data.total
                  : q.data.items.length < PAGE_SIZE
              }
              onClick={() => setPage((p) => p + 1)}
              className="rounded border border-neutral-700 px-2 py-1 disabled:opacity-50"
            >
              Next ›
            </button>
          </nav>
        </>
      )}
    </section>
  );
}