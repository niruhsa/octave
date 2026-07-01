// Opt-in metadata editor (Phase 9). Manager+ gated at the call site; the
// server re-enforces. Two layouts share one modal:
//   * single track  → streamlined form (the only entry point on mobile)
//   * many tracks    → a richer batch table (desktop "Edit tags" on an album)
//
// The server is authoritative: each row is pushed via `libraryEditTrackMetadata`
// (gRPC→REST), which also mirrors the change into the offline cache for
// downloaded items. Only the fields a manager actually changed are sent, so an
// untouched field is left unchanged server-side.

import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  type AliasInfo,
  libraryEditTrackMetadata,
  libraryListTrackAliases,
  type MergedTrack,
  type MetadataEdit,
} from "../ipc";
import { formatError } from "../lib/error";
import { btnGhost, btnGhostSm, btnPrimary, errorBox, input, label } from "../lib/ui";
import { Aliases } from "./Aliases";
import { EditIcon } from "./icons";

type Props = {
  tracks: MergedTrack[];
  online: boolean;
  onClose: () => void;
  /** Called after at least one track was successfully edited. */
  onSaved: () => void;
};

const OFFLINE_NOTICE = "Editing metadata requires a connection to the server.";

function parseIntOrUndef(s: string): number | undefined {
  const t = s.trim();
  if (t === "") return undefined;
  const n = Number.parseInt(t, 10);
  return Number.isNaN(n) ? undefined : n;
}

/** Modal shell: backdrop + centered panel, Escape to close. */
function Shell({ title, onClose, children, footer }: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  footer: React.ReactNode;
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
      aria-label={title}
    >
      <div
        className="flex max-h-[92vh] w-full flex-col overflow-hidden rounded-t-2xl border border-oct-border-strong bg-oct-panel shadow-2xl sm:max-w-2xl sm:rounded-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <header className="flex items-center gap-2 border-b border-oct-border px-5 py-3.5">
          <EditIcon size={15} className="text-oct-accent" />
          <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
          <button
            onClick={onClose}
            className="ml-auto font-mono text-[11px] text-oct-subtle hover:text-oct-text"
            aria-label="Close"
          >
            ESC ✕
          </button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">{children}</div>
        <footer className="flex items-center justify-end gap-3 border-t border-oct-border px-5 py-3.5">
          {footer}
        </footer>
      </div>
    </div>,
    document.body,
  );
}

function Field({ children, hint, htmlLabel }: { children: React.ReactNode; hint?: string; htmlLabel: string }) {
  return (
    <label className="flex flex-col gap-1.5">
      <span className={label}>{htmlLabel}</span>
      {children}
      {hint && <span className="text-[11px] text-oct-faint">{hint}</span>}
    </label>
  );
}

// ───────────────────────────── single-track ─────────────────────────────

function SingleEditor({ track, online, onClose, onSaved }: Props & { track: MergedTrack }) {
  const [title, setTitle] = useState(track.title);
  const [trackNo, setTrackNo] = useState(track.track_no?.toString() ?? "");
  const [discNo, setDiscNo] = useState(track.disc_no?.toString() ?? "");
  const [year, setYear] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [metaJson, setMetaJson] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const titleRef = useRef<HTMLInputElement>(null);

  // Alternate title spellings (loaded on open; only present on a single-track
  // read). Managers add/remove/choose the displayed one — mirrors the Album
  // route's "Also known as" strip.
  const [aliases, setAliases] = useState<AliasInfo[]>([]);
  const refetchAliases = () => {
    void libraryListTrackAliases(track.id)
      .then(setAliases)
      .catch(() => {});
  };
  useEffect(refetchAliases, [track.id]);

  useEffect(() => titleRef.current?.focus(), []);

  // The track's current metadata_json isn't on MergedTrack; load it lazily
  // only when the manager opens the advanced editor, to keep this a single
  // round-trip for the common case. We seed with "{}" and let them paste.
  function buildEdit(): { edit: MetadataEdit; err?: string } {
    const edit: MetadataEdit = {};
    if (title.trim() && title !== track.title) edit.title = title.trim();
    const tn = parseIntOrUndef(trackNo);
    if (tn !== undefined && tn !== (track.track_no ?? undefined)) edit.track_no = tn;
    const dn = parseIntOrUndef(discNo);
    if (dn !== undefined && dn !== (track.disc_no ?? undefined)) edit.disc_no = dn;
    const yr = parseIntOrUndef(year);
    if (yr !== undefined) edit.year = yr;
    if (showAdvanced && metaJson.trim()) {
      try {
        JSON.parse(metaJson);
      } catch {
        return { edit, err: "Advanced metadata must be valid JSON." };
      }
      edit.metadata_json = metaJson.trim();
    }
    return { edit };
  }

  async function save() {
    const { edit, err } = buildEdit();
    if (err) return setError(err);
    if (Object.keys(edit).length === 0) {
      onClose();
      return;
    }
    setError(null);
    setSaving(true);
    try {
      await libraryEditTrackMetadata(track.id, edit);
      onSaved();
      onClose();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Shell
      title="Edit metadata"
      onClose={onClose}
      footer={
        <>
          {!online && <span className="mr-auto text-[11px] text-oct-danger">{OFFLINE_NOTICE}</span>}
          <button onClick={onClose} className={btnGhost} disabled={saving}>
            Cancel
          </button>
          <button onClick={() => void save()} className={btnPrimary} disabled={saving || !online}>
            {saving ? "Saving…" : "Save"}
          </button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        {error && <p className={errorBox}>{error}</p>}
        <Field htmlLabel="TITLE">
          <input ref={titleRef} value={title} onChange={(e) => setTitle(e.target.value)} className={input} />
        </Field>
        <div className="grid grid-cols-3 gap-3">
          <Field htmlLabel="TRACK #">
            <input type="number" min={0} value={trackNo} onChange={(e) => setTrackNo(e.target.value)} className={input} />
          </Field>
          <Field htmlLabel="DISC #">
            <input type="number" min={0} value={discNo} onChange={(e) => setDiscNo(e.target.value)} className={input} />
          </Field>
          <Field htmlLabel="YEAR" hint="file tag only">
            <input type="number" min={0} placeholder="—" value={year} onChange={(e) => setYear(e.target.value)} className={input} />
          </Field>
        </div>

        <button
          onClick={() => setShowAdvanced((v) => !v)}
          className="self-start font-mono text-[11px] tracking-wide text-oct-accent hover:underline"
        >
          {showAdvanced ? "− Advanced (raw metadata JSON)" : "+ Advanced (raw metadata JSON)"}
        </button>
        {showAdvanced && (
          <Field htmlLabel="METADATA JSON" hint="Replaces the track's free-form metadata blob. Must be valid JSON.">
            <textarea
              value={metaJson}
              onChange={(e) => setMetaJson(e.target.value)}
              placeholder={'{\n  "comment": "remastered"\n}'}
              rows={5}
              className={`${input} font-mono text-xs`}
              spellCheck={false}
            />
          </Field>
        )}

        {/* Alternate title spellings (per language). The displayed title
            follows the server's PRIMARY_LANGUAGE; "make primary" overrides it. */}
        <div className="flex flex-col gap-2 border-t border-oct-border pt-4">
          <span className={label}>ALTERNATE SPELLINGS</span>
          <Aliases
            kind="track"
            entityId={track.id}
            aliases={aliases}
            online={online}
            isManager
            onChanged={refetchAliases}
          />
        </div>
      </div>
    </Shell>
  );
}

// ───────────────────────────── batch table ──────────────────────────────

type Row = { id: string; title: string; trackNo: string; discNo: string };
type RowStatus = "idle" | "saving" | "ok" | "error";

function BatchEditor({ tracks, online, onClose, onSaved }: Props) {
  const initial = useMemo<Row[]>(
    () =>
      tracks.map((t) => ({
        id: t.id,
        title: t.title,
        trackNo: t.track_no?.toString() ?? "",
        discNo: t.disc_no?.toString() ?? "",
      })),
    [tracks],
  );
  const originals = useMemo(() => new Map(tracks.map((t) => [t.id, t])), [tracks]);
  const [rows, setRows] = useState<Row[]>(initial);
  const [commonDisc, setCommonDisc] = useState("");
  const [commonYear, setCommonYear] = useState("");
  const [seqStart, setSeqStart] = useState("1");
  const [status, setStatus] = useState<Record<string, RowStatus>>({});
  const [rowError, setRowError] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [savedCount, setSavedCount] = useState<number | null>(null);

  function setRow(id: string, patch: Partial<Row>) {
    setRows((rs) => rs.map((r) => (r.id === id ? { ...r, ...patch } : r)));
  }

  function applyDiscToAll() {
    if (commonDisc.trim() === "") return;
    setRows((rs) => rs.map((r) => ({ ...r, discNo: commonDisc.trim() })));
  }
  function numberSequentially() {
    const start = parseIntOrUndef(seqStart) ?? 1;
    setRows((rs) => rs.map((r, i) => ({ ...r, trackNo: (start + i).toString() })));
  }

  /** Build the edit for one row vs its original; `undefined` → nothing to do. */
  function editFor(row: Row): MetadataEdit | undefined {
    const orig = originals.get(row.id);
    if (!orig) return undefined;
    const edit: MetadataEdit = {};
    if (row.title.trim() && row.title !== orig.title) edit.title = row.title.trim();
    const tn = parseIntOrUndef(row.trackNo);
    if (tn !== undefined && tn !== (orig.track_no ?? undefined)) edit.track_no = tn;
    const dn = parseIntOrUndef(row.discNo);
    if (dn !== undefined && dn !== (orig.disc_no ?? undefined)) edit.disc_no = dn;
    const yr = parseIntOrUndef(commonYear);
    if (yr !== undefined) edit.year = yr;
    return Object.keys(edit).length > 0 ? edit : undefined;
  }

  const pending = useMemo(
    () => rows.filter((r) => editFor(r) !== undefined).length,
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [rows, commonYear],
  );

  async function saveAll() {
    setSaving(true);
    setSavedCount(null);
    let ok = 0;
    let failed = 0;
    for (const row of rows) {
      const edit = editFor(row);
      if (!edit) continue;
      setStatus((s) => ({ ...s, [row.id]: "saving" }));
      try {
        await libraryEditTrackMetadata(row.id, edit);
        setStatus((s) => ({ ...s, [row.id]: "ok" }));
        setRowError((e) => {
          const n = { ...e };
          delete n[row.id];
          return n;
        });
        ok++;
      } catch (e) {
        setStatus((s) => ({ ...s, [row.id]: "error" }));
        setRowError((er) => ({ ...er, [row.id]: formatError(e) }));
        failed++;
      }
    }
    setSaving(false);
    setSavedCount(ok);
    if (ok > 0) onSaved();
    // Close only when every attempted save succeeded; keep the modal open on
    // any failure so the manager can fix + Retry the failed rows.
    if (failed === 0) onClose();
  }

  const anyError = Object.values(status).some((s) => s === "error");

  return (
    <Shell
      title={`Edit metadata — ${tracks.length} tracks`}
      onClose={onClose}
      footer={
        <>
          <span className="mr-auto text-[11px] text-oct-subtle">
            {!online ? (
              <span className="text-oct-danger">{OFFLINE_NOTICE}</span>
            ) : savedCount !== null ? (
              <>
                {savedCount} saved{anyError ? `, ${Object.keys(rowError).length} failed` : ""}.
              </>
            ) : (
              <>{pending} of {tracks.length} changed</>
            )}
          </span>
          <button onClick={onClose} className={btnGhost} disabled={saving}>
            {anyError ? "Close" : "Cancel"}
          </button>
          <button onClick={() => void saveAll()} className={btnPrimary} disabled={saving || !online || pending === 0}>
            {saving ? "Saving…" : anyError ? "Retry" : `Save ${pending || ""}`}
          </button>
        </>
      }
    >
      {/* common-field helpers */}
      <div className="mb-4 flex flex-wrap items-end gap-3 rounded-lg border border-oct-border bg-oct-card/40 p-3">
        <div className="flex items-end gap-1.5">
          <Field htmlLabel="SET DISC # FOR ALL">
            <input type="number" min={0} value={commonDisc} onChange={(e) => setCommonDisc(e.target.value)} className={`${input} w-20`} />
          </Field>
          <button onClick={applyDiscToAll} className={btnGhostSm} disabled={commonDisc.trim() === ""}>
            Apply
          </button>
        </div>
        <div className="flex items-end gap-1.5">
          <Field htmlLabel="NUMBER FROM">
            <input type="number" min={0} value={seqStart} onChange={(e) => setSeqStart(e.target.value)} className={`${input} w-20`} />
          </Field>
          <button onClick={numberSequentially} className={btnGhostSm}>
            Number sequentially
          </button>
        </div>
        <Field htmlLabel="YEAR (ALL)" hint="file tag only">
          <input type="number" min={0} placeholder="—" value={commonYear} onChange={(e) => setCommonYear(e.target.value)} className={`${input} w-24`} />
        </Field>
      </div>

      {/* per-track table */}
      <div className="grid grid-cols-[1fr_64px_64px_20px] items-center gap-x-3 border-b border-oct-border pb-2 font-mono text-[10px] tracking-[0.12em] text-oct-faint">
        <span>TITLE</span>
        <span>TRACK</span>
        <span>DISC</span>
        <span />
      </div>
      <div className="flex flex-col">
        {rows.map((r) => {
          const st = status[r.id];
          const changed = editFor(r) !== undefined;
          return (
            <div key={r.id} className="grid grid-cols-[1fr_64px_64px_20px] items-center gap-x-3 border-b border-oct-border/60 py-1.5">
              <input
                value={r.title}
                onChange={(e) => setRow(r.id, { title: e.target.value })}
                className={`${input} ${changed ? "border-oct-accent/50" : ""}`}
              />
              <input type="number" min={0} value={r.trackNo} onChange={(e) => setRow(r.id, { trackNo: e.target.value })} className={input} />
              <input type="number" min={0} value={r.discNo} onChange={(e) => setRow(r.id, { discNo: e.target.value })} className={input} />
              <span className="text-center text-[11px]" title={rowError[r.id]}>
                {st === "saving" && <span className="text-oct-subtle">…</span>}
                {st === "ok" && <span className="text-oct-online">✓</span>}
                {st === "error" && <span className="text-oct-danger">✕</span>}
              </span>
            </div>
          );
        })}
      </div>
      {anyError && (
        <p className="mt-3 text-[11px] text-oct-danger">
          Some tracks failed to save — hover the ✕ for details. Fix and press Retry.
        </p>
      )}
    </Shell>
  );
}

// ───────────────────────────── entry point ──────────────────────────────

export function MetadataEditor(props: Props) {
  if (props.tracks.length === 0) return null;
  if (props.tracks.length === 1) {
    return <SingleEditor {...props} track={props.tracks[0]} />;
  }
  return <BatchEditor {...props} />;
}

/** Small pencil button reused by the album rows + the upload report. */
export function EditMetaButton({ onClick, online, className = "" }: { onClick: () => void; online: boolean; className?: string }) {
  return (
    <button
      onClick={onClick}
      disabled={!online}
      title={online ? "Edit metadata" : OFFLINE_NOTICE}
      className={`text-oct-dim hover:text-oct-accent disabled:opacity-30 ${className}`}
    >
      <EditIcon size={14} />
    </button>
  );
}
