import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  appInfo,
  authRefreshOnline,
  cacheListDownloadedTracks,
} from "../ipc";
import { useAppStore } from "../store";
import { useSyncStore } from "../sync/useSync";

/**
 * Home dashboard. Navigation lives in the permanent `Sidebar` (rendered
 * by `RootLayout`); this view is a status landing page — IPC bridge smoke
 * test, offline-cache reachability, and the online/tier/sync summary.
 * The anonymous case (no session yet) shows a Sign-in link since the
 * sidebar is hidden without a session.
 */
export default function Home() {
  const online = useAppStore((s) => s.online);
  const tier = useAppStore((s) => s.tier);
  const session = useAppStore((s) => s.session);
  const setOnline = useAppStore((s) => s.setOnline);

  const syncStatus = useSyncStore((s) => s.status);
  const pending = useSyncStore((s) => s.pending);
  const lastReport = useSyncStore((s) => s.lastReport);
  const lastError = useSyncStore((s) => s.lastError);
  const runSync = useSyncStore((s) => s.run);

  const info = useQuery({ queryKey: ["app_info"], queryFn: appInfo });
  const downloads = useQuery({
    queryKey: ["cache", "downloaded_tracks"],
    queryFn: cacheListDownloadedTracks,
  });

  async function refresh() {
    setOnline(await authRefreshOnline());
  }

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <h1 className="text-2xl font-semibold">music-app</h1>
          <p className="text-sm text-neutral-400">
            Phase 7 — playlists, downloads, sync.
          </p>
        </div>
        {!session && (
          <Link
            to="/login"
            className="rounded bg-blue-600 px-3 py-1.5 text-sm text-white"
          >
            Sign in
          </Link>
        )}
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
          <Link to="/downloads" className="text-blue-400 hover:underline">
            {downloads.isLoading
              ? "loading…"
              : downloads.isError
                ? `error: ${(downloads.error as Error).message}`
                : `${downloads.data?.length ?? 0} downloaded`}
          </Link>
        </dd>
        <dt className="text-neutral-400">sync</dt>
        <dd className="flex items-center gap-2">
          <span>
            {syncStatus === "syncing"
              ? "syncing…"
              : syncStatus === "error"
                ? `error: ${lastError}`
                : syncStatus === "ok"
                  ? "up to date"
                  : "idle"}
          </span>
          {pending > 0 && (
            <span className="rounded bg-amber-900/40 px-1.5 py-0.5 text-xs text-amber-200">
              {pending} unsynced
            </span>
          )}
          {session && (
            <button
              onClick={() => runSync()}
              disabled={syncStatus === "syncing"}
              className="text-xs text-blue-400 underline disabled:opacity-50"
            >
              sync now
            </button>
          )}
          {lastReport && syncStatus === "ok" && (
            <span className="text-xs text-neutral-500">
              ↑{lastReport.ops_pushed} ↻{lastReport.entities_updated} ✕
              {lastReport.entities_pruned}
              {lastReport.conflicts.length > 0 &&
                ` · ${lastReport.conflicts.length} conflict(s)`}
            </span>
          )}
        </dd>
      </dl>
    </section>
  );
}
