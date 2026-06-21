// Bottom tab bar for small screens (the OCTAVE Android chrome). Hidden on
// md+ where the `Sidebar` takes over. Returns null with no session.

import { NavLink } from "react-router-dom";
import { useAppStore } from "../store";
import {
  DiscIcon,
  DownloadIcon,
  HomeIcon,
  PlaylistIcon,
  SearchIcon,
  type IconProps,
} from "./icons";

const TABS: { to: string; label: string; Icon: (p: IconProps) => React.ReactElement }[] = [
  { to: "/", label: "Home", Icon: HomeIcon },
  { to: "/library", label: "Library", Icon: DiscIcon },
  { to: "/search", label: "Search", Icon: SearchIcon },
  { to: "/playlists", label: "Playlists", Icon: PlaylistIcon },
  { to: "/downloads", label: "Downloads", Icon: DownloadIcon },
];

export default function MobileNav() {
  const session = useAppStore((s) => s.session);
  if (!session) return null;

  return (
    <nav
      aria-label="Primary"
      className="flex shrink-0 items-stretch justify-around border-t border-oct-border bg-oct-surface px-1 pb-1.5 pt-2 md:hidden"
    >
      {TABS.map((t) => (
        <NavLink
          key={t.to}
          to={t.to}
          end={t.to === "/"}
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
