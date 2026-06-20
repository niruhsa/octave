import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { libraryListArtists } from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";

const PAGE_SIZE = 50;

/**
 * Browse-by-artist landing page. Server-paginated; offline view drops to
 * the cache and shows only artists with at least one downloaded track.
 */
export default function Library() {
  const [page, setPage] = useState(0);
  const offset = page * PAGE_SIZE;
  const q = useQuery({
    queryKey: ["library", "artists", page],
    queryFn: () => libraryListArtists({ limit: PAGE_SIZE, offset }),
    placeholderData: (prev) => prev,
  });

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Library</h1>
          <p className="text-sm text-neutral-400">Artists</p>
        </div>
        <div className="flex items-center gap-3">
          {q.data && <SourceBadge source={q.data.source} />}
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
