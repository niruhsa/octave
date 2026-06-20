import { useEffect, useRef } from "react";
import { usePlayerStore } from "../player/store";
import { formatDuration } from "../lib/format";
import { coverUrl } from "../ipc";

/**
 * Persistent playback bar + hidden `<audio>` element.
 *
 * One `<audio>` lives for the app's lifetime (mounted here, always in the
 * tree). The store owns its `src`/play/pause; this component only renders
 * the controls and mirrors state into the OS Media Session API
 * (`navigator.mediaSession`) so platform media keys / lock-screen /
 * Bluetooth controls drive the same store.
 *
 * Desktop-native integration (SMTC on Windows, MPRIS on Linux, macOS Now
 * Playing) is layered on top of Media Session in later polish — the
 * webview's Media Session hooks into those on most platforms already.
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

  // Bind the audio element once.
  useEffect(() => {
    if (!audioRef.current) return;
    const unbind = bind(audioRef.current);
    return unbind;
  }, [bind]);

  // Mirror current track into the OS Media Session.
  useEffect(() => {
    if (!("mediaSession" in navigator) || !current) return;
    navigator.mediaSession.metadata = new MediaMetadata({
      title: current.title,
      artist: "", // enriched later when album/artist browse is cached
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

  // Reflect play state into Media Session (drives lock-screen "playing").
  useEffect(() => {
    if ("mediaSession" in navigator) {
      navigator.mediaSession.playbackState = isPlaying ? "playing" : "paused";
    }
  }, [isPlaying]);

  const dur = durationSec || (current ? current.duration_ms / 1000 : 0);
  const empty = queue.length === 0 && !current;

  // The `<audio>` element is mounted exactly ONCE and never conditionally
  // unmounted — the store binds to it on mount (before any track is
  // queued), so `loadAndPlay` always has a live element. We only toggle
  // the *visible* bar chrome based on whether anything is queued.
  return (
    <>
      <audio ref={audioRef} preload="auto" className="hidden" />
      {empty ? null : (
        <div className="fixed inset-x-0 bottom-0 z-50 border-t border-neutral-800 bg-neutral-950/95 backdrop-blur">
          {error && (
            <p className="border-b border-red-900/50 bg-red-950/40 px-4 py-1 text-xs text-red-200">
              {error}
            </p>
          )}
          <div className="mx-auto flex max-w-6xl items-center gap-4 px-4 py-2">
        {/* Now-playing */}
        <div className="flex min-w-0 flex-1 items-center gap-3">
          {current?.downloaded ? (
            <img
              src={coverUrl(current.album_id)}
              alt="cover"
              className="h-10 w-10 shrink-0 rounded bg-neutral-800 object-cover"
              onError={(e) => {
                (e.currentTarget as HTMLImageElement).style.visibility = "hidden";
              }}
            />
          ) : (
            <div className="h-10 w-10 shrink-0 rounded bg-neutral-800" />
          )}
          <div className="min-w-0">
            <p className="truncate text-sm font-medium">
              {current?.title ?? "—"}
            </p>
            <p className="truncate text-xs text-neutral-500">
              {current?.codec ?? ""}
              {current?.bitrate_kbps ? ` · ${current.bitrate_kbps} kbps` : ""}
              {current?.downloaded ? " · offline" : " · streaming"}
            </p>
          </div>
        </div>

        {/* Transport */}
        <div className="flex flex-col items-center gap-1">
          <div className="flex items-center gap-2">
            <button
              onClick={toggleShuffle}
              className={`rounded px-1.5 py-1 text-sm ${shuffle ? "text-blue-400" : "text-neutral-400 hover:text-neutral-200"}`}
              title="Shuffle"
            >
              ⤮
            </button>
            <button
              onClick={prev}
              className="rounded px-1.5 py-1 text-neutral-300 hover:text-white"
              title="Previous"
            >
              ⏮
            </button>
            <button
              onClick={togglePlay}
              className="rounded-full bg-white px-3 py-1 text-sm text-black hover:bg-neutral-200"
              title={isPlaying ? "Pause" : "Play"}
            >
              {isPlaying ? "⏸" : "▶"}
            </button>
            <button
              onClick={next}
              className="rounded px-1.5 py-1 text-neutral-300 hover:text-white"
              title="Next"
            >
              ⏭
            </button>
            <button
              onClick={cycleRepeat}
              className={`rounded px-1.5 py-1 text-sm ${repeat !== "off" ? "text-blue-400" : "text-neutral-400 hover:text-neutral-200"}`}
              title={`Repeat: ${repeat}`}
            >
              {repeat === "one" ? "🔁¹" : "🔁"}
            </button>
          </div>
          <div className="flex items-center gap-2 text-xs tabular-nums text-neutral-500">
            <span className="w-10 text-right">{formatDuration(positionSec * 1000)}</span>
            <input
              type="range"
              min={0}
              max={dur || 0}
              step={0.1}
              value={Math.min(positionSec, dur || 0)}
              onChange={(e) => seekTo(Number(e.target.value))}
              className="w-64 accent-blue-500"
              disabled={!dur}
            />
            <span className="w-10">{formatDuration(dur * 1000)}</span>
          </div>
        </div>

        {/* Volume + queue count */}
        <div className="flex flex-1 items-center justify-end gap-3">
          <span className="text-xs text-neutral-500">
            {currentIndex + 1}/{queue.length}
          </span>
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={volume}
            onChange={(e) => setVolume(Number(e.target.value))}
            className="w-24 accent-blue-500"
            title="Volume"
          />
          </div>
          </div>
        </div>
      )}
    </>
  );
}
