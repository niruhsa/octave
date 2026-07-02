import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams, useNavigate } from "react-router-dom";
import {
  artistImageUrl,
  discoverRadio,
  followArtist,
  isFollowing,
  libraryDeleteArtist,
  libraryGetArtist,
  libraryListAlbumsByArtist,
  libraryListArtistLibraryPaths,
  libraryListTracksByAlbum,
  libraryMergeArtists,
  unfollowArtist,
} from "../ipc";
import type { AlbumType, MergedAlbum } from "../ipc";
import { serverTrackToQueueItem, usePlayerStore } from "../player/store";
import { byteSize } from "../lib/format";
import { Cover } from "../components/Cover";
import { BlurUpImage } from "../components/BlurUpImage";
import { ImageUploader } from "../components/ImageUploader";
import { Aliases } from "../components/Aliases";
import { LibraryLocation } from "../components/LibraryLocation";
import { DiscographyPanel } from "../components/DiscographyPanel";
import { EntityPicker } from "../components/EntityPicker";
import { SavedBadge, SourceBadge, StreamBadge } from "../components/SourceBadge";
import { formatError } from "../lib/error";
import { gradientFor } from "../lib/visual";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { btnDanger, btnGhost, btnPrimary } from "../lib/ui";
import { OFFLINE_MSG, offlineAttrs } from "../components/OfflineGate";
import {
  BellIcon,
  ChevronDownIcon,
  EditIcon,
  FolderIcon,
  PlayIcon,
  ShuffleIcon,
  TrashIcon,
} from "../components/icons";
import { FavoriteButton } from "../components/FavoriteButton";
import { SkeletonGrid } from "../components/Skeleton";

// Discography is split into per-type sections (Albums → EPs → Singles → Live) —
// the same grouping the server uses for `album_type`. Order here is the display
// order; the label map drives both the section heading and each card's caption.
const TYPE_LABEL: Record<AlbumType, string> = {
  album: "Album",
  ep: "EP",
  single: "Single",
  live: "Live",
};
const SECTION_DEFS: { key: AlbumType; title: string }[] = [
  { key: "album", title: "Albums" },
  { key: "ep", title: "EPs" },
  { key: "single", title: "Singles" },
  { key: "live", title: "Live Albums" },
];
// Discography filter chips — "All" plus one per type. `all` shows every
// section; a specific type collapses the view to that single section.
const FILTERS: { key: "all" | AlbumType; label: string }[] = [
  { key: "all", label: "All" },
  { key: "album", label: "Albums" },
  { key: "ep", label: "EPs" },
  { key: "single", label: "Singles" },
  { key: "live", label: "Live" },
];

export default function Artist() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const tier = useAppStore((s) => s.tier);
  const online = useAppStore((s) => s.online);
  const session = useAppStore((s) => s.session);
  const isManager = tier === "admin" || tier === "manager";
  // Only a logged-in user (bearer) can follow; a SECRET_KEY session has no
  // user to own the follow (the server rejects it).
  const canFollow = session?.kind === "bearer";
  const [editImage, setEditImage] = useState(false);
  const [imgVersion, setImgVersion] = useState(0);
  const [merging, setMerging] = useState(false);
  const [followBusy, setFollowBusy] = useState(false);
  // Discography controls: active type filter + release-order sort.
  const [filter, setFilter] = useState<"all" | AlbumType>("all");
  const [sort, setSort] = useState<"newest" | "oldest">("newest");
  // "Storage & artist tools" panel — collapsed by default (managers only).
  const [adminOpen, setAdminOpen] = useState(false);

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

  // Storage-folder count, only to surface an at-a-glance "split across N
  // folders" chip on the collapsed tools panel. Shares its query key with
  // <LibraryLocation> so React Query serves both from one fetch.
  const pathsQ = useQuery({
    queryKey: ["library", "artist-paths", id],
    queryFn: () => libraryListArtistLibraryPaths(id),
    enabled: !!id && online && isManager,
  });
  const folderCount = pathsQ.data?.paths?.length ?? 0;

  // Follow state (online + bearer only). When the query is disabled/loading we
  // optimistically treat it as not-following; the button is offline-disabled.
  const followQ = useQuery({
    queryKey: ["follow", id],
    queryFn: () => isFollowing(id),
    enabled: !!id && canFollow && online,
  });
  const following = followQ.data ?? false;

  async function toggleFollow() {
    if (followBusy) return;
    setFollowBusy(true);
    try {
      if (following) await unfollowArtist(id);
      else await followArtist(id);
      await qc.invalidateQueries({ queryKey: ["follow", id] });
    } catch (e) {
      alert(formatError(e));
    } finally {
      setFollowBusy(false);
    }
  }

  // Seed the artist radio into the queue. `shuffle` flips the player's shuffle
  // mode first (mobile has a dedicated Shuffle button; desktop's Radio plays
  // in order).
  async function startRadio(shuffle = false) {
    try {
      const tracks = await discoverRadio(id, undefined);
      if (tracks.length === 0) return;
      const st = usePlayerStore.getState();
      if (st.shuffle !== shuffle) st.toggleShuffle();
      st.playQueue(tracks.map(serverTrackToQueueItem), 0);
    } catch (e) {
      alert(formatError(e));
    }
  }

  // Play a whole release straight from its card — fetch the album's tracks and
  // hand them to the queue (works offline for downloaded albums via the cache).
  async function playAlbum(album: MergedAlbum) {
    try {
      const view = await libraryListTracksByAlbum(album.id);
      if (view.items.length > 0) usePlayerStore.getState().playQueue(view.items, 0);
    } catch (e) {
      alert(formatError(e));
    }
  }

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
  const name = artist?.name ?? "Artist";
  const sizeLabel = artist && artist.storage_bytes > 0 ? byteSize(artist.storage_bytes) : null;

  // Per-type counts for the filter chips (the "All" chip counts every release).
  const counts: Record<string, number> = { all: items.length };
  for (const d of SECTION_DEFS) counts[d.key] = items.filter((a) => a.album_type === d.key).length;

  // Sort within a section by release year (undated releases sink to the end for
  // "newest", rise to the front for "oldest"), tie-broken by title.
  const sortAlbums = (arr: MergedAlbum[]) =>
    [...arr].sort((a, b) => {
      const cmp =
        sort === "newest"
          ? (b.release_year ?? -Infinity) - (a.release_year ?? -Infinity)
          : (a.release_year ?? Infinity) - (b.release_year ?? Infinity);
      return cmp || a.title.localeCompare(b.title);
    });

  // Visible sections: honor the active filter, drop empties.
  const sections = SECTION_DEFS.filter((d) => filter === "all" || filter === d.key)
    .map((d) => ({ ...d, items: sortAlbums(items.filter((a) => a.album_type === d.key)) }))
    .filter((s) => s.items.length > 0);

  const emptyLabel =
    filter === "all" ? "releases" : (FILTERS.find((f) => f.key === filter)?.label ?? "releases").toLowerCase();

  // Shared metadata readout (release count · downloaded · size · source badge).
  const metaLine = (
    <>
      <span className="font-mono">
        {items.length} release{items.length === 1 ? "" : "s"}
        {downloaded > 0 ? ` · ${downloaded} downloaded` : ""}
        {sizeLabel ? ` · ${sizeLabel}` : ""}
      </span>
      {q.data && <SourceBadge source={q.data.source} />}
    </>
  );

  // Follow pill (shared between the mobile action row and desktop hero).
  const followButton = canFollow && (
    <button
      onClick={toggleFollow}
      {...offlineAttrs(
        online,
        followBusy,
        following ? "Unfollow this artist" : "Follow for new-release alerts",
      )}
      className={`inline-flex items-center gap-2 rounded-full border px-4 py-2.5 text-[13.5px] font-medium transition-colors disabled:opacity-50 ${
        following
          ? "border-oct-accent bg-oct-accent/10 text-oct-accent hover:border-oct-danger/50 hover:bg-oct-danger/10 hover:text-oct-danger"
          : "border-oct-border-strong text-oct-muted hover:border-oct-line hover:text-oct-text"
      }`}
    >
      <BellIcon size={14} />
      {following ? "Following" : "Follow"}
    </button>
  );

  // Filter chip — shared shape, tuned per layout via `className`.
  const filterChip = (f: (typeof FILTERS)[number], extra: string) => {
    const on = filter === f.key;
    return (
      <button
        key={f.key}
        onClick={() => setFilter(f.key)}
        className={`flex items-center gap-1.5 rounded-full text-[12.5px] transition-colors ${extra} ${
          on ? "bg-oct-accent font-semibold text-oct-bg" : "text-oct-muted hover:text-oct-text"
        }`}
      >
        {f.label}
        <span className={`font-mono text-[10px] ${on ? "text-oct-bg/60" : "text-oct-faint"}`}>
          {counts[f.key]}
        </span>
      </button>
    );
  };

  const sortButton = (extra: string) => (
    <button
      onClick={() => setSort((s) => (s === "newest" ? "oldest" : "newest"))}
      title="Toggle release order"
      className={extra}
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M4 4h9M4 8h6M4 12h3M12.5 6v7M12.5 13l2-2M12.5 13l-2-2" />
      </svg>
      {sort === "newest" ? "Newest first" : "Oldest first"}
    </button>
  );

  return (
    <section className="flex flex-col">
      {/* ── MOBILE HERO (full-bleed) ────────────────────────────────────── */}
      <div className="relative h-[320px] shrink-0 sm:hidden">
        <div className="absolute inset-0" style={{ background: gradientFor(id) }} />
        <BlurUpImage
          lowSrc={artistImageUrl(id, imgVersion || undefined, true)}
          fullSrc={artistImageUrl(id, imgVersion || undefined)}
          className="absolute inset-0 h-full w-full object-cover"
        />
        {/* legibility scrim — fades the art into the canvas toward the title */}
        <div
          className="absolute inset-0"
          style={{
            background:
              "linear-gradient(to bottom, rgba(11,12,14,0.28) 0%, rgba(11,12,14,0) 30%, rgba(11,12,14,0.55) 72%, #0b0c0e 100%)",
          }}
        />
        {/* top bar: back + (manager) edit image */}
        <div className="absolute inset-x-0 top-0 flex items-center justify-between p-3">
          <button
            onClick={() => navigate("/library")}
            aria-label="Back to library"
            className="grid h-[34px] w-[34px] place-items-center rounded-full bg-black/35 text-white backdrop-blur-sm"
          >
            <svg width="19" height="19" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
              <path d="M9.5 3.5 5 8l4.5 4.5" />
            </svg>
          </button>
          {isManager && (
            <button
              onClick={() => setEditImage(true)}
              {...offlineAttrs(online, false, "Edit artist image")}
              className="grid h-[34px] w-[34px] place-items-center rounded-full bg-black/35 text-white backdrop-blur-sm disabled:opacity-40"
            >
              <EditIcon size={15} />
            </button>
          )}
        </div>
        {/* title block anchored to the bottom of the art */}
        <div className="absolute inset-x-0 bottom-0 px-[18px] pb-3.5">
          <div className="font-mono text-[10px] tracking-[0.2em] text-oct-accent">ARTIST</div>
          <h1 className="mt-1.5 text-[42px] font-bold leading-none tracking-tight [text-shadow:0_2px_20px_rgba(0,0,0,0.45)]">
            {name}
          </h1>
          <p className="mt-2.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-oct-muted">
            {metaLine}
          </p>
        </div>
      </div>

      {/* ── MOBILE ACTIONS ──────────────────────────────────────────────── */}
      <div className="flex items-center gap-3 px-[18px] pb-1 pt-3 sm:hidden">
        {followButton}
        <FavoriteButton kind="artist" id={id} size={18} />
        <span className="flex-1" />
        <button
          onClick={() => void startRadio(true)}
          {...offlineAttrs(online, false, "Shuffle this artist's radio")}
          className="grid h-11 w-11 place-items-center rounded-full text-oct-muted transition-colors active:text-oct-text disabled:opacity-40"
        >
          <ShuffleIcon size={19} />
        </button>
        <button
          onClick={() => void startRadio(false)}
          {...offlineAttrs(online, false, "Play this artist's radio")}
          className="grid h-[52px] w-[52px] place-items-center rounded-full bg-oct-accent text-oct-bg shadow-[0_8px_22px_-8px_rgba(224,168,75,0.6)] transition-colors hover:bg-oct-accent-bright disabled:opacity-50"
        >
          <PlayIcon size={18} />
        </button>
      </div>

      {/* ── DESKTOP HERO ────────────────────────────────────────────────── */}
      <div className="hidden flex-col gap-6 px-6 pt-6 sm:flex md:px-8 md:pt-8">
        <Link to="/library" className="font-mono text-[11px] tracking-wide text-oct-subtle hover:text-oct-muted">
          ← LIBRARY
        </Link>
        <header className="flex flex-row items-center gap-6">
          <div className="relative shrink-0">
            <div
              className="relative h-[132px] w-[132px] overflow-hidden rounded-full border border-oct-border shadow-[0_16px_40px_-18px_rgba(0,0,0,0.6)]"
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
                className="absolute bottom-1 right-1 grid h-7 w-7 place-items-center rounded-full bg-black/60 text-white/90 backdrop-blur-sm transition-colors hover:bg-black/80 disabled:opacity-40"
              >
                <EditIcon size={13} />
              </button>
            )}
          </div>
          <div className="flex min-w-0 flex-col">
            <span className="font-mono text-[11px] tracking-[0.2em] text-oct-accent">ARTIST</span>
            <h1 className="mt-1.5 text-[44px] font-semibold tracking-tight">{name}</h1>
            <p className="mt-2.5 flex flex-wrap items-center gap-x-2 text-[13px] text-oct-subtle">{metaLine}</p>
            <div className="mt-4 flex flex-wrap items-center gap-3">
              {followButton}
              {/* Favorite (heart) is distinct from Follow (new-release alerts). */}
              <FavoriteButton kind="artist" id={id} size={18} />
              <button
                onClick={() => void startRadio(false)}
                {...offlineAttrs(online, false, "Start a radio from this artist")}
                className={btnPrimary}
              >
                <PlayIcon size={13} /> Radio
              </button>
            </div>
          </div>
        </header>
      </div>

      {/* ── BODY ────────────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-6 px-4 pb-8 pt-4 sm:px-6 sm:pb-10 sm:pt-6 md:px-8">
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

        {/* Storage & artist tools. For a manager these fold into one collapsible
            panel (storage relocation + merge + delete); a non-manager only ever
            sees the standalone storage warning when files are split, so
            <LibraryLocation> is rendered bare and self-hides otherwise. */}
        {isManager ? (
          <div className="overflow-hidden rounded-xl border border-oct-border-strong bg-oct-panel">
            <button
              onClick={() => setAdminOpen((o) => !o)}
              className="flex w-full items-center gap-3 px-4 py-3.5 text-left"
            >
              <FolderIcon size={14} className="shrink-0 text-oct-dim" />
              <span className="font-mono text-[10.5px] tracking-[0.16em] text-oct-subtle">
                STORAGE &amp; ARTIST TOOLS
              </span>
              {folderCount > 1 && (
                <span className="inline-flex items-center gap-1 rounded-md border border-oct-accent/25 bg-oct-accent/10 px-2 py-0.5 font-mono text-[9.5px] text-oct-accent">
                  ⚠ split across {folderCount} folders
                </span>
              )}
              <span className="flex-1" />
              <span className="font-mono text-[10px] text-oct-faint">{adminOpen ? "hide" : "manage"}</span>
              <ChevronDownIcon
                size={13}
                className={`text-oct-dim transition-transform ${adminOpen ? "rotate-180" : ""}`}
              />
            </button>
            {adminOpen && (
              <div className="flex flex-col gap-4 border-t border-oct-border-strong px-4 pb-4 pt-4">
                <LibraryLocation artistId={id} online={online} isManager={isManager} onChanged={refreshArtist} />
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
              </div>
            )}
          </div>
        ) : (
          <LibraryLocation artistId={id} online={online} isManager={isManager} onChanged={refreshArtist} />
        )}

        <DiscographyPanel artistId={id} online={online} isManager={isManager} />

        {q.isLoading && <SkeletonGrid count={12} />}
        {q.isError && (
          <p className="rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-sm text-oct-danger">
            {formatError(q.error)}
          </p>
        )}

        {/* discography */}
        {q.data && items.length === 0 && <p className="text-sm text-oct-subtle">No albums.</p>}
        {q.data && items.length > 0 && (
          <div className="flex flex-col gap-6 sm:gap-7">
            {/* desktop control bar: heading · filter chips · sort */}
            <div className="hidden flex-wrap items-center gap-3 sm:flex">
              <h2 className="text-xl font-semibold tracking-tight">Discography</h2>
              <div className="ml-1 flex gap-1 rounded-full border border-oct-border-strong bg-oct-card p-1">
                {FILTERS.map((f) => filterChip(f, "px-3.5 py-1.5"))}
              </div>
              <span className="flex-1" />
              {sortButton(
                "inline-flex items-center gap-2 rounded-lg border border-oct-border-strong px-3 py-2 text-[12.5px] text-oct-muted transition-colors hover:border-oct-line hover:text-oct-text",
              )}
            </div>

            {/* mobile: sticky, edge-to-edge filter rail */}
            <div className="sticky top-0 z-10 -mx-4 bg-oct-bg/95 px-4 py-2.5 backdrop-blur-sm sm:hidden">
              <div className="no-scrollbar flex gap-2 overflow-x-auto">
                {FILTERS.map((f) => filterChip(f, "shrink-0 border border-oct-border-strong px-4 py-1.5"))}
              </div>
            </div>
            {/* mobile: heading + sort */}
            <div className="-mt-1 flex items-center justify-between sm:hidden">
              <h2 className="text-[19px] font-semibold tracking-tight">Discography</h2>
              {sortButton("flex items-center gap-1.5 text-[12px] text-oct-muted active:text-oct-text")}
            </div>

            {/* per-type sections (rail on mobile, grid on desktop) */}
            {sections.length === 0 ? (
              <div className="flex flex-col items-center gap-2.5 py-14 text-center">
                <DiscGlyph />
                <p className="text-[13.5px] text-oct-muted">No {emptyLabel} for this artist</p>
              </div>
            ) : (
              <div className="flex flex-col gap-8 sm:gap-9">
                {sections.map((sec) => (
                  <div key={sec.key} className="flex flex-col gap-3.5 sm:gap-4">
                    <div className="flex items-baseline gap-3">
                      <h3 className="text-[16px] font-semibold tracking-tight sm:text-[17px]">{sec.title}</h3>
                      <span className="font-mono text-[11px] text-oct-faint">{sec.items.length}</span>
                    </div>
                    <div
                      className="no-scrollbar -mx-4 flex snap-x snap-mandatory gap-3.5 overflow-x-auto px-4 [scroll-padding-left:1rem] sm:mx-0 sm:grid sm:gap-x-[22px] sm:gap-y-7 sm:overflow-visible sm:px-0"
                      style={{ gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))" }}
                    >
                      {sec.items.map((a) => (
                        <ReleaseCard
                          key={a.id}
                          album={a}
                          online={online}
                          typeLabel={TYPE_LABEL[a.album_type]}
                          onOpen={() => navigate(`/albums/${a.id}`)}
                          onPlay={() => void playAlbum(a)}
                        />
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>

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
          hint={`Pick a duplicate artist to fold into "${name}". Its albums, tracks and followers move here, and every spelling is preserved.`}
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

/** One discography release tile: cover (with SAVED/STREAM badge) + play button,
 *  then title and "year · type" caption. The whole tile opens the album; the
 *  play button plays it in place. On mobile the play button is always shown and
 *  the tile is a fixed-width rail card; on desktop it's a fluid grid cell whose
 *  play button reveals on hover. Rendered as a div (not an <a>) so the nested
 *  play button stays valid markup. */
function ReleaseCard({
  album,
  online,
  typeLabel,
  onOpen,
  onPlay,
}: {
  album: MergedAlbum;
  online: boolean;
  typeLabel: string;
  onOpen: () => void;
  onPlay: () => void;
}) {
  // A downloaded album plays offline; an online-only one can't be fetched.
  const playDisabled = !online && !album.downloaded;
  return (
    <div
      className="group w-[150px] shrink-0 cursor-pointer snap-start sm:w-auto sm:shrink"
      onClick={onOpen}
    >
      <div className="relative">
        <Cover
          album={album}
          size={9999}
          radius={10}
          badge={album.downloaded ? <SavedBadge /> : <StreamBadge />}
        />
        <button
          onClick={(e) => {
            e.stopPropagation();
            onPlay();
          }}
          disabled={playDisabled}
          title={playDisabled ? OFFLINE_MSG : `Play ${album.title}`}
          className="absolute bottom-2.5 right-2.5 z-10 grid h-9 w-9 place-items-center rounded-full bg-oct-accent text-oct-bg shadow-[0_8px_20px_-6px_rgba(0,0,0,0.55)] transition-all duration-150 hover:bg-oct-accent-bright disabled:opacity-40 sm:translate-y-1.5 sm:opacity-0 sm:group-hover:translate-y-0 sm:group-hover:opacity-100 sm:disabled:opacity-0"
        >
          <PlayIcon size={14} />
        </button>
      </div>
      <div className="mt-2.5 truncate text-[13.5px] font-medium group-hover:text-white">{album.title}</div>
      <div className="mt-0.5 flex items-center gap-1.5 font-mono text-[11px] text-oct-subtle">
        {album.release_year != null && (
          <>
            <span>{album.release_year}</span>
            <span className="text-oct-faint">·</span>
          </>
        )}
        <span>{typeLabel}</span>
      </div>
    </div>
  );
}

/** Empty-state disc glyph (matches the comp's centered "no releases" motif). */
function DiscGlyph() {
  return (
    <svg width="26" height="26" viewBox="0 0 16 16" fill="none" stroke="currentColor" className="text-oct-faint">
      <circle cx="8" cy="8" r="5.5" strokeWidth="1.3" />
      <circle cx="8" cy="8" r="1.2" fill="currentColor" stroke="none" />
    </svg>
  );
}
