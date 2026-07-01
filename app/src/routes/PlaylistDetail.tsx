import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import {
  discoverPlaylistRecommendations,
  discoverRadio,
  downloadDelete,
  downloadPlaylist,
  downloadTrack,
  librarySearchTracks,
  playlistAddTrack,
  playlistDelete,
  playlistGet,
  playlistRemoveTrack,
  playlistRename,
  playlistReorderTrack,
  type FavoriteTrack,
  type MergedPlaylistEntry,
  type MergedTrack,
  type PlaylistDetailView,
} from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { DownloadStatus } from "../components/DownloadStatus";
import { ActionSheet, SheetItem } from "../components/ActionSheet";
import { AddToPlaylistSheet } from "../components/AddToPlaylistSheet";
import { EqBars } from "../components/EqBars";
import {
  DownloadIcon,
  PlaylistIcon,
  PlayIcon,
  PlusIcon,
  RadioIcon,
  ShuffleIcon,
  SyncIcon,
} from "../components/icons";
import { formatDuration } from "../lib/format";
import { formatError } from "../lib/error";
import { gradientFor } from "../lib/visual";
import { trackMetaLine } from "../lib/trackMeta";
import { useTrackNames } from "../lib/useTrackNames";
import { serverTrackToQueueItem, usePlayerStore } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { btnDanger, btnGhost, btnGhostSm, btnPrimary, card, errorBox, input } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { SkeletonHero, SkeletonTracks } from "../components/Skeleton";

/**
 * /playlists/:id — one playlist with its ordered entries. Reorder via ↑/↓.
 * Add tracks via an inline library search. Remove / rename / delete are one
 * click each. The whole list is queueable; offline + uncached entries surface
 * a stub the player resolves (and fails gracefully) via `media://`.
 */
export default function PlaylistDetail() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const online = useAppStore((s) => s.online);
  const session = useAppStore((s) => s.session);
  const tier = useAppStore((s) => s.tier);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const queue = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const currentId = currentIndex >= 0 ? queue[currentIndex]?.id : undefined;
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);
  // Playlist batch download in flight → mark not-yet-started rows "pending".
  const dlBatch = useDownloadsStore((s) => s.active[id]);
  const batchActive = !!dlBatch && !dlBatch.done;
  const clearDownload = useDownloadsStore((s) => s.clear);

  const [renaming, setRenaming] = useState(false);
  const [renameVal, setRenameVal] = useState("");
  const [addQuery, setAddQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Mobile long-press action sheet for a playlist entry + the add-to-playlist
  // picker it can open.
  const [sheetEntry, setSheetEntry] = useState<MergedPlaylistEntry | null>(null);
  const [addToPlaylistTrack, setAddToPlaylistTrack] = useState<{ id: string; title: string } | null>(null);

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
    (playlist?.owner_id === session.user_id || tier === "manager" || tier === "admin");
  const canEdit = isOwnerOrManager || !!playlist?.local;

  function refresh() {
    broadcastInvalidate(["playlists", "detail", id]);
    return qc.invalidateQueries({ queryKey: ["playlists", "detail", id] });
  }
  // Add/remove/reorder return the already-refreshed detail view, so write it
  // straight into the cache — the list updates instantly with no refetch round
  // trip. Other windows still get nudged to re-read.
  function applyDetail(view: PlaylistDetailView) {
    qc.setQueryData(["playlists", "detail", id], view);
    broadcastInvalidate(["playlists", "detail", id]);
  }
  async function guard<T>(fn: () => Promise<T>): Promise<T | null> {
    setErr(null);
    try { return await fn(); } catch (e) { setErr(formatError(e)); return null; }
  }
  async function startRename() {
    if (!playlist) return;
    setRenameVal(playlist.name);
    setRenaming(true);
  }
  async function commitRename() {
    const trimmed = renameVal.trim();
    if (!playlist || !trimmed || trimmed === playlist.name) { setRenaming(false); return; }
    setBusy(true);
    await guard(async () => {
      await playlistRename(playlist.id, trimmed);
      broadcastInvalidate(["playlists", "mine"]);
      await Promise.all([refresh(), qc.invalidateQueries({ queryKey: ["playlists", "mine"] })]);
    });
    setBusy(false);
    setRenaming(false);
  }
  async function remove() {
    if (!playlist) return;
    if (!confirm(`Delete playlist "${playlist.name}"?`)) return;
    setBusy(true);
    const ok = await guard(async () => { await playlistDelete(playlist.id); });
    setBusy(false);
    if (ok !== null) {
      broadcastInvalidate(["playlists", "mine"]);
      await qc.invalidateQueries({ queryKey: ["playlists", "mine"] });
      window.history.back();
    }
  }
  async function addTrack(track: MergedTrack | FavoriteTrack) {
    if (!playlist) return;
    // Optimistically append the row so it shows the instant you click — then
    // reconcile with the authoritative view (real positions + metadata) when
    // the call returns, or roll back by refetching if it failed.
    const entryTrack: MergedTrack = {
      ...track,
      local_file_path: "local_file_path" in track ? track.local_file_path : null,
      downloaded: "downloaded" in track ? track.downloaded : false,
    } as MergedTrack;
    qc.setQueryData<PlaylistDetailView | null>(["playlists", "detail", id], (prev) =>
      prev
        ? {
            ...prev,
            entries: [
              ...prev.entries,
              { position: prev.entries.length + 1, added_at: new Date().toISOString(), track: entryTrack },
            ],
          }
        : prev,
    );
    setBusy(true);
    const view = await guard(() => playlistAddTrack(playlist.id, track.id, 0));
    if (view) applyDetail(view);
    else await refresh();
    setBusy(false);
    setAddQuery("");
  }
  async function removeAt(position: number) {
    if (!playlist) return;
    setBusy(true);
    await guard(async () => { applyDetail(await playlistRemoveTrack(playlist.id, position)); });
    setBusy(false);
  }
  async function move(from: number, to: number) {
    if (!playlist || from === to) return;
    setBusy(true);
    await guard(async () => { applyDetail(await playlistReorderTrack(playlist.id, from, to)); });
    setBusy(false);
  }
  function playAll(shuffle = false) {
    if (entries.length === 0) return;
    const st = usePlayerStore.getState();
    if (shuffle && !st.shuffle) st.toggleShuffle();
    playQueue(entries.map((e) => e.track), 0);
  }
  async function dlPlaylist() {
    if (!playlist) return;
    setBusy(true);
    await guard(async () => {
      await downloadPlaylist(playlist.id);
      broadcastInvalidate(["library"]);
      await Promise.all([refresh(), refreshStorage()]);
    });
    setBusy(false);
  }
  async function dlTrack(trackId: string) {
    setBusy(true);
    await guard(async () => {
      await downloadTrack(trackId);
      await Promise.all([refresh(), refreshStorage()]);
    });
    setBusy(false);
  }
  async function rmTrackDownload(trackId: string) {
    setBusy(true);
    await guard(async () => {
      await downloadDelete(trackId);
      clearDownload(trackId);
      await Promise.all([refresh(), refreshStorage()]);
    });
    setBusy(false);
  }

  const playableCount = useMemo(
    () => entries.filter((e) => e.track.downloaded || online).length,
    [entries, online],
  );

  // ----- Recommended songs (Spotify-style; Phase 12 acoustic recs) -------
  // A pool (~30) of suggestions similar to the *whole* playlist. We show up to
  // 10; adding one drops it from the pool so the next slides in (no refetch).
  // "Refresh" recomputes from whatever is *now* in the playlist.
  const [recs, setRecs] = useState<FavoriteTrack[] | null>(null);
  const [recsLoading, setRecsLoading] = useState(false);
  const entryIds = useMemo(() => new Set(entries.map((e) => e.track.id)), [entries]);
  const recVisible = useMemo(
    () => (recs ?? []).filter((r) => !entryIds.has(r.id)).slice(0, 10),
    [recs, entryIds],
  );

  // Batch-resolve artist/album names for the inline track listings. These are
  // hooks (useQueries internally) so they MUST run unconditionally, before any
  // early return. `?? []`-guarded so they're safe before data loads. Resolving
  // the whole entry list *once* here (and passing the getter to each row) keeps
  // it to a single batched `useQueries` — a per-row hook would otherwise spin up
  // one observer set per track and flood the page with re-renders on load.
  const entryNames = useTrackNames(entries.map((e) => e.track));
  const recNames = useTrackNames(recVisible);
  const addNames = useTrackNames(search.data?.items?.slice(0, 20) ?? []);

  async function loadRecs() {
    const seedIds = entries.map((e) => e.track.id);
    if (seedIds.length === 0) { setRecs([]); return; }
    setRecsLoading(true);
    const pool = await guard(() => discoverPlaylistRecommendations(seedIds, 30));
    setRecs(pool ?? []);
    setRecsLoading(false);
  }
  // Load once the playlist has tracks (re-runs only if the pool is reset to null).
  useEffect(() => {
    if (recs === null && online && canEdit && entries.length > 0) void loadRecs();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recs, online, canEdit, entries.length]);

  async function addRec(t: FavoriteTrack) {
    // Optimistically drop it from the pool so the next recommendation slides in;
    // `addTrack` optimistically appends it to the playlist (it's then a future seed).
    setRecs((prev) => (prev ? prev.filter((r) => r.id !== t.id) : prev));
    await addTrack(t);
  }

  if (q.isLoading)
    return (
      <section className="flex flex-col gap-6 p-6 md:p-8">
        <Link to="/playlists" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">
          ← PLAYLISTS
        </Link>
        <SkeletonHero />
        <SkeletonTracks rows={9} cols={3} />
      </section>
    );
  if (q.isError)
    return <p className="m-6 rounded-lg border border-oct-offline/50 bg-oct-offline/10 p-3 text-sm text-oct-danger md:m-8">{formatError(q.error)}</p>;
  if (!detail || !playlist)
    return (
      <section className="flex flex-col gap-3 p-6 md:p-8">
        <Link to="/playlists" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">← PLAYLISTS</Link>
        <p className="text-sm text-oct-subtle">Playlist not found.</p>
      </section>
    );

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <Link to="/playlists" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">← PLAYLISTS</Link>

      {/* hero */}
      <header className="flex flex-col gap-5 sm:flex-row sm:items-end">
        <div
          className="grid aspect-square w-[120px] shrink-0 place-items-center rounded-xl shadow-[0_10px_24px_-10px_rgba(0,0,0,0.6)]"
          style={{ background: gradientFor(playlist.id) }}
        >
          <PlaylistIcon size={36} className="text-white/85" />
        </div>
        <div className="flex min-w-0 flex-col">
          <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">PLAYLIST</span>
          {renaming ? (
            <div className="mt-1.5 flex flex-wrap items-center gap-2">
              <input
                value={renameVal}
                onChange={(e) => setRenameVal(e.target.value)}
                maxLength={200}
                autoFocus
                className={`${input} max-w-xs text-2xl font-semibold`}
              />
              <button onClick={commitRename} disabled={busy} className={btnPrimary}>Save</button>
              <button onClick={() => setRenaming(false)} className={btnGhost}>Cancel</button>
            </div>
          ) : (
            <h1 className="mt-1.5 flex items-center gap-2 text-3xl font-semibold tracking-tight sm:text-[34px]">
              <span className="truncate">{playlist.name}</span>
              {playlist.local && (
                <span className="rounded-md bg-oct-accent/15 px-1.5 py-0.5 font-mono text-[10px] text-oct-accent-bright" title="Created offline; waiting to sync">
                  UNSYNCED
                </span>
              )}
            </h1>
          )}
          <p className="mt-2 flex flex-wrap items-center gap-x-2 text-[13px] text-oct-subtle">
            <span className="font-mono">
              {entries.length} track{entries.length === 1 ? "" : "s"} · {playableCount} playable {online ? "" : "offline"}
            </span>
            {detail && <SourceBadge source={detail.source} />}
          </p>
        </div>
      </header>

      {/* actions */}
      {canEdit && (
        <div className="flex flex-wrap items-center gap-3">
          <button onClick={() => playAll(false)} disabled={entries.length === 0} className={btnPrimary}>
            <PlayIcon size={13} /> Play
          </button>
          <button onClick={() => playAll(true)} disabled={entries.length === 0} className={btnGhost}>
            <ShuffleIcon size={14} /> Shuffle
          </button>
          <button onClick={dlPlaylist} {...offlineAttrs(online, busy || entries.length === 0, "Download every track for offline")} className={btnGhost}>
            <DownloadIcon size={14} /> Download all
          </button>
          {!renaming && <button onClick={startRename} className={btnGhost}>Rename</button>}
          <button onClick={remove} disabled={busy} className={`${btnDanger} sm:ml-auto`}>Delete</button>
        </div>
      )}

      {err && <p className={errorBox}>{err}</p>}

      {/* add-track search */}
      {canEdit && (
        <div className={`${card} p-2.5`}>
          <input
            value={addQuery}
            onChange={(e) => setAddQuery(e.target.value)}
            placeholder="Search tracks to add…"
            className={input}
          />
          {search.data && search.data.items.length > 0 && (
            <ul className="oct-scroll mt-2 max-h-52 divide-y divide-oct-border overflow-auto">
              {search.data.items.slice(0, 20).map((t) => {
                const m = addNames(t);
                const sub = trackMetaLine(m.artistName, m.albumTitle);
                return (
                  <li key={t.id} className="flex items-center gap-2 px-1.5 py-2 text-sm hover:bg-oct-elevated/50">
                    <DownloadedDot downloaded={t.downloaded} />
                    <span className="flex min-w-0 flex-1 flex-col">
                      <span className="truncate">{t.title}</span>
                      {sub && <span className="mt-0.5 truncate text-[11px] text-oct-subtle">{sub}</span>}
                    </span>
                    <button onClick={() => addTrack(t)} disabled={busy} className={btnGhostSm}>
                      <PlusIcon size={12} /> add
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}

      {/* track table */}
      <div className="flex flex-col">
        {entries.length === 0 ? (
          <p className="text-sm text-oct-subtle">No tracks yet.</p>
        ) : (
          <>
            <div className="grid grid-cols-[28px_1fr_56px] items-center gap-x-4 border-b border-oct-border px-2 pb-2.5 font-mono text-[10.5px] tracking-[0.1em] text-oct-faint">
              <span>#</span>
              <span>TITLE</span>
              <span className="text-right">TIME</span>
            </div>
            {entries.map((e, i) => (
              <PlaylistEntryRow
                key={`${e.position}-${e.track.id}`}
                entry={e}
                index={i}
                total={entries.length}
                names={entryNames}
                canEdit={canEdit}
                busy={busy}
                online={online}
                active={e.track.id === currentId}
                playing={isPlaying}
                batchActive={batchActive}
                onLongPress={() => setSheetEntry(e)}
                onPlay={() => playQueue(entries.map((x) => x.track), i)}
                onRemove={() => removeAt(e.position)}
                onUp={() => move(e.position, e.position - 1)}
                onDown={() => move(e.position, e.position + 1)}
              />
            ))}
          </>
        )}
      </div>

      {/* Recommended songs — similar to the whole playlist; preview or add.
          Each add is replaced from the pool; Refresh recomputes from the
          current playlist (so it improves as the playlist grows). */}
      {canEdit && online && entries.length > 0 && (
        <div className="flex flex-col gap-3">
          <div className="flex items-center justify-between">
            <h2 className="flex items-center gap-2 font-mono text-[11px] tracking-[0.14em] text-oct-faint">
              <RadioIcon size={13} /> RECOMMENDED
            </h2>
            <button
              onClick={() => void loadRecs()}
              disabled={recsLoading}
              className="flex items-center gap-1.5 font-mono text-[11px] text-oct-subtle hover:text-oct-text disabled:opacity-40"
              title="Recalculate from the current playlist"
            >
              <SyncIcon size={13} className={recsLoading ? "animate-octspin" : ""} /> Refresh
            </button>
          </div>
          {recs === null && recsLoading ? (
            <SkeletonTracks rows={4} cols={3} />
          ) : recVisible.length === 0 ? (
            <p className="text-[13px] text-oct-subtle">
              No recommendations right now — add more songs to improve them.
            </p>
          ) : (
            <div className="flex flex-col divide-y divide-oct-border/60">
              {recVisible.map((t, i) => {
                const active = t.id === currentId;
                const m = recNames(t);
                const sub = trackMetaLine(m.artistName, m.albumTitle);
                return (
                  <div
                    key={t.id}
                    className="group grid grid-cols-[1fr_auto_auto] items-center gap-3 rounded-lg px-2 py-2 hover:bg-oct-elevated/40"
                  >
                    <button
                      onClick={() => playQueue(recVisible.map(serverTrackToQueueItem), i)}
                      className="flex min-w-0 items-center gap-3 text-left"
                      title={`Preview "${t.title}"`}
                    >
                      <span
                        className="grid h-9 w-9 shrink-0 place-items-center rounded-md"
                        style={{ background: gradientFor(t.album_id) }}
                      >
                        {active && isPlaying ? (
                          <EqBars playing />
                        ) : (
                          <PlayIcon size={12} className="text-white/85 opacity-70 group-hover:opacity-100" />
                        )}
                      </span>
                      <span className="flex min-w-0 flex-col">
                        <span className={`truncate text-[13.5px] ${active ? "text-oct-accent" : ""}`}>
                          {t.title}
                        </span>
                        {sub && <span className="mt-0.5 truncate text-[11px] text-oct-subtle">{sub}</span>}
                      </span>
                    </button>
                    <span className="font-mono text-[11px] text-oct-subtle">
                      {formatDuration(t.duration_ms)}
                    </span>
                    <button
                      onClick={() => void addRec(t)}
                      disabled={busy}
                      className={btnGhostSm}
                      title="Add to playlist"
                    >
                      <PlusIcon size={12} /> Add
                    </button>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      {sheetEntry && (
        <PlaylistEntryActionSheet
          entry={sheetEntry}
          total={entries.length}
          canEdit={canEdit}
          online={online}
          onClose={() => setSheetEntry(null)}
          onPlay={() => {
            const i = entries.findIndex((x) => x.position === sheetEntry.position);
            setSheetEntry(null);
            if (i >= 0) playQueue(entries.map((x) => x.track), i);
          }}
          onRadio={() => {
            const tid = sheetEntry.track.id;
            setSheetEntry(null);
            void (async () => {
              try {
                const tracks = await discoverRadio(undefined, undefined, tid);
                if (tracks.length > 0) playQueue(tracks.map(serverTrackToQueueItem), 0);
              } catch (e) {
                setErr(formatError(e));
              }
            })();
          }}
          onAddToPlaylist={() => {
            const tr = sheetEntry.track;
            setSheetEntry(null);
            setAddToPlaylistTrack({ id: tr.id, title: tr.title });
          }}
          onDownload={() => {
            const tid = sheetEntry.track.id;
            setSheetEntry(null);
            void dlTrack(tid);
          }}
          onRemoveDownload={() => {
            const tid = sheetEntry.track.id;
            setSheetEntry(null);
            void rmTrackDownload(tid);
          }}
          onMoveUp={() => {
            const p = sheetEntry.position;
            setSheetEntry(null);
            void move(p, p - 1);
          }}
          onMoveDown={() => {
            const p = sheetEntry.position;
            setSheetEntry(null);
            void move(p, p + 1);
          }}
          onRemove={() => {
            const p = sheetEntry.position;
            setSheetEntry(null);
            void removeAt(p);
          }}
        />
      )}

      {addToPlaylistTrack && (
        <AddToPlaylistSheet
          trackId={addToPlaylistTrack.id}
          trackTitle={addToPlaylistTrack.title}
          onClose={() => setAddToPlaylistTrack(null)}
        />
      )}
    </section>
  );
}

function PlaylistEntryRow({
  entry, index, total, names, canEdit, busy, online, active, playing, batchActive, onLongPress, onPlay, onRemove, onUp, onDown,
}: {
  entry: MergedPlaylistEntry;
  index: number;
  total: number;
  names: (t: MergedPlaylistEntry["track"]) => { artistName: string | null; albumTitle: string | null };
  canEdit: boolean;
  busy: boolean;
  online: boolean;
  active: boolean;
  playing: boolean;
  batchActive: boolean;
  onLongPress: () => void;
  onPlay: () => void;
  onRemove: () => void;
  onUp: () => void;
  onDown: () => void;
}) {
  const t = entry.track;
  const unavailable = !t.downloaded && !online;
  const meta = names(entry.track);
  const sub = trackMetaLine(meta.artistName, meta.albumTitle);
  const pressTimer = useRef<number | null>(null);
  const longPressed = useRef(false);
  function startPress() {
    longPressed.current = false;
    pressTimer.current = window.setTimeout(() => {
      longPressed.current = true;
      pressTimer.current = null;
      onLongPress();
      if (navigator.vibrate) navigator.vibrate(10);
    }, 450);
  }
  function cancelPress() {
    if (pressTimer.current !== null) {
      clearTimeout(pressTimer.current);
      pressTimer.current = null;
    }
  }
  return (
    <div
      onClick={() => {
        if (longPressed.current) {
          longPressed.current = false;
          return;
        }
        onPlay();
      }}
      onTouchStart={startPress}
      onTouchEnd={(e) => {
        cancelPress();
        if (longPressed.current) e.preventDefault();
      }}
      onTouchMove={cancelPress}
      onContextMenu={(e) => e.preventDefault()}
      className={`group grid cursor-pointer select-none grid-cols-[28px_1fr_56px] items-center gap-x-4 rounded-lg px-2 py-2.5 text-[13.5px] ${
        active ? "bg-oct-elevated" : "hover:bg-oct-elevated/50"
      }`}
    >
      <span className="flex justify-center">
        {active ? <EqBars playing={playing} /> : <span className="font-mono text-xs text-oct-faint">{entry.position}</span>}
      </span>
      <span className="flex min-w-0 items-center gap-2">
        <DownloadStatus trackId={t.id} downloaded={t.downloaded} pending={batchActive} streamDot />
        <span className="flex min-w-0 flex-col">
          <span className={`truncate ${active ? "font-medium text-oct-accent" : ""}`}>
            {t.title || <span className="italic text-oct-faint">{unavailable ? "not available offline" : "(unknown track)"}</span>}
          </span>
          {sub && <span className="truncate text-[12px] text-oct-subtle">{sub}</span>}
        </span>
      </span>
      <span className="flex items-center justify-end gap-2">
        {canEdit && (
          <span className="hidden items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100 sm:flex" onClick={(e) => e.stopPropagation()}>
            <button onClick={onUp} disabled={busy || index === 0} title="Move up" className="text-oct-dim hover:text-oct-text disabled:opacity-30">↑</button>
            <button onClick={onDown} disabled={busy || index === total - 1} title="Move down" className="text-oct-dim hover:text-oct-text disabled:opacity-30">↓</button>
            <button onClick={onRemove} disabled={busy} title="Remove" className="text-oct-dim hover:text-oct-danger disabled:opacity-40">✕</button>
          </span>
        )}
        <span className="w-9 text-right font-mono text-[11px] text-oct-subtle">
          {t.duration_ms > 0 ? formatDuration(t.duration_ms) : ""}
        </span>
      </span>
    </div>
  );
}

function PlaylistEntryActionSheet({
  entry,
  total,
  canEdit,
  online,
  onClose,
  onPlay,
  onRadio,
  onAddToPlaylist,
  onDownload,
  onRemoveDownload,
  onMoveUp,
  onMoveDown,
  onRemove,
}: {
  entry: MergedPlaylistEntry;
  total: number;
  canEdit: boolean;
  online: boolean;
  onClose: () => void;
  onPlay: () => void;
  onRadio: () => void;
  onAddToPlaylist: () => void;
  onDownload: () => void;
  onRemoveDownload: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onRemove: () => void;
}) {
  const t = entry.track;
  const isFirst = entry.position <= 1;
  const isLast = entry.position >= total;
  return (
    <ActionSheet
      title={t.title || "Track"}
      subtitle={t.duration_ms > 0 ? formatDuration(t.duration_ms) : undefined}
      onClose={onClose}
    >
      <SheetItem icon={<PlayIcon size={13} />} label="Play" onClick={onPlay} />
      <SheetItem icon={<RadioIcon size={15} />} label="Start radio" onClick={onRadio} disabled={!online} />
      <SheetItem icon={<PlaylistIcon size={16} />} label="Add to playlist…" onClick={onAddToPlaylist} />
      {t.downloaded ? (
        <SheetItem icon={<DownloadIcon size={16} />} label="Remove download" onClick={onRemoveDownload} />
      ) : (
        <SheetItem icon={<DownloadIcon size={16} />} label="Download" onClick={onDownload} disabled={!online} />
      )}
      {canEdit && (
        <>
          <div className="my-1.5 h-px bg-oct-border" />
          <SheetItem icon={<span className="text-[15px] leading-none">↑</span>} label="Move up" onClick={onMoveUp} disabled={isFirst} />
          <SheetItem icon={<span className="text-[15px] leading-none">↓</span>} label="Move down" onClick={onMoveDown} disabled={isLast} />
          <SheetItem icon={<span className="text-[13px] leading-none">✕</span>} label="Remove from playlist" onClick={onRemove} />
        </>
      )}
    </ActionSheet>
  );
}
