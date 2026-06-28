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
import type { MergedEpisode, MergedTrack } from "../ipc";
import { playerMediaUrl, playerPrefetch, podcastRecordProgress } from "../ipc";

export type RepeatMode = "off" | "all" | "one";

/**
 * A queue item is a `MergedTrack`, optionally flavored as a podcast episode.
 * `mediaKind: "episode"` routes the source URL through the episode endpoint;
 * `streamUrl`, when set, is used directly as the `<audio>` src (a non-cached
 * episode's origin enclosure URL). Both optional → a plain track is unchanged.
 */
export type QueueItem = MergedTrack & {
  mediaKind?: "track" | "episode";
  streamUrl?: string | null;
  /** Episodes only: position (ms) to resume from on first load. 0 = from start. */
  resumeMs?: number;
};

/**
 * Adapt a podcast episode into a `QueueItem` the player can queue. A downloaded
 * or server-cached episode routes through the loopback media server; otherwise
 * it streams from its origin enclosure URL directly. `podcast_id` stands in for
 * the album/artist refs so now-playing metadata lookups have something to key on.
 */
export function episodeToQueueItem(ep: MergedEpisode): QueueItem {
  const useLoopback = ep.downloaded || ep.server_downloaded;
  return {
    id: ep.id,
    album_id: ep.podcast_id,
    artist_id: ep.podcast_id,
    title: ep.title,
    track_no: ep.episode_no,
    disc_no: null,
    duration_ms: ep.duration_ms ?? 0,
    codec: ep.codec ?? "",
    bitrate_kbps: ep.bitrate_kbps,
    file_path: "",
    file_size: ep.file_size,
    sample_rate_hz: null,
    bit_depth: null,
    channels: null,
    local_file_path: ep.local_file_path,
    is_single_release: false,
    downloaded: ep.downloaded,
    mediaKind: "episode",
    streamUrl: useLoopback ? null : ep.enclosure_url,
    // Resume an in-progress episode where the listener left off; a completed
    // one starts fresh from 0.
    resumeMs: ep.completed ? 0 : ep.position_ms ?? 0,
  };
}

export type PlayerState = {
  /** Ordered queue (post-shuffle ordering applied when shuffle is on). */
  queue: QueueItem[];
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
  playTrack: (track: QueueItem, queue?: QueueItem[]) => void;
  playQueue: (tracks: QueueItem[], startIndex?: number) => void;
  /** Play the existing queue at `index` (the now-playing queue list taps). */
  playAt: (index: number) => void;
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

// ---------------------------------------------------------------------------
// persistence
//
// The store is otherwise in-memory. On mobile the OS routinely kills the
// backgrounded app (screen off), which would drop the queue and dump the user
// back on the home screen on relaunch. We mirror the durable slice of playback
// state to localStorage (disk-backed, survives a process kill), flush it the
// moment the app is backgrounded, and seed the store from it on the next
// launch — restored paused, ready to resume.
// ---------------------------------------------------------------------------

const STORAGE_KEY = "octave:player";

type PersistShape = {
  queue: QueueItem[];
  currentIndex: number;
  shuffle: boolean;
  repeat: RepeatMode;
  volume: number;
  positionSec: number;
};

function loadPersisted(): PersistShape {
  const empty: PersistShape = {
    queue: [],
    currentIndex: -1,
    shuffle: false,
    repeat: "off",
    volume: 1,
    positionSec: 0,
  };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return empty;
    const p = JSON.parse(raw) as Partial<PersistShape>;
    if (!Array.isArray(p.queue)) return empty;
    const queue = p.queue;
    // Clamp the index into the restored queue so a corrupt value can't point
    // past the end.
    const idx =
      queue.length > 0 && typeof p.currentIndex === "number"
        ? Math.max(0, Math.min(p.currentIndex, queue.length - 1))
        : -1;
    return {
      queue,
      currentIndex: idx,
      shuffle: !!p.shuffle,
      repeat: p.repeat === "all" || p.repeat === "one" ? p.repeat : "off",
      volume: typeof p.volume === "number" ? Math.max(0, Math.min(1, p.volume)) : 1,
      positionSec:
        typeof p.positionSec === "number" && idx >= 0 ? Math.max(0, p.positionSec) : 0,
    };
  } catch {
    return empty;
  }
}

const restored = loadPersisted();

export const usePlayerStore = create<PlayerState>((set, get) => ({
  queue: restored.queue,
  currentIndex: restored.currentIndex,
  isPlaying: false,
  positionSec: restored.positionSec,
  durationSec: 0,
  volume: restored.volume,
  shuffle: restored.shuffle,
  repeat: restored.repeat,
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

  playAt: (index) => {
    const { queue } = get();
    if (index < 0 || index >= queue.length) return;
    set({ error: null });
    loadAndPlay(get, set, index);
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
        prefetchNext(get); // upcoming track changed — re-prime the look-ahead
        return;
      }
    }
    set({ shuffle: nowShuffling });
    prefetchNext(get);
  },

  cycleRepeat: () => {
    const next: Record<RepeatMode, RepeatMode> = {
      off: "all",
      all: "one",
      one: "off",
    };
    set({ repeat: next[get().repeat] });
    prefetchNext(get); // what plays next may have changed (e.g. repeat-one)
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
    set({ audio: el });
    el.volume = get().volume; // apply restored (or default) volume to the element
    primeRestored(get, el); // load a restored track at its saved position, paused
    const onPlay = () => set({ isPlaying: true });
    const onPause = () => {
      // A track reaching its end fires `pause` right before `ended`. Ignore that
      // one: next() is about to load the next track, and flipping isPlaying=false
      // here makes the native MediaService drop its wake/WiFi lock + foreground
      // status — so Android kills the screen-off process in the ~20ms gap before
      // the next track starts. Genuine pauses are unaffected: a user tap pauses
      // mid-track (not ended), and end-of-queue sets isPlaying=false in next().
      if (el.ended || (el.duration > 0 && el.currentTime >= el.duration - 0.5)) return;
      set({ isPlaying: false });
      // A genuine pause is a good moment to checkpoint episode progress.
      const s = get();
      recordEpisodeProgress(s.queue[s.currentIndex], el.currentTime, el.duration || 0);
    };
    const onTime = () => {
      set({ positionSec: el.currentTime, durationSec: el.duration || 0 });
      const s = get();
      maybeRecordProgress(s.queue[s.currentIndex], el.currentTime, el.duration || 0);
    };
    const onDuration = () => set({ durationSec: el.duration || 0 });
    const onEnded = () => {
      // Mark the finished episode listened before advancing.
      const s = get();
      recordEpisodeProgress(s.queue[s.currentIndex], el.currentTime, el.duration || 0, true);
      get().next();
    };
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
// persistence wiring
// ---------------------------------------------------------------------------

let _persistTimer: ReturnType<typeof setTimeout> | undefined;

function writePersisted() {
  try {
    const s = usePlayerStore.getState();
    const snap: PersistShape = {
      queue: s.queue,
      currentIndex: s.currentIndex,
      shuffle: s.shuffle,
      repeat: s.repeat,
      volume: s.volume,
      // `positionSec` already tracks the element clock via timeupdate/seeked
      // events. Don't read `audio.currentTime` directly — during the post-launch
      // prime window it's still 0 (not yet seeked), which would clobber the
      // restored position with 0.
      positionSec: s.positionSec,
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(snap));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

/** Coalesce the ~4 Hz `timeupdate` churn into at most one write per second. */
function schedulePersist() {
  if (_persistTimer != null) return;
  _persistTimer = setTimeout(() => {
    _persistTimer = undefined;
    writePersisted();
  }, 1000);
}

function flushPersisted() {
  if (_persistTimer != null) {
    clearTimeout(_persistTimer);
    _persistTimer = undefined;
  }
  writePersisted();
}

usePlayerStore.subscribe(schedulePersist);

if (typeof window !== "undefined") {
  // Backgrounding / screen-off is exactly when the OS is about to kill us —
  // flush the latest position synchronously before it does. `pagehide` is the
  // reliable terminal event in WebViews (`beforeunload` often doesn't fire).
  window.addEventListener("pagehide", flushPersisted);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flushPersisted();
  });
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

type Get = () => PlayerState;
type Set = (partial: Partial<PlayerState>) => void;

// ---------------------------------------------------------------------------
// podcast progress recording
//
// An episode's playback position is persisted server-side (and to the local
// cache) so "continue where you left off" survives across devices and relaunches.
// We record on a throttle during playback plus at the meaningful edges (pause,
// end, switching away). An episode counts as completed once the position is
// within COMPLETE_TAIL_SEC of the end (or it fires `ended`).
// ---------------------------------------------------------------------------

const COMPLETE_TAIL_SEC = 15;
const PROGRESS_THROTTLE_MS = 10_000;
let _lastProgressAt = 0;

/** Persist one episode's progress (no-op for plain tracks). */
function recordEpisodeProgress(
  item: QueueItem | undefined,
  positionSec: number,
  durationSec: number,
  forceCompleted = false,
) {
  if (!item || item.mediaKind !== "episode") return;
  const posMs = Math.max(0, Math.round(positionSec * 1000));
  const durMs =
    durationSec > 0 ? Math.round(durationSec * 1000) : item.duration_ms ?? 0;
  const completed =
    forceCompleted || (durMs > 0 && posMs >= durMs - COMPLETE_TAIL_SEC * 1000);
  // A completed episode pins the position at the end so the bar reads "done".
  const finalMs = completed && durMs > 0 ? durMs : posMs;
  _lastProgressAt = Date.now();
  void podcastRecordProgress(item.id, finalMs, completed).catch(() => {});
}

/** Throttled record from the `timeupdate` firehose (~4 Hz). */
function maybeRecordProgress(item: QueueItem | undefined, posSec: number, durSec: number) {
  if (!item || item.mediaKind !== "episode") return;
  if (Date.now() - _lastProgressAt < PROGRESS_THROTTLE_MS) return;
  recordEpisodeProgress(item, posSec, durSec);
}

/** Seek the element to `sec` once it has enough metadata to honor it. */
function seekOnReady(el: HTMLAudioElement, sec: number) {
  if (sec <= 0) return;
  const apply = () => {
    try {
      el.currentTime = sec;
    } catch {
      /* metadata not ready — leave at 0 */
    }
  };
  if (el.readyState >= 1) {
    apply();
  } else {
    const once = () => {
      el.removeEventListener("loadedmetadata", once);
      apply();
    };
    el.addEventListener("loadedmetadata", once);
  }
}

/**
 * On a cold launch with a restored queue, load the current track's source and
 * seek to the saved position — but stay paused. Autoplay is blocked without a
 * user gesture on a fresh load anyway; the user (or the media-notification play
 * button) resumes from exactly where the OS killed us.
 */
function primeRestored(get: Get, el: HTMLAudioElement) {
  const { queue, currentIndex, positionSec, isPlaying } = get();
  const track = queue[currentIndex];
  if (!track || el.src || isPlaying) return;
  const wantPos = positionSec;
  resolveSrc(track)
    .then((url: string) => {
      const st = get();
      // Bail if playback started or the track changed while we resolved the URL.
      if (st.isPlaying || el.src || st.queue[st.currentIndex]?.id !== track.id) return;
      el.src = url;
      if (wantPos > 0) {
        const seek = () => {
          el.removeEventListener("loadedmetadata", seek);
          try {
            el.currentTime = wantPos;
          } catch {
            /* metadata not ready — leave at 0 */
          }
        };
        el.addEventListener("loadedmetadata", seek);
      }
    })
    .catch(() => {
      /* offline / not downloaded — leave it for the user to retry */
    });
}

/** Load the track at `index`, set it as src, and start playback. */
function loadAndPlay(_get: Get, set: Set, index: number) {
  const { audio, queue, currentIndex } = _get();
  const track = queue[index];
  if (!track || !audio) return;
  // Checkpoint the outgoing episode (if any) before we switch away from it, so
  // its resume position is saved even on a manual skip.
  const outgoing = queue[currentIndex];
  if (outgoing && outgoing.id !== track.id) {
    recordEpisodeProgress(outgoing, audio.currentTime, audio.duration || 0);
  }
  set({ currentIndex: index, positionSec: 0, durationSec: 0, error: null });
  // Kick off the next track's prefetch in parallel with this one's load, so it's
  // a local file by the time we reach it (screen-off auto-advance — see below).
  prefetchNext(_get);
  resolveSrc(track)
    .then((url: string) => {
      // Guard against races: a rapid skip could have moved currentIndex.
      if (_get().queue[_get().currentIndex]?.id !== track.id) return;
      audio.src = url;
      audio.currentTime = 0;
      // Resume an in-progress episode where the listener left off.
      if (track.mediaKind === "episode" && track.resumeMs && track.resumeMs > 0) {
        seekOnReady(audio, track.resumeMs / 1000);
      }
      void audio.play().catch((e: unknown) => reportPlayError(set, e));
    })
    .catch((e) => set({ error: formatErr(e) }));
}

/**
 * Resolve the `<audio>` src for a queue item. A non-cached podcast episode
 * carries its origin `streamUrl` (played directly); everything else routes
 * through the loopback media server (`mediaKind` selects track vs episode).
 */
function resolveSrc(item: QueueItem): Promise<string> {
  if (item.streamUrl != null && item.streamUrl !== "") {
    return Promise.resolve(item.streamUrl);
  }
  return playerMediaUrl(item.id, item.mediaKind === "episode" ? "episode" : undefined);
}

/**
 * Fire-and-forget prefetch of the track that will play after the current one,
 * so a *streamed* queue can advance with the screen off. A hidden WebView won't
 * start a network media load, but it will load a local file — and Rust fetches
 * the next track to disk while this one plays (see `playerPrefetch`). Idempotent
 * on the Rust side, so redundant calls are cheap no-ops. For repeat-one we
 * prefetch the current track, since it reloads at `ended` (also screen-off).
 */
function prefetchNext(_get: Get) {
  const { queue, currentIndex, repeat } = _get();
  if (currentIndex < 0 || queue.length === 0) return;
  let n: number;
  if (repeat === "one") {
    n = currentIndex;
  } else {
    n = currentIndex + 1;
    if (n >= queue.length) n = repeat === "all" ? 0 : -1;
  }
  if (n < 0) return;
  const next = queue[n];
  // Prefetch is track-specific (it pulls `/tracks/{id}/stream`); skip episodes.
  if (next && next.mediaKind !== "episode") void playerPrefetch(next.id).catch(() => {});
}

/**
 * Return the queue in play order. When `shuffle` is true we Fisher–Yates
 * the tracks but pin the `startIndex` track at position 0 so the
 * user-pressed track plays immediately.
 */
function applyShuffle<T extends MergedTrack>(
  tracks: T[],
  shuffle: boolean,
  startIndex: number,
): T[] {
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
