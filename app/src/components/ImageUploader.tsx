// Upload artwork as part of metadata editing (Phase 9 extension). One modal,
// reused for an album cover and an artist image. Manager+ at the call site;
// the server re-enforces. The picked file is read + POSTed natively (Rust) —
// the WebView never touches the bytes.

import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { open } from "@tauri-apps/plugin-dialog";
import { libraryUploadAlbumCover, libraryUploadArtistImage } from "../ipc";
import { formatError } from "../lib/error";
import { btnGhost, btnPrimary, errorBox, label, okBox } from "../lib/ui";
import { EditIcon } from "./icons";
import { FallbackImg } from "./FallbackImg";

const IMAGE_EXTS = ["jpg", "jpeg", "png", "webp", "gif"];
const OFFLINE_NOTICE = "Uploading artwork requires a connection to the server.";

type Props = {
  kind: "album" | "artist";
  id: string;
  online: boolean;
  /** Current image URL to preview (caller cache-busts it). */
  currentUrl: string;
  onClose: () => void;
  /** Called after a successful upload — caller bumps its image version. */
  onUploaded: () => void;
};

export function ImageUploader({ kind, id, online, currentUrl, onClose, onUploaded }: Props) {
  const [pickedPath, setPickedPath] = useState<string | null>(null);
  const [pickedName, setPickedName] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  const title = kind === "album" ? "Album cover" : "Artist image";

  async function pick() {
    setError(null);
    setDone(false);
    try {
      const sel = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "Image", extensions: IMAGE_EXTS }],
      });
      if (typeof sel === "string") {
        setPickedPath(sel);
        setPickedName(sel.split(/[\\/]/).pop() ?? sel);
      }
    } catch (e) {
      setError(formatError(e));
    }
  }

  async function upload() {
    if (!pickedPath) return;
    setBusy(true);
    setError(null);
    try {
      if (kind === "album") await libraryUploadAlbumCover(id, pickedPath);
      else await libraryUploadArtistImage(id, pickedPath);
      // Keep the modal open and let the parent bump the cache-bust version
      // (→ a fresh `currentUrl` prop) so the preview below reloads to the
      // newly-uploaded image instead of closing on a stale frame.
      onUploaded();
      setPickedPath(null);
      setPickedName(null);
      setDone(true);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  return createPortal(
    <div
      className="fixed inset-0 z-[60] flex items-end justify-center bg-black/60 p-0 backdrop-blur-sm sm:items-center sm:p-6"
      onMouseDown={() => !busy && onClose()}
      role="dialog"
      aria-modal="true"
      aria-label={`Edit ${title.toLowerCase()}`}
    >
      <div
        className="flex w-full flex-col gap-4 rounded-t-2xl border border-oct-border-strong bg-oct-panel p-5 shadow-2xl sm:max-w-sm sm:rounded-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <div className="flex items-center gap-2">
          <EditIcon size={15} className="text-oct-accent" />
          <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
          <button
            onClick={() => !busy && onClose()}
            className="ml-auto font-mono text-[11px] text-oct-subtle hover:text-oct-text"
            aria-label="Close"
          >
            ESC ✕
          </button>
        </div>

        {error && <p className={errorBox}>{error}</p>}
        {done && <p className={okBox}>Image updated.</p>}

        {/* current image preview — bordered box always renders so an empty /
            failed image shows a placeholder; the image fills it when present.
            `FallbackImg` retries when `currentUrl` changes (e.g. after upload
            bumps the cache-bust token), so the new image appears immediately. */}
        <div className="flex flex-col items-center gap-2">
          <span className={`self-start ${label}`}>{done ? "UPDATED" : "CURRENT"}</span>
          <div
            className={`grid h-40 w-40 place-items-center overflow-hidden border border-oct-border bg-oct-elevated ${
              kind === "artist" ? "rounded-full" : "rounded-lg"
            }`}
          >
            <FallbackImg src={currentUrl} className="h-full w-full object-cover" />
          </div>
        </div>

        <div className="flex flex-col gap-2">
          <button onClick={() => void pick()} className={btnGhost} disabled={busy}>
            {pickedName || done ? "Choose a different image…" : "Choose image…"}
          </button>
          {pickedName && (
            <p className="truncate text-center font-mono text-[11px] text-oct-subtle">
              {pickedName}
            </p>
          )}
        </div>

        <div className="flex items-center justify-end gap-3">
          {!online && <span className="mr-auto text-[11px] text-oct-danger">{OFFLINE_NOTICE}</span>}
          {done && !pickedPath ? (
            <button onClick={onClose} className={btnPrimary}>
              Done
            </button>
          ) : (
            <>
              <button onClick={() => !busy && onClose()} className={btnGhost} disabled={busy}>
                Cancel
              </button>
              <button
                onClick={() => void upload()}
                className={btnPrimary}
                disabled={busy || !online || !pickedPath}
              >
                {busy ? "Uploading…" : "Upload"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
