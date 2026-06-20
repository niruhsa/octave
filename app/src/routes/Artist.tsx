import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { libraryListAlbumsByArtist } from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";

export default function Artist() {
  const { id = "" } = useParams();
  const q = useQuery({
    queryKey: ["library", "albums-by-artist", id],
    queryFn: () => libraryListAlbumsByArtist(id),
    enabled: !!id,
  });

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <Link to="/library" className="text-sm text-blue-400 hover:underline">
            ← Library
          </Link>
          <h1 className="text-2xl font-semibold">Albums</h1>
          <p className="text-xs text-neutral-500">artist {id}</p>
        </div>
        {q.data && <SourceBadge source={q.data.source} />}
      </header>

      {q.isLoading && <p className="text-sm text-neutral-400">Loading…</p>}
      {q.isError && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(q.error)}
        </p>
      )}

      {q.data && (
        <ul className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-3">
          {q.data.items.length === 0 ? (
            <li className="text-sm text-neutral-500">No albums.</li>
          ) : (
            q.data.items.map((a) => (
              <li
                key={a.id}
                className="flex flex-col gap-2 rounded border border-neutral-800 p-3"
              >
                <div className="aspect-square w-full rounded bg-neutral-800" />
                <div className="flex items-start gap-2">
                  <DownloadedDot downloaded={a.downloaded} />
                  <div className="flex-1">
                    <Link
                      to={`/albums/${a.id}`}
                      className="block text-sm font-medium hover:underline"
                    >
                      {a.title}
                    </Link>
                    {a.release_year && (
                      <p className="text-xs text-neutral-500">
                        {a.release_year}
                      </p>
                    )}
                  </div>
                </div>
              </li>
            ))
          )}
        </ul>
      )}
    </section>
  );
}
