// Listening stats (Phase 11). A lightweight "wrapped": top tracks, top artists,
// and totals over a selectable window, from the server's play-history
// aggregation. Server-authoritative + online-only (like the notifications feed).

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { playStats, type ArtistStat, type TrackStat } from "../ipc";
import { OfflineGate } from "../components/OfflineGate";
import { formatError } from "../lib/error";
import { Skeleton } from "../components/Skeleton";
import { card } from "../lib/ui";
import { SongIcon, ArtistIcon } from "../components/icons";

/** Windows offered in the selector. `days: null` = all time. */
const WINDOWS: { label: string; days: number | null }[] = [
  { label: "7 days", days: 7 },
  { label: "30 days", days: 30 },
  { label: "1 year", days: 365 },
  { label: "All time", days: null },
];

/** "3h 24m" / "12m" / "45s" from a millisecond total. */
function formatListenTime(ms: number): string {
  const secs = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${secs}s`;
}

export default function Stats() {
  return (
    <OfflineGate feature="Listening stats">
      <StatsInner />
    </OfflineGate>
  );
}

function StatsInner() {
  const [windowIdx, setWindowIdx] = useState(1); // default: 30 days
  const win = WINDOWS[windowIdx];

  const q = useQuery({
    queryKey: ["play_stats", win.days],
    queryFn: () => playStats(win.days ?? undefined, 20),
  });

  const stats = q.data;

  return (
    <section className="mx-auto flex max-w-3xl flex-col gap-6 p-6 md:p-8">
      <header className="flex flex-col gap-3">
        <div className="flex min-w-0 flex-col">
          <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">
            LISTENING
          </span>
          <h1 className="mt-1.5 text-3xl font-semibold tracking-tight">Your stats</h1>
        </div>
        {/* window selector */}
        <div className="flex flex-wrap gap-2">
          {WINDOWS.map((w, i) => (
            <button
              key={w.label}
              onClick={() => setWindowIdx(i)}
              className={`rounded-full border px-3 py-1 font-mono text-[12px] transition-colors ${
                i === windowIdx
                  ? "border-oct-accent bg-oct-accent/15 text-oct-text"
                  : "border-oct-border text-oct-subtle hover:bg-oct-elevated/50"
              }`}
            >
              {w.label}
            </button>
          ))}
        </div>
      </header>

      {q.isError && (
        <p className="rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-sm text-oct-danger">
          {formatError(q.error)}
        </p>
      )}

      {/* totals */}
      <div className="grid gap-4 sm:grid-cols-2">
        <div className={`${card} p-5`}>
          <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">PLAYS</div>
          {q.isLoading ? (
            <Skeleton className="mt-2 h-8 w-20" />
          ) : (
            <div className="mt-1.5 text-3xl font-semibold tracking-tight">
              {(stats?.total_plays ?? 0).toLocaleString()}
            </div>
          )}
        </div>
        <div className={`${card} p-5`}>
          <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">
            LISTENING TIME
          </div>
          {q.isLoading ? (
            <Skeleton className="mt-2 h-8 w-28" />
          ) : (
            <div className="mt-1.5 text-3xl font-semibold tracking-tight">
              {formatListenTime(stats?.total_ms ?? 0)}
            </div>
          )}
        </div>
      </div>

      {q.data && stats && stats.total_plays === 0 && (
        <div className="flex flex-col items-center gap-3 rounded-2xl border border-oct-border bg-oct-panel/40 px-6 py-14 text-center">
          <span className="grid h-12 w-12 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
            <SongIcon size={22} />
          </span>
          <p className="text-sm text-oct-subtle">No plays in this window yet.</p>
          <p className="max-w-xs text-[12.5px] leading-relaxed text-oct-faint">
            Play some music and your top tracks &amp; artists will show up here.
          </p>
        </div>
      )}

      <div className="grid gap-6 md:grid-cols-2">
        <TopList
          title="Top tracks"
          Icon={SongIcon}
          loading={q.isLoading}
          rows={(stats?.top_tracks ?? []).map((t: TrackStat) => ({
            key: (t.track_id ?? t.track_title) + t.artist_name,
            primary: t.track_title,
            secondary: t.artist_name,
            plays: t.plays,
          }))}
        />
        <TopList
          title="Top artists"
          Icon={ArtistIcon}
          loading={q.isLoading}
          rows={(stats?.top_artists ?? []).map((a: ArtistStat) => ({
            key: a.artist_id ?? a.artist_name,
            primary: a.artist_name,
            secondary: null,
            plays: a.plays,
          }))}
        />
      </div>
    </section>
  );
}

type Row = { key: string; primary: string; secondary: string | null; plays: number };

function TopList({
  title,
  Icon,
  loading,
  rows,
}: {
  title: string;
  Icon: (p: { size?: number; className?: string }) => React.ReactElement;
  loading: boolean;
  rows: Row[];
}) {
  return (
    <div className="flex flex-col gap-3">
      <h2 className="flex items-center gap-2 font-mono text-[11px] tracking-[0.14em] text-oct-faint">
        <Icon size={13} /> {title.toUpperCase()}
      </h2>
      {loading ? (
        <div className="flex flex-col gap-1.5">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-11 w-full rounded-lg" />
          ))}
        </div>
      ) : rows.length === 0 ? (
        <p className="font-mono text-[12px] text-oct-subtle">No data.</p>
      ) : (
        <ol className="flex flex-col gap-1">
          {rows.map((r, i) => (
            <li
              key={r.key}
              className="flex items-center gap-3 rounded-lg border border-oct-border px-3 py-2"
            >
              <span className="w-5 shrink-0 text-right font-mono text-[12px] text-oct-faint">
                {i + 1}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[14px] font-medium">{r.primary}</span>
                {r.secondary && (
                  <span className="block truncate text-[12px] text-oct-subtle">{r.secondary}</span>
                )}
              </span>
              <span className="shrink-0 font-mono text-[11.5px] text-oct-subtle">
                {r.plays.toLocaleString()} {r.plays === 1 ? "play" : "plays"}
              </span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
