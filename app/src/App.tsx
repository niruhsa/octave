import { useEffect } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
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
import PlayerBar from "./components/PlayerBar";
import { authSession } from "./ipc";
import { useAppStore } from "./store";
import { useSyncScheduler } from "./sync/useSync";
import { useDownloadListener } from "./downloads/useDownloads";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Server is authority when online; cache offline. Phase 5 tunes this.
      staleTime: 30_000,
      retry: 1,
    },
  },
});

function RootLayout() {
  const setSession = useAppStore((s) => s.setSession);

  // Phase 5: schedule reconcile on online-regain / focus.
  useSyncScheduler();

  // Phase 6: aggregate download-progress events + read storage usage.
  useDownloadListener();

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
    <div className="flex min-h-full">
      <Sidebar />
      <div className="flex min-h-full flex-1 flex-col">
        <main className="flex-1 overflow-auto p-6 pb-28">
          <Outlet />
        </main>
        <PlayerBar />
      </div>
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
