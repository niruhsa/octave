import { useEffect } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createBrowserRouter, Outlet } from "react-router-dom";
import Home from "./routes/Home";
import Login from "./routes/Login";
import Library from "./routes/Library";
import Artist from "./routes/Artist";
import Album from "./routes/Album";
import Search from "./routes/Search";
import PlayerBar from "./components/PlayerBar";
import { authSession } from "./ipc";
import { useAppStore } from "./store";
import { useSyncScheduler } from "./sync/useSync";

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
    <main className="min-h-full p-6 pb-28">
      <Outlet />
      <PlayerBar />
    </main>
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
