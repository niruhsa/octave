// Upload reports (Uploads v2).
//
// List view: previous + in-flight uploads (own by default; admins see everyone
// and can filter by user). Detail view (`?id=`): per-file / per-chunk progress
// plus the completion report (what songs/albums/artists were ingested). Both
// refresh live off the server's `uploads` broadcast.

import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  onUploadEvent,
  uploadsCancel,
  uploadsGet,
  uploadsList,
  uploadsPause,
  uploadsResume,
  uploadsSubscribe,
  type MergedTrack,
  type UploadLifecycle,
  type UploadReport,
  type UploadSummary,
} from "../ipc";
import { useAppStore } from "../store";
import { formatError } from "../lib/error";
import { btnGhostSm, card, errorBox } from "../lib/ui";
import { SkeletonList } from "../components/Skeleton";
import { OfflineGate } from "../components/OfflineGate";
import { EditMetaButton, MetadataEditor } from "../components/MetadataEditor";
import { broadcastInvalidate } from "../App";
import { formatBytes } from "../downloads/useDownloads";

// A freshly-ingested track only carries id/title/artist/album in the report,
// so seed a minimal `MergedTrack` for the editor — track/disc numbers start
// blank (the manager fills what they want to fix). The editor only reads
// id + title + track_no/disc_no, so the unused fields are harmless stubs.
function reportTrackToMerged(t: ReportTrack): MergedTrack {
  return {
    id: t.id,
    album_id: "",
    artist_id: "",
    title: t.title,
    track_no: null,
    disc_no: null,
    duration_ms: 0,
    codec: "",
    bitrate_kbps: null,
    file_path: "",
    file_size: null,
    sample_rate_hz: null,
    bit_depth: null,
    channels: null,
    local_file_path: null,
    is_single_release: false,
    is_explicit: false,
    aliases: [],
    downloaded: false,
  };
}

const STATES: (UploadLifecycle | "all")[] = ["all", "uploading", "paused", "initialized", "completed", "cancelled"];

/** States in which an upload is still in flight (cancellable / pausable). */
function isActiveState(state: string): boolean {
  return state === "uploading" || state === "initialized" || state === "paused";
}

// One process-wide live subscription feeding `upload-event`.
let subscribed = false;
function ensureSubscribed() {
  if (subscribed) return;
  subscribed = true;
  void uploadsSubscribe().catch(() => {
    subscribed = false; // allow a later retry if it failed (e.g. not logged in)
  });
}

function statePill(state: string): string {
  const base = "inline-block rounded-full px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide";
  switch (state) {
    case "completed":
      return `${base} bg-emerald-500/15 text-emerald-300`;
    case "uploading":
      return `${base} bg-oct-accent/15 text-oct-accent`;
    case "paused":
      return `${base} bg-sky-500/15 text-sky-300`;
    case "initialized":
      return `${base} bg-amber-500/15 text-amber-300`;
    case "cancelled":
      return `${base} bg-oct-danger/15 text-oct-danger`;
    default:
      return `${base} bg-oct-elevated text-oct-subtle`;
  }
}

function fmtDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

export default function Uploads() {
  const [params, setParams] = useSearchParams();
  const id = params.get("id");

  useEffect(() => {
    ensureSubscribed();
  }, []);

  return (
    <OfflineGate feature="Upload reports">
      {id ? (
        <UploadDetail id={id} onBack={() => setParams({}, { replace: true })} />
      ) : (
        <UploadList onOpen={(uid) => setParams({ id: uid })} />
      )}
    </OfflineGate>
  );
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

function UploadList({ onOpen }: { onOpen: (id: string) => void }) {
  const tier = useAppStore((s) => s.tier);
  const isAdmin = tier === "admin";
  const qc = useQueryClient();
  const [params, setParams] = useSearchParams();
  const stateFilter = (params.get("state") as UploadLifecycle | "all") ?? "all";
  const userFilter = params.get("user_id") ?? "";

  const q = useQuery({
    queryKey: ["uploads", "list", stateFilter, userFilter],
    queryFn: () =>
      uploadsList({
        state: stateFilter === "all" ? null : stateFilter,
        user_id: isAdmin && userFilter ? userFilter : null,
      }),
    placeholderData: (prev) => prev,
  });

  // Live: refresh the list on any broadcast event.
  useEffect(() => {
    let un: (() => void) | undefined;
    onUploadEvent(() => qc.invalidateQueries({ queryKey: ["uploads", "list"] })).then((f) => (un = f));
    return () => un?.();
  }, [qc]);

  function setState(s: string) {
    const next = new URLSearchParams(params);
    if (s === "all") next.delete("state");
    else next.set("state", s);
    setParams(next, { replace: true });
  }

  async function cancel(uid: string) {
    try {
      await uploadsCancel(uid);
      qc.invalidateQueries({ queryKey: ["uploads"] });
    } catch (e) {
      alert(formatError(e));
    }
  }

  async function togglePauseRow(uid: string, state: string) {
    try {
      if (state === "paused") await uploadsResume(uid);
      else await uploadsPause(uid);
      qc.invalidateQueries({ queryKey: ["uploads"] });
    } catch (e) {
      alert(formatError(e));
    }
  }

  const rows = q.data ?? [];

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-[27px] font-semibold tracking-tight">Upload reports</h1>
          <p className="mt-1 font-mono text-[11.5px] text-oct-subtle">
            {isAdmin ? "All users' uploads" : "Your uploads"}
          </p>
        </div>
        <Link to="/upload" className={btnGhostSm}>
          ← New upload
        </Link>
      </header>

      <div className="flex flex-wrap items-center gap-2">
        {STATES.map((s) => (
          <button
            key={s}
            onClick={() => setState(s)}
            className={`rounded-full px-3 py-1 font-mono text-[11px] capitalize ${
              stateFilter === s ? "bg-oct-accent text-black" : "bg-oct-elevated text-oct-subtle hover:text-white"
            }`}
          >
            {s}
          </button>
        ))}
        {isAdmin && (
          <input
            defaultValue={userFilter}
            placeholder="filter by user UUID…"
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const next = new URLSearchParams(params);
                const v = (e.target as HTMLInputElement).value.trim();
                if (v) next.set("user_id", v);
                else next.delete("user_id");
                setParams(next, { replace: true });
              }
            }}
            className="ml-auto w-56 rounded-md border border-oct-border bg-oct-elevated px-2 py-1 font-mono text-[11px] text-oct-text placeholder:text-oct-faint"
          />
        )}
      </div>

      {q.isLoading && <SkeletonList rows={6} />}
      {q.isError && <p className={errorBox}>{formatError(q.error)}</p>}

      {q.data && (
        <div className={`${card} divide-y divide-oct-border`}>
          {rows.length === 0 ? (
            <p className="p-4 text-sm text-oct-subtle">No uploads.</p>
          ) : (
            rows.map((u: UploadSummary) => {
              const active = isActiveState(u.state);
              const canPause = u.state === "uploading" || u.state === "paused";
              return (
                <div key={u.id} className="group flex items-center gap-3 px-3 py-2.5 first:rounded-t-xl last:rounded-b-xl hover:bg-oct-elevated/50">
                  <button onClick={() => onOpen(u.id)} className="min-w-0 flex-1 text-left">
                    <div className="flex items-center gap-2">
                      <span className={statePill(u.state)}>{u.state}</span>
                      <span className="truncate text-[13.5px] group-hover:text-white">
                        {u.total_files} file{u.total_files === 1 ? "" : "s"} · {formatBytes(u.total_bytes)}
                      </span>
                    </div>
                    <span className="block truncate font-mono text-[10.5px] text-oct-faint">
                      {fmtDate(u.created_at)}
                      {u.error ? ` · ${u.error}` : ""}
                    </span>
                  </button>
                  {canPause && (
                    <button
                      onClick={() => void togglePauseRow(u.id, u.state)}
                      className="text-oct-dim opacity-0 transition-opacity hover:text-oct-accent group-hover:opacity-100 font-mono text-[11px]"
                    >
                      {u.state === "paused" ? "Resume" : "Pause"}
                    </button>
                  )}
                  {active && (
                    <button
                      onClick={() => void cancel(u.id)}
                      className="text-oct-dim opacity-0 transition-opacity hover:text-oct-danger group-hover:opacity-100 font-mono text-[11px]"
                    >
                      Cancel
                    </button>
                  )}
                </div>
              );
            })
          )}
        </div>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

type ReportTrack = { id: string; title: string; artist: string; album: string };
type ReportFile = {
  filename: string;
  ok: boolean;
  error: string | null;
  is_archive: boolean;
  archive_kind: string | null;
  ingested: number;
  already_indexed: number;
  non_audio_skipped: number;
  errors: number;
  track_ids: string[];
  tracks: ReportTrack[];
};

function UploadDetail({ id, onBack }: { id: string; onBack: () => void }) {
  const qc = useQueryClient();
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const isManager = tier === "admin" || tier === "manager";
  const [editTrack, setEditTrack] = useState<MergedTrack | null>(null);
  const q = useQuery({
    queryKey: ["uploads", "detail", id],
    queryFn: () => uploadsGet(id),
    refetchInterval: (query) => {
      const s = query.state.data?.state;
      return s && isActiveState(s) ? 1500 : false;
    },
  });

  // Live: refetch this report when an event for it lands.
  useEffect(() => {
    let un: (() => void) | undefined;
    onUploadEvent((e) => {
      if (e.upload_id === id) qc.invalidateQueries({ queryKey: ["uploads", "detail", id] });
    }).then((f) => (un = f));
    return () => un?.();
  }, [id, qc]);

  async function cancel() {
    try {
      await uploadsCancel(id);
      qc.invalidateQueries({ queryKey: ["uploads"] });
    } catch (e) {
      alert(formatError(e));
    }
  }

  async function togglePause(state: string) {
    try {
      if (state === "paused") await uploadsResume(id);
      else await uploadsPause(id);
      qc.invalidateQueries({ queryKey: ["uploads", "detail", id] });
    } catch (e) {
      alert(formatError(e));
    }
  }

  const u: UploadReport | undefined = q.data;
  const active = isActiveState(u?.state ?? "");
  const canPause = u?.state === "uploading" || u?.state === "paused";
  const pct = u && u.total_bytes > 0 ? Math.round(Math.min(u.bytes_received / u.total_bytes, 1) * 100) : 0;
  const report = (u?.report ?? null) as unknown as
    | { files?: ReportFile[]; tracks_ingested?: number; files_failed?: number }
    | null;

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <button onClick={onBack} className={btnGhostSm}>
          ← Back
        </button>
        <div className="flex items-center gap-4">
          {canPause && u && (
            <button
              onClick={() => void togglePause(u.state)}
              className="font-mono text-[11px] text-oct-accent hover:underline"
            >
              {u.state === "paused" ? "Resume upload" : "Pause upload"}
            </button>
          )}
          {active && (
            <button onClick={() => void cancel()} className="font-mono text-[11px] text-oct-danger hover:underline">
              Cancel upload
            </button>
          )}
        </div>
      </header>

      {q.isLoading && <SkeletonList rows={4} />}
      {q.isError && <p className={errorBox}>{formatError(q.error)}</p>}

      {u && (
        <>
          <div className={`${card} p-4`}>
            <div className="mb-3 flex items-center gap-2">
              <span className={statePill(u.state)}>{u.state}</span>
              <span className="font-mono text-[11px] text-oct-subtle">
                {u.total_files} file{u.total_files === 1 ? "" : "s"} · {formatBytes(u.bytes_received)} / {formatBytes(u.total_bytes)}
              </span>
            </div>
            <div className="h-2 w-full overflow-hidden rounded-full bg-oct-line">
              <div
                className={`h-full rounded-full bg-oct-accent transition-all duration-300 ${u.state === "paused" ? "opacity-50" : ""}`}
                style={{ width: `${pct}%` }}
              />
            </div>
            <p className="mt-2 font-mono text-[10.5px] text-oct-faint">
              created {fmtDate(u.created_at)} · updated {fmtDate(u.updated_at)}
            </p>
            {u.error && <p className="mt-2 text-xs text-oct-danger">{u.error}</p>}
          </div>

          {/* Per-file chunk progress */}
          <div className="flex flex-col gap-3">
            {u.files.map((f) => (
              <div key={f.file_index} className={`${card} p-4`}>
                <div className="mb-2 flex items-center justify-between gap-3">
                  <span className="min-w-0 truncate text-[13.5px]">{f.filename}</span>
                  <span className={statePill(f.state)}>{f.state}</span>
                </div>
                <p className="mb-2 font-mono text-[10.5px] text-oct-faint">
                  {f.received_chunks}/{f.total_chunks} chunks · {formatBytes(f.total_size)}
                  {f.error ? ` · ${f.error}` : ""}
                </p>
                {f.total_chunks <= 240 ? (
                  <div className="flex flex-wrap gap-[3px]">
                    {f.chunks.map((c) => (
                      <span
                        key={c.index}
                        title={`chunk ${c.index}${c.received ? " ✓" : ""}`}
                        className={`h-2 w-2 rounded-[2px] ${c.received ? "bg-oct-accent" : "bg-oct-line"}`}
                      />
                    ))}
                  </div>
                ) : (
                  <div className="h-2 w-full overflow-hidden rounded-full bg-oct-line">
                    <div
                      className="h-full rounded-full bg-oct-accent"
                      style={{ width: `${Math.round((f.received_chunks / Math.max(f.total_chunks, 1)) * 100)}%` }}
                    />
                  </div>
                )}
              </div>
            ))}
          </div>

          {/* Completion report: what was ingested */}
          {report && (
            <div className={`${card} p-4`}>
              <h2 className="mb-2 text-sm font-semibold">
                Ingest report — {report.tracks_ingested ?? 0} track(s)
                {(report.files_failed ?? 0) > 0 ? `, ${report.files_failed} file(s) failed` : ""}
              </h2>
              {(report.files ?? []).map((f, i) => (
                <div key={i} className="border-t border-oct-border py-2 first:border-t-0">
                  <div className="flex items-center gap-2">
                    <span className={statePill(f.ok ? "completed" : "cancelled")}>{f.ok ? "ok" : "failed"}</span>
                    <span className="truncate text-[12.5px]">{f.filename}</span>
                    {f.is_archive && <span className="font-mono text-[10px] text-oct-faint">{f.archive_kind}</span>}
                  </div>
                  {f.error && <p className="mt-1 text-xs text-oct-danger">{f.error}</p>}
                  {f.tracks.length > 0 && (
                    <ul className="mt-1 flex flex-col gap-0.5 text-[11.5px] text-oct-subtle">
                      {f.tracks.map((t) => (
                        <li key={t.id} className="group flex min-w-0 items-center gap-2">
                          <span className="truncate">
                            {t.title} — <span className="text-oct-faint">{t.artist} · {t.album}</span>
                          </span>
                          {isManager && (
                            <EditMetaButton
                              online={online}
                              onClick={() => setEditTrack(reportTrackToMerged(t))}
                              className="shrink-0 opacity-0 transition-opacity group-hover:opacity-100"
                            />
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              ))}
            </div>
          )}
        </>
      )}

      {editTrack && (
        <MetadataEditor
          tracks={[editTrack]}
          online={online}
          onClose={() => setEditTrack(null)}
          onSaved={() => broadcastInvalidate(["library"])}
        />
      )}
    </section>
  );
}
