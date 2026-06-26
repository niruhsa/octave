// Follows & notifications — unread badge + OS-notification surfacing (Phase 10).
//
// New-release notifications are server-authoritative; the server has no push
// transport yet (PLAN: "push transport later"), so the client *polls*: while
// online with a logged-in user session it fetches the unread feed on a 30 s
// floor interval and on window focus / foreground, mirrors the unread count
// into a Zustand store (the sidebar/mobile badge), and fires an OS notification
// (via `@tauri-apps/plugin-notification`, which posts on desktop *and* Android)
// for any notification id it hasn't surfaced before.
//
// True background push (FCM on Android / APNs on iOS) is deferred/best-effort —
// it needs a server push transport + a Firebase/APNs integration. Polling
// covers the foreground case ("following an artist surfaces new-release
// notifications") on every platform.

import { useEffect } from "react";
import { create } from "zustand";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import {
  notifBackgroundSyncDisable,
  notifBackgroundSyncEnable,
  notificationsList,
  notificationsUnreadCount,
  pushRegister,
} from "../ipc";
import { useAppStore } from "../store";

/** Persisted set of notification ids we've already surfaced as an OS
 *  notification, so a new release alerts exactly once (across reloads). */
const NOTIFIED_KEY = "octave:notified-ids";
/** Set once the initial backlog has been seeded (so a first run with existing
 *  unread notifications doesn't blast one OS notification per historical row). */
const SEEDED_KEY = "octave:notified-seeded";
/** Cap the tracked-id set so it can't grow unbounded. */
const MAX_TRACKED = 300;
/** Floor poll cadence while online + signed in. */
const POLL_MS = 30_000;
/** How many unread rows to scan per poll for fresh OS notifications. */
const SCAN_LIMIT = 50;

function loadNotified(): Set<string> {
  try {
    const raw = localStorage.getItem(NOTIFIED_KEY);
    if (!raw) return new Set();
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? new Set(arr.map(String)) : new Set();
  } catch {
    return new Set();
  }
}

function saveNotified(ids: Set<string>) {
  try {
    // Keep only the most-recently-added ids (Set preserves insertion order).
    const arr = [...ids].slice(-MAX_TRACKED);
    localStorage.setItem(NOTIFIED_KEY, JSON.stringify(arr));
  } catch {
    /* storage unavailable */
  }
}

type NotificationsStore = {
  /** Total unread count for the badge. */
  unreadCount: number;
  setUnread: (n: number) => void;
  /** Re-read the unread count (e.g. after marking one read in the route). */
  refresh: () => Promise<void>;
};

export const useNotificationsStore = create<NotificationsStore>((set) => ({
  unreadCount: 0,
  setUnread: (n) => set({ unreadCount: n }),
  refresh: async () => {
    try {
      set({ unreadCount: await notificationsUnreadCount() });
    } catch {
      /* offline / anonymous — leave as-is */
    }
  },
}));

async function ensurePermission(): Promise<boolean> {
  try {
    if (await isPermissionGranted()) return true;
    return (await requestPermission()) === "granted";
  } catch {
    return false;
  }
}

/** One poll: refresh the badge + fire OS notifications for newly-seen rows. */
async function poll(setUnread: (n: number) => void) {
  let page;
  try {
    page = await notificationsList(true, SCAN_LIMIT);
  } catch {
    return; // offline / anonymous
  }
  setUnread(page.unread_count);

  const notified = loadNotified();

  // First run ever: seed the backlog without firing so the user isn't blasted
  // with one OS notification per pre-existing unread row.
  if (localStorage.getItem(SEEDED_KEY) !== "1") {
    for (const n of page.notifications) notified.add(n.id);
    saveNotified(notified);
    try {
      localStorage.setItem(SEEDED_KEY, "1");
    } catch {
      /* storage unavailable */
    }
    return;
  }

  const fresh = page.notifications.filter((n) => !notified.has(n.id));
  if (fresh.length === 0) return;

  const canNotify = await ensurePermission();
  for (const n of fresh) {
    if (canNotify) {
      try {
        sendNotification({ title: n.title, body: n.body ?? undefined });
      } catch {
        /* plugin unavailable (e.g. browser preview) */
      }
    }
    notified.add(n.id);
  }
  saveNotified(notified);
}

/**
 * Mount once (in the root layout). Polls the unread feed while online with a
 * logged-in *user* (bearer) session — a `SECRET_KEY` session has no
 * per-user notifications, so the badge stays at 0 and no polling runs.
 */
export function useNotificationsScheduler() {
  const online = useAppStore((s) => s.online);
  const session = useAppStore((s) => s.session);
  const setUnread = useNotificationsStore((s) => s.setUnread);
  const isUser = session?.kind === "bearer";

  // Clear the badge whenever there's no eligible user (sign-out / secret key).
  useEffect(() => {
    if (!isUser) setUnread(0);
  }, [isUser, setUnread]);

  // Wire up while-closed delivery for a signed-in user, preferring real-time
  // FCM push and falling back to the WorkManager background poll when FCM is
  // unavailable (desktop / no Google Play Services). Both no-op on desktop.
  // Keyed on the session identity bits so a re-login re-arms with a fresh token.
  useEffect(() => {
    let cancelled = false;
    if (isUser) {
      void (async () => {
        let fcm = false;
        try {
          fcm = await pushRegister();
        } catch {
          /* desktop / plugin unavailable */
        }
        if (cancelled) return;
        try {
          // FCM delivers while closed → the poll would only double-post.
          if (fcm) await notifBackgroundSyncDisable();
          else await notifBackgroundSyncEnable();
        } catch {
          /* no-op on desktop */
        }
      })();
    } else {
      void notifBackgroundSyncDisable().catch(() => {});
    }
    return () => {
      cancelled = true;
    };
  }, [isUser, session?.user_id, session?.expires_at]);

  // Poll while online + signed in (immediately, then on the floor interval).
  useEffect(() => {
    if (!online || !isUser) return;
    let cancelled = false;
    const tick = () => {
      if (!cancelled) void poll(setUnread);
    };
    tick();
    const id = window.setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [online, isUser, setUnread]);

  // Poll on focus / foreground so a new release surfaces promptly on return.
  useEffect(() => {
    const onFocus = () => {
      const s = useAppStore.getState();
      if (s.online && s.session?.kind === "bearer") void poll(setUnread);
    };
    window.addEventListener("focus", onFocus);
    const onVis = () => {
      if (document.visibilityState === "visible") onFocus();
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [setUnread]);
}
