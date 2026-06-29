// Favorites client state (Phase 11).
//
// A Zustand store holds the set of favorited *track* ids so heart toggles on
// track rows + the now-playing bar are instant and don't each fire an
// is-favorite query. Album/artist hearts are rarer (one per hero) and query
// their state directly. Mutations are optimistic with revert-on-error —
// favorites are server-authoritative and online-only (no offline outbox yet).

import { create } from "zustand";
import { favoritesFavorite, favoritesTrackIds, favoritesUnfavorite } from "../ipc";

type FavoritesStore = {
  /** Favorited track ids (the heart-state source for track rows + player bar). */
  trackIds: Set<string>;
  loaded: boolean;
  /** Hydrate the track-id set (bearer-user only; swallows offline/anon). */
  load: () => Promise<void>;
  /** Reset on sign-out / a non-user session. */
  clear: () => void;
  isTrackFav: (id: string) => boolean;
  /** Optimistically toggle a track favorite; reverts on error. */
  toggleTrack: (id: string) => Promise<void>;
};

export const useFavoritesStore = create<FavoritesStore>((set, get) => ({
  trackIds: new Set(),
  loaded: false,

  load: async () => {
    try {
      const ids = await favoritesTrackIds();
      set({ trackIds: new Set(ids), loaded: true });
    } catch {
      /* offline / anonymous — leave as-is */
    }
  },

  clear: () => set({ trackIds: new Set(), loaded: false }),

  isTrackFav: (id) => get().trackIds.has(id),

  toggleTrack: async (id) => {
    const has = get().trackIds.has(id);
    // Optimistic flip.
    set((s) => {
      const next = new Set(s.trackIds);
      if (has) next.delete(id);
      else next.add(id);
      return { trackIds: next };
    });
    try {
      if (has) await favoritesUnfavorite("track", id);
      else await favoritesFavorite("track", id);
    } catch (e) {
      // Revert on failure (offline / server error).
      set((s) => {
        const next = new Set(s.trackIds);
        if (has) next.add(id);
        else next.delete(id);
        return { trackIds: next };
      });
      throw e;
    }
  },
}));
