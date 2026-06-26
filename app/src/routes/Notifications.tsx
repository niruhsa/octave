// Notifications feed (Phase 10). Lists the user's new-release notifications,
// newest first, with an unread dot + "mark all read". Tapping a row marks it
// read and (for a new-release with a known album) opens that album.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import {
  notificationsList,
  notificationsMarkAllRead,
  notificationsMarkRead,
  type AppNotification,
} from "../ipc";
import { OfflineGate } from "../components/OfflineGate";
import { useNotificationsStore } from "../notifications/useNotifications";
import { formatError } from "../lib/error";
import { BellIcon, CheckIcon } from "../components/icons";
import { btnGhostSm } from "../lib/ui";
import { Skeleton } from "../components/Skeleton";

/** Compact "2h ago" style relative time from an ISO/RFC timestamp. */
function timeAgo(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const secs = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (secs < 60) return "just now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  if (days < 7) return `${days}d ago`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks}w ago`;
  return new Date(t).toLocaleDateString();
}

export default function Notifications() {
  return (
    <OfflineGate feature="Notifications">
      <NotificationsInner />
    </OfflineGate>
  );
}

function NotificationsInner() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const refreshBadge = useNotificationsStore((s) => s.refresh);

  const q = useQuery({
    queryKey: ["notifications", "list"],
    queryFn: () => notificationsList(false, 100),
  });

  const items = q.data?.notifications ?? [];
  const unread = q.data?.unread_count ?? 0;

  async function afterChange() {
    await qc.invalidateQueries({ queryKey: ["notifications"] });
    await refreshBadge();
  }

  async function open(n: AppNotification) {
    try {
      if (!n.read) {
        await notificationsMarkRead(n.id);
        await afterChange();
      }
    } catch (e) {
      // Marking read is best-effort; still navigate on a known target.
      console.warn("mark notification read failed", formatError(e));
    }
    if (n.kind === "new_episode" && n.podcast_id) navigate(`/podcasts/${n.podcast_id}`);
    else if (n.album_id) navigate(`/albums/${n.album_id}`);
    else if (n.artist_id) navigate(`/artists/${n.artist_id}`);
  }

  async function markAll() {
    try {
      await notificationsMarkAllRead();
      await afterChange();
    } catch (e) {
      alert(formatError(e));
    }
  }

  return (
    <section className="mx-auto flex max-w-2xl flex-col gap-5 p-6 md:p-8">
      <header className="flex items-end justify-between gap-3">
        <div className="flex min-w-0 flex-col">
          <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">
            NOTIFICATIONS
          </span>
          <h1 className="mt-1.5 flex items-center gap-2.5 text-3xl font-semibold tracking-tight">
            <BellIcon size={24} className="text-oct-muted" />
            Notifications
          </h1>
          <p className="mt-1.5 font-mono text-[12px] text-oct-subtle">
            {unread > 0 ? `${unread} unread` : "All caught up"}
          </p>
        </div>
        {unread > 0 && (
          <button onClick={markAll} className={btnGhostSm} title="Mark all as read">
            <CheckIcon size={13} /> Mark all read
          </button>
        )}
      </header>

      {q.isLoading && (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-16 w-full rounded-xl" />
          ))}
        </div>
      )}

      {q.isError && (
        <p className="rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-sm text-oct-danger">
          {formatError(q.error)}
        </p>
      )}

      {q.data && items.length === 0 && (
        <div className="flex flex-col items-center gap-3 rounded-2xl border border-oct-border bg-oct-panel/40 px-6 py-14 text-center">
          <span className="grid h-12 w-12 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
            <BellIcon size={22} />
          </span>
          <p className="text-sm text-oct-subtle">No notifications yet.</p>
          <p className="max-w-xs text-[12.5px] leading-relaxed text-oct-faint">
            Follow an artist and you'll be notified here when they release
            something new.
          </p>
        </div>
      )}

      {items.length > 0 && (
        <ul className="flex flex-col gap-1.5">
          {items.map((n) => (
            <li key={n.id}>
              <button
                onClick={() => void open(n)}
                className={`group flex w-full items-start gap-3 rounded-xl border px-3.5 py-3 text-left transition-colors ${
                  n.read
                    ? "border-oct-border bg-transparent hover:bg-oct-elevated/50"
                    : "border-oct-border-strong bg-oct-panel hover:bg-oct-elevated/70"
                }`}
              >
                <span className="mt-1 shrink-0">
                  <span
                    className={`block h-2 w-2 rounded-full ${
                      n.read ? "bg-transparent" : "bg-oct-accent"
                    }`}
                  />
                </span>
                <span className="min-w-0 flex-1">
                  <span
                    className={`block truncate text-[14px] ${
                      n.read ? "text-oct-muted" : "font-medium text-oct-text"
                    }`}
                  >
                    {n.title}
                  </span>
                  {n.body && (
                    <span className="mt-0.5 block truncate text-[12.5px] text-oct-subtle">
                      {n.body}
                    </span>
                  )}
                </span>
                <span className="mt-0.5 shrink-0 font-mono text-[10.5px] text-oct-faint">
                  {timeAgo(n.created_at)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
