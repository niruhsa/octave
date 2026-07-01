// "Storage location" — shows the on-disk `<Language>/<Artist>` folder(s) an
// artist's files occupy and (Manager+, online) lets a manager consolidate a
// split artist under one language folder, or relocate a single-folder artist
// to a different language.
//
// An artist ends up split when its tracks were ingested under different
// language tags / name spellings (e.g. `English/aespa` + `Korean/에스파`).
// Files physically move; the artist's display name/aliases are untouched.

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  type ArtistLibraryPath,
  libraryListArtistLibraryPaths,
  librarySetArtistLanguage,
} from "../ipc";
import { byteSize } from "../lib/format";
import { formatError } from "../lib/error";
import { input } from "../lib/ui";
import { offlineAttrs } from "./OfflineGate";

type Props = {
  artistId: string;
  online: boolean;
  isManager: boolean;
  /** Refresh the parent artist/albums queries after a move. */
  onChanged: () => void;
};

// Canonical language labels, mirroring the server's `tag::normalize_language`
// outputs, so the picker offers a sensible known set alongside whatever folders
// already exist in the library.
const KNOWN_LANGUAGES = [
  "English",
  "Japanese",
  "Korean",
  "Chinese",
  "Spanish",
  "French",
  "German",
  "Italian",
  "Portuguese",
  "Russian",
  "Arabic",
  "Hindi",
  "Hebrew",
  "Greek",
  "Thai",
  "Vietnamese",
  "Indonesian",
  "Turkish",
  "Polish",
  "Dutch",
  "Swedish",
];

export function LibraryLocation({ artistId, online, isManager, onChanged }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [target, setTarget] = useState<string | null>(null); // relative_dir of chosen path
  const [language, setLanguage] = useState<string>("");

  const q = useQuery({
    queryKey: ["library", "artist-paths", artistId],
    queryFn: () => libraryListArtistLibraryPaths(artistId),
    enabled: !!artistId && online,
  });

  const paths = q.data?.paths ?? [];
  const split = paths.length > 1;

  // Known languages ∪ existing library folders ∪ this artist's current folders.
  const languages = useMemo(() => {
    const set = new Set<string>(KNOWN_LANGUAGES);
    for (const l of q.data?.library_languages ?? []) set.add(l);
    for (const p of paths) set.add(p.language);
    return [...set].sort();
  }, [q.data?.library_languages, paths]);

  // Nothing to show: no data, single folder + not a manager, or empty.
  if (q.isLoading || q.isError) return null;
  if (paths.length === 0) return null;
  if (!split && !isManager) return null;

  async function run(fn: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await fn();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  function consolidateInto(p: ArtistLibraryPath) {
    void run(async () => {
      const r = await librarySetArtistLanguage(artistId, p.language, p.artist_folder);
      setNotice(`Moved ${r.moved} track${r.moved === 1 ? "" : "s"} into ${r.target_relative_dir}.`);
      await q.refetch();
      onChanged();
    });
  }

  function moveToLanguage() {
    const lang = language.trim();
    if (!lang) return;
    void run(async () => {
      // Folder omitted → the server resolves the destination spelling (an
      // existing folder in that language, else an alias in that language, else
      // the current folder).
      const r = await librarySetArtistLanguage(artistId, lang);
      setNotice(`Moved ${r.moved} track${r.moved === 1 ? "" : "s"} into ${r.target_relative_dir}.`);
      setLanguage("");
      await q.refetch();
      onChanged();
    });
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">STORAGE LOCATION</span>
        {split && (
          <span className="inline-flex items-center gap-1 rounded-full border border-oct-offline/50 bg-oct-offline/15 px-2 py-0.5 text-[11px] text-oct-offline">
            ⚠ split across {paths.length} folders
          </span>
        )}
      </div>

      {/* Current folder(s) */}
      <div className="flex flex-col gap-1.5">
        {paths.map((p) => {
          const selectable = isManager && split;
          return (
            <label
              key={p.relative_dir}
              className={`flex items-center gap-2 rounded-md border px-2.5 py-1.5 text-[12px] ${
                selectable ? "cursor-pointer" : ""
              } ${
                target === p.relative_dir
                  ? "border-oct-accent/50 bg-oct-accent/10"
                  : "border-oct-border bg-oct-elevated/40"
              }`}
            >
              {selectable && (
                <input
                  type="radio"
                  name="relocate-target"
                  checked={target === p.relative_dir}
                  onChange={() => setTarget(p.relative_dir)}
                  className="accent-oct-accent"
                />
              )}
              <span className="min-w-0 flex-1 truncate font-mono text-oct-muted">{p.relative_dir}</span>
              <span className="shrink-0 font-mono text-[11px] text-oct-subtle">
                {p.track_count} track{p.track_count === 1 ? "" : "s"}
                {p.storage_bytes > 0 ? ` · ${byteSize(p.storage_bytes)}` : ""}
              </span>
            </label>
          );
        })}
      </div>

      {isManager && (
        <div className="flex flex-col gap-2">
          {split && (
            <div className="flex flex-wrap items-center gap-2">
              <button
                onClick={() => {
                  const p = paths.find((x) => x.relative_dir === target);
                  if (p) consolidateInto(p);
                }}
                {...offlineAttrs(online, busy || !target, "Move all files into the selected folder")}
                className="rounded-md bg-oct-accent/90 px-3 py-1.5 text-[12px] font-medium text-black hover:bg-oct-accent disabled:opacity-40"
              >
                Consolidate into selected
              </button>
              <span className="text-[11px] text-oct-subtle">
                moves the other {paths.length - 1} folder{paths.length - 1 === 1 ? "" : "s"} in and deletes them
              </span>
            </div>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <span className="text-[12px] text-oct-subtle">Move to language:</span>
            <select
              value={language}
              onChange={(e) => setLanguage(e.target.value)}
              className={`${input} max-w-[180px]`}
            >
              <option value="">Choose…</option>
              {languages.map((l) => (
                <option key={l} value={l}>
                  {l}
                </option>
              ))}
            </select>
            <button
              onClick={() => moveToLanguage()}
              {...offlineAttrs(online, busy || !language.trim(), "Move all files under this language")}
              className="rounded-md border border-oct-border px-3 py-1.5 text-[12px] text-oct-muted hover:border-oct-accent/50 hover:text-oct-accent disabled:opacity-40"
            >
              Move
            </button>
          </div>
        </div>
      )}

      {notice && <p className="text-[12px] text-oct-accent">{notice}</p>}
      {error && <p className="text-[12px] text-oct-danger">{error}</p>}
    </div>
  );
}
