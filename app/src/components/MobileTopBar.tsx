// Mobile top bar (md:hidden). The desktop Sidebar is hidden on phones and the
// bottom MobileNav only holds the primary content tabs, so this bar exposes the
// remaining destinations — Upload (Manager+), Account, Create account (Admin) —
// plus sync + sign-out, via an overflow menu. Without it those routes are
// unreachable on Android. Returns null with no session.

import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { authLogout, authRefreshTransports } from "../ipc";
import { useAppStore } from "../store";
import { useSyncStore } from "../sync/useSync";
import { useNotificationsStore } from "../notifications/useNotifications";
import { TransportStatus } from "./TransportStatus";
import {
  ArtistIcon,
  BellIcon,
  HomeIcon,
  MenuIcon,
  PlusIcon,
  PowerIcon,
  SyncIcon,
  UploadIcon,
  type IconProps,
} from "./icons";

export default function MobileTopBar() {
  const session = useAppStore((s) => s.session);
  const tier = useAppStore((s) => s.tier);
  const transports = useAppStore((s) => s.transports);
  const setTransports = useAppStore((s) => s.setTransports);
  const setSession = useAppStore((s) => s.setSession);
  const navigate = useNavigate();

  const pending = useSyncStore((s) => s.pending);
  const syncStatus = useSyncStore((s) => s.status);
  const runSync = useSyncStore((s) => s.run);
  const unread = useNotificationsStore((s) => s.unreadCount);

  const [open, setOpen] = useState(false);

  if (!session) return null;

  const isManager = tier === "admin" || tier === "manager";
  const isAdmin = tier === "admin";
  // Only a logged-in user (bearer) has per-user follows + notifications.
  const isUser = session.kind === "bearer";

  function go(to: string) {
    setOpen(false);
    navigate(to);
  }
  async function logout() {
    setOpen(false);
    await authLogout();
    setSession(null);
    navigate("/login");
  }

  return (
    <header
      className="relative z-40 flex shrink-0 items-center justify-between border-b border-oct-border bg-oct-surface px-4 pb-2.5 md:hidden"
      // Pad below the Android status bar / display cutout so the bar's content
      // isn't drawn underneath it (the bar background still fills the inset).
      style={{ paddingTop: "calc(env(safe-area-inset-top) + 0.625rem)" }}
    >
      <button onClick={() => go("/")} className="flex items-center gap-2">
        <span className="block h-4 w-4 rounded bg-oct-accent" />
        <span className="text-[15px] font-semibold tracking-[0.16em]">OCTAVE</span>
      </button>

      <div className="flex items-center gap-3.5">
        <button
          onClick={async () => setTransports(await authRefreshTransports())}
          title="Re-check server"
          className="flex items-center"
        >
          <TransportStatus transports={transports} compact />
        </button>
        {isUser && (
          <button
            onClick={() => navigate("/notifications")}
            aria-label="Notifications"
            className="relative text-oct-muted"
          >
            <BellIcon size={19} />
            {unread > 0 && (
              <span className="absolute -right-1.5 -top-1.5 grid h-[15px] min-w-[15px] place-items-center rounded-full bg-oct-accent px-1 text-[9px] font-semibold leading-none text-black">
                {unread > 99 ? "99+" : unread}
              </span>
            )}
          </button>
        )}
        <button onClick={() => setOpen((v) => !v)} aria-label="Menu" className="text-oct-muted">
          <MenuIcon size={20} />
        </button>
      </div>

      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div className="absolute right-3 top-full z-40 mt-1 w-56 overflow-hidden rounded-xl border border-oct-border-strong bg-oct-surface shadow-[0_20px_50px_-18px_rgba(0,0,0,0.6)]">
            <MenuItem Icon={HomeIcon} label="Home" onClick={() => go("/")} />
            {isManager && <MenuItem Icon={UploadIcon} label="Upload" onClick={() => go("/upload")} />}
            <MenuItem Icon={ArtistIcon} label="Account" onClick={() => go("/account")} />
            {isAdmin && <MenuItem Icon={PlusIcon} label="Create account" onClick={() => go("/register")} />}
            <div className="h-px bg-oct-border" />
            <button
              onClick={() => {
                setOpen(false);
                void runSync();
              }}
              disabled={syncStatus === "syncing"}
              className="flex w-full items-center gap-3 px-3.5 py-2.5 text-left text-[13.5px] text-oct-muted hover:bg-oct-elevated/60 disabled:opacity-50"
            >
              <SyncIcon size={16} className={syncStatus === "syncing" ? "animate-octspin" : ""} />
              <span className="flex-1">{syncStatus === "syncing" ? "Syncing…" : "Sync now"}</span>
              {pending > 0 && <span className="font-mono text-[11px] text-oct-accent">{pending}</span>}
            </button>
            <button
              onClick={logout}
              className="flex w-full items-center gap-3 px-3.5 py-2.5 text-left text-[13.5px] text-oct-danger hover:bg-oct-offline/15"
            >
              <PowerIcon size={16} />
              Sign out
            </button>
          </div>
        </>
      )}
    </header>
  );
}

function MenuItem({
  Icon,
  label,
  onClick,
}: {
  Icon: (p: IconProps) => React.ReactElement;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="flex w-full items-center gap-3 px-3.5 py-2.5 text-left text-[13.5px] text-oct-muted hover:bg-oct-elevated/60 hover:text-oct-text"
    >
      <Icon size={16} />
      {label}
    </button>
  );
}
