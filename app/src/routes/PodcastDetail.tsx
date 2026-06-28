import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router-dom";
import {
  podcastDeleteEpisode,
  podcastDownloadEpisode,
  podcastDownloadShow,
  podcastGet,
  podcastListEpisodes,
  podcastRefresh,
  podcastSetAutoDownload,
  podcastSubscribe,
  podcastUnsubscribe,
  type LibraryView,
  type MergedEpisode,
} from "../ipc";
import { SourceBadge } from "../components/SourceBadge";
import { DownloadStatus } from "../components/DownloadStatus";
import { DownloadIcon, PlayIcon, PodcastIcon, SearchIcon, TrashIcon } from "../components/icons";
import { EqBars } from "../components/EqBars";
import { usePlayerStore, episodeToQueueItem } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import { useAppStore } from "../store";
import { byteSize, formatDuration } from "../lib/format";
import { formatError } from "../lib/error";
import { btnGhostSm, btnPrimary, card, errorBox, input, label } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";

function fmtDate(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

/** Published timestamp as a sortable number; missing/invalid dates sink last. */
function pubTime(iso: string | null): number {
  if (!iso) return 0;
  const t = new Date(iso).getTime();
  return Number.isNaN(t) ? 0 : t;
}

type EpisodeSort = "newest" | "oldest" | "longest" | "shortest";

const SORT_OPTIONS: { value: EpisodeSort; label: string }[] = [
  { value: "newest", label: "Newest first" },
  { value: "oldest", label: "Oldest first" },
  { value: "longest", label: "Longest" },
  { value: "shortest", label: "Shortest" },
];

/** Episodes shown per page; the first is the default. */
const PAGE_SIZES = [10, 25, 50, 75, 100] as const;

/**
 * Trailing watch-state indicator for an episode row:
 *   • new / unwatched (never started) → solid orange dot
 *   • started but not finished        → play-progress ring (mirrors the download
 *                                        ring, with a play glyph + the played arc)
 *   • finished                        → muted grey dot
 */
function EpisodeWatchStatus({
  completed,
  positionMs,
  durationMs,
}: {
  completed: boolean;
  positionMs: number;
  durationMs: number;
}) {
  if (completed) {
    return (
      <span
        className="inline-block h-2 w-2 shrink-0 rounded-full bg-oct-faint"
        title="Played"
      />
    );
  }
  const pct = durationMs > 0 ? Math.min(1, positionMs / durationMs) : 0;
  const partway = positionMs > 0 && pct < 0.999;
  if (!partway) {
    return (
      <span
        className="inline-block h-2 w-2 shrink-0 rounded-full bg-oct-accent"
        title="New — not played yet"
      />
    );
  }
  // Play-progress ring — same geometry as the download ring, play glyph inside.
  const size = 16;
  const sw = 2;
  const r = (size - sw) / 2;
  const c = 2 * Math.PI * r;
  const shown = Math.max(0.05, Math.min(1, pct));
  return (
    <span
      className="relative grid shrink-0 place-items-center"
      style={{ width: size, height: size }}
      title={`${Math.round(pct * 100)}% played`}
    >
      <svg
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        style={{ transform: "rotate(-90deg)" }}
      >
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth={sw}
          className="text-oct-line"
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth={sw}
          strokeLinecap="round"
          strokeDasharray={c}
          strokeDashoffset={c * (1 - shown)}
          className="text-oct-accent"
        />
      </svg>
      <PlayIcon
        size={8}
        className="pointer-events-none absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 text-oct-accent"
      />
    </span>
  );
}

export default function PodcastDetail() {
  const { id = "" } = useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const online = useAppStore((s) => s.online);
  const tier = useAppStore((s) => s.tier);
  const session = useAppStore((s) => s.session);
  const isManager = tier === "admin" || tier === "manager";

  const showQ = useQuery({
    queryKey: ["podcasts", "show", id],
    queryFn: () => podcastGet(id),
    enabled: !!id,
  });
  const epQ = useQuery({
    queryKey: ["podcasts", "episodes", id],
    queryFn: () => podcastListEpisodes(id),
    enabled: !!id,
  });

  const playQueue = usePlayerStore((s) => s.playQueue);
  const nowPlayingId = usePlayerStore((s) => s.queue[s.currentIndex]?.id);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const active = useDownloadsStore((s) => s.active);
  const clearDownload = useDownloadsStore((s) => s.clear);
  const [actionErr, setActionErr] = useState<string | null>(null);
  // Episodes tapped for download whose transfer hasn't begun yet (drives the
  // pending ring). `downloadStarted` is a synchronous guard so a burst of taps
  // before the first re-render can't each kick off a duplicate download.
  const [pendingIds, setPendingIds] = useState<Record<string, boolean>>({});
  const downloadStarted = useRef<Set<string>>(new Set());
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<EpisodeSort>("newest");
  const [pageSize, setPageSize] = useState<number>(PAGE_SIZES[0]);
  const [page, setPage] = useState(1);

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: ["podcasts", "episodes", id] });
    void qc.invalidateQueries({ queryKey: ["podcasts", "show", id] });
  };

  const refresh = useMutation({
    mutationFn: () => podcastRefresh(id),
    onSuccess: invalidate,
    onError: (e) => setActionErr(formatError(e)),
  });
  const toggleSub = useMutation({
    mutationFn: () => {
      const wasSubscribed = !!showQ.data?.subscribed;
      return (wasSubscribed ? podcastUnsubscribe(id) : podcastSubscribe(id)).then(
        () => wasSubscribed,
      );
    },
    onSuccess: (wasSubscribed) => {
      void qc.invalidateQueries({ queryKey: ["podcasts", "show", id] });
      // Refresh the subscription list so the unsubscribed show drops out of it.
      void qc.invalidateQueries({ queryKey: ["podcasts", "subscriptions"] });
      // After unsubscribing, return to the podcast tab so the user isn't left
      // staring at a show they no longer follow.
      if (wasSubscribed) navigate("/podcasts");
    },
    onError: (e) => setActionErr(formatError(e)),
  });
  const setAuto = useMutation({
    mutationFn: (n: number) => podcastSetAutoDownload(id, n),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["podcasts", "show", id] }),
    onError: (e) => setActionErr(formatError(e)),
  });
  const dlShow = useMutation({
    mutationFn: () => podcastDownloadShow(id, 10),
    onSuccess: invalidate,
    onError: (e) => setActionErr(formatError(e)),
  });

  const episodes = epQ.data?.items ?? [];

  // Search by title/description, then sort. A stable sort keeps the server's
  // newest-first order as the tiebreaker for equal keys.
  const visibleEpisodes = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? episodes.filter(
          (e) =>
            e.title.toLowerCase().includes(q) ||
            (e.description?.toLowerCase().includes(q) ?? false),
        )
      : episodes;
    return [...filtered].sort((a, b) => {
      switch (sort) {
        case "oldest":
          return pubTime(a.published_at) - pubTime(b.published_at);
        case "longest":
          return (b.duration_ms ?? 0) - (a.duration_ms ?? 0);
        case "shortest":
          return (a.duration_ms ?? 0) - (b.duration_ms ?? 0);
        case "newest":
        default:
          return pubTime(b.published_at) - pubTime(a.published_at);
      }
    });
  }, [episodes, query, sort]);

  // Paginate the filtered/sorted list. `page` is 1-based; clamp it so a shrinking
  // result set (new search) or a larger page size can't strand us past the end.
  const pageCount = Math.max(1, Math.ceil(visibleEpisodes.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const pagedEpisodes = useMemo(
    () => visibleEpisodes.slice((safePage - 1) * pageSize, safePage * pageSize),
    [visibleEpisodes, safePage, pageSize],
  );

  // Jump back to the first page whenever the result set or page size changes, so
  // the user isn't left on an empty/stale page.
  useEffect(() => setPage(1), [query, sort, pageSize]);

  // Play from the full filtered/sorted series (not just the visible page), so the
  // queue order — and what "next" plays — matches the active search + sort and
  // continues past the page boundary.
  const play = (ep: MergedEpisode) => {
    const items = visibleEpisodes.map(episodeToQueueItem);
    const start = Math.max(0, items.findIndex((i) => i.id === ep.id));
    playQueue(items, start);
  };

  const download = async (ep: MergedEpisode) => {
    // Ignore repeat taps: already downloaded, queued, or transfer in flight.
    if (ep.downloaded || downloadStarted.current.has(ep.id) || active[ep.id]) return;
    downloadStarted.current.add(ep.id);
    setPendingIds((p) => ({ ...p, [ep.id]: true }));
    setActionErr(null);
    try {
      await podcastDownloadEpisode(ep.id);
      invalidate();
    } catch (e) {
      setActionErr(formatError(e));
    } finally {
      downloadStarted.current.delete(ep.id);
      setPendingIds((p) => {
        const next = { ...p };
        delete next[ep.id];
        return next;
      });
    }
  };
  const remove = async (ep: MergedEpisode) => {
    setActionErr(null);
    try {
      await podcastDeleteEpisode(ep.id);
      // Drop any finished progress entry so the row resets to a download button
      // (the store keeps terminal entries until the app restarts).
      clearDownload(ep.id);
      // The local file + cache row are gone, so the episode is definitively not
      // downloaded — flip the cached list immediately instead of waiting on the
      // server-first refetch, which can lag or transiently fail and leave the row
      // stuck showing "downloaded" until a manual refresh.
      qc.setQueryData<LibraryView<MergedEpisode>>(["podcasts", "episodes", id], (old) =>
        old
          ? {
              ...old,
              items: old.items.map((e) =>
                e.id === ep.id ? { ...e, downloaded: false, local_file_path: null } : e,
              ),
            }
          : old,
      );
      invalidate();
    } catch (e) {
      setActionErr(formatError(e));
    }
  };

  const show = showQ.data;

  return (
    <div className="mx-auto max-w-3xl px-4 py-6 space-y-6">
      {/* ---- header ---- */}
      <header className="flex gap-4">
        {show?.image_url ? (
          <img
            src={show.image_url}
            alt=""
            className="h-28 w-28 rounded-xl object-cover shrink-0"
          />
        ) : (
          <div className="h-28 w-28 rounded-xl bg-oct-panel grid place-items-center text-oct-faint shrink-0">
            <PodcastIcon size={40} />
          </div>
        )}
        <div className="min-w-0 flex-1">
          <h1 className="text-lg font-semibold leading-tight">
            {show?.title ?? "Podcast"}
          </h1>
          {show?.author && <div className="text-sm text-oct-dim">{show.author}</div>}
          {show && show.storage_bytes > 0 && (
            <div className="mt-1 font-mono text-[11px] text-oct-subtle">
              {byteSize(show.storage_bytes)} downloaded on server
            </div>
          )}
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {/* Subscribe toggle — bearer users only (a SECRET_KEY session can't
                own a subscription). */}
            {session?.kind === "bearer" && (
              <button
                className={show?.subscribed ? btnGhostSm : btnPrimary}
                onClick={() => toggleSub.mutate()}
                disabled={!online || toggleSub.isPending}
                title={online ? undefined : "Reconnect to change subscription"}
              >
                {show?.subscribed ? "Subscribed ✓" : "Subscribe"}
              </button>
            )}
            <button
              className={btnGhostSm}
              onClick={() => refresh.mutate()}
              disabled={!online || !isManager || refresh.isPending}
              title={isManager ? "Check the feed for new episodes" : "Managers only"}
            >
              {refresh.isPending ? "Checking…" : "Check for new"}
            </button>
            <button
              className={btnGhostSm}
              onClick={() => dlShow.mutate()}
              disabled={!online || dlShow.isPending}
              title="Download the newest 10 episodes"
            >
              {dlShow.isPending ? "Queued…" : "Download newest"}
            </button>
            {isManager && (
              <label className="flex items-center gap-1 text-[11px] text-oct-faint">
                Auto-DL
                <select
                  className="bg-oct-panel border border-oct-border rounded px-1 py-0.5 text-oct-text"
                  value={show?.auto_download ?? 0}
                  onChange={(e) => setAuto.mutate(Number(e.target.value))}
                  disabled={!online}
                >
                  {[0, 1, 3, 5, 10].map((n) => (
                    <option key={n} value={n}>
                      {n === 0 ? "off" : n}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>
        </div>
      </header>

      {refresh.data && !refresh.data.not_modified && (
        <div className="text-xs text-oct-dim">
          {refresh.data.new_episodes} new episode
          {refresh.data.new_episodes === 1 ? "" : "s"}.
        </div>
      )}
      {actionErr && <div className={errorBox}>{actionErr}</div>}

      {/* ---- episodes ---- */}
      <section className="space-y-3">
        <div className="flex items-center gap-2">
          <h2 className={label}>EPISODES</h2>
          {epQ.data && <SourceBadge source={epQ.data.source} />}
          {!epQ.isLoading && episodes.length > 0 && (
            <span className="ml-auto font-mono text-[10.5px] text-oct-faint">
              {query.trim() && visibleEpisodes.length !== episodes.length
                ? `${visibleEpisodes.length} / ${episodes.length}`
                : episodes.length}
            </span>
          )}
        </div>

        {/* search within the series + sort/filter */}
        {!epQ.isLoading && episodes.length > 0 && (
          <div className="flex gap-2">
            <div className="relative flex-1">
              <span className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-oct-faint">
                <SearchIcon size={15} sw={1.4} />
              </span>
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search episodes"
                aria-label="Search episodes"
                className={`${input} pl-9`}
              />
            </div>
            <select
              value={sort}
              onChange={(e) => setSort(e.target.value as EpisodeSort)}
              aria-label="Sort episodes"
              className="shrink-0 rounded-lg border border-oct-border-strong bg-oct-card px-2.5 py-2 text-sm text-oct-text focus:border-oct-accent focus:outline-none"
            >
              {SORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
            <select
              value={pageSize}
              onChange={(e) => setPageSize(Number(e.target.value))}
              aria-label="Episodes per page"
              className="shrink-0 rounded-lg border border-oct-border-strong bg-oct-card px-2.5 py-2 text-sm text-oct-text focus:border-oct-accent focus:outline-none"
            >
              {PAGE_SIZES.map((n) => (
                <option key={n} value={n}>
                  {n} / page
                </option>
              ))}
            </select>
          </div>
        )}

        {epQ.isLoading ? (
          <p className="text-sm text-oct-dim">Loading…</p>
        ) : episodes.length === 0 ? (
          <p className="text-sm text-oct-dim">No episodes.</p>
        ) : visibleEpisodes.length === 0 ? (
          <p className="text-sm text-oct-dim">No episodes match “{query.trim()}”.</p>
        ) : (
          <ul className={`${card} divide-y divide-oct-border`}>
            {pagedEpisodes.map((ep) => {
              const live = active[ep.id];
              const inProgress = (live && !live.error) || pendingIds[ep.id];
              const playingThis = nowPlayingId === ep.id;
              // Watch state for the trailing indicator: started-but-unfinished
              // shows a play-progress ring; "Resume" tooltip on the play button.
              const dur = ep.duration_ms ?? 0;
              const pos = ep.position_ms ?? 0;
              const partway = !ep.completed && pos > 0 && (dur === 0 || pos < dur * 0.999);
              return (
                <li key={ep.id} className="flex items-center gap-3 p-3">
                  <button
                    onClick={() => play(ep)}
                    className="grid h-8 w-8 place-items-center rounded-full bg-oct-panel text-oct-text hover:bg-oct-border-strong shrink-0"
                    title={partway ? "Resume" : "Play"}
                  >
                    {playingThis && isPlaying ? <EqBars /> : <PlayIcon size={14} />}
                  </button>
                  <div className="min-w-0 flex-1">
                    <div
                      className={`truncate text-sm font-medium ${
                        ep.completed ? "text-oct-subtle" : ""
                      }`}
                    >
                      {ep.title}
                    </div>
                    <div className="flex items-center gap-2 text-[11px] text-oct-faint">
                      {ep.published_at && (
                        <span className="text-oct-dim">{fmtDate(ep.published_at)}</span>
                      )}
                      {ep.duration_ms != null && ep.duration_ms > 0 && (
                        <span>
                          {ep.published_at ? "· " : ""}
                          {formatDuration(ep.duration_ms)}
                        </span>
                      )}
                      <EpisodeWatchStatus
                        completed={ep.completed}
                        positionMs={pos}
                        durationMs={dur}
                      />
                    </div>
                  </div>
                  {ep.downloaded ? (
                    <button
                      onClick={() => void remove(ep)}
                      className="text-oct-accent hover:text-oct-accent-bright shrink-0"
                      title="Remove download"
                    >
                      <TrashIcon size={15} />
                    </button>
                  ) : inProgress ? (
                    <span className="flex h-[15px] w-[15px] shrink-0 items-center justify-center">
                      <DownloadStatus
                        trackId={ep.id}
                        downloaded={ep.downloaded}
                        pending={!!pendingIds[ep.id]}
                      />
                    </span>
                  ) : (
                    <button
                      onClick={() => void download(ep)}
                      className="text-oct-dim hover:text-oct-text disabled:opacity-30 shrink-0"
                      {...offlineAttrs(online, false, "Download")}
                      title="Download"
                    >
                      <DownloadIcon size={15} />
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}

        {/* pagination — only when the filtered list spills past one page */}
        {!epQ.isLoading && visibleEpisodes.length > pageSize && (
          <div className="flex items-center justify-between gap-2 pt-1">
            <span className="font-mono text-[10.5px] text-oct-faint">
              {(safePage - 1) * pageSize + 1}–
              {Math.min(safePage * pageSize, visibleEpisodes.length)} of{" "}
              {visibleEpisodes.length}
            </span>
            <div className="flex items-center gap-2">
              <button
                className={btnGhostSm}
                onClick={() => setPage(Math.max(1, safePage - 1))}
                disabled={safePage <= 1}
              >
                Prev
              </button>
              <span className="text-[11px] text-oct-subtle">
                Page {safePage} / {pageCount}
              </span>
              <button
                className={btnGhostSm}
                onClick={() => setPage(Math.min(pageCount, safePage + 1))}
                disabled={safePage >= pageCount}
              >
                Next
              </button>
            </div>
          </div>
        )}
      </section>
    </div>
  );
}
