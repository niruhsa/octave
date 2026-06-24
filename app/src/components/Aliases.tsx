// "Also known as" — shows every preserved spelling of an artist/album and
// (Manager+, online) lets a manager add/remove a spelling or choose which one
// displays. The canonical display name normally follows the server's
// PRIMARY_LANGUAGE; "make primary" is a manual override.
//
// Aliases are populated only on single-entity reads (the Artist/Album routes),
// so this renders nothing when the list is empty and the user can't add.

import { useState } from "react";
import {
  type AliasInfo,
  libraryAddAlbumAlias,
  libraryAddArtistAlias,
  libraryRemoveAlbumAlias,
  libraryRemoveArtistAlias,
  librarySetPrimaryAlbumAlias,
  librarySetPrimaryArtistAlias,
} from "../ipc";
import { formatError } from "../lib/error";
import { input } from "../lib/ui";
import { offlineAttrs } from "./OfflineGate";

type Props = {
  kind: "artist" | "album";
  entityId: string;
  aliases: AliasInfo[];
  online: boolean;
  isManager: boolean;
  /** Refresh the parent entity query after a change. */
  onChanged: () => void;
};

export function Aliases({ kind, entityId, aliases, online, isManager, onChanged }: Props) {
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [language, setLanguage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Nothing to show and nothing to do.
  if (aliases.length === 0 && !isManager) return null;

  async function run(fn: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await fn();
      onChanged();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusy(false);
    }
  }

  async function addAlias() {
    const n = name.trim();
    if (!n) return;
    const lang = language.trim() || undefined;
    await run(async () => {
      if (kind === "artist") await libraryAddArtistAlias(entityId, n, undefined, lang);
      else await libraryAddAlbumAlias(entityId, n, lang);
      setName("");
      setLanguage("");
      setAdding(false);
    });
  }
  const makePrimary = (aliasId: string) =>
    run(() =>
      kind === "artist"
        ? librarySetPrimaryArtistAlias(entityId, aliasId)
        : librarySetPrimaryAlbumAlias(entityId, aliasId),
    );
  const remove = (aliasId: string) =>
    run(() =>
      kind === "artist"
        ? libraryRemoveArtistAlias(entityId, aliasId)
        : libraryRemoveAlbumAlias(entityId, aliasId),
    );

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">ALSO KNOWN AS</span>
        {aliases.map((a) => (
          <span
            key={a.id}
            className={`group inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[12px] ${
              a.is_primary
                ? "border-oct-accent/50 bg-oct-accent/10 text-oct-accent"
                : "border-oct-border bg-oct-elevated/40 text-oct-muted"
            }`}
            title={a.language ?? undefined}
          >
            {a.is_primary && <span title="Primary (displayed) spelling">★</span>}
            <span className="max-w-[220px] truncate">{a.name}</span>
            {a.language && <span className="font-mono text-[9.5px] text-oct-faint">{a.language}</span>}
            {isManager && !a.is_primary && (
              <span className="ml-0.5 hidden items-center gap-1 group-hover:inline-flex">
                <button
                  onClick={() => void makePrimary(a.id)}
                  {...offlineAttrs(online, busy, "Show this spelling")}
                  className="text-oct-subtle hover:text-oct-accent disabled:opacity-40"
                  title="Make this the displayed name"
                >
                  ☆
                </button>
                {aliases.length > 1 && (
                  <button
                    onClick={() => void remove(a.id)}
                    {...offlineAttrs(online, busy, "Remove spelling")}
                    className="text-oct-subtle hover:text-oct-danger disabled:opacity-40"
                    title="Remove this spelling"
                  >
                    ✕
                  </button>
                )}
              </span>
            )}
          </span>
        ))}
        {isManager && !adding && (
          <button
            onClick={() => setAdding(true)}
            {...offlineAttrs(online, busy, "Add a spelling")}
            className="rounded-full border border-dashed border-oct-border px-2.5 py-1 text-[12px] text-oct-subtle hover:border-oct-accent/50 hover:text-oct-accent disabled:opacity-40"
          >
            + add spelling
          </button>
        )}
      </div>

      {adding && (
        <div className="flex flex-wrap items-center gap-2">
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void addAlias();
              if (e.key === "Escape") setAdding(false);
            }}
            placeholder={kind === "artist" ? "Alternate name" : "Alternate title"}
            className={`${input} max-w-[260px]`}
          />
          <input
            value={language}
            onChange={(e) => setLanguage(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void addAlias();
              if (e.key === "Escape") setAdding(false);
            }}
            placeholder="Language (optional)"
            className={`${input} max-w-[160px]`}
          />
          <button
            onClick={() => void addAlias()}
            disabled={busy || !name.trim()}
            className="rounded-md bg-oct-accent/90 px-3 py-1.5 text-[12px] font-medium text-black hover:bg-oct-accent disabled:opacity-40"
          >
            Add
          </button>
          <button
            onClick={() => {
              setAdding(false);
              setName("");
              setLanguage("");
            }}
            className="font-mono text-[11px] text-oct-subtle hover:text-oct-text"
          >
            cancel
          </button>
        </div>
      )}
      {error && <p className="text-[12px] text-oct-danger">{error}</p>}
    </div>
  );
}
