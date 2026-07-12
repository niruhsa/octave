// Playback preferences (Settings → Player).
//
// Shapes how the playback deck (`player/deck.ts`) hands one track off to the
// next: gapless (preload the next track, start it the instant the current one
// ends) and the optional crossfade layered on top. The deck reads these live
// at each track boundary — changes apply from the next transition, no rebind
// needed. Persisted to localStorage so they survive relaunches (same pattern
// as `settings/network.ts`).

import { create } from "zustand";

const PREFS_KEY = "octave:playback:prefs";

export const MIN_CROSSFADE_SEC = 0;
export const MAX_CROSSFADE_SEC = 12;

/** Loudness normalization (Phase 16 — ReplayGain / EBU R128). */
export type LoudnessMode = "off" | "track" | "album";
/** Target loudness range (LUFS). -18 is the ReplayGain 2.0 reference; -14 is
 *  streaming-loud; -23 is broadcast-quiet. */
export const MIN_LOUDNESS_TARGET_LUFS = -30;
export const MAX_LOUDNESS_TARGET_LUFS = -8;
/** Manual preamp trim (dB) on top of the computed gain. */
export const MIN_LOUDNESS_PREAMP_DB = -15;
export const MAX_LOUDNESS_PREAMP_DB = 15;

export type PlaybackPrefs = {
  /**
   * Preload the upcoming track on the standby element and swap at the
   * boundary. Off = every track change goes through the legacy
   * teardown-and-reload path (with its audible gap). Crossfade requires this.
   */
  gaplessEnabled: boolean;
  /** Seconds of equal-power crossfade at track boundaries. 0 = off (gapless cut). */
  crossfadeSec: number;
  /** Also fade when the user skips (next/prev/jump) — capped short so skips stay snappy. */
  crossfadeOnManualSkip: boolean;
  /** Consecutive tracks of the same album always transition gaplessly (never fade). */
  smartAlbumGapless: boolean;
  /**
   * Loudness normalization: `off` plays files at their mastered level; `track`
   * normalizes every song to `loudnessTargetLufs`; `album` applies one gain per
   * album so intra-album dynamics are preserved. Requires the server to have
   * measured loudness (unmeasured tracks play unchanged).
   */
  loudnessMode: LoudnessMode;
  /** Reference loudness the player normalizes toward, in LUFS. */
  loudnessTargetLufs: number;
  /** Extra gain trim applied after normalization, in dB. */
  loudnessPreampDb: number;
};

export const DEFAULT_PLAYBACK_PREFS: PlaybackPrefs = {
  gaplessEnabled: true,
  crossfadeSec: 0,
  crossfadeOnManualSkip: true,
  smartAlbumGapless: true,
  loudnessMode: "off",
  loudnessTargetLufs: -18,
  loudnessPreampDb: 0,
};

function clampCrossfade(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_PLAYBACK_PREFS.crossfadeSec;
  return Math.min(MAX_CROSSFADE_SEC, Math.max(MIN_CROSSFADE_SEC, Math.round(n)));
}

function clampTargetLufs(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_PLAYBACK_PREFS.loudnessTargetLufs;
  return Math.min(MAX_LOUDNESS_TARGET_LUFS, Math.max(MIN_LOUDNESS_TARGET_LUFS, Math.round(n)));
}

function clampPreampDb(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_PLAYBACK_PREFS.loudnessPreampDb;
  return Math.min(MAX_LOUDNESS_PREAMP_DB, Math.max(MIN_LOUDNESS_PREAMP_DB, Math.round(n)));
}

function parseLoudnessMode(v: unknown): LoudnessMode {
  return v === "track" || v === "album" ? v : "off";
}

function loadPrefs(): PlaybackPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { ...DEFAULT_PLAYBACK_PREFS };
    const parsed = JSON.parse(raw) as Partial<PlaybackPrefs>;
    return {
      gaplessEnabled: parsed.gaplessEnabled ?? DEFAULT_PLAYBACK_PREFS.gaplessEnabled,
      crossfadeSec: clampCrossfade(
        parsed.crossfadeSec ?? DEFAULT_PLAYBACK_PREFS.crossfadeSec,
      ),
      crossfadeOnManualSkip:
        parsed.crossfadeOnManualSkip ?? DEFAULT_PLAYBACK_PREFS.crossfadeOnManualSkip,
      smartAlbumGapless:
        parsed.smartAlbumGapless ?? DEFAULT_PLAYBACK_PREFS.smartAlbumGapless,
      loudnessMode: parseLoudnessMode(parsed.loudnessMode),
      loudnessTargetLufs: clampTargetLufs(
        parsed.loudnessTargetLufs ?? DEFAULT_PLAYBACK_PREFS.loudnessTargetLufs,
      ),
      loudnessPreampDb: clampPreampDb(
        parsed.loudnessPreampDb ?? DEFAULT_PLAYBACK_PREFS.loudnessPreampDb,
      ),
    };
  } catch {
    return { ...DEFAULT_PLAYBACK_PREFS };
  }
}

function persistPrefs(prefs: PlaybackPrefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

type PlaybackPrefsStore = {
  prefs: PlaybackPrefs;
  setPref: <K extends keyof PlaybackPrefs>(key: K, value: PlaybackPrefs[K]) => void;
};

export const usePlaybackPrefsStore = create<PlaybackPrefsStore>((set, get) => ({
  prefs: loadPrefs(),
  setPref: (key, value) => {
    let v: PlaybackPrefs[typeof key] = value;
    if (key === "crossfadeSec") v = clampCrossfade(value as number) as PlaybackPrefs[typeof key];
    else if (key === "loudnessTargetLufs")
      v = clampTargetLufs(value as number) as PlaybackPrefs[typeof key];
    else if (key === "loudnessPreampDb")
      v = clampPreampDb(value as number) as PlaybackPrefs[typeof key];
    const next = { ...get().prefs, [key]: v };
    persistPrefs(next);
    set({ prefs: next });
  },
}));

/** Snapshot accessor for reads outside React (the deck, the player store). */
export function playbackPrefs(): PlaybackPrefs {
  return usePlaybackPrefsStore.getState().prefs;
}
