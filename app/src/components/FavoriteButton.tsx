// Reusable favorite (heart) toggle (Phase 11). Renders nothing for a non-user
// (SECRET_KEY / anonymous) session — favorites are per-user. Tracks use the
// shared store (instant, bulk-hydrated); albums/artists query their own state.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { favoritesFavorite, favoritesIsFavorite, favoritesUnfavorite, type FavoriteKind } from "../ipc";
import { useAppStore } from "../store";
import { useFavoritesStore } from "../favorites/useFavorites";
import { formatError } from "../lib/error";
import { HeartIcon } from "./icons";

export function FavoriteButton({
  kind,
  id,
  size = 16,
  className = "",
}: {
  kind: FavoriteKind;
  id: string;
  size?: number;
  className?: string;
}) {
  const isUser = useAppStore((s) => s.session?.kind === "bearer");
  const online = useAppStore((s) => s.online);
  // Favorites are per-user + online-only; hide the affordance otherwise.
  if (!isUser) return null;
  return kind === "track" ? (
    <TrackHeart id={id} size={size} className={className} disabled={!online} />
  ) : (
    <EntityHeart kind={kind} id={id} size={size} className={className} disabled={!online} />
  );
}

function heartCls(active: boolean, disabled: boolean, extra: string): string {
  return [
    "inline-grid place-items-center rounded-md p-1 transition-colors",
    active ? "text-oct-accent" : "text-oct-faint hover:text-oct-subtle",
    disabled ? "opacity-40" : "hover:bg-oct-elevated/50",
    extra,
  ].join(" ");
}

function TrackHeart({
  id,
  size,
  className,
  disabled,
}: {
  id: string;
  size: number;
  className: string;
  disabled: boolean;
}) {
  const active = useFavoritesStore((s) => s.trackIds.has(id));
  const toggle = useFavoritesStore((s) => s.toggleTrack);
  return (
    <button
      type="button"
      disabled={disabled}
      aria-pressed={active}
      title={active ? "Remove from favorites" : "Add to favorites"}
      onClick={(e) => {
        e.stopPropagation();
        e.preventDefault();
        void toggle(id).catch((err) => alert(formatError(err)));
      }}
      className={heartCls(active, disabled, className)}
    >
      <HeartIcon size={size} />
    </button>
  );
}

function EntityHeart({
  kind,
  id,
  size,
  className,
  disabled,
}: {
  kind: FavoriteKind;
  id: string;
  size: number;
  className: string;
  disabled: boolean;
}) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["favorite", kind, id],
    queryFn: () => favoritesIsFavorite(kind, id),
    enabled: !disabled,
  });
  const active = q.data ?? false;

  async function toggle(e: React.MouseEvent) {
    e.stopPropagation();
    e.preventDefault();
    // Optimistic update of the cached query value, revert on error.
    qc.setQueryData(["favorite", kind, id], !active);
    try {
      if (active) await favoritesUnfavorite(kind, id);
      else await favoritesFavorite(kind, id);
    } catch (err) {
      qc.setQueryData(["favorite", kind, id], active);
      alert(formatError(err));
    }
  }

  return (
    <button
      type="button"
      disabled={disabled}
      aria-pressed={active}
      title={active ? "Remove from favorites" : "Add to favorites"}
      onClick={(e) => void toggle(e)}
      className={heartCls(active, disabled, className)}
    >
      <HeartIcon size={size} />
    </button>
  );
}
