import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  librarySearchAlbums,
  librarySearchArtists,
  librarySearchTracks,
} from "../ipc";
import type { MergedTrack } from "../ipc";
import { DownloadedDot, SourceBadge } from "../components/SourceBadge";
import { formatDuration } from "../lib/format";
import { formatError } from "../lib/error";
import { usePlayerStore } from "../player/store";

const PAGE_SIZE = 25;

/**
 * Unified search across artists, albums, tracks. Each section fires its
 * own query so a slow track index doesn't gate the others.
 */
export default function Search() {
  const [q, setQ] = useState("");
  const [submitted, setSubmitted] = useState("");
  const playTrack = usePlayerStore((s) => s.playTrack);

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

  return (
    <section className="flex flex-col gap-4">
      <header className="flex items-baseline justify-between">
        <h1 className="text-2xl font-semibold">Search</h1>
        <Link to="/library" className="text-sm text-blue-400 hover:underline">
          Library
        </Link>
      </header>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          setSubmitted(q.trim());
        }}
        className="flex gap-2"
      >
        <input
          autoFocus
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search artists, albums, tracks"
          className="flex-1 rounded border border-neutral-700 bg-neutral-900 px-3 py-1.5 text-sm"
        />
        <button
          type="submit"
          className="rounded bg-blue-600 px-3 py-1.5 text-sm text-white"
        >
          Search
        </button>
      </form>

      {submitted && (
        <div className="flex flex-col gap-6">
          {/* Artists ----------------------------------------------------- */}
          <Section
            title="Artists"
            error={artists.error}
            isLoading={artists.isLoading}
            source={artists.data?.source}
          >
            {artists.data && artists.data.items.length === 0 ? (
              <Empty />
            ) : (
              <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
                {artists.data?.items.map((a) => (
                  <li key={a.id} className="flex items-center gap-3 p-2 text-sm">
                    <DownloadedDot downloaded={a.downloaded} />
                    <Link
                      to={`/artists/${a.id}`}
                      className="flex-1 hover:underline"
                    >
                      {a.name}
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </Section>

          {/* Albums ------------------------------------------------------ */}
          <Section
            title="Albums"
            error={albums.error}
            isLoading={albums.isLoading}
            source={albums.data?.source}
          >
            {albums.data && albums.data.items.length === 0 ? (
              <Empty />
            ) : (
              <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
                {albums.data?.items.map((a) => (
                  <li key={a.id} className="flex items-center gap-3 p-2 text-sm">
                    <DownloadedDot downloaded={a.downloaded} />
                    <Link
                      to={`/albums/${a.id}`}
                      className="flex-1 hover:underline"
                    >
                      {a.title}
                    </Link>
                    {a.release_year && (
                      <span className="text-xs text-neutral-500">
                        {a.release_year}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </Section>

          {/* Tracks ------------------------------------------------------ */}
          <Section
            title="Tracks"
            error={tracks.error}
            isLoading={tracks.isLoading}
            source={tracks.data?.source}
          >
            {tracks.data && tracks.data.items.length === 0 ? (
              <Empty />
            ) : (
              <ul className="divide-y divide-neutral-800 rounded border border-neutral-800">
                {tracks.data?.items.map((t) => (
                  <li
                    key={t.id}
                    className="flex cursor-pointer items-center gap-3 p-2 text-sm hover:bg-neutral-800/50"
                    onClick={() => playTrack(t as MergedTrack, tracks.data!.items)}
                  >
                    <DownloadedDot downloaded={t.downloaded} />
                    <span className="flex-1 hover:underline">
                      {t.title}
                    </span>
                    <Link
                      to={`/albums/${t.album_id}`}
                      className="text-xs text-neutral-500 hover:underline"
                      onClick={(e) => e.stopPropagation()}
                    >
                      album
                    </Link>
                    <span className="w-12 text-right tabular-nums text-neutral-500">
                      {formatDuration(t.duration_ms)}
                    </span>
                  </li>
                ))}
              </ul>
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
    <div className="flex flex-col gap-2">
      <h2 className="flex items-center gap-2 text-sm font-semibold uppercase tracking-wide text-neutral-400">
        {title}
        {source && <SourceBadge source={source} />}
      </h2>
      {isLoading && <p className="text-sm text-neutral-500">Loading…</p>}
      {error ? (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {formatError(error)}
        </p>
      ) : (
        children
      )}
    </div>
  );
}

function Empty() {
  return <p className="text-sm text-neutral-500">No matches.</p>;
}
