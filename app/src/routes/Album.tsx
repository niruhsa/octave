import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useNavigate } from "react-router-dom";
import {
  cacheGetAlbum,
  coverUrl,
  discoverRadio,
  downloadAlbum,
  downloadDelete,
  downloadTrack,
  libraryDeleteAlbum,
  libraryDeleteTrack,
  libraryGetAlbum,
  libraryListTracksByAlbum,
  libraryMergeAlbums,
  libraryMoveTrack,
  librarySetAlbumType,
  librarySetTrackExplicit,
  librarySetTrackSingleRelease,
} from "../ipc";
import { Cover } from "../components/Cover";
import { SourceBadge } from "../components/SourceBadge";
import { DownloadStatus } from "../components/DownloadStatus";
import { AddToPlaylistSheet } from "../components/AddToPlaylistSheet";
import { ActionSheet, SheetItem } from "../components/ActionSheet";
import { Aliases } from "../components/Aliases";
import { AlbumFolderLocation } from "../components/AlbumFolderLocation";
import { EntityPicker } from "../components/EntityPicker";
import { EqBars } from "../components/EqBars";
import { byteSize, formatDuration } from "../lib/format";
import { qualityLabel } from "../lib/visual";
import { useTrackNames } from "../lib/useTrackNames";
import { formatError } from "../lib/error";
import { serverTrackToQueueItem, usePlayerStore } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import { broadcastInvalidate } from "../App";
import { useAppStore } from "../store";
import { btnDanger, btnGhost, btnPrimary, errorBox } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { SkeletonHero, SkeletonTracks } from "../components/Skeleton";
import {
  DiscIcon,
  DownloadIcon,
  EditIcon,
  InfoIcon,
  PlayIcon,
  PlaylistIcon,
  RadioIcon,
  ShuffleIcon,
  TrashIcon,
} from "../components/icons";
import { EditMetaButton, MetadataEditor } from "../components/MetadataEditor";
import { SoundsLikeShelf } from "../components/SoundsLikeShelf";
import { FavoriteButton } from "../components/FavoriteButton";
import { ImageUploader } from "../components/ImageUploader";
import { TrackInfoSheet } from "../components/TrackInfoSheet";
import type { AlbumType, MergedTrack } from "../ipc";

const ALBUM_TYPES: { value: AlbumType; label: string }[] = [
  { value: "album", label: "Album" },
  { value: "ep", label: "EP" },
  { value: "single", label: "Single" },
  { value: "live", label: "Live" },
];

function totalLabel(ms: number): string {
  const min = Math.round(ms / 60000);
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} hr ${min % 60} min`;
}

type DiscGroup = {
  disc: number;
  tracks: MergedTrack[];
  /** Offset of this disc's first track inside the flattened play queue, so a
      per-disc "play" button can start the whole-album queue at this disc. */
  startIndex: number;
};

// Split an album's flat track list into per-disc groups. Multi-disc releases
// (deluxe / limited / "Type-A" bonus-live editions, box sets) then render each
// disc under its own labelled separator instead of as one undifferentiated run.
//
// Rules:
//   • A track's disc is `disc_no`; a null `disc_no` counts as disc 1 — ordinary
//     single-disc albums leave it unset, so this keeps them on one disc.
//   • Discs are ordered ascending by number.
//   • Within a disc, tracks are stably sorted by `track_no` (nulls sink to the
//     end, keeping their original order) so the visible order always matches
//     the printed numbers.
//   • `startIndex` is each disc's offset into the flattened queue (see
//     `ordered` below), used to seed playback from a given disc.
//
// The disc *header* itself is only drawn when there's more than one group — a
// lone "DISC 1" band on a normal album is noise — see `multiDisc` at the call
// site. Numbering stays per-disc: each disc shows its own `track_no` (metadata
// numbers already restart per disc), falling back to the 1-based position
// within the disc when `track_no` is missing.
function groupTracksByDisc(items: MergedTrack[]): DiscGroup[] {
  const byDisc = new Map<number, MergedTrack[]>();
  for (const t of items) {
    const d = t.disc_no ?? 1;
    const bucket = byDisc.get(d);
    if (bucket) bucket.push(t);
    else byDisc.set(d, [t]);
  }
  let startIndex = 0;
  return [...byDisc.keys()]
    .sort((a, b) => a - b)
    .map((disc) => {
      const tracks = byDisc
        .get(disc)!
        .map((t, i) => [t, i] as const)
        .sort(([a, ai], [b, bi]) => {
          if (a.track_no == null && b.track_no == null) return ai - bi;
          if (a.track_no == null) return 1;
          if (b.track_no == null) return -1;
          return a.track_no - b.track_no || ai - bi;
        })
        .map(([t]) => t);
      const group: DiscGroup = { disc, tracks, startIndex };
      startIndex += tracks.length;
      return group;
    });
}

export default function Album() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const isManager = tier === "admin" || tier === "manager";

  const q = useQuery({
    queryKey: ["library", "tracks-by-album", id],
    queryFn: () => libraryListTracksByAlbum(id),
    enabled: !!id,
  });
  // Best-effort album metadata (title) from the offline cache — present for
  // downloaded/cached albums; online-only albums fall back to "Album".
  const meta = useQuery({
    queryKey: ["cache", "album", id],
    queryFn: () => cacheGetAlbum(id),
    enabled: !!id,
  });
  // Server-backed album (canonical title + preserved-spelling aliases). Falls
  // back to the cache title when offline.
  const albumQ = useQuery({
    queryKey: ["library", "album", id],
    queryFn: () => libraryGetAlbum(id),
    enabled: !!id,
  });
  const album = albumQ.data;

  const playTrack = usePlayerStore((s) => s.playTrack);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const queue = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const currentId = currentIndex >= 0 ? queue[currentIndex]?.id : undefined;
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);
  const clearDownload = useDownloadsStore((s) => s.clear);
  // An album batch download in flight → mark this album's not-yet-started
  // tracks "pending" so every row shows an indicator from the start.
  const albumBatch = useDownloadsStore((s) => s.active[id]);
  const batchActive = !!albumBatch && !albumBatch.done;

  // Metadata editor (Manager+). `null` = closed; a non-empty list opens the
  // single (1) or batch (>1) editor.
  const [editTracks, setEditTracks] = useState<MergedTrack[] | null>(null);
  // Cover-art uploader (Manager+) + a cache-bust token bumped after upload.
  const [editCover, setEditCover] = useState(false);
  const [coverVersion, setCoverVersion] = useState(0);
  // Merge-album picker + per-track "move to album" picker (Manager+).
  const [mergingAlbum, setMergingAlbum] = useState(false);
  const [moveTrack, setMoveTrack] = useState<MergedTrack | null>(null);
  const [moveAsSingle, setMoveAsSingle] = useState(false);
  // "Choose the main single" picker (Manager+) — opened when switching an album
  // to `single` while no track is yet flagged. Holds the album's tracks.
  const [pickSingleFor, setPickSingleFor] = useState<MergedTrack[] | null>(null);
  // Mobile long-press action sheet — a touch device can't reach the hover-only
  // row actions, so a press-and-hold opens them in a bottom sheet instead.
  const [sheetTrack, setSheetTrack] = useState<MergedTrack | null>(null);
  const [infoTrack, setInfoTrack] = useState<MergedTrack | null>(null);
  // "Add to playlist" picker — long-press → "Add to playlist…" opens it for the
  // chosen track (so you never have to type the title into the playlist search).
  const [addToPlaylist, setAddToPlaylist] = useState<MergedTrack | null>(null);
  const pressTimer = useRef<number | null>(null);
  const longPressed = useRef(false);

  async function onMetaSaved() {
    await qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] });
    broadcastInvalidate(["library"]);
  }
  function refreshAlbum() {
    void qc.invalidateQueries({ queryKey: ["library"] });
    broadcastInvalidate(["library"]);
  }
  async function toggleSingle(track: MergedTrack) {
    try {
      await librarySetTrackSingleRelease(track.id, !track.is_single_release);
      await qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] });
      broadcastInvalidate(["library"]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function toggleExplicit(track: MergedTrack) {
    try {
      await librarySetTrackExplicit(track.id, !track.is_explicit);
      // Invalidate the album too — its explicit rollup recomputes server-side.
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        qc.invalidateQueries({ queryKey: ["library", "album", id] }),
      ]);
      broadcastInvalidate(["library"]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  // Persist a new album classification. Setting `single` requires a track
  // flagged as the album's single: reuse an existing one, else open a picker
  // (the chosen track is flagged server-side before the invariant is checked).
  async function applyAlbumType(type: AlbumType, singleTrackId?: string) {
    try {
      await librarySetAlbumType(id, type, singleTrackId);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "album", id] }),
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
      ]);
      broadcastInvalidate(["library"]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  function setAlbumType(type: AlbumType) {
    if (type === (album?.album_type ?? "album")) return;
    if (type === "single" && !items.some((t) => t.is_single_release)) {
      if (items.length === 0) {
        alert("Add a track before marking this album a single.");
        return;
      }
      setPickSingleFor(items); // choose the main single, then apply
      return;
    }
    void applyAlbumType(type);
  }
  function onCoverUploaded() {
    setCoverVersion(Date.now());
    void qc.invalidateQueries({ queryKey: ["cache", "album", id] });
    broadcastInvalidate(["library"]);
  }

  const items = q.data?.items ?? [];
  const trackNames = useTrackNames(items);
  const totalMs = items.reduce((s, t) => s + t.duration_ms, 0);
  // Disc-grouped view + the flattened order that grouping implies. `ordered` is
  // the canonical play queue (Play / Shuffle / row-click / per-disc play) so
  // the queue sequence always matches what's on screen, disc by disc.
  const discGroups = groupTracksByDisc(items);
  const ordered = discGroups.flatMap((g) => g.tracks);
  const multiDisc = discGroups.length > 1;
  const anyDownloaded = items.some((t) => t.downloaded);
  const title = album?.title ?? meta.data?.title ?? "Album";

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
      clearDownload(track.id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function delTrack(track: MergedTrack) {
    if (!window.confirm(`Permanently delete "${track.title}" from the server?`)) return;
    try {
      await libraryDeleteTrack(track.id);
      await qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] });
      broadcastInvalidate(["library"]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function delAlbum() {
    if (!window.confirm("Permanently delete this entire album from the server? All tracks will be removed.")) return;
    try {
      await libraryDeleteAlbum(id);
      navigate("/library");
    } catch (e) {
      alert(formatError(e));
    }
  }

  // Press-and-hold (touch) opens the per-track sheet; a quick tap still plays.
  // Any movement (a scroll) cancels the pending long-press.
  function startPress(t: MergedTrack) {
    longPressed.current = false;
    pressTimer.current = window.setTimeout(() => {
      longPressed.current = true;
      pressTimer.current = null;
      setSheetTrack(t);
      if (navigator.vibrate) navigator.vibrate(10);
    }, 450);
  }
  function cancelPress() {
    if (pressTimer.current !== null) {
      clearTimeout(pressTimer.current);
      pressTimer.current = null;
    }
  }

  // Start an acoustic "sounds like" radio seeded by a track (falls back to
  // behavioral artist radio server-side when the track has no embedding yet).
  async function startTrackRadio(t: MergedTrack) {
    try {
      const tracks = await discoverRadio(undefined, undefined, t.id);
      if (tracks.length > 0) playQueue(tracks.map(serverTrackToQueueItem), 0);
    } catch (e) {
      alert(formatError(e));
    }
  }

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <Link to="/library" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">
        ← LIBRARY
      </Link>

      {/* hero */}
      {q.isLoading ? (
        <SkeletonHero />
      ) : (
      <header className="flex flex-col gap-5 sm:flex-row sm:items-end">
        <div className="relative shrink-0" style={{ width: 150 }}>
          <Cover
            album={{ id, cover_path: meta.data ? "1" : null, local_cover_path: null }}
            tryCover
            size={150}
            radius={12}
            version={coverVersion || undefined}
            className="shadow-[0_10px_24px_-10px_rgba(0,0,0,0.6)]"
          />
          {isManager && (
            <button
              onClick={() => setEditCover(true)}
              {...offlineAttrs(online, false, "Edit cover art")}
              className="absolute bottom-1.5 right-1.5 grid h-7 w-7 place-items-center rounded-full bg-black/60 text-white/90 backdrop-blur-sm transition-colors hover:bg-black/80 disabled:opacity-40"
            >
              <EditIcon size={13} />
            </button>
          )}
        </div>
        <div className="flex min-w-0 flex-col">
          <span className="flex items-center gap-2">
            <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">
              {(ALBUM_TYPES.find((t) => t.value === (album?.album_type ?? "album"))?.label ?? "Album").toUpperCase()}
            </span>
            {album?.is_explicit && (
              <span
                className="rounded-sm bg-oct-subtle/25 px-1 py-px font-mono text-[9px] font-semibold tracking-wide text-oct-muted"
                title="Contains explicit content"
              >
                E
              </span>
            )}
          </span>
          <h1 className="mt-1.5 text-4xl font-semibold tracking-tight sm:text-[44px]">{title}</h1>
          <p className="mt-2 flex flex-wrap items-center gap-x-2 text-[13px] text-oct-subtle">
            <span className="font-mono">
              {items.length} song{items.length === 1 ? "" : "s"}
              {totalMs > 0 ? ` · ${totalLabel(totalMs)}` : ""}
              {album && album.storage_bytes > 0 ? ` · ${byteSize(album.storage_bytes)}` : ""}
            </span>
            {q.data && <SourceBadge source={q.data.source} />}
          </p>
        </div>
      </header>
      )}

      {/* Preserved title spellings + manager alias controls.
          Hidden for a single spelling unless a manager can add more. */}
      {((album?.aliases?.length ?? 0) > 1 || isManager) && (
        <Aliases
          kind="album"
          entityId={id}
          aliases={album?.aliases ?? []}
          online={online}
          isManager={isManager}
          onChanged={refreshAlbum}
        />
      )}

      {/* On-disk folder — rename to match the title or a custom name (Manager+). */}
      <AlbumFolderLocation
        albumId={id}
        online={online}
        isManager={isManager}
        onChanged={refreshAlbum}
      />

      {/* actions */}
      {items.length > 0 && (
        <div className="flex flex-wrap items-center gap-3">
          <button onClick={() => playQueue(ordered, 0)} className={btnPrimary}>
            <PlayIcon size={13} /> Play
          </button>
          <button
            onClick={() => {
              const st = usePlayerStore.getState();
              if (!st.shuffle) st.toggleShuffle();
              playQueue(ordered, 0);
            }}
            className={btnGhost}
          >
            <ShuffleIcon size={14} /> Shuffle
          </button>
          <button onClick={dlAlbum} className={btnGhost} {...offlineAttrs(online)}>
            <DownloadIcon size={14} /> Download
          </button>
          <FavoriteButton kind="album" id={id} size={18} />
          <button
            onClick={async () => {
              try {
                const tracks = await discoverRadio(undefined, id);
                if (tracks.length > 0) playQueue(tracks.map(serverTrackToQueueItem), 0);
              } catch (e) {
                alert(formatError(e));
              }
            }}
            className={btnGhost}
            {...offlineAttrs(online)}
          >
            <RadioIcon size={14} /> Radio
          </button>
          {anyDownloaded && (
            <Link to="/downloads" className="font-mono text-[11px] text-oct-accent hover:underline">
              manage downloads
            </Link>
          )}
          {isManager && (
            <div className="flex items-center gap-3 sm:ml-auto">
              <div
                className="flex overflow-hidden rounded-md border border-oct-border"
                role="group"
                aria-label="Album type"
                {...offlineAttrs(online, false, "Classify this album")}
              >
                {ALBUM_TYPES.map((t) => {
                  const activeType = (album?.album_type ?? "album") === t.value;
                  return (
                    <button
                      key={t.value}
                      onClick={() => setAlbumType(t.value)}
                      disabled={!online}
                      aria-pressed={activeType}
                      title={`Mark as ${t.label}`}
                      className={`px-2.5 py-1 font-mono text-[11px] transition-colors disabled:opacity-40 ${
                        activeType
                          ? "bg-oct-accent/15 text-oct-accent"
                          : "text-oct-subtle hover:bg-oct-elevated/60 hover:text-oct-text"
                      }`}
                    >
                      {t.label}
                    </button>
                  );
                })}
              </div>
              <button
                onClick={() => setEditTracks(items)}
                className={`${btnGhost} hidden sm:inline-flex`}
                {...offlineAttrs(online, false, "Edit metadata for all tracks")}
              >
                <EditIcon size={14} /> Edit tags
              </button>
              <button
                onClick={() => setMergingAlbum(true)}
                className={btnGhost}
                {...offlineAttrs(online, false, "Merge a duplicate album into this one")}
              >
                Merge album…
              </button>
              <button onClick={delAlbum} className={btnDanger} {...offlineAttrs(online)}>
                <TrashIcon size={14} /> Delete album
              </button>
            </div>
          )}
        </div>
      )}

      {q.isLoading && <SkeletonTracks rows={9} cols={4} />}
      {q.isError && <p className={errorBox}>{formatError(q.error)}</p>}

      {/* track table */}
      {q.data && (
        <div className="flex flex-col">
          {items.length === 0 ? (
            <p className="text-sm text-oct-subtle">No tracks.</p>
          ) : (
            <>
              <div className="grid grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-x-4 border-b border-oct-border px-2 pb-2.5 font-mono text-[10.5px] tracking-[0.1em] text-oct-faint sm:grid-cols-[28px_minmax(0,1fr)_110px_64px]">
                <span>#</span>
                <span>TITLE</span>
                <span className="hidden sm:block">QUALITY</span>
                <span className="text-right">TIME</span>
              </div>
              {discGroups.map((group) => {
                const discMs = group.tracks.reduce((s, t) => s + t.duration_ms, 0);
                return (
                  <div key={group.disc} className="flex flex-col">
                    {/* Disc separator — only for genuine multi-disc releases; a
                        single-disc album stays a plain run (see groupTracksByDisc). */}
                    {multiDisc && (
                      <div className="flex items-center gap-3 px-2 pb-2 pt-5">
                        <button
                          onClick={() => playQueue(ordered, group.startIndex)}
                          title={`Play disc ${group.disc}`}
                          className="oct-disc-play grid h-[26px] w-[26px] shrink-0 place-items-center rounded-full border border-oct-border-strong text-oct-subtle transition-colors"
                        >
                          <DiscIcon size={12} />
                        </button>
                        <span className="font-mono text-[11px] font-bold tracking-[0.18em] text-oct-muted">
                          DISC {group.disc}
                        </span>
                        <span className="h-px flex-1 bg-oct-border" />
                        <span className="font-mono text-[10.5px] text-oct-faint">
                          {group.tracks.length} song{group.tracks.length === 1 ? "" : "s"}
                          {discMs > 0 ? ` · ${totalLabel(discMs)}` : ""}
                        </span>
                      </div>
                    )}
                    {group.tracks.map((t, i) => {
                      const active = t.id === currentId;
                      const artistName = trackNames(t).artistName;
                      // Per-disc numbering: the printed `track_no` (metadata
                      // numbers already restart per disc), or the row's 1-based
                      // position within this disc when it's missing.
                      const trackNo = t.track_no ?? i + 1;
                      return (
                        <div
                          key={t.id}
                          onClick={() => {
                            if (longPressed.current) {
                              longPressed.current = false;
                              return;
                            }
                            playTrack(t, ordered);
                          }}
                          onTouchStart={() => startPress(t)}
                          onTouchEnd={(e) => {
                            cancelPress();
                            if (longPressed.current) e.preventDefault();
                          }}
                          onTouchMove={cancelPress}
                          onContextMenu={(e) => e.preventDefault()}
                          className={`group relative grid cursor-pointer select-none grid-cols-[28px_minmax(0,1fr)_auto] items-center gap-x-4 rounded-lg px-2 py-2.5 text-[13.5px] sm:grid-cols-[28px_minmax(0,1fr)_110px_64px] ${
                            active ? "bg-oct-elevated" : "hover:bg-oct-elevated/50"
                          }`}
                        >
                          <span className="relative flex h-5 items-center justify-center">
                            {active ? (
                              <EqBars playing={isPlaying} />
                            ) : (
                              <>
                                {/* number fades out to reveal a play triangle on hover */}
                                <span className="font-mono text-xs text-oct-faint transition-opacity group-hover:opacity-0">
                                  {trackNo}
                                </span>
                                <PlayIcon
                                  size={12}
                                  className="pointer-events-none absolute text-oct-text opacity-0 transition-opacity group-hover:opacity-100"
                                />
                              </>
                            )}
                          </span>
                          <span className="flex min-w-0 flex-col">
                            <span className="flex min-w-0 items-center gap-2">
                              <span className={`truncate ${active ? "font-medium text-oct-accent" : ""}`}>
                                {t.title}
                              </span>
                              {t.is_single_release && (
                                <span
                                  className="shrink-0 rounded-full border border-oct-accent/40 bg-oct-accent/10 px-1.5 py-px font-mono text-[9px] tracking-wide text-oct-accent"
                                  title="Single release within this album"
                                >
                                  SINGLE
                                </span>
                              )}
                              {t.is_explicit && (
                                <span
                                  className="shrink-0 rounded-sm bg-oct-subtle/25 px-1 py-px font-mono text-[9px] font-semibold tracking-wide text-oct-muted"
                                  title="Explicit content"
                                >
                                  E
                                </span>
                              )}
                              <DownloadStatus trackId={t.id} downloaded={t.downloaded} pending={batchActive} />
                            </span>
                            {artistName && (
                              <span className="truncate text-[12px] text-oct-subtle">{artistName}</span>
                            )}
                          </span>
                          <span className="hidden font-mono text-[11px] text-oct-subtle sm:block">
                            {qualityLabel(t)}
                          </span>
                          <span className="flex items-center justify-end gap-2.5">
                            <FavoriteButton kind="track" id={t.id} size={15} />
                            <span className="w-9 text-right font-mono text-[11px] text-oct-subtle">
                              {formatDuration(t.duration_ms)}
                            </span>
                          </span>
                          {/* floating action toolbar — lifted out of the narrow last
                              column so the icons have room and never crowd TIME */}
                          <span
                            className="pointer-events-none absolute right-[76px] top-1/2 z-10 hidden -translate-y-1/2 items-center gap-2 rounded-lg border border-oct-border bg-oct-elevated px-2.5 py-1.5 opacity-0 shadow-[0_8px_24px_-10px_rgba(0,0,0,0.7)] transition-opacity duration-150 group-hover:pointer-events-auto group-hover:opacity-100 sm:flex"
                            onClick={(e) => e.stopPropagation()}
                          >
                            <button
                              onClick={() => void startTrackRadio(t)}
                              {...offlineAttrs(online, false, "Start a radio that sounds like this track")}
                              className="text-oct-dim hover:text-oct-text disabled:opacity-30"
                            >
                              <RadioIcon size={15} />
                            </button>
                            {t.downloaded ? (
                              <button onClick={() => void removeTrack(t)} title="Remove download" className="text-oct-accent hover:text-oct-accent-bright">
                                <DownloadIcon size={15} />
                              </button>
                            ) : (
                              <button onClick={() => void dlTrack(t)} {...offlineAttrs(online, false, "Download")} className="text-oct-dim hover:text-oct-text disabled:opacity-30">
                                <DownloadIcon size={15} />
                              </button>
                            )}
                            <button onClick={() => setAddToPlaylist(t)} title="Add to playlist" className="text-oct-dim hover:text-oct-text">
                              <PlaylistIcon size={15} />
                            </button>
                            <button onClick={() => setInfoTrack(t)} title="Media information" className="text-oct-dim hover:text-oct-text">
                              <InfoIcon size={15} />
                            </button>
                            {isManager && (
                              <>
                                <EditMetaButton online={online} onClick={() => setEditTracks([t])} />
                                <button
                                  onClick={() => void toggleSingle(t)}
                                  {...offlineAttrs(online, false, t.is_single_release ? "Unmark single release" : "Mark as single release")}
                                  className={`text-[15px] leading-none disabled:opacity-30 ${t.is_single_release ? "text-oct-accent hover:text-oct-accent-bright" : "text-oct-dim hover:text-oct-text"}`}
                                >
                                  {t.is_single_release ? "★" : "☆"}
                                </button>
                                <button
                                  onClick={() => void toggleExplicit(t)}
                                  {...offlineAttrs(online, false, t.is_explicit ? "Unmark explicit" : "Mark as explicit")}
                                  className={`font-mono text-[11px] font-semibold leading-none disabled:opacity-30 ${t.is_explicit ? "text-oct-text" : "text-oct-dim hover:text-oct-text"}`}
                                >
                                  E
                                </button>
                                <button
                                  onClick={() => {
                                    setMoveAsSingle(t.is_single_release);
                                    setMoveTrack(t);
                                  }}
                                  {...offlineAttrs(online, false, "Move to another album")}
                                  className="text-oct-dim hover:text-oct-text disabled:opacity-30"
                                >
                                  <DiscIcon size={15} />
                                </button>
                                <button onClick={() => void delTrack(t)} {...offlineAttrs(online, false, "Delete from server")} className="text-oct-dim hover:text-oct-danger disabled:opacity-30">
                                  <TrashIcon size={15} />
                                </button>
                              </>
                            )}
                          </span>
                        </div>
                      );
                    })}
                  </div>
                );
              })}
            </>
          )}
        </div>
      )}

      {/* Acoustic "sounds like" shelf, seeded by the album's first track. Self-
          hides when there are no neighbors (fingerprinting off / not analyzed). */}
      {online && ordered.length > 0 && (
        <SoundsLikeShelf seedTrackId={ordered[0].id} title="Sounds like this album" />
      )}

      {editTracks && (
        <MetadataEditor
          tracks={editTracks}
          online={online}
          onClose={() => setEditTracks(null)}
          onSaved={() => void onMetaSaved()}
        />
      )}

      {editCover && (
        <ImageUploader
          kind="album"
          id={id}
          online={online}
          currentUrl={coverUrl(id, coverVersion || undefined)}
          onClose={() => setEditCover(false)}
          onUploaded={onCoverUploaded}
        />
      )}

      {mergingAlbum && (
        <EntityPicker
          kind="album"
          excludeId={id}
          title="Merge album"
          hint={`Pick a duplicate album to fold into "${title}". Its tracks move here and every title spelling is preserved.`}
          online={online}
          onPick={async (dupId) => {
            await libraryMergeAlbums(id, dupId);
            refreshAlbum();
          }}
          onClose={() => setMergingAlbum(false)}
        />
      )}

      {moveTrack && (
        <EntityPicker
          kind="album"
          excludeId={moveTrack.album_id}
          title="Move track to album"
          hint={`Move "${moveTrack.title}" into another album. Its source album is removed if it's left empty.`}
          online={online}
          extra={
            <label className="flex items-center gap-2 text-[12.5px] text-oct-muted">
              <input
                type="checkbox"
                checked={moveAsSingle}
                onChange={(e) => setMoveAsSingle(e.target.checked)}
                className="accent-oct-accent"
              />
              Mark as a single release in the destination album
            </label>
          }
          onPick={async (destId) => {
            const trackId = moveTrack.id;
            await libraryMoveTrack(trackId, destId, moveAsSingle);
            refreshAlbum();
            navigate(`/albums/${destId}`);
          }}
          onClose={() => setMoveTrack(null)}
        />
      )}

      {sheetTrack && (
        <TrackActionSheet
          track={sheetTrack}
          online={online}
          isManager={isManager}
          onClose={() => setSheetTrack(null)}
          actions={{
            play: () => { setSheetTrack(null); playTrack(sheetTrack, ordered); },
            radio: () => { const t = sheetTrack; setSheetTrack(null); void startTrackRadio(t); },
            download: () => { setSheetTrack(null); void dlTrack(sheetTrack); },
            removeDownload: () => { setSheetTrack(null); void removeTrack(sheetTrack); },
            edit: () => { setSheetTrack(null); setEditTracks([sheetTrack]); },
            move: () => { setSheetTrack(null); setMoveAsSingle(sheetTrack.is_single_release); setMoveTrack(sheetTrack); },
            toggleSingle: () => { setSheetTrack(null); void toggleSingle(sheetTrack); },
            toggleExplicit: () => { setSheetTrack(null); void toggleExplicit(sheetTrack); },
            del: () => { setSheetTrack(null); void delTrack(sheetTrack); },
            addToPlaylist: () => { setSheetTrack(null); setAddToPlaylist(sheetTrack); },
            info: () => { const t = sheetTrack; setSheetTrack(null); setInfoTrack(t); },
          }}
        />
      )}

      {infoTrack && <TrackInfoSheet track={infoTrack} onClose={() => setInfoTrack(null)} />}

      {addToPlaylist && (
        <AddToPlaylistSheet
          trackId={addToPlaylist.id}
          trackTitle={addToPlaylist.title}
          onClose={() => setAddToPlaylist(null)}
        />
      )}

      {pickSingleFor && (
        <ActionSheet
          title="Choose the single"
          subtitle="A single album needs one main single song"
          onClose={() => setPickSingleFor(null)}
        >
          {pickSingleFor.map((t) => (
            <SheetItem
              key={t.id}
              icon={<span className="text-oct-dim">☆</span>}
              label={t.title}
              onClick={() => {
                setPickSingleFor(null);
                void applyAlbumType("single", t.id);
              }}
            />
          ))}
        </ActionSheet>
      )}
    </section>
  );
}

type SheetActions = {
  play: () => void;
  radio: () => void;
  download: () => void;
  removeDownload: () => void;
  edit: () => void;
  toggleSingle: () => void;
  toggleExplicit: () => void;
  move: () => void;
  del: () => void;
  addToPlaylist: () => void;
  info: () => void;
};

/** Mobile press-and-hold action sheet for a single track. */
function TrackActionSheet({
  track,
  online,
  isManager,
  onClose,
  actions,
}: {
  track: MergedTrack;
  online: boolean;
  isManager: boolean;
  onClose: () => void;
  actions: SheetActions;
}) {
  return (
    <ActionSheet
      title={track.title}
      subtitle={`${qualityLabel(track)} · ${formatDuration(track.duration_ms)}`}
      onClose={onClose}
    >
      <SheetItem icon={<PlayIcon size={13} />} label="Play" onClick={actions.play} />
      <SheetItem
        icon={<RadioIcon size={15} />}
        label="Start radio"
        onClick={actions.radio}
        disabled={!online}
      />
      {track.downloaded ? (
        <SheetItem icon={<DownloadIcon size={16} />} label="Remove download" onClick={actions.removeDownload} />
      ) : (
        <SheetItem icon={<DownloadIcon size={16} />} label="Download" onClick={actions.download} disabled={!online} />
      )}
      <SheetItem icon={<PlaylistIcon size={16} />} label="Add to playlist…" onClick={actions.addToPlaylist} />
      <SheetItem icon={<InfoIcon size={16} />} label="Information" onClick={actions.info} />
      {isManager && (
        <>
          <div className="my-1.5 h-px bg-oct-border" />
          <SheetItem icon={<EditIcon size={16} />} label="Edit metadata" onClick={actions.edit} disabled={!online} />
          <SheetItem icon={<DiscIcon size={16} />} label="Move to album…" onClick={actions.move} disabled={!online} />
          <SheetItem
            icon={<span className="text-[14px] leading-none">{track.is_single_release ? "★" : "☆"}</span>}
            label={track.is_single_release ? "Unmark single release" : "Mark as single release"}
            onClick={actions.toggleSingle}
            disabled={!online}
          />
          <SheetItem
            icon={<span className="font-mono text-[13px] font-semibold leading-none">E</span>}
            label={track.is_explicit ? "Unmark explicit" : "Mark as explicit"}
            onClick={actions.toggleExplicit}
            disabled={!online}
          />
          <SheetItem icon={<TrashIcon size={16} />} label="Delete from server" onClick={actions.del} disabled={!online} danger />
        </>
      )}
    </ActionSheet>
  );
}
