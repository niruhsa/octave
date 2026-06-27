import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { appInfo, authRefreshOnline, cacheListDownloadedTracks, getLibraryStorage } from "../ipc";
import { useAppStore } from "../store";
import { useSyncStore } from "../sync/useSync";
import { btnPrimary, card } from "../lib/ui";
import { byteSize } from "../lib/format";
import { Skeleton } from "../components/Skeleton";
import {
  DiscIcon,
  DownloadIcon,
  PlaylistIcon,
  SearchIcon,
  SyncIcon,
  UploadIcon,
  type IconProps,
} from "../components/icons";

/**
 * Home dashboard — OCTAVE status landing. Server/sync/cache summary cards plus
 * quick links. The anonymous case (no session) shows a sign-in prompt since
 * the sidebar is hidden without a session.
 */
export default function Home() {
  const online = useAppStore((s) => s.online);
  const tier = useAppStore((s) => s.tier);
  const session = useAppStore((s) => s.session);
  const setOnline = useAppStore((s) => s.setOnline);

  const syncStatus = useSyncStore((s) => s.status);
  const pending = useSyncStore((s) => s.pending);
  const lastReport = useSyncStore((s) => s.lastReport);
  const lastError = useSyncStore((s) => s.lastError);
  const runSync = useSyncStore((s) => s.run);

  const info = useQuery({ queryKey: ["app_info"], queryFn: appInfo });
  const downloads = useQuery({ queryKey: ["cache", "downloaded_tracks"], queryFn: cacheListDownloadedTracks });
  // Server-side library size. Online-only (a live server view); when offline
  // the query errors and the widget shows an "unavailable offline" note.
  const storage = useQuery({
    queryKey: ["library_storage"],
    queryFn: getLibraryStorage,
    enabled: online,
    retry: false,
  });

  if (!session) {
    return (
      <div className="flex min-h-full flex-col items-center justify-center gap-5 p-6 text-center">
        <span className="block h-12 w-12 rounded-xl bg-oct-accent" />
        <div>
          <div className="text-2xl font-semibold tracking-[0.18em]">OCTAVE</div>
          <p className="mt-2 text-sm text-oct-subtle">Sign in to stream and manage your library.</p>
        </div>
        <Link to="/login" className={btnPrimary}>Sign in</Link>
      </div>
    );
  }

  const syncLabel =
    syncStatus === "syncing" ? "Syncing…" : syncStatus === "error" ? "Sync error" : pending > 0 ? `${pending} pending` : "Up to date";

  return (
    <section className="flex flex-col gap-7 p-6 md:p-8">
      <header className="flex items-end justify-between gap-4">
        <div>
          <p className="font-mono text-[11px] tracking-[0.16em] text-oct-faint">OCTAVE · DESKTOP</p>
          <h1 className="mt-1.5 text-3xl font-semibold tracking-tight">
            Welcome back{session.username ? `, ${session.username}` : ""}
          </h1>
        </div>
      </header>

      {/* status cards */}
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard label="SERVER">
          <div className="flex items-center gap-2.5">
            <button onClick={async () => setOnline(await authRefreshOnline())} title="Re-check" className="flex items-center gap-2.5">
              <span
                className={`inline-block h-2.5 w-2.5 rounded-full ${online ? "bg-oct-online animate-octpulse" : "bg-oct-offline"}`}
                style={{ boxShadow: online ? "0 0 0 3px rgba(63,185,80,0.15)" : "0 0 0 3px rgba(138,90,74,0.12)" }}
              />
              <span className={`text-lg font-semibold ${online ? "text-oct-online" : "text-oct-offline"}`}>
                {online ? "Online" : "Offline"}
              </span>
            </button>
          </div>
          <p className="mt-1 font-mono text-[11px] text-oct-subtle">tap to re-check</p>
        </StatCard>

        <StatCard label="SYNC">
          <button onClick={() => void runSync()} disabled={syncStatus === "syncing"} className="flex items-center gap-2.5 text-left">
            <SyncIcon size={18} className={`text-oct-accent ${syncStatus === "syncing" ? "animate-octspin" : ""}`} />
            <span className="text-lg font-semibold text-oct-text">{syncLabel}</span>
          </button>
          <p className="mt-1 font-mono text-[11px] text-oct-subtle">
            {syncStatus === "error"
              ? lastError
              : lastReport
                ? `↑${lastReport.ops_pushed} ↻${lastReport.entities_updated} ✕${lastReport.entities_pruned}`
                : "tap to sync now"}
          </p>
        </StatCard>

        <Link to="/downloads" className="block">
          <StatCard label="OFFLINE">
            <div className="flex items-center gap-2.5">
              <DownloadIcon size={18} className="text-oct-accent" />
              {downloads.isLoading ? (
                <Skeleton className="h-6 w-8" />
              ) : (
                <span className="text-lg font-semibold">{downloads.data?.length ?? 0}</span>
              )}
            </div>
            <p className="mt-1 font-mono text-[11px] text-oct-subtle">tracks downloaded</p>
          </StatCard>
        </Link>

        <StatCard label="ACCOUNT">
          <div className="text-lg font-semibold capitalize">{tier}</div>
          {info.isLoading ? (
            <Skeleton className="mt-1.5 h-3 w-24" />
          ) : (
            <p className="mt-1 truncate font-mono text-[11px] text-oct-subtle">
              {info.data ? `${info.data.name} ${info.data.version}` : "—"}
            </p>
          )}
        </StatCard>
      </div>

      {/* library storage */}
      <StorageWidget
        data={storage.data}
        loading={storage.isLoading && online}
        unavailable={!online || storage.isError}
      />

      {/* quick links */}
      <div>
        <h2 className="mb-3 font-mono text-[11px] tracking-[0.14em] text-oct-faint">JUMP TO</h2>
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <QuickTile to="/library" Icon={DiscIcon} title="Library" sub="Browse artists & albums" />
          <QuickTile to="/search" Icon={SearchIcon} title="Search" sub="Find anything" />
          <QuickTile to="/playlists" Icon={PlaylistIcon} title="Playlists" sub="Your collections" />
          <QuickTile to="/downloads" Icon={DownloadIcon} title="Downloads" sub="Offline content" />
          {(tier === "admin" || tier === "manager") && (
            <QuickTile to="/upload" Icon={UploadIcon} title="Upload" sub="Add music to the server" />
          )}
        </div>
      </div>
    </section>
  );
}

/** Server-side library size: total + a stacked music/podcasts/misc bar. */
function StorageWidget({
  data,
  loading,
  unavailable,
}: {
  data: import("../ipc").LibraryStorage | undefined;
  loading: boolean;
  unavailable: boolean;
}) {
  const misc = data ? data.artwork_bytes + data.other_bytes : 0;
  const total = data?.total_bytes ?? 0;
  const segments = [
    { label: "Music", bytes: data?.music_bytes ?? 0, color: "var(--oct-accent, #e0a84b)" },
    { label: "Podcasts", bytes: data?.podcast_bytes ?? 0, color: "#5a8bd6" },
    { label: "Misc", bytes: misc, color: "#6b7280" },
  ];
  const pct = (b: number) => (total > 0 ? (b / total) * 100 : 0);

  return (
    <div className={`${card} p-5`}>
      <div className="flex items-baseline justify-between gap-4">
        <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">LIBRARY STORAGE</div>
        {data && (
          <div className="font-mono text-[11px] text-oct-subtle">
            {data.track_count.toLocaleString()} tracks · {data.album_count.toLocaleString()} albums
            {data.episode_count > 0 ? ` · ${data.episode_count.toLocaleString()} episodes` : ""}
          </div>
        )}
      </div>

      {loading ? (
        <div className="mt-3 space-y-3">
          <Skeleton className="h-8 w-40" />
          <Skeleton className="h-2.5 w-full rounded-full" />
        </div>
      ) : unavailable || !data ? (
        <p className="mt-3 font-mono text-[12px] text-oct-subtle">
          Storage usage is unavailable offline.
        </p>
      ) : (
        <>
          <div className="mt-2 text-3xl font-semibold tracking-tight">{byteSize(total)}</div>

          {/* stacked bar */}
          <div className="mt-3 flex h-2.5 w-full overflow-hidden rounded-full bg-oct-elevated">
            {segments.map((s) =>
              s.bytes > 0 ? (
                <div
                  key={s.label}
                  style={{ width: `${pct(s.bytes)}%`, background: s.color }}
                  title={`${s.label} · ${byteSize(s.bytes)}`}
                />
              ) : null,
            )}
          </div>

          {/* legend */}
          <div className="mt-3 flex flex-wrap gap-x-6 gap-y-1.5">
            {segments.map((s) => (
              <div key={s.label} className="flex items-center gap-2">
                <span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: s.color }} />
                <span className="text-[13px] font-medium">{s.label}</span>
                <span className="font-mono text-[11px] text-oct-subtle">{byteSize(s.bytes)}</span>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function StatCard({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className={`${card} p-4`}>
      <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">{label}</div>
      <div className="mt-2.5">{children}</div>
    </div>
  );
}

function QuickTile({
  to,
  Icon,
  title,
  sub,
}: {
  to: string;
  Icon: (p: IconProps) => React.ReactElement;
  title: string;
  sub: string;
}) {
  return (
    <Link
      to={to}
      className="group flex items-center gap-3 rounded-xl border border-oct-border-strong bg-oct-panel p-4 transition-colors hover:border-oct-line hover:bg-oct-elevated/40"
    >
      <span className="grid h-11 w-11 shrink-0 place-items-center rounded-lg text-oct-accent" style={{ background: "rgba(224,168,75,0.12)" }}>
        <Icon size={20} />
      </span>
      <span className="min-w-0">
        <span className="block text-[15px] font-medium group-hover:text-white">{title}</span>
        <span className="block truncate font-mono text-[11px] text-oct-subtle">{sub}</span>
      </span>
    </Link>
  );
}
