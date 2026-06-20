import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  appInfo,
  authLogout,
  authRefreshOnline,
  cacheListDownloadedTracks,
} from "../ipc";
import { useAppStore } from "../store";

/**
 * Home view. Verifies:
 *   - React → Rust IPC bridge (`app_info`)
 *   - Offline cache pool is reachable (`cache_list_downloaded_tracks`)
 *   - Auth tier + online flag come from `AuthManager`
 */
export default function Home() {
  const online = useAppStore((s) => s.online);
  const tier = useAppStore((s) => s.tier);
  const session = useAppStore((s) => s.session);
  const setSession = useAppStore((s) => s.setSession);
  const setOnline = useAppStore((s) => s.setOnline);

  const info = useQuery({ queryKey: ["app_info"], queryFn: appInfo });
  const downloads = useQuery({
    queryKey: ["cache", "downloaded_tracks"],
    queryFn: cacheListDownloadedTracks,
  });

  async function refresh() {
    setOnline(await authRefreshOnline());
  }

  async function logout() {
    await authLogout();
    setSession(null);
  }

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold">music-app</h1>
          <p className="text-sm text-neutral-400">
            Phase 2 — server transport + auth wired.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {session && (
            <>
              <Link
                to="/library"
                className="rounded border border-neutral-700 px-2 py-1 text-sm hover:bg-neutral-800"
              >
                Library
              </Link>
              <Link
                to="/search"
                className="rounded border border-neutral-700 px-2 py-1 text-sm hover:bg-neutral-800"
              >
                Search
              </Link>
            </>
          )}
          {session ? (
            <button
              onClick={logout}
              className="rounded border border-neutral-700 px-2 py-1 text-sm hover:bg-neutral-800"
            >
              Sign out
            </button>
          ) : (
            <Link
              to="/login"
              className="rounded bg-blue-600 px-3 py-1.5 text-sm text-white"
            >
              Sign in
            </Link>
          )}
        </div>
      </header>

      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
        <dt className="text-neutral-400">online</dt>
        <dd>
          {String(online)}{" "}
          <button
            onClick={refresh}
            className="text-xs text-blue-400 underline"
          >
            refresh
          </button>
        </dd>
        <dt className="text-neutral-400">tier</dt>
        <dd>{tier}</dd>
        <dt className="text-neutral-400">user</dt>
        <dd>{session?.username ?? session?.kind ?? "(anonymous)"}</dd>
        <dt className="text-neutral-400">app</dt>
        <dd>
          {info.isLoading
            ? "loading…"
            : info.isError
              ? `error: ${(info.error as Error).message}`
              : `${info.data?.name} ${info.data?.version} (tauri ${info.data?.tauri_version})`}
        </dd>
        <dt className="text-neutral-400">offline tracks</dt>
        <dd>
          {downloads.isLoading
            ? "loading…"
            : downloads.isError
              ? `error: ${(downloads.error as Error).message}`
              : `${downloads.data?.length ?? 0} downloaded`}
        </dd>
      </dl>
    </section>
  );
}
