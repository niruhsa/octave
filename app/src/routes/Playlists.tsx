import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { playlistCreate, playlistList } from "../ipc";
import { formatError } from "../lib/error";
import { SourceBadge } from "../components/SourceBadge";
import { PlaylistIcon, PlusIcon } from "../components/icons";
import { broadcastInvalidate } from "../App";
import { btnPrimary, card, errorBox, input } from "../lib/ui";
import { SkeletonList } from "../components/Skeleton";

/**
 * /playlists — the current user's playlists. Online → server list (mirrored
 * into the cache); offline → cache only. A locally-created playlist (create
 * op still queued) carries a `local` flag and is badged "unsynced".
 */
export default function Playlists() {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const q = useQuery({ queryKey: ["playlists", "mine"], queryFn: playlistList });

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

  const items = q.data?.items ?? [];

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <header className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-[27px] font-semibold tracking-tight">Playlists</h1>
          <p className="mt-1 font-mono text-[11.5px] text-oct-subtle">
            {items.length} playlist{items.length === 1 ? "" : "s"} · server-synced
          </p>
        </div>
        {q.data && <SourceBadge source={q.data.source} />}
      </header>

      <form onSubmit={create} className="flex flex-wrap items-center gap-2">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="New playlist name…"
          maxLength={200}
          className={`${input} flex-1`}
        />
        <button type="submit" disabled={busy || !name.trim()} className={btnPrimary}>
          <PlusIcon size={14} /> {busy ? "…" : "Create"}
        </button>
      </form>
      {err && <p className={errorBox}>{err}</p>}

      {q.isLoading && <SkeletonList rows={6} avatar="square" trailing={false} />}
      {q.isError && <p className={errorBox}>{formatError(q.error)}</p>}

      {q.data && (
        <div className={`${card} divide-y divide-oct-border`}>
          {items.length === 0 ? (
            <p className="p-4 text-sm text-oct-subtle">No playlists yet.</p>
          ) : (
            items.map((p) => (
              <Link
                key={p.id}
                to={`/playlists/${encodeURIComponent(p.id)}`}
                className="group flex items-center gap-3 px-3 py-2.5 first:rounded-t-xl last:rounded-b-xl hover:bg-oct-elevated/50"
              >
                <span
                  className="grid h-10 w-10 shrink-0 place-items-center rounded-lg text-oct-accent"
                  style={{ background: "rgba(224,168,75,0.12)" }}
                >
                  <PlaylistIcon size={18} />
                </span>
                <span className="flex-1 truncate text-[13.5px] group-hover:text-white">{p.name}</span>
                {p.local && (
                  <span
                    className="rounded-md bg-oct-accent/15 px-1.5 py-0.5 font-mono text-[10px] text-oct-accent-bright"
                    title="Created offline; waiting to sync"
                  >
                    UNSYNCED
                  </span>
                )}
              </Link>
            ))
          )}
        </div>
      )}
    </section>
  );
}
