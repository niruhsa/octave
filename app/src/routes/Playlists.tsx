import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { playlistCreate, playlistList } from "../ipc";
import { formatError } from "../lib/error";
import { SourceBadge } from "../components/SourceBadge";
import { broadcastInvalidate } from "../App";

/**
 * /playlists — the current user's playlists. Online → server list (mirrored
 * into the cache so the next offline view has them); offline → cache only.
 * A locally-created playlist (whose `playlist.create` op is still queued)
 * carries a `local` flag and is badged "unsynced".
 */
export default function Playlists() {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const q = useQuery({
    queryKey: ["playlists", "mine"],
    queryFn: playlistList,
  });

  async function create(e: React.FormEvent) {
    e.preventDefault();
    const trimmed = name.trim();
    if (!trimmed) return;
    setBusy(true);
    setErr(null);
    try {
      await playlistCreate(trimmed);
      setName("");
      broadcastInvalidate(["playlists", "mine"]);
      await qc.invalidateQueries({ queryKey: ["playlists", "mine"] });
      broadcastInvalidate(["playlists"]);
    } catch (e) {
      setErr(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Playlists</h1>
          <p className="text-xs text-neutral-500">yours · server-synced</p>
        </div>
        {q.data && <SourceBadge source={q.data.source} />}
      </header>

      <form onSubmit={create} className="flex flex-wrap items-center gap-2">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="New playlist name…"
          maxLength={200}
          className="flex-1 rounded border border-neutral-700 bg-neutral-900 px-3 py-1.5 text-sm focus:border-blue-500 focus:outline-none"
        />
        <button
          type="submit"
          disabled={busy || !name.trim()}
          className="rounded bg-blue-600 px-3 py-1.5 text-sm text-white hover:bg-blue-500 disabled:opacity-50"
        >
          {busy ? "…" : "Create"}
        </button>
      </form>
      {err && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {err}
        </p>
      )}

      {q.isLoading && <p className="text-sm text-neutral-400">Loading…</p>}
      {q.isError && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(q.error)}
        </p>
      )}

      {q.data && (
        <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
          {q.data.items.length === 0 ? (
            <li className="p-3 text-sm text-neutral-500">No playlists yet.</li>
          ) : (
            q.data.items.map((p) => (
              <li key={p.id}>
                <Link
                  to={`/playlists/${encodeURIComponent(p.id)}`}
                  className="flex items-center justify-between p-3 text-sm hover:bg-neutral-800/50"
                >
                  <span className="truncate">{p.name}</span>
                  {p.local && (
                    <span
                      className="rounded bg-amber-900/40 px-1.5 py-0.5 text-xs text-amber-200"
                      title="Created offline; waiting to sync"
                    >
                      unsynced
                    </span>
                  )}
                </Link>
              </li>
            ))
          )}
        </ul>
      )}
    </section>
  );
}
