// Upload route (Phase 8).
//
// File picker + drag-and-drop that pushes single audio files or archives
// to the server. Manager+ gated server-side; the UI gates the affordance
// but never trusts it.

import { useState, useCallback, useRef, type DragEvent } from "react";
import { useNavigate } from "react-router-dom";
import { open } from "@tauri-apps/plugin-dialog";
import { uploadFile, type UploadResult } from "../ipc";
import { useAppStore } from "../store";

type UploadStatus =
  | { kind: "idle" }
  | { kind: "uploading"; file: string }
  | { kind: "done"; result: UploadResult }
  | { kind: "error"; file: string; message: string };

export default function Upload() {
  const tier = useAppStore((s) => s.tier);
  const navigate = useNavigate();
  const [status, setStatus] = useState<UploadStatus>({ kind: "idle" });
  const [dragOver, setDragOver] = useState(false);
  const dropRef = useRef<HTMLDivElement>(null);

  const isManager = tier === "admin" || tier === "manager";

  const doUpload = useCallback(async (path: string) => {
    const name = path.replace(/^.*[\\/]/, "");
    setStatus({ kind: "uploading", file: name });
    try {
      const result = await uploadFile(path);
      setStatus({ kind: "done", result });
    } catch (e) {
      setStatus({ kind: "error", file: name, message: String(e) });
    }
  }, []);

  async function pickFile() {
    const selected = await open({
      multiple: false,
      filters: [
        {
          name: "Audio & Archives",
          extensions: [
            // Audio
            "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv",
            "aac", "mp4",
            // Archives
            "zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz",
            "iso", "img", "nrg", "bin", "cue",
          ],
        },
      ],
    });
    if (selected) doUpload(selected);
  }

  async function pickFolder() {
    const selected = await open({
      directory: true,
      multiple: false,
    });
    if (selected) {
      // TODO: walk dir for audio files, upload each (Phase 8 stretch).
      // For now, just note the path.
      setStatus({ kind: "error", file: selected, message: "Folder upload not yet supported — pick individual files or archives" });
    }
  }

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
    // WebView drag-drop: files may arrive via dataTransfer.
    // On Tauri desktop, the webview doesn't receive file paths directly
    // from native drag-and-drop without additional setup. The primary
    // flow is the native file picker.
    if (e.dataTransfer?.files?.length) {
      // Cannot get file paths from web API — fall back to picker prompt.
      setStatus({
        kind: "error",
        file: e.dataTransfer.files[0]?.name ?? "?",
        message: "Drag-and-drop uses the browser File API — use the file picker button instead for native paths",
      });
    }
  }

  function reset() {
    setStatus({ kind: "idle" });
  }

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

  return (
    <div className="mx-auto max-w-lg">
      <h2 className="mb-2 text-lg font-semibold text-white">Upload</h2>
      <p className="mb-6 text-sm text-neutral-400">
        Push single audio tracks or archives (zip, tarball) to the server.
        ISO/CD images are recognised but not yet supported.
      </p>

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

        <div className="flex gap-3">
          <button
            onClick={pickFile}
            disabled={status.kind === "uploading"}
            className="rounded bg-blue-700 px-4 py-2 text-sm text-white hover:bg-blue-600 disabled:opacity-50"
          >
            Choose File…
          </button>
          <button
            onClick={pickFolder}
            disabled={status.kind === "uploading"}
            className="rounded bg-neutral-700 px-4 py-2 text-sm text-neutral-200 hover:bg-neutral-600 disabled:opacity-50"
          >
            Choose Folder…
          </button>
        </div>
      </div>

      {/* Status area */}
      {status.kind === "uploading" && (
        <div className="rounded bg-neutral-800/60 p-4 text-sm text-neutral-300">
          <span className="mr-2 inline-block animate-spin">↻</span>
          Uploading <span className="font-mono text-neutral-100">{status.file}</span>…
        </div>
      )}

      {status.kind === "done" && (
        <div className="rounded bg-emerald-900/30 p-4 text-sm text-emerald-200">
          <div className="mb-2 font-semibold">✓ Upload complete</div>
          {status.result.variant === "single" ? (
            <div>
              <span className="text-emerald-300">Track:</span>{" "}
              <span className="font-mono text-xs text-emerald-100">
                {status.result.data.track_id}
              </span>
              <br />
              <span className="text-emerald-300">Path:</span>{" "}
              <span className="text-xs text-emerald-100">
                {status.result.data.path}
              </span>
            </div>
          ) : (
            <div>
              <span className="text-emerald-300">Archive:</span>{" "}
              {status.result.data.kind}
              <ul className="mt-1 list-inside list-disc text-xs text-emerald-100">
                <li>Ingested: {status.result.data.ingested}</li>
                <li>Already indexed: {status.result.data.already_indexed}</li>
                <li>Skipped (non-audio): {status.result.data.non_audio_skipped}</li>
                <li>Errors: {status.result.data.errors}</li>
              </ul>
            </div>
          )}
          <button
            onClick={reset}
            className="mt-3 text-xs text-emerald-400 underline hover:text-emerald-300"
          >
            Upload another
          </button>
        </div>
      )}

      {status.kind === "error" && (
        <div className="rounded bg-red-900/30 p-4 text-sm text-red-200">
          <div className="mb-1 font-semibold">✕ Upload failed</div>
          <div className="text-xs text-red-300">
            {status.file}: {status.message}
          </div>
          <button
            onClick={reset}
            className="mt-3 text-xs text-red-400 underline hover:text-red-300"
          >
            Try again
          </button>
        </div>
      )}
    </div>
  );
}