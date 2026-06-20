// Permanent left navigation rail.
//
// Rendered by `RootLayout` whenever a session is active, on every
// authenticated route — so a page with no in-content hyperlinks (e.g. an
// Album, a single Playlist) can never leave the user stranded. The Login
// screen is the only route without it (no session yet).
//
// Nav items are `NavLink`s so the active route is highlighted. The footer
// carries the online indicator + sync status (reused from the Phase 5
// store) and the sign-out action, so those controls are reachable from
// anywhere too.

import { NavLink, useNavigate } from "react-router-dom";
import { authLogout, authRefreshOnline } from "../ipc";
import { useAppStore } from "../store";
import { useSyncStore } from "../sync/useSync";

type NavItem = {
  to: string;
  label: string;
  icon: string;
  /** When true, only render for admin tier. */
  adminOnly?: boolean;
  /** When true, only render for manager+ tier. */
  managerOnly?: boolean;
};

const NAV: NavItem[] = [
  { to: "/", label: "Home", icon: "⌂" },
  { to: "/library", label: "Library", icon: "♪" },
  { to: "/search", label: "Search", icon: "⌕" },
  { to: "/playlists", label: "Playlists", icon: "☰" },
  { to: "/downloads", label: "Downloads", icon: "⬇" },
  { to: "/upload", label: "Upload", icon: "⬆", managerOnly: true },
  { to: "/account", label: "Account", icon: "♯" },
  { to: "/register", label: "Create account", icon: "✚", adminOnly: true },
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

  if (!session) return null;

  async function refresh() {
    setOnline(await authRefreshOnline());
  }

  async function logout() {
    await authLogout();
    setSession(null);
    navigate("/login");
  }

  return (
    <nav
      aria-label="Primary"
      className="flex h-full w-14 shrink-0 flex-col gap-1 border-r border-neutral-800 bg-neutral-950/60 px-1 py-3 sm:w-52"
    >
      <div className="mb-1 hidden px-2 text-xs font-semibold tracking-wide text-neutral-500 sm:block">
        music-app
      </div>

      <ul className="flex flex-1 flex-col gap-1">
        {NAV.filter(
          (n) =>
            (!n.adminOnly || tier === "admin") &&
            (!n.managerOnly || tier === "admin" || tier === "manager"),
        ).map((n) => (
          <li key={n.to}>
            <NavLink
              to={n.to}
              end={n.to === "/"}
              title={n.label}
              className={({ isActive }) =>
                `flex items-center gap-2 rounded px-2 py-1.5 text-sm ${
                  isActive
                    ? "bg-neutral-800 text-white"
                    : "text-neutral-400 hover:bg-neutral-800/50 hover:text-neutral-200"
                }`
              }
            >
              <span className="w-4 text-center text-base leading-none">
                {n.icon}
              </span>
              <span className="hidden sm:inline">{n.label}</span>
              {n.to === "/playlists" && pending > 0 && (
                <span className="hidden rounded bg-amber-900/40 px-1 text-xs text-amber-200 sm:inline">
                  {pending}
                </span>
              )}
            </NavLink>
          </li>
        ))}
      </ul>

      {/* Footer: online + sync + session. */}
      <div className="mt-2 flex flex-col gap-2 border-t border-neutral-800 pt-2 text-xs text-neutral-400">
        <button
          onClick={refresh}
          className="flex items-center gap-2 rounded px-2 py-1 text-left hover:bg-neutral-800/50"
          title="Click to re-check server reachability"
        >
          <span
            className={`inline-block h-2 w-2 rounded-full ${
              online ? "bg-emerald-400" : "bg-red-500"
            }`}
          />
          <span className="hidden sm:inline">
            {online ? "online" : "offline"}
          </span>
        </button>

        <button
          onClick={() => void runSync()}
          disabled={syncStatus === "syncing"}
          className="flex items-center gap-2 rounded px-2 py-1 text-left hover:bg-neutral-800/50 disabled:opacity-50"
          title="Sync now"
        >
          <span>{syncStatus === "syncing" ? "↻" : "⟳"}</span>
          <span className="hidden sm:inline">
            {syncStatus === "syncing"
              ? "syncing…"
              : pending > 0
                ? `${pending} unsynced`
                : "synced"}
          </span>
        </button>

        <div className="hidden items-center gap-2 px-2 py-1 sm:flex">
          <span className="truncate text-neutral-500">
            {session.username ?? session.kind}
          </span>
          <span className="ml-auto rounded bg-neutral-800 px-1 py-0.5 text-[10px] uppercase tracking-wide text-neutral-400">
            {tier}
          </span>
        </div>

        <button
          onClick={logout}
          className="flex items-center gap-2 rounded px-2 py-1 text-left text-red-300 hover:bg-red-900/20"
        >
          <span>⏻</span>
          <span className="hidden sm:inline">Sign out</span>
        </button>
      </div>
    </nav>
  );
}