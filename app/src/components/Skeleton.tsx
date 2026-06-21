// Shimmer loading placeholders (OCTAVE). Each composed skeleton mirrors the
// shape of the real content it stands in for, so swapping data in doesn't
// shift the layout. Used across every page's loading state.

/** Base shimmer block. Size it with Tailwind utilities via `className`. */
export function Skeleton({ className = "" }: { className?: string }) {
  return <div className={`oct-skeleton rounded-md ${className}`} />;
}

/** Card-framed list rows (artists, playlists, search results, downloads). */
export function SkeletonList({
  rows = 8,
  avatar = "circle",
  trailing = true,
}: {
  rows?: number;
  avatar?: "circle" | "square" | "none";
  trailing?: boolean;
}) {
  return (
    <div className="divide-y divide-oct-border rounded-xl border border-oct-border-strong bg-oct-panel">
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="flex items-center gap-3 px-3 py-2.5">
          {avatar !== "none" && (
            <Skeleton className={`h-9 w-9 shrink-0 ${avatar === "circle" ? "rounded-full" : "rounded-lg"}`} />
          )}
          <div className="flex flex-1 flex-col gap-2">
            <Skeleton className={`h-3 ${i % 3 === 0 ? "w-2/5" : i % 3 === 1 ? "w-1/2" : "w-1/3"}`} />
            <Skeleton className="h-2.5 w-1/5" />
          </div>
          {trailing && <Skeleton className="h-3 w-10" />}
        </div>
      ))}
    </div>
  );
}

/** Album-card grid (Artist page). Matches the auto-fill 160px grid. */
export function SkeletonGrid({ count = 12 }: { count?: number }) {
  return (
    <div className="grid gap-x-[22px] gap-y-7" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))" }}>
      {Array.from({ length: count }).map((_, i) => (
        <div key={i}>
          <Skeleton className="aspect-square w-full rounded-[9px]" />
          <Skeleton className="mt-2.5 h-3 w-3/4" />
          <Skeleton className="mt-2 h-2.5 w-1/2" />
        </div>
      ))}
    </div>
  );
}

/** Detail hero: big cover + eyebrow / title / meta (Album, PlaylistDetail). */
export function SkeletonHero() {
  return (
    <div className="flex flex-col gap-5 sm:flex-row sm:items-end">
      <Skeleton className="aspect-square w-[120px] shrink-0 rounded-xl sm:w-[132px]" />
      <div className="flex w-full max-w-md flex-col gap-3">
        <Skeleton className="h-3 w-20" />
        <Skeleton className="h-8 w-2/3" />
        <Skeleton className="h-3 w-1/2" />
      </div>
    </div>
  );
}

/** Track-table rows. `cols=4` for Album (#/title/quality/time), `3` for playlist. */
export function SkeletonTracks({ rows = 8, cols = 4 }: { rows?: number; cols?: 3 | 4 }) {
  const grid = cols === 4 ? "grid-cols-[28px_1fr_110px_56px]" : "grid-cols-[28px_1fr_56px]";
  return (
    <div className="flex flex-col">
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className={`grid items-center gap-x-4 px-2 py-2.5 ${grid}`}>
          <Skeleton className="h-3 w-3 justify-self-center" />
          <Skeleton className={`h-3 ${i % 2 === 0 ? "w-1/2" : "w-2/5"}`} />
          {cols === 4 && <Skeleton className="hidden h-2.5 w-16 sm:block" />}
          <Skeleton className="h-3 w-8 justify-self-end" />
        </div>
      ))}
    </div>
  );
}
