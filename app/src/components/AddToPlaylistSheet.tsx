// "Add to playlist" bottom sheet, opened from a track's long-press action sheet.
//
// The point: drop a song into a playlist (existing or freshly created) by
// tapping, instead of opening the playlist and typing the track's title into the
// add-search — which is painful when the title isn't in your keyboard's
// language. Supports adding to several playlists in one go (each shows "ADDED").

import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createPortal } from "react-dom";
import { playlistAddTrack, playlistCreate, playlistList } from "../ipc";
import { broadcastInvalidate } from "../App";
import { formatError } from "../lib/error";
import { input } from "../lib/ui";
import { CheckIcon, PlaylistIcon, PlusIcon } from "./icons";

export function AddToPlaylistSheet({
  trackId,
  trackTitle,
  onClose,
}: {
  trackId: string;
  trackTitle: string;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["playlists", "mine"], queryFn: playlistList });
  const [busyId, setBusyId] = useState<string | null>(null);
  const [added, setAdded] = useState<Set<string>>(new Set());
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const playlists = q.data?.items ?? [];

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  function markAdded(id: string) {
    setAdded((prev) => new Set(prev).add(id));
  }

  async function add(playlistId: string) {
    if (busyId || added.has(playlistId)) return;
    setBusyId(playlistId);
    setError(null);
    try {
      await playlistAddTrack(playlistId, trackId, 0);
      broadcastInvalidate(["playlists", "detail", playlistId]);
      await qc.invalidateQueries({ queryKey: ["playlists", "detail", playlistId] });
      markAdded(playlistId);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusyId(null);
    }
  }

  async function createAndAdd() {
    const name = newName.trim();
    if (!name || busyId) return;
    setBusyId("__new__");
    setError(null);
    try {
      const pl = await playlistCreate(name);
      await playlistAddTrack(pl.id, trackId, 0);
      broadcastInvalidate(["playlists", "mine"]);
      broadcastInvalidate(["playlists"]);
      broadcastInvalidate(["playlists", "detail", pl.id]);
      await qc.invalidateQueries({ queryKey: ["playlists", "mine"] });
      markAdded(pl.id);
      setNewName("");
      setCreating(false);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusyId(null);
    }
  }

  return createPortal(
    <div
      className="fixed inset-0 z-[70] flex items-end justify-center bg-black/60 backdrop-blur-sm sm:items-center sm:p-6"
      onMouseDown={onClose}
      role="dialog"
      aria-modal="true"
      aria-label="Add to playlist"
    >
      <div
        className="flex max-h-[88vh] w-full flex-col overflow-hidden rounded-t-2xl border border-oct-border-strong bg-oct-panel shadow-2xl sm:max-w-md sm:rounded-2xl"
        onMouseDown={(e) => e.stopPropagation()}
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <header className="flex items-start gap-2 border-b border-oct-border px-5 py-3.5">
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold tracking-tight">Add to playlist</h2>
            <p className="mt-0.5 truncate text-[11.5px] text-oct-subtle">{trackTitle}</p>
          </div>
          <button
            onClick={onClose}
            className="font-mono text-[11px] text-oct-subtle hover:text-oct-text"
            aria-label="Close"
          >
            ESC ✕
          </button>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto p-3">
          {creating ? (
            <div className="mb-2 flex items-center gap-2">
              <input
                autoFocus
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void createAndAdd();
                  if (e.key === "Escape") {
                    setCreating(false);
                    setNewName("");
                  }
                }}
                placeholder="New playlist name…"
                maxLength={200}
                className={`${input} min-w-0 flex-1`}
              />
              <button
                onClick={() => void createAndAdd()}
                disabled={!newName.trim() || busyId === "__new__"}
                className="shrink-0 rounded-lg bg-oct-accent/90 px-3 py-2 text-[12.5px] font-medium text-oct-bg hover:bg-oct-accent disabled:opacity-40"
              >
                {busyId === "__new__" ? "…" : "Create"}
              </button>
            </div>
          ) : (
            <button
              onClick={() => setCreating(true)}
              className="mb-2 flex w-full items-center gap-3 rounded-lg border border-dashed border-oct-border-strong px-3 py-2.5 text-left text-[13.5px] text-oct-muted transition-colors hover:border-oct-accent/50 hover:text-oct-accent"
            >
              <span
                className="grid h-9 w-9 shrink-0 place-items-center rounded-lg text-oct-accent"
                style={{ background: "rgba(224,168,75,0.12)" }}
              >
                <PlusIcon size={16} />
              </span>
              New playlist
            </button>
          )}

          {error && <p className="mb-2 px-1 text-[12px] text-oct-danger">{error}</p>}

          {q.isLoading && <p className="px-1 py-3 text-[12px] text-oct-subtle">Loading playlists…</p>}
          {q.isError && <p className="px-1 py-3 text-[12px] text-oct-danger">{formatError(q.error)}</p>}
          {q.data && playlists.length === 0 && !creating && (
            <p className="px-1 py-3 text-[12px] text-oct-subtle">No playlists yet — create one above.</p>
          )}

          <div className="flex flex-col gap-0.5">
            {playlists.map((p) => {
              const isAdded = added.has(p.id);
              return (
                <button
                  key={p.id}
                  onClick={() => void add(p.id)}
                  disabled={isAdded || busyId !== null}
                  className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-colors hover:bg-oct-elevated disabled:cursor-default disabled:hover:bg-transparent"
                >
                  <span
                    className="grid h-9 w-9 shrink-0 place-items-center rounded-lg text-oct-accent"
                    style={{ background: "rgba(224,168,75,0.12)" }}
                  >
                    <PlaylistIcon size={16} />
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[13.5px]">{p.name}</span>
                  {isAdded ? (
                    <span className="flex shrink-0 items-center gap-1 font-mono text-[11px] text-oct-online">
                      <CheckIcon size={12} /> ADDED
                    </span>
                  ) : (
                    <span className="shrink-0 font-mono text-[11px] text-oct-accent">
                      {busyId === p.id ? "…" : "ADD"}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </div>

        <button
          onClick={onClose}
          className="w-full border-t border-oct-border px-5 py-3.5 text-center text-[13px] text-oct-subtle hover:text-oct-text"
        >
          Done
        </button>
      </div>
    </div>,
    document.body,
  );
}
