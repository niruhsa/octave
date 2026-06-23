// Now-playing UI state (separate from playback state in `store.ts`).
//
// Just the expand/collapse flag for the full-screen player (`NowPlaying`).
// Kept out of the playback store so a re-render of the overlay never touches
// queue/position state and vice-versa.

import { create } from "zustand";

type PlayerUiState = {
  /** Whether the full-screen now-playing player is open. */
  expanded: boolean;
  open: () => void;
  close: () => void;
  toggle: () => void;
};

export const usePlayerUi = create<PlayerUiState>((set) => ({
  expanded: false,
  open: () => set({ expanded: true }),
  close: () => set({ expanded: false }),
  toggle: () => set((s) => ({ expanded: !s.expanded })),
}));
