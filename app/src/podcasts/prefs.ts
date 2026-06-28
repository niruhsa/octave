// Podcast behaviour preferences (Settings → Podcasts).
//
// Currently a single toggle: whether subscribing to a show jumps straight to
// its page. The Subscribe button on the Podcasts tab inverts this with a
// Shift+Click, so whichever way the default is set, holding Shift does the
// opposite. Persisted to localStorage so it survives relaunches.

import { create } from "zustand";

const PREFS_KEY = "octave:podcast:prefs";

export type PodcastPrefs = {
  /** After subscribing to a show, navigate to its page (Shift+Click inverts). */
  openAfterSubscribe: boolean;
};

export const DEFAULT_PODCAST_PREFS: PodcastPrefs = {
  openAfterSubscribe: true,
};

function loadPrefs(): PodcastPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { ...DEFAULT_PODCAST_PREFS };
    const parsed = JSON.parse(raw) as Partial<PodcastPrefs>;
    return { ...DEFAULT_PODCAST_PREFS, ...parsed };
  } catch {
    return { ...DEFAULT_PODCAST_PREFS };
  }
}

function persistPrefs(prefs: PodcastPrefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

type PodcastPrefsStore = {
  prefs: PodcastPrefs;
  setPref: <K extends keyof PodcastPrefs>(key: K, value: PodcastPrefs[K]) => void;
};

export const usePodcastPrefsStore = create<PodcastPrefsStore>((set, get) => ({
  prefs: loadPrefs(),
  setPref: (key, value) => {
    const next = { ...get().prefs, [key]: value };
    persistPrefs(next);
    set({ prefs: next });
  },
}));
