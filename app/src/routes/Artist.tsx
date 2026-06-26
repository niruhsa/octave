import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useNavigate } from "react-router-dom";
import {
  artistImageUrl,
  libraryDeleteArtist,
  libraryGetArtist,
  libraryListAlbumsByArtist,
  libraryMergeArtists,
} from "../ipc";
import { Cover } from "../components/Cover";
import { BlurUpImage } from "../components/BlurUpImage";
import { ImageUploader } from "../components/ImageUploader";
import { Aliases } from "../components/Aliases";
import { EntityPicker } from "../components/EntityPicker";
import { SavedBadge, SourceBadge, StreamBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";
import { gradientFor } from "../lib/visual";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { btnDanger, btnGhost } from "../lib/ui";
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
  const [merging, setMerging] = useState(false);

  const q = useQuery({
    queryKey: ["library", "albums-by-artist", id],
    queryFn: () => libraryListAlbumsByArtist(id),
    enabled: !!id,
  });
  // Single-entity fetch for the canonical name + preserved-spelling aliases.
  const artistQ = useQuery({
    queryKey: ["library", "artist", id],
    queryFn: () => libraryGetArtist(id),
    enabled: !!id,
  });
  const artist = artistQ.data;

  function refreshArtist() {
    void qc.invalidateQueries({ queryKey: ["library"] });
    broadcastInvalidate(["library"]);
  }

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
      <Link to="/library" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">
        ← LIBRARY
      </Link>

      {/* hero */}
      <header className="flex flex-col gap-5 sm:flex-row sm:items-end">
        {/* artist image hero — always attempted by id; hides on 404 */}
        <div className="relative shrink-0">
          <div
            className="relative h-[120px] w-[120px] overflow-hidden rounded-full border border-oct-border"
            style={{ background: gradientFor(id) }}
          >
            <BlurUpImage
              lowSrc={artistImageUrl(id, imgVersion || undefined, true)}
              fullSrc={artistImageUrl(id, imgVersion || undefined)}
              className="absolute inset-0 h-full w-full object-cover"
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
        <div className="flex min-w-0 flex-col">
          <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">ARTIST</span>
          <h1 className="mt-1.5 text-3xl font-semibold tracking-tight sm:text-[34px]">{artist?.name ?? "Artist"}</h1>
          <p className="mt-2 flex flex-wrap items-center gap-x-2 text-[13px] text-oct-subtle">
            <span className="font-mono">
              {items.length} album{items.length === 1 ? "" : "s"}
              {downloaded > 0 ? ` · ${downloaded} downloaded` : ""}
            </span>
            {q.data && <SourceBadge source={q.data.source} />}
          </p>
        </div>
      </header>

      {/* Preserved spellings (Korean + English, etc.) + manager controls.
          Hidden for a single spelling unless a manager can add more. */}
      {((artist?.aliases?.length ?? 0) > 1 || isManager) && (
        <Aliases
          kind="artist"
          entityId={id}
          aliases={artist?.aliases ?? []}
          online={online}
          isManager={isManager}
          onChanged={refreshArtist}
        />
      )}

      {/* actions (manager) */}
      {isManager && (
        <div className="flex flex-wrap items-center gap-3">
          <button
            onClick={() => setMerging(true)}
            className={btnGhost}
            {...offlineAttrs(online, false, "Merge a duplicate artist into this one")}
          >
            Merge artist…
          </button>
          <button onClick={delArtist} className={btnDanger} {...offlineAttrs(online)}>
            <TrashIcon size={14} /> Delete artist
          </button>
        </div>
      )}

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

      {merging && (
        <EntityPicker
          kind="artist"
          excludeId={id}
          title="Merge artist"
          hint={`Pick a duplicate artist to fold into "${artist?.name ?? "this artist"}". Its albums, tracks and followers move here, and every spelling is preserved.`}
          online={online}
          onPick={async (dupId) => {
            await libraryMergeArtists(id, dupId);
            refreshArtist();
          }}
          onClose={() => setMerging(false)}
        />
      )}
    </section>
  );
}
