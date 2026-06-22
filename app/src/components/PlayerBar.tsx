import { useEffect, useRef } from "react";
import { usePlayerStore } from "../player/store";
import { formatDuration } from "../lib/format";
import { qualityLabel } from "../lib/visual";
import { Thumb } from "./Cover";
import { SavedPill } from "./SourceBadge";
import {
  NextIcon,
  PauseIcon,
  PlayIcon,
  PrevIcon,
  QueueIcon,
  RepeatIcon,
  RepeatOneIcon,
  ShuffleIcon,
  VolumeIcon,
} from "./icons";

/**
 * Persistent now-playing bar + hidden `<audio>` element (OCTAVE styling).
 *
 * One `<audio>` lives for the app's lifetime (mounted here, always in the
 * tree); the store owns its src/play/pause. This component renders the
 * controls (full bar on md+, condensed mini-player on mobile) and mirrors
 * state into the OS Media Session API so platform media keys / lock-screen /
 * Bluetooth controls drive the same store.
 */
export default function PlayerBar() {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const bind = usePlayerStore((s) => s._bind);

  const queue = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const positionSec = usePlayerStore((s) => s.positionSec);
  const durationSec = usePlayerStore((s) => s.durationSec);
  const volume = usePlayerStore((s) => s.volume);
  const shuffle = usePlayerStore((s) => s.shuffle);
  const repeat = usePlayerStore((s) => s.repeat);
  const error = usePlayerStore((s) => s.error);
  const togglePlay = usePlayerStore((s) => s.togglePlay);
  const next = usePlayerStore((s) => s.next);
  const prev = usePlayerStore((s) => s.prev);
  const seekTo = usePlayerStore((s) => s.seekTo);
  const setVolume = usePlayerStore((s) => s.setVolume);
  const toggleShuffle = usePlayerStore((s) => s.toggleShuffle);
  const cycleRepeat = usePlayerStore((s) => s.cycleRepeat);

  const current = currentIndex >= 0 ? queue[currentIndex] : null;

  useEffect(() => {
    if (!audioRef.current) return;
    return bind(audioRef.current);
  }, [bind]);

  useEffect(() => {
    if (!("mediaSession" in navigator) || !current) return;
    navigator.mediaSession.metadata = new MediaMetadata({
      title: current.title,
      artist: "",
      album: "",
    });
    navigator.mediaSession.setActionHandler("play", () => togglePlay());
    navigator.mediaSession.setActionHandler("pause", () => togglePlay());
    navigator.mediaSession.setActionHandler("nexttrack", () => next());
    navigator.mediaSession.setActionHandler("previoustrack", () => prev());
    navigator.mediaSession.setActionHandler("seekto", (d) => {
      if (typeof d.seekTime === "number") seekTo(d.seekTime);
    });
  }, [current, togglePlay, next, prev, seekTo]);

  useEffect(() => {
    if ("mediaSession" in navigator) {
      navigator.mediaSession.playbackState = isPlaying ? "playing" : "paused";
    }
  }, [isPlaying]);

  const dur = durationSec || (current ? current.duration_ms / 1000 : 0);
  const empty = queue.length === 0 && !current;
  const pct = dur > 0 ? (Math.min(positionSec, dur) / dur) * 100 : 0;

  // One persistent <audio>, mounted once at a stable position regardless of
  // `empty`, so a re-render never removes it from the document. It used to be
  // rendered in two separate return branches (bare <audio> when empty vs.
  // <div><audio>…</div> when playing); the empty → playing transition that
  // happens when you start an album made React tear the element down and build
  // a new one mid-play, which on Chromium rejects the pending play() with
  // "AbortError: … media was removed from the document". Only the visible
  // chrome toggles on whether something is queued.
  return (
    <>
      <audio ref={audioRef} preload="auto" className="hidden" />
      {!empty && (
    <div className="shrink-0 border-t border-oct-border bg-oct-surface">
      {error && (
        <p className="border-b border-oct-offline/40 bg-oct-offline/15 px-4 py-1 text-center text-xs text-oct-danger">
          {error}
        </p>
      )}

      {/* mobile progress hairline */}
      <div className="h-[2px] bg-oct-line md:hidden">
        <div className="h-full bg-oct-accent" style={{ width: `${pct}%` }} />
      </div>

      {/* ───────── desktop / wide bar ───────── */}
      <div className="hidden h-[90px] grid-cols-[1fr_auto_1fr] items-center gap-6 px-5 md:grid">
        {/* now-playing */}
        <div className="flex min-w-0 items-center gap-3">
          <Thumb album={current ? { id: current.album_id } : null} size={52} tryCover />
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="truncate text-[13.5px] font-medium">{current?.title ?? "—"}</span>
              {current?.downloaded && <SavedPill />}
            </div>
            <div className="mt-0.5 truncate font-mono text-[11px] text-oct-subtle">
              {current ? qualityLabel(current) : ""}
            </div>
          </div>
        </div>

        {/* transport + progress */}
        <div className="flex w-[440px] flex-col items-center gap-2">
          <div className="flex items-center gap-6 text-oct-text">
            <button
              onClick={toggleShuffle}
              title="Shuffle"
              className={shuffle ? "text-oct-accent" : "text-oct-dim hover:text-oct-text"}
            >
              <ShuffleIcon size={16} />
            </button>
            <button onClick={prev} title="Previous" className="text-oct-text hover:text-white">
              <PrevIcon size={17} />
            </button>
            <button
              onClick={togglePlay}
              title={isPlaying ? "Pause" : "Play"}
              className="grid h-[42px] w-[42px] place-items-center rounded-full bg-oct-text text-oct-bg transition-transform hover:scale-105"
            >
              {isPlaying ? <PauseIcon size={15} /> : <PlayIcon size={15} />}
            </button>
            <button onClick={next} title="Next" className="text-oct-text hover:text-white">
              <NextIcon size={17} />
            </button>
            <button
              onClick={cycleRepeat}
              title={`Repeat: ${repeat}`}
              className={repeat !== "off" ? "text-oct-accent" : "text-oct-dim hover:text-oct-text"}
            >
              {repeat === "one" ? <RepeatOneIcon size={16} /> : <RepeatIcon size={16} />}
            </button>
          </div>
          <div className="flex w-full items-center gap-3">
            <span className="w-9 text-right font-mono text-[11px] text-oct-subtle">
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
              className="oct-range flex-1"
            />
            <span className="w-9 font-mono text-[11px] text-oct-subtle">
              {formatDuration(dur * 1000)}
            </span>
          </div>
        </div>

        {/* meta + volume */}
        <div className="flex items-center justify-end gap-4 text-oct-dim">
          {current && (
            <span className="rounded-md border border-oct-line px-2 py-1 font-mono text-[10.5px] tracking-wide text-oct-muted">
              {qualityLabel(current)}
              {current.downloaded ? " · OFFLINE" : " · STREAM"}
            </span>
          )}
          <span className="font-mono text-[10.5px] text-oct-faint">
            {currentIndex + 1}/{queue.length}
          </span>
          <span title="Queue" className="text-oct-dim">
            <QueueIcon size={16} />
          </span>
          <div className="flex items-center gap-2">
            <VolumeIcon size={16} />
            <input
              type="range"
              min={0}
              max={1}
              step={0.01}
              value={volume}
              onChange={(e) => setVolume(Number(e.target.value))}
              title="Volume"
              className="oct-range w-[72px]"
            />
          </div>
        </div>
      </div>

      {/* ───────── mobile mini-player ───────── */}
      <div className="flex items-center gap-3 px-3.5 py-2 md:hidden">
        <Thumb album={current ? { id: current.album_id } : null} size={44} tryCover />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <span className="truncate text-[13px] font-medium">{current?.title ?? "—"}</span>
            {current?.downloaded && <SavedPill />}
          </div>
          <div className="mt-0.5 truncate font-mono text-[11px] text-oct-subtle">
            {current ? qualityLabel(current) : ""}
          </div>
        </div>
        <button onClick={prev} title="Previous" className="text-oct-text">
          <PrevIcon size={18} />
        </button>
        <button
          onClick={togglePlay}
          title={isPlaying ? "Pause" : "Play"}
          className="grid h-10 w-10 shrink-0 place-items-center rounded-full bg-oct-accent text-oct-bg"
        >
          {isPlaying ? <PauseIcon size={15} /> : <PlayIcon size={15} />}
        </button>
        <button onClick={next} title="Next" className="text-oct-text">
          <NextIcon size={18} />
        </button>
      </div>
    </div>
      )}
    </>
  );
}
