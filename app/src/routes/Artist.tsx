import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useNavigate } from "react-router-dom";
import { libraryDeleteArtist, libraryListAlbumsByArtist } from "../ipc";
import { Cover } from "../components/Cover";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";

export default function Artist() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tier = useAppStore((s) => s.tier);
  const isManager = tier === "admin" || tier === "manager";

  const q = useQuery({
    queryKey: ["library", "albums-by-artist", id],
    queryFn: () => libraryListAlbumsByArtist(id),
    enabled: !!id,
  });

  async function delArtist() {
    if (!window.confirm("Permanently delete this artist and all their albums/tracks from the server?")) return;
    try {
      await libraryDeleteArtist(id);
      await qc.invalidateQueries({ queryKey: ["library"] });
      broadcastInvalidate(["library"]);
      navigate("/library");
    } catch (e) {
      alert(formatError(e));
    }
  }

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
        <div className="flex items-center gap-3">
          {q.data && <SourceBadge source={q.data.source} />}
          {isManager && (
            <button
              onClick={delArtist}
              className="rounded border border-red-800 px-3 py-1 text-sm text-red-400 hover:bg-red-900/20"
            >
              ✕ Delete artist
            </button>
          )}
        </div>
      </header>

      {q.isLoading && <p className="text-sm text-neutral-400">Loading…</p>}
      {q.isError && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(q.error)}
        </p>
      )}

      {q.data && (
        <ul className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
          {q.data.items.length === 0 ? (
            <li className="text-sm text-neutral-500">No albums.</li>
          ) : (
            q.data.items.map((a) => (
              <li
                key={a.id}
                className="flex flex-col gap-2 rounded border border-neutral-800 p-3"
              >
                <Cover album={a} />
                <div className="flex items-start gap-2">
                  <DownloadedDot downloaded={a.downloaded} />
                  <div className="flex-1 min-w-0">
                    <Link
                      to={`/albums/${a.id}`}
                      className="block text-sm font-medium hover:underline truncate"
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