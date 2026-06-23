import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { broadcastInvalidate } from "../App";
import {
  librarySearchAlbums,
  librarySearchArtists,
  librarySearchTracks,
  libraryDeleteArtist,
  libraryDeleteAlbum,
  libraryDeleteTrack,
} from "../ipc";
import type { MergedTrack } from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { Thumb } from "../components/Cover";
import { ArtistAvatar } from "../components/ArtistAvatar";
import { PlayIcon, SearchIcon, TrashIcon } from "../components/icons";
import { formatDuration } from "../lib/format";
import { qualityLabel } from "../lib/visual";
import { formatError } from "../lib/error";
import { usePlayerStore } from "../player/store";
import { useAppStore } from "../store";
import { btnPrimary, card, errorBox, input } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { SkeletonList } from "../components/Skeleton";

const PAGE_SIZE = 25;

export default function Search() {
  const [q, setQ] = useState("");
  const [submitted, setSubmitted] = useState("");
  const qc = useQueryClient();
  const playTrack = usePlayerStore((s) => s.playTrack);
  const queue = usePlayerStore((s) => s.queue);
  const currentIndex = usePlayerStore((s) => s.currentIndex);
  const currentId = currentIndex >= 0 ? queue[currentIndex]?.id : undefined;
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const isManager = tier === "admin" || tier === "manager";

  const artists = useQuery({
    queryKey: ["search", "artists", submitted],
    queryFn: () => librarySearchArtists(submitted, { limit: PAGE_SIZE }),
    enabled: submitted.length > 0,
  });
  const albums = useQuery({
    queryKey: ["search", "albums", submitted],
    queryFn: () => librarySearchAlbums(submitted, { limit: PAGE_SIZE }),
    enabled: submitted.length > 0,
  });
  const tracks = useQuery({
    queryKey: ["search", "tracks", submitted],
    queryFn: () => librarySearchTracks(submitted, { limit: PAGE_SIZE }),
    enabled: submitted.length > 0,
  });

  function invalidateSearch() {
    broadcastInvalidate(["search"]);
    broadcastInvalidate(["library"]);
    qc.invalidateQueries({ queryKey: ["search"] });
    qc.invalidateQueries({ queryKey: ["library"] });
  }
  async function delArtist(id: string, name: string) {
    if (!window.confirm(`Permanently delete artist "${name}" and all their albums/tracks?`)) return;
    try { await libraryDeleteArtist(id); invalidateSearch(); } catch (e) { alert(formatError(e)); }
  }
  async function delAlbum(id: string, t: string) {
    if (!window.confirm(`Permanently delete album "${t}" and all its tracks?`)) return;
    try { await libraryDeleteAlbum(id); invalidateSearch(); } catch (e) { alert(formatError(e)); }
  }
  async function delTrack(id: string, t: string) {
    if (!window.confirm(`Permanently delete track "${t}" from the server?`)) return;
    try { await libraryDeleteTrack(id); invalidateSearch(); } catch (e) { alert(formatError(e)); }
  }

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <h1 className="text-[27px] font-semibold tracking-tight">Search</h1>

      <form
        onSubmit={(e) => { e.preventDefault(); setSubmitted(q.trim()); }}
        className="flex gap-2"
      >
        <div className="relative flex-1">
          <span className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-oct-faint">
            <SearchIcon size={15} sw={1.4} />
          </span>
          <input
            autoFocus
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search artists, albums, tracks"
            className={`${input} pl-9`}
          />
        </div>
        <button type="submit" className={btnPrimary}>Search</button>
      </form>

      {submitted && (
        <div className="flex flex-col gap-7">
          <Section title="Artists" isLoading={artists.isLoading} error={artists.error} source={artists.data?.source}>
            {artists.data && artists.data.items.length === 0 ? <Empty /> : (
              <div className={`${card} divide-y divide-oct-border`}>
                {artists.data?.items.map((a) => (
                  <div key={a.id} className="group flex items-center gap-3 px-3 py-2 text-sm hover:bg-oct-elevated/50">
                    <ArtistAvatar id={a.id} imagePath={a.image_path} size={32} />
                    <Link to={`/artists/${a.id}`} className="flex-1 truncate group-hover:text-white">{a.name}</Link>
                    <DownloadedDot downloaded={a.downloaded} />
                    {isManager && (
                      <button onClick={() => void delArtist(a.id, a.name)} {...offlineAttrs(online, false, "Delete artist")} className="text-oct-dim opacity-0 hover:text-oct-danger group-hover:opacity-100 disabled:cursor-not-allowed">
                        <TrashIcon size={14} />
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </Section>

          <Section title="Albums" isLoading={albums.isLoading} error={albums.error} source={albums.data?.source}>
            {albums.data && albums.data.items.length === 0 ? <Empty /> : (
              <div className={`${card} divide-y divide-oct-border`}>
                {albums.data?.items.map((a) => (
                  <div key={a.id} className="group flex items-center gap-3 px-3 py-2 text-sm hover:bg-oct-elevated/50">
                    <Thumb album={a} size={34} tryCover />
                    <Link to={`/albums/${a.id}`} className="flex-1 truncate group-hover:text-white">{a.title}</Link>
                    {a.release_year && <span className="font-mono text-[10.5px] text-oct-faint">{a.release_year}</span>}
                    <DownloadedDot downloaded={a.downloaded} />
                    {isManager && (
                      <button onClick={() => void delAlbum(a.id, a.title)} {...offlineAttrs(online, false, "Delete album")} className="text-oct-dim opacity-0 hover:text-oct-danger group-hover:opacity-100 disabled:cursor-not-allowed">
                        <TrashIcon size={14} />
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </Section>

          <Section title="Tracks" isLoading={tracks.isLoading} error={tracks.error} source={tracks.data?.source}>
            {tracks.data && tracks.data.items.length === 0 ? <Empty /> : (
              <div className={`${card} divide-y divide-oct-border`}>
                {tracks.data?.items.map((t) => {
                  const active = t.id === currentId;
                  return (
                    <div
                      key={t.id}
                      onClick={() => playTrack(t as MergedTrack, tracks.data!.items)}
                      className={`group flex cursor-pointer items-center gap-3 px-3 py-2 text-sm ${active ? "bg-oct-elevated" : "hover:bg-oct-elevated/50"}`}
                    >
                      <span className="grid h-7 w-7 shrink-0 place-items-center text-oct-dim group-hover:text-oct-accent">
                        <PlayIcon size={12} />
                      </span>
                      <span className={`flex-1 truncate ${active ? "text-oct-accent" : ""}`}>{t.title}</span>
                      <DownloadedDot downloaded={t.downloaded} />
                      <span className="hidden font-mono text-[10.5px] text-oct-subtle sm:block">{qualityLabel(t)}</span>
                      <span className="w-10 text-right font-mono text-[11px] text-oct-subtle">{formatDuration(t.duration_ms)}</span>
                      {isManager && (
                        <button onClick={(e) => { e.stopPropagation(); void delTrack(t.id, t.title); }} {...offlineAttrs(online, false, "Delete track")} className="text-oct-dim opacity-0 hover:text-oct-danger group-hover:opacity-100 disabled:cursor-not-allowed">
                          <TrashIcon size={14} />
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </Section>
        </div>
      )}
    </section>
  );
}

function Section({
  title,
  isLoading,
  error,
  source,
  children,
}: {
  title: string;
  isLoading: boolean;
  error: unknown;
  source?: "server" | "cache";
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2.5">
      <h2 className="flex items-center gap-2 font-mono text-[11px] tracking-[0.14em] text-oct-faint">
        {title.toUpperCase()}
        {source && <SourceBadge source={source} />}
      </h2>
      {isLoading ? (
        <SkeletonList rows={4} />
      ) : error ? (
        <p className={errorBox}>{formatError(error)}</p>
      ) : (
        children
      )}
    </div>
  );
}

function Empty() {
  return <p className="text-sm text-oct-subtle">No matches.</p>;
}
