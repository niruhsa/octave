import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import {
  cacheGetAlbum,
  cacheListArtists,
  cacheListDownloadedTracks,
  downloadDelete,
  downloadsDir,
  downloadsWifiOnly,
  downloadsSetWifiOnly,
  type Album,
} from "../ipc";
import { formatBytes, useDownloadsStore } from "../downloads/useDownloads";
import { formatError } from "../lib/error";
import { formatDuration } from "../lib/format";
import { broadcastInvalidate } from "../App";
import { card, errorBox } from "../lib/ui";
import { Thumb } from "../components/Cover";
import { SkeletonList } from "../components/Skeleton";
import { ArtistIcon, DownloadIcon, FolderIcon, TrashIcon } from "../components/icons";

/**
 * Offline-downloads management (Phase 6): storage usage, downloads root +
 * Wi-Fi-only toggle, active-download progress, and a Songs / Albums / Artists
 * filter. Albums and Artists group the downloaded tracks and roll their
 * on-disk sizes up per group (e.g. two songs at 47 + 53 MB → a 100 MB album).
 */
type Filter = "songs" | "albums" | "artists";

const FILTERS: { key: Filter; label: string }[] = [
  { key: "songs", label: "Songs" },
  { key: "albums", label: "Albums" },
  { key: "artists", label: "Artists" },
];

export default function Downloads() {
  const qc = useQueryClient();
  const storage = useDownloadsStore((s) => s.storage);
  const active = useDownloadsStore((s) => s.active);
  const refreshStorage = useDownloadsStore((s) => s.refreshStorage);
  const clear = useDownloadsStore((s) => s.clear);

  const [filter, setFilter] = useState<Filter>("songs");

  const tracks = useQuery({ queryKey: ["cache", "downloaded_tracks"], queryFn: cacheListDownloadedTracks });
  const dir = useQuery({ queryKey: ["downloads", "dir"], queryFn: downloadsDir });
  const wifiOnly = useQuery({ queryKey: ["downloads", "wifi_only"], queryFn: downloadsWifiOnly });

  const allTracks = useMemo(() => tracks.data ?? [], [tracks.data]);

  // Resolve album / artist names from the offline cache (downloaded items'
  // album+artist rows are mirrored locally, so this works offline too).
  const albumIds = useMemo(
    () => [...new Set(allTracks.map((t) => t.album_id))],
    [allTracks],
  );
  const albumsMeta = useQuery({
    queryKey: ["cache", "albums-meta", albumIds],
    queryFn: async () => {
      const rows = await Promise.all(albumIds.map((id) => cacheGetAlbum(id)));
      return rows.filter((r): r is Album => !!r);
    },
    enabled: albumIds.length > 0,
  });
  const artistsAll = useQuery({ queryKey: ["cache", "artists"], queryFn: cacheListArtists });

  const albumMeta = useMemo(() => {
    const m = new Map<string, { title: string; artistId: string }>();
    for (const a of albumsMeta.data ?? []) m.set(a.id, { title: a.title, artistId: a.artist_id });
    return m;
  }, [albumsMeta.data]);
  const artistName = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of artistsAll.data ?? []) m.set(a.id, a.name);
    return m;
  }, [artistsAll.data]);

  // Per-album rollup: summed bytes + track count + the album's track ids.
  const albumGroups = useMemo(() => {
    const m = new Map<string, { bytes: number; trackIds: string[]; artistId: string }>();
    for (const t of allTracks) {
      const g = m.get(t.album_id) ?? { bytes: 0, trackIds: [], artistId: t.artist_id };
      g.bytes += t.file_size ?? 0;
      g.trackIds.push(t.id);
      m.set(t.album_id, g);
    }
    return [...m.entries()]
      .map(([id, g]) => ({ id, ...g }))
      .sort((a, b) => b.bytes - a.bytes);
  }, [allTracks]);

  // Per-artist rollup: summed bytes + distinct albums + the artist's track ids.
  const artistGroups = useMemo(() => {
    const m = new Map<string, { bytes: number; trackIds: string[]; albums: Set<string> }>();
    for (const t of allTracks) {
      const g = m.get(t.artist_id) ?? { bytes: 0, trackIds: [], albums: new Set<string>() };
      g.bytes += t.file_size ?? 0;
      g.trackIds.push(t.id);
      g.albums.add(t.album_id);
      m.set(t.artist_id, g);
    }
    return [...m.entries()]
      .map(([id, g]) => ({ id, bytes: g.bytes, trackIds: g.trackIds, albums: g.albums.size }))
      .sort((a, b) => b.bytes - a.bytes);
  }, [allTracks]);

  const trackCount = allTracks.length;
  useEffect(() => { void refreshStorage(); }, [trackCount, refreshStorage]);

  const activeList = Object.values(active);

  async function invalidateAfterDelete() {
    broadcastInvalidate(["library"]);
    await Promise.all([
      qc.invalidateQueries({ queryKey: ["cache", "downloaded_tracks"] }),
      qc.invalidateQueries({ queryKey: ["library"] }),
      refreshStorage(),
    ]);
  }

  async function remove(id: string) {
    try {
      await downloadDelete(id);
      await invalidateAfterDelete();
    } catch (e) {
      alert(formatError(e));
    }
  }

  async function removeGroup(trackIds: string[], label: string) {
    if (!window.confirm(`Remove ${trackIds.length} downloaded track${trackIds.length === 1 ? "" : "s"} from ${label}?`)) return;
    try {
      for (const id of trackIds) await downloadDelete(id);
      await invalidateAfterDelete();
    } catch (e) {
      alert(formatError(e));
    }
  }

  return (
    <section className="flex flex-col gap-6 p-6 md:p-8">
      <header>
        <h1 className="text-[27px] font-semibold tracking-tight">Downloads</h1>
        <p className="mt-1 font-mono text-[11.5px] text-oct-subtle">Offline content — playable without the server</p>
      </header>

      {/* storage panel */}
      <div className={`${card} grid gap-5 p-5 sm:grid-cols-3`}>
        <div>
          <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">STORAGE USED</div>
          <div className="mt-1.5 text-2xl font-semibold text-oct-accent">
            {storage ? formatBytes(storage.bytes) : "…"}
          </div>
          <div className="mt-0.5 font-mono text-[11px] text-oct-subtle">
            {storage ? `${storage.track_count} tracks · ${storage.cover_count} covers` : ""}
          </div>
        </div>
        <div className="min-w-0">
          <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">LOCATION</div>
          <div className="mt-1.5 flex items-center gap-2 text-oct-muted">
            <FolderIcon size={15} className="shrink-0 text-oct-subtle" />
            <span className="truncate font-mono text-[11px]">{dir.data ?? "…"}</span>
          </div>
        </div>
        <div>
          <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">WI-FI ONLY</div>
          <button
            onClick={async () => {
              await downloadsSetWifiOnly(!wifiOnly.data);
              await qc.invalidateQueries({ queryKey: ["downloads", "wifi_only"] });
            }}
            className={`mt-1.5 inline-flex items-center gap-2 rounded-full border px-3 py-1 text-xs transition-colors ${
              wifiOnly.data
                ? "border-oct-accent/50 bg-oct-accent/15 text-oct-accent-bright"
                : "border-oct-border-strong text-oct-subtle hover:text-oct-muted"
            }`}
          >
            <span className={`h-1.5 w-1.5 rounded-full ${wifiOnly.data ? "bg-oct-accent" : "bg-oct-faint"}`} />
            {wifiOnly.data ? "On" : "Off"}
          </button>
        </div>
      </div>

      {activeList.length > 0 && (
        <div className={`${card} p-4`}>
          <h2 className="mb-3 font-mono text-[11px] tracking-[0.14em] text-oct-faint">IN PROGRESS</h2>
          <ul className="flex flex-col gap-3">
            {activeList.map((d) => (
              <li key={d.id} className="flex flex-col gap-1.5 text-xs">
                <div className="flex items-center justify-between">
                  <span className="truncate font-mono text-oct-muted">{d.id}</span>
                  <span className="font-mono text-oct-subtle">
                    {d.done
                      ? "done"
                      : d.error
                        ? `error: ${d.error}`
                        : d.total
                          ? `${formatBytes(d.received)} / ${formatBytes(d.total)}`
                          : formatBytes(d.received)}
                    {d.totalTracks ? ` · ${d.index ?? 0}/${d.totalTracks}` : ""}
                  </span>
                </div>
                <div className="h-1.5 w-full overflow-hidden rounded-full bg-oct-line">
                  <div
                    className={`h-full rounded-full ${d.error ? "bg-oct-danger" : d.done ? "bg-oct-online" : "bg-oct-accent"}`}
                    style={{ width: d.total ? `${Math.min(100, (d.received / d.total) * 100)}%` : d.done ? "100%" : "30%" }}
                  />
                </div>
                {d.done && (
                  <button onClick={() => { clear(d.id); void refreshStorage(); }} className="self-end font-mono text-[11px] text-oct-accent hover:underline">
                    dismiss
                  </button>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* filter pills */}
      <div className="flex items-center gap-2">
        {FILTERS.map((f) => {
          const activeF = filter === f.key;
          const n = f.key === "songs" ? allTracks.length : f.key === "albums" ? albumGroups.length : artistGroups.length;
          return (
            <button
              key={f.key}
              onClick={() => setFilter(f.key)}
              className={`rounded-full px-3.5 py-1.5 text-[12.5px] transition-colors ${
                activeF
                  ? "bg-oct-accent font-medium text-oct-bg"
                  : "border border-oct-border-strong text-oct-muted hover:text-oct-text"
              }`}
            >
              {f.label}
              <span className={`ml-1.5 font-mono text-[10.5px] ${activeF ? "text-oct-bg/70" : "text-oct-faint"}`}>{n}</span>
            </button>
          );
        })}
      </div>

      {tracks.isLoading && <SkeletonList rows={8} avatar="none" />}
      {tracks.isError && <p className={errorBox}>{formatError(tracks.error)}</p>}

      {tracks.data && allTracks.length === 0 && (
        <div className={`${card} flex flex-col items-center gap-2 p-8 text-center text-oct-subtle`}>
          <DownloadIcon size={22} className="text-oct-faint" />
          <p className="text-sm">Nothing downloaded yet. Browse the library and hit Download.</p>
        </div>
      )}

      {/* ── Songs ── */}
      {tracks.data && allTracks.length > 0 && filter === "songs" && (
        <div className={`${card} divide-y divide-oct-border`}>
          {allTracks.map((t) => (
            <div key={t.id} className="flex items-center gap-3 px-3 py-2.5 text-sm first:rounded-t-xl last:rounded-b-xl">
              <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-oct-accent" />
              <span className="flex-1 truncate">{t.title}</span>
              <span className="hidden font-mono text-[10.5px] text-oct-subtle sm:block">{t.codec?.toUpperCase()}</span>
              <span className="w-10 text-right font-mono text-[11px] text-oct-subtle">{formatDuration(t.duration_ms)}</span>
              <span className="w-16 text-right font-mono text-[11px] text-oct-accent">{t.file_size ? formatBytes(t.file_size) : "—"}</span>
              <button onClick={() => void remove(t.id)} title="Remove download" className="text-oct-dim hover:text-oct-danger">
                <TrashIcon size={15} />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* ── Albums ── */}
      {tracks.data && allTracks.length > 0 && filter === "albums" && (
        <div className={`${card} divide-y divide-oct-border`}>
          {albumGroups.map((g) => {
            const meta = albumMeta.get(g.id);
            const title = meta?.title ?? "Album";
            const artist = artistName.get(meta?.artistId ?? g.artistId) ?? "Unknown artist";
            return (
              <div key={g.id} className="group flex items-center gap-3 px-3 py-2.5 first:rounded-t-xl last:rounded-b-xl hover:bg-oct-elevated/40">
                <Link to={`/albums/${g.id}`} className="flex min-w-0 flex-1 items-center gap-3">
                  <Thumb album={{ id: g.id }} size={40} tryCover />
                  <span className="min-w-0">
                    <span className="block truncate text-[13.5px] group-hover:text-white">{title}</span>
                    <span className="block truncate font-mono text-[10.5px] text-oct-subtle">
                      {artist} · {g.trackIds.length} track{g.trackIds.length === 1 ? "" : "s"}
                    </span>
                  </span>
                </Link>
                <span className="w-16 text-right font-mono text-[12px] text-oct-accent">{formatBytes(g.bytes)}</span>
                <button onClick={() => void removeGroup(g.trackIds, title)} title="Remove all downloads in this album" className="text-oct-dim hover:text-oct-danger">
                  <TrashIcon size={15} />
                </button>
              </div>
            );
          })}
        </div>
      )}

      {/* ── Artists ── */}
      {tracks.data && allTracks.length > 0 && filter === "artists" && (
        <div className={`${card} divide-y divide-oct-border`}>
          {artistGroups.map((g) => {
            const name = artistName.get(g.id) ?? "Unknown artist";
            return (
              <div key={g.id} className="group flex items-center gap-3 px-3 py-2.5 first:rounded-t-xl last:rounded-b-xl hover:bg-oct-elevated/40">
                <Link to={`/artists/${g.id}`} className="flex min-w-0 flex-1 items-center gap-3">
                  <span className="grid h-10 w-10 shrink-0 place-items-center rounded-full bg-oct-elevated text-oct-subtle">
                    <ArtistIcon size={16} />
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate text-[13.5px] group-hover:text-white">{name}</span>
                    <span className="block truncate font-mono text-[10.5px] text-oct-subtle">
                      {g.albums} album{g.albums === 1 ? "" : "s"} · {g.trackIds.length} track{g.trackIds.length === 1 ? "" : "s"}
                    </span>
                  </span>
                </Link>
                <span className="w-16 text-right font-mono text-[12px] text-oct-accent">{formatBytes(g.bytes)}</span>
                <button onClick={() => void removeGroup(g.trackIds, name)} title="Remove all downloads by this artist" className="text-oct-dim hover:text-oct-danger">
                  <TrashIcon size={15} />
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
