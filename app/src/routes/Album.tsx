import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useNavigate } from "react-router-dom";
import {
  cacheGetAlbum,
  coverUrl,
  downloadAlbum,
  downloadDelete,
  downloadTrack,
  libraryDeleteAlbum,
  libraryDeleteTrack,
  libraryListTracksByAlbum,
} from "../ipc";
import { Cover } from "../components/Cover";
import { SourceBadge } from "../components/SourceBadge";
import { EqBars } from "../components/EqBars";
import { formatDuration } from "../lib/format";
import { qualityLabel } from "../lib/visual";
import { formatError } from "../lib/error";
import { usePlayerStore } from "../player/store";
import { useDownloadsStore } from "../downloads/useDownloads";
import { broadcastInvalidate } from "../App";
import { useAppStore } from "../store";
import { btnDanger, btnGhost, btnPrimary, errorBox } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { SkeletonHero, SkeletonTracks } from "../components/Skeleton";
import {
  DownloadIcon,
  EditIcon,
  PlayIcon,
  ShuffleIcon,
  TrashIcon,
} from "../components/icons";
import { EditMetaButton, MetadataEditor } from "../components/MetadataEditor";
import { ImageUploader } from "../components/ImageUploader";
import type { MergedTrack } from "../ipc";

function totalLabel(ms: number): string {
  const min = Math.round(ms / 60000);
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} hr ${min % 60} min`;
}

export default function Album() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const isManager = tier === "admin" || tier === "manager";

  const q = useQuery({
    queryKey: ["library", "tracks-by-album", id],
    queryFn: () => libraryListTracksByAlbum(id),
    enabled: !!id,
  });
  // Best-effort album metadata (title) from the offline cache — present for
  // downloaded/cached albums; online-only albums fall back to "Album".
  const meta = useQuery({
    queryKey: ["cache", "album", id],
    queryFn: () => cacheGetAlbum(id),
    enabled: !!id,
  });

  const playTrack = usePlayerStore((s) => s.playTrack);
  const playQueue = usePlayerStore((s) => s.playQueue);
  const queue = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const currentId = currentIndex >= 0 ? queue[currentIndex]?.id : undefined;
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);

  // Metadata editor (Manager+). `null` = closed; a non-empty list opens the
  // single (1) or batch (>1) editor.
  const [editTracks, setEditTracks] = useState<MergedTrack[] | null>(null);
  // Cover-art uploader (Manager+) + a cache-bust token bumped after upload.
  const [editCover, setEditCover] = useState(false);
  const [coverVersion, setCoverVersion] = useState(0);

  async function onMetaSaved() {
    await qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] });
    broadcastInvalidate(["library"]);
  }
  function onCoverUploaded() {
    setCoverVersion(Date.now());
    void qc.invalidateQueries({ queryKey: ["cache", "album", id] });
    broadcastInvalidate(["library"]);
  }

  const items = q.data?.items ?? [];
  const totalMs = items.reduce((s, t) => s + t.duration_ms, 0);
  const anyDownloaded = items.some((t) => t.downloaded);
  const title = meta.data?.title ?? "Album";

  async function dlTrack(track: MergedTrack) {
    try {
      await downloadTrack(track.id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function dlAlbum() {
    try {
      await downloadAlbum(id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function removeTrack(track: MergedTrack) {
    try {
      await downloadDelete(track.id);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] }),
        refreshStorage(),
      ]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function delTrack(track: MergedTrack) {
    if (!window.confirm(`Permanently delete "${track.title}" from the server?`)) return;
    try {
      await libraryDeleteTrack(track.id);
      await qc.invalidateQueries({ queryKey: ["library", "tracks-by-album", id] });
      broadcastInvalidate(["library"]);
    } catch (e) {
      alert(formatError(e));
    }
  }
  async function delAlbum() {
    if (!window.confirm("Permanently delete this entire album from the server? All tracks will be removed.")) return;
    try {
      await libraryDeleteAlbum(id);
      navigate("/library");
    } catch (e) {
      alert(formatError(e));
    }
  }

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <Link to="/library" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">
        ← LIBRARY
      </Link>

      {/* hero */}
      {q.isLoading ? (
        <SkeletonHero />
      ) : (
      <header className="flex flex-col gap-5 sm:flex-row sm:items-end">
        <div className="relative shrink-0" style={{ width: 132 }}>
          <Cover
            album={{ id, cover_path: meta.data ? "1" : null, local_cover_path: null }}
            tryCover
            size={132}
            radius={10}
            version={coverVersion || undefined}
            className="shadow-[0_10px_24px_-10px_rgba(0,0,0,0.6)]"
          />
          {isManager && (
            <button
              onClick={() => setEditCover(true)}
              {...offlineAttrs(online, false, "Edit cover art")}
              className="absolute bottom-1.5 right-1.5 grid h-7 w-7 place-items-center rounded-full bg-black/60 text-white/90 backdrop-blur-sm transition-colors hover:bg-black/80 disabled:opacity-40"
            >
              <EditIcon size={13} />
            </button>
          )}
        </div>
        <div className="flex min-w-0 flex-col">
          <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">ALBUM</span>
          <h1 className="mt-1.5 text-3xl font-semibold tracking-tight sm:text-[34px]">{title}</h1>
          <p className="mt-2 flex flex-wrap items-center gap-x-2 text-[13px] text-oct-subtle">
            <span className="font-mono">
              {items.length} song{items.length === 1 ? "" : "s"}
              {totalMs > 0 ? ` · ${totalLabel(totalMs)}` : ""}
            </span>
            {q.data && <SourceBadge source={q.data.source} />}
          </p>
        </div>
      </header>
      )}

      {/* actions */}
      {items.length > 0 && (
        <div className="flex flex-wrap items-center gap-3">
          <button onClick={() => playQueue(items, 0)} className={btnPrimary}>
            <PlayIcon size={13} /> Play
          </button>
          <button
            onClick={() => {
              const st = usePlayerStore.getState();
              if (!st.shuffle) st.toggleShuffle();
              playQueue(items, 0);
            }}
            className={btnGhost}
          >
            <ShuffleIcon size={14} /> Shuffle
          </button>
          <button onClick={dlAlbum} className={btnGhost} {...offlineAttrs(online)}>
            <DownloadIcon size={14} /> Download
          </button>
          {anyDownloaded && (
            <Link to="/downloads" className="font-mono text-[11px] text-oct-accent hover:underline">
              manage downloads
            </Link>
          )}
          {isManager && (
            <div className="ml-auto flex items-center gap-3">
              <button
                onClick={() => setEditTracks(items)}
                className={`${btnGhost} hidden sm:inline-flex`}
                {...offlineAttrs(online, false, "Edit metadata for all tracks")}
              >
                <EditIcon size={14} /> Edit tags
              </button>
              <button onClick={delAlbum} className={btnDanger} {...offlineAttrs(online)}>
                <TrashIcon size={14} /> Delete album
              </button>
            </div>
          )}
        </div>
      )}

      {q.isLoading && <SkeletonTracks rows={9} cols={4} />}
      {q.isError && <p className={errorBox}>{formatError(q.error)}</p>}

      {/* track table */}
      {q.data && (
        <div className="flex flex-col">
          {items.length === 0 ? (
            <p className="text-sm text-oct-subtle">No tracks.</p>
          ) : (
            <>
              <div className="grid grid-cols-[28px_1fr_110px_56px] items-center gap-x-4 border-b border-oct-border px-2 pb-2.5 font-mono text-[10.5px] tracking-[0.1em] text-oct-faint">
                <span>#</span>
                <span>TITLE</span>
                <span className="hidden sm:block">QUALITY</span>
                <span className="text-right">TIME</span>
              </div>
              {items.map((t, i) => {
                const active = t.id === currentId;
                return (
                  <div
                    key={t.id}
                    onClick={() => playTrack(t, items)}
                    className={`group grid cursor-pointer grid-cols-[28px_1fr_110px_56px] items-center gap-x-4 rounded-lg px-2 py-2.5 text-[13.5px] ${
                      active ? "bg-oct-elevated" : "hover:bg-oct-elevated/50"
                    }`}
                  >
                    <span className="flex justify-center">
                      {active ? (
                        <EqBars playing={isPlaying} />
                      ) : (
                        <span className="font-mono text-xs text-oct-faint">{t.track_no ?? i + 1}</span>
                      )}
                    </span>
                    <span className="flex min-w-0 items-center gap-2">
                      <span className={`truncate ${active ? "font-medium text-oct-accent" : ""}`}>
                        {t.title}
                      </span>
                      {t.downloaded && <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-oct-accent" title="downloaded" />}
                    </span>
                    <span className="hidden font-mono text-[11px] text-oct-subtle sm:block">
                      {qualityLabel(t)}
                    </span>
                    <span className="flex items-center justify-end gap-2">
                      <span
                        className="flex items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100"
                        onClick={(e) => e.stopPropagation()}
                      >
                        {t.downloaded ? (
                          <button onClick={() => void removeTrack(t)} title="Remove download" className="text-oct-accent hover:text-oct-accent-bright">
                            <DownloadIcon size={14} />
                          </button>
                        ) : (
                          <button onClick={() => void dlTrack(t)} {...offlineAttrs(online, false, "Download")} className="text-oct-dim hover:text-oct-text disabled:opacity-30">
                            <DownloadIcon size={14} />
                          </button>
                        )}
                        {isManager && (
                          <>
                            <EditMetaButton online={online} onClick={() => setEditTracks([t])} />
                            <button onClick={() => void delTrack(t)} {...offlineAttrs(online, false, "Delete from server")} className="text-oct-dim hover:text-oct-danger disabled:opacity-30">
                              <TrashIcon size={14} />
                            </button>
                          </>
                        )}
                      </span>
                      <span className="w-9 text-right font-mono text-[11px] text-oct-subtle">
                        {formatDuration(t.duration_ms)}
                      </span>
                    </span>
                  </div>
                );
              })}
            </>
          )}
        </div>
      )}

      {editTracks && (
        <MetadataEditor
          tracks={editTracks}
          online={online}
          onClose={() => setEditTracks(null)}
          onSaved={() => void onMetaSaved()}
        />
      )}

      {editCover && (
        <ImageUploader
          kind="album"
          id={id}
          online={online}
          currentUrl={coverUrl(id, coverVersion || undefined)}
          onClose={() => setEditCover(false)}
          onUploaded={onCoverUploaded}
        />
      )}
    </section>
  );
}
