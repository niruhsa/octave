// Manager-only "Discography" panel (Phase 14). Reconciles an artist against
// MusicBrainz and surfaces the albums/EPs/singles the library is missing and,
// for owned releases, the missing tracks — with per-item Ignore + an "Ignored"
// management view. See DISCOGRAPHY_SYNC.md §9. Manager-gated at the call site;
// the server re-enforces every mutation.

import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import {
  discographyAddIgnore,
  discographyCandidates,
  discographyIgnores,
  discographyRemoveIgnore,
  discographyReport,
  discographyResolve,
  discographySync,
  type DiscographyCandidate,
  type DiscographyIgnore,
  type DiscographyReport,
  type IncompleteAlbum,
  type MissingRelease,
  type MissingTrack,
} from "../ipc";
import { formatError } from "../lib/error";
import { btnGhost, btnGhostSm, errorBox } from "../lib/ui";
import { CheckIcon, ChevronDownIcon, DiscIcon, InfoIcon, SyncIcon } from "./icons";
import { offlineAttrs } from "./OfflineGate";

type Props = { artistId: string; online: boolean; isManager: boolean };

const TYPE_GROUPS: { key: string; label: string }[] = [
  { key: "album", label: "Albums" },
  { key: "ep", label: "EPs" },
  { key: "single", label: "Singles" },
  { key: "live", label: "Live" },
];

function fmtWhen(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

export function DiscographyPanel({ artistId, online, isManager }: Props) {
  const qc = useQueryClient();
  const reportKey = ["discography", "report", artistId];
  const ignoresKey = ["discography", "ignores", artistId];

  const reportQ = useQuery({
    queryKey: reportKey,
    queryFn: () => discographyReport(artistId),
    enabled: !!artistId && online && isManager,
    retry: false,
    staleTime: 60_000,
  });

  const [open, setOpen] = useState(false);
  const [ignoredOpen, setIgnoredOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<DiscographyCandidate[] | null>(null);

  const ignoresQ = useQuery({
    queryKey: ignoresKey,
    queryFn: () => discographyIgnores(artistId),
    enabled: !!artistId && online && isManager && ignoredOpen,
    retry: false,
  });

  const report = reportQ.data ?? null;
  const setReport = (r: DiscographyReport | null) => qc.setQueryData(reportKey, r);
  const gaps = report
    ? report.missing_release_count + report.incomplete_album_count
    : 0;

  const grouped = useMemo(() => {
    const g: Record<string, MissingRelease[]> = {};
    for (const r of report?.missing_releases ?? []) (g[r.album_type] ??= []).push(r);
    return g;
  }, [report]);

  // A single wrapper for every mutation: track a per-action busy key + error.
  async function run(key: string, fn: () => Promise<void>) {
    setBusy(key);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusy(null);
    }
  }

  const doSync = () =>
    run("sync", async () => {
      const res = await discographySync(artistId);
      if (res.status === "needs_resolution") {
        setCandidates(res.candidates);
      } else {
        setReport(res.report);
        setCandidates(null);
      }
    });

  const pick = (mbid: string | null) =>
    run("resolve", async () => {
      await discographyResolve(artistId, mbid ?? undefined);
      setCandidates(null);
      if (mbid) {
        const res = await discographySync(artistId);
        if (res.status === "needs_resolution") setCandidates(res.candidates);
        else setReport(res.report);
      } else {
        setReport(null); // artist ignored — clear any stale report
      }
    });

  const openCandidates = () =>
    run("candidates", async () => {
      setCandidates(await discographyCandidates(artistId));
    });

  const ignoreRelease = (r: MissingRelease) =>
    run(`ig:${r.provider_id}`, async () => {
      setReport(
        await discographyAddIgnore(artistId, "release", r.provider_id, {
          label: r.title,
        }),
      );
      qc.invalidateQueries({ queryKey: ignoresKey });
    });

  const ignoreTrack = (album: IncompleteAlbum, t: MissingTrack) =>
    run(`ig:${album.release_group_id}:${t.title_key}`, async () => {
      setReport(
        await discographyAddIgnore(artistId, "track", album.release_group_id, {
          recordingId: t.recording_id ?? undefined,
          titleKey: t.title_key,
          label: `${album.title} — ${t.title}`,
        }),
      );
      qc.invalidateQueries({ queryKey: ignoresKey });
    });

  const unignore = (ig: DiscographyIgnore) =>
    run(`un:${ig.id}`, async () => {
      setReport(await discographyRemoveIgnore(artistId, ig.id));
      qc.invalidateQueries({ queryKey: ignoresKey });
    });

  // Hooks are all above — safe to gate the render now.
  if (!isManager) return null;

  const syncing = busy === "sync" || busy === "resolve";

  return (
    <div className="overflow-hidden rounded-xl border border-oct-border-strong bg-oct-panel">
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-3 px-4 py-3.5 text-left"
      >
        <DiscIcon size={14} className="shrink-0 text-oct-dim" />
        <span className="font-mono text-[10.5px] tracking-[0.16em] text-oct-subtle">
          DISCOGRAPHY · MUSICBRAINZ
        </span>
        {gaps > 0 && (
          <span className="inline-flex items-center gap-1 rounded-md border border-oct-accent/25 bg-oct-accent/10 px-2 py-0.5 font-mono text-[9.5px] text-oct-accent">
            {gaps} missing
          </span>
        )}
        <span className="flex-1" />
        <span className="font-mono text-[10px] text-oct-faint">{open ? "hide" : "manage"}</span>
        <ChevronDownIcon
          size={13}
          className={`text-oct-dim transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>

      {open && (
        <div className="flex flex-col gap-4 border-t border-oct-border-strong px-4 pb-4 pt-4">
          {/* Action row */}
          <div className="flex flex-wrap items-center gap-3">
            <button
              onClick={doSync}
              className={btnGhost}
              {...offlineAttrs(online, syncing, "Reconcile this artist against MusicBrainz")}
            >
              <SyncIcon size={14} className={syncing ? "animate-octspin" : ""} />
              {report ? "Re-sync" : "Sync now"}
            </button>
            {report && (
              <button
                onClick={openCandidates}
                className={btnGhostSm}
                {...offlineAttrs(online, busy === "candidates", "Re-pick the MusicBrainz artist")}
              >
                Re-match…
              </button>
            )}
            {report && (
              <span className="font-mono text-[10.5px] text-oct-faint">
                synced {fmtWhen(report.generated_at)}
              </span>
            )}
          </div>

          {error && <p className={errorBox}>{error}</p>}

          {!online && (
            <p className="text-sm text-oct-subtle">
              Reconnect to the server to reconcile the discography.
            </p>
          )}

          {online && reportQ.isLoading && (
            <p className="text-sm text-oct-subtle">Loading…</p>
          )}

          {online && !reportQ.isLoading && !report && (
            <p className="flex items-center gap-2 text-sm text-oct-subtle">
              <InfoIcon size={14} className="shrink-0 text-oct-dim" />
              Not yet reconciled. Sync to check MusicBrainz for missing releases.
            </p>
          )}

          {report && gaps === 0 && (
            <p className="flex items-center gap-2 text-sm text-oct-online">
              <CheckIcon size={14} className="shrink-0" />
              Complete — nothing missing on MusicBrainz.
            </p>
          )}

          {/* Missing releases, grouped by type */}
          {report &&
            TYPE_GROUPS.filter((g) => (grouped[g.key]?.length ?? 0) > 0).map((g) => (
              <section key={g.key} className="flex flex-col gap-1.5">
                <h4 className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">
                  MISSING {g.label.toUpperCase()}
                </h4>
                {grouped[g.key].map((r) => (
                  <div
                    key={r.provider_id}
                    className="flex items-center gap-3 rounded-lg bg-oct-card px-3 py-2"
                  >
                    <span className="min-w-0 flex-1 truncate text-sm text-oct-text">
                      {r.title}
                      {r.year != null && (
                        <span className="ml-2 text-oct-faint">{r.year}</span>
                      )}
                    </span>
                    <button
                      onClick={() => ignoreRelease(r)}
                      className={btnGhostSm}
                      {...offlineAttrs(online, busy === `ig:${r.provider_id}`, "Never flag this release")}
                    >
                      Ignore
                    </button>
                  </div>
                ))}
              </section>
            ))}

          {/* Incomplete albums (owned, missing tracks) */}
          {report && report.incomplete_albums.length > 0 && (
            <section className="flex flex-col gap-2">
              <h4 className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">
                INCOMPLETE ALBUMS
              </h4>
              {report.incomplete_albums.map((album) => (
                <div
                  key={album.release_group_id}
                  className="rounded-lg border border-oct-border bg-oct-card px-3 py-2.5"
                >
                  <div className="mb-1.5 truncate text-sm font-medium text-oct-text">
                    {album.title}
                    <span className="ml-2 font-mono text-[10px] text-oct-faint">
                      {album.missing_tracks.length} missing
                    </span>
                  </div>
                  <ul className="flex flex-col gap-1">
                    {album.missing_tracks.map((t) => (
                      <li
                        key={`${t.title_key}:${t.position ?? 0}`}
                        className="flex items-center gap-3 text-[13px]"
                      >
                        <span className="min-w-0 flex-1 truncate text-oct-muted">
                          {t.disc_no != null && t.position != null && (
                            <span className="mr-2 font-mono text-[10px] text-oct-faint">
                              {t.disc_no}-{t.position}
                            </span>
                          )}
                          {t.title}
                        </span>
                        <button
                          onClick={() => ignoreTrack(album, t)}
                          className={btnGhostSm}
                          {...offlineAttrs(
                            online,
                            busy === `ig:${album.release_group_id}:${t.title_key}`,
                            "Never flag this track",
                          )}
                        >
                          Ignore
                        </button>
                      </li>
                    ))}
                  </ul>
                </div>
              ))}
            </section>
          )}

          {/* Ignored management view */}
          <div className="rounded-lg border border-oct-border">
            <button
              onClick={() => setIgnoredOpen((o) => !o)}
              className="flex w-full items-center gap-2 px-3 py-2 text-left"
            >
              <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">
                IGNORED
              </span>
              <span className="flex-1" />
              <ChevronDownIcon
                size={12}
                className={`text-oct-dim transition-transform ${ignoredOpen ? "rotate-180" : ""}`}
              />
            </button>
            {ignoredOpen && (
              <div className="flex flex-col gap-1 border-t border-oct-border px-3 py-2">
                {ignoresQ.isLoading && <p className="text-[13px] text-oct-subtle">Loading…</p>}
                {ignoresQ.data && ignoresQ.data.length === 0 && (
                  <p className="text-[13px] text-oct-subtle">Nothing ignored.</p>
                )}
                {ignoresQ.data?.map((ig) => (
                  <div key={ig.id} className="flex items-center gap-3 text-[13px]">
                    <span className="shrink-0 rounded border border-oct-border px-1.5 py-0.5 font-mono text-[9px] uppercase text-oct-faint">
                      {ig.scope}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-oct-muted">{ig.label}</span>
                    <button
                      onClick={() => unignore(ig)}
                      className={btnGhostSm}
                      {...offlineAttrs(online, busy === `un:${ig.id}`, "Restore this gap")}
                    >
                      Un-ignore
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}

      {candidates && (
        <CandidatePicker
          candidates={candidates}
          busy={busy === "resolve"}
          onPick={pick}
          onClose={() => setCandidates(null)}
        />
      )}
    </div>
  );
}

// ── Disambiguation dialog ──────────────────────────────────────────────────

function CandidatePicker({
  candidates,
  busy,
  onPick,
  onClose,
}: {
  candidates: DiscographyCandidate[];
  busy: boolean;
  onPick: (mbid: string | null) => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return createPortal(
    <div
      className="fixed inset-0 z-[60] flex items-end justify-center bg-black/60 p-0 backdrop-blur-sm sm:items-center sm:p-6"
      onMouseDown={onClose}
      role="dialog"
      aria-modal="true"
      aria-label="Match on MusicBrainz"
    >
      <div
        className="flex max-h-[92vh] w-full flex-col overflow-hidden rounded-t-2xl border border-oct-border-strong bg-oct-panel shadow-2xl sm:max-w-xl sm:rounded-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <header className="flex items-start gap-2 border-b border-oct-border px-5 py-3.5">
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold tracking-tight">Match on MusicBrainz</h2>
            <p className="mt-0.5 text-[11.5px] text-oct-subtle">
              Pick the artist this maps to, then it syncs automatically.
            </p>
          </div>
          <button
            onClick={onClose}
            className="font-mono text-[11px] text-oct-subtle hover:text-oct-text"
            aria-label="Close"
          >
            ESC ✕
          </button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {candidates.length === 0 && (
            <p className="text-sm text-oct-subtle">No candidates found for this artist name.</p>
          )}
          <div className="flex flex-col gap-1.5">
            {candidates.map((c) => (
              <button
                key={c.provider_id}
                onClick={() => onPick(c.provider_id)}
                disabled={busy}
                className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-colors hover:bg-oct-elevated disabled:opacity-50"
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm text-oct-text">{c.name}</span>
                  {c.disambiguation && (
                    <span className="block truncate text-[12px] text-oct-subtle">
                      {c.disambiguation}
                    </span>
                  )}
                </span>
                <span className="shrink-0 font-mono text-[11px] text-oct-accent">
                  {c.score}%
                </span>
              </button>
            ))}
          </div>
        </div>
        <footer className="flex items-center justify-between gap-2 border-t border-oct-border px-5 py-3">
          <button
            onClick={() => onPick(null)}
            disabled={busy}
            className="font-mono text-[11px] text-oct-subtle hover:text-oct-danger disabled:opacity-50"
            title="Exclude this artist from reconciliation"
          >
            None — ignore artist
          </button>
          <button onClick={onClose} className={btnGhost} disabled={busy}>
            Cancel
          </button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}
