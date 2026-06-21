import type { LibrarySource } from "../ipc";
import { CheckIcon, CloudIcon, DownloadIcon } from "./icons";

/**
 * Chip telling the user whether the data came from the server (live) or the
 * local cache (offline). Always rendered so "am I seeing fresh data?" is one
 * glance away.
 */
export function SourceBadge({ source }: { source: LibrarySource }) {
  const live = source === "server";
  return (
    <span
      className={`rounded-md px-2 py-0.5 font-mono text-[10px] tracking-wide ${
        live
          ? "bg-oct-online/15 text-oct-online"
          : "bg-oct-accent/15 text-oct-accent-bright"
      }`}
      title={live ? "live from server" : "from offline cache"}
    >
      {live ? "SERVER" : "OFFLINE"}
    </span>
  );
}

/** Inline downloaded/stream-only dot for a list row. */
export function DownloadedDot({ downloaded }: { downloaded: boolean }) {
  return (
    <span
      className={`inline-block h-2 w-2 shrink-0 rounded-full ${
        downloaded ? "bg-oct-accent" : "bg-oct-faint"
      }`}
      title={downloaded ? "available offline" : "stream-only"}
    />
  );
}

/** "SAVED" overlay chip for a downloaded album (amber). */
export function SavedBadge() {
  return (
    <span className="flex items-center gap-1 rounded-md bg-oct-accent/20 px-1.5 py-[3px] font-mono text-[9px] tracking-wide text-oct-accent-bright">
      <CheckIcon size={9} />
      SAVED
    </span>
  );
}

/** "STREAM" overlay chip for a stream-only album (neutral). */
export function StreamBadge() {
  return (
    <span className="flex items-center gap-1 rounded-md bg-black/35 px-1.5 py-[3px] font-mono text-[9px] tracking-wide text-white/75">
      <CloudIcon size={10} />
      STREAM
    </span>
  );
}

/** Inline "saved" pill used next to the now-playing title (player bar). */
export function SavedPill() {
  return (
    <span className="flex shrink-0 items-center gap-1 rounded bg-oct-accent/15 px-1.5 py-0.5 font-mono text-[8.5px] tracking-wide text-oct-accent">
      <DownloadIcon size={8} sw={2} />
      SAVED
    </span>
  );
}
