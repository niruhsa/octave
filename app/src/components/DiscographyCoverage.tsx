// Library-wide discography coverage (Phase C admin dashboard). Manager-gated at
// the call site (rendered from Account inside the OfflineGate); the server
// re-enforces. Per-artist gaps live on each artist's page — this is the
// library-wide summary + a "Sync all" trigger. See DISCOGRAPHY_SYNC.md §9.

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import {
  discographyStatus,
  discographySyncAll,
  type DiscographySyncAll,
} from "../ipc";
import { formatError } from "../lib/error";
import { btnPrimary, errorBox, okBox } from "../lib/ui";
import { SyncIcon } from "./icons";
import { Skeleton } from "./Skeleton";

export function DiscographyCoverage() {
  const qc = useQueryClient();
  const statusQ = useQuery({
    queryKey: ["discography", "status"],
    queryFn: discographyStatus,
    staleTime: 30_000,
    retry: false,
  });

  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [result, setResult] = useState<DiscographySyncAll | null>(null);

  const st = statusQ.data;

  async function syncAll() {
    setBusy(true);
    setErr(null);
    setResult(null);
    try {
      const r = await discographySyncAll();
      setResult(r);
      qc.invalidateQueries({ queryKey: ["discography", "status"] });
    } catch (e) {
      setErr(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="border-t border-oct-border pt-6">
      <h2 className="mb-2 text-lg font-semibold">Discography coverage</h2>
      <p className="mb-3 text-xs text-oct-subtle">
        Reconcile the library against MusicBrainz. Per-artist missing releases +
        tracks live on each artist's page; this is the library-wide summary.
      </p>

      {statusQ.isLoading && <Skeleton className="h-20 w-full rounded-lg" />}
      {statusQ.isError && <p className={errorBox}>{formatError(statusQ.error)}</p>}

      {st && !st.enabled && (
        <p className="text-sm text-oct-subtle">
          Discography sync is disabled on the server (
          <code className="font-mono text-oct-muted">DISCOGRAPHY_ENABLED</code> off).
        </p>
      )}

      {st && st.enabled && (
        <>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
            <Stat label="Artists" value={st.artists_total} />
            <Stat label="Matched" value={st.matched} />
            <Stat label="Unresolved" value={st.unresolved} accent={st.unresolved > 0} />
            <Stat label="Ignored" value={st.ignored} />
          </div>

          <div className="mt-3 flex flex-wrap items-center gap-3">
            <button onClick={syncAll} disabled={busy} className={btnPrimary}>
              <SyncIcon size={14} className={busy ? "animate-octspin" : ""} />
              {busy ? "Syncing all…" : "Sync all"}
            </button>
            <span className="font-mono text-[10.5px] text-oct-faint">
              {st.provider} · rate-limited (~1 artist/sec)
            </span>
          </div>

          {busy && (
            <p className="mt-3 text-xs text-oct-subtle">
              This reconciles every matched artist and can take a while on a large
              library. It's safe to leave — it runs server-side.
            </p>
          )}
          {err && <p className={`${errorBox} mt-3`}>{err}</p>}
          {result && (
            <p className={`${okBox} mt-3`}>
              Synced {result.synced} · skipped {result.skipped_fresh} fresh ·{" "}
              {result.failed} failed · {result.total} total.
            </p>
          )}
        </>
      )}
    </section>
  );
}

function Stat({
  label,
  value,
  accent = false,
}: {
  label: string;
  value: number;
  accent?: boolean;
}) {
  return (
    <div className="rounded-lg border border-oct-border bg-oct-card px-3 py-2">
      <div className={`text-xl font-semibold ${accent ? "text-oct-accent" : "text-oct-text"}`}>
        {value}
      </div>
      <div className="font-mono text-[9.5px] tracking-[0.16em] text-oct-faint">
        {label.toUpperCase()}
      </div>
    </div>
  );
}
