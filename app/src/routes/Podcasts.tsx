import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import {
  podcastList,
  podcastSearch,
  podcastSubscribe,
  podcastSubscribeFeed,
  type MergedPodcast,
  type PodcastCandidate,
} from "../ipc";
import { SourceBadge } from "../components/SourceBadge";
import { PodcastIcon } from "../components/icons";
import { useAppStore } from "../store";
import { btnGhostSm, btnPrimary, card, errorBox, input, label } from "../lib/ui";
import { offlineAttrs } from "../components/OfflineGate";
import { formatError } from "../lib/error";
import { usePodcastPrefsStore } from "../podcasts/prefs";

/** Square show cover from a remote art URL, falling back to the mic icon. */
function ShowArt({ url, size = 56 }: { url: string | null; size?: number }) {
  const [broken, setBroken] = useState(false);
  if (url && !broken) {
    return (
      <img
        src={url}
        alt=""
        width={size}
        height={size}
        onError={() => setBroken(true)}
        className="rounded-lg object-cover shrink-0"
        style={{ width: size, height: size }}
      />
    );
  }
  return (
    <div
      className="rounded-lg bg-oct-panel grid place-items-center text-oct-faint shrink-0"
      style={{ width: size, height: size }}
    >
      <PodcastIcon size={Math.round(size * 0.42)} />
    </div>
  );
}

export default function Podcasts() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const online = useAppStore((s) => s.online);
  const tier = useAppStore((s) => s.tier);
  const isManager = tier === "admin" || tier === "manager";
  const openAfterSubscribe = usePodcastPrefsStore((s) => s.prefs.openAfterSubscribe);

  const subs = useQuery({
    queryKey: ["podcasts", "subscriptions"],
    queryFn: () => podcastList(),
  });

  const [term, setTerm] = useState("");
  const [results, setResults] = useState<PodcastCandidate[] | null>(null);
  const [searchErr, setSearchErr] = useState<string | null>(null);

  const search = useMutation({
    mutationFn: (q: string) => podcastSearch(q, 25),
    onSuccess: (r) => {
      setResults(r);
      setSearchErr(null);
    },
    onError: (e) => setSearchErr(formatError(e)),
  });

  const add = useMutation({
    // Add the feed to the catalog (Manager+), then subscribe the current user.
    // `open` decides whether to jump to the new show's page afterward (the
    // pref, inverted by a Shift+Click — resolved at click time).
    mutationFn: async ({ candidate }: { candidate: PodcastCandidate; open: boolean }) => {
      const show = await podcastSubscribeFeed(
        candidate.feed_url ?? undefined,
        candidate.itunes_id ?? undefined,
      );
      await podcastSubscribe(show.id).catch(() => {});
      return show;
    },
    onSuccess: (show, { open }) => {
      void qc.invalidateQueries({ queryKey: ["podcasts", "subscriptions"] });
      if (open) navigate(`/podcasts/${show.id}`);
    },
  });

  const subscribedFeeds = new Set((subs.data?.items ?? []).map((p) => p.feed_url));

  return (
    <div className="mx-auto max-w-3xl px-4 py-6 space-y-8">
      {/* ---- search / discover ---- */}
      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <h1 className="text-lg font-semibold">Podcasts</h1>
        </div>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (term.trim()) search.mutate(term.trim());
          }}
          className="flex gap-2"
        >
          <input
            value={term}
            onChange={(e) => setTerm(e.target.value)}
            placeholder="Search Apple Podcasts / PodcastIndex…"
            className={`${input} flex-1`}
            {...offlineAttrs(online, false, "Search needs a connection")}
          />
          <button
            type="submit"
            className={btnPrimary}
            disabled={!online || search.isPending || !term.trim()}
          >
            {search.isPending ? "Searching…" : "Search"}
          </button>
        </form>
        {searchErr && <div className={errorBox}>{searchErr}</div>}
        {results && results.length === 0 && (
          <p className="text-sm text-oct-dim">No shows found.</p>
        )}
        {results && results.length > 0 && (
          <ul className={`${card} divide-y divide-oct-border`}>
            {results.map((c) => {
              const already = subscribedFeeds.has(c.feed_url);
              // `add` is one mutation shared by every row, so scope the pending
              // state to the candidate actually being added.
              const adding =
                add.isPending && add.variables?.candidate.feed_url === c.feed_url;
              return (
                <li key={c.feed_url} className="flex items-center gap-3 p-3">
                  <ShowArt url={c.image_url} size={48} />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{c.title}</div>
                    {c.author && (
                      <div className="truncate text-xs text-oct-dim">{c.author}</div>
                    )}
                  </div>
                  {already ? (
                    <span className="text-xs text-oct-faint">Subscribed</span>
                  ) : (
                    <button
                      className={btnGhostSm}
                      onClick={(e) =>
                        add.mutate({
                          candidate: c,
                          // Shift inverts the "open after subscribing" pref.
                          open: openAfterSubscribe !== e.shiftKey,
                        })
                      }
                      disabled={!isManager || adding}
                      title={
                        isManager
                          ? openAfterSubscribe
                            ? "Add + subscribe (Shift+Click to stay here)"
                            : "Add + subscribe (Shift+Click to open the show)"
                          : "Only managers can add new podcasts"
                      }
                    >
                      {adding ? "Adding…" : "Subscribe"}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
        {add.isError && <div className={errorBox}>{formatError(add.error)}</div>}
      </section>

      {/* ---- subscribed shows ---- */}
      <section className="space-y-3">
        <div className="flex items-center gap-2">
          <h2 className={label}>SUBSCRIBED</h2>
          {subs.data && <SourceBadge source={subs.data.source} />}
        </div>
        {subs.isLoading ? (
          <p className="text-sm text-oct-dim">Loading…</p>
        ) : (subs.data?.items.length ?? 0) === 0 ? (
          <p className="text-sm text-oct-dim">
            No subscriptions yet. Search above to find a show.
          </p>
        ) : (
          <ul className="grid grid-cols-1 sm:grid-cols-2 gap-2">
            {subs.data!.items.map((p: MergedPodcast) => (
              <li key={p.id}>
                <Link
                  to={`/podcasts/${p.id}`}
                  className={`${card} flex items-center gap-3 p-3 hover:border-oct-border-strong`}
                >
                  <ShowArt url={p.image_url} />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{p.title}</div>
                    {p.author && (
                      <div className="truncate text-xs text-oct-dim">{p.author}</div>
                    )}
                    {p.downloaded_count > 0 && (
                      <div className="mt-0.5 text-[11px] text-oct-faint">
                        {p.downloaded_count} downloaded
                      </div>
                    )}
                  </div>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
