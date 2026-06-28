// Quick Search — the OCTAVE command palette (⌘K / Ctrl K).
//
// A single global overlay, mounted once in `RootLayout`, that replaces the old
// full-page Search tab. It has three modes selected by the first character of
// the draft:
//   • (default)  search — filter the library; pills (`artist:`, `album:`,
//                 `song:`, `playlist:`, `podcast:`) scope the query.
//   • `>`        command — run a playback / sync action.
//   • `!`        go to — jump to a route.
//
// Results split into "on this device" (downloaded) and "stream from <server>"
// (online-only) groups. The keyboard model mirrors the design comp: Tab/→
// accepts the ghost completion, `|` or ↵ commits the draft to a pill, ↑↓ move
// the selection, ↵ activates, and Esc steps back out (clear draft → close).

import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  librarySearchArtists,
  librarySearchAlbums,
  librarySearchTracks,
  playlistList,
  podcastList,
  authServerConfig,
} from "../ipc";
import type { MergedTrack } from "../ipc";
import { usePlayerStore } from "../player/store";
import { useSyncStore } from "../sync/useSync";
import { useQuickSearchStore } from "../quicksearch/store";
import { useKeybindStore, eventMatches } from "../settings/keybinds";
import {
  deriveSearch,
  modeOf,
  computeGhost,
  parseToken,
  includesCI,
  PREFIXES,
  type Token,
  type SearchCat,
} from "../quicksearch/match";
import { qualityLabel } from "../lib/visual";
import { formatDuration } from "../lib/format";
import {
  ArtistIcon,
  DiscIcon,
  PlaylistIcon,
  PodcastIcon,
  SearchIcon,
  SongIcon,
  CloudIcon,
  type IconProps,
} from "./icons";

// ---------------------------------------------------------------------------
// command + go-to catalogs (real app actions / routes)
// ---------------------------------------------------------------------------

type Command = { name: string; desc: string; run: () => void };

const COMMANDS: Command[] = [
  { name: "Play / Pause", desc: "Toggle playback", run: () => usePlayerStore.getState().togglePlay() },
  { name: "Next track", desc: "Skip to the next track", run: () => usePlayerStore.getState().next() },
  { name: "Previous track", desc: "Previous track (or restart)", run: () => usePlayerStore.getState().prev() },
  { name: "Toggle shuffle", desc: "Shuffle the queue on / off", run: () => usePlayerStore.getState().toggleShuffle() },
  { name: "Toggle repeat", desc: "Cycle repeat mode", run: () => usePlayerStore.getState().cycleRepeat() },
  { name: "Sync library now", desc: "Reconcile with the server", run: () => void useSyncStore.getState().run() },
  { name: "Clear queue", desc: "Empty the playback queue", run: () => usePlayerStore.getState().clearQueue() },
];

type Tab = { id: string; label: string; desc: string; to: string };

const TABS: Tab[] = [
  { id: "home", label: "Home", desc: "Overview & recents", to: "/" },
  { id: "library", label: "Library", desc: "Artists, albums & tracks", to: "/library" },
  { id: "playlists", label: "Playlists", desc: "Your playlists", to: "/playlists" },
  { id: "podcasts", label: "Podcasts", desc: "Subscribed shows", to: "/podcasts" },
  { id: "downloads", label: "Downloads", desc: "Saved on this device", to: "/downloads" },
  { id: "notifications", label: "Notifications", desc: "New releases", to: "/notifications" },
  { id: "account", label: "Account", desc: "Server, audio & account", to: "/account" },
  { id: "upload", label: "Upload", desc: "Add music to the library", to: "/upload" },
  { id: "settings", label: "Settings", desc: "Preferences & keybinds", to: "/settings" },
];

const COMMAND_NAMES = COMMANDS.map((c) => c.name);
const TAB_IDS = TABS.map((t) => t.id);

const CAT_ICON: Record<SearchCat, (p: IconProps) => React.ReactElement> = {
  artist: ArtistIcon,
  album: DiscIcon,
  track: SongIcon,
  playlist: PlaylistIcon,
  podcast: PodcastIcon,
};

const TAB_ICON: Record<string, (p: IconProps) => React.ReactElement> = {
  home: DiscIcon,
  library: DiscIcon,
  playlists: PlaylistIcon,
  podcasts: PodcastIcon,
  downloads: SongIcon,
  notifications: PodcastIcon,
  account: ArtistIcon,
  upload: SongIcon,
  settings: PlaylistIcon,
};

// ---------------------------------------------------------------------------
// result row model
// ---------------------------------------------------------------------------

type Row = {
  cat: SearchCat;
  id: string;
  title: string;
  subtitle: string;
  /** Right-aligned mono detail (duration / quality / count). */
  detail: string;
  downloaded: boolean;
  /** Underlying merged entity (used for playback / navigation). */
  data: unknown;
};

function hex2rgba(hex: string, a: number): string {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
}

const ACCENT = "#e0a84b";
const CMD_COLOR = "#8a93e0";
const TAB_COLOR = "#5fb3a3";

export default function QuickSearch() {
  const open = useQuickSearchStore((s) => s.open);
  const close = useQuickSearchStore((s) => s.close);
  const recents = useQuickSearchStore((s) => s.recents);
  const addRecent = useQuickSearchStore((s) => s.addRecent);
  const clearRecents = useQuickSearchStore((s) => s.clearRecents);
  const showHints = useQuickSearchStore((s) => s.prefs.keyboardHints);
  const dimBackground = useQuickSearchStore((s) => s.prefs.dimBackground);
  const openBinding = useKeybindStore((s) => s.bindings.quickSearch);

  const navigate = useNavigate();
  const playTrack = usePlayerStore((s) => s.playTrack);

  const inputRef = useRef<HTMLInputElement>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const [draft, setDraft] = useState("");
  const [tokens, setTokens] = useState<Token[]>([]);
  const [focused, setFocused] = useState(-1); // index into tokens (pill focus)
  const [sel, setSel] = useState(0); // selection in command/tab/result lists
  const [nav, setNav] = useState(false); // focus is on a result row, not the input
  const [toast, setToast] = useState<string | null>(null);
  const [serverHost, setServerHost] = useState("the server");
  // Below Tailwind's `md` breakpoint (768px) we render the full-screen mobile
  // sheet instead of the centered desktop palette.
  const [isMobile, setIsMobile] = useState(
    typeof window !== "undefined" ? window.matchMedia("(max-width: 767px)").matches : false,
  );

  const mode = modeOf(draft);
  const isSearch = mode === "search";
  const isCommand = mode === "command";
  const isTab = mode === "tab";

  const modeColor = isCommand ? CMD_COLOR : isTab ? TAB_COLOR : ACCENT;
  const modeLabel = isCommand ? "COMMAND" : isTab ? "GO TO" : "SEARCH";

  const ghost = computeGhost(draft, COMMAND_NAMES, TAB_IDS);
  const derived = useMemo(() => deriveSearch(tokens, draft), [tokens, draft]);

  // ---- track the mobile breakpoint ----
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 767px)");
    const onChange = () => setIsMobile(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  // ---- server host (for the "stream from <host>" label) ----
  useEffect(() => {
    authServerConfig()
      .then((cfg) => {
        if (!cfg) return;
        try {
          setServerHost(new URL(cfg.rest_url).host || "the server");
        } catch {
          /* malformed URL — keep the fallback */
        }
      })
      .catch(() => {});
  }, []);

  // ---- reset + focus when opened ----
  useEffect(() => {
    if (!open) return;
    setDraft("");
    setTokens([]);
    setFocused(-1);
    setSel(0);
    setNav(false);
    const t = setTimeout(() => inputRef.current?.focus(), 0);
    return () => clearTimeout(t);
  }, [open]);

  // ---- debounced category queries ----
  const [dq, setDq] = useState<{
    q: Record<SearchCat, string>;
    active: Record<SearchCat, boolean>;
    terms: string[];
  }>({
    q: { artist: "", album: "", track: "", playlist: "", podcast: "" },
    active: { artist: false, album: false, track: false, playlist: false, podcast: false },
    terms: [],
  });

  useEffect(() => {
    const t = setTimeout(() => {
      const cats: SearchCat[] = ["artist", "album", "track", "playlist", "podcast"];
      const q = {} as Record<SearchCat, string>;
      const active = {} as Record<SearchCat, boolean>;
      for (const c of cats) {
        const on = isSearch && derived.cats.includes(c);
        active[c] = on;
        q[c] = on ? derived.queryFor(c) : "";
      }
      setDq({ q, active, terms: derived.plainTerms });
    }, 180);
    return () => clearTimeout(t);
  }, [derived, isSearch]);

  // ---- data ----
  const artistsQ = useQuery({
    queryKey: ["qs", "artists", dq.q.artist],
    queryFn: () => librarySearchArtists(dq.q.artist, { limit: 20 }),
    enabled: open && dq.active.artist && dq.q.artist.length > 0,
  });
  const albumsQ = useQuery({
    queryKey: ["qs", "albums", dq.q.album],
    queryFn: () => librarySearchAlbums(dq.q.album, { limit: 20 }),
    enabled: open && dq.active.album && dq.q.album.length > 0,
  });
  const tracksQ = useQuery({
    queryKey: ["qs", "tracks", dq.q.track],
    queryFn: () => librarySearchTracks(dq.q.track, { limit: 30 }),
    enabled: open && dq.active.track && dq.q.track.length > 0,
  });
  const playlistsQ = useQuery({
    queryKey: ["qs", "playlists"],
    queryFn: () => playlistList(),
    enabled: open && dq.active.playlist,
  });
  const podcastsQ = useQuery({
    queryKey: ["qs", "podcasts"],
    queryFn: () => podcastList(),
    enabled: open && dq.active.podcast,
  });

  // ---- assemble rows, split into device / server groups ----
  const { deviceRows, serverRows, anyLoading } = useMemo(() => {
    const device: Row[] = [];
    const server: Row[] = [];
    const terms = dq.terms;
    const passes = (text: string) => terms.every((t) => includesCI(text, t));
    const push = (row: Row) => (row.downloaded ? device : server).push(row);

    if (dq.active.artist) {
      for (const a of artistsQ.data?.items ?? []) {
        if (!passes(a.name)) continue;
        push({ cat: "artist", id: a.id, title: a.name, subtitle: "Artist", detail: "", downloaded: a.downloaded, data: a });
      }
    }
    if (dq.active.album) {
      for (const al of albumsQ.data?.items ?? []) {
        if (!passes(al.title)) continue;
        push({
          cat: "album",
          id: al.id,
          title: al.title,
          subtitle: al.release_year ? `Album · ${al.release_year}` : "Album",
          detail: "",
          downloaded: al.downloaded,
          data: al,
        });
      }
    }
    if (dq.active.track) {
      for (const tr of tracksQ.data?.items ?? []) {
        if (!passes(tr.title)) continue;
        push({
          cat: "track",
          id: tr.id,
          title: tr.title,
          subtitle: qualityLabel(tr),
          detail: formatDuration(tr.duration_ms),
          downloaded: tr.downloaded,
          data: tr,
        });
      }
    }
    if (dq.active.playlist) {
      for (const pl of playlistsQ.data?.items ?? []) {
        if (!passes(pl.name)) continue;
        // Playlists are local config — always grouped with on-device entries.
        device.push({ cat: "playlist", id: pl.id, title: pl.name, subtitle: "Playlist", detail: "", downloaded: true, data: pl });
      }
    }
    if (dq.active.podcast) {
      for (const pod of podcastsQ.data?.items ?? []) {
        if (!passes(`${pod.title} ${pod.author ?? ""}`)) continue;
        const onDevice = pod.downloaded_count > 0;
        const row: Row = {
          cat: "podcast",
          id: pod.id,
          title: pod.title,
          subtitle: pod.author ?? "Podcast",
          detail: onDevice ? `${pod.downloaded_count} ↓` : "",
          downloaded: onDevice,
          data: pod,
        };
        (onDevice ? device : server).push(row);
      }
    }

    const loading =
      (dq.active.artist && artistsQ.isLoading) ||
      (dq.active.album && albumsQ.isLoading) ||
      (dq.active.track && tracksQ.isLoading) ||
      (dq.active.playlist && playlistsQ.isLoading) ||
      (dq.active.podcast && podcastsQ.isLoading);

    return { deviceRows: device, serverRows: server, anyLoading: loading };
  }, [
    dq,
    artistsQ.data, artistsQ.isLoading,
    albumsQ.data, albumsQ.isLoading,
    tracksQ.data, tracksQ.isLoading,
    playlistsQ.data, playlistsQ.isLoading,
    podcastsQ.data, podcastsQ.isLoading,
  ]);

  const flat = useMemo(() => [...deviceRows, ...serverRows], [deviceRows, serverRows]);

  // command / tab lists (filtered by the draft after the prefix)
  const cmdList = useMemo(() => {
    const q = draft.slice(1).trim().toLowerCase();
    return q ? COMMANDS.filter((c) => c.name.toLowerCase().includes(q)) : COMMANDS;
  }, [draft]);
  const tabList = useMemo(() => {
    const q = draft.slice(1).trim().toLowerCase();
    if (!q) return TABS;
    return TABS.filter((t) => t.id.startsWith(q) || t.label.toLowerCase().includes(q)).sort(
      (a, b) => Number(b.id.startsWith(q)) - Number(a.id.startsWith(q)),
    );
  }, [draft]);

  // ---- actions ----
  function fire(msg: string) {
    setToast(msg);
    clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 2600);
  }

  function queryLabel(): string {
    const parts = tokens.map((t) => t.raw);
    const d = draft.trim();
    if (d && !isCommand && !isTab) parts.push(d);
    return parts.join(" ");
  }

  function acceptGhost() {
    if (ghost) setDraft(draft + ghost);
  }

  function confirmToken() {
    const d = draft.trim();
    if (!d) return;
    setTokens((ts) => [...ts, parseToken(d)]);
    setDraft("");
    setFocused(-1);
    setSel(0);
    setNav(false);
  }

  function editToken(i: number) {
    setTokens((ts) => {
      const next = ts.slice();
      const [t] = next.splice(i, 1);
      setDraft(t.raw);
      return next;
    });
    setFocused(-1);
    setSel(0);
    setTimeout(() => inputRef.current?.focus(), 0);
  }

  function removeToken(i: number) {
    setTokens((ts) => {
      const next = ts.slice();
      next.splice(i, 1);
      const nf = next.length ? Math.min(i, next.length - 1) : -1;
      setFocused(nf);
      if (nf < 0) setTimeout(() => inputRef.current?.focus(), 0);
      return next;
    });
  }

  function activateRow(row: Row) {
    if (row.cat === "track") {
      const queue = flat.filter((r) => r.cat === "track").map((r) => r.data as MergedTrack);
      playTrack(row.data as MergedTrack, queue);
      fire(`Playing “${row.title}”`);
    } else {
      const route =
        row.cat === "artist" ? `/artists/${row.id}`
        : row.cat === "album" ? `/albums/${row.id}`
        : row.cat === "playlist" ? `/playlists/${row.id}`
        : `/podcasts/${row.id}`;
      navigate(route);
    }
    addRecent(queryLabel());
    close();
  }

  function activateSelected() {
    if (!flat.length) return;
    activateRow(flat[Math.min(sel, flat.length - 1)]);
  }

  function runCommand(cmd?: Command) {
    const c = cmd ?? cmdList[Math.min(sel, cmdList.length - 1)];
    if (!c) return;
    c.run();
    addRecent(`> ${c.name}`);
    fire(`Command · ${c.name.replace(/…$/, "")}`);
    close();
  }

  function switchTab(tab?: Tab) {
    const t = tab ?? tabList[Math.min(sel, tabList.length - 1)];
    if (!t) return;
    navigate(t.to);
    addRecent(`!${t.id}`);
    close();
  }

  function moveSel(dir: number) {
    let len = 0;
    if (isCommand) len = cmdList.length;
    else if (isTab) len = tabList.length;
    else len = flat.length;
    if (!len) return;
    setSel((s) => (s + dir + len) % len);
  }

  function enterNav() {
    if (!flat.length) return;
    setNav(true);
    setSel(0);
  }

  function moveNav(dir: number) {
    if (!flat.length) {
      setNav(false);
      return;
    }
    let s = sel + dir;
    if (s < 0) {
      setNav(false);
      setSel(0);
      setTimeout(() => inputRef.current?.focus(), 0);
      return;
    }
    if (s > flat.length - 1) s = flat.length - 1;
    setSel(s);
  }

  function clickRecent(text: string) {
    setTokens([]);
    setDraft(text);
    setFocused(-1);
    setSel(0);
    setNav(false);
    setTimeout(() => inputRef.current?.focus(), 0);
  }

  function clickPrefix(label: string) {
    const d = label === "> command" ? ">" : label === "! go to" ? "!" : label;
    setDraft(d);
    setFocused(-1);
    setSel(0);
    setNav(false);
    setTimeout(() => inputRef.current?.focus(), 0);
  }

  // ---- keyboard ----
  function onKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    // Toggle-close via the bound open shortcut, even while typing.
    if (openBinding && eventMatches(e.nativeEvent, openBinding)) {
      e.preventDefault();
      close();
      return;
    }

    const input = e.currentTarget;

    if (e.key === "Escape") {
      e.preventDefault();
      if (nav) {
        setNav(false);
        inputRef.current?.focus();
        return;
      }
      if (focused >= 0) {
        setFocused(-1);
        inputRef.current?.focus();
        return;
      }
      if (draft !== "") {
        setDraft("");
        return;
      }
      close();
      return;
    }

    // ----- pill-focus mode -----
    if (focused >= 0) {
      if (e.key === "ArrowLeft") { e.preventDefault(); setFocused(Math.max(0, focused - 1)); return; }
      if (e.key === "ArrowRight") {
        e.preventDefault();
        if (focused >= tokens.length - 1) { setFocused(-1); inputRef.current?.focus(); }
        else setFocused(focused + 1);
        return;
      }
      if (e.key === "e" || e.key === "E") { e.preventDefault(); editToken(focused); return; }
      if (e.key === "Backspace" || e.key === "Delete") { e.preventDefault(); removeToken(focused); return; }
      if (e.key === "Enter") { e.preventDefault(); enterNav(); return; }
      if (e.key.length === 1) e.preventDefault();
      return;
    }

    // ----- result-navigation mode -----
    if (nav) {
      if (e.key === "ArrowDown") { e.preventDefault(); moveNav(1); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); moveNav(-1); return; }
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); return; }
      if (e.key === "Enter" || e.key === " ") { e.preventDefault(); activateSelected(); return; }
      if (e.key.length === 1) { setNav(false); return; }
      return;
    }

    // ----- normal input mode -----
    if (e.key === "ArrowLeft") {
      if (input.selectionStart === 0 && input.selectionEnd === 0 && tokens.length) {
        e.preventDefault(); setFocused(tokens.length - 1); return;
      }
    }
    if (e.key === "Backspace") {
      if (input.selectionStart === 0 && draft === "" && tokens.length) {
        e.preventDefault(); setFocused(tokens.length - 1); return;
      }
    }
    if (e.key === "Tab") { if (ghost) { e.preventDefault(); acceptGhost(); return; } }
    if (e.key === "ArrowRight") {
      if (input.selectionStart === draft.length && ghost) { e.preventDefault(); acceptGhost(); return; }
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (isCommand || isTab) { moveSel(1); return; }
      enterNav();
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      if (isCommand || isTab) moveSel(-1);
      return;
    }
    if (e.key === "|" && isSearch) { e.preventDefault(); confirmToken(); return; }
    if (e.key === "Enter") {
      e.preventDefault();
      if (isCommand) { runCommand(); return; }
      if (isTab) { switchTab(); return; }
      const d = draft.trim();
      if (d !== "") { confirmToken(); return; }
      enterNav();
      return;
    }
  }

  function onChange(e: React.ChangeEvent<HTMLInputElement>) {
    setDraft(e.target.value);
    setFocused(-1);
    setSel(0);
    setNav(false);
  }

  if (!open) return null;

  // ---- derived render flags ----
  const hasQuery = derived.hasQuery;
  const selC = flat.length ? Math.min(sel, flat.length - 1) : 0;
  const showDefault = isSearch && !hasQuery;
  const showNoResults =
    isSearch && hasQuery && !anyLoading && flat.length === 0;
  const escLabel = nav || focused >= 0 || draft.trim() !== "" ? "back" : "close";

  const placeholder = tokens.length
    ? "Add filter, > command, ! go to…"
    : "Search your library…   try  album:  artist:  song:  > or !";

  let footerHints: { key: string; label: string }[];
  if (focused >= 0) footerHints = [{ key: "E", label: "edit" }, { key: "⌫", label: "remove" }, { key: "←→", label: "move" }, { key: "↵", label: "search" }];
  else if (nav) footerHints = [{ key: "↵", label: "play / open" }, { key: "↑↓", label: "move" }, { key: "esc", label: "back" }];
  else if (isCommand) footerHints = [{ key: "↵", label: "run" }, { key: "↑↓", label: "select" }, { key: "⇥", label: "complete" }];
  else if (isTab) footerHints = [{ key: "↵", label: "go to" }, { key: "↑↓", label: "select" }, { key: "⇥", label: "complete" }];
  else if (draft.trim() !== "") footerHints = [{ key: "↵", label: "add filter" }, { key: "⇥", label: "complete" }, { key: "←", label: "pills" }];
  else footerHints = [{ key: "↵", label: "results" }, { key: "←", label: "edit pills" }, { key: "esc", label: "close" }];

  let globalIdx = 0; // running index across both result groups for nav highlight

  // =========================================================================
  // MOBILE — full-screen sheet (slides up). Touch-driven: chips carry an inline
  // remove button + tap-to-edit, suggestions sit above the OS keyboard. Sized to
  // the visual viewport (`--app-height`) so the strip stays above the keyboard.
  // =========================================================================
  if (isMobile) {
    const mb = hex2rgba(modeColor, 0.32);
    const modeHint = isCommand
      ? "Run an action"
      : isTab
        ? "Jump to a tab or page"
        : "Tap a chip to edit · > for commands · ! to jump";
    const mobilePlaceholder = tokens.length ? "Add filter…" : "Search your library…";

    type Sug = { label: string; icon?: string; color: string; bg: string; border: string; onTap: () => void };
    const suggestions: Sug[] = [];
    if (ghost) {
      suggestions.push({ label: draft + ghost, icon: "⇥", color: ACCENT, bg: hex2rgba(ACCENT, 0.1), border: mb, onTap: acceptGhost });
    }
    if (isCommand) {
      cmdList.slice(0, 6).forEach((c) =>
        suggestions.push({ label: c.name.replace(/…$/, ""), color: "#c7cbe8", bg: hex2rgba(modeColor, 0.08), border: mb, onTap: () => runCommand(c) }),
      );
    } else if (isTab) {
      tabList.slice(0, 6).forEach((t) =>
        suggestions.push({ label: `!${t.id}`, color: "#bfe3da", bg: hex2rgba(modeColor, 0.08), border: mb, onTap: () => switchTab(t) }),
      );
    } else {
      if (draft.trim() !== "") {
        suggestions.push({ label: "Confirm filter", icon: "↵", color: ACCENT, bg: hex2rgba(ACCENT, 0.1), border: mb, onTap: confirmToken });
      }
      PREFIXES.forEach((p) =>
        suggestions.push({ label: `${p}:`, color: ACCENT, bg: "#16181c", border: "#23262b", onTap: () => clickPrefix(`${p}:`) }),
      );
    }

    return (
      <div
        className="fixed inset-x-0 top-0 z-[80] flex animate-qssheet flex-col bg-oct-bg font-sans text-oct-text"
        style={{ height: "var(--app-height, 100%)", paddingTop: "env(safe-area-inset-top)" }}
      >
        {/* header */}
        <div className="flex-none border-b border-oct-border bg-oct-surface px-3 py-2.5">
          <div className="flex items-center gap-2.5">
            <button onClick={close} aria-label="Back" className="grid h-9 w-9 shrink-0 place-items-center rounded-lg text-oct-muted active:bg-oct-elevated">
              <ChevronLeftIcon size={19} />
            </button>

            {/* search field */}
            <div
              onMouseDown={(e) => {
                if ((e.target as HTMLElement).tagName !== "INPUT") {
                  e.preventDefault();
                  inputRef.current?.focus();
                }
              }}
              className="flex min-h-[44px] min-w-0 flex-1 items-center gap-2.5 rounded-xl border bg-oct-elevated px-3 py-2"
              style={{ borderColor: hex2rgba(modeColor, 0.45) }}
            >
              <span className="grid h-[18px] w-[18px] shrink-0 place-items-center">
                {isSearch ? (
                  <SearchIcon size={16} sw={1.5} style={{ color: modeColor }} />
                ) : (
                  <span className="font-mono text-[16px] font-bold leading-none" style={{ color: modeColor }}>
                    {isCommand ? ">" : "!"}
                  </span>
                )}
              </span>

              <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
                {tokens.map((t, i) => (
                  <MobilePill key={i} token={t} onEdit={() => editToken(i)} onRemove={() => removeToken(i)} />
                ))}
                <div className="relative flex h-[22px] min-w-[90px] flex-1 items-center">
                  <div aria-hidden className="pointer-events-none absolute left-0 top-0 flex h-[22px] items-center whitespace-pre font-mono text-[14px]">
                    <span className="invisible">{draft}</span>
                    <span className="text-oct-faint">{ghost}</span>
                  </div>
                  <input
                    ref={inputRef}
                    value={draft}
                    onChange={onChange}
                    onKeyDown={onKeyDown}
                    placeholder={mobilePlaceholder}
                    spellCheck={false}
                    autoComplete="off"
                    autoCapitalize="off"
                    className="relative h-[22px] min-w-0 flex-1 border-none bg-transparent p-0 font-mono text-[14px] text-oct-text outline-none placeholder:text-oct-faint"
                    style={{ caretColor: ACCENT }}
                  />
                </div>
              </div>

              {draft.length > 0 && (
                <button
                  onMouseDown={(e) => { e.preventDefault(); setDraft(""); inputRef.current?.focus(); }}
                  aria-label="Clear"
                  className="grid h-5 w-5 shrink-0 place-items-center rounded-full bg-oct-line text-oct-muted"
                >
                  <XIcon size={10} />
                </button>
              )}
            </div>

            <button onClick={close} className="shrink-0 px-1 py-1.5 text-[14px]" style={{ color: modeColor }}>
              Cancel
            </button>
          </div>

          {/* mode hint */}
          <div className="mt-2 flex items-center gap-2 px-0.5">
            <span
              className="rounded-md border px-1.5 py-0.5 font-mono text-[9px] tracking-[0.14em]"
              style={{ color: modeColor, background: hex2rgba(modeColor, 0.1), borderColor: hex2rgba(modeColor, 0.32) }}
            >
              {modeLabel}
            </span>
            <span className="min-w-0 flex-1 truncate text-[11px] text-oct-subtle">{modeHint}</span>
          </div>
        </div>

        {/* body */}
        <div className="qs-scroll min-h-0 flex-1 overflow-y-auto px-2 py-1.5">
          {/* DEFAULT STATE */}
          {showDefault && (
            <div className="p-1.5">
              {recents.length > 0 && (
                <>
                  <div className="flex items-center justify-between px-1 pb-2 pt-2">
                    <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">RECENT</span>
                    <button onMouseDown={(e) => { e.preventDefault(); clearRecents(); }} className="font-mono text-[10px] tracking-[0.08em] text-oct-faint active:text-oct-muted">
                      CLEAR
                    </button>
                  </div>
                  {recents.map((r) => (
                    <button
                      key={r}
                      onMouseDown={(e) => { e.preventDefault(); clickRecent(r); }}
                      className="flex w-full items-center gap-3 rounded-[10px] px-1.5 py-2.5 text-left active:bg-oct-elevated"
                    >
                      <ClockIcon size={15} className="shrink-0 text-oct-subtle" />
                      <span className="min-w-0 flex-1 truncate font-mono text-[13px] text-oct-muted">{r}</span>
                      <ChevronRightIcon size={13} className="shrink-0 text-oct-line" />
                    </button>
                  ))}
                </>
              )}

              <div className="px-1 pb-2.5 pt-4 font-mono text-[10px] tracking-[0.16em] text-oct-faint">FILTER BY</div>
              <div className="flex flex-wrap gap-2 px-1 pb-2">
                {[
                  ...PREFIXES.map((p) => ({ label: `${p}:`, color: ACCENT })),
                  { label: "> command", color: CMD_COLOR },
                  { label: "! go to", color: TAB_COLOR },
                ].map((p) => (
                  <button
                    key={p.label}
                    onMouseDown={(e) => { e.preventDefault(); clickPrefix(p.label); }}
                    className="rounded-[9px] border border-oct-border-strong bg-oct-elevated px-3 py-2 font-mono text-[13px] active:bg-oct-elevated2"
                    style={{ color: p.color }}
                  >
                    {p.label}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* SEARCH RESULTS */}
          {isSearch && hasQuery && (
            <>
              {deviceRows.length > 0 && (
                <>
                  <MobileGroupHeader
                    icon={<span className="h-1.5 w-1.5 rounded-full bg-oct-online" style={{ boxShadow: "0 0 0 3px rgba(63,185,80,0.14)" }} />}
                    label="ON THIS DEVICE"
                    color="#3fb950"
                    count={`${deviceRows.length} · live`}
                  />
                  {deviceRows.map((row) => (
                    <MobileResultRow key={`d-${row.cat}-${row.id}`} row={row} server={false} onActivate={() => activateRow(row)} />
                  ))}
                </>
              )}

              {serverRows.length > 0 && (
                <>
                  <MobileGroupHeader
                    icon={<CloudIcon size={13} style={{ color: "#6f9bd1" }} />}
                    label={`STREAM · ${serverHost.toUpperCase()}`}
                    color="#6f9bd1"
                    count={`${serverRows.length} found`}
                  />
                  {serverRows.map((row) => (
                    <MobileResultRow key={`s-${row.cat}-${row.id}`} row={row} server onActivate={() => activateRow(row)} />
                  ))}
                </>
              )}

              {anyLoading && flat.length === 0 && (
                <div className="px-3 py-10 text-center font-mono text-[12px] text-oct-faint">Searching…</div>
              )}

              {showNoResults && (
                <div className="flex flex-col items-center gap-2.5 px-6 pb-8 pt-10 text-center">
                  <SearchIcon size={26} sw={1.3} className="text-oct-line" />
                  <div className="text-[14px] text-oct-muted">No matches</div>
                  <div className="font-mono text-[11px] leading-relaxed text-oct-faint">Adjust your filters or try a different term</div>
                </div>
              )}
            </>
          )}

          {/* COMMANDS */}
          {isCommand && (
            <>
              <div className="px-2 pb-2 pt-2.5 font-mono text-[10px] tracking-[0.12em]" style={{ color: modeColor }}>COMMANDS</div>
              {cmdList.length === 0 ? (
                <Empty text="No matching command" />
              ) : (
                cmdList.map((c) => (
                  <MobileActionRow
                    key={c.name}
                    color={modeColor}
                    glyph={<span className="font-mono text-[15px] font-bold">&gt;</span>}
                    title={c.name}
                    subtitle={c.desc}
                    onActivate={() => runCommand(c)}
                  />
                ))
              )}
            </>
          )}

          {/* GO TO */}
          {isTab && (
            <>
              <div className="px-2 pb-2 pt-2.5 font-mono text-[10px] tracking-[0.12em]" style={{ color: modeColor }}>GO TO</div>
              {tabList.length === 0 ? (
                <Empty text="No matching destination" />
              ) : (
                tabList.map((t) => {
                  const Icon = TAB_ICON[t.id] ?? DiscIcon;
                  return (
                    <MobileActionRow
                      key={t.id}
                      color={modeColor}
                      glyph={<Icon size={17} />}
                      title={t.label}
                      tag={`!${t.id}`}
                      subtitle={t.desc}
                      onActivate={() => switchTab(t)}
                    />
                  );
                })
              )}
            </>
          )}
        </div>

        {/* suggestion strip (sits above the OS keyboard) */}
        {suggestions.length > 0 && (
          <div
            className="no-scrollbar flex flex-none items-center gap-2 overflow-x-auto border-t border-oct-border bg-oct-surface px-2.5 py-2"
            style={{ paddingBottom: "calc(env(safe-area-inset-bottom) + 0.5rem)" }}
          >
            {suggestions.map((s, i) => (
              <button
                key={i}
                onMouseDown={(e) => { e.preventDefault(); s.onTap(); }}
                className="flex flex-none items-center gap-1.5 rounded-[9px] border px-3 py-2 font-mono text-[13px] active:opacity-80"
                style={{ color: s.color, background: s.bg, borderColor: s.border }}
              >
                {s.icon && <span className="text-[11px] opacity-80">{s.icon}</span>}
                {s.label}
              </button>
            ))}
          </div>
        )}

        {/* toast */}
        {toast && (
          <div className="pointer-events-none fixed bottom-10 left-1/2 flex -translate-x-1/2 items-center gap-2.5 rounded-xl border border-oct-line bg-oct-elevated px-4 py-2.5 shadow-[0_16px_40px_-12px_rgba(0,0,0,0.6)]">
            <span className="h-1.5 w-1.5 rounded-full bg-oct-accent" style={{ boxShadow: `0 0 0 3px ${hex2rgba(ACCENT, 0.18)}` }} />
            <span className="text-[13px] text-oct-text">{toast}</span>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="fixed inset-0 z-[80] font-sans text-oct-text">
      {/* scrim */}
      <div
        onMouseDown={() => close()}
        className={`absolute inset-0 animate-qsfade bg-black/55 ${dimBackground ? "backdrop-blur-[2px]" : ""}`}
      />

      {/* palette — centered via flex so the entrance transform can't fight the
          horizontal centering (a translateX-based center flashes off-axis). The
          wrapper is click-through so taps beside the panel still hit the scrim. */}
      <div className="pointer-events-none absolute inset-x-0 top-[13%] flex justify-center px-4">
        <div className="pointer-events-auto w-[680px] max-w-full animate-qspop overflow-hidden rounded-2xl border border-oct-line bg-oct-panel shadow-[0_30px_90px_-20px_rgba(0,0,0,0.75)]">
          {/* input bar */}
          <div
            onMouseDown={(e) => {
              if ((e.target as HTMLElement).tagName !== "INPUT") {
                e.preventDefault();
                setFocused(-1);
                inputRef.current?.focus();
              }
            }}
            className="flex cursor-text items-center gap-3 px-4.5 py-4"
          >
            {/* leading glyph */}
            <span className="grid h-5 w-5 shrink-0 place-items-center">
              {isSearch ? (
                <SearchIcon size={18} sw={1.5} style={{ color: ACCENT }} />
              ) : (
                <span className="font-mono text-[17px] font-bold leading-none" style={{ color: modeColor }}>
                  {isCommand ? ">" : "!"}
                </span>
              )}
            </span>

            {/* pills + input */}
            <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
              {tokens.map((t, i) => (
                <Pill
                  key={i}
                  token={t}
                  focused={i === focused}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    if (focused === i) editToken(i);
                    else setFocused(i);
                  }}
                />
              ))}

              <div className={`relative flex h-6 min-w-[140px] flex-1 items-center ${nav ? "opacity-50" : ""}`}>
                <div
                  aria-hidden
                  className="pointer-events-none absolute left-0 top-0 flex h-6 items-center whitespace-pre font-mono text-[14.5px]"
                >
                  <span className="invisible">{draft}</span>
                  <span className="text-oct-faint">{ghost}</span>
                </div>
                <input
                  ref={inputRef}
                  value={draft}
                  onChange={onChange}
                  onKeyDown={onKeyDown}
                  placeholder={placeholder}
                  spellCheck={false}
                  autoComplete="off"
                  className="relative h-6 min-w-0 flex-1 border-none bg-transparent p-0 font-mono text-[14.5px] text-oct-text outline-none placeholder:text-oct-faint"
                  style={{ caretColor: nav ? "transparent" : ACCENT }}
                />
              </div>
            </div>

            {/* mode pill */}
            <span
              className="shrink-0 rounded-md border px-2 py-1 font-mono text-[9.5px] tracking-[0.16em]"
              style={{ color: modeColor, background: hex2rgba(modeColor, 0.1), borderColor: hex2rgba(modeColor, 0.32) }}
            >
              {modeLabel}
            </span>
          </div>

          <div className="h-px bg-oct-border-strong" />

          {/* body */}
          <div className="qs-scroll max-h-[430px] overflow-y-auto p-2">
            {/* DEFAULT STATE */}
            {showDefault && (
              <div className="p-2">
                {recents.length > 0 && (
                  <>
                    <div className="flex items-center justify-between px-1.5 pb-2.5 pt-1.5">
                      <span className="font-mono text-[10px] tracking-[0.16em] text-oct-faint">RECENT</span>
                      <button
                        onMouseDown={(e) => { e.preventDefault(); clearRecents(); }}
                        className="font-mono text-[10px] tracking-[0.08em] text-oct-faint hover:text-oct-muted"
                      >
                        CLEAR
                      </button>
                    </div>
                    {recents.map((r) => (
                      <button
                        key={r}
                        onMouseDown={(e) => { e.preventDefault(); clickRecent(r); }}
                        className="flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left hover:bg-oct-elevated"
                      >
                        <ClockIcon size={14} className="shrink-0 text-oct-subtle" />
                        <span className="truncate font-mono text-[13px] text-oct-muted">{r}</span>
                      </button>
                    ))}
                  </>
                )}

                <div className="px-1.5 pb-2.5 pt-4 font-mono text-[10px] tracking-[0.16em] text-oct-faint">FILTER BY</div>
                <div className="flex flex-wrap gap-2 px-1.5 pb-1.5">
                  {[
                    ...PREFIXES.map((p) => ({ label: `${p}:`, color: ACCENT })),
                    { label: "> command", color: CMD_COLOR },
                    { label: "! go to", color: TAB_COLOR },
                  ].map((p) => (
                    <button
                      key={p.label}
                      onMouseDown={(e) => { e.preventDefault(); clickPrefix(p.label); }}
                      className="rounded-lg border border-oct-border-strong bg-oct-elevated px-2.5 py-1.5 font-mono text-[12px] hover:border-oct-line"
                      style={{ color: p.color }}
                    >
                      {p.label}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* SEARCH RESULTS */}
            {isSearch && hasQuery && (
              <>
                {deviceRows.length > 0 && (
                  <>
                    <GroupHeader
                      icon={<span className="h-1.5 w-1.5 rounded-full bg-oct-online" style={{ boxShadow: "0 0 0 3px rgba(63,185,80,0.14)" }} />}
                      label="ON THIS DEVICE"
                      color="#3fb950"
                      count={`${deviceRows.length} · live`}
                    />
                    {deviceRows.map((row) => {
                      const idx = globalIdx++;
                      return <ResultRow key={`d-${row.cat}-${row.id}`} row={row} active={nav && idx === selC} onActivate={() => activateRow(row)} server={false} />;
                    })}
                  </>
                )}

                {serverRows.length > 0 && (
                  <>
                    <GroupHeader
                      icon={<CloudIcon size={13} style={{ color: "#6f9bd1" }} />}
                      label={`STREAM FROM ${serverHost.toUpperCase()}`}
                      color="#6f9bd1"
                      count={`${serverRows.length} found`}
                    />
                    {serverRows.map((row) => {
                      const idx = globalIdx++;
                      return <ResultRow key={`s-${row.cat}-${row.id}`} row={row} active={nav && idx === selC} onActivate={() => activateRow(row)} server />;
                    })}
                  </>
                )}

                {anyLoading && flat.length === 0 && (
                  <div className="px-3 py-8 text-center font-mono text-[12px] text-oct-faint">Searching…</div>
                )}

                {showNoResults && (
                  <div className="flex flex-col items-center gap-2.5 px-5 pb-10 pt-9 text-center">
                    <SearchIcon size={26} sw={1.3} className="text-oct-line" />
                    <div className="text-[13.5px] text-oct-muted">No matches for this query</div>
                    <div className="font-mono text-[11px] text-oct-faint">Adjust your filters or try a different term</div>
                  </div>
                )}
              </>
            )}

            {/* COMMANDS */}
            {isCommand && (
              <>
                <div className="px-3 pb-2 pt-2.5 font-mono text-[10px] tracking-[0.14em]" style={{ color: modeColor }}>COMMANDS</div>
                {cmdList.length === 0 ? (
                  <Empty text="No matching command" />
                ) : (
                  cmdList.map((c, i) => (
                    <ActionRow
                      key={c.name}
                      active={i === sel}
                      color={modeColor}
                      glyph={<span className="font-mono text-[13px] font-bold">&gt;</span>}
                      title={c.name}
                      subtitle={c.desc}
                      badge="RUN ↵"
                      onActivate={() => { setSel(i); runCommand(); }}
                    />
                  ))
                )}
              </>
            )}

            {/* GO TO */}
            {isTab && (
              <>
                <div className="px-3 pb-2 pt-2.5 font-mono text-[10px] tracking-[0.14em]" style={{ color: modeColor }}>GO TO</div>
                {tabList.length === 0 ? (
                  <Empty text="No matching destination" />
                ) : (
                  tabList.map((t, i) => {
                    const Icon = TAB_ICON[t.id] ?? DiscIcon;
                    return (
                      <ActionRow
                        key={t.id}
                        active={i === sel}
                        color={modeColor}
                        glyph={<Icon size={15} />}
                        title={t.label}
                        tag={`!${t.id}`}
                        subtitle={t.desc}
                        badge="GO ↵"
                        onActivate={() => { setSel(i); switchTab(); }}
                      />
                    );
                  })
                )}
              </>
            )}
          </div>

          {/* footer */}
          {showHints && (
            <div className="flex flex-wrap items-center gap-4 border-t border-oct-border-strong bg-oct-surface px-4 py-2.5">
              {footerHints.map((h) => (
                <span key={h.key + h.label} className="flex items-center gap-1.5 whitespace-nowrap text-[11px] text-oct-subtle">
                  <Kbd>{h.key}</Kbd>
                  {h.label}
                </span>
              ))}
              <span className="flex-1" />
              <span className="flex items-center gap-1.5 whitespace-nowrap text-[11px] text-oct-subtle">
                <Kbd>esc</Kbd>
                {escLabel}
              </span>
            </div>
          )}
        </div>
      </div>

      {/* toast */}
      {toast && (
        <div className="absolute bottom-8 left-1/2 flex -translate-x-1/2 items-center gap-2.5 rounded-xl border border-oct-line bg-oct-elevated px-4 py-2.5 shadow-[0_16px_40px_-12px_rgba(0,0,0,0.6)]">
          <span className="h-1.5 w-1.5 rounded-full bg-oct-accent" style={{ boxShadow: `0 0 0 3px ${hex2rgba(ACCENT, 0.18)}` }} />
          <span className="text-[13px] text-oct-text">{toast}</span>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// sub-components
// ---------------------------------------------------------------------------

function Pill({ token, focused, onMouseDown }: { token: Token; focused: boolean; onMouseDown: (e: React.MouseEvent) => void }) {
  const segs: { text: string; color: string; weight: number }[] = [];
  if (!token.field) {
    segs.push({ text: token.value, color: "#d7d9dd", weight: 500 });
  } else {
    segs.push({ text: token.field + ":", color: ACCENT, weight: 700 });
    token.value.split(",").forEach((p, i) => {
      if (i > 0) segs.push({ text: ",", color: "#73777e", weight: 400 });
      if (p[0] === "-") segs.push({ text: p, color: "#d98a6a", weight: 600 });
      else segs.push({ text: p, color: "#d7d9dd", weight: 500 });
    });
  }
  return (
    <div
      onMouseDown={onMouseDown}
      className="flex cursor-pointer items-center gap-px whitespace-nowrap rounded-lg border px-2 py-1 font-mono text-[12.5px] leading-none"
      style={{
        background: focused ? hex2rgba(ACCENT, 0.16) : "#1a1c20",
        borderColor: focused ? ACCENT : "#23262b",
        boxShadow: focused ? `0 0 0 3px ${hex2rgba(ACCENT, 0.12)}` : "none",
      }}
    >
      {segs.map((s, i) => (
        <span key={i} style={{ color: s.color, fontWeight: s.weight }}>{s.text}</span>
      ))}
      {focused && <span className="ml-1.5 font-mono text-[8.5px] tracking-[0.08em]" style={{ color: ACCENT, opacity: 0.85 }}>e</span>}
    </div>
  );
}

function GroupHeader({ icon, label, color, count }: { icon: React.ReactNode; label: string; color: string; count: string }) {
  return (
    <div className="flex items-center justify-between px-3 pb-2 pt-3">
      <span className="flex items-center gap-2 font-mono text-[10px] tracking-[0.14em]" style={{ color }}>
        {icon}
        {label}
      </span>
      <span className="font-mono text-[10px] text-oct-faint">{count}</span>
    </div>
  );
}

function ResultRow({ row, active, onActivate, server }: { row: Row; active: boolean; onActivate: () => void; server: boolean }) {
  const Icon = CAT_ICON[row.cat];
  const isPlay = row.cat === "track";
  return (
    <div
      onMouseDown={(e) => { e.preventDefault(); onActivate(); }}
      className="flex cursor-pointer items-center gap-3 rounded-[10px] px-3 py-2 hover:bg-oct-elevated"
      style={active ? { background: hex2rgba(ACCENT, 0.1), boxShadow: `0 0 0 1px ${hex2rgba(ACCENT, 0.5)}` } : undefined}
    >
      <span
        className="grid h-8 w-8 shrink-0 place-items-center rounded-lg border border-oct-border-strong"
        style={{ background: server ? "#15161a" : hex2rgba(ACCENT, 0.13) }}
      >
        <Icon size={15} style={{ color: server && !active ? "#73777e" : ACCENT }} />
      </span>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13.5px] font-medium">{row.title}</div>
        {row.subtitle && <div className="truncate text-[11.5px] text-oct-subtle">{row.subtitle}</div>}
      </div>
      <span className="flex shrink-0 items-center gap-2">
        {active && (
          <span className="flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-[9px] tracking-[0.08em]" style={{ color: ACCENT, background: hex2rgba(ACCENT, 0.1), borderColor: hex2rgba(ACCENT, 0.4) }}>
            ↵ {isPlay ? "Play" : "Open"}
          </span>
        )}
        {server ? (
          <span className="flex items-center gap-1 rounded-md px-1.5 py-0.5 font-mono text-[9px] tracking-[0.06em]" style={{ color: "#6f9bd1", background: "rgba(111,155,209,0.1)" }}>
            <CloudIcon size={9} sw={1.6} />STREAM
          </span>
        ) : (
          <span className="flex items-center gap-1 rounded-md px-1.5 py-0.5 font-mono text-[9px] tracking-[0.06em]" style={{ color: "#3fb950", background: "rgba(63,185,80,0.12)" }}>
            <span className="h-1 w-1 rounded-full bg-oct-online" />ON DEVICE
          </span>
        )}
        {row.detail && <span className="min-w-[42px] text-right font-mono text-[9.5px] text-oct-subtle">{row.detail}</span>}
      </span>
    </div>
  );
}

function ActionRow({
  active, color, glyph, title, subtitle, tag, badge, onActivate,
}: {
  active: boolean; color: string; glyph: React.ReactNode; title: string; subtitle: string; tag?: string; badge: string; onActivate: () => void;
}) {
  return (
    <div
      onMouseDown={(e) => { e.preventDefault(); onActivate(); }}
      className="flex cursor-pointer items-center gap-3 rounded-[10px] px-3 py-2 hover:bg-oct-elevated"
      style={active ? { background: hex2rgba(color, 0.1), boxShadow: `0 0 0 1px ${hex2rgba(color, 0.32)}` } : undefined}
    >
      <span
        className="grid h-8 w-8 shrink-0 place-items-center rounded-lg border border-oct-border-strong"
        style={{ background: active ? hex2rgba(color, 0.14) : "#15161a", color: active ? color : "#73777e" }}
      >
        {glyph}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-[13.5px] font-medium">{title}</span>
          {tag && <span className="font-mono text-[11px]" style={{ color: active ? color : "#6b6f76" }}>{tag}</span>}
        </div>
        <div className="truncate text-[11.5px] text-oct-subtle">{subtitle}</div>
      </div>
      {active && (
        <span className="shrink-0 rounded-md border px-1.5 py-0.5 font-mono text-[9.5px] tracking-[0.1em]" style={{ color, borderColor: hex2rgba(color, 0.32) }}>
          {badge}
        </span>
      )}
    </div>
  );
}

function Empty({ text }: { text: string }) {
  return <div className="px-3 py-8 text-center text-[13px] text-oct-muted">{text}</div>;
}

function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <span className="min-w-[18px] rounded-md border border-oct-border-strong bg-oct-elevated px-1.5 py-0.5 text-center font-mono text-[10px] text-oct-muted">
      {children}
    </span>
  );
}

function ClockIcon({ size = 16, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" strokeLinejoin="round" className={className}>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 4v4l2.5 1.5" />
    </svg>
  );
}

function ChevronLeftIcon({ size = 16, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" className={className}>
      <path d="M9.5 3.5 5 8l4.5 4.5" />
    </svg>
  );
}

function ChevronRightIcon({ size = 16, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" className={className}>
      <path d="M6 3.5 10.5 8 6 12.5" />
    </svg>
  );
}

function XIcon({ size = 16, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" className={className}>
      <path d="M3.5 3.5 12.5 12.5M12.5 3.5 3.5 12.5" />
    </svg>
  );
}

// ---- mobile sub-components ----

function MobilePill({ token, onEdit, onRemove }: { token: Token; onEdit: () => void; onRemove: () => void }) {
  const segs: { text: string; color: string; weight: number }[] = [];
  if (!token.field) {
    segs.push({ text: token.value, color: "#d7d9dd", weight: 500 });
  } else {
    segs.push({ text: token.field + ":", color: ACCENT, weight: 700 });
    token.value.split(",").forEach((p, i) => {
      if (i > 0) segs.push({ text: ",", color: "#73777e", weight: 400 });
      if (p[0] === "-") segs.push({ text: p, color: "#d98a6a", weight: 600 });
      else segs.push({ text: p, color: "#d7d9dd", weight: 500 });
    });
  }
  return (
    <span
      className="inline-flex items-center gap-1.5 whitespace-nowrap rounded-lg border py-1 pl-2.5 pr-1.5 font-mono text-[12px] leading-none"
      style={{ background: hex2rgba(ACCENT, 0.16), borderColor: ACCENT }}
    >
      <span onMouseDown={(e) => { e.preventDefault(); onEdit(); }} className="inline-flex cursor-pointer items-center">
        {segs.map((s, i) => (
          <span key={i} style={{ color: s.color, fontWeight: s.weight }}>{s.text}</span>
        ))}
      </span>
      <span
        onMouseDown={(e) => { e.preventDefault(); onRemove(); }}
        className="grid h-[15px] w-[15px] shrink-0 place-items-center rounded-[5px] text-oct-muted active:bg-white/10"
        aria-label="Remove filter"
      >
        <XIcon size={9} />
      </span>
    </span>
  );
}

function MobileGroupHeader({ icon, label, color, count }: { icon: React.ReactNode; label: string; color: string; count: string }) {
  return (
    <div className="flex items-center justify-between px-2 pb-2 pt-3">
      <span className="flex items-center gap-2 font-mono text-[10px] tracking-[0.12em]" style={{ color }}>
        {icon}
        {label}
      </span>
      <span className="font-mono text-[10px] text-oct-faint">{count}</span>
    </div>
  );
}

function MobileResultRow({ row, server, onActivate }: { row: Row; server: boolean; onActivate: () => void }) {
  const Icon = CAT_ICON[row.cat];
  return (
    <div
      onMouseDown={(e) => { e.preventDefault(); onActivate(); }}
      className="flex cursor-pointer items-center gap-3 rounded-[11px] px-2 py-2.5 active:bg-oct-elevated"
    >
      <span
        className="grid h-[38px] w-[38px] shrink-0 place-items-center rounded-[9px] border border-oct-border-strong"
        style={{ background: server ? "#15161a" : hex2rgba(ACCENT, 0.13) }}
      >
        <Icon size={17} style={{ color: server ? "#73777e" : ACCENT }} />
      </span>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[14px] font-medium">{row.title}</div>
        {row.subtitle && <div className="truncate text-[12px] text-oct-subtle">{row.subtitle}</div>}
      </div>
      <span className="flex shrink-0 flex-col items-end gap-1.5">
        {server ? (
          <span className="flex items-center gap-1 rounded-md px-1.5 py-0.5 font-mono text-[8.5px] tracking-[0.04em]" style={{ color: "#6f9bd1", background: "rgba(111,155,209,0.1)" }}>
            <CloudIcon size={8} sw={1.6} />STREAM
          </span>
        ) : (
          <span className="flex items-center gap-1 rounded-md px-1.5 py-0.5 font-mono text-[8.5px] tracking-[0.04em]" style={{ color: "#3fb950", background: "rgba(63,185,80,0.12)" }}>
            <span className="h-1 w-1 rounded-full bg-oct-online" />ON DEVICE
          </span>
        )}
        {row.detail && <span className="font-mono text-[9px] text-oct-subtle">{row.detail}</span>}
      </span>
    </div>
  );
}

function MobileActionRow({
  color, glyph, title, subtitle, tag, onActivate,
}: {
  color: string; glyph: React.ReactNode; title: string; subtitle: string; tag?: string; onActivate: () => void;
}) {
  return (
    <div
      onMouseDown={(e) => { e.preventDefault(); onActivate(); }}
      className="flex cursor-pointer items-center gap-3 rounded-[11px] px-2 py-2.5 active:bg-oct-elevated"
    >
      <span className="grid h-[38px] w-[38px] shrink-0 place-items-center rounded-[9px] border border-oct-border-strong bg-[#15161a]" style={{ color }}>
        {glyph}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-[14px] font-medium">{title}</span>
          {tag && <span className="font-mono text-[11px]" style={{ color }}>{tag}</span>}
        </div>
        <div className="truncate text-[12px] text-oct-subtle">{subtitle}</div>
      </div>
      <ChevronRightIcon size={14} className="shrink-0 text-oct-line" />
    </div>
  );
}
