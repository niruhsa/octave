// Favorites (Phase 11). Tabs for liked tracks / albums / artists. The Tracks
// tab is the "Liked Songs" view — Play-all queues them. Server-authoritative +
// online-only, like the notifications feed.

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  favoritesListAlbums,
  favoritesListArtists,
  favoritesListTracks,
} from "../ipc";
import { OfflineGate } from "../components/OfflineGate";
import { FavoriteButton } from "../components/FavoriteButton";
import { Cover } from "../components/Cover";
import { ArtistAvatar } from "../components/ArtistAvatar";
import { Skeleton } from "../components/Skeleton";
import { formatError } from "../lib/error";
import { btnPrimary } from "../lib/ui";
import { HeartIcon, PlayIcon } from "../components/icons";
import { serverTrackToQueueItem, usePlayerStore } from "../player/store";

type Tab = "tracks" | "albums" | "artists";

export default function Favorites() {
  return (
    <OfflineGate feature="Favorites">
      <FavoritesInner />
    </OfflineGate>
  );
}

function FavoritesInner() {
  const [tab, setTab] = useState<Tab>("tracks");

  return (
    <section className="mx-auto flex max-w-3xl flex-col gap-5 p-6 md:p-8">
      <header className="flex min-w-0 flex-col">
        <span className="font-mono text-[11px] tracking-[0.16em] text-oct-accent">FAVORITES</span>
        <h1 className="mt-1.5 flex items-center gap-2.5 text-3xl font-semibold tracking-tight">
          <HeartIcon size={22} className="text-oct-accent" />
          Favorites
        </h1>
      </header>

      <div className="flex gap-2">
        {(["tracks", "albums", "artists"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={`rounded-full border px-3.5 py-1 text-[13px] capitalize transition-colors ${
              t === tab
                ? "border-oct-accent bg-oct-accent/15 text-oct-text"
                : "border-oct-border text-oct-subtle hover:bg-oct-elevated/50"
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {tab === "tracks" && <TracksTab />}
      {tab === "albums" && <AlbumsTab />}
      {tab === "artists" && <ArtistsTab />}
    </section>
  );
}

function EmptyState({ label }: { label: string }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-2xl border border-oct-border bg-oct-panel/40 px-6 py-14 text-center">
      <span className="grid h-12 w-12 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
        <HeartIcon size={22} />
      </span>
      <p className="text-sm text-oct-subtle">No favorite {label} yet.</p>
      <p className="max-w-xs text-[12.5px] leading-relaxed text-oct-faint">
        Tap the heart on a {label.replace(/s$/, "")} to add it here.
      </p>
    </div>
  );
}

function ListSkeleton() {
  return (
    <div className="flex flex-col gap-1.5">
      {Array.from({ length: 6 }).map((_, i) => (
        <Skeleton key={i} className="h-14 w-full rounded-xl" />
      ))}
    </div>
  );
}

function TracksTab() {
  const playQueue = usePlayerStore((s) => s.playQueue);
  const q = useQuery({
    queryKey: ["favorites", "tracks"],
    queryFn: favoritesListTracks,
  });
  const tracks = q.data ?? [];

  if (q.isLoading) return <ListSkeleton />;
  if (q.isError)
    return <p className="text-sm text-oct-danger">{formatError(q.error)}</p>;
  if (tracks.length === 0) return <EmptyState label="tracks" />;

  const queue = tracks.map(serverTrackToQueueItem);

  return (
    <div className="flex flex-col gap-3">
      <div>
        <button onClick={() => playQueue(queue, 0)} className={btnPrimary}>
          <PlayIcon size={14} /> Play all
        </button>
      </div>
      <ul className="flex flex-col gap-1">
        {tracks.map((t, i) => (
          <li
            key={t.id}
            className="group flex items-center gap-3 rounded-xl border border-oct-border px-3 py-2"
          >
            <button
              onClick={() => playQueue(queue, i)}
              className="grid h-8 w-8 shrink-0 place-items-center rounded-md text-oct-subtle hover:bg-oct-elevated/60 hover:text-oct-text"
              title="Play"
            >
              <PlayIcon size={14} />
            </button>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[14px] font-medium">{t.title}</span>
            </span>
            <FavoriteButton kind="track" id={t.id} />
          </li>
        ))}
      </ul>
    </div>
  );
}

function AlbumsTab() {
  const q = useQuery({
    queryKey: ["favorites", "albums"],
    queryFn: favoritesListAlbums,
  });
  const albums = q.data ?? [];

  if (q.isLoading) return <ListSkeleton />;
  if (q.isError)
    return <p className="text-sm text-oct-danger">{formatError(q.error)}</p>;
  if (albums.length === 0) return <EmptyState label="albums" />;

  return (
    <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
      {albums.map((a) => (
        <div key={a.id} className="flex flex-col gap-2">
          <Link to={`/albums/${a.id}`} className="block">
            <Cover album={a} tryCover className="w-full" />
          </Link>
          <div className="flex items-start justify-between gap-1.5">
            <Link to={`/albums/${a.id}`} className="min-w-0 flex-1">
              <span className="block truncate text-[13.5px] font-medium">{a.title}</span>
            </Link>
            <FavoriteButton kind="album" id={a.id} size={15} />
          </div>
        </div>
      ))}
    </div>
  );
}

function ArtistsTab() {
  const q = useQuery({
    queryKey: ["favorites", "artists"],
    queryFn: favoritesListArtists,
  });
  const artists = q.data ?? [];

  if (q.isLoading) return <ListSkeleton />;
  if (q.isError)
    return <p className="text-sm text-oct-danger">{formatError(q.error)}</p>;
  if (artists.length === 0) return <EmptyState label="artists" />;

  return (
    <ul className="flex flex-col gap-1">
      {artists.map((a) => (
        <li
          key={a.id}
          className="flex items-center gap-3 rounded-xl border border-oct-border px-3 py-2"
        >
          <Link to={`/artists/${a.id}`} className="flex min-w-0 flex-1 items-center gap-3">
            <ArtistAvatar id={a.id} imagePath={a.image_path} size={40} />
            <span className="min-w-0">
              <span className="block truncate text-[14px] font-medium">{a.name}</span>
            </span>
          </Link>
          <FavoriteButton kind="artist" id={a.id} size={15} />
        </li>
      ))}
    </ul>
  );
}
