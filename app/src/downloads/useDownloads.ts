// Downloads UI state (Phase 6).
//
// Tracks in-flight downloads (keyed by id) from the `download-progress`
// Tauri event and exposes a storage-usage refresher. The actual download
// calls live in `ipc.ts`; this just aggregates progress for the UI.

import { useEffect } from "react";
import { create } from "zustand";
import {
  downloadsStorageUsage,
  onDownloadProgress,
  type ProgressEvent,
  type StorageUsage,
} from "../ipc";

type ActiveDownload = {
  id: string;
  received: number;
  total: number | null;
  /** For batch scope: which track is currently in flight. */
  trackId: string | null;
  index: number | null;
  totalTracks: number | null;
  done: boolean;
  error: string | null;
};

type DownloadsState = {
  active: Record<string, ActiveDownload>;
  storage: StorageUsage | null;

  apply: (e: ProgressEvent) => void;
  clear: (id: string) => void;
  refreshStorage: () => Promise<void>;
};

export const useDownloadsStore = create<DownloadsState>((set) => ({
  active: {},
  storage: null,

  apply: (e) =>
    set((s) => {
      const prev = s.active[e.id] ?? {
        id: e.id,
        received: 0,
        total: null,
        trackId: null,
        index: null,
        totalTracks: null,
        done: false,
        error: null,
      };
      const next: ActiveDownload = {
        ...prev,
        received: e.received ?? prev.received,
        total: e.total ?? prev.total,
        trackId: e.track_id ?? prev.trackId,
        index: e.index ?? prev.index,
        totalTracks: e.total_tracks ?? prev.totalTracks,
        done: e.phase === "done",
        error: e.phase === "error" ? (e.message ?? "error") : prev.error,
      };
      return { active: { ...s.active, [e.id]: next } };
    }),

  clear: (id) =>
    set((s) => {
      const next = { ...s.active };
      delete next[id];
      return { active: next };
    }),

  refreshStorage: async () => {
    try {
      set({ storage: await downloadsStorageUsage() });
    } catch {
      /* anonymous / no DB yet */
    }
  },
}));

/**
 * Mount once (root layout). Wires the progress-event listener, an
 * initial storage-usage read, and triggers query invalidation whenever a
 * download reaches its terminal phase so all pages reflect the new state
 * without a manual refresh.
 */
export function useDownloadListener(onTransactionComplete?: () => void) {
  const apply = useDownloadsStore((s) => s.apply);
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onDownloadProgress((e) => {
      apply(e);
      if (e.phase === "done" || e.phase === "error") {
        onTransactionComplete?.();
        void refreshStorage();
      }
    }).then((fn) => {
      unlisten = fn;
    });
    void refreshStorage();
    return () => {
      unlisten?.();
    };
  }, [apply, refreshStorage, onTransactionComplete]);
}

/** Format bytes as a human-readable string. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(1)} ${units[i]}`;
}
