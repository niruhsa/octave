import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { Link } from "react-router-dom";
import {
  cacheListDownloadedTracks,
  downloadDelete,
  downloadsDir,
  downloadsWifiOnly,
  downloadsSetWifiOnly,
} from "../ipc";
import {
  formatBytes,
  useDownloadsStore,
} from "../downloads/useDownloads";
import { formatError } from "../lib/error";
import { formatDuration } from "../lib/format";
import { broadcastInvalidate } from "../App";

/**
 * Offline-downloads management view (Phase 6):
 *   - storage usage (bytes / track count / cover count),
 *   - downloads root (+ Wi-Fi-only toggle),
 *   - per-track delete (removes file + cache row; prunes empty album cover),
 *   - active-download progress bars fed by the `download-progress` event.
 */
export default function Downloads() {
  const qc = useQueryClient();
  const storage = useDownloadsStore((s) => s.storage);
  const active = useDownloadsStore((s) => s.active);
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);
  const clear = useDownloadsStore((s) => s.clear);

  const tracks = useQuery({
    queryKey: ["cache", "downloaded_tracks"],
    queryFn: cacheListDownloadedTracks,
  });

  const dir = useQuery({ queryKey: ["downloads", "dir"], queryFn: downloadsDir });
  const wifiOnly = useQuery({
    queryKey: ["downloads", "wifi_only"],
    queryFn: downloadsWifiOnly,
  });

  // Refresh storage usage when the tracks list changes (post-delete etc.).
  const trackCount = tracks.data?.length ?? 0;
  useEffect(() => {
    void refreshStorage();
  }, [trackCount, refreshStorage]);

  const activeList = Object.values(active);

  async function remove(id: string) {
    try {
      await downloadDelete(id);
      broadcastInvalidate(["library"]);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["cache", "downloaded_tracks"] }),
        qc.invalidateQueries({ queryKey: ["library"] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <div>
          <Link to="/" className="text-sm text-blue-400 hover:underline">
            ← Home
          </Link>
          <h1 className="text-2xl font-semibold">Downloads</h1>
          <p className="text-sm text-neutral-400">
            Offline content — playable without the server.
          </p>
        </div>
        <Link to="/library" className="text-sm text-blue-400 hover:underline">
          Browse to download
        </Link>
      </header>

      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
        <dt className="text-neutral-400">storage used</dt>
        <dd>
          {storage
            ? `${formatBytes(storage.bytes)} · ${storage.track_count} tracks · ${storage.cover_count} covers`
            : "loading…"}
        </dd>
        <dt className="text-neutral-400">downloads dir</dt>
        <dd className="flex items-center gap-2">
          <span className="truncate font-mono text-xs">
            {dir.data ?? "…"}
          </span>
        </dd>
        <dt className="text-neutral-400">Wi-Fi only</dt>
        <dd>
          <button
            onClick={async () => {
              await downloadsSetWifiOnly(!wifiOnly.data);
              await qc.invalidateQueries({ queryKey: ["downloads", "wifi_only"] });
            }}
            className="rounded border border-neutral-700 px-2 py-0.5 text-xs hover:bg-neutral-800"
          >
            {wifiOnly.data ? "on (tap to disable)" : "off (tap to enable)"}
          </button>
        </dd>
      </dl>

      {activeList.length > 0 && (
        <div className="rounded border border-neutral-800 p-3">
          <h2 className="mb-2 text-sm font-medium">In progress</h2>
          <ul className="flex flex-col gap-2">
            {activeList.map((d) => (
              <li key={d.id} className="flex flex-col gap-1 text-xs">
                <div className="flex items-center justify-between">
                  <span className="truncate font-mono">{d.id}</span>
                  <span className="text-neutral-500">
                    {d.done
                      ? "done"
                      : d.error
                        ? `error: ${d.error}`
                        : d.total
                          ? `${formatBytes(d.received)} / ${formatBytes(d.total)}`
                          : formatBytes(d.received)}
                    {d.totalTracks ? ` · ${d.index ?? 0}/${d.totalTracks}` : ""}
                  </span>
                </div>
                <div className="h-1.5 w-full overflow-hidden rounded bg-neutral-800">
                  <div
                    className={`h-full ${d.error ? "bg-red-500" : d.done ? "bg-emerald-500" : "bg-blue-500"}`}
                    style={{
                      width: d.total
                        ? `${Math.min(100, (d.received / d.total) * 100)}%`
                        : d.done
                          ? "100%"
                          : "30%",
                    }}
                  />
                </div>
                {d.done && (
                  <button
                    onClick={() => {
                      clear(d.id);
                      void refreshStorage();
                    }}
                    className="self-end text-blue-400 underline"
                  >
                    dismiss
                  </button>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {tracks.isLoading && <p className="text-sm text-neutral-400">Loading…</p>}
      {tracks.isError && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(tracks.error)}
        </p>
      )}

      {tracks.data && (
        <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
          {tracks.data.length === 0 ? (
            <li className="p-3 text-sm text-neutral-500">
              Nothing downloaded yet. Browse the library and hit Download.
            </li>
          ) : (
            tracks.data.map((t) => (
              <li
                key={t.id}
                className="flex items-center gap-3 p-3 text-sm"
              >
                <span className="flex-1 truncate">{t.title}</span>
                <span className="text-xs text-neutral-500">{t.codec}</span>
                <span className="w-12 text-right tabular-nums text-neutral-500">
                  {formatDuration(t.duration_ms)}
                </span>
                <span className="w-16 text-right text-xs text-neutral-500">
                  {t.file_size ? formatBytes(t.file_size) : "—"}
                </span>
                <button
                  onClick={() => void remove(t.id)}
                  className="rounded border border-neutral-700 px-2 py-0.5 text-xs hover:bg-neutral-800"
                  title="Remove download"
                >
                  remove
                </button>
              </li>
            ))
          )}
        </ul>
      )}
    </section>
  );
}
