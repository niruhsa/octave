// "Folder on disk" — shows the album's current on-disk folder and (Manager+,
// online) lets a manager rename it: either to match the album title (EPs,
// singles, and live albums included) or to a hand-entered name.
//
// The album folder is the third path component of a track's stored path
// (`<Language>/<Artist>/<Album>/<file>`). Renaming physically moves every track
// file into the renamed folder — the `<Language>/<Artist>/` prefix is kept, so
// a rename never moves the album out from under its artist.

import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { libraryAlbumFolder, libraryRenameAlbumFolder } from "../ipc";
import { formatError } from "../lib/error";
import { input } from "../lib/ui";
import { offlineAttrs } from "./OfflineGate";

type Props = {
  albumId: string;
  online: boolean;
  isManager: boolean;
  /** Refresh the parent album/tracks queries after a rename. */
  onChanged: () => void;
};

export function AlbumFolderLocation({ albumId, online, isManager, onChanged }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [custom, setCustom] = useState("");

  const q = useQuery({
    queryKey: ["library", "album-folder", albumId],
    queryFn: () => libraryAlbumFolder(albumId),
    enabled: !!albumId && online,
  });

  const info = q.data;
  // Seed the text box with the current folder once it loads, so a manager edits
  // from the existing name rather than an empty field.
  useEffect(() => {
    if (info?.current_folder) setCustom(info.current_folder);
  }, [info?.current_folder]);

  // Nothing on disk to act on (online-only album, no library root, or no
  // resolvable tracks) and not a manager → render nothing.
  if (q.isLoading || q.isError) return null;
  if (!info) return null;
  if (!info.current_folder && !isManager) return null;

  const suggested = info.suggested_folder;
  // "Match title" is only meaningful when the on-disk folder differs from the
  // sanitized title.
  const matchesTitle = info.current_folder === suggested;
  const trimmed = custom.trim();
  const customUnchanged = trimmed === "" || trimmed === (info.current_folder ?? "");

  async function run(folderName: string | undefined) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const r = await libraryRenameAlbumFolder(albumId, folderName);
      setNotice(
        `Renamed folder to ${r.target_relative_dir} — moved ${r.moved} file${
          r.moved === 1 ? "" : "s"
        }${r.skipped > 0 ? `, skipped ${r.skipped}` : ""}.`,
      );
      await q.refetch();
      onChanged();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-2">
      <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">FOLDER ON DISK</span>

      <div className="flex items-center gap-2 rounded-md border border-oct-border bg-oct-elevated/40 px-2.5 py-1.5 text-[12px]">
        <span className="min-w-0 flex-1 truncate font-mono text-oct-muted">
          {info.relative_dir ?? info.current_folder ?? "—"}
        </span>
        <span className="shrink-0 font-mono text-[11px] text-oct-subtle">
          {info.track_count} track{info.track_count === 1 ? "" : "s"}
        </span>
      </div>

      {isManager && (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <button
              onClick={() => void run(undefined)}
              {...offlineAttrs(
                online,
                busy || matchesTitle,
                matchesTitle
                  ? "The folder already matches the album title"
                  : `Rename the folder to "${suggested}"`,
              )}
              className="rounded-md bg-oct-accent/90 px-3 py-1.5 text-[12px] font-medium text-black hover:bg-oct-accent disabled:opacity-40"
            >
              Rename to match title
            </button>
            <span className="min-w-0 truncate font-mono text-[11px] text-oct-subtle">
              → {suggested}
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <span className="text-[12px] text-oct-subtle">Or a custom name:</span>
            <input
              value={custom}
              onChange={(e) => setCustom(e.target.value)}
              placeholder="Folder name"
              spellCheck={false}
              className={`${input} max-w-[220px]`}
            />
            <button
              onClick={() => void run(trimmed)}
              {...offlineAttrs(online, busy || customUnchanged, "Rename the folder to this name")}
              className="rounded-md border border-oct-border px-3 py-1.5 text-[12px] text-oct-muted hover:border-oct-accent/50 hover:text-oct-accent disabled:opacity-40"
            >
              Rename
            </button>
          </div>
        </div>
      )}

      {notice && <p className="text-[12px] text-oct-accent">{notice}</p>}
      {error && <p className="text-[12px] text-oct-danger">{error}</p>}
    </div>
  );
}
