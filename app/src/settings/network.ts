// Networking preferences (Settings → Networking).
//
// Currently a single knob: how many file chunks upload concurrently. It
// overrides the backend default (`CHUNK_CONCURRENCY` in `upload_commands.rs`)
// and — crucially — can be changed mid-upload: `setPref` pushes the new value to
// Rust (`uploads_set_concurrency`), which resizes the running upload's
// concurrency gate on the fly. Persisted to localStorage so it survives
// relaunches; `syncNetworkPrefs` re-pushes the stored value to the backend on
// startup (so an upload uses the user's setting even before Settings is opened).

import { create } from "zustand";

import { uploadsSetConcurrency } from "../ipc";

const PREFS_KEY = "octave:network:prefs";

/** Bounds mirror the backend clamp (`1..=MAX_CHUNK_CONCURRENCY`). */
export const MIN_CHUNK_CONCURRENCY = 1;
export const MAX_CHUNK_CONCURRENCY = 16;
/** Backend default (`CHUNK_CONCURRENCY` in `upload_commands.rs`). */
export const DEFAULT_CHUNK_CONCURRENCY = 4;

export type NetworkPrefs = {
  /** Chunks uploaded in parallel per upload (1–16). */
  chunkConcurrency: number;
};

export const DEFAULT_NETWORK_PREFS: NetworkPrefs = {
  chunkConcurrency: DEFAULT_CHUNK_CONCURRENCY,
};

function clampConcurrency(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_CHUNK_CONCURRENCY;
  return Math.min(
    MAX_CHUNK_CONCURRENCY,
    Math.max(MIN_CHUNK_CONCURRENCY, Math.round(n)),
  );
}

function loadPrefs(): NetworkPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { ...DEFAULT_NETWORK_PREFS };
    const parsed = JSON.parse(raw) as Partial<NetworkPrefs>;
    return {
      chunkConcurrency: clampConcurrency(
        parsed.chunkConcurrency ?? DEFAULT_CHUNK_CONCURRENCY,
      ),
    };
  } catch {
    return { ...DEFAULT_NETWORK_PREFS };
  }
}

function persistPrefs(prefs: NetworkPrefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

type NetworkPrefsStore = {
  prefs: NetworkPrefs;
  setPref: <K extends keyof NetworkPrefs>(key: K, value: NetworkPrefs[K]) => void;
};

export const useNetworkPrefsStore = create<NetworkPrefsStore>((set, get) => ({
  prefs: loadPrefs(),
  setPref: (key, value) => {
    const v =
      key === "chunkConcurrency" ? clampConcurrency(value as number) : value;
    const next = { ...get().prefs, [key]: v };
    persistPrefs(next);
    set({ prefs: next });
    // Push to the backend so it takes effect immediately — including for an
    // upload that's already in progress.
    void uploadsSetConcurrency(next.chunkConcurrency).catch(() => {});
  },
}));

/**
 * Push the persisted networking prefs to the backend. Call once on startup so
 * the backend's concurrency matches the stored setting before any upload begins.
 */
export function syncNetworkPrefs() {
  const { chunkConcurrency } = useNetworkPrefsStore.getState().prefs;
  void uploadsSetConcurrency(chunkConcurrency).catch(() => {});
}
