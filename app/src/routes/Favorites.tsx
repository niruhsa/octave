// Favorites (Phase 11). Redesigned to the "OCTAVE Favorites" comp: a gradient
// hero with live counts, a Play-all / Shuffle row beside a segmented
// Tracks/Albums/Artists switch, and three richer views — a track table with
// cover thumbs + quality + the now-playing equalizer, and album/artist grids
// with a hover play affordance. Server-authoritative + online-only, like the
// notifications feed. The hero stats (count, runtime, size) come from the
// tracks query, which loads regardless of the active view.

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  cacheListDownloadedTracks,
  discoverRadio,
  favoritesListAlbums,
  favoritesListArtists,
  favoritesListTracks,
  libraryListTracksByAlbum,
  type FavoriteTrack,
} from "../ipc";
import { OfflineGate } from "../components/OfflineGate";
import { FavoriteButton } from "../components/FavoriteButton";
import { Cover, Thumb } from "../components/Cover";
import { ArtistAvatar } from "../components/ArtistAvatar";
import { EqBars } from "../components/EqBars";
import { Skeleton, SkeletonGrid } from "../components/Skeleton";
import { formatError } from "../lib/error";
import { byteSize, formatDuration } from "../lib/format";
import { isLossless, qualityLabel, sampleRateKHz } from "../lib/visual";
import { trackMetaLine } from "../lib/trackMeta";
import { useTrackNames } from "../lib/useTrackNames";
import { btnGhost, btnPrimary, errorBox } from "../lib/ui";
import { CheckIcon, ChevronDownIcon, CloudIcon, HeartIcon, PlayIcon, ShuffleIcon, SlidersIcon } from "../components/icons";
import { useAppStore } from "../store";
import { serverTrackToQueueItem, usePlayerStore } from "../player/store";

type Tab = "tracks" | "albums" | "artists";

/** Hero runtime readout: "1 hr 23 min" / "42 min". */
function runtimeLabel(ms: number): string {
  const min = Math.round(ms / 60000);
  if (min <= 0) return "0 min";
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} hr ${min % 60} min`;
}

/** Compact quality chip for the dense mobile track rows: "FLAC" / "320k" /
 * "96/24" — terser than the full `qualityLabel` used in the desktop table. */
function compactQuality(t: Pick<FavoriteTrack, "codec" | "bitrate_kbps" | "sample_rate_hz" | "bit_depth">): string {
  const codec = (t.codec || "").toUpperCase();
  if (isLossless(codec)) {
    const khz = sampleRateKHz(t.sample_rate_hz);
    if (t.bit_depth && khz && (t.bit_depth > 16 || Number(khz) > 48)) return `${khz}/${t.bit_depth}`;
    return codec || "Lossless";
  }
  return t.bitrate_kbps ? `${t.bitrate_kbps}k` : codec || "—";
}

export default function Favorites() {
  return (
    <OfflineGate feature="Favorites">
      <FavoritesInner />
    </OfflineGate>
  );
}

function FavoritesInner() {
  const [tab, setTab] = useState<Tab>("tracks");

  // Loaded regardless of the active view so the hero count/runtime/size stay
  // live and Play-all / Shuffle always work. The Tracks view re-reads the same
  // query key, so react-query dedupes it (no second fetch).
  const tracksQ = useQuery({ queryKey: ["favorites", "tracks"], queryFn: favoritesListTracks });
  const tracks = tracksQ.data ?? [];

  // Downloaded-track set — favorites carry no local/stream flag, so we
  // cross-reference the cache to show the hero's on-device vs streaming split
  // (and the per-row indicator on mobile). Cheap, cached, offline-safe.
  const downloadedQ = useQuery({ queryKey: ["cache", "downloaded_tracks"], queryFn: cacheListDownloadedTracks });
  const downloadedIds = useMemo(
    () => new Set((downloadedQ.data ?? []).map((t) => t.id)),
    [downloadedQ.data],
  );

  const playQueue = usePlayerStore((s) => s.playQueue);
  const startPlay = (shuffle: boolean) => {
    if (tracks.length === 0) return;
    const st = usePlayerStore.getState();
    if (st.shuffle !== shuffle) st.toggleShuffle();
    playQueue(tracks.map(serverTrackToQueueItem), 0);
  };

  const totalMs = tracks.reduce((s, t) => s + t.duration_ms, 0);
  const totalBytes = tracks.reduce((s, t) => s + (t.file_size ?? 0), 0);
  const localN = tracks.reduce((n, t) => n + (downloadedIds.has(t.id) ? 1 : 0), 0);
  const streamN = tracks.length - localN;

  return (
    <section className="flex flex-col">
      {/* hero */}
      <div className="bg-[linear-gradient(120deg,rgba(224,168,75,0.06),transparent_46%)] px-4 pb-5 pt-5 sm:px-6 sm:pb-6 sm:pt-8 md:px-8">
        <div className="font-mono text-[11px] tracking-[0.22em] text-oct-accent">FAVORITES</div>

        <div className="mt-4 flex items-end gap-4 sm:gap-6">
          <div
            className="grid h-[84px] w-[84px] shrink-0 place-items-center rounded-[15px] text-white/95 shadow-[0_10px_22px_-14px_rgba(224,168,75,0.45)] sm:h-32 sm:w-32 sm:rounded-2xl"
            style={{ background: "linear-gradient(145deg,#e0a84b 0%,#b9762f 55%,#7d3f5a 100%)" }}
          >
            <HeartIcon size={38} className="sm:hidden" />
            <HeartIcon size={54} className="hidden sm:block" />
          </div>
          <div className="min-w-0 flex-1 pb-1">
            <h1 className="text-[30px] font-semibold leading-none tracking-tight sm:text-[46px]">
              Favorites
            </h1>
            <div className="mt-2.5 flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-[11px] text-oct-subtle sm:mt-4 sm:text-[12px]">
              <span className="text-oct-muted">{tracks.length} tracks</span>
              {totalMs > 0 && (
                <>
                  <span className="text-oct-faint">·</span>
                  <span>{runtimeLabel(totalMs)}</span>
                </>
              )}
              {totalBytes > 0 && (
                <>
                  <span className="hidden text-oct-faint sm:inline">·</span>
                  <span className="hidden sm:inline">{byteSize(totalBytes)}</span>
                </>
              )}
            </div>
            {/* on-device vs streaming — mobile only (desktop shows size above) */}
            {tracks.length > 0 && (
              <div className="mt-2.5 flex flex-wrap items-center gap-x-3.5 gap-y-1 font-mono text-[10.5px] sm:hidden">
                <span className="inline-flex items-center gap-1.5 text-oct-online">
                  <span className="h-1.5 w-1.5 rounded-full bg-oct-online" />
                  {localN} on device
                </span>
                <span className="inline-flex items-center gap-1.5" style={{ color: "#6f9bd1" }}>
                  <CloudIcon size={11} />
                  {streamN} streaming
                </span>
              </div>
            )}
          </div>
        </div>

        {/* actions + segmented switch — full-width split on mobile, inline on desktop */}
        <div className="mt-5 flex flex-wrap items-center gap-2.5 sm:mt-6 sm:gap-3">
          <button
            onClick={() => startPlay(false)}
            disabled={tracks.length === 0}
            className={`${btnPrimary} flex-1 sm:flex-none`}
          >
            <PlayIcon size={13} /> Play all
          </button>
          <button
            onClick={() => startPlay(true)}
            disabled={tracks.length === 0}
            className={`${btnGhost} flex-1 sm:flex-none`}
          >
            <ShuffleIcon size={14} /> Shuffle
          </button>
          <div className="order-last flex w-full gap-1 rounded-full border border-oct-border-strong bg-oct-card p-1 sm:order-none sm:ml-auto sm:w-auto">
            {(["tracks", "albums", "artists"] as Tab[]).map((t) => (
              <button
                key={t}
                onClick={() => setTab(t)}
                className={`flex-1 rounded-full px-4 py-1.5 text-center text-[13px] capitalize transition-colors sm:flex-none ${
                  t === tab
                    ? "bg-oct-accent font-medium text-oct-bg"
                    : "text-oct-muted hover:text-oct-text"
                }`}
              >
                {t}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div className="px-4 pb-10 sm:px-6 md:px-8">
        {tab === "tracks" && <TracksTab q={tracksQ} tracks={tracks} downloadedIds={downloadedIds} />}
        {tab === "albums" && <AlbumsTab />}
        {tab === "artists" && <ArtistsTab />}
      </div>
    </section>
  );
}

function EmptyState({ label }: { label: string }) {
  return (
    <div className="mt-2 flex flex-col items-center gap-3 rounded-2xl border border-oct-border bg-oct-panel/40 px-6 py-16 text-center">
      <span className="grid h-12 w-12 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
        <HeartIcon size={22} />
      </span>
      <p className="text-sm text-oct-subtle">No favorite {label} yet.</p>
      <p className="max-w-xs text-[12.5px] leading-relaxed text-oct-faint">
        Tap the heart on a {label.replace(/s$/, "")} to add it here.
      </p>
    </div>
  );
}

// ── Tracks ─────────────────────────────────────────────────────────────────

type TrackSort = "added" | "title" | "duration";
const SORT_LABEL: Record<TrackSort, string> = {
  added: "recently added",
  title: "title",
  duration: "duration",
};
/** Options shown in the sort dropdown, in display order. */
const SORT_OPTIONS: { value: TrackSort; label: string }[] = [
  { value: "added", label: "Recently added" },
  { value: "title", label: "Title" },
  { value: "duration", label: "Duration" },
];

/** Sort picker — a dropdown of the available orders (mirrors the app's
 * overlay + panel menu pattern: an invisible full-screen scrim closes it). */
function SortMenu({ sort, onChange }: { sort: TrackSort; onChange: (s: TrackSort) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="flex items-center gap-2 text-[12.5px] text-oct-muted transition-colors hover:text-oct-text"
      >
        <SlidersIcon size={14} />
        <span>Sorted by {SORT_LABEL[sort]}</span>
        <ChevronDownIcon size={13} className={`transition-transform ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div
            role="menu"
            className="absolute right-0 top-full z-40 mt-1.5 w-44 overflow-hidden rounded-xl border border-oct-border-strong bg-oct-surface py-1 shadow-[0_20px_50px_-18px_rgba(0,0,0,0.6)]"
          >
            {SORT_OPTIONS.map((o) => {
              const active = o.value === sort;
              return (
                <button
                  key={o.value}
                  role="menuitemradio"
                  aria-checked={active}
                  onClick={() => {
                    onChange(o.value);
                    setOpen(false);
                  }}
                  className={`flex w-full items-center justify-between gap-3 px-3.5 py-2 text-left text-[13px] transition-colors hover:bg-oct-elevated/60 hover:text-oct-text ${
                    active ? "text-oct-text" : "text-oct-muted"
                  }`}
                >
                  {o.label}
                  {active && <CheckIcon size={14} className="text-oct-accent" />}
                </button>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

// Desktop track table columns (the mobile view uses card rows instead).
const TRACK_GRID = "grid-cols-[28px_40px_minmax(0,1fr)_120px_56px_auto]";

function TracksTab({
  q,
  tracks,
  downloadedIds,
}: {
  q: { isLoading: boolean; isError: boolean; error: unknown };
  tracks: FavoriteTrack[];
  downloadedIds: Set<string>;
}) {
  const [sort, setSort] = useState<TrackSort>("added");
  const playQueue = usePlayerStore((s) => s.playQueue);
  const queueState = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const currentId = currentIndex >= 0 ? queueState[currentIndex]?.id : undefined;

  const sorted = useMemo(() => {
    const list = tracks.slice();
    if (sort === "title") list.sort((a, b) => a.title.localeCompare(b.title));
    else if (sort === "duration") list.sort((a, b) => a.duration_ms - b.duration_ms);
    return list;
  }, [tracks, sort]);
  const trackNames = useTrackNames(tracks);

  if (q.isLoading)
    return (
      <div className="mt-3 flex flex-col gap-1.5">
        {Array.from({ length: 8 }).map((_, i) => (
          <Skeleton key={i} className="h-12 w-full rounded-xl" />
        ))}
      </div>
    );
  if (q.isError) return <p className={`mt-3 ${errorBox}`}>{formatError(q.error)}</p>;
  if (sorted.length === 0) return <EmptyState label="tracks" />;

  const queue = sorted.map(serverTrackToQueueItem);

  return (
    <div className="mt-2 flex flex-col">
      {/* sort row (shared) */}
      <div className="flex items-center justify-between px-1 pb-1.5">
        <span className="font-mono text-[11px] text-oct-subtle">{sorted.length} saved tracks</span>
        <SortMenu sort={sort} onChange={setSort} />
      </div>

      {/* ── desktop: grid table ─────────────────────────────────────────── */}
      <div className="hidden sm:flex sm:flex-col">
        {/* column header */}
        <div
          className={`grid ${TRACK_GRID} items-center gap-x-3.5 border-b border-oct-border px-2.5 pb-2.5 font-mono text-[10.5px] tracking-[0.1em] text-oct-faint`}
        >
          <span className="text-center">#</span>
          <span />
          <span>TITLE</span>
          <span>QUALITY</span>
          <span className="text-right">TIME</span>
          <span />
        </div>

        <div className="flex flex-col pt-1">
          {sorted.map((t, i) => {
            const active = t.id === currentId;
            const m = trackNames(t);
            const sub = trackMetaLine(m.artistName, m.albumTitle);
            return (
              <div
                key={t.id}
                onClick={() => playQueue(queue, i)}
                className={`group grid ${TRACK_GRID} cursor-pointer select-none items-center gap-x-3.5 rounded-xl px-2.5 py-2 ${
                  active ? "bg-oct-accent/[0.08]" : "hover:bg-oct-elevated"
                }`}
              >
                {/* index / play / eq */}
                <span className="relative grid h-[18px] place-items-center">
                  {active ? (
                    <EqBars playing={isPlaying} />
                  ) : (
                    <>
                      <span className="font-mono text-[12.5px] text-oct-faint transition-opacity group-hover:opacity-0">
                        {String(i + 1).padStart(2, "0")}
                      </span>
                      <span className="absolute inset-0 grid place-items-center opacity-0 transition-opacity group-hover:opacity-100">
                        <PlayIcon size={12} className="text-oct-text" />
                      </span>
                    </>
                  )}
                </span>
                <Thumb album={{ id: t.album_id }} size={40} tryCover />
                <span className="min-w-0">
                  <span
                    className={`block truncate text-[14px] ${
                      active ? "font-medium text-oct-accent" : "font-medium"
                    }`}
                  >
                    {t.title}
                  </span>
                  {sub && <span className="mt-0.5 block truncate text-[12px] text-oct-subtle">{sub}</span>}
                </span>
                <span className="truncate font-mono text-[11px] text-oct-subtle">{qualityLabel(t)}</span>
                <span className="text-right font-mono text-[12px] text-oct-subtle">
                  {formatDuration(t.duration_ms)}
                </span>
                <span className="justify-self-end" onClick={(e) => e.stopPropagation()}>
                  <FavoriteButton kind="track" id={t.id} size={16} />
                </span>
              </div>
            );
          })}
        </div>
      </div>

      {/* ── mobile: touch-sized cards ───────────────────────────────────── */}
      <div className="flex flex-col gap-0.5 sm:hidden">
        {sorted.map((t, i) => {
          const active = t.id === currentId;
          const m = trackNames(t);
          const local = downloadedIds.has(t.id);
          return (
            <div
              key={t.id}
              onClick={() => playQueue(queue, i)}
              className={`flex select-none items-center gap-3 rounded-[11px] px-2 py-2 active:bg-oct-elevated ${
                active ? "bg-oct-accent/[0.08]" : ""
              }`}
            >
              {/* cover + now-playing overlay */}
              <div className="relative h-[46px] w-[46px] shrink-0">
                <Thumb album={{ id: t.album_id }} size={46} radius={9} tryCover />
                {active && (
                  <span className="absolute inset-0 grid place-items-center rounded-[9px] bg-black/45">
                    <EqBars playing={isPlaying} />
                  </span>
                )}
              </div>
              {/* title + meta */}
              <div className="min-w-0 flex-1">
                <div className={`truncate text-[14.5px] font-medium ${active ? "text-oct-accent" : ""}`}>
                  {t.title}
                </div>
                <div className="mt-0.5 flex min-w-0 items-center gap-2">
                  {local ? (
                    <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-oct-online" title="On device" />
                  ) : (
                    <CloudIcon size={11} className="shrink-0 text-[#6f9bd1]" />
                  )}
                  <span className="truncate text-[12px] text-oct-subtle">{m.artistName}</span>
                  <span className="shrink-0 font-mono text-[9.5px] text-oct-faint">{compactQuality(t)}</span>
                </div>
              </div>
              {/* duration + heart */}
              <span className="shrink-0 font-mono text-[11.5px] text-oct-subtle">
                {formatDuration(t.duration_ms)}
              </span>
              <span className="shrink-0" onClick={(e) => e.stopPropagation()}>
                <FavoriteButton kind="track" id={t.id} size={18} />
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ── Albums ─────────────────────────────────────────────────────────────────

function AlbumsTab() {
  const online = useAppStore((s) => s.online);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const q = useQuery({ queryKey: ["favorites", "albums"], queryFn: favoritesListAlbums });
  const albums = q.data ?? [];

  async function playAlbum(id: string) {
    try {
      const view = await libraryListTracksByAlbum(id);
      if (view.items.length > 0) playQueue(view.items, 0);
    } catch (e) {
      alert(formatError(e));
    }
  }

  if (q.isLoading) return <div className="mt-3"><SkeletonGrid count={8} /></div>;
  if (q.isError) return <p className={`mt-3 ${errorBox}`}>{formatError(q.error)}</p>;
  if (albums.length === 0) return <EmptyState label="albums" />;

  return (
    <div
      className="mt-3 grid gap-x-[22px] gap-y-7"
      style={{ gridTemplateColumns: "repeat(auto-fill, minmax(176px, 1fr))" }}
    >
      {albums.map((a) => {
        const sub = [a.release_year ? String(a.release_year) : null, a.storage_bytes > 0 ? byteSize(a.storage_bytes) : null]
          .filter(Boolean)
          .join(" · ");
        return (
          <div key={a.id} className="group flex flex-col">
            <div className="relative">
              <Link to={`/albums/${a.id}`} className="block">
                <Cover album={a} tryCover className="w-full" />
              </Link>
              <button
                onClick={() => void playAlbum(a.id)}
                disabled={!online}
                title="Play album"
                className="absolute bottom-2.5 right-2.5 grid h-9 w-9 place-items-center rounded-full bg-oct-accent text-oct-bg shadow-[0_6px_16px_-6px_rgba(0,0,0,0.6)] transition-all duration-150 hover:bg-oct-accent-bright disabled:opacity-40 sm:translate-y-1 sm:opacity-0 sm:group-hover:translate-y-0 sm:group-hover:opacity-100 sm:disabled:opacity-0"
              >
                <PlayIcon size={14} />
              </button>
            </div>
            <div className="mt-2.5 flex items-start justify-between gap-1.5">
              <Link to={`/albums/${a.id}`} className="min-w-0 flex-1">
                <span className="block truncate text-[14px] font-medium">{a.title}</span>
              </Link>
              <FavoriteButton kind="album" id={a.id} size={15} />
            </div>
            {sub && <div className="truncate text-[12.5px] text-oct-subtle">{sub}</div>}
          </div>
        );
      })}
    </div>
  );
}

// ── Artists ────────────────────────────────────────────────────────────────

function ArtistsTab() {
  const online = useAppStore((s) => s.online);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const q = useQuery({ queryKey: ["favorites", "artists"], queryFn: favoritesListArtists });
  const artists = q.data ?? [];

  async function playArtist(id: string) {
    try {
      const tracks = await discoverRadio(id);
      if (tracks.length > 0) playQueue(tracks.map(serverTrackToQueueItem), 0);
    } catch (e) {
      alert(formatError(e));
    }
  }

  // 3-up on phones, fluid auto-fill from `sm` upward.
  const gridCls =
    "mt-3 grid grid-cols-3 gap-x-3.5 gap-y-6 sm:gap-x-[22px] sm:gap-y-7 sm:[grid-template-columns:repeat(auto-fill,minmax(160px,1fr))]";

  if (q.isLoading)
    return (
      <div className={gridCls}>
        {Array.from({ length: 6 }).map((_, i) => (
          <div key={i} className="flex flex-col items-center gap-3">
            <Skeleton className="h-[92px] w-[92px] rounded-full sm:h-[132px] sm:w-[132px]" />
            <Skeleton className="h-3 w-2/3" />
            <Skeleton className="h-2.5 w-1/3" />
          </div>
        ))}
      </div>
    );
  if (q.isError) return <p className={`mt-3 ${errorBox}`}>{formatError(q.error)}</p>;
  if (artists.length === 0) return <EmptyState label="artists" />;

  return (
    <div className={gridCls}>
      {artists.map((a) => (
        <div key={a.id} className="group flex flex-col items-center text-center">
          <div className="relative">
            <Link to={`/artists/${a.id}`} className="block">
              {/* smaller on phones, full size on desktop */}
              <span className="sm:hidden">
                <ArtistAvatar id={a.id} imagePath={a.image_path} size={92} />
              </span>
              <span className="hidden sm:block">
                <ArtistAvatar id={a.id} imagePath={a.image_path} size={132} />
              </span>
            </Link>
            <button
              onClick={() => void playArtist(a.id)}
              disabled={!online}
              title="Play artist radio"
              className="absolute bottom-1.5 right-1.5 grid h-8 w-8 place-items-center rounded-full border-2 border-oct-bg bg-oct-accent text-oct-bg shadow-[0_6px_16px_-6px_rgba(0,0,0,0.6)] transition-all duration-150 hover:bg-oct-accent-bright disabled:opacity-40 sm:h-9 sm:w-9 sm:translate-y-1 sm:opacity-0 sm:group-hover:translate-y-0 sm:group-hover:opacity-100 sm:disabled:opacity-0"
            >
              <PlayIcon size={13} />
            </button>
          </div>
          <Link to={`/artists/${a.id}`} className="mt-2.5 min-w-0 max-w-full sm:mt-3">
            <span className="block truncate text-[13px] font-medium sm:text-[14px]">{a.name}</span>
          </Link>
          <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-oct-subtle sm:text-[12px]">
            <FavoriteButton kind="artist" id={a.id} size={13} className="!p-0" />
            <span>{a.storage_bytes > 0 ? byteSize(a.storage_bytes) : "Favorite"}</span>
          </div>
        </div>
      ))}
    </div>
  );
}
