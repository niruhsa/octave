// Upload route (Uploads v2).
//
// State lives in the global upload store (`useUploads`), NOT here — so leaving
// and returning to the tab preserves the in-flight upload, and completion
// refreshes the library no matter where you are. This route just drives the
// pickers, renders the store's live progress, and offers cancel.

import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import {
  uploadFiles,
  uploadFolder,
  uploadsCancel,
  uploadsPause,
  uploadsResume,
  type UploadItem,
} from "../ipc";
import { useAppStore } from "../store";
import { useUploadStore, useUploadBusy } from "../uploads/useUploads";
import { formatError } from "../lib/error";
import { btnGhost, btnPrimary, card, errorBox, okBox } from "../lib/ui";
import { CheckIcon, FolderIcon, UploadIcon } from "../components/icons";
import { OfflineGate } from "../components/OfflineGate";
import { formatBytes } from "../downloads/useDownloads";

const EXTS = [
  "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv", "aac", "mp4",
  "zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz",
  "iso", "img", "nrg", "bin", "cue",
];

const isAndroid = typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);

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
  const isManager = tier === "admin" || tier === "manager";

  const progress = useUploadStore((s) => s.progress);
  const lastComplete = useUploadStore((s) => s.lastComplete);
  const activeUploadId = useUploadStore((s) => s.activeUploadId);
  const paused = useUploadStore((s) => s.paused);
  const pauseReason = useUploadStore((s) => s.pauseReason);
  const dismissComplete = useUploadStore((s) => s.dismissComplete);
  const setStarting = useUploadStore((s) => s.setStarting);
  const busy = useUploadBusy();

  const [err, setErr] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [pausing, setPausing] = useState(false);

  async function startJob(starter: () => Promise<string | null>) {
    setErr(null);
    dismissComplete();
    setStarting(true);
    try {
      await ensureNotifyPermission();
      const jobId = await starter();
      if (!jobId) setStarting(false); // user cancelled the picker
    } catch (e) {
      setStarting(false);
      setErr(formatError(e));
    }
  }

  function pickFiles() {
    void startJob(async () => {
      const sel = await open({ multiple: true, filters: [{ name: "Audio & Archives", extensions: EXTS }] });
      if (!sel) return null;
      const list = Array.isArray(sel) ? sel : [sel];
      const items: UploadItem[] = list.map((p) => ({ path: p }));
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

  async function cancelActive() {
    if (!activeUploadId) return;
    setCancelling(true);
    try {
      await uploadsCancel(activeUploadId);
      // Optimistically clear; the job's own completion event will confirm.
      useUploadStore.setState({
        progress: null,
        activeUploadId: null,
        starting: false,
        paused: false,
        pauseReason: null,
      });
    } catch (e) {
      setErr(formatError(e));
    } finally {
      setCancelling(false);
    }
  }

  async function togglePause() {
    if (!activeUploadId) return;
    setPausing(true);
    setErr(null);
    try {
      if (paused) {
        await uploadsResume(activeUploadId);
        useUploadStore.setState({ paused: false, pauseReason: null });
      } else {
        await uploadsPause(activeUploadId);
        useUploadStore.setState({ paused: true, pauseReason: "manual" });
      }
    } catch (e) {
      setErr(formatError(e));
    } finally {
      setPausing(false);
    }
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

  const sessionReceived = progress?.sessionReceived ?? 0;
  const sessionTotal = progress?.sessionTotal ?? 0;
  const overallPct = sessionTotal > 0 ? Math.round(Math.min(sessionReceived / sessionTotal, 1) * 100) : 0;
  const speed = progress?.bytesPerSec ?? 0;
  const scanning = progress?.phase === "scanning" || progress?.phase === "finalizing" || (progress !== null && sessionTotal === 0);
  const indeterminate = scanning;
  const fileLabel =
    progress?.file ?? (progress ? `File ${Math.min(progress.current + 1, progress.total || 1)} of ${progress.total || 1}` : "");

  return (
    <OfflineGate feature="Uploads">
      <div className="mx-auto max-w-lg p-6 md:p-8">
        <div className="flex items-end justify-between gap-4">
          <h1 className="text-[27px] font-semibold tracking-tight">Upload</h1>
          <Link to="/uploads" className="font-mono text-[11px] text-oct-accent hover:underline">
            View reports →
          </Link>
        </div>
        <p className="mb-6 mt-1 text-sm text-oct-subtle">
          Push audio tracks or archives (zip, tarball) to the server. Every chunk is
          verified by hash; uploads run in the background with a progress notification.
        </p>

        {/* pickers */}
        <div className={`${card} mb-4 flex flex-col items-center gap-4 p-8 text-center`}>
          <span className="grid h-14 w-14 place-items-center rounded-full bg-oct-elevated text-oct-accent">
            <UploadIcon size={24} />
          </span>
          <p className="text-sm text-oct-subtle">
            {busy
              ? "An upload is already in progress — only one upload runs at a time."
              : isAndroid
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
              <span className="truncate">
                {paused ? (
                  <span className="text-oct-accent">
                    ⏸ Paused{pauseReason === "stalled" ? " · stalled" : ""}
                  </span>
                ) : progress.phase === "finalizing" ? (
                  "Finalizing…"
                ) : scanning ? (
                  (progress.message ?? "Preparing…")
                ) : (
                  `Uploading ${fileLabel}`
                )}
                {progress.total > 1 && !(scanning && progress.message)
                  ? `  ·  ${Math.min(progress.current + 1, progress.total)}/${progress.total} files`
                  : ""}
              </span>
              <span className="whitespace-nowrap">
                {indeterminate ? "" : `${overallPct}%`}
                {speed > 0 && !paused ? `  ·  ${formatBytes(speed)}/s` : ""}
              </span>
            </div>
            <div className="h-2 w-full overflow-hidden rounded-full bg-oct-line">
              <div
                className={`h-full rounded-full bg-oct-accent ${paused ? "opacity-50" : ""} ${indeterminate && !paused ? "w-1/3 animate-octpulse" : "transition-all duration-300"}`}
                style={indeterminate && !paused ? undefined : { width: `${overallPct}%` }}
              />
            </div>
            <div className="mt-3 flex items-center justify-between gap-3">
              <p className="text-[11px] text-oct-faint">
                {paused
                  ? pauseReason === "stalled"
                    ? "Paused — waiting for a connection. Resumes automatically."
                    : "Paused — resume when you're ready."
                  : "Running in the background — you can keep using the app."}
              </p>
              <div className="flex items-center gap-3 whitespace-nowrap">
                {!scanning && (
                  <button
                    onClick={togglePause}
                    disabled={pausing || !activeUploadId}
                    className="font-mono text-[11px] text-oct-accent hover:underline disabled:text-oct-faint"
                  >
                    {pausing ? "…" : paused ? "Resume" : "Pause"}
                  </button>
                )}
                <button
                  onClick={cancelActive}
                  disabled={cancelling || !activeUploadId}
                  className="font-mono text-[11px] text-oct-danger hover:underline disabled:text-oct-faint"
                >
                  {cancelling ? "Cancelling…" : "Cancel"}
                </button>
              </div>
            </div>
          </div>
        )}

        {/* completion summary */}
        {lastComplete && !progress && (
          <div className={lastComplete.filesFailed > 0 || lastComplete.state !== "completed" ? errorBox : okBox}>
            <div className="mb-2 flex items-center gap-2 font-semibold">
              {lastComplete.state === "completed" && lastComplete.filesFailed === 0 && <CheckIcon size={14} />}
              {lastComplete.state === "cancelled"
                ? "Upload cancelled"
                : lastComplete.state === "error"
                  ? "Upload failed"
                  : lastComplete.filesFailed > 0
                    ? "Upload finished with errors"
                    : "Upload complete"}
            </div>
            <ul className="list-inside list-disc text-xs opacity-90">
              <li>Files: {lastComplete.totalFiles}</li>
              <li>Tracks ingested: {lastComplete.tracksIngested}</li>
              {lastComplete.filesFailed > 0 && <li>Files failed: {lastComplete.filesFailed}</li>}
              {lastComplete.skipped > 0 && <li>Skipped (empty): {lastComplete.skipped}</li>}
            </ul>
            {lastComplete.errors.length > 0 && (
              <details className="mt-2">
                <summary className="cursor-pointer text-xs text-oct-danger">{lastComplete.errors.length} error(s)</summary>
                <ul className="mt-1 list-inside list-disc text-xs text-oct-danger">
                  {lastComplete.errors.map((e, i) => (
                    <li key={i}>{e}</li>
                  ))}
                </ul>
              </details>
            )}
            <div className="mt-3 flex items-center gap-4">
              <button onClick={dismissComplete} className="font-mono text-[11px] text-oct-accent hover:underline">
                Upload more
              </button>
              {lastComplete.uploadId && (
                <Link to={`/uploads?id=${lastComplete.uploadId}`} className="font-mono text-[11px] text-oct-accent hover:underline">
                  View report
                </Link>
              )}
            </div>
          </div>
        )}

        {err && <p className={errorBox}>{err}</p>}
      </div>
    </OfflineGate>
  );
}
