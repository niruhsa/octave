// Playback state (Phase 4).
//
// Owns the queue, current index, shuffle/repeat, and the playback *deck* —
// two persistent HTMLAudioElements with swappable active/standby roles (see
// `player/deck.ts` + GAPLESS_CROSSFADE.md). The active element drives store
// state; the standby preloads the upcoming track so boundaries are gapless
// (or crossfaded, per Settings → Player). Both elements live for the app's
// lifetime so track switches never tear down the pipeline.
//
// Source resolution lives in Rust (loopback media server): we just ask for
// the URL per track id and hand it to the deck. The loopback serves the
// local file when downloaded (or prefetched), else proxies the server
// stream with auth.

import { create } from "zustand";
import type { FavoriteTrack, MergedEpisode, MergedTrack } from "../ipc";
import {
  onPrefetchReady,
  playerMediaUrl,
  playerPrefetch,
  playerPrefetchIsReady,
  playHistoryFlush,
  playHistoryRecord,
  podcastRecordProgress,
} from "../ipc";
import { MANUAL_FADE_MAX_SEC, Deck, type DeckCallbacks } from "./deck";
import { playbackPrefs } from "../settings/playback";
import { useAppStore } from "../store";

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
    is_explicit: false,
    // Episodes aren't loudness-analyzed → no gain applied.
    loudness_lufs: null,
    loudness_peak: null,
    album_loudness_lufs: null,
    aliases: [],
    downloaded: ep.downloaded,
    mediaKind: "episode",
    streamUrl: useLoopback ? null : ep.enclosure_url,
    // Resume an in-progress episode where the listener left off; a completed
    // one starts fresh from 0.
    resumeMs: ep.completed ? 0 : ep.position_ms ?? 0,
  };
}

/**
 * Adapt a *server* track (the favorites / discover shape, which lacks the cache
 * `downloaded` / `local_file_path` fields) into a `QueueItem`. Playback resolves
 * local-or-stream in Rust, so `downloaded: false` still plays when online.
 */
export function serverTrackToQueueItem(t: FavoriteTrack): QueueItem {
  return {
    id: t.id,
    album_id: t.album_id,
    artist_id: t.artist_id,
    title: t.title,
    track_no: t.track_no,
    disc_no: t.disc_no,
    duration_ms: t.duration_ms,
    codec: t.codec,
    bitrate_kbps: t.bitrate_kbps,
    file_path: t.file_path,
    file_size: t.file_size,
    sample_rate_hz: t.sample_rate_hz,
    bit_depth: t.bit_depth,
    channels: t.channels,
    local_file_path: null,
    is_single_release: t.is_single_release,
    is_explicit: false,
    loudness_lufs: t.loudness_lufs,
    loudness_peak: t.loudness_peak,
    album_loudness_lufs: t.album_loudness_lufs,
    aliases: [],
    downloaded: false,
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

  // internal: the deck's live *active* element (re-pointed on every handoff),
  // kept out of the serialized state. Consumers treat it as "the" element.
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
  /** Re-apply loudness gain to the playing track (call after a loudness pref change). */
  refreshLoudness: () => void;
  toggleShuffle: () => void;
  cycleRepeat: () => void;
  clearQueue: () => void;
  /** Binds the two deck `<audio>` elements — not for UI use. */
  _bind: (a: HTMLAudioElement, b: HTMLAudioElement) => () => void;
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
    loadAndPlay(get, set, index, manualFadeSec());
  },

  togglePlay: () => {
    const { isPlaying } = get();
    if (!_deck) return;
    if (isPlaying) {
      _deck.pause();
    } else {
      void _deck.resume().catch((e) => reportPlayError(set, e));
    }
  },

  next: () => {
    advance(get, set, { manual: true });
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
    loadAndPlay(get, set, prevIdx, manualFadeSec());
  },

  seekTo: (sec) => {
    _deck?.seekTo(sec);
  },

  setVolume: (v) => {
    const clamped = Math.max(0, Math.min(1, v));
    _deck?.setMasterVolume(clamped);
    set({ volume: clamped });
  },

  refreshLoudness: () => {
    _deck?.refreshGain();
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
        armPreload(get);
        return;
      }
    }
    set({ shuffle: nowShuffling });
    prefetchNext(get);
    armPreload(get);
  },

  cycleRepeat: () => {
    const next: Record<RepeatMode, RepeatMode> = {
      off: "all",
      all: "one",
      one: "off",
    };
    set({ repeat: next[get().repeat] });
    prefetchNext(get); // what plays next may have changed (e.g. repeat-one)
    armPreload(get);
  },

  clearQueue: () => {
    _deck?.reset();
    set({ queue: [], currentIndex: -1, isPlaying: false, positionSec: 0, durationSec: 0 });
  },

  _bind: (a, b) => {
    // The two persistent elements form the playback deck. Rebinds (StrictMode
    // double-mount, HMR) tear the old deck down first — construction is
    // idempotent, and the deck adopts whichever element is already sounding.
    _deck?.destroy();
    const deck = new Deck(a, b, deckCallbacks(get, set));
    _deck = deck;
    set({ audio: deck.active });
    deck.setMasterVolume(get().volume); // apply restored (or default) volume
    hookPrefetchReady();
    primeRestored(get, deck.active); // load a restored track at its saved position, paused
    return () => {
      if (_deck === deck) _deck = null;
      deck.destroy();
    };
  },
}));

// ---------------------------------------------------------------------------
// the deck
//
// Module-level so helpers outside the store body (armPreload, the
// prefetch-ready hook) can reach it. Only `_bind` writes it.
// ---------------------------------------------------------------------------

let _deck: Deck | null = null;

/** Wire deck events into store state + the recording hooks. */
function deckCallbacks(get: Get, set: Set): DeckCallbacks {
  return {
    onPlay: () => {
      set({ isPlaying: true });
      // A restored session resumes without ever passing loadAndPlay — make
      // sure the look-ahead (Rust prefetch + standby preload) is primed.
      prefetchNext(get);
      armPreload(get);
    },

    onPause: (atEnd, posSec, durSec) => {
      // A track reaching its end fires `pause` right before `ended`. Ignore
      // that one (the deck/fallback is about to start the next track):
      // flipping isPlaying=false here makes the native MediaService drop its
      // wake/WiFi lock + foreground status — so Android kills the screen-off
      // process in the ~20ms gap before the next track starts. Genuine pauses
      // are unaffected: a user tap pauses mid-track (not ended), and
      // end-of-queue sets isPlaying=false in advance().
      if (atEnd) return;
      set({ isPlaying: false });
      // A genuine pause is a good moment to checkpoint episode progress.
      const s = get();
      recordEpisodeProgress(s.queue[s.currentIndex], posSec, durSec);
    },

    onTime: (posSec, durSec) => {
      set({ positionSec: posSec, durationSec: durSec });
      const s = get();
      maybeRecordProgress(s.queue[s.currentIndex], posSec, durSec);
      maybeRecordPlay(s.queue[s.currentIndex], posSec, durSec);
    },

    onDurationChange: (durSec) => set({ durationSec: durSec }),

    onActiveError: (code) => {
      const msg =
        code === 4
          ? "track not available (offline and not downloaded?)"
          : `audio error (code ${code})`;
      set({ error: msg, isPlaying: false });
    },

    onPlayRejected: (e) => reportPlayError(set, e),

    onSwapped: (index, item) => {
      // The outgoing listen's play-record guard migrates to the retiring slot
      // so its onRetired can tell "already counted at threshold" from "never
      // counted"; the incoming listen starts counting fresh.
      _retiringPlayRecordedId = _playRecordedId;
      _playRecordedId = null;
      const s = get();
      // The armed index may have gone stale if the queue mutated while the
      // boundary was in flight — re-locate the item by id if so.
      const idx =
        s.queue[index]?.id === item.id
          ? index
          : s.queue.findIndex((t) => t.id === item.id);
      set({
        currentIndex: idx >= 0 ? idx : index,
        positionSec: 0,
        durationSec: item.duration_ms / 1000,
        error: null,
        audio: _deck?.active ?? s.audio,
      });
      prefetchNext(get);
      armPreload(get);
    },

    onRetired: (item, posSec, durSec, reachedEnd) => {
      recordEpisodeProgress(item, posSec, durSec, reachedEnd);
      // Mirror the legacy guarded `ended` record: the threshold recorder
      // (maybeRecordPlay) usually already counted this listen — only record
      // here if it never did. Post-swap retires (crossfade tail) check the
      // migrated guard; pre-swap ones (gapless, same tick as the swap) were
      // migrated a moment ago and land on the same field.
      const counted =
        _retiringPlayRecordedId === item.id || _playRecordedId === item.id;
      _retiringPlayRecordedId = null;
      if (!counted) submitPlay(item, posSec, reachedEnd);
    },

    onAdvanceFallback: () => {
      // Natural end with no usable standby — exactly the pre-deck `ended`
      // path: record the finished listen from state, then advance.
      const s = get();
      const el = _deck?.active;
      const pos = el?.currentTime ?? s.positionSec;
      const dur = el?.duration ?? s.durationSec;
      recordEpisodeProgress(s.queue[s.currentIndex], pos, dur || 0, true);
      recordPlay(s.queue[s.currentIndex], pos, true);
      advance(get, set, { manual: false });
    },

    onRecover: () => {
      // A gapless swap announced itself but its play() was rejected — reload
      // the (already swapped-to) current track through the legacy path.
      const s = get();
      if (s.currentIndex >= 0) loadAndPlay(get, set, s.currentIndex);
    },
  };
}

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

// ---------------------------------------------------------------------------
// play-history recording (Phase 11)
//
// A track counts as a "play" once the listener reaches PLAY_COUNT_MIN_SEC OR
// PLAY_COUNT_MIN_FRACTION of it (whichever first), or plays it to the end. We
// record at most one play per listen (`_playRecordedId`, reset on each load),
// so a paused/resumed track isn't double-counted but a repeat replays a new
// play. Deck handoffs extend a listen past the track switch (the outgoing
// track keeps fading after the UI moves on), so at each swap the guard
// migrates to `_retiringPlayRecordedId` — the retire hook consults it to
// avoid double-counting a listen the threshold already recorded. Only a
// bearer *user* session records — play history is per-user, and a SECRET_KEY
// session has no user to own it (the server would reject it).
// ---------------------------------------------------------------------------

const PLAY_COUNT_MIN_SEC = 30;
const PLAY_COUNT_MIN_FRACTION = 0.5;
let _playRecordedId: string | null = null;
let _retiringPlayRecordedId: string | null = null;

function isBearerUser(): boolean {
  return useAppStore.getState().session?.kind === "bearer";
}

/** Fire-and-forget push of one play event (no per-listen guard — see callers). */
function submitPlay(item: QueueItem, posSec: number, completed: boolean) {
  if (item.mediaKind === "episode") return;
  if (!isBearerUser()) return;
  const msPlayed = Math.max(0, Math.round(posSec * 1000));
  // Queue locally, then opportunistically flush (both fire-and-forget — an
  // offline failure leaves the play queued for the sync scheduler).
  void playHistoryRecord(item.id, msPlayed, completed)
    .then(() => playHistoryFlush())
    .catch(() => {});
}

/** Record a track play once per listen (no-op for episodes / non-user sessions). */
function recordPlay(item: QueueItem | undefined, posSec: number, completed: boolean) {
  if (!item || item.mediaKind === "episode") return;
  if (_playRecordedId === item.id) return;
  if (!isBearerUser()) return;
  _playRecordedId = item.id;
  submitPlay(item, posSec, completed);
}

/** From the `timeupdate` firehose: count a play once it crosses the threshold. */
function maybeRecordPlay(item: QueueItem | undefined, posSec: number, durSec: number) {
  if (!item || item.mediaKind === "episode") return;
  if (_playRecordedId === item.id) return;
  const crossed =
    posSec >= PLAY_COUNT_MIN_SEC ||
    (durSec > 0 && posSec / durSec >= PLAY_COUNT_MIN_FRACTION);
  if (crossed) recordPlay(item, posSec, false);
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

/**
 * The manual-skip fade duration per current prefs: crossfade must be on
 * (which requires gapless on) plus the manual-skip toggle, capped short so
 * skips feel snappy. 0 = instant cut (the default and legacy behavior).
 */
function manualFadeSec(): number {
  const prefs = playbackPrefs();
  if (!prefs.gaplessEnabled || !(prefs.crossfadeSec > 0) || !prefs.crossfadeOnManualSkip) {
    return 0;
  }
  return Math.min(prefs.crossfadeSec, MANUAL_FADE_MAX_SEC);
}

/**
 * Move to the track after the current one, honoring repeat semantics.
 * `manual` distinguishes a user skip (may crossfade, per prefs) from a
 * natural end-of-track advance on the fallback path (always an instant cut —
 * the deck handles fading natural boundaries itself).
 */
function advance(get: Get, set: Set, opts: { manual: boolean }) {
  const { queue, currentIndex, repeat } = get();
  if (queue.length === 0) return;
  const fadeSec = opts.manual ? manualFadeSec() : 0;
  if (repeat === "one") {
    // Replay the same track.
    loadAndPlay(get, set, currentIndex, fadeSec);
    return;
  }
  let nextIdx = currentIndex + 1;
  if (nextIdx >= queue.length) {
    if (repeat === "all") nextIdx = 0;
    else {
      // End of queue, no repeat → stop.
      _deck?.pause();
      set({ isPlaying: false });
      return;
    }
  }
  loadAndPlay(get, set, nextIdx, fadeSec);
}

/** Load the track at `index` onto the deck and start playback. */
function loadAndPlay(_get: Get, set: Set, index: number, fadeSec = 0) {
  const { queue, currentIndex } = _get();
  const track = queue[index];
  const deck = _deck;
  if (!track || !deck) return;
  // Checkpoint the outgoing episode (if any) before we switch away from it, so
  // its resume position is saved even on a manual skip.
  const outgoing = queue[currentIndex];
  if (outgoing && outgoing.id !== track.id) {
    const el = deck.active;
    recordEpisodeProgress(outgoing, el.currentTime, el.duration || 0);
  }
  set({ currentIndex: index, positionSec: 0, durationSec: 0, error: null });
  // New listen → allow this track to count a fresh play (repeat re-counts).
  _playRecordedId = null;
  // Kick off the next track's prefetch in parallel with this one's load, so it's
  // a local file by the time we reach it (screen-off auto-advance — see below).
  prefetchNext(_get);
  resolveSrc(track)
    .then((url: string) => {
      // Guard against races: a rapid skip could have moved currentIndex.
      if (_get().queue[_get().currentIndex]?.id !== track.id) return;
      deck.playNow(track, index, url, { fadeSec });
      // A manual-skip fade moves the active element without an onSwapped.
      set({ audio: deck.active });
      armPreload(_get);
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
 * The index that plays after the current track (repeat rules applied), or -1
 * when playback stops there. Shared by the Rust prefetch chain and the deck's
 * standby preload so they always agree on "what's next".
 */
function nextIndexFor(s: Pick<PlayerState, "queue" | "currentIndex" | "repeat">): number {
  const { queue, currentIndex, repeat } = s;
  if (currentIndex < 0 || queue.length === 0) return -1;
  if (repeat === "one") return currentIndex;
  const n = currentIndex + 1;
  if (n >= queue.length) return repeat === "all" ? 0 : -1;
  return n;
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
  const n = nextIndexFor(_get());
  if (n < 0) return;
  const next = _get().queue[n];
  // Prefetch is track-specific (it pulls `/tracks/{id}/stream`); skip episodes.
  if (next && next.mediaKind !== "episode") void playerPrefetch(next.id).catch(() => {});
}

/**
 * Arm (or clear) the deck's standby element for the upcoming track. Local
 * sources arm immediately; a streamed track only once the Rust prefetcher has
 * it on disk — arming earlier would proxy-stream it in parallel with the
 * prefetch download. Re-invoked by the `player-prefetch-ready` event for the
 * "completes later" case. Idempotent (the deck no-ops on a same-id re-arm).
 */
function armPreload(_get: Get) {
  const deck = _deck;
  if (!deck) return;
  if (!playbackPrefs().gaplessEnabled) {
    deck.syncPreload(null, -1, null);
    return;
  }
  const s = _get();
  const n = nextIndexFor(s);
  const item = n >= 0 ? s.queue[n] : null;
  if (!item) {
    deck.syncPreload(null, -1, null);
    return;
  }
  void (async () => {
    let local = item.downloaded || (item.streamUrl ?? null) != null;
    if (!local && item.mediaKind !== "episode") {
      local = await playerPrefetchIsReady(item.id).catch(() => false);
    }
    if (!local) return; // the prefetch-ready event re-invokes us
    const url = await resolveSrc(item);
    const st = _get();
    // Re-check: the queue (or what's next) may have changed while resolving.
    if (nextIndexFor(st) === n && st.queue[n]?.id === item.id) {
      deck.syncPreload(item, n, url);
    }
  })().catch(() => {});
}

let _prefetchReadyHooked = false;

/** Re-arm the standby whenever the Rust prefetcher lands a file (once, lazily). */
function hookPrefetchReady() {
  if (_prefetchReadyHooked) return;
  _prefetchReadyHooked = true;
  onPrefetchReady(() => armPreload(usePlayerStore.getState)).catch(() => {
    /* non-Tauri browser dev — the deck just falls back at boundaries */
  });
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
