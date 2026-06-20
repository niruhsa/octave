// Sync state + auto-trigger (Phase 5, extended).
//
// Drives the reconcile cycle and exposes its status to the UI. Sync runs:
//   * on connectivity regain (offline → online),
//   * on window focus (desktop) / app foreground,
//   * when a new offline edit is queued (pending-op count rises),
//   * on a 30 s floor interval (catches server-side changes that need
//     pulling even when nothing local changed),
//   * manually (the "sync now" affordance in the sidebar).
//
// Connectivity probing: `useReconnect` pings `/health` once per second
// while a session is active and mirrors the result into the app store's
// `online` flag. Zustand only notifies subscribers on an actual change,
// so a steady `true` doesn't re-render or re-trigger the online-regain
// sync effect. The 1 s cadence is what makes an offline→online transition
// surface within a second.
//
// The engine itself lives in Rust (`sync_now`); this module is just the
// scheduler + a Zustand store for the badge ("N unsynced edits") and last
// report.

import { useEffect, useRef } from "react";
import { create } from "zustand";
import {
  authRefreshOnline,
  syncNow,
  syncPendingCount,
  type SyncReport,
} from "../ipc";
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
 * Connectivity probe. Pings `/health` once per second while a session is
 * active and writes the result into the app store's `online` flag. The
 * ping is issued regardless of the current `online` value so an
 * offline→online transition is detected within a second; Zustand's
 * identity check means a steady state doesn't churn. `AuthNotConfigured`
 * (no manager yet) is swallowed — nothing to probe until the user points
 * us at a server.
 */
const RECONNECT_INTERVAL_MS = 1000;

function useReconnect() {
  const session = useAppStore((s) => s.session);
  const setOnline = useAppStore((s) => s.setOnline);

  useEffect(() => {
    if (!session) return;
    let cancelled = false;

    const tick = async () => {
      if (cancelled) return;
      try {
        const ok = await authRefreshOnline();
        if (!cancelled) setOnline(ok);
      } catch {
        // AuthNotConfigured / no manager — leave the flag as-is.
      }
    };

    // Fire once immediately so the boot state isn't stale for a second.
    void tick();
    const id = window.setInterval(tick, RECONNECT_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [session, setOnline]);
}

/**
 * Mount once (in the root layout). Wires the auto-sync triggers:
 *   - 1 s `/health` reconnect probe (`useReconnect`),
 *   - online regain (watches the app store's `online` flag),
 *   - window focus,
 *   - pending-op-count rise (a new offline edit was queued → sync now),
 *   - 30 s floor interval (pull server-side changes even when idle),
 *   - an initial pending-count read so the badge is populated on boot.
 */
const PENDING_POLL_MS = 2000;
const AUTO_SYNC_INTERVAL_MS = 30_000;

export function useSyncScheduler() {
  const online = useAppStore((s) => s.online);
  const session = useAppStore((s) => s.session);
  const run = useSyncStore((s) => s.run);
  const refreshPending = useSyncStore((s) => s.refreshPending);
  const pending = useSyncStore((s) => s.pending);

  // 1 s reconnect probe.
  useReconnect();

  // Initial pending count.
  useEffect(() => {
    void refreshPending();
  }, [refreshPending]);

  // Poll the pending-op count so a newly-queued offline edit is detected
  // within ~2 s. The actual sync fires in the pending-rise effect below;
  // this just keeps `pending` fresh.
  useEffect(() => {
    if (!session) return;
    const id = window.setInterval(() => void refreshPending(), PENDING_POLL_MS);
    return () => window.clearInterval(id);
  }, [session, refreshPending]);

  // Sync when the pending-op count rises (a change was made locally) and
  // we're online + authenticated. A decrease (post-sync clear) does NOT
  // retrigger — only new edits do.
  const prevPending = useRef(pending);
  useEffect(() => {
    if (pending > prevPending.current && online && session) {
      void run();
    }
    prevPending.current = pending;
  }, [pending, online, session, run]);

  // Sync when we (re)gain connectivity while authenticated.
  useEffect(() => {
    if (online && session) void run();
  }, [online, session, run]);

  // 30 s floor: pull server-side changes even when nothing local queued.
  useEffect(() => {
    if (!online || !session) return;
    const id = window.setInterval(() => void run(), AUTO_SYNC_INTERVAL_MS);
    return () => window.clearInterval(id);
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
