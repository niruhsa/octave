import type { LibrarySource } from "../ipc";

/**
 * Small chip that tells the user whether the data they're looking at came
 * from the server (live) or the local cache (offline). Always rendered so
 * the answer to "am I seeing fresh data?" is one glance away.
 */
export function SourceBadge({ source }: { source: LibrarySource }) {
  const live = source === "server";
  return (
    <span
      className={`rounded px-1.5 py-0.5 text-xs ${
        live
          ? "bg-emerald-900/40 text-emerald-200"
          : "bg-amber-900/40 text-amber-200"
      }`}
      title={live ? "live from server" : "from offline cache"}
    >
      {live ? "server" : "offline"}
    </span>
  );
}

/** Inline "✓ downloaded" / "stream-only" tag for a list item. */
export function DownloadedDot({ downloaded }: { downloaded: boolean }) {
  return (
    <span
      className={`inline-block h-2 w-2 rounded-full ${
        downloaded ? "bg-emerald-400" : "bg-neutral-600"
      }`}
      title={downloaded ? "available offline" : "stream-only"}
    />
  );
}
