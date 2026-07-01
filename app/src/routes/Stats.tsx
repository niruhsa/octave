// Listening stats (Phase 11), redesigned to the "OCTAVE Listening" comp: a
// "Wrapped"-style dashboard — a range selector + dated subtitle, five KPI
// cards with vs-prev deltas, a per-day/-month activity chart and a 24-hour
// "when you listen" histogram, a habits panel (completion rate + most-skipped)
// and a sound-quality split, ranked Top tracks / Top artists, and a row of
// records & milestones.
//
// Server-authoritative + online-only, like the notifications feed. Totals and
// top lists come from the server's `play_history_stats` aggregation; the richer
// metrics (unique counts, streak, activity/hour charts, deltas, habits) are
// derived client-side from a page of raw play history. The quality split joins
// the most-played albums' codecs (fetched on demand) onto the windowed plays —
// best-effort, "where resolvable", with an honest coverage caption.

import { useMemo, useState } from "react";
import { useQuery, useQueries } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  playStats,
  playHistoryList,
  libraryListTracksByAlbum,
  type ArtistStat,
  type TrackStat,
  type PlayEvent,
  type MergedTrack,
} from "../ipc";
import { OfflineGate } from "../components/OfflineGate";
import { formatError } from "../lib/error";
import { Skeleton } from "../components/Skeleton";
import { gradientFor, isLossless } from "../lib/visual";

/** Windows offered in the selector. `days: null` = all time. `short` is the
 * compact label used on the full-width mobile range switcher. */
const WINDOWS: { key: string; label: string; short: string; days: number | null }[] = [
  { key: "7d", label: "7 days", short: "7d", days: 7 },
  { key: "30d", label: "30 days", short: "30d", days: 30 },
  { key: "1y", label: "1 year", short: "1y", days: 365 },
  { key: "all", label: "All time", short: "All", days: null },
];

/** How many raw plays we pull to derive charts/streak/quality client-side.
 * Long windows with more plays than this only chart their most recent slice. */
const HISTORY_CAP = 3000;

const DOW = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const DOW_SHORT = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MON = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// accent + the comp's categorical palette
const ACCENT = "#e0a84b";
const BLUE = "#6f9bd1";
const TEAL = "#5fb3a3";
const PINK = "#c77dba";
const CORAL = "#d98a6a";

const panel = "rounded-[13px] border border-oct-border-strong bg-oct-panel";

/** Big KPI readout split into value + unit (e.g. {value:"14", unit:"hrs"}). */
function bigTime(ms: number): { value: string; unit: string } {
  const min = Math.round(ms / 60000);
  if (min < 60) return { value: String(min), unit: "min" };
  const h = Math.floor(min / 60);
  if (h < 48) return { value: String(h), unit: h === 1 ? "hour" : "hrs" };
  const d = Math.floor(h / 24);
  return { value: `${d}d`, unit: `${h % 24}h` };
}

/** "31 min" / "1h 4m" — average session length. */
function avgSessionLabel(ms: number): string {
  const min = Math.round(ms / 60000);
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)}h ${min % 60}m`;
}

/** "9 PM" — hour-of-day label. */
function fmtHour(i: number): string {
  const ap = i < 12 ? "AM" : "PM";
  const hr = i % 12 === 0 ? 12 : i % 12;
  return `${hr} ${ap}`;
}

/** Percent change cur-vs-prev, or null when there's no comparable prior. */
function deltaPct(cur: number, prev: number): number | null {
  if (prev <= 0) return null;
  return Math.round(((cur - prev) / prev) * 100);
}

/** Dated subtitle under the title, computed from "now" so it stays live. */
function rangeSubtitle(days: number | null, now: Date): string {
  const fmtDay = (d: Date) => `${MON[d.getMonth()]} ${d.getDate()}`;
  const fmtMon = (d: Date) => `${MON[d.getMonth()]} ${d.getFullYear()}`;
  if (days == null) return "All time · since you joined";
  if (days <= 31) {
    const start = new Date(now.getTime() - (days - 1) * 86400000);
    return `${fmtDay(start)} – ${fmtDay(now)}, ${now.getFullYear()} · last ${days} days`;
  }
  const start = new Date(now.getFullYear(), now.getMonth() - 11, 1);
  return `${fmtMon(start)} – ${fmtMon(now)} · last 12 months`;
}

// ── derived metrics ─────────────────────────────────────────────────────────

type Bucket = { label: string; full: string; plays: number };
type SkipRow = { key: string; title: string; artist: string; skips: number };

type Derived = {
  windowEvents: PlayEvent[];
  plays: number;
  ms: number;
  uniqueTracks: number;
  uniqueArtists: number;
  streak: number;
  longestStreak: number;
  avgSessionMs: number;
  completionPct: number;
  mostActiveDow: string | null;
  activity: Bucket[];
  activityPeakIdx: number;
  hours: number[];
  hourPeak: number;
  skips: SkipRow[];
  topAlbumIds: string[];
  // previous equal-length window (for deltas); null for all-time
  prev: { plays: number; ms: number; uniqueTracks: number; uniqueArtists: number } | null;
};

/** Bucket windowed plays into the activity chart's columns. */
function buildActivity(events: PlayEvent[], days: number | null, now: Date): { buckets: Bucket[]; peak: number } {
  const monthly = days == null || days > 31;
  const buckets: Bucket[] = [];
  const index = new Map<string, number>();

  if (!monthly) {
    const n = days ?? 30;
    for (let i = n - 1; i >= 0; i--) {
      const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() - i);
      const key = `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
      const pos = buckets.length;
      index.set(key, pos);
      const label = n <= 7 ? DOW_SHORT[d.getDay()] : pos % 5 === 0 ? String(d.getDate()) : "";
      const full = n <= 7 ? DOW[d.getDay()] : `${MON[d.getMonth()]} ${d.getDate()}`;
      buckets.push({ label, full, plays: 0 });
    }
    for (const e of events) {
      const d = new Date(e.played_at);
      const key = `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
      const pos = index.get(key);
      if (pos != null) buckets[pos].plays++;
    }
  } else {
    // months back from the current one — 12 for "1 year", up to 24 for all-time
    let span = 12;
    if (days == null) {
      let oldest = now.getTime();
      for (const e of events) oldest = Math.min(oldest, new Date(e.played_at).getTime());
      const od = new Date(oldest);
      span = Math.min(24, Math.max(1, (now.getFullYear() - od.getFullYear()) * 12 + (now.getMonth() - od.getMonth()) + 1));
    }
    for (let i = span - 1; i >= 0; i--) {
      const d = new Date(now.getFullYear(), now.getMonth() - i, 1);
      const key = `${d.getFullYear()}-${d.getMonth()}`;
      index.set(key, buckets.length);
      buckets.push({ label: MON[d.getMonth()].slice(0, 1), full: MON[d.getMonth()], plays: 0 });
    }
    for (const e of events) {
      const d = new Date(e.played_at);
      const key = `${d.getFullYear()}-${d.getMonth()}`;
      const pos = index.get(key);
      if (pos != null) buckets[pos].plays++;
    }
  }

  let peak = 0;
  buckets.forEach((b, i) => {
    if (b.plays > buckets[peak].plays) peak = i;
  });
  return { buckets, peak };
}

/** Longest run of consecutive calendar days with ≥1 play inside `dayKeys`. */
function longestRun(dayKeys: Set<string>): number {
  if (dayKeys.size === 0) return 0;
  const days = [...dayKeys].map((k) => Number(k)).sort((a, b) => a - b);
  let best = 1;
  let run = 1;
  for (let i = 1; i < days.length; i++) {
    if (days[i] - days[i - 1] === 1) run++;
    else run = 1;
    best = Math.max(best, run);
  }
  return best;
}

/** Day number since epoch (local) — stable key for streak math. */
function dayNumber(d: Date): number {
  return Math.floor(new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 86400000);
}

function deriveStats(all: PlayEvent[], days: number | null, now: Date): Derived {
  const nowMs = now.getTime();
  const cutoff = days == null ? -Infinity : nowMs - days * 86400000;
  const prevCutoff = days == null ? null : cutoff - days * 86400000;

  const windowEvents: PlayEvent[] = [];
  const prevEvents: PlayEvent[] = [];
  for (const e of all) {
    const t = new Date(e.played_at).getTime();
    if (t >= cutoff) windowEvents.push(e);
    else if (prevCutoff != null && t >= prevCutoff) prevEvents.push(e);
  }

  const tally = (evs: PlayEvent[]) => {
    const tracks = new Set<string>();
    const artists = new Set<string>();
    let ms = 0;
    for (const e of evs) {
      tracks.add(e.track_id ?? e.track_title);
      artists.add(e.artist_id ?? e.artist_name);
      ms += e.ms_played;
    }
    return { plays: evs.length, ms, uniqueTracks: tracks.size, uniqueArtists: artists.size };
  };

  const cur = tally(windowEvents);
  const prev = days == null ? null : tally(prevEvents);

  // completion + most-skipped
  let completed = 0;
  const skipMap = new Map<string, SkipRow>();
  const dowCount = new Array(7).fill(0);
  const hours = new Array(24).fill(0);
  const dayKeys = new Set<string>();
  for (const e of windowEvents) {
    if (e.completed) completed++;
    else {
      const key = e.track_id ?? e.track_title;
      const row = skipMap.get(key) ?? { key, title: e.track_title, artist: e.artist_name, skips: 0 };
      row.skips++;
      skipMap.set(key, row);
    }
    const d = new Date(e.played_at);
    dowCount[d.getDay()]++;
    hours[d.getHours()]++;
    dayKeys.add(String(dayNumber(d)));
  }

  let mostActiveDow: string | null = null;
  if (windowEvents.length) {
    let max = 0;
    dowCount.forEach((c, i) => {
      if (c > max) {
        max = c;
        mostActiveDow = DOW[i];
      }
    });
  }

  let hourPeak = 0;
  hours.forEach((c, i) => {
    if (c > hours[hourPeak]) hourPeak = i;
  });

  // current streak: consecutive days ending today (or yesterday) with a play,
  // computed across ALL plays — a streak isn't bounded by the chart window.
  const allDayKeys = new Set<string>();
  for (const e of all) allDayKeys.add(String(dayNumber(new Date(e.played_at))));
  const today = dayNumber(now);
  let streak = 0;
  if (allDayKeys.has(String(today)) || allDayKeys.has(String(today - 1))) {
    let cursor = allDayKeys.has(String(today)) ? today : today - 1;
    while (allDayKeys.has(String(cursor))) {
      streak++;
      cursor--;
    }
  }

  const { buckets, peak } = buildActivity(windowEvents, days, now);

  // top albums by play count — drives the on-demand codec lookups
  const albumCount = new Map<string, number>();
  for (const e of windowEvents) {
    if (e.album_id) albumCount.set(e.album_id, (albumCount.get(e.album_id) ?? 0) + 1);
  }
  const topAlbumIds = [...albumCount.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 12)
    .map(([id]) => id);

  return {
    windowEvents,
    plays: cur.plays,
    ms: cur.ms,
    uniqueTracks: cur.uniqueTracks,
    uniqueArtists: cur.uniqueArtists,
    streak,
    longestStreak: longestRun(dayKeys),
    avgSessionMs: windowEvents.length ? cur.ms / windowEvents.length : 0,
    completionPct: windowEvents.length ? Math.round((completed / windowEvents.length) * 100) : 0,
    mostActiveDow,
    activity: buckets,
    activityPeakIdx: peak,
    hours,
    hourPeak,
    skips: [...skipMap.values()].sort((a, b) => b.skips - a.skips).slice(0, 5),
    topAlbumIds,
    prev,
  };
}

// ── shell ────────────────────────────────────────────────────────────────────

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
  const now = useMemo(() => new Date(), []);

  // server-aggregated totals + top lists (per window)
  const statsQ = useQuery({
    queryKey: ["play_stats", win.days],
    queryFn: () => playStats(win.days ?? undefined, 8),
  });
  // raw plays for client-side charts/streak/quality (window-independent)
  const eventsQ = useQuery({
    queryKey: ["play_history", "events", HISTORY_CAP],
    queryFn: () => playHistoryList(HISTORY_CAP, 0),
  });

  const stats = statsQ.data;
  const events = eventsQ.data?.events;

  const derived = useMemo(
    () => (events ? deriveStats(events, win.days, now) : null),
    [events, win.days, now],
  );

  const empty = !!stats && stats.total_plays === 0;

  // KPI data — rendered as 5 equal cards on desktop and, on mobile, as the
  // comp's 2 "big" cards (plays / time, with deltas) + 3 compact cards.
  const time = bigTime(stats?.total_ms ?? 0);
  const kpis = [
    {
      key: "plays",
      label: "PLAYS",
      value: (stats?.total_plays ?? 0).toLocaleString(),
      iconPath: "M4 2.6v10.8L13 8z",
      iconColor: ACCENT,
      loading: statsQ.isLoading,
      delta: derived && derived.prev ? deltaPct(derived.plays, derived.prev.plays) : null,
    },
    {
      key: "time",
      label: "LISTENING TIME",
      value: time.value,
      unit: time.unit,
      iconPath: "M8 4v4l2.5 1.5M8 1.8a6.2 6.2 0 1 0 0 12.4A6.2 6.2 0 0 0 8 1.8",
      iconColor: BLUE,
      loading: statsQ.isLoading,
      delta: derived && derived.prev ? deltaPct(derived.ms, derived.prev.ms) : null,
    },
    {
      key: "tracks",
      label: "UNIQUE TRACKS",
      value: (derived?.uniqueTracks ?? 0).toLocaleString(),
      iconPath: "M5.5 11.5V4l7-1.5v7",
      iconColor: TEAL,
      loading: eventsQ.isLoading,
      delta: derived && derived.prev ? deltaPct(derived.uniqueTracks, derived.prev.uniqueTracks) : null,
    },
    {
      key: "artists",
      label: "UNIQUE ARTISTS",
      value: (derived?.uniqueArtists ?? 0).toLocaleString(),
      iconPath: "M8 8a2.4 2.4 0 1 0 0-4.8A2.4 2.4 0 0 0 8 8M3.6 13c0-2.3 2-3.7 4.4-3.7s4.4 1.4 4.4 3.7",
      iconColor: PINK,
      loading: eventsQ.isLoading,
      delta: derived && derived.prev ? deltaPct(derived.uniqueArtists, derived.prev.uniqueArtists) : null,
    },
    {
      key: "streak",
      label: "CURRENT STREAK",
      value: String(derived?.streak ?? 0),
      unit: derived?.streak === 1 ? "day" : "days",
      iconPath: "M8 1.8s3.6 2.6 3.6 6.2a3.6 3.6 0 0 1-7.2 0c0-1.2.5-2.2.5-2.2s.7.8 1.6.8c0-2 1.5-4.8 1.5-4.8",
      iconColor: CORAL,
      loading: eventsQ.isLoading,
      foot: "current run",
    },
  ];

  return (
    <section className="oct-scroll">
      <div className="mx-auto max-w-[1180px] px-4 py-6 md:px-8 md:py-7">
        {/* header — title + range switcher side-by-side on desktop; stacked,
            with a full-width segmented switcher, on mobile */}
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div className="min-w-0">
            <div className="font-mono text-[11px] tracking-[0.22em] text-oct-accent">LISTENING</div>
            <h1 className="mt-2 text-[28px] font-semibold leading-none tracking-tight md:text-[32px]">
              Your stats
            </h1>
            <div className="mt-2 font-mono text-[11px] text-oct-subtle md:text-[11.5px]">
              {rangeSubtitle(win.days, now)}
            </div>
          </div>
          <div className="flex w-full gap-1 rounded-full border border-oct-border-strong bg-oct-card p-1 md:w-auto">
            {WINDOWS.map((w, i) => (
              <button
                key={w.key}
                onClick={() => setWindowIdx(i)}
                className={`flex-1 rounded-full px-[15px] py-[7px] text-center text-[12.5px] transition-colors md:flex-none ${
                  i === windowIdx
                    ? "bg-oct-accent font-medium text-oct-bg"
                    : "text-oct-muted hover:text-oct-text"
                }`}
              >
                <span className="md:hidden">{w.short}</span>
                <span className="hidden md:inline">{w.label}</span>
              </button>
            ))}
          </div>
        </div>

        {statsQ.isError && (
          <p className="mt-5 rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-sm text-oct-danger">
            {formatError(statsQ.error)}
          </p>
        )}

        {/* KPI cards — desktop: 5 equal cards in a row */}
        <div className="mt-6 hidden gap-3.5 md:grid md:grid-cols-3 lg:grid-cols-5">
          {kpis.map(({ key, ...k }) => (
            <KpiCard key={key} {...k} />
          ))}
        </div>
        {/* KPI cards — mobile: 2 big (with deltas) + 3 compact */}
        <div className="mt-5 flex flex-col gap-3 md:hidden">
          <div className="grid grid-cols-2 gap-3">
            {kpis.slice(0, 2).map(({ key, ...k }) => (
              <KpiCard key={key} {...k} />
            ))}
          </div>
          <div className="grid grid-cols-3 gap-3">
            {kpis.slice(2).map(({ key, ...k }) => (
              <KpiCard key={key} {...k} variant="small" />
            ))}
          </div>
        </div>

        {empty ? (
          <div className="mt-5 flex flex-col items-center gap-3 rounded-2xl border border-oct-border bg-oct-panel/40 px-6 py-16 text-center">
            <span className="grid h-12 w-12 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
              <Glyph path="M5.5 11.5V4l7-1.5v7" size={22} color="currentColor" />
            </span>
            <p className="text-sm text-oct-subtle">No plays in this window yet.</p>
            <p className="max-w-xs text-[12.5px] leading-relaxed text-oct-faint">
              Play some music and your charts, top tracks &amp; artists will show up here.
            </p>
          </div>
        ) : (
          <>
            {eventsQ.isError ? (
              <p className="mt-3.5 rounded-lg border border-oct-border bg-oct-panel/40 px-3 py-2.5 font-mono text-[12px] text-oct-subtle">
                Couldn&apos;t load your play history — charts &amp; records are unavailable right now.
              </p>
            ) : (
              <>
                {/* activity + when you listen */}
                <div className="mt-3.5 grid gap-3.5 lg:grid-cols-[1.85fr_1fr]">
                  <ActivityCard
                    derived={derived}
                    loading={eventsQ.isLoading}
                    monthly={win.days == null || (win.days ?? 0) > 31}
                  />
                  <HoursCard derived={derived} loading={eventsQ.isLoading} />
                </div>

                {/* habits + sound quality */}
                <div className="mt-3.5 grid gap-3.5 lg:grid-cols-2">
                  <HabitsCard derived={derived} loading={eventsQ.isLoading} />
                  <QualityCard derived={derived} eventsLoading={eventsQ.isLoading} />
                </div>
              </>
            )}

            {/* top tracks + artists (server-aggregated — available even if the
                raw history query failed) */}
            <div className="mt-3.5 grid gap-3.5 lg:grid-cols-2">
              <TopTracks rows={stats?.top_tracks} loading={statsQ.isLoading} />
              <TopArtists rows={stats?.top_artists} loading={statsQ.isLoading} />
            </div>

            {/* records & milestones */}
            {!eventsQ.isError && <RecordsRow derived={derived} loading={eventsQ.isLoading} />}
          </>
        )}
      </div>
    </section>
  );
}

// ── primitives ────────────────────────────────────────────────────────────────

/** Single-path 16×16 line glyph (matches the comp's inline icons). */
function Glyph({ path, size = 14, color, sw = 1.4 }: { path: string; size?: number; color: string; sw?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke={color}
      strokeWidth={sw}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d={path} />
    </svg>
  );
}

function DeltaChip({ delta, foot }: { delta?: number | null; foot?: string }) {
  if (delta == null) {
    return (
      <div className="mt-3 flex items-center gap-1.5 font-mono text-[10px] text-oct-faint">
        <span>{foot ?? "—"}</span>
      </div>
    );
  }
  const up = delta >= 0;
  return (
    <div className="mt-3 flex items-center gap-1.5">
      <span
        className="inline-flex items-center gap-0.5 font-mono text-[10px]"
        style={{ color: up ? "#3fb950" : "#d07a5a" }}
      >
        <svg
          width="9"
          height="9"
          viewBox="0 0 12 12"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
          style={{ transform: up ? "none" : "rotate(180deg)" }}
        >
          <path d="M6 9.5V2.5M3 5.5 6 2.5l3 3" />
        </svg>
        {up ? "+" : ""}
        {delta}%
      </span>
      <span className="text-[10.5px] text-oct-faint">vs prev</span>
    </div>
  );
}

function KpiCard({
  label,
  value,
  unit,
  iconPath,
  iconColor,
  loading,
  delta,
  foot,
  variant = "default",
}: {
  label: string;
  value: string;
  unit?: string;
  iconPath: string;
  iconColor: string;
  loading: boolean;
  delta?: number | null;
  foot?: string;
  /** `small` = the comp's compact mobile tile (icon, value, label; no delta). */
  variant?: "default" | "small";
}) {
  if (variant === "small") {
    return (
      <div className={`${panel} animate-octstatpop p-3`}>
        <Glyph path={iconPath} color={iconColor} size={13} />
        {loading ? (
          <Skeleton className="mt-2.5 h-5 w-12" />
        ) : (
          <div className="mt-2.5 text-[19px] font-semibold leading-none tracking-tight">
            {value}
            {unit && <span className="text-[11px] font-medium text-oct-subtle"> {unit}</span>}
          </div>
        )}
        <div className="mt-1.5 font-mono text-[8px] tracking-[0.08em] text-oct-faint">{label}</div>
      </div>
    );
  }
  return (
    <div className={`${panel} animate-octstatpop p-4`}>
      <div className="flex items-center gap-2">
        <Glyph path={iconPath} color={iconColor} />
        <span className="font-mono text-[9.5px] tracking-[0.13em] text-oct-faint">{label}</span>
      </div>
      {loading ? (
        <Skeleton className="mt-3.5 h-7 w-16" />
      ) : (
        <div className="mt-3.5 flex items-baseline gap-1.5">
          <span className="text-[28px] font-semibold leading-none tracking-tight md:text-[30px]">{value}</span>
          {unit && <span className="text-[13px] font-medium text-oct-subtle">{unit}</span>}
        </div>
      )}
      {!loading && <DeltaChip delta={delta} foot={foot} />}
    </div>
  );
}

// ── activity chart ─────────────────────────────────────────────────────────

function ActivityCard({
  derived,
  loading,
  monthly,
}: {
  derived: Derived | null;
  loading: boolean;
  monthly: boolean;
}) {
  if (loading || !derived) {
    return (
      <div className={`${panel} p-5`}>
        <Skeleton className="h-4 w-40" />
        <Skeleton className="mt-5 h-[148px] w-full rounded-lg" />
      </div>
    );
  }
  const max = Math.max(1, ...derived.activity.map((b) => b.plays));
  const peak = derived.activity[derived.activityPeakIdx];
  const peakLabel = peak?.full ?? "—";
  const n = derived.activity.length;
  const gap = n > 16 ? 4 : n > 10 ? 7 : 14;

  return (
    <div className={`${panel} px-5 pb-4 pt-[18px]`}>
      <div className="flex items-start justify-between">
        <div>
          <div className="text-[14.5px] font-semibold">Listening activity</div>
          <div className="mt-1 font-mono text-[10.5px] text-oct-subtle">
            plays per {monthly ? "month" : "day"} · {n} {monthly ? "months" : "days"}
          </div>
        </div>
        <div className="text-right">
          <div className="text-[18px] font-semibold text-oct-accent">{derived.activity[derived.activityPeakIdx]?.plays ?? 0}</div>
          <div className="mt-0.5 font-mono text-[9.5px] uppercase text-oct-faint">peak · {peakLabel}</div>
        </div>
      </div>
      <div className="mt-5 flex h-[104px] items-end pt-3.5 md:h-[148px]" style={{ gap }}>
        {derived.activity.map((b, i) => {
          const isPeak = i === derived.activityPeakIdx && b.plays > 0;
          return (
            <div
              key={i}
              className="group relative flex h-full min-w-0 flex-1 cursor-default flex-col items-center justify-end"
            >
              <div className="pointer-events-none absolute top-0 left-1/2 z-[2] -translate-x-1/2 rounded-[5px] border border-oct-border-strong bg-oct-card px-1.5 py-0.5 font-mono text-[10px] text-oct-text opacity-0 transition-opacity group-hover:opacity-100">
                {b.plays} plays
              </div>
              <div
                className="animate-octbar w-full max-w-[16px] rounded-t-[4px] transition-colors group-hover:!bg-oct-accent-bright md:max-w-[22px]"
                style={{
                  height: `${14 + (b.plays / max) * 86}%`,
                  background: isPeak ? ACCENT : "rgba(224,168,75,0.42)",
                  animationDelay: `${i * (monthly ? 26 : 9)}ms`,
                }}
              />
              <div className="mt-1.5 overflow-hidden font-mono text-[8.5px] whitespace-nowrap text-oct-faint">
                {b.label}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ── when you listen ──────────────────────────────────────────────────────────

function HoursCard({ derived, loading }: { derived: Derived | null; loading: boolean }) {
  if (loading || !derived) {
    return (
      <div className={`${panel} p-5`}>
        <Skeleton className="h-4 w-32" />
        <Skeleton className="mt-5 h-[120px] w-full rounded-lg" />
      </div>
    );
  }
  const max = Math.max(1, ...derived.hours);
  const hasData = derived.hours.some((h) => h > 0);

  return (
    <div className={`${panel} px-5 pb-4 pt-[18px]`}>
      <div className="text-[14.5px] font-semibold">When you listen</div>
      <div className="mt-1 font-mono text-[10.5px] text-oct-subtle">
        {hasData ? `peak around ${fmtHour(derived.hourPeak)}` : "no plays yet"}
      </div>
      <div className="mt-5 flex h-[84px] items-end gap-0.5 md:h-[120px]">
        {derived.hours.map((v, i) => (
          <div key={i} className="group flex h-full min-w-0 flex-1 cursor-default items-end">
            <div
              className="animate-octbar w-full rounded-[2px] transition-colors group-hover:!bg-[#6f9bd1]"
              style={{
                height: `${8 + (v / max) * 92}%`,
                background: i === derived.hourPeak && hasData ? BLUE : "rgba(111,155,209,0.34)",
                animationDelay: `${i * 7}ms`,
              }}
            />
          </div>
        ))}
      </div>
      <div className="mt-2 flex justify-between font-mono text-[8.5px] text-oct-faint">
        <span>12a</span>
        <span>6a</span>
        <span>12p</span>
        <span>6p</span>
        <span>11p</span>
      </div>
      <div className="my-3.5 h-px bg-oct-border-strong" />
      <div className="flex justify-between">
        <div>
          <div className="text-[11px] text-oct-subtle">Most active day</div>
          <div className="mt-0.5 text-[14px] font-semibold">{derived.mostActiveDow ?? "—"}</div>
        </div>
        <div className="text-right">
          <div className="text-[11px] text-oct-subtle">Avg session</div>
          <div className="mt-0.5 text-[14px] font-semibold">
            {derived.plays ? avgSessionLabel(derived.avgSessionMs) : "—"}
          </div>
        </div>
      </div>
    </div>
  );
}

// ── listening habits (completion + most-skipped) ────────────────────────────

function HabitsCard({ derived, loading }: { derived: Derived | null; loading: boolean }) {
  if (loading || !derived) {
    return (
      <div className={`${panel} p-5`}>
        <Skeleton className="h-4 w-36" />
        <Skeleton className="mt-5 h-24 w-full rounded-lg" />
      </div>
    );
  }
  const completion = derived.completionPct;
  const maxSkips = Math.max(1, ...derived.skips.map((s) => s.skips));

  return (
    <div className={`${panel} px-5 py-[18px]`}>
      <div className="flex items-center justify-between">
        <div className="text-[14.5px] font-semibold">Listening habits</div>
        <div className="font-mono text-[10px] text-oct-faint">{completion}% finished</div>
      </div>

      {/* completion meter */}
      <div className="mt-4">
        <div className="mb-1.5 flex items-center justify-between">
          <span className="text-[13px] font-medium">Completion rate</span>
          <span className="font-mono text-[11px] text-oct-subtle">{completion}%</span>
        </div>
        <div className="flex h-[7px] gap-0.5 overflow-hidden rounded">
          <div
            className="animate-octbarx rounded-l"
            style={{ width: `${completion}%`, background: TEAL }}
          />
          <div className="flex-1 rounded-r bg-oct-line" />
        </div>
      </div>

      <div className="my-4 h-px bg-oct-border-strong" />

      <div className="mb-3 font-mono text-[9.5px] tracking-[0.1em] text-oct-faint">MOST SKIPPED</div>
      {derived.skips.length === 0 ? (
        <p className="font-mono text-[12px] text-oct-subtle">No skipped tracks — you finish what you start.</p>
      ) : (
        <div className="flex flex-col gap-3">
          {derived.skips.map((s) => (
            <div key={s.key}>
              <div className="mb-1.5 flex items-center justify-between gap-3">
                <span className="min-w-0 truncate text-[13px] font-medium">{s.title}</span>
                <span className="shrink-0 font-mono text-[11px] text-oct-subtle">
                  {s.skips} {s.skips === 1 ? "skip" : "skips"}
                </span>
              </div>
              <div className="h-[6px] overflow-hidden rounded bg-oct-line">
                <div
                  className="animate-octbarx h-full rounded"
                  style={{ width: `${(s.skips / maxSkips) * 100}%`, background: CORAL }}
                />
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── sound & source (quality split from played albums' codecs) ───────────────

function QualityCard({ derived, eventsLoading }: { derived: Derived | null; eventsLoading: boolean }) {
  const albumIds = derived?.topAlbumIds ?? [];
  const albumQs = useQueries({
    queries: albumIds.map((id) => ({
      queryKey: ["album_tracks", id],
      queryFn: () => libraryListTracksByAlbum(id),
      staleTime: 5 * 60_000,
    })),
  });

  const quality = useMemo(() => {
    if (!derived) return null;
    const byId = new Map<string, MergedTrack>();
    for (const q of albumQs) for (const t of q.data?.items ?? []) byId.set(t.id, t);
    let lossless = 0;
    let lossy = 0;
    let hires = 0;
    let resolved = 0;
    for (const e of derived.windowEvents) {
      if (!e.track_id) continue;
      const t = byId.get(e.track_id);
      if (!t) continue;
      resolved++;
      if (isLossless(t.codec)) {
        lossless++;
        if ((t.bit_depth ?? 0) > 16 || (t.sample_rate_hz ?? 0) > 48000) hires++;
      } else lossy++;
    }
    const total = lossless + lossy || 1;
    return {
      resolved,
      losslessPct: Math.round((lossless / total) * 100),
      lossyPct: Math.round((lossy / total) * 100),
      hiresPct: resolved ? Math.round((hires / resolved) * 100) : 0,
      coverage: derived.plays ? Math.round((resolved / derived.plays) * 100) : 0,
    };
  }, [derived, albumQs]);

  const albumsLoading = albumQs.some((q) => q.isLoading);

  if (eventsLoading || !derived) {
    return (
      <div className={`${panel} p-5`}>
        <Skeleton className="h-4 w-32" />
        <Skeleton className="mt-5 h-24 w-full rounded-lg" />
      </div>
    );
  }

  return (
    <div className={`${panel} px-5 py-[18px]`}>
      <div className="text-[14.5px] font-semibold">Sound &amp; source</div>
      <div className="mt-1 font-mono text-[10.5px] text-oct-subtle">how it reached your ears</div>

      {albumsLoading && (!quality || quality.resolved === 0) ? (
        <Skeleton className="mt-5 h-[30px] w-full rounded-lg" />
      ) : !quality || quality.resolved === 0 ? (
        <p className="mt-5 font-mono text-[12px] text-oct-subtle">
          Not enough resolvable tracks to gauge quality.
        </p>
      ) : (
        <>
          <div className="mt-5">
            <div className="mb-2 font-mono text-[9.5px] tracking-[0.1em] text-oct-faint">QUALITY</div>
            <div className="flex h-[30px] gap-0.5 overflow-hidden rounded-lg">
              {quality.losslessPct > 0 && (
                <div
                  className="flex min-w-0 items-center px-2.5"
                  style={{ width: `${quality.losslessPct}%`, background: "rgba(224,168,75,0.85)" }}
                >
                  <span className="truncate text-[11.5px] font-semibold text-oct-bg">
                    Lossless {quality.losslessPct}%
                  </span>
                </div>
              )}
              {quality.lossyPct > 0 && (
                <div
                  className="flex min-w-0 items-center justify-end px-2.5"
                  style={{ width: `${quality.lossyPct}%`, background: "var(--color-oct-line)" }}
                >
                  <span className="truncate text-[11.5px] font-medium text-oct-muted">
                    Lossy {quality.lossyPct}%
                  </span>
                </div>
              )}
            </div>
          </div>

          <div className="my-4 h-px bg-oct-border-strong" />
          <div className="flex items-center gap-3.5">
            <div
              className="grid h-[46px] w-[46px] shrink-0 place-items-center rounded-[11px]"
              style={{ background: "rgba(224,168,75,0.13)", border: "1px solid rgba(224,168,75,0.25)" }}
            >
              <Glyph path="M2 9.5V6.5M5 11V5M8 13V3M11 11V5M14 9.5V6.5" size={22} color={ACCENT} sw={1.3} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-semibold">{quality.hiresPct}% hi-res</div>
              <div className="mt-0.5 text-[11.5px] text-oct-subtle">24-bit or &gt;48 kHz playback</div>
            </div>
          </div>
          <div className="mt-3 font-mono text-[9.5px] text-oct-faint">
            based on {quality.coverage}% of plays · top albums resolved
          </div>
        </>
      )}
    </div>
  );
}

// ── top tracks / artists ────────────────────────────────────────────────────

function ListShell({ title, iconPath, children }: { title: string; iconPath: string; children: React.ReactNode }) {
  return (
    <div className={`${panel} px-3.5 pt-4 pb-2.5`}>
      <div className="flex items-center gap-2.5 px-1.5 pb-3">
        <Glyph path={iconPath} color="var(--color-oct-dim)" sw={1.3} />
        <span className="font-mono text-[10.5px] tracking-[0.14em] text-oct-subtle">{title}</span>
      </div>
      {children}
    </div>
  );
}

function ListSkeleton() {
  return (
    <div className="flex flex-col gap-1.5 px-1.5">
      {Array.from({ length: 6 }).map((_, i) => (
        <Skeleton key={i} className="h-12 w-full rounded-lg" />
      ))}
    </div>
  );
}

function TopTracks({ rows, loading }: { rows?: TrackStat[]; loading: boolean }) {
  return (
    <ListShell title="TOP TRACKS" iconPath="M5.5 11.5V4l7-1.5v7">
      {loading ? (
        <ListSkeleton />
      ) : !rows || rows.length === 0 ? (
        <p className="px-1.5 font-mono text-[12px] text-oct-subtle">No data.</p>
      ) : (
        (() => {
          const max = Math.max(1, ...rows.map((r) => r.plays));
          return rows.map((t, i) => (
            <div
              key={(t.track_id ?? t.track_title) + t.artist_name}
              className="flex items-center gap-3 rounded-[9px] px-1.5 py-2 hover:bg-oct-elevated"
            >
              <span className="w-4 shrink-0 text-center font-mono text-[11px] text-oct-faint">{i + 1}</span>
              <span
                className="grid h-9 w-9 shrink-0 place-items-center rounded-[7px]"
                style={{ background: gradientFor(t.track_id ?? t.track_title) }}
              >
                <span className="aspect-square w-2/5 rounded-full border border-white/20" />
              </span>
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] font-medium">{t.track_title}</div>
                <div className="mt-1.5 flex items-center gap-2">
                  <div className="h-1 max-w-[130px] flex-1 overflow-hidden rounded-full bg-oct-line">
                    <div className="h-full rounded-full" style={{ width: `${(t.plays / max) * 100}%`, background: ACCENT }} />
                  </div>
                  <span className="truncate text-[11px] text-oct-subtle">{t.artist_name}</span>
                </div>
              </div>
              <span className="shrink-0 font-mono text-[11px] text-oct-muted">{t.plays.toLocaleString()}</span>
            </div>
          ));
        })()
      )}
    </ListShell>
  );
}

function TopArtists({ rows, loading }: { rows?: ArtistStat[]; loading: boolean }) {
  return (
    <ListShell title="TOP ARTISTS" iconPath="M8 5.5a2.6 2.6 0 1 0 0-5.2A2.6 2.6 0 0 0 8 5.5M3.5 13c0-2.5 2-4 4.5-4s4.5 1.5 4.5 4">
      {loading ? (
        <ListSkeleton />
      ) : !rows || rows.length === 0 ? (
        <p className="px-1.5 font-mono text-[12px] text-oct-subtle">No data.</p>
      ) : (
        (() => {
          const max = Math.max(1, ...rows.map((r) => r.plays));
          return rows.map((a, i) => {
            const inner = (
              <>
                <span className="w-4 shrink-0 text-center font-mono text-[11px] text-oct-faint">{i + 1}</span>
                <span
                  className="grid h-9 w-9 shrink-0 place-items-center rounded-[9px]"
                  style={{ background: gradientFor(a.artist_id ?? a.artist_name) }}
                >
                  <svg width="17" height="17" viewBox="0 0 16 16" fill="rgba(255,255,255,0.85)">
                    <circle cx="8" cy="6" r="2.6" />
                    <path d="M3.6 13.4c0-2.4 2-3.9 4.4-3.9s4.4 1.5 4.4 3.9z" />
                  </svg>
                </span>
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13px] font-medium">{a.artist_name}</div>
                  <div className="mt-1.5 h-1 max-w-[150px] overflow-hidden rounded-full bg-oct-line">
                    <div className="h-full rounded-full" style={{ width: `${(a.plays / max) * 100}%`, background: BLUE }} />
                  </div>
                </div>
                <span className="shrink-0 font-mono text-[11px] text-oct-muted">{a.plays.toLocaleString()}</span>
              </>
            );
            const cls = "flex items-center gap-3 rounded-[9px] px-1.5 py-2 hover:bg-oct-elevated";
            return a.artist_id ? (
              <Link key={a.artist_id} to={`/artists/${a.artist_id}`} className={cls}>
                {inner}
              </Link>
            ) : (
              <div key={a.artist_name} className={cls}>
                {inner}
              </div>
            );
          });
        })()
      )}
    </ListShell>
  );
}

// ── records & milestones ─────────────────────────────────────────────────────

function RecordsRow({ derived, loading }: { derived: Derived | null; loading: boolean }) {
  const records =
    derived && !loading
      ? [
          {
            value: `${derived.longestStreak} ${derived.longestStreak === 1 ? "day" : "days"}`,
            label: "Longest streak this period",
            icon: "M8 1.8s3.6 2.6 3.6 6.2a3.6 3.6 0 0 1-7.2 0c0-1.2.5-2.2.5-2.2s.7.8 1.6.8c0-2 1.5-4.8 1.5-4.8",
            color: CORAL,
          },
          {
            value: derived.mostActiveDow ?? "—",
            label: "Most active day of the week",
            icon: "M2 13h12M4 13V8M7.3 13V4M10.6 13V6M13.9 13V9.5",
            color: ACCENT,
          },
          {
            value: derived.plays ? fmtHour(derived.hourPeak) : "—",
            label: "Your golden hour",
            icon: "M8 4v4l2.5 1.5M8 1.8a6.2 6.2 0 1 0 0 12.4A6.2 6.2 0 0 0 8 1.8",
            color: BLUE,
          },
          {
            value: `${derived.completionPct}%`,
            label: "Tracks played to the end",
            icon: "M8 1.8a6.2 6.2 0 1 0 0 12.4A6.2 6.2 0 0 0 8 1.8M5.5 8l1.8 1.8L10.8 6",
            color: TEAL,
          },
        ]
      : null;

  return (
    <div className="mt-6">
      <div className="px-0.5 pb-3 font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">
        RECORDS &amp; MILESTONES
      </div>
      <div className="grid grid-cols-2 gap-3.5 lg:grid-cols-4">
        {!records
          ? Array.from({ length: 4 }).map((_, i) => <Skeleton key={i} className="h-[110px] w-full rounded-[13px]" />)
          : records.map((r, i) => (
              <div key={i} className={`${panel} p-4`}>
                <div
                  className="grid h-[34px] w-[34px] place-items-center rounded-[9px]"
                  style={{ background: `${r.color}22` }}
                >
                  <Glyph path={r.icon} size={17} color={r.color} />
                </div>
                <div className="mt-3 truncate text-[18px] font-semibold tracking-tight">{r.value}</div>
                <div className="mt-0.5 text-[12px] text-oct-subtle">{r.label}</div>
              </div>
            ))}
      </div>
    </div>
  );
}
