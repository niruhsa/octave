// Quick Search token model + query derivation.
//
// The palette's search bar is built from "tokens" (the pills) plus the live
// draft. A token is either a free-text term or a `field:value` filter where
// `field` scopes the search to one entity category. The OCTAVE backend only
// offers free-text search per entity type (no server-side genre/field query),
// so field prefixes here select *which* categories to search and supply that
// category's query string; plain terms broaden the search to the core three
// categories and additionally narrow every category client-side.

export type SearchCat = "artist" | "album" | "track" | "playlist" | "podcast";

/** Prefix → category. `song`/`track` are aliases for the track category. */
export const FIELD_CATS: Record<string, SearchCat> = {
  artist: "artist",
  album: "album",
  song: "track",
  track: "track",
  playlist: "playlist",
  podcast: "podcast",
};

/** Prefixes offered as autocomplete + the "Filter by" hints (display order). */
export const PREFIXES = ["artist", "album", "song", "playlist", "podcast"] as const;

/** Categories searched when no field prefix is present. */
const CORE_CATS: SearchCat[] = ["artist", "album", "track"];

export type Token = {
  /** The raw text the user typed (round-trips when editing the pill). */
  raw: string;
  /** Lowercased field name when the token is `field:value`, else null. */
  field: string | null;
  /** The value portion (the whole raw text for a fieldless token). */
  value: string;
};

/** Parse a raw string into a token, splitting on the first `field:` prefix. */
export function parseToken(raw: string): Token {
  const trimmed = (raw || "").trim();
  const m = trimmed.match(/^([a-z]+):(.*)$/i);
  if (m) return { raw: trimmed, field: m[1].toLowerCase(), value: m[2] };
  return { raw: trimmed, field: null, value: trimmed };
}

/** The mode the draft selects: `>` commands, `!` go-to, else search. */
export type Mode = "search" | "command" | "tab";

export function modeOf(draft: string): Mode {
  if (draft[0] === ">") return "command";
  if (draft[0] === "!") return "tab";
  return "search";
}

export type DerivedSearch = {
  /** Categories to query, derived from field tokens (or the core three). */
  cats: SearchCat[];
  /** Whether there's anything to search at all. */
  hasQuery: boolean;
  /** The query string to send for a given category. */
  queryFor: (cat: SearchCat) => string;
  /** Plain (fieldless) term values — used to narrow every category client-side. */
  plainTerms: string[];
  /** A stable string identifying this search (for debounce/query keys). */
  key: string;
};

/**
 * Fold the tokens + live draft into the set of category searches to run. The
 * draft is treated as an implicit trailing token while it's being typed (so
 * results update live before the user commits it to a pill).
 */
export function deriveSearch(tokens: Token[], draftRaw: string): DerivedSearch {
  const draft = draftRaw.trim();
  const all = [...tokens];
  if (draft && draft[0] !== ">" && draft[0] !== "!") all.push(parseToken(draft));

  const fieldTokens = all.filter((t) => t.field && FIELD_CATS[t.field]);
  const plainTerms = all.filter((t) => !t.field).map((t) => t.value.trim()).filter(Boolean);

  let cats: SearchCat[];
  if (fieldTokens.length) {
    cats = [...new Set(fieldTokens.map((t) => FIELD_CATS[t.field as string]))];
  } else {
    cats = [...CORE_CATS];
  }

  const queryFor = (cat: SearchCat): string => {
    const ft = fieldTokens.find((t) => FIELD_CATS[t.field as string] === cat);
    const base = ft ? ft.value.trim() : "";
    return [base, ...plainTerms].filter(Boolean).join(" ").trim();
  };

  const key = JSON.stringify({ c: cats, q: cats.map(queryFor), p: plainTerms });
  return { cats, hasQuery: all.length > 0, queryFor, plainTerms, key };
}

/** Case-insensitive substring test used for client-side narrowing. */
export function includesCI(haystack: string, needle: string): boolean {
  return haystack.toLowerCase().includes(needle.toLowerCase());
}

/**
 * Ghost-completion for the draft: command/tab names in their modes, and the
 * field prefixes in search mode (e.g. typing `art` ghosts `ist:`). Returns the
 * suffix to append, or "" when there's nothing to complete.
 */
export function computeGhost(
  draft: string,
  commandNames: string[],
  tabIds: string[],
): string {
  if (draft[0] === ">") {
    const q = draft.slice(1).toLowerCase();
    if (!q) return "";
    const m = commandNames.find((c) => c.toLowerCase().startsWith(q));
    return m ? m.slice(draft.length - 1) : "";
  }
  if (draft[0] === "!") {
    const q = draft.slice(1).toLowerCase();
    if (!q) return "";
    const m = tabIds.find((t) => t.startsWith(q));
    return m ? m.slice(draft.length - 1) : "";
  }
  // search mode — complete a field prefix while the user is still typing it.
  const q = draft.toLowerCase();
  if (q && !q.includes(":") && !q.includes(" ")) {
    const m = PREFIXES.find((p) => p.startsWith(q) && p !== q);
    if (m) return m.slice(draft.length) + ":";
  }
  return "";
}
