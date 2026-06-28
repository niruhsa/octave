// Quick Search (command palette) state.
//
// Owns the palette's open/closed flag, the persisted "recent queries" list, and
// a couple of user preferences (keyboard-hint footer, backdrop dim). Kept in a
// store rather than RootLayout state so the global hotkey dispatcher
// (`useHotkeys`), the sidebar/mobile-nav launch buttons, and the palette itself
// can all toggle it without prop drilling. Recents + prefs persist to
// localStorage so they survive relaunches.

import { create } from "zustand";

const RECENTS_KEY = "octave:qs:recents";
const PREFS_KEY = "octave:qs:prefs";
const MAX_RECENTS = 6;

/** Toggleable behaviours exposed in Settings → Quick Search. */
export type QuickSearchPrefs = {
  /** Show the keyboard-hint footer at the bottom of the palette. */
  keyboardHints: boolean;
  /** Dim + blur the app behind the palette while it's open. */
  dimBackground: boolean;
};

export const DEFAULT_PREFS: QuickSearchPrefs = {
  keyboardHints: true,
  dimBackground: true,
};

function loadRecents(): string[] {
  try {
    const raw = localStorage.getItem(RECENTS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((x): x is string => typeof x === "string").slice(0, MAX_RECENTS);
  } catch {
    return [];
  }
}

function loadPrefs(): QuickSearchPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { ...DEFAULT_PREFS };
    const parsed = JSON.parse(raw) as Partial<QuickSearchPrefs>;
    return { ...DEFAULT_PREFS, ...parsed };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

function persistRecents(recents: string[]) {
  try {
    localStorage.setItem(RECENTS_KEY, JSON.stringify(recents));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

function persistPrefs(prefs: QuickSearchPrefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

type QuickSearchStore = {
  open: boolean;
  recents: string[];
  prefs: QuickSearchPrefs;

  openPalette: () => void;
  close: () => void;
  toggle: () => void;

  /** Record a query/command/tab the user just acted on (most-recent first). */
  addRecent: (entry: string) => void;
  clearRecents: () => void;

  setPref: <K extends keyof QuickSearchPrefs>(key: K, value: QuickSearchPrefs[K]) => void;
};

export const useQuickSearchStore = create<QuickSearchStore>((set, get) => ({
  open: false,
  recents: loadRecents(),
  prefs: loadPrefs(),

  openPalette: () => set({ open: true }),
  close: () => set({ open: false }),
  toggle: () => set((s) => ({ open: !s.open })),

  addRecent: (entry) => {
    const e = entry.trim();
    if (!e) return;
    const next = [e, ...get().recents.filter((r) => r !== e)].slice(0, MAX_RECENTS);
    persistRecents(next);
    set({ recents: next });
  },

  clearRecents: () => {
    persistRecents([]);
    set({ recents: [] });
  },

  setPref: (key, value) => {
    const next = { ...get().prefs, [key]: value };
    persistPrefs(next);
    set({ prefs: next });
  },
}));
