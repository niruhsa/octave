// Permanent left navigation rail — OCTAVE "Obsidian" styling.
//
// Rendered by `RootLayout` on every authenticated route (hidden on small
// screens, where `MobileNav` takes over). Carries the OCTAVE wordmark, a
// search field, the primary nav (active route gets an amber edge), a LIBRARY
// group, a live SERVER/SYNC status panel wired to the app + sync stores, and
// a user/sign-out footer. Returns null with no session so Login stays
// full-width.

import { NavLink, useNavigate } from "react-router-dom";
import { authLogout, authRefreshOnline } from "../ipc";
import { useAppStore } from "../store";
import { useSyncStore } from "../sync/useSync";
import { useDownloadsStore } from "../downloads/useDownloads";
import {
  ArtistIcon,
  CloudOffIcon,
  DiscIcon,
  DownloadIcon,
  GearIcon,
  HomeIcon,
  PlaylistIcon,
  PlusIcon,
  PowerIcon,
  SearchIcon,
  SyncIcon,
  UploadIcon,
  type IconProps,
} from "./icons";
import { OFFLINE_MSG } from "./OfflineGate";

type NavItem = {
  to: string;
  label: string;
  Icon: (p: IconProps) => React.ReactElement;
  adminOnly?: boolean;
  managerOnly?: boolean;
  /** Render in the secondary "LIBRARY" group instead of the primary group. */
  group?: "library";
  badge?: "downloads" | "pending";
  /** Dim + tooltip when offline — the route is connection-only. */
  requiresConnection?: boolean;
};

const NAV: NavItem[] = [
  { to: "/", label: "Home", Icon: HomeIcon },
  { to: "/library", label: "Library", Icon: DiscIcon },
  { to: "/search", label: "Search", Icon: SearchIcon },
  { to: "/playlists", label: "Playlists", Icon: PlaylistIcon, badge: "pending" },
  { to: "/downloads", label: "Downloads", Icon: DownloadIcon, group: "library", badge: "downloads" },
  { to: "/upload", label: "Upload", Icon: UploadIcon, group: "library", managerOnly: true, requiresConnection: true },
  { to: "/account", label: "Account", Icon: ArtistIcon, group: "library", requiresConnection: true },
  { to: "/register", label: "Create account", Icon: PlusIcon, group: "library", adminOnly: true, requiresConnection: true },
];

export default function Sidebar() {
  const session = useAppStore((s) => s.session);
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const setOnline = useAppStore((s) => s.setOnline);
  const setSession = useAppStore((s) => s.setSession);
  const navigate = useNavigate();

  const syncStatus = useSyncStore((s) => s.status);
  const pending = useSyncStore((s) => s.pending);
  const runSync = useSyncStore((s) => s.run);
  const downloads = useDownloadsStore((s) => s.storage);

  if (!session) return null;

  const visible = (n: NavItem) =>
    (!n.adminOnly || tier === "admin") &&
    (!n.managerOnly || tier === "admin" || tier === "manager");

  const primary = NAV.filter((n) => !n.group && visible(n));
  const library = NAV.filter((n) => n.group === "library" && visible(n));

  async function logout() {
    await authLogout();
    setSession(null);
    navigate("/login");
  }

  const badgeFor = (n: NavItem): number | null => {
    if (n.badge === "downloads") return downloads?.track_count ?? null;
    if (n.badge === "pending") return pending > 0 ? pending : null;
    return null;
  };

  const renderItem = (n: NavItem) => {
    const count = badgeFor(n);
    const gated = !!n.requiresConnection && !online;
    return (
      <NavLink
        key={n.to}
        to={n.to}
        end={n.to === "/"}
        title={gated ? `${n.label} — ${OFFLINE_MSG}` : n.label}
        className={({ isActive }) =>
          `group relative flex items-center gap-3 rounded-lg px-2.5 py-2 text-[13.5px] transition-colors ${
            isActive
              ? "bg-oct-elevated text-oct-text"
              : "text-oct-muted hover:bg-oct-elevated/60 hover:text-oct-text"
          } ${gated ? "opacity-40" : ""}`
        }
      >
        {({ isActive }) => (
          <>
            {isActive && (
              <span className="absolute inset-y-2 left-0 w-[2.5px] rounded-full bg-oct-accent" />
            )}
            <n.Icon size={16} className="shrink-0" />
            <span className="flex-1 truncate">{n.label}</span>
            {gated && <CloudOffIcon size={13} className="shrink-0 text-oct-faint" />}
            {count !== null && !gated && (
              <span
                className={`font-mono text-[10.5px] ${
                  n.badge === "pending" ? "text-oct-accent" : "text-oct-faint"
                }`}
              >
                {count}
              </span>
            )}
          </>
        )}
      </NavLink>
    );
  };

  return (
    <nav
      aria-label="Primary"
      className="hidden h-full w-[248px] shrink-0 flex-col border-r border-oct-border bg-oct-surface px-3 py-5 md:flex"
    >
      {/* wordmark */}
      <div className="flex items-center gap-2.5 px-1.5 pb-4">
        <span className="block h-4 w-4 rounded bg-oct-accent" />
        <span className="text-base font-semibold tracking-[0.16em]">OCTAVE</span>
      </div>

      {/* search field → /search */}
      <NavLink
        to="/search"
        className="mb-4 flex items-center gap-2.5 rounded-lg border border-oct-border-strong bg-oct-card px-3 py-2 text-[13px] text-oct-subtle transition-colors hover:text-oct-muted"
      >
        <SearchIcon size={15} sw={1.4} />
        <span>Search</span>
      </NavLink>

      {/* nav (scrolls if it overflows) */}
      <div className="oct-scroll flex min-h-0 flex-1 flex-col overflow-y-auto">
        <div className="flex flex-col gap-0.5">{primary.map(renderItem)}</div>

        {library.length > 0 && (
          <>
            <div className="mx-1.5 my-4 h-px bg-oct-border" />
            <div className="px-2 pb-2 font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">
              LIBRARY
            </div>
            <div className="flex flex-col gap-0.5">{library.map(renderItem)}</div>
          </>
        )}
      </div>

      <StatusPanel
        online={online}
        syncStatus={syncStatus}
        pending={pending}
        onRecheck={async () => setOnline(await authRefreshOnline())}
        onSync={() => void runSync()}
      />

      {/* user footer */}
      <div className="mt-3 flex items-center gap-2 border-t border-oct-border px-1 pt-3">
        <NavLink
          to="/account"
          className="flex min-w-0 flex-1 items-center gap-2 rounded-lg px-1.5 py-1 hover:bg-oct-elevated/60"
          title="Account"
        >
          <span className="grid h-7 w-7 shrink-0 place-items-center rounded-full bg-oct-elevated text-oct-muted">
            <GearIcon size={14} />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-[12.5px] text-oct-text">
              {session.username ?? session.kind}
            </span>
            <span className="block font-mono text-[9.5px] uppercase tracking-wide text-oct-faint">
              {tier}
            </span>
          </span>
        </NavLink>
        <button
          onClick={logout}
          title="Sign out"
          className="grid h-7 w-7 shrink-0 place-items-center rounded-lg text-oct-subtle hover:bg-oct-offline/15 hover:text-oct-danger"
        >
          <PowerIcon size={15} />
        </button>
      </div>
    </nav>
  );
}

function StatusPanel({
  online,
  syncStatus,
  pending,
  onRecheck,
  onSync,
}: {
  online: boolean;
  syncStatus: "idle" | "syncing" | "ok" | "error";
  pending: number;
  onRecheck: () => void;
  onSync: () => void;
}) {
  const syncing = syncStatus === "syncing";
  const syncColor = !online
    ? "text-oct-subtle"
    : syncing
      ? "text-oct-accent"
      : pending > 0
        ? "text-oct-accent"
        : "text-oct-online";
  const syncLabel = !online
    ? "Sync paused"
    : syncing
      ? "Syncing library…"
      : pending > 0
        ? `${pending} change${pending === 1 ? "" : "s"} pending`
        : "Library up to date";
  const syncSub = !online
    ? "Reconnect to resume sync"
    : syncing
      ? "pushing edits · pulling changes"
      : pending > 0
        ? "tap to sync now"
        : "everything in sync";

  return (
    <div className="mt-4 rounded-xl border border-oct-border-strong bg-oct-panel p-3.5">
      {/* server row */}
      <button
        onClick={onRecheck}
        className="flex w-full items-center justify-between"
        title="Re-check server reachability"
      >
        <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">
          SERVER
        </span>
        <span className="flex items-center gap-2">
          <span
            className={`inline-block h-2 w-2 rounded-full ${
              online ? "bg-oct-online animate-octpulse" : "bg-oct-offline"
            }`}
            style={
              online
                ? { boxShadow: "0 0 0 3px rgba(63,185,80,0.15)" }
                : { boxShadow: "0 0 0 3px rgba(138,90,74,0.12)" }
            }
          />
          <span className={`text-xs font-medium ${online ? "text-oct-online" : "text-oct-offline"}`}>
            {online ? "Online" : "Offline"}
          </span>
        </span>
      </button>

      <div className="my-3 h-px bg-oct-border-strong" />

      {/* sync row */}
      <button onClick={onSync} disabled={syncing} className="flex w-full items-start gap-2.5 text-left">
        <SyncIcon size={15} className={`mt-0.5 shrink-0 ${syncColor} ${syncing ? "animate-octspin" : ""}`} />
        <span className="min-w-0 flex-1">
          <span className={`block text-[12.5px] font-medium ${syncColor}`}>{syncLabel}</span>
          <span className="mt-0.5 block truncate font-mono text-[10px] text-oct-subtle">
            {syncSub}
          </span>
        </span>
      </button>

      {syncing && (
        <div className="mt-2.5 h-[3px] overflow-hidden rounded-full bg-oct-border-strong">
          <div className="h-full w-[62%] rounded-full bg-oct-accent" />
        </div>
      )}
    </div>
  );
}
