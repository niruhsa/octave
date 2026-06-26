// Per-track download indicator for the library track lists.
//
// States, next to the song where its status dot lives:
//   • pending   — queued in an in-flight album/playlist batch, not started yet
//                 (faint ring + glyph, passed in via `pending`)
//   • active    — downloading now → amber progress ring around a download glyph
//                 (determinate from bytes; spins when the size isn't known)
//   • done      — the amber "downloaded" dot
//
// Progress is read from the per-track download record (`active[trackId]`), which
// carries byte-level `received`/`total`. The batch aggregate entry is ignored on
// purpose: its `received`/`total` are *track counts*, not bytes, so reading it
// would show a finished track at "N / total tracks" (≈50% mid-album) instead of
// its real state. Album/playlist "pending" is derived from the batch entry by
// the page and handed in via `pending`.

import { useDownloadsStore } from "../downloads/useDownloads";
import { DownloadIcon } from "./icons";

/** This track's own (per-track scope) download record, if any. */
function useTrackDownload(trackId: string) {
  return useDownloadsStore((s) => s.active[trackId] ?? null);
}

export function DownloadStatus({
  trackId,
  downloaded,
  pending = false,
  streamDot = false,
}: {
  trackId: string;
  downloaded: boolean;
  /** Belongs to an in-flight batch but hasn't started downloading yet. */
  pending?: boolean;
  /** Show the faint stream-only dot when not downloaded (playlist rows). */
  streamDot?: boolean;
}) {
  const d = useTrackDownload(trackId);
  if (d && !d.error) {
    // `done` lands the moment the file finishes — switch straight to the dot
    // instead of waiting for the library query to refetch.
    if (d.done) return <DownloadedMark />;
    return <DownloadRing fraction={d.total ? d.received / d.total : null} />;
  }
  if (downloaded) return <DownloadedMark />;
  if (pending) return <DownloadRing fraction={null} pending />;
  if (streamDot)
    return <span className="inline-block h-2 w-2 shrink-0 rounded-full bg-oct-faint" title="stream-only" />;
  return null;
}

function DownloadedMark() {
  return <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-oct-accent" title="downloaded" />;
}

/** Ring around a download glyph. `pending` → faint, no arc (queued). Otherwise
 *  the arc fills to `fraction`, or spins when `fraction` is null (size unknown). */
function DownloadRing({ fraction, pending = false }: { fraction: number | null; pending?: boolean }) {
  const size = 16;
  const sw = 2;
  const r = (size - sw) / 2;
  const c = 2 * Math.PI * r;
  const indeterminate = !pending && fraction == null;
  const frac = fraction ?? 0;
  // Keep a sliver visible at 0% so an active ring never fully vanishes.
  const shown = pending ? 0 : indeterminate ? 0.3 : Math.max(0.05, Math.min(1, frac));
  const title = pending
    ? "download pending"
    : indeterminate
      ? "downloading…"
      : `downloading ${Math.round(frac * 100)}%`;
  return (
    <span
      className="relative grid shrink-0 place-items-center"
      style={{ width: size, height: size }}
      title={title}
    >
      <svg
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        className={indeterminate ? "animate-octspin" : ""}
        style={indeterminate ? undefined : { transform: "rotate(-90deg)" }}
      >
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth={sw}
          className="text-oct-line"
        />
        {shown > 0 && (
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            stroke="currentColor"
            strokeWidth={sw}
            strokeLinecap="round"
            strokeDasharray={c}
            strokeDashoffset={c * (1 - shown)}
            className="text-oct-accent"
          />
        )}
      </svg>
      <DownloadIcon
        size={8}
        className={`pointer-events-none absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 ${
          pending ? "text-oct-subtle" : "text-oct-accent"
        }`}
      />
    </span>
  );
}
