// Upload route (Phase 8) — background uploads with OS notifications.
//
// Uploads run as a background job in Rust: the picker resolves sources, we
// start the job (returns a jobId immediately), and a progress notification
// updates per file then is replaced by a completion notification. The in-app
// UI mirrors the same progress via events but the user can navigate away —
// the job keeps running and the notification carries the status.
//
// Platform handling:
//   * Desktop — the dialog returns real paths; folder upload walks a tree.
//   * Android — the dialog returns SAF `content://` URIs; we read each via the
//     fs plugin and stage it into the app cache, then hand the temp paths to
//     Rust (which deletes them after upload). Folder selection isn't available
//     through SAF here, so "bulk" upload uses the picker's multi-select.

import { useState, useEffect, useRef, useCallback } from "react";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { open } from "@tauri-apps/plugin-dialog";
import { readFile, writeFile, mkdir, BaseDirectory } from "@tauri-apps/plugin-fs";
import { appCacheDir, join } from "@tauri-apps/api/path";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import {
  uploadFiles,
  uploadFolder,
  onUploadProgress,
  onUploadComplete,
  type UploadItem,
  type UploadProgressEvent,
  type UploadCompleteEvent,
} from "../ipc";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { formatError } from "../lib/error";
import { btnGhost, btnPrimary, card, errorBox, okBox } from "../lib/ui";
import { CheckIcon, FolderIcon, UploadIcon } from "../components/icons";
import { OfflineGate } from "../components/OfflineGate";

const EXTS = [
  "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv", "aac", "mp4",
  "zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz",
  "iso", "img", "nrg", "bin", "cue",
];

const isAndroid = typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);

/**
 * Read an Android `content://` URI via the fs plugin and stage it into the app
 * cache, returning a temp filesystem path for the Rust job (deleted after
 * upload via `cleanup`). Content-URI names are unreliable, so the name is a
 * best-effort hint — Rust sniffs the real format from the bytes.
 */
async function stageUri(uri: string, index: number): Promise<UploadItem> {
  const bytes = await readFile(uri);
  const raw = decodeURIComponent((uri.split("/").pop() ?? "").split("?")[0] ?? "");
  const safe = raw.replace(/[^\w.\-]+/g, "_").replace(/^_+|_+$/g, "") || `upload-${index + 1}`;
  const rel = `octave-uploads/${Date.now()}-${index}-${safe}`;
  await mkdir("octave-uploads", { baseDir: BaseDirectory.AppCache, recursive: true });
  await writeFile(rel, bytes, { baseDir: BaseDirectory.AppCache });
  const abs = await join(await appCacheDir(), rel);
  return { path: abs, name: safe, cleanup: true };
}

async function ensureNotifyPermission(): Promise<void> {
  try {
    if (!(await isPermissionGranted())) await requestPermission();
  } catch {
    /* notifications unavailable — uploads still work, just no OS notice */
  }
}

export default function Upload() {
  const tier = useAppStore((s) => s.tier);
  const navigate = useNavigate();
  const qc = useQueryClient();
  const isManager = tier === "admin" || tier === "manager";

  const [progress, setProgress] = useState<UploadProgressEvent | null>(null);
  const [result, setResult] = useState<UploadCompleteEvent | null>(null);
  const [starting, setStarting] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const jobIdRef = useRef<string | null>(null);

  // Track the active job via background events (works even if the user
  // navigates away and back while this component is mounted).
  useEffect(() => {
    let un1: (() => void) | undefined;
    let un2: (() => void) | undefined;
    onUploadProgress((e) => {
      if (e.jobId === jobIdRef.current) setProgress(e);
    }).then((f) => (un1 = f));
    onUploadComplete((e) => {
      if (e.jobId !== jobIdRef.current) return;
      setResult(e);
      setProgress(null);
      jobIdRef.current = null;
      qc.invalidateQueries({ queryKey: ["library"] });
      broadcastInvalidate(["library"]);
    }).then((f) => (un2 = f));
    return () => {
      un1?.();
      un2?.();
    };
  }, [qc]);

  const startJob = useCallback(async (starter: () => Promise<string | null>) => {
    setErr(null);
    setStarting(true);
    try {
      await ensureNotifyPermission();
      const jobId = await starter();
      if (jobId) {
        jobIdRef.current = jobId;
        setResult(null);
        setProgress({ jobId, phase: "scanning", current: 0, total: 0, file: null, ok: null, message: null });
      }
    } catch (e) {
      setErr(formatError(e));
    } finally {
      setStarting(false);
    }
  }, []);

  function pickFiles() {
    void startJob(async () => {
      const sel = await open({ multiple: true, filters: [{ name: "Audio & Archives", extensions: EXTS }] });
      if (!sel) return null;
      const list = Array.isArray(sel) ? sel : [sel];
      let items: UploadItem[];
      if (isAndroid) {
        // Stage sequentially so we never hold more than one file in memory.
        items = [];
        for (let i = 0; i < list.length; i++) items.push(await stageUri(list[i], i));
      } else {
        items = list.map((p) => ({ path: p }));
      }
      return uploadFiles(items);
    });
  }

  function pickFolder() {
    void startJob(async () => {
      const sel = await open({ directory: true, multiple: false });
      if (!sel || Array.isArray(sel)) return null;
      return uploadFolder(sel);
    });
  }

  if (!isManager) {
    return (
      <div className="mx-auto max-w-lg px-6 pt-16 text-center text-oct-subtle">
        <p className="text-lg text-oct-muted">Uploads require Manager or Admin permission.</p>
        <button onClick={() => navigate("/")} className="mt-4 font-mono text-[11px] text-oct-accent hover:underline">
          ← Back to Home
        </button>
      </div>
    );
  }

  const busy = starting || progress !== null;
  const total = progress?.total ?? 0;
  const current = progress?.current ?? 0;
  const pct = total > 0 ? Math.round((current / total) * 100) : 0;
  const scanning = progress?.phase === "scanning" || (busy && total === 0);

  return (
    <OfflineGate feature="Uploads">
      <div className="mx-auto max-w-lg p-6 md:p-8">
        <h1 className="text-[27px] font-semibold tracking-tight">Upload</h1>
        <p className="mb-6 mt-1 text-sm text-oct-subtle">
          Push audio tracks or archives (zip, tarball) to the server. Uploads run in the
          background with a progress notification.
        </p>

        {/* pickers */}
        <div className={`${card} mb-4 flex flex-col items-center gap-4 p-8 text-center`}>
          <span className="grid h-14 w-14 place-items-center rounded-full bg-oct-elevated text-oct-accent">
            <UploadIcon size={24} />
          </span>
          <p className="text-sm text-oct-subtle">
            {isAndroid
              ? "Pick one or more files. The Android picker supports selecting many at once."
              : "Choose files, or a whole folder to upload everything inside."}
          </p>
          <div className="flex flex-wrap items-center justify-center gap-3">
            <button onClick={pickFiles} disabled={busy} className={btnPrimary}>
              <UploadIcon size={14} /> Choose files…
            </button>
            {!isAndroid && (
              <button onClick={pickFolder} disabled={busy} className={btnGhost}>
                <FolderIcon size={15} /> Choose folder…
              </button>
            )}
          </div>
        </div>

        {/* live progress */}
        {progress && (
          <div className={`${card} mb-4 p-4`}>
            <div className="mb-2 flex items-center justify-between font-mono text-[11px] text-oct-subtle">
              <span>{scanning ? "Scanning…" : `Uploading ${current} / ${total}`}</span>
              <span>{scanning ? "" : `${pct}%`}</span>
            </div>
            <div className="h-2 w-full overflow-hidden rounded-full bg-oct-line">
              <div
                className={`h-full rounded-full bg-oct-accent ${scanning ? "w-1/3 animate-octpulse" : "transition-all duration-300"}`}
                style={scanning ? undefined : { width: `${pct}%` }}
              />
            </div>
            {progress.file && !scanning && (
              <p className="mt-2 truncate font-mono text-[11px] text-oct-muted">{progress.file}</p>
            )}
            <p className="mt-2 text-[11px] text-oct-faint">
              Running in the background — you can keep using the app; the notification shows progress.
            </p>
          </div>
        )}

        {/* completion summary */}
        {result && (
          <div className={result.failed > 0 ? errorBox : okBox}>
            <div className="mb-2 flex items-center gap-2 font-semibold">
              {result.failed === 0 && <CheckIcon size={14} />}
              {result.failed > 0 ? "Upload finished with errors" : "Upload complete"}
            </div>
            <ul className="list-inside list-disc text-xs opacity-90">
              <li>Total: {result.total}</li>
              <li>Succeeded: {result.succeeded}</li>
              {result.failed > 0 && <li>Failed: {result.failed}</li>}
              {result.skipped > 0 && <li>Skipped (empty): {result.skipped}</li>}
            </ul>
            {result.errors.length > 0 && (
              <details className="mt-2">
                <summary className="cursor-pointer text-xs text-oct-danger">{result.errors.length} error(s)</summary>
                <ul className="mt-1 list-inside list-disc text-xs text-oct-danger">
                  {result.errors.map((e, i) => (
                    <li key={i}>{e}</li>
                  ))}
                </ul>
              </details>
            )}
            <button onClick={() => setResult(null)} className="mt-3 font-mono text-[11px] text-oct-accent hover:underline">
              Upload more
            </button>
          </div>
        )}

        {err && <p className={errorBox}>{err}</p>}
      </div>
    </OfflineGate>
  );
}
