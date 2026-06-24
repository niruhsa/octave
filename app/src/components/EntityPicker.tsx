// Reusable "search → pick an entity → run an action" modal (Manager+ flows).
//
// Used for two Phase-10 operations:
//   * Merge   — pick a duplicate artist/album to fold into the current one.
//   * Move    — pick a destination album to move a track into.
//
// The action itself (merge / move) is supplied by the caller via `onPick`, so
// this component stays generic: it only handles searching, listing, busy/error
// state, and closing on success. Requires a live server (search hits it).

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { librarySearchAlbums, librarySearchArtists } from "../ipc";
import { formatError } from "../lib/error";
import { errorBox, input } from "../lib/ui";

type Pickable = { id: string; label: string; sub?: string };

type Props = {
  kind: "artist" | "album";
  /** id excluded from results (the survivor, or the track's current album). */
  excludeId?: string;
  title: string;
  /** One-line explanation shown under the title. */
  hint?: string;
  /** Optional content above the search box (e.g. a single-release toggle). */
  extra?: React.ReactNode;
  online: boolean;
  /** Run the action for the picked entity. Throwing surfaces the error and
   * keeps the modal open. */
  onPick: (id: string, label: string) => Promise<void>;
  onClose: () => void;
};

export function EntityPicker({ kind, excludeId, title, hint, extra, online, onPick, onClose }: Props) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Pickable[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => inputRef.current?.focus(), []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Debounced search against the server.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setResults([]);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    const t = setTimeout(async () => {
      try {
        const items: Pickable[] =
          kind === "artist"
            ? (await librarySearchArtists(q)).items
                .filter((a) => a.id !== excludeId)
                .map((a) => ({ id: a.id, label: a.name, sub: a.sort_name ?? undefined }))
            : (await librarySearchAlbums(q)).items
                .filter((a) => a.id !== excludeId)
                .map((a) => ({ id: a.id, label: a.title, sub: a.release_year ? String(a.release_year) : undefined }));
        if (!cancelled) setResults(items);
      } catch (e) {
        if (!cancelled) setError(formatError(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [query, kind, excludeId]);

  async function pick(p: Pickable) {
    setBusyId(p.id);
    setError(null);
    try {
      await onPick(p.id, p.label);
      onClose();
    } catch (e) {
      setError(formatError(e));
      setBusyId(null);
    }
  }

  return createPortal(
    <div
      className="fixed inset-0 z-[60] flex items-end justify-center bg-black/60 p-0 backdrop-blur-sm sm:items-center sm:p-6"
      onMouseDown={onClose}
      role="dialog"
      aria-modal="true"
      aria-label={title}
    >
      <div
        className="flex max-h-[92vh] w-full flex-col overflow-hidden rounded-t-2xl border border-oct-border-strong bg-oct-panel shadow-2xl sm:max-w-xl sm:rounded-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <header className="flex items-start gap-2 border-b border-oct-border px-5 py-3.5">
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold tracking-tight">{title}</h2>
            {hint && <p className="mt-0.5 text-[11.5px] text-oct-subtle">{hint}</p>}
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
          {extra && <div className="mb-3">{extra}</div>}
          {!online && (
            <p className="mb-3 rounded-md border border-oct-offline/40 bg-oct-offline/10 px-3 py-2 text-[12px] text-oct-muted">
              This requires a connection to the server.
            </p>
          )}
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={kind === "artist" ? "Search artists…" : "Search albums…"}
            className={input}
            disabled={!online}
          />
          {error && <p className={`mt-3 ${errorBox}`}>{error}</p>}
          <div className="mt-3 flex flex-col gap-1">
            {loading && <p className="px-1 py-2 text-[12px] text-oct-subtle">Searching…</p>}
            {!loading && query.trim() && results.length === 0 && (
              <p className="px-1 py-2 text-[12px] text-oct-subtle">No matches.</p>
            )}
            {results.map((r) => (
              <button
                key={r.id}
                onClick={() => void pick(r)}
                disabled={busyId !== null}
                className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-colors hover:bg-oct-elevated disabled:opacity-50"
              >
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13.5px]">{r.label}</span>
                  {r.sub && <span className="block truncate font-mono text-[11px] text-oct-faint">{r.sub}</span>}
                </span>
                <span className="shrink-0 font-mono text-[11px] text-oct-accent">
                  {busyId === r.id ? "…" : "SELECT"}
                </span>
              </button>
            ))}
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
