import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import {
  downloadPlaylist,
  librarySearchTracks,
  playlistAddTrack,
  playlistDelete,
  playlistGet,
  playlistRemoveTrack,
  playlistRename,
  playlistReorderTrack,
  type MergedPlaylistEntry,
} from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatDuration } from "../lib/format";
import { formatError } from "../lib/error";
import { usePlayerStore } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import { useAppStore } from "../store";

/**
 * /playlists/:id — one playlist with its ordered entries. Reorder via ↑/↓
 * (drag-and-drop is a deferred polish — the reorder RPC is by position so
 * either UI maps onto it). Add tracks via an inline search backed by the
 * library search. Remove / rename / delete are one click each. The whole
 * list is queueable; offline + uncached entries surface a stub the player
 * resolves (and fails gracefully) via `media://`.
 */
export default function PlaylistDetail() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const online = useAppStore((s) => s.online);
  const session = useAppStore((s) => s.session);
  const tier = useAppStore((s) => s.tier);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);

  const [renaming, setRenaming] = useState(false);
  const [renameVal, setRenameVal] = useState("");
  const [addQuery, setAddQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const q = useQuery({
    queryKey: ["playlists", "detail", id],
    queryFn: () => playlistGet(id),
    enabled: !!id,
  });

  const search = useQuery({
    queryKey: ["library", "search-tracks", addQuery],
    queryFn: () => librarySearchTracks(addQuery, {}),
    enabled: addQuery.trim().length > 0,
  });

  const detail = q.data ?? null;
  const playlist = detail?.playlist;
  const entries = detail?.entries ?? [];
  const isOwnerOrManager =
    !!session &&
    (playlist?.owner_id === session.user_id ||
      tier === "manager" ||
      tier === "admin");
  // Local-id playlists are always editable (they're the user's own offline
  // draft) and never server-deletable until synced.
  const canEdit = isOwnerOrManager || !!playlist?.local;

  function refresh() {
    return qc.invalidateQueries({ queryKey: ["playlists", "detail", id] });
  }

  async function guard<T>(fn: () => Promise<T>): Promise<T | null> {
    setErr(null);
    try {
      return await fn();
    } catch (e) {
      setErr(formatError(e));
      return null;
    }
  }

  async function startRename() {
    if (!playlist) return;
    setRenameVal(playlist.name);
    setRenaming(true);
  }

  async function commitRename() {
    const trimmed = renameVal.trim();
    if (!playlist || !trimmed || trimmed === playlist.name) {
      setRenaming(false);
      return;
    }
    setBusy(true);
    await guard(async () => {
      await playlistRename(playlist.id, trimmed);
      await Promise.all([
        refresh(),
        qc.invalidateQueries({ queryKey: ["playlists", "mine"] }),
      ]);
    });
    setBusy(false);
    setRenaming(false);
  }

  async function remove() {
    if (!playlist) return;
    if (!confirm(`Delete playlist "${playlist.name}"?`)) return;
    setBusy(true);
    const ok = await guard(async () => {
      await playlistDelete(playlist.id);
    });
    setBusy(false);
    if (ok !== null) {
      await qc.invalidateQueries({ queryKey: ["playlists", "mine"] });
      // Navigate back to the list via a full reload of the list query; the
      // link below is the cheap way "back".
      window.history.back();
    }
  }

  async function addTrack(trackId: string) {
    if (!playlist) return;
    setBusy(true);
    await guard(async () => {
      await playlistAddTrack(playlist.id, trackId, 0); // append
      await refresh();
    });
    setBusy(false);
    setAddQuery("");
  }

  async function removeAt(position: number) {
    if (!playlist) return;
    setBusy(true);
    await guard(async () => {
      await playlistRemoveTrack(playlist.id, position);
      await refresh();
    });
    setBusy(false);
  }

  async function move(from: number, to: number) {
    if (!playlist || from === to) return;
    setBusy(true);
    await guard(async () => {
      await playlistReorderTrack(playlist.id, from, to);
      await refresh();
    });
    setBusy(false);
  }

  async function playAll() {
    if (entries.length === 0) return;
    playQueue(
      entries.map((e) => e.track),
      0,
    );
  }

  async function dlPlaylist() {
    if (!playlist) return;
    setBusy(true);
    await guard(async () => {
      await downloadPlaylist(playlist.id);
      await Promise.all([refresh(), refreshStorage()]);
    });
    setBusy(false);
  }

  const playableCount = useMemo(
    () => entries.filter((e) => e.track.downloaded || online).length,
    [entries, online],
  );

  if (q.isLoading) return <p className="text-sm text-neutral-400">Loading…</p>;
  if (q.isError)
    return (
      <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
        {formatError(q.error)}
      </p>
    );
  if (!detail || !playlist)
    return (
      <section className="flex flex-col gap-3">
        <Link to="/playlists" className="text-sm text-blue-400 hover:underline">
          ← Playlists
        </Link>
        <p className="text-sm text-neutral-500">Playlist not found.</p>
      </section>
    );

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div className="min-w-0">
          <Link to="/playlists" className="text-sm text-blue-400 hover:underline">
            ← Playlists
          </Link>
          {renaming ? (
            <div className="flex items-center gap-2">
              <input
                value={renameVal}
                onChange={(e) => setRenameVal(e.target.value)}
                maxLength={200}
                autoFocus
                className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-2xl font-semibold focus:border-blue-500 focus:outline-none"
              />
              <button
                onClick={commitRename}
                disabled={busy}
                className="rounded bg-blue-600 px-2 py-1 text-sm text-white hover:bg-blue-500 disabled:opacity-50"
              >
                Save
              </button>
              <button
                onClick={() => setRenaming(false)}
                className="rounded border border-neutral-700 px-2 py-1 text-sm hover:bg-neutral-800"
              >
                Cancel
              </button>
            </div>
          ) : (
            <h1 className="flex items-center gap-2 text-2xl font-semibold">
              <span className="truncate">{playlist.name}</span>
              {playlist.local && (
                <span
                  className="rounded bg-amber-900/40 px-1.5 py-0.5 text-xs text-amber-200"
                  title="Created offline; waiting to sync"
                >
                  unsynced
                </span>
              )}
            </h1>
          )}
          <p className="text-xs text-neutral-500">
            {entries.length} track{entries.length === 1 ? "" : "s"} ·{" "}
            {playableCount} playable {online ? "" : "offline"}
          </p>
        </div>
        {detail && <SourceBadge source={detail.source} />}
      </header>

      {canEdit && (
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={playAll}
            disabled={entries.length === 0}
            className="rounded bg-blue-600 px-3 py-1.5 text-sm text-white hover:bg-blue-500 disabled:opacity-50"
          >
            ▶ Play
          </button>
          <button
            onClick={dlPlaylist}
            disabled={busy || entries.length === 0}
            className="rounded border border-neutral-700 px-3 py-1.5 text-sm hover:bg-neutral-800 disabled:opacity-50"
            title="Download every track for offline"
          >
            ⬇ Download all
          </button>
          {!renaming && (
            <button
              onClick={startRename}
              className="rounded border border-neutral-700 px-3 py-1.5 text-sm hover:bg-neutral-800"
            >
              Rename
            </button>
          )}
          <button
            onClick={remove}
            disabled={busy}
            className="rounded border border-red-800 px-3 py-1.5 text-sm text-red-300 hover:bg-red-900/30 disabled:opacity-50"
          >
            Delete
          </button>
        </div>
      )}

      {err && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {err}
        </p>
      )}

      {canEdit && (
        <div className="rounded border border-neutral-800 p-2">
          <input
            value={addQuery}
            onChange={(e) => setAddQuery(e.target.value)}
            placeholder="Search tracks to add…"
            className="w-full rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm focus:border-blue-500 focus:outline-none"
          />
          {search.data && search.data.items.length > 0 && (
            <ul className="mt-1 max-h-48 divide-y divide-neutral-800 overflow-auto">
              {search.data.items.slice(0, 20).map((t) => (
                <li
                  key={t.id}
                  className="flex items-center gap-2 p-1.5 text-sm hover:bg-neutral-800/50"
                >
                  <DownloadedDot downloaded={t.downloaded} />
                  <span className="flex-1 truncate">{t.title}</span>
                  <button
                    onClick={() => addTrack(t.id)}
                    disabled={busy}
                    className="rounded border border-neutral-700 px-1.5 py-0.5 text-xs hover:bg-neutral-800 disabled:opacity-50"
                  >
                    add
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <ol className="divide-y divide-neutral-800 rounded border border-neutral-800">
        {entries.length === 0 ? (
          <li className="p-3 text-sm text-neutral-500">No tracks yet.</li>
        ) : (
          entries.map((e, i) => (
            <PlaylistEntryRow
              key={`${e.position}-${e.track.id}`}
              entry={e}
              index={i}
              total={entries.length}
              canEdit={canEdit}
              busy={busy}
              online={online}
              onPlay={() => playQueue(entries.map((x) => x.track), i)}
              onRemove={() => removeAt(e.position)}
              onUp={() => move(e.position, e.position - 1)}
              onDown={() => move(e.position, e.position + 1)}
            />
          ))
        )}
      </ol>
    </section>
  );
}

function PlaylistEntryRow({
  entry,
  index,
  total,
  canEdit,
  busy,
  online,
  onPlay,
  onRemove,
  onUp,
  onDown,
}: {
  entry: MergedPlaylistEntry;
  index: number;
  total: number;
  canEdit: boolean;
  busy: boolean;
  online: boolean;
  onPlay: () => void;
  onRemove: () => void;
  onUp: () => void;
  onDown: () => void;
}) {
  const t = entry.track;
  const unavailable = !t.downloaded && !online;
  return (
    <li
      className="flex cursor-pointer items-center gap-3 p-3 text-sm hover:bg-neutral-800/50"
      onClick={onPlay}
    >
      <span className="w-6 text-right text-neutral-500">{entry.position}</span>
      <DownloadedDot downloaded={t.downloaded} />
      <span className="flex-1 truncate">
        {t.title || (
          <span className="italic text-neutral-500">
            {unavailable ? "not available offline" : "(unknown track)"}
          </span>
        )}
      </span>
      <span className="w-12 text-right tabular-nums text-neutral-500">
        {t.duration_ms > 0 ? formatDuration(t.duration_ms) : ""}
      </span>
      {canEdit && (
        <span
          className="flex gap-0.5"
          onClick={(e) => e.stopPropagation()}
        >
          <button
            onClick={onUp}
            disabled={busy || index === 0}
            className="rounded border border-neutral-700 px-1.5 py-0.5 text-xs hover:bg-neutral-800 disabled:opacity-30"
            title="Move up"
          >
            ↑
          </button>
          <button
            onClick={onDown}
            disabled={busy || index === total - 1}
            className="rounded border border-neutral-700 px-1.5 py-0.5 text-xs hover:bg-neutral-800 disabled:opacity-30"
            title="Move down"
          >
            ↓
          </button>
          <button
            onClick={onRemove}
            disabled={busy}
            className="rounded border border-neutral-700 px-1.5 py-0.5 text-xs hover:bg-neutral-800 disabled:opacity-50"
            title="Remove from playlist"
          >
            ✕
          </button>
        </span>
      )}
    </li>
  );
}
