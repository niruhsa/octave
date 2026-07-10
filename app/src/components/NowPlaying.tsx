// Full-screen "Now Playing" player (OCTAVE design B — "Full player with
// hi-res readout"). Opened by tapping the mini-player in `PlayerBar`; the
// header chevron collapses it back. Drives the shared `usePlayerStore`, so
// transport here and in the bar (and the OS media notification) stay in sync.
//
// Layout mirrors the design comp: large artwork, title + artist, SAVED + a
// hi-res format chip, an animated visualizer, a scrubber with time readouts,
// the transport row (shuffle / prev / play-pause / next / repeat), and a
// bottom output/details strip. The header's queue icon swaps the upper region
// for the "Up next" queue list.

import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { usePlayerStore } from "../player/store";
import { getLyrics, type Lyrics } from "../ipc";
import { usePlayerUi } from "../player/ui";
import { useNowPlayingMeta } from "../player/useNowPlayingMeta";
import { formatDuration } from "../lib/format";
import { qualityLabel } from "../lib/visual";
import { trackMetaLine } from "../lib/trackMeta";
import { useTrackNames } from "../lib/useTrackNames";
import { Cover } from "./Cover";
import { EqBars } from "./EqBars";
import {
  ChevronDownIcon,
  DownloadIcon,
  NextIcon,
  PauseIcon,
  PlayIcon,
  PrevIcon,
  QueueIcon,
  RepeatIcon,
  RepeatOneIcon,
  ShuffleIcon,
  VolumeHiIcon,
} from "./icons";

// Per-bar timing for the 10-bar visualizer, lifted from the design comp so the
// equalizer reads the same. Each bar runs the shared `eqbar` keyframe.
const VIS_BARS = [
  { dur: "1s", delay: "0s" },
  { dur: "1.1s", delay: "0.1s" },
  { dur: "0.85s", delay: "0.25s" },
  { dur: "1.2s", delay: "0.4s" },
  { dur: "0.95s", delay: "0.15s" },
  { dur: "1.05s", delay: "0.5s" },
  { dur: "0.9s", delay: "0.3s" },
  { dur: "1.15s", delay: "0.05s" },
  { dur: "1s", delay: "0.45s" },
  { dur: "0.88s", delay: "0.2s" },
];

export default function NowPlaying() {
  const expanded = usePlayerUi((s) => s.expanded);
  const close = usePlayerUi((s) => s.close);

  const queue = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const positionSec = usePlayerStore((s) => s.positionSec);
  const durationSec = usePlayerStore((s) => s.durationSec);
  const shuffle = usePlayerStore((s) => s.shuffle);
  const repeat = usePlayerStore((s) => s.repeat);
  const togglePlay = usePlayerStore((s) => s.togglePlay);
  const next = usePlayerStore((s) => s.next);
  const prev = usePlayerStore((s) => s.prev);
  const seekTo = usePlayerStore((s) => s.seekTo);
  const playAt = usePlayerStore((s) => s.playAt);
  const toggleShuffle = usePlayerStore((s) => s.toggleShuffle);
  const cycleRepeat = usePlayerStore((s) => s.cycleRepeat);

  const current = currentIndex >= 0 ? queue[currentIndex] : null;
  const meta = useNowPlayingMeta(current);
  // Upper region has three modes: artwork (default), queue, and lyrics. The
  // header icons toggle queue/lyrics; they're mutually exclusive.
  const [showQueue, setShowQueue] = useState(false);
  const [showLyrics, setShowLyrics] = useState(false);

  // Escape collapses the player (desktop).
  useEffect(() => {
    if (!expanded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [expanded, close]);

  if (!expanded || !current) return null;

  const dur = durationSec || current.duration_ms / 1000;
  const pct = dur > 0 ? (Math.min(positionSec, dur) / dur) * 100 : 0;
  // Hour-plus tracks (podcasts, long mixes) read as `H:MM:SS`, so the time
  // readouts need extra room to avoid clipping the leading hour digits.
  const longForm = dur >= 3600;

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col bg-oct-bg text-oct-text"
      style={{
        paddingTop: "env(safe-area-inset-top)",
        paddingBottom: "env(safe-area-inset-bottom)",
      }}
    >
      {/* Centered phone-width column so the player reads the same on desktop. */}
      <div className="mx-auto flex min-h-0 w-full max-w-md flex-1 flex-col">
      {/* ── header ── */}
      <div className="flex flex-none items-center justify-between px-5 pb-1.5 pt-3.5">
        <button onClick={close} title="Collapse" className="text-oct-muted hover:text-oct-text">
          <ChevronDownIcon size={20} sw={1.5} />
        </button>
        <span className="font-mono text-[10.5px] tracking-[0.18em] text-oct-subtle">
          {showLyrics ? "LYRICS" : "NOW PLAYING"}
        </span>
        <div className="flex items-center gap-3.5">
          <button
            onClick={() => {
              setShowLyrics((v) => !v);
              setShowQueue(false);
            }}
            title="Lyrics"
            className={showLyrics ? "text-oct-accent" : "text-oct-muted hover:text-oct-text"}
          >
            <LyricsGlyph size={19} />
          </button>
          <button
            onClick={() => {
              setShowQueue((v) => !v);
              setShowLyrics(false);
            }}
            title="Queue"
            className={showQueue ? "text-oct-accent" : "text-oct-muted hover:text-oct-text"}
          >
            <QueueIcon size={20} />
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col items-center px-6 pb-2">
        {showQueue ? (
          <QueueList
            queue={queue}
            currentIndex={currentIndex}
            isPlaying={isPlaying}
            onPick={(i) => playAt(i)}
          />
        ) : showLyrics ? (
          <LyricsPanel trackId={current.id} isPlaying={isPlaying} onSeek={seekTo} />
        ) : (
          <>
            {/* ── artwork ── */}
            <div
              className="mt-2 flex-none"
              style={{ width: "min(74vw, 300px)" }}
            >
              <Cover
                album={current ? { id: current.album_id } : { id: undefined }}
                tryCover
                size={300}
                radius={14}
                className="shadow-[0_22px_50px_-16px_rgba(0,0,0,0.6)]"
              />
            </div>

            {/* ── title + format readout ── */}
            <div className="mt-6 w-full text-center">
              <div className="line-clamp-2 text-[23px] font-semibold leading-tight tracking-tight">
                {current.title}
              </div>
              {trackMetaLine(meta.artistName, meta.albumTitle) && (
                <div className="mt-1.5 truncate text-sm text-oct-muted">
                  {trackMetaLine(meta.artistName, meta.albumTitle)}
                </div>
              )}
              <div className="mt-2.5 flex items-center justify-center gap-2">
                {current.downloaded && (
                  <span className="flex items-center gap-1 rounded-md bg-oct-accent/15 px-1.5 py-[3px] font-mono text-[9px] tracking-wide text-oct-accent-bright">
                    <DownloadIcon size={9} sw={1.8} />
                    SAVED
                  </span>
                )}
                <span className="rounded-md border border-oct-line px-1.5 py-[3px] font-mono text-[9.5px] text-oct-muted">
                  {qualityLabel(current)}
                </span>
              </div>
            </div>

            {/* ── visualizer ── */}
            <div className="mt-5 flex h-[22px] items-end gap-[3px]">
              {VIS_BARS.map((b, i) => (
                <span
                  key={i}
                  className="w-[3px] origin-bottom rounded-sm bg-oct-accent"
                  style={{
                    height: "100%",
                    animation: isPlaying
                      ? `eqbar ${b.dur} ease-in-out infinite ${b.delay}`
                      : "none",
                    transform: isPlaying ? undefined : "scaleY(0.25)",
                  }}
                />
              ))}
            </div>
          </>
        )}

        {/* ── scrubber ── */}
        <div className="mt-5 flex w-full items-center gap-3">
          <span className={`${longForm ? "w-16" : "w-10"} text-right font-mono text-[11px] text-oct-muted`}>
            {formatDuration(positionSec * 1000)}
          </span>
          <input
            type="range"
            min={0}
            max={dur || 0}
            step={0.1}
            value={Math.min(positionSec, dur || 0)}
            onChange={(e) => seekTo(Number(e.target.value))}
            disabled={!dur}
            aria-label="Seek"
            className="oct-range flex-1"
            style={{ background: `linear-gradient(to right, var(--color-oct-accent) ${pct}%, var(--color-oct-line) ${pct}%)` }}
          />
          <span className={`${longForm ? "w-16" : "w-10"} font-mono text-[11px] text-oct-muted`}>
            {formatDuration(dur * 1000)}
          </span>
        </div>

        {/* ── transport ── */}
        <div className="mt-6 flex items-center justify-center gap-7">
          <button
            onClick={toggleShuffle}
            title="Shuffle"
            className={shuffle ? "text-oct-accent" : "text-oct-dim hover:text-oct-text"}
          >
            <ShuffleIcon size={20} />
          </button>
          <button onClick={prev} title="Previous" className="text-oct-text hover:text-white">
            <PrevIcon size={22} />
          </button>
          <button
            onClick={togglePlay}
            title={isPlaying ? "Pause" : "Play"}
            className="grid h-16 w-16 place-items-center rounded-full bg-oct-accent text-oct-bg shadow-[0_8px_24px_-6px_rgba(224,168,75,0.45)] transition-transform hover:scale-105"
          >
            {isPlaying ? <PauseIcon size={22} /> : <PlayIcon size={22} />}
          </button>
          <button onClick={next} title="Next" className="text-oct-text hover:text-white">
            <NextIcon size={22} />
          </button>
          <button
            onClick={cycleRepeat}
            title={`Repeat: ${repeat}`}
            className={repeat !== "off" ? "text-oct-accent" : "text-oct-dim hover:text-oct-text"}
          >
            {repeat === "one" ? <RepeatOneIcon size={20} /> : <RepeatIcon size={20} />}
          </button>
        </div>

        <div className="min-h-[8px] flex-1" />

        {/* ── output / details strip ── */}
        <div className="flex w-full flex-none items-center justify-between border-t border-oct-border pt-3.5">
          <div className="flex min-w-0 items-center gap-2.5 text-oct-muted">
            <VolumeHiIcon size={16} />
            <div className="min-w-0">
              <div className="font-mono text-[9px] tracking-[0.08em] text-oct-faint">OUTPUT</div>
              <div className="mt-0.5 truncate text-xs text-oct-accent">This device</div>
            </div>
          </div>
          <div className="flex shrink-0 gap-1.5">
            <span className="rounded-md border border-oct-line px-1.5 py-1 font-mono text-[9px] text-oct-muted">
              {(current.codec || "—").toUpperCase()}
            </span>
            <span className="rounded-md border border-oct-line px-1.5 py-1 font-mono text-[9px] text-oct-muted">
              {current.downloaded ? "OFFLINE" : "STREAM"}
            </span>
          </div>
        </div>
      </div>
      </div>
    </div>
  );
}

/** Up-next list shown when the header queue icon is toggled. */
function QueueList({
  queue,
  currentIndex,
  isPlaying,
  onPick,
}: {
  queue: {
    id: string;
    title: string;
    duration_ms: number;
    downloaded: boolean;
    album_id?: string;
    artist_id?: string;
  }[];
  currentIndex: number;
  isPlaying: boolean;
  onPick: (index: number) => void;
}) {
  const trackNames = useTrackNames(queue);
  return (
    <div className="oct-scroll mt-2 w-full flex-1 overflow-y-auto">
      <div className="mb-2 font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">
        QUEUE · {queue.length}
      </div>
      <div className="flex flex-col">
        {queue.map((t, i) => {
          const active = i === currentIndex;
          const m = trackNames(t);
          const sub = trackMetaLine(m.artistName, m.albumTitle);
          return (
            <button
              key={`${t.id}-${i}`}
              onClick={() => onPick(i)}
              className={`grid grid-cols-[24px_1fr_auto] items-center gap-3 rounded-lg px-2 py-2.5 text-left text-[13.5px] ${
                active ? "bg-oct-elevated" : "hover:bg-oct-elevated/50"
              }`}
            >
              <span className="flex justify-center">
                {active ? (
                  <EqBars playing={isPlaying} />
                ) : (
                  <span className="font-mono text-xs text-oct-faint">{i + 1}</span>
                )}
              </span>
              <span className="flex min-w-0 flex-col">
                <span className="flex min-w-0 items-center gap-2">
                  <span className={`truncate ${active ? "font-medium text-oct-accent" : ""}`}>
                    {t.title}
                  </span>
                  {t.downloaded && (
                    <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-oct-accent" title="downloaded" />
                  )}
                </span>
                {sub && <span className="truncate text-[11px] text-oct-subtle">{sub}</span>}
              </span>
              <span className="font-mono text-[11px] text-oct-subtle">
                {formatDuration(t.duration_ms)}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

// Per-track lyric cache (keyed by track id) so re-opening the panel is instant
// and switching tracks doesn't refetch what we already have. Lives at module
// scope — shared across mounts, like a tiny query cache.
const lyricsCache = new Map<string, Lyrics>();
const lyricsInflight = new Map<string, Promise<Lyrics>>();

function useLyrics(trackId: string | undefined) {
  const [lyrics, setLyrics] = useState<Lyrics | null>(() =>
    trackId ? lyricsCache.get(trackId) ?? null : null,
  );
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!trackId) {
      setLyrics(null);
      return;
    }
    const cached = lyricsCache.get(trackId);
    if (cached) {
      setLyrics(cached);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLyrics(null);
    setLoading(true);
    let p = lyricsInflight.get(trackId);
    if (!p) {
      p = getLyrics(trackId);
      lyricsInflight.set(trackId, p);
    }
    p.then((l) => {
      lyricsCache.set(trackId, l);
      if (!cancelled) {
        setLyrics(l);
        setLoading(false);
      }
    })
      .catch(() => {
        if (!cancelled) {
          setLyrics(null);
          setLoading(false);
        }
      })
      .finally(() => lyricsInflight.delete(trackId));
    return () => {
      cancelled = true;
    };
  }, [trackId]);
  return { lyrics, loading };
}

/** Lyrics glyph for the header toggle (matches the design's lyrics icon). */
function LyricsGlyph({ size = 20 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M3 4h7M3 8h7M3 12h4.5" />
      <path d="M12.5 3.5v6.2" />
      <circle cx="11" cy="10.6" r="1.6" />
    </svg>
  );
}

/** Centered placeholder for the loading / instrumental / no-lyrics states. */
function LyricsMessage({ children, dots }: { children: string; dots?: boolean }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 text-oct-subtle">
      {dots && (
        <span className="flex items-center gap-2">
          {[0, 1, 2].map((d) => (
            <span
              key={d}
              className="h-2.5 w-2.5 animate-pulse rounded-full bg-oct-faint"
              style={{ animationDelay: `${d * 0.2}s` }}
            />
          ))}
        </span>
      )}
      <span className="font-mono text-xs tracking-wide">{children}</span>
    </div>
  );
}

/**
 * Auto-scrolling, tap-to-seek lyric view (design: "OCTAVE Lyrics"). Synced
 * lyrics highlight the active line (gold) and smoothly center it; tapping a
 * line seeks there. Plain lyrics render as a static block; instrumental /
 * missing show a placeholder.
 */
function LyricsPanel({
  trackId,
  isPlaying,
  onSeek,
}: {
  trackId: string;
  isPlaying: boolean;
  onSeek: (sec: number) => void;
}) {
  const { lyrics, loading } = useLyrics(trackId);
  const positionSec = usePlayerStore((s) => s.positionSec);
  const positionMs = positionSec * 1000;
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const tweenRef = useRef<number | null>(null);
  const lastIdxRef = useRef(-1);

  const lines = lyrics?.lines ?? [];
  const synced = !!lyrics?.synced;

  // Active line: last line whose timestamp is at/before the playhead.
  const active = useMemo(() => {
    if (!synced || lines.length === 0) return -1;
    let a = -1;
    let lo = 0;
    let hi = lines.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (lines[mid].ms <= positionMs) {
        a = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    return a;
  }, [synced, lines, positionMs]);

  // Reset scroll memory + cancel any tween when the track changes / unmounts.
  useEffect(() => {
    lastIdxRef.current = -1;
  }, [trackId]);
  useEffect(
    () => () => {
      if (tweenRef.current) cancelAnimationFrame(tweenRef.current);
    },
    [],
  );

  // Smoothly center the active line (420ms cubic ease-out; instant on first).
  useEffect(() => {
    if (!synced || active < 0 || active === lastIdxRef.current) return;
    const first = lastIdxRef.current < 0;
    lastIdxRef.current = active;
    const c = scrollRef.current;
    if (!c) return;
    const el = c.querySelector<HTMLElement>(`[data-line="${active}"]`);
    if (!el) return;
    const target = Math.max(
      0,
      Math.min(
        el.offsetTop - c.clientHeight / 2 + el.offsetHeight / 2,
        c.scrollHeight - c.clientHeight,
      ),
    );
    if (first) {
      c.scrollTop = target;
      return;
    }
    if (tweenRef.current) cancelAnimationFrame(tweenRef.current);
    const start = c.scrollTop;
    const delta = target - start;
    if (Math.abs(delta) < 1) return;
    const t0 = performance.now();
    const dur = 420;
    const ease = (p: number) => 1 - Math.pow(1 - p, 3);
    const step = (now: number) => {
      const p = Math.min(1, (now - t0) / dur);
      c.scrollTop = start + delta * ease(p);
      if (p < 1) tweenRef.current = requestAnimationFrame(step);
    };
    tweenRef.current = requestAnimationFrame(step);
  }, [active, synced]);

  const header = (
    <div className="flex flex-none items-center justify-between pb-3 pt-1">
      <span className="font-mono text-[10.5px] tracking-[0.2em] text-oct-muted">LYRICS</span>
      {lyrics?.found && (
        <span
          className="flex items-center gap-2 font-mono text-[9.5px] tracking-wide"
          style={{ color: synced && isPlaying ? "var(--color-oct-accent)" : "var(--color-oct-faint)" }}
        >
          <span
            className={`h-[6px] w-[6px] rounded-full ${synced && isPlaying ? "animate-pulse" : ""}`}
            style={{ background: "currentColor" }}
          />
          {synced ? (isPlaying ? "SYNCED" : "PAUSED") : "PLAIN"}
        </span>
      )}
    </div>
  );

  let body: ReactNode;
  if (loading && !lyrics) {
    body = <LyricsMessage>Loading lyrics…</LyricsMessage>;
  } else if (lyrics?.instrumental) {
    body = <LyricsMessage dots>Instrumental</LyricsMessage>;
  } else if (!lyrics || !lyrics.found) {
    body = <LyricsMessage>No lyrics available</LyricsMessage>;
  } else if (!synced) {
    body = (
      <div className="oct-scroll min-h-0 flex-1 overflow-y-auto pr-1">
        <div className="whitespace-pre-wrap py-2 text-[17px] leading-relaxed text-oct-text/80">
          {lyrics.plain}
        </div>
      </div>
    );
  } else {
    body = (
      <div className="relative min-h-0 flex-1">
        {/* top + bottom fade so lines dissolve at the edges */}
        <div className="pointer-events-none absolute inset-x-0 top-0 z-10 h-14 bg-gradient-to-b from-oct-bg to-transparent" />
        <div className="pointer-events-none absolute inset-x-0 bottom-0 z-10 h-20 bg-gradient-to-t from-oct-bg to-transparent" />
        <div ref={scrollRef} className="oct-scroll h-full overflow-y-auto pr-1">
          {/* spacers so the first/last line can rest at center */}
          <div style={{ height: "30vh" }} />
          {lines.map((l, i) => {
            const isActive = i === active;
            const dist = active >= 0 ? Math.abs(i - active) : 0;
            const opacity = isActive ? 1 : Math.max(0.22, 0.8 - 0.16 * dist);
            // A timed line with no text is an instrumental gap — pulsing dots.
            if (l.text.trim() === "") {
              return (
                <div key={i} data-line={i} className="py-[7px]">
                  <span className="inline-flex items-center gap-2 py-1">
                    {[0, 1, 2].map((d) => (
                      <span
                        key={d}
                        className="h-2 w-2 animate-pulse rounded-full"
                        style={{
                          background: isActive
                            ? "var(--color-oct-accent)"
                            : "var(--color-oct-faint)",
                          animationDelay: `${d * 0.2}s`,
                        }}
                      />
                    ))}
                  </span>
                </div>
              );
            }
            return (
              <div key={i} data-line={i} className="py-[7px]">
                <button
                  type="button"
                  onClick={() => onSeek(l.ms / 1000)}
                  className="block w-full text-left text-[22px] leading-snug tracking-tight"
                  style={{
                    color: isActive ? "var(--color-oct-accent)" : "var(--color-oct-text)",
                    opacity,
                    fontWeight: isActive ? 600 : 500,
                    transition: "color 0.45s ease, opacity 0.45s ease",
                  }}
                >
                  {l.text}
                </button>
              </div>
            );
          })}
          <div style={{ height: "40vh" }} />
        </div>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 w-full flex-1 flex-col">
      {header}
      {body}
    </div>
  );
}
