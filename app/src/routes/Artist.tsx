import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useNavigate } from "react-router-dom";
import { artistImageUrl, libraryDeleteArtist, libraryListAlbumsByArtist } from "../ipc";
import { Cover } from "../components/Cover";
import { ImageUploader } from "../components/ImageUploader";
import { SavedBadge, SourceBadge, StreamBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";
import { gradientFor } from "../lib/visual";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { btnDangerSm } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { EditIcon, TrashIcon } from "../components/icons";
import { SkeletonGrid } from "../components/Skeleton";

export default function Artist() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const isManager = tier === "admin" || tier === "manager";
  const [editImage, setEditImage] = useState(false);
  const [imgVersion, setImgVersion] = useState(0);

  const q = useQuery({
    queryKey: ["library", "albums-by-artist", id],
    queryFn: () => libraryListAlbumsByArtist(id),
    enabled: !!id,
  });

  async function delArtist() {
    if (!window.confirm("Permanently delete this artist and all their albums/tracks from the server?")) return;
    try {
      await libraryDeleteArtist(id);
      await qc.invalidateQueries({ queryKey: ["library"] });
      broadcastInvalidate(["library"]);
      navigate("/library");
    } catch (e) {
      alert(formatError(e));
    }
  }

  const items = q.data?.items ?? [];
  const downloaded = items.filter((a) => a.downloaded).length;

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <header className="flex items-end gap-4">
        {/* artist image hero — always attempted by id; hides on 404 */}
        <div className="relative shrink-0">
          <div
            className="h-[88px] w-[88px] overflow-hidden rounded-full border border-oct-border"
            style={{ background: gradientFor(id) }}
          >
            <img
              src={artistImageUrl(id, imgVersion || undefined)}
              alt=""
              className="h-full w-full object-cover"
              loading="lazy"
              onError={(e) => ((e.currentTarget as HTMLImageElement).style.display = "none")}
            />
          </div>
          {isManager && (
            <button
              onClick={() => setEditImage(true)}
              {...offlineAttrs(online, false, "Edit artist image")}
              className="absolute bottom-0 right-0 grid h-7 w-7 place-items-center rounded-full bg-black/60 text-white/90 backdrop-blur-sm transition-colors hover:bg-black/80 disabled:opacity-40"
            >
              <EditIcon size={13} />
            </button>
          )}
        </div>
        <div className="min-w-0 flex-1">
          <Link to="/library" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">
            ← LIBRARY
          </Link>
          <h1 className="mt-2 text-[27px] font-semibold tracking-tight">Albums</h1>
          <p className="mt-1 font-mono text-[11.5px] text-oct-subtle">
            {items.length} album{items.length === 1 ? "" : "s"}
            {downloaded > 0 ? ` · ${downloaded} downloaded` : ""}
          </p>
        </div>
        <div className="flex items-center gap-3">
          {q.data && <SourceBadge source={q.data.source} />}
          {isManager && (
            <button onClick={delArtist} className={btnDangerSm} {...offlineAttrs(online)}>
              <TrashIcon size={13} /> Delete artist
            </button>
          )}
        </div>
      </header>

      {q.isLoading && <SkeletonGrid count={12} />}
      {q.isError && <p className="rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-sm text-oct-danger">{formatError(q.error)}</p>}

      {q.data && (
        <div
          className="grid gap-x-[22px] gap-y-7"
          style={{ gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))" }}
        >
          {items.length === 0 ? (
            <p className="text-sm text-oct-subtle">No albums.</p>
          ) : (
            items.map((a) => (
              <Link key={a.id} to={`/albums/${a.id}`} className="group cursor-pointer">
                <Cover
                  album={a}
                  size={9999}
                  badge={a.downloaded ? <SavedBadge /> : <StreamBadge />}
                />
                <div className="mt-2.5 truncate text-sm font-medium group-hover:text-white">
                  {a.title}
                </div>
                {a.release_year && (
                  <div className="mt-0.5 font-mono text-[11px] text-oct-subtle">{a.release_year}</div>
                )}
              </Link>
            ))
          )}
        </div>
      )}

      {editImage && (
        <ImageUploader
          kind="artist"
          id={id}
          online={online}
          currentUrl={artistImageUrl(id, imgVersion || undefined)}
          onClose={() => setEditImage(false)}
          onUploaded={() => {
            setImgVersion(Date.now());
            broadcastInvalidate(["library"]);
          }}
        />
      )}
    </section>
  );
}
