// Sync state + auto-trigger (Phase 5).
//
// Drives the reconcile cycle and exposes its status to the UI. Sync runs:
//   * on connectivity regain (offline → online),
//   * on window focus (desktop) / app foreground,
//   * manually (a "Sync now" button).
//
// The engine itself lives in Rust (`sync_now`); this is just the scheduler
// + a Zustand store for the badge ("N unsynced edits") and last report.

import { useEffect } from "react";
import { create } from "zustand";
import { syncNow, syncPendingCount, type SyncReport } from "../ipc";
import { useAppStore } from "../store";

type SyncStatus = "idle" | "syncing" | "ok" | "error";

type SyncStoreState = {
  status: SyncStatus;
  pending: number;
  lastReport: SyncReport | null;
  lastError: string | null;
  lastSyncedAt: number | null;

  run: () => Promise<void>;
  refreshPending: () => Promise<void>;
};

export const useSyncStore = create<SyncStoreState>((set, get) => ({
  status: "idle",
  pending: 0,
  lastReport: null,
  lastError: null,
  lastSyncedAt: null,

  run: async () => {
    // Don't stack runs; a sync in flight already covers newer edits.
    if (get().status === "syncing") return;
    set({ status: "syncing", lastError: null });
    try {
      const report = await syncNow();
      set({
        status: "ok",
        lastReport: report,
        lastSyncedAt: Date.now(),
      });
      await get().refreshPending();
    } catch (e) {
      // AuthNotConfigured is the anonymous case — treat as a no-op, not an
      // error the user needs to see.
      const obj = e as { kind?: string; message?: string };
      if (obj?.kind === "AuthNotConfigured") {
        set({ status: "idle" });
        return;
      }
      set({
        status: "error",
        lastError: obj?.message ?? String(e),
      });
    }
  },

  refreshPending: async () => {
    try {
      set({ pending: await syncPendingCount() });
    } catch {
      /* anonymous / no DB yet — leave pending as-is */
    }
  },
}));

/**
 * Mount once (in the root layout). Wires the auto-sync triggers:
 *   - online regain (watches the app store's `online` flag),
 *   - window focus,
 *   - an initial pending-count read so the badge is populated on boot.
 */
export function useSyncScheduler() {
  const online = useAppStore((s) => s.online);
  const session = useAppStore((s) => s.session);
  const run = useSyncStore((s) => s.run);
  const refreshPending = useSyncStore((s) => s.refreshPending);

  // Initial pending count.
  useEffect(() => {
    void refreshPending();
  }, [refreshPending]);

  // Sync when we (re)gain connectivity while authenticated.
  useEffect(() => {
    if (online && session) void run();
  }, [online, session, run]);

  // Sync on window focus (desktop) / foreground (mobile webview).
  useEffect(() => {
    const onFocus = () => {
      if (useAppStore.getState().online && useAppStore.getState().session) {
        void run();
      }
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") onFocus();
    });
    return () => {
      window.removeEventListener("focus", onFocus);
    };
  }, [run]);
}
