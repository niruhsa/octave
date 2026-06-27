// Keyboard shortcut definitions + the binding store.
//
// A "command" is an app action that can be triggered by a key combo (play,
// next, jump to a tab, sync, …). Each command can be bound to at most one
// combo. Bindings persist to localStorage so they survive relaunches.
//
// Scope: a binding is either "local" (fires while the OCTAVE window is focused —
// fully implemented here via a window keydown listener) or "global" (system-wide,
// fires even when another app is focused). System-wide capture needs an OS hook
// that isn't wired yet; a "global" binding currently behaves like a local one and
// the settings UI flags the system-wide part as forthcoming. The scope is stored
// now so nothing has to be re-bound once OS capture lands.

import { create } from "zustand";

export type CommandId =
  // playback
  | "play"
  | "prev"
  | "next"
  | "volumeUp"
  | "volumeDown"
  | "toggleShuffle"
  | "toggleRepeat"
  // navigation / app
  | "search"
  | "home"
  | "library"
  | "sync"
  | "upload"
  | "uploadReports"
  | "account";

export type CommandGroup = "Playback" | "Navigation";

export type CommandDef = {
  id: CommandId;
  label: string;
  description: string;
  group: CommandGroup;
};

/** Ordered command catalog — drives the settings UI and the dispatcher. */
export const COMMANDS: CommandDef[] = [
  { id: "play", label: "Play / Pause", description: "Toggle playback", group: "Playback" },
  { id: "prev", label: "Previous", description: "Previous track (or restart)", group: "Playback" },
  { id: "next", label: "Next", description: "Skip to next track", group: "Playback" },
  { id: "volumeUp", label: "Volume up", description: "Raise volume", group: "Playback" },
  { id: "volumeDown", label: "Volume down", description: "Lower volume", group: "Playback" },
  { id: "toggleShuffle", label: "Toggle shuffle", description: "Shuffle on / off", group: "Playback" },
  { id: "toggleRepeat", label: "Toggle repeat", description: "Cycle repeat mode", group: "Playback" },
  { id: "search", label: "Search", description: "Open the Search tab", group: "Navigation" },
  { id: "home", label: "Home tab", description: "Open the Home tab", group: "Navigation" },
  { id: "library", label: "Library tab", description: "Open the Library tab", group: "Navigation" },
  { id: "sync", label: "Sync now", description: "Reconcile with the server", group: "Navigation" },
  { id: "upload", label: "Upload", description: "Open the Upload tab", group: "Navigation" },
  { id: "uploadReports", label: "Upload reports", description: "Open the Upload reports tab", group: "Navigation" },
  { id: "account", label: "Account", description: "Open the Account tab", group: "Navigation" },
];

export type BindScope = "local" | "global";

/** A single key combo. `code` is a `KeyboardEvent.code` (layout-independent). */
export type Binding = {
  code: string;
  shift: boolean;
  ctrl: boolean;
  alt: boolean;
  meta: boolean;
  scope: BindScope;
};

/** Map of command → its binding (absent = unbound). */
export type Bindings = Partial<Record<CommandId, Binding>>;

function b(code: string, mods: Partial<Omit<Binding, "code" | "scope">> = {}): Binding {
  return {
    code,
    shift: !!mods.shift,
    ctrl: !!mods.ctrl,
    alt: !!mods.alt,
    meta: !!mods.meta,
    scope: "local",
  };
}

/** Factory-default bindings (all local). */
export const DEFAULT_BINDINGS: Bindings = {
  play: b("Space"),
  prev: b("ArrowLeft", { shift: true }),
  next: b("ArrowRight", { shift: true }),
  volumeUp: b("ArrowUp"),
  volumeDown: b("ArrowDown"),
  toggleShuffle: b("KeyS", { shift: true }),
  toggleRepeat: b("KeyR", { shift: true }),
  search: b("Space", { ctrl: true }),
  upload: b("KeyU", { shift: true }),
  uploadReports: b("KeyU", { ctrl: true }),
  account: b("KeyA", { shift: true }),
};

// ---------------------------------------------------------------------------
// combo formatting + matching
// ---------------------------------------------------------------------------

const CODE_LABELS: Record<string, string> = {
  Space: "Space",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Escape: "Esc",
  Enter: "Enter",
  Backspace: "⌫",
  Tab: "Tab",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

/** Human label for a single key code (no modifiers). */
function keyLabel(code: string): string {
  if (CODE_LABELS[code]) return CODE_LABELS[code];
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  return code;
}

const IS_MAC =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

/** Ordered token list for a binding, e.g. ["Shift", "←"]. */
export function bindingTokens(bnd: Binding): string[] {
  const t: string[] = [];
  if (bnd.ctrl) t.push("Ctrl");
  if (bnd.alt) t.push(IS_MAC ? "⌥" : "Alt");
  if (bnd.shift) t.push("Shift");
  if (bnd.meta) t.push(IS_MAC ? "⌘" : "Win");
  t.push(keyLabel(bnd.code));
  return t;
}

/** A modifier-only code we should never accept as a standalone binding. */
const MODIFIER_CODES = new Set([
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

export function isModifierCode(code: string): boolean {
  return MODIFIER_CODES.has(code);
}

/** Build a binding from a keydown event (scope defaults to local). */
export function bindingFromEvent(e: KeyboardEvent, scope: BindScope = "local"): Binding {
  return {
    code: e.code,
    shift: e.shiftKey,
    ctrl: e.ctrlKey,
    alt: e.altKey,
    meta: e.metaKey,
    scope,
  };
}

/** True when a keydown event exactly matches a binding (modifiers included). */
export function eventMatches(e: KeyboardEvent, bnd: Binding): boolean {
  return (
    e.code === bnd.code &&
    e.shiftKey === bnd.shift &&
    e.ctrlKey === bnd.ctrl &&
    e.altKey === bnd.alt &&
    e.metaKey === bnd.meta
  );
}

/** Two bindings share the same combo (scope ignored). */
export function sameCombo(a: Binding, c: Binding): boolean {
  return (
    a.code === c.code &&
    a.shift === c.shift &&
    a.ctrl === c.ctrl &&
    a.alt === c.alt &&
    a.meta === c.meta
  );
}

// ---------------------------------------------------------------------------
// persistence + store
// ---------------------------------------------------------------------------

const STORAGE_KEY = "octave:keybinds";

function load(): Bindings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_BINDINGS };
    const parsed = JSON.parse(raw) as Bindings;
    // Trust stored values but keep defaults for any command the user has never
    // touched. A stored `null`/absent for a command that *has* a default still
    // means "unbound" only if the key is present; absence falls back to default.
    const merged: Bindings = { ...DEFAULT_BINDINGS };
    for (const id of Object.keys(parsed) as CommandId[]) {
      merged[id] = parsed[id]; // may be a Binding or undefined (cleared)
    }
    return merged;
  } catch {
    return { ...DEFAULT_BINDINGS };
  }
}

function persist(bindings: Bindings) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(bindings));
  } catch {
    /* storage full / unavailable — non-fatal */
  }
}

type KeybindStore = {
  bindings: Bindings;
  /** Assign a combo to a command, clearing the same combo off any other. */
  setBinding: (id: CommandId, bnd: Binding) => void;
  /** Remove a command's binding. */
  clearBinding: (id: CommandId) => void;
  /** Change only the scope of an existing binding. */
  setScope: (id: CommandId, scope: BindScope) => void;
  /** Restore one command to its factory default (or unbind if none). */
  resetBinding: (id: CommandId) => void;
  /** Restore every command to factory defaults. */
  resetAll: () => void;
  /** Command currently using this combo, if any (excluding `except`). */
  conflictFor: (bnd: Binding, except?: CommandId) => CommandId | null;
};

export const useKeybindStore = create<KeybindStore>((set, get) => ({
  bindings: load(),

  setBinding: (id, bnd) => {
    const next: Bindings = { ...get().bindings };
    // A combo can only drive one command — steal it from whoever held it.
    for (const other of Object.keys(next) as CommandId[]) {
      const ob = next[other];
      if (other !== id && ob && sameCombo(ob, bnd)) delete next[other];
    }
    next[id] = bnd;
    persist(next);
    set({ bindings: next });
  },

  clearBinding: (id) => {
    const next: Bindings = { ...get().bindings };
    delete next[id];
    persist(next);
    set({ bindings: next });
  },

  setScope: (id, scope) => {
    const cur = get().bindings[id];
    if (!cur) return;
    const next: Bindings = { ...get().bindings, [id]: { ...cur, scope } };
    persist(next);
    set({ bindings: next });
  },

  resetBinding: (id) => {
    const next: Bindings = { ...get().bindings };
    const def = DEFAULT_BINDINGS[id];
    if (def) next[id] = { ...def };
    else delete next[id];
    persist(next);
    set({ bindings: next });
  },

  resetAll: () => {
    const next = { ...DEFAULT_BINDINGS };
    persist(next);
    set({ bindings: next });
  },

  conflictFor: (bnd, except) => {
    const { bindings } = get();
    for (const id of Object.keys(bindings) as CommandId[]) {
      if (id === except) continue;
      const ob = bindings[id];
      if (ob && sameCombo(ob, bnd)) return id;
    }
    return null;
  },
}));
