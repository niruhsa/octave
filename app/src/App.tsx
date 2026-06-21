import { useEffect, useCallback } from "react";
import { QueryClient, QueryClientProvider, useQueryClient } from "@tanstack/react-query";
import { RouterProvider, createBrowserRouter, Outlet } from "react-router-dom";
import Home from "./routes/Home";
import Login from "./routes/Login";
import Library from "./routes/Library";
import Artist from "./routes/Artist";
import Album from "./routes/Album";
import Search from "./routes/Search";
import Downloads from "./routes/Downloads";
import Playlists from "./routes/Playlists";
import PlaylistDetail from "./routes/PlaylistDetail";
import Register from "./routes/Register";
import Account from "./routes/Account";
import Upload from "./routes/Upload";
import Sidebar from "./components/Sidebar";
import MobileNav from "./components/MobileNav";
import PlayerBar from "./components/PlayerBar";
import { authSession } from "./ipc";
import { useAppStore } from "./store";
import { useSyncScheduler } from "./sync/useSync";
import { useDownloadListener } from "./downloads/useDownloads";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Always refetch on mount so the UI never shows stale data.
      // placeholderData keeps the previous view while fetching.
      staleTime: 0,
      refetchOnWindowFocus: true,
      retry: 1,
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
      const ch = new BroadcastChannel("music-app-query-sync");
      const handler = (e: MessageEvent<{ type: string; queryKey: string[] }>) => {
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
  if (!_bc) _bc = new BroadcastChannel("music-app-query-sync");
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

function RootLayout() {
  const setSession = useAppStore((s) => s.setSession);

  // Phase 5: schedule reconcile on online-regain / focus.
  useSyncScheduler();

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

  return (
    <div className="flex h-full flex-col overflow-hidden bg-oct-bg text-oct-text">
      <div className="flex min-h-0 flex-1">
        <Sidebar />
        <main className="oct-scroll min-w-0 flex-1 overflow-y-auto">
          <Outlet />
        </main>
      </div>
      <PlayerBar />
      <MobileNav />
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
      { path: "search", element: <Search /> },
      { path: "downloads", element: <Downloads /> },
      { path: "playlists", element: <Playlists /> },
      { path: "playlists/:id", element: <PlaylistDetail /> },
      { path: "register", element: <Register /> },
      { path: "account", element: <Account /> },
      { path: "upload", element: <Upload /> },
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
