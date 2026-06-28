// Bottom tab bar for small screens (the OCTAVE Android chrome). Hidden on
// md+ where the `Sidebar` takes over. Returns null with no session.

import { NavLink } from "react-router-dom";
import { useAppStore } from "../store";
import { useQuickSearchStore } from "../quicksearch/store";
import {
  DiscIcon,
  DownloadIcon,
  HomeIcon,
  PlaylistIcon,
  PodcastIcon,
  SearchIcon,
  type IconProps,
} from "./icons";

const TABS: { to: string; label: string; Icon: (p: IconProps) => React.ReactElement }[] = [
  { to: "/", label: "Home", Icon: HomeIcon },
  { to: "/library", label: "Library", Icon: DiscIcon },
  { to: "/playlists", label: "Playlists", Icon: PlaylistIcon },
  { to: "/podcasts", label: "Podcasts", Icon: PodcastIcon },
  { to: "/downloads", label: "Downloads", Icon: DownloadIcon },
];

export default function MobileNav() {
  const session = useAppStore((s) => s.session);
  const openQuickSearch = useQuickSearchStore((s) => s.openPalette);
  if (!session) return null;

  return (
    <nav
      aria-label="Primary"
      className="flex shrink-0 items-stretch justify-around border-t border-oct-border bg-oct-surface px-1 pt-2 md:hidden"
      // Pad above the Android gesture indicator / nav bar so the tabs aren't
      // overlapped (the bar background still fills the inset area).
      style={{ paddingBottom: "calc(env(safe-area-inset-bottom) + 0.375rem)" }}
    >
      <NavLink
        to="/"
        end
        className={({ isActive }) =>
          `flex flex-1 flex-col items-center gap-1 rounded-lg py-1 ${isActive ? "text-oct-accent" : "text-oct-subtle"}`
        }
      >
        {({ isActive }) => (
          <>
            <HomeIcon size={20} sw={1.3} />
            <span className={`text-[9.5px] ${isActive ? "font-medium" : ""}`}>Home</span>
          </>
        )}
      </NavLink>

      {/* Quick search — opens the palette instead of routing to a tab. */}
      <button
        onClick={openQuickSearch}
        aria-label="Quick search"
        className="flex flex-1 flex-col items-center gap-1 rounded-lg py-1 text-oct-subtle"
      >
        <SearchIcon size={20} sw={1.3} />
        <span className="text-[9.5px]">Search</span>
      </button>

      {TABS.slice(1).map((t) => (
        <NavLink
          key={t.to}
          to={t.to}
          className={({ isActive }) =>
            `flex flex-1 flex-col items-center gap-1 rounded-lg py-1 ${
              isActive ? "text-oct-accent" : "text-oct-subtle"
            }`
          }
        >
          {({ isActive }) => (
            <>
              <t.Icon size={20} sw={1.3} />
              <span className={`text-[9.5px] ${isActive ? "font-medium" : ""}`}>{t.label}</span>
            </>
          )}
        </NavLink>
      ))}
    </nav>
  );
}
