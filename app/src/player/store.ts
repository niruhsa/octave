// Playback state (Phase 4).
//
// Owns the queue, current index, shuffle/repeat, and the `<audio>` element
// ref. Keeps a single HTMLAudioElement alive for the app's lifetime so
// track switches don't tear down/recreate the pipeline (better for
// gapless-ish transitions and for OS media-session integration).
//
// Source resolution lives in Rust (`media://` protocol): we just ask for
// the URL per track id and hand it to the element. The protocol serves the
// local file when downloaded, else proxies the server stream with auth.

import { create } from "zustand";
import type { MergedTrack } from "../ipc";
import { playerMediaUrl } from "../ipc";

export type RepeatMode = "off" | "all" | "one";

export type PlayerState = {
  /** Ordered queue (post-shuffle ordering applied when shuffle is on). */
  queue: MergedTrack[];
  /** Index into `queue` of the current track. -1 when empty. */
  currentIndex: number;
  isPlaying: boolean;
  /** Current playback position (s) and duration (s), updated on timeupdate. */
  positionSec: number;
  durationSec: number;
  /** Volume 0..1. */
  volume: number;
  shuffle: boolean;
  repeat: RepeatMode;
  /** Human-readable error from the last `<audio>` error event, if any. */
  error: string | null;

  // internal: the live HTMLAudioElement, kept out of the serialized state.
  audio: HTMLAudioElement | null;

  // actions
  playTrack: (track: MergedTrack, queue?: MergedTrack[]) => void;
  playQueue: (tracks: MergedTrack[], startIndex?: number) => void;
  togglePlay: () => void;
  next: () => void;
  prev: () => void;
  seekTo: (sec: number) => void;
  setVolume: (v: number) => void;
  toggleShuffle: () => void;
  cycleRepeat: () => void;
  clearQueue: () => void;
  /** Called by the `<audio>` event listeners — not for UI use. */
  _bind: (el: HTMLAudioElement) => () => void;
};

export const usePlayerStore = create<PlayerState>((set, get) => ({
  queue: [],
  currentIndex: -1,
  isPlaying: false,
  positionSec: 0,
  durationSec: 0,
  volume: 1,
  shuffle: false,
  repeat: "off",
  error: null,
  audio: null,

  playTrack: (track, queue) => {
    const q = queue && queue.length ? queue : [track];
    const idx = q.findIndex((t) => t.id === track.id);
    const start = idx >= 0 ? idx : 0;
    get().playQueue(q, start);
  },

  playQueue: (tracks, startIndex = 0) => {
    const shuffle = get().shuffle;
    const q = applyShuffle(tracks, shuffle, startIndex);
    // `applyShuffle` pins the chosen track at position 0 when shuffling,
    // so the play index is 0 in that case; otherwise it's the raw index.
    const playIndex = shuffle ? 0 : Math.max(0, Math.min(startIndex, q.length - 1));
    set({ queue: q, error: null });
    loadAndPlay(get, set, playIndex);
  },

  togglePlay: () => {
    const { audio, isPlaying } = get();
    if (!audio) return;
    if (isPlaying) {
      audio.pause();
    } else {
      void audio.play().catch((e) => reportPlayError(set, e));
    }
  },

  next: () => {
    const { queue, currentIndex, repeat } = get();
    if (queue.length === 0) return;
    if (repeat === "one") {
      // Replay the same track.
      loadAndPlay(get, set, currentIndex);
      return;
    }
    let nextIdx = currentIndex + 1;
    if (nextIdx >= queue.length) {
      if (repeat === "all") nextIdx = 0;
      else {
        // End of queue, no repeat → stop.
        const audio = get().audio;
        if (audio) audio.pause();
        set({ isPlaying: false });
        return;
      }
    }
    loadAndPlay(get, set, nextIdx);
  },

  prev: () => {
    const { queue, currentIndex, audio } = get();
    if (queue.length === 0) return;
    // Standard media-key semantics: if >3s in, restart current; else prev.
    if (audio && audio.currentTime > 3) {
      audio.currentTime = 0;
      return;
    }
    let prevIdx = currentIndex - 1;
    if (prevIdx < 0) prevIdx = get().repeat === "all" ? queue.length - 1 : 0;
    loadAndPlay(get, set, prevIdx);
  },

  seekTo: (sec) => {
    const audio = get().audio;
    if (audio) audio.currentTime = sec;
  },

  setVolume: (v) => {
    const clamped = Math.max(0, Math.min(1, v));
    const audio = get().audio;
    if (audio) audio.volume = clamped;
    set({ volume: clamped });
  },

  toggleShuffle: () => {
    const { queue, currentIndex, shuffle } = get();
    const nowShuffling = !shuffle;
    // Keep the current track playing; reshuffle the rest around it.
    const current = queue[currentIndex];
    const newQueue = applyShuffle(queue, nowShuffling, currentIndex);
    // `applyShuffle` puts the start track first; we want it at its index.
    if (current && nowShuffling) {
      const i = newQueue.findIndex((t) => t.id === current.id);
      if (i >= 0 && i !== currentIndex) {
        // Re-derive so currentIndex still points at `current`.
        set({ shuffle: nowShuffling, queue: newQueue, currentIndex: i });
        return;
      }
    }
    set({ shuffle: nowShuffling });
  },

  cycleRepeat: () => {
    const next: Record<RepeatMode, RepeatMode> = {
      off: "all",
      all: "one",
      one: "off",
    };
    set({ repeat: next[get().repeat] });
  },

  clearQueue: () => {
    const audio = get().audio;
    if (audio) {
      audio.pause();
      audio.removeAttribute("src");
      audio.load();
    }
    set({ queue: [], currentIndex: -1, isPlaying: false, positionSec: 0, durationSec: 0 });
  },

  _bind: (el) => {
    // Wire `<audio>` events into store state. Returns an unbind fn.
    set({ audio: el, volume: el.volume });
    const onPlay = () => set({ isPlaying: true });
    const onPause = () => set({ isPlaying: false });
    const onTime = () =>
      set({ positionSec: el.currentTime, durationSec: el.duration || 0 });
    const onDuration = () => set({ durationSec: el.duration || 0 });
    const onEnded = () => get().next();
    const onErr = () => {
      const code = el.error?.code;
      const msg =
        code === 4
          ? "track not available (offline and not downloaded?)"
          : `audio error (code ${code})`;
      set({ error: msg, isPlaying: false });
    };
    el.addEventListener("play", onPlay);
    el.addEventListener("pause", onPause);
    el.addEventListener("timeupdate", onTime);
    el.addEventListener("durationchange", onDuration);
    el.addEventListener("ended", onEnded);
    el.addEventListener("error", onErr);
    return () => {
      el.removeEventListener("play", onPlay);
      el.removeEventListener("pause", onPause);
      el.removeEventListener("timeupdate", onTime);
      el.removeEventListener("durationchange", onDuration);
      el.removeEventListener("ended", onEnded);
      el.removeEventListener("error", onErr);
    };
  },
}));

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

type Get = () => PlayerState;
type Set = (partial: Partial<PlayerState>) => void;

/** Load the track at `index`, set it as src, and start playback. */
function loadAndPlay(_get: Get, set: Set, index: number) {
  const { audio, queue } = _get();
  const track = queue[index];
  if (!track || !audio) return;
  set({ currentIndex: index, positionSec: 0, durationSec: 0, error: null });
  playerMediaUrl(track.id)
    .then((url: string) => {
      // Guard against races: a rapid skip could have moved currentIndex.
      if (_get().queue[_get().currentIndex]?.id !== track.id) return;
      audio.src = url;
      audio.currentTime = 0;
      void audio.play().catch((e: unknown) => reportPlayError(set, e));
    })
    .catch((e) => set({ error: formatErr(e) }));
}

/**
 * Return the queue in play order. When `shuffle` is true we Fisher–Yates
 * the tracks but pin the `startIndex` track at position 0 so the
 * user-pressed track plays immediately.
 */
function applyShuffle(
  tracks: MergedTrack[],
  shuffle: boolean,
  startIndex: number,
): MergedTrack[] {
  if (tracks.length === 0) return tracks;
  if (!shuffle) return [...tracks];
  const pinned = tracks[startIndex];
  const rest = tracks.filter((_, i) => i !== startIndex);
  // Fisher–Yates.
  for (let i = rest.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [rest[i], rest[j]] = [rest[j], rest[i]];
  }
  return pinned ? [pinned, ...rest] : rest;
}

/**
 * Surface a play() failure — but swallow `AbortError`, which the browser
 * raises when a pending play() is superseded (rapid skip, a new src set on the
 * element, etc.). That's expected churn, not something to show the user; the
 * follow-up load/play resolves the real state.
 */
function reportPlayError(set: Set, e: unknown) {
  if (e instanceof DOMException && e.name === "AbortError") return;
  set({ error: formatErr(e) });
}

function formatErr(e: unknown): string {
  if (typeof e === "object" && e !== null) {
    const obj = e as { message?: string };
    if (obj.message) return obj.message;
  }
  return String(e);
}
