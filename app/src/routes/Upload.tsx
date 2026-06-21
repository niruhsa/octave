// Upload route (Phase 8).
//
// File picker + folder upload + drag-and-drop zone. Pushes single audio
// files or archives to the server. Folder mode walks the tree, collects
// uploadable files, and uploads each in sequence with live progress.
// Manager+ gated server-side; the UI gates the affordance.

import { useState, useCallback, useRef, useEffect, type DragEvent } from "react";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { open } from "@tauri-apps/plugin-dialog";
import {
  uploadFile,
  uploadFolder,
  onUploadProgress,
  type UploadResult,
  type FolderUploadResult,
} from "../ipc";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";

type SingleStatus =
  | { kind: "idle" }
  | { kind: "uploading"; file: string }
  | { kind: "done"; result: UploadResult }
  | { kind: "error"; file: string; message: string };

type FolderStatus =
  | { kind: "idle" }
  | { kind: "uploading"; current: number; total: number; file: string; ok?: boolean }
  | { kind: "done"; result: FolderUploadResult }
  | { kind: "error"; message: string };

type Tab = "file" | "folder";

export default function Upload() {
  const tier = useAppStore((s) => s.tier);
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [tab, setTab] = useState<Tab>("file");
  const [single, setSingle] = useState<SingleStatus>({ kind: "idle" });
  const [folder, setFolder] = useState<FolderStatus>({ kind: "idle" });
  const [dragOver, setDragOver] = useState(false);
  const dropRef = useRef<HTMLDivElement>(null);

  const isManager = tier === "admin" || tier === "manager";

  // ── Single-file upload ────────────────────────────────────────────

  const doUpload = useCallback(async (path: string) => {
    const name = path.replace(/^.*[\\/]/, "");
    setSingle({ kind: "uploading", file: name });
    try {
      const result = await uploadFile(path);
      setSingle({ kind: "done", result });
      // Invalidate library queries so navigating to Library / Search
      // shows the new content without a manual refresh.
      qc.invalidateQueries({ queryKey: ["library"] });
      broadcastInvalidate(["library"]);
    } catch (e) {
      setSingle({ kind: "error", file: name, message: String(e) });
    }
  }, [qc]);

  async function pickFile() {
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: "Audio & Archives",
          extensions: [
            "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv",
            "aac", "mp4",
            "zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz",
            "iso", "img", "nrg", "bin", "cue",
          ],
        },
      ],
    });
    if (selected) doUpload(selected);
  }

  // ── Folder upload ─────────────────────────────────────────────────

  async function pickFolder() {
    const selected = await open({ directory: true, multiple: false });
    if (!selected) return;

    setFolder({ kind: "uploading", current: 0, total: 0, file: "scanning…" });

    try {
      const result = await uploadFolder(selected);
      setFolder({ kind: "done", result });
      qc.invalidateQueries({ queryKey: ["library"] });
      broadcastInvalidate(["library"]);
    } catch (e) {
      setFolder({ kind: "error", message: String(e) });
    }
  }

  // Listen to per-file progress events during folder upload.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onUploadProgress((e) => {
      if (e.phase === "scanning") {
        setFolder({ kind: "uploading", current: 0, total: 0, file: "scanning…" });
      } else if (e.phase === "uploading") {
        setFolder({
          kind: "uploading",
          current: e.current,
          total: e.total,
          file: e.file ?? "?",
          ok: e.ok,
        });
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // ── Drag-and-drop ─────────────────────────────────────────────────

  function handleDragOver(e: DragEvent) {
    e.preventDefault();
    setDragOver(true);
  }
  function handleDragLeave(e: DragEvent) {
    e.preventDefault();
    setDragOver(false);
  }
  async function handleDrop(e: DragEvent) {
    e.preventDefault();
    setDragOver(false);
    if (e.dataTransfer?.files?.length) {
      setSingle({
        kind: "error",
        file: e.dataTransfer.files[0]?.name ?? "?",
        message:
          "Drag-and-drop uses the browser File API — use the file/folder picker buttons instead for native paths",
      });
    }
  }

  function reset() {
    setSingle({ kind: "idle" });
    setFolder({ kind: "idle" });
  }

  // ── Permission gate ───────────────────────────────────────────────

  if (!isManager) {
    return (
      <div className="mx-auto max-w-lg pt-12 text-center text-neutral-400">
        <p className="text-lg">Uploads require Manager or Admin permission.</p>
        <button
          onClick={() => navigate("/")}
          className="mt-4 text-sm text-blue-400 underline hover:text-blue-300"
        >
          Back to Home
        </button>
      </div>
    );
  }

  // ── Folder progress bar ───────────────────────────────────────────

  function folderProgress() {
    if (folder.kind !== "uploading") return null;
    const pct =
      folder.total > 0 ? Math.round((folder.current / folder.total) * 100) : 0;
    return (
      <div className="mb-4">
        <div className="mb-1 flex items-center justify-between text-xs text-neutral-400">
          <span>
            {folder.current}/{folder.total}
          </span>
          <span className="truncate font-mono text-neutral-300">
            {folder.file}
          </span>
        </div>
        <div className="h-2 w-full overflow-hidden rounded-full bg-neutral-800">
          <div
            className="h-full rounded-full bg-blue-500 transition-all duration-300"
            style={{ width: `${pct}%` }}
          />
        </div>
        {folder.ok === false && (
          <div className="mt-1 text-xs text-red-400">last file failed — continues anyway</div>
        )}
      </div>
    );
  }

  // ── Main render ───────────────────────────────────────────────────

  return (
    <div className="mx-auto max-w-lg">
      <h2 className="mb-2 text-lg font-semibold text-white">Upload</h2>
      <p className="mb-6 text-sm text-neutral-400">
        Push audio tracks or archives (zip, tarball) to the server.
        ISO/CD images are recognised but not yet supported.
      </p>

      {/* Tabs */}
      <div className="mb-4 flex gap-1 rounded-lg bg-neutral-900 p-0.5 text-sm">
        {(["file", "folder"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`flex-1 rounded-md px-3 py-1.5 text-center transition-colors ${
              tab === t
                ? "bg-neutral-700 text-white"
                : "text-neutral-400 hover:text-neutral-200"
            }`}
          >
            {t === "file" ? "Single File" : "Folder"}
          </button>
        ))}
      </div>

      {tab === "file" ? (
        <>
          {/* Drop zone */}
          <div
            ref={dropRef}
            onDragOver={handleDragOver}
            onDragLeave={handleDragLeave}
            onDrop={handleDrop}
            className={`mb-4 flex flex-col items-center gap-3 rounded-xl border-2 border-dashed p-10 transition-colors ${
              dragOver
                ? "border-blue-400 bg-blue-950/20"
                : "border-neutral-700 bg-neutral-900/30"
            }`}
          >
            <span className="text-3xl">⬆</span>
            <span className="text-sm text-neutral-400">
              Drop a file here, or use the picker
            </span>
            <button
              onClick={pickFile}
              disabled={single.kind === "uploading" || folder.kind === "uploading"}
              className="rounded bg-blue-700 px-4 py-2 text-sm text-white hover:bg-blue-600 disabled:opacity-50"
            >
              Choose File…
            </button>
          </div>

          {/* Single-file status */}
          {single.kind === "uploading" && (
            <div className="rounded bg-neutral-800/60 p-4 text-sm text-neutral-300">
              <span className="mr-2 inline-block animate-spin">↻</span>
              Uploading{" "}
              <span className="font-mono text-neutral-100">{single.file}</span>…
            </div>
          )}
          {single.kind === "done" && <SingleResult result={single.result} onReset={reset} />}
          {single.kind === "error" && (
            <ErrorBox file={single.file} message={single.message} onReset={reset} />
          )}
        </>
      ) : (
        <>
          {/* Folder drop zone */}
          <div
            onDragOver={handleDragOver}
            onDragLeave={handleDragLeave}
            onDrop={handleDrop}
            className={`mb-4 flex flex-col items-center gap-3 rounded-xl border-2 border-dashed p-10 transition-colors ${
              dragOver
                ? "border-blue-400 bg-blue-950/20"
                : "border-neutral-700 bg-neutral-900/30"
            }`}
          >
            <span className="text-3xl">📁</span>
            <span className="text-sm text-neutral-400">
              Select a folder — every audio and archive file inside is uploaded
            </span>
            <button
              onClick={pickFolder}
              disabled={single.kind === "uploading" || folder.kind === "uploading"}
              className="rounded bg-blue-700 px-4 py-2 text-sm text-white hover:bg-blue-600 disabled:opacity-50"
            >
              Choose Folder…
            </button>
          </div>

          {folderProgress()}

          {/* Folder result */}
          {folder.kind === "done" && (
            <div className="rounded bg-emerald-900/30 p-4 text-sm text-emerald-200">
              <div className="mb-2 font-semibold">✓ Folder upload complete</div>
              <ul className="list-inside list-disc text-xs text-emerald-100">
                <li>Total: {folder.result.total}</li>
                <li>Succeeded: {folder.result.succeeded}</li>
                <li>Failed: {folder.result.failed}</li>
                <li>Skipped (empty): {folder.result.skipped}</li>
              </ul>
              {folder.result.errors.length > 0 && (
                <details className="mt-2">
                  <summary className="cursor-pointer text-xs text-red-300">
                    {folder.result.errors.length} error(s)
                  </summary>
                  <ul className="mt-1 list-inside list-disc text-xs text-red-200">
                    {folder.result.errors.map((e, i) => (
                      <li key={i}>{e}</li>
                    ))}
                  </ul>
                </details>
              )}
              <button
                onClick={reset}
                className="mt-3 text-xs text-emerald-400 underline hover:text-emerald-300"
              >
                Upload another
              </button>
            </div>
          )}
          {folder.kind === "error" && (
            <ErrorBox file="folder" message={folder.message} onReset={reset} />
          )}
        </>
      )}
    </div>
  );
}

// ── Sub-components ────────────────────────────────────────────────────────

function SingleResult({
  result,
  onReset,
}: {
  result: UploadResult;
  onReset: () => void;
}) {
  return (
    <div className="rounded bg-emerald-900/30 p-4 text-sm text-emerald-200">
      <div className="mb-2 font-semibold">✓ Upload complete</div>
      {result.variant === "single" ? (
        <div>
          <span className="text-emerald-300">Track:</span>{" "}
          <span className="font-mono text-xs text-emerald-100">
            {result.data.track_id}
          </span>
          <br />
          <span className="text-emerald-300">Path:</span>{" "}
          <span className="text-xs text-emerald-100">{result.data.path}</span>
        </div>
      ) : (
        <div>
          <span className="text-emerald-300">Archive:</span> {result.data.kind}
          <ul className="mt-1 list-inside list-disc text-xs text-emerald-100">
            <li>Ingested: {result.data.ingested}</li>
            <li>Already indexed: {result.data.already_indexed}</li>
            <li>Skipped (non-audio): {result.data.non_audio_skipped}</li>
            <li>Errors: {result.data.errors}</li>
          </ul>
        </div>
      )}
      <button
        onClick={onReset}
        className="mt-3 text-xs text-emerald-400 underline hover:text-emerald-300"
      >
        Upload another
      </button>
    </div>
  );
}

function ErrorBox({
  file,
  message,
  onReset,
}: {
  file: string;
  message: string;
  onReset: () => void;
}) {
  return (
    <div className="rounded bg-red-900/30 p-4 text-sm text-red-200">
      <div className="mb-1 font-semibold">✕ Upload failed</div>
      <div className="text-xs text-red-300">
        {file}: {message}
      </div>
      <button
        onClick={onReset}
        className="mt-3 text-xs text-red-400 underline hover:text-red-300"
      >
        Try again
      </button>
    </div>
  );
}