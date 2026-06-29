import { useEffect, useCallback, useRef } from "react";
import {
  QueryClient,
  QueryClientProvider,
  useQueryClient,
} from "@tanstack/react-query";
import {
  RouterProvider,
  createBrowserRouter,
  Navigate,
  Outlet,
  useLocation,
  useNavigate,
} from "react-router-dom";
import Home from "./routes/Home";
import Login from "./routes/Login";
import Library from "./routes/Library";
import Artist from "./routes/Artist";
import Album from "./routes/Album";
import Downloads from "./routes/Downloads";
import Playlists from "./routes/Playlists";
import PlaylistDetail from "./routes/PlaylistDetail";
import Register from "./routes/Register";
import Account from "./routes/Account";
import Upload from "./routes/Upload";
import Uploads from "./routes/Uploads";
import Notifications from "./routes/Notifications";
import Stats from "./routes/Stats";
import Favorites from "./routes/Favorites";
import Podcasts from "./routes/Podcasts";
import PodcastDetail from "./routes/PodcastDetail";
import Settings from "./routes/Settings";
import Sidebar from "./components/Sidebar";
import MobileNav from "./components/MobileNav";
import MobileTopBar from "./components/MobileTopBar";
import PlayerBar from "./components/PlayerBar";
import QuickSearch from "./components/QuickSearch";
import { authSession, uploadsResumePending } from "./ipc";
import { syncNetworkPrefs } from "./settings/network";
import { useAppStore } from "./store";
import { useSyncScheduler } from "./sync/useSync";
import { useDownloadListener } from "./downloads/useDownloads";
import { useUploadEvents } from "./uploads/useUploads";
import { useNotificationsScheduler } from "./notifications/useNotifications";
import { useFavoritesStore } from "./favorites/useFavorites";
import { useHotkeys } from "./settings/useHotkeys";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Every read goes through Tauri `invoke` (local IPC to Rust, which does
      // its own server→cache fallback), never the browser network. React
      // Query's default `networkMode: "online"` would *pause* queries whenever
      // `navigator.onLine` is false, so offline the queryFn never runs and the
      // Rust cache fallback is never reached — Library / Playlists / Podcasts /
      // Downloads would render empty even with downloaded content on disk.
      // `"always"` runs queries regardless of the browser's online status.
      networkMode: "always",
      // Always refetch on mount so the UI never shows stale data.
      // placeholderData keeps the previous view while fetching.
      staleTime: 0,
      refetchOnWindowFocus: true,
      retry: 1,
    },
    // Same reasoning for mutations: offline playlist edits are queued through
    // `invoke` and must fire while offline rather than sit paused.
    mutations: {
      networkMode: "always",
    },
  },
});

/** Export for modules that can't use hooks (sync engine, etc.). */
export { queryClient };

/** Cross-tab query invalidation via BroadcastChannel. */
function useQuerySync() {
  const qc = useQueryClient();
  useEffect(() => {
    try {
      const ch = new BroadcastChannel("octave-query-sync");
      const handler = (
        e: MessageEvent<{ type: string; queryKey: string[] }>,
      ) => {
        if (e.data?.type === "invalidate" && e.data.queryKey) {
          qc.invalidateQueries({ queryKey: e.data.queryKey });
        }
      };
      ch.addEventListener("message", handler);
      return () => {
        ch.removeEventListener("message", handler);
        ch.close();
      };
    } catch {
      /* BroadcastChannel unavailable (e.g. some mobile webviews) */
    }
  }, [qc]);
}

/** Persistent singleton channel for sending invalidation messages.
 *  Creating a new BroadcastChannel per postMessage can drop messages
 *  due to registration timing in some engines. */
let _bc: BroadcastChannel | null = null;
function bc(): BroadcastChannel | null {
  if (typeof BroadcastChannel === "undefined") return null;
  if (!_bc) _bc = new BroadcastChannel("octave-query-sync");
  return _bc;
}

/** Post an invalidation to all tabs (including current). */
export function broadcastInvalidate(queryKey: string[]) {
  try {
    bc()?.postMessage({ type: "invalidate", queryKey });
    // Also invalidate locally — BroadcastChannel delivers to the sending
    // tab, but some engines delay registration so use an explicit call too.
    queryClient.invalidateQueries({ queryKey });
  } catch {
    /* unavailable */
  }
}

const ROUTE_KEY = "octave:route";

function RootLayout() {
  const setSession = useAppStore((s) => s.setSession);

  // Restore the last route on a cold launch, then record it as the user
  // navigates. On mobile the OS can kill the backgrounded app; without this the
  // WebView reloads at "/" and the user loses their place. (The playback queue
  // is restored separately in the player store.)
  const location = useLocation();
  const navigate = useNavigate();
  // `navigate(replace)` doesn't update `location` until the next render, so the
  // recorder below would otherwise overwrite the saved route with "/" before the
  // restore lands. Skip that one stale write.
  const skipRecord = useRef(0);
  useEffect(() => {
    try {
      const saved = localStorage.getItem(ROUTE_KEY);
      const here = location.pathname + location.search;
      if (saved && saved !== here) {
        skipRecord.current = 1;
        navigate(saved, { replace: true });
      }
    } catch {
      /* storage unavailable */
    }
    // Run once, on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    if (skipRecord.current > 0) {
      skipRecord.current -= 1;
      return;
    }
    try {
      localStorage.setItem(ROUTE_KEY, location.pathname + location.search);
    } catch {
      /* storage unavailable */
    }
  }, [location]);

  // Phase 5: schedule reconcile on online-regain / focus.
  useSyncScheduler();

  // Uploads v2: keep active-upload state alive across tab navigation and
  // refresh the library/reports on completion (global, not tab-local).
  useUploadEvents();

  // Phase 10: poll the unread notification feed (badge + OS notifications).
  useNotificationsScheduler();

  // Phase 6: aggregate download-progress events + read storage usage.
  // When a download finishes (done/error), invalidate all library and
  // downloads queries so every page picks up the change automatically.
  const onDownloadComplete = useCallback(() => {
    broadcastInvalidate(["library"]);
    broadcastInvalidate(["cache", "downloaded_tracks"]);
  }, []);
  useDownloadListener(onDownloadComplete);

  // Cross-tab query invalidation.
  useQuerySync();

  // Keyboard shortcuts (play/next/nav/…), user-configurable in Settings.
  useHotkeys();

  // On boot, ask Rust for any cached session so the UI starts with the
  // right tier without a network round-trip. Errors are non-fatal — they
  // just mean no server was ever configured.
  useEffect(() => {
    let cancelled = false;
    authSession()
      .then((s) => {
        if (!cancelled) setSession(s);
      })
      .catch(() => {
        /* no manager yet — user lands on /login */
      });
    return () => {
      cancelled = true;
    };
  }, [setSession]);

  // Once a session is available (auth configured — via boot restore or a fresh
  // login), pick up any upload left in flight by a previous run (e.g. the OS
  // killed the backgrounded app). The command no-ops when there's nothing to
  // resume or an upload is already running, so re-firing on session change is
  // safe.
  const session = useAppStore((s) => s.session);
  useEffect(() => {
    if (session) void uploadsResumePending().catch(() => {});
  }, [session]);

  // Phase 11: hydrate the favorited-track set for a bearer-user session (drives
  // instant heart state on track rows + the player bar); clear it otherwise.
  useEffect(() => {
    if (session?.kind === "bearer") void useFavoritesStore.getState().load();
    else useFavoritesStore.getState().clear();
  }, [session]);

  // Push the persisted Networking prefs (chunk upload concurrency) to the
  // backend on startup, so an upload honours the user's setting from the first
  // chunk — before they ever open Settings this run.
  useEffect(() => {
    syncNetworkPrefs();
  }, []);

  return (
    <div className="flex h-full flex-col overflow-hidden bg-oct-bg text-oct-text">
      <MobileTopBar />
      <div className="flex min-h-0 flex-1">
        <Sidebar />
        <main className="oct-scroll min-w-0 flex-1 overflow-y-auto">
          <Outlet />
        </main>
      </div>
      <PlayerBar />
      <MobileNav />
      <QuickSearch />
    </div>
  );
}

const router = createBrowserRouter([
  {
    path: "/",
    element: <RootLayout />,
    children: [
      { index: true, element: <Home /> },
      { path: "login", element: <Login /> },
      { path: "library", element: <Library /> },
      { path: "artists/:id", element: <Artist /> },
      { path: "albums/:id", element: <Album /> },
      // The standalone Search route was replaced by the Quick Search palette
      // (⌘K). Redirect any persisted/bookmarked `/search` deep-link home.
      { path: "search", element: <Navigate to="/" replace /> },
      { path: "downloads", element: <Downloads /> },
      { path: "playlists", element: <Playlists /> },
      { path: "playlists/:id", element: <PlaylistDetail /> },
      { path: "podcasts", element: <Podcasts /> },
      { path: "podcasts/:id", element: <PodcastDetail /> },
      { path: "register", element: <Register /> },
      { path: "account", element: <Account /> },
      { path: "upload", element: <Upload /> },
      { path: "uploads", element: <Uploads /> },
      { path: "notifications", element: <Notifications /> },
      { path: "stats", element: <Stats /> },
      { path: "favorites", element: <Favorites /> },
      { path: "settings", element: <Settings /> },
    ],
  },
]);

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
