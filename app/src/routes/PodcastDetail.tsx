import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams } from "react-router-dom";
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
  type MergedEpisode,
} from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { DownloadIcon, PlayIcon, PodcastIcon, TrashIcon } from "../components/icons";
import { EqBars } from "../components/EqBars";
import { usePlayerStore, episodeToQueueItem } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import { useAppStore } from "../store";
import { formatDuration } from "../lib/format";
import { formatError } from "../lib/error";
import { btnGhostSm, btnPrimary, card, errorBox, label } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";

function fmtDate(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? ""
    : d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

export default function PodcastDetail() {
  const { id = "" } = useParams();
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
    queryFn: () => podcastListEpisodes(id, { limit: 100 }),
    enabled: !!id,
  });

  const playQueue = usePlayerStore((s) => s.playQueue);
  const nowPlayingId = usePlayerStore((s) => s.queue[s.currentIndex]?.id);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const active = useDownloadsStore((s) => s.active);
  const [actionErr, setActionErr] = useState<string | null>(null);

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
    mutationFn: () =>
      showQ.data?.subscribed ? podcastUnsubscribe(id) : podcastSubscribe(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["podcasts", "show", id] }),
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

  const play = (ep: MergedEpisode) => {
    const items = episodes.map(episodeToQueueItem);
    const start = Math.max(0, items.findIndex((i) => i.id === ep.id));
    playQueue(items, start);
  };

  const download = async (ep: MergedEpisode) => {
    setActionErr(null);
    try {
      await podcastDownloadEpisode(ep.id);
      invalidate();
    } catch (e) {
      setActionErr(formatError(e));
    }
  };
  const remove = async (ep: MergedEpisode) => {
    setActionErr(null);
    try {
      await podcastDeleteEpisode(ep.id);
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
      <section className="space-y-2">
        <div className="flex items-center gap-2">
          <h2 className={label}>EPISODES</h2>
          {epQ.data && <SourceBadge source={epQ.data.source} />}
        </div>
        {epQ.isLoading ? (
          <p className="text-sm text-oct-dim">Loading…</p>
        ) : episodes.length === 0 ? (
          <p className="text-sm text-oct-dim">No episodes.</p>
        ) : (
          <ul className={`${card} divide-y divide-oct-border`}>
            {episodes.map((ep) => {
              const live = active[ep.id];
              const downloading = live && !live.done && !live.error;
              const pct =
                downloading && live.total
                  ? Math.round((live.received / live.total) * 100)
                  : null;
              const playingThis = nowPlayingId === ep.id;
              return (
                <li key={ep.id} className="flex items-center gap-3 p-3">
                  <button
                    onClick={() => play(ep)}
                    className="grid h-8 w-8 place-items-center rounded-full bg-oct-panel text-oct-text hover:bg-oct-border-strong shrink-0"
                    title="Play"
                  >
                    {playingThis && isPlaying ? <EqBars /> : <PlayIcon size={14} />}
                  </button>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{ep.title}</div>
                    <div className="flex items-center gap-2 text-[11px] text-oct-faint">
                      {ep.published_at && <span>{fmtDate(ep.published_at)}</span>}
                      {ep.duration_ms != null && ep.duration_ms > 0 && (
                        <span>· {formatDuration(ep.duration_ms)}</span>
                      )}
                      <DownloadedDot downloaded={ep.downloaded} />
                    </div>
                  </div>
                  {downloading ? (
                    <span className="text-[11px] text-oct-accent shrink-0">
                      {pct != null ? `${pct}%` : "…"}
                    </span>
                  ) : ep.downloaded ? (
                    <button
                      onClick={() => void remove(ep)}
                      className="text-oct-accent hover:text-oct-accent-bright shrink-0"
                      title="Remove download"
                    >
                      <TrashIcon size={15} />
                    </button>
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
      </section>
    </div>
  );
}
