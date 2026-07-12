// /settings — app preferences. Currently a single sub-menu, "Keybinds &
// Hotkeys", but laid out as a sectioned page so more panels can slot in later.
//
// The keybind editor lets the user rebind any command, choose its scope
// (in-app vs system-wide), clear it, or reset to defaults. Bindings live in
// the keybind store (persisted); the in-app dispatcher (`useHotkeys`) reads
// them live, so changes take effect immediately.

import { useEffect, useState } from "react";
import {
  COMMANDS,
  bindingTokens,
  bindingFromEvent,
  isModifierCode,
  useKeybindStore,
  type Binding,
  type BindScope,
  type CommandDef,
  type CommandGroup,
  type CommandId,
} from "../settings/keybinds";
import { btnGhostSm, card, label } from "../lib/ui";
import {
  KeyIcon,
  NetworkIcon,
  PlayIcon,
  PodcastIcon,
  SearchIcon,
} from "../components/icons";
import { useQuickSearchStore } from "../quicksearch/store";
import { usePodcastPrefsStore } from "../podcasts/prefs";
import {
  DEFAULT_CHUNK_CONCURRENCY,
  MAX_CHUNK_CONCURRENCY,
  MIN_CHUNK_CONCURRENCY,
  useNetworkPrefsStore,
} from "../settings/network";
import {
  MAX_CROSSFADE_SEC,
  MIN_LOUDNESS_TARGET_LUFS,
  MAX_LOUDNESS_TARGET_LUFS,
  MIN_LOUDNESS_PREAMP_DB,
  MAX_LOUDNESS_PREAMP_DB,
  usePlaybackPrefsStore,
  type LoudnessMode,
} from "../settings/playback";
import { usePlayerStore } from "../player/store";

type SectionId = "player" | "keybinds" | "quicksearch" | "podcasts" | "networking";

const SECTIONS: { id: SectionId; label: string; Icon: typeof KeyIcon }[] = [
  { id: "player", label: "Player", Icon: PlayIcon },
  { id: "keybinds", label: "Keybinds & Hotkeys", Icon: KeyIcon },
  { id: "quicksearch", label: "Quick Search", Icon: SearchIcon },
  { id: "podcasts", label: "Podcasts", Icon: PodcastIcon },
  { id: "networking", label: "Networking", Icon: NetworkIcon },
];

export default function Settings() {
  const [section, setSection] = useState<SectionId>("keybinds");

  return (
    <section className="mx-auto flex max-w-4xl flex-col gap-6 p-6 md:p-8">
      <h1 className="text-[27px] font-semibold tracking-tight">Settings</h1>

      <div className="flex flex-col gap-6 md:flex-row md:gap-8">
        {/* sub-menu */}
        <nav className="flex shrink-0 gap-1 overflow-x-auto md:w-52 md:flex-col">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              onClick={() => setSection(s.id)}
              className={`flex items-center gap-2.5 whitespace-nowrap rounded-lg px-3 py-2 text-left text-[13.5px] transition-colors ${
                section === s.id
                  ? "bg-oct-elevated text-oct-text"
                  : "text-oct-muted hover:bg-oct-elevated/60 hover:text-oct-text"
              }`}
            >
              <s.Icon size={16} className="shrink-0" />
              <span>{s.label}</span>
            </button>
          ))}
        </nav>

        {/* content */}
        <div className="min-w-0 flex-1">
          {section === "player" && <PlayerSection />}
          {section === "keybinds" && <KeybindsSection />}
          {section === "quicksearch" && <QuickSearchSection />}
          {section === "podcasts" && <PodcastsSection />}
          {section === "networking" && <NetworkingSection />}
        </div>
      </div>
    </section>
  );
}

const GROUP_ORDER: CommandGroup[] = ["Playback", "Navigation"];

function KeybindsSection() {
  const resetAll = useKeybindStore((s) => s.resetAll);

  return (
    <div className="flex flex-col gap-5">
      <div className="flex items-start justify-between gap-4">
        <p className="text-[13px] leading-relaxed text-oct-subtle">
          Click a command's shortcut to rebind it. In-app shortcuts work while
          OCTAVE is focused.
        </p>
        <button onClick={resetAll} className={`${btnGhostSm} shrink-0`}>
          Reset all
        </button>
      </div>

      {GROUP_ORDER.map((group) => {
        const cmds = COMMANDS.filter((c) => c.group === group);
        if (cmds.length === 0) return null;
        return (
          <div key={group} className="flex flex-col gap-2">
            <div className={label}>{group.toUpperCase()}</div>
            <div className={`${card} divide-y divide-oct-border`}>
              {cmds.map((c) => (
                <KeybindRow key={c.id} cmd={c} />
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}

function KeybindRow({ cmd }: { cmd: CommandDef }) {
  const binding = useKeybindStore((s) => s.bindings[cmd.id]);
  const setBinding = useKeybindStore((s) => s.setBinding);
  const clearBinding = useKeybindStore((s) => s.clearBinding);
  const setScope = useKeybindStore((s) => s.setScope);
  const resetBinding = useKeybindStore((s) => s.resetBinding);
  const conflictFor = useKeybindStore((s) => s.conflictFor);

  const [capturing, setCapturing] = useState(false);
  const [stolen, setStolen] = useState<CommandId | null>(null);

  // While capturing, the next real key combo becomes the binding. Modifier-only
  // presses are ignored (wait for the actual key); Escape cancels.
  useEffect(() => {
    if (!capturing) return;
    function onKey(e: KeyboardEvent) {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setCapturing(false);
        return;
      }
      if (isModifierCode(e.code)) return; // still holding modifiers — wait
      const next: Binding = bindingFromEvent(e, binding?.scope ?? "local");
      const clash = conflictFor(next, cmd.id);
      setStolen(clash);
      setBinding(cmd.id, next);
      setCapturing(false);
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturing, binding?.scope, cmd.id, conflictFor, setBinding]);

  // Clear the "stole from X" note once the user moves on.
  useEffect(() => {
    if (!stolen) return;
    const t = setTimeout(() => setStolen(null), 4000);
    return () => clearTimeout(t);
  }, [stolen]);

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3">
      <div className="min-w-0 flex-1">
        <div className="text-[13.5px] text-oct-text">{cmd.label}</div>
        <div className="truncate text-[11.5px] text-oct-faint">{cmd.description}</div>
        {stolen && (
          <div className="mt-1 text-[11px] text-oct-accent">
            Reassigned from “{COMMANDS.find((c) => c.id === stolen)?.label}”
          </div>
        )}
      </div>

      {/* scope toggle (only meaningful when bound) */}
      {binding && (
        <ScopeToggle
          scope={binding.scope}
          onChange={(s) => setScope(cmd.id, s)}
        />
      )}

      {/* combo display / capture button */}
      <button
        onClick={() => setCapturing((v) => !v)}
        className={`min-w-[120px] rounded-lg border px-3 py-1.5 text-center transition-colors ${
          capturing
            ? "border-oct-accent bg-oct-accent/10 text-oct-accent"
            : "border-oct-border-strong text-oct-text hover:border-oct-line"
        }`}
        title="Click, then press the keys to bind"
      >
        {capturing ? (
          <span className="text-[12px]">Press keys…</span>
        ) : binding ? (
          <span className="inline-flex flex-wrap items-center justify-center gap-1">
            {bindingTokens(binding).map((t, i) => (
              <kbd
                key={i}
                className="rounded bg-oct-elevated px-1.5 py-0.5 font-mono text-[11px] text-oct-text"
              >
                {t}
              </kbd>
            ))}
          </span>
        ) : (
          <span className="font-mono text-[11px] text-oct-faint">Unbound</span>
        )}
      </button>

      {/* row actions */}
      <div className="flex items-center gap-1">
        <button
          onClick={() => clearBinding(cmd.id)}
          disabled={!binding || capturing}
          className="rounded px-2 py-1 text-[11px] text-oct-subtle transition-colors hover:text-oct-danger disabled:opacity-30"
          title="Clear shortcut"
        >
          Clear
        </button>
        <button
          onClick={() => resetBinding(cmd.id)}
          disabled={capturing}
          className="rounded px-2 py-1 text-[11px] text-oct-subtle transition-colors hover:text-oct-text disabled:opacity-30"
          title="Reset to default"
        >
          Reset
        </button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Player section
// ---------------------------------------------------------------------------

function PlayerSection() {
  const prefs = usePlaybackPrefsStore((s) => s.prefs);
  const setPref = usePlaybackPrefsStore((s) => s.setPref);
  const refreshLoudness = usePlayerStore((s) => s.refreshLoudness);
  const fadeOn = prefs.gaplessEnabled && prefs.crossfadeSec > 0;
  const loudnessOn = prefs.loudnessMode !== "off";

  return (
    <div className="flex flex-col gap-5">
      <p className="text-[13px] leading-relaxed text-oct-subtle">
        How one track hands off to the next. Gapless preloads the upcoming
        track and starts it the instant the current one ends; crossfade blends
        them into each other instead. Changes apply from the next track change.
      </p>

      <div className="flex flex-col gap-2">
        <div className={label}>TRANSITIONS</div>
        <div className={`${card} divide-y divide-oct-border`}>
          <ToggleRow
            title="Gapless playback"
            desc="Preload the next track so playback continues without a gap. Turning this off restores the old load-at-the-boundary behavior (and disables crossfade)."
            on={prefs.gaplessEnabled}
            onChange={(v) => setPref("gaplessEnabled", v)}
          />

          {/* crossfade duration */}
          <div className="flex flex-col gap-3 px-4 py-3">
            <div className="flex items-center justify-between gap-4">
              <div className="min-w-0">
                <div
                  className={`text-[13.5px] ${prefs.gaplessEnabled ? "text-oct-text" : "text-oct-faint"}`}
                >
                  Crossfade between tracks
                </div>
                <div className="text-[11.5px] text-oct-faint">
                  Fade the ending track out while the next fades in
                  (equal-power, so loudness stays level). Off = gapless cut.
                  Podcast episodes never crossfade.
                </div>
              </div>
              <span className="shrink-0 rounded-lg bg-oct-elevated px-2.5 py-1 font-mono text-[13px] text-oct-text">
                {fadeOn ? `${prefs.crossfadeSec} s` : "Off"}
              </span>
            </div>
            <input
              type="range"
              min={0}
              max={MAX_CROSSFADE_SEC}
              step={1}
              value={prefs.crossfadeSec}
              disabled={!prefs.gaplessEnabled}
              onChange={(e) => setPref("crossfadeSec", Number(e.target.value))}
              className="oct-range flex-1 disabled:opacity-40"
              aria-label="Crossfade duration (seconds)"
            />
            <div className="flex justify-between font-mono text-[10.5px] text-oct-faint">
              <span>Off (gapless)</span>
              <span>{MAX_CROSSFADE_SEC} s</span>
            </div>
          </div>

          <ToggleRow
            title="Crossfade on manual skip"
            desc="Also fade briefly when you press next/previous or jump to a track."
            on={prefs.crossfadeOnManualSkip}
            disabled={!fadeOn}
            onChange={(v) => setPref("crossfadeOnManualSkip", v)}
          />
          <ToggleRow
            title="Album-aware gapless"
            desc="Consecutive tracks of the same album always transition gaplessly — continuous albums and live sets are never faded."
            on={prefs.smartAlbumGapless}
            disabled={!fadeOn}
            onChange={(v) => setPref("smartAlbumGapless", v)}
          />
        </div>
      </div>

      {/* ───────── loudness normalization ───────── */}
      <div className="flex flex-col gap-2">
        <div className={label}>LOUDNESS NORMALIZATION</div>
        <div className={`${card} flex flex-col gap-4 px-4 py-3`}>
          <div>
            <div className="text-[13.5px] text-oct-text">Volume leveling</div>
            <div className="text-[11.5px] text-oct-faint">
              Play every track at a consistent loudness from the server's measured
              values (ReplayGain / EBU R128). <b>Track</b> levels each song on its
              own; <b>Album</b> keeps an album's quiet and loud moments relative to
              each other. Tracks the server hasn't analyzed yet play unchanged.
            </div>
          </div>

          {/* segmented Off / Track / Album */}
          <div className="grid grid-cols-3 gap-1 rounded-lg bg-oct-elevated p-1">
            {(["off", "track", "album"] as LoudnessMode[]).map((m) => (
              <button
                key={m}
                onClick={() => {
                  setPref("loudnessMode", m);
                  refreshLoudness();
                }}
                className={`rounded-md px-3 py-1.5 text-[12.5px] capitalize transition ${
                  prefs.loudnessMode === m
                    ? "bg-oct-accent text-white"
                    : "text-oct-subtle hover:text-oct-text"
                }`}
              >
                {m}
              </button>
            ))}
          </div>

          {loudnessOn && (
            <>
              {/* target loudness */}
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between gap-4">
                  <span className="text-[12.5px] text-oct-text">Target loudness</span>
                  <span className="shrink-0 rounded-lg bg-oct-elevated px-2.5 py-1 font-mono text-[13px] text-oct-text">
                    {prefs.loudnessTargetLufs} LUFS
                  </span>
                </div>
                <input
                  type="range"
                  min={MIN_LOUDNESS_TARGET_LUFS}
                  max={MAX_LOUDNESS_TARGET_LUFS}
                  step={1}
                  value={prefs.loudnessTargetLufs}
                  onChange={(e) => {
                    setPref("loudnessTargetLufs", Number(e.target.value));
                    refreshLoudness();
                  }}
                  className="oct-range flex-1"
                  aria-label="Target loudness (LUFS)"
                />
                <div className="flex justify-between font-mono text-[10.5px] text-oct-faint">
                  <span>Quieter</span>
                  <span>Louder</span>
                </div>
              </div>

              {/* preamp */}
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between gap-4">
                  <div className="min-w-0">
                    <div className="text-[12.5px] text-oct-text">Preamp</div>
                    <div className="text-[11.5px] text-oct-faint">
                      Extra trim on top of normalization.
                    </div>
                  </div>
                  <span className="shrink-0 rounded-lg bg-oct-elevated px-2.5 py-1 font-mono text-[13px] text-oct-text">
                    {prefs.loudnessPreampDb > 0 ? "+" : ""}
                    {prefs.loudnessPreampDb} dB
                  </span>
                </div>
                <input
                  type="range"
                  min={MIN_LOUDNESS_PREAMP_DB}
                  max={MAX_LOUDNESS_PREAMP_DB}
                  step={1}
                  value={prefs.loudnessPreampDb}
                  onChange={(e) => {
                    setPref("loudnessPreampDb", Number(e.target.value));
                    refreshLoudness();
                  }}
                  className="oct-range flex-1"
                  aria-label="Preamp (dB)"
                />
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Quick Search section
// ---------------------------------------------------------------------------

/** In-palette keys that are fixed (not rebindable) while the palette is focused. */
const PALETTE_KEYS: { label: string; keys: string[] }[] = [
  { label: "Commit the current text to a filter pill", keys: ["↵", "|"] },
  { label: "Accept the inline suggestion", keys: ["Tab"] },
  { label: "Move through results / commands", keys: ["↑", "↓"] },
  { label: "Play or open the selected result", keys: ["↵"] },
  { label: "Focus the filter pills", keys: ["←"] },
  { label: "Edit the focused pill", keys: ["E"] },
  { label: "Remove the focused pill", keys: ["⌫"] },
  { label: "Step back / close the palette", keys: ["Esc"] },
];

function QuickSearchSection() {
  const prefs = useQuickSearchStore((s) => s.prefs);
  const setPref = useQuickSearchStore((s) => s.setPref);
  const recents = useQuickSearchStore((s) => s.recents);
  const clearRecents = useQuickSearchStore((s) => s.clearRecents);

  const openCmd = COMMANDS.find((c) => c.id === "quickSearch");

  return (
    <div className="flex flex-col gap-5">
      <p className="text-[13px] leading-relaxed text-oct-subtle">
        Quick Search is the command palette that replaces the old Search tab.
        Open it from anywhere to search your library, run an action by typing{" "}
        <span className="font-mono text-oct-muted">&gt;</span>, or jump to a page
        with <span className="font-mono text-oct-muted">!</span>.
      </p>

      {/* open shortcut (rebindable — shares the keybind store) */}
      {openCmd && (
        <div className="flex flex-col gap-2">
          <div className={label}>SHORTCUT</div>
          <div className={`${card} divide-y divide-oct-border`}>
            <KeybindRow cmd={openCmd} />
          </div>
        </div>
      )}

      {/* behaviour toggles */}
      <div className="flex flex-col gap-2">
        <div className={label}>BEHAVIOUR</div>
        <div className={`${card} divide-y divide-oct-border`}>
          <ToggleRow
            title="Keyboard hint footer"
            desc="Show the shortcut hints along the bottom of the palette."
            on={prefs.keyboardHints}
            onChange={(v) => setPref("keyboardHints", v)}
          />
          <ToggleRow
            title="Dim background"
            desc="Blur and darken the app behind the palette while it's open."
            on={prefs.dimBackground}
            onChange={(v) => setPref("dimBackground", v)}
          />
        </div>
      </div>

      {/* fixed in-palette keys reference */}
      <div className="flex flex-col gap-2">
        <div className={label}>IN-PALETTE KEYS</div>
        <div className={`${card} divide-y divide-oct-border`}>
          {PALETTE_KEYS.map((k) => (
            <div key={k.label} className="flex items-center justify-between gap-4 px-4 py-2.5">
              <span className="text-[13px] text-oct-text">{k.label}</span>
              <span className="flex shrink-0 gap-1">
                {k.keys.map((kk, i) => (
                  <kbd key={i} className="rounded bg-oct-elevated px-1.5 py-0.5 font-mono text-[11px] text-oct-text">
                    {kk}
                  </kbd>
                ))}
              </span>
            </div>
          ))}
        </div>
        <p className="text-[11.5px] text-oct-faint">
          These work while the palette is focused and aren't rebindable.
        </p>
      </div>

      {/* recents */}
      <div className="flex items-center justify-between gap-4">
        <span className="text-[13px] text-oct-subtle">
          {recents.length} recent search{recents.length === 1 ? "" : "es"} remembered
        </span>
        <button onClick={clearRecents} disabled={!recents.length} className={btnGhostSm}>
          Clear recents
        </button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Podcasts section
// ---------------------------------------------------------------------------

function PodcastsSection() {
  const openAfterSubscribe = usePodcastPrefsStore((s) => s.prefs.openAfterSubscribe);
  const setPref = usePodcastPrefsStore((s) => s.setPref);

  return (
    <div className="flex flex-col gap-5">
      <p className="text-[13px] leading-relaxed text-oct-subtle">
        Subscribing is instant — the show's back-catalogue keeps loading in the
        background after you're taken to it.
      </p>

      <div className="flex flex-col gap-2">
        <div className={label}>BEHAVIOUR</div>
        <div className={`${card} divide-y divide-oct-border`}>
          <ToggleRow
            title="Open the show after subscribing"
            desc="Jump to the podcast's page when you subscribe. Hold Shift while clicking Subscribe to do the opposite."
            on={openAfterSubscribe}
            onChange={(v) => setPref("openAfterSubscribe", v)}
          />
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Networking section
// ---------------------------------------------------------------------------

function NetworkingSection() {
  const concurrency = useNetworkPrefsStore((s) => s.prefs.chunkConcurrency);
  const setPref = useNetworkPrefsStore((s) => s.setPref);

  return (
    <div className="flex flex-col gap-5">
      <p className="text-[13px] leading-relaxed text-oct-subtle">
        Tune how OCTAVE talks to your server while uploading. Changes apply right
        away — including to an upload that's already in progress.
      </p>

      <div className="flex flex-col gap-2">
        <div className={label}>UPLOADS</div>
        <div className={`${card} divide-y divide-oct-border`}>
          <div className="flex flex-col gap-3 px-4 py-3">
            <div className="flex items-center justify-between gap-4">
              <div className="min-w-0">
                <div className="text-[13.5px] text-oct-text">
                  Chunk upload concurrency
                </div>
                <div className="text-[11.5px] text-oct-faint">
                  How many file chunks upload in parallel. Higher can be faster on
                  high-latency links but uses more memory and bandwidth. Default{" "}
                  {DEFAULT_CHUNK_CONCURRENCY}.
                </div>
              </div>
              <span className="shrink-0 rounded-lg bg-oct-elevated px-2.5 py-1 font-mono text-[13px] text-oct-text">
                {concurrency}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <input
                type="range"
                min={MIN_CHUNK_CONCURRENCY}
                max={MAX_CHUNK_CONCURRENCY}
                step={1}
                value={concurrency}
                onChange={(e) =>
                  setPref("chunkConcurrency", Number(e.target.value))
                }
                className="oct-range flex-1"
                aria-label="Chunk upload concurrency"
              />
              <button
                onClick={() =>
                  setPref("chunkConcurrency", DEFAULT_CHUNK_CONCURRENCY)
                }
                disabled={concurrency === DEFAULT_CHUNK_CONCURRENCY}
                className={`${btnGhostSm} shrink-0`}
              >
                Reset
              </button>
            </div>
            <div className="flex justify-between font-mono text-[10.5px] text-oct-faint">
              <span>{MIN_CHUNK_CONCURRENCY}</span>
              <span>{MAX_CHUNK_CONCURRENCY}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function ToggleRow({
  title,
  desc,
  on,
  onChange,
  disabled = false,
}: {
  title: string;
  desc: string;
  on: boolean;
  onChange: (v: boolean) => void;
  /** Grey the row out and ignore taps (e.g. a pref gated behind another). */
  disabled?: boolean;
}) {
  return (
    <div
      className={`flex items-center justify-between gap-4 px-4 py-3 ${disabled ? "opacity-40" : ""}`}
    >
      <div className="min-w-0">
        <div className="text-[13.5px] text-oct-text">{title}</div>
        <div className="text-[11.5px] text-oct-faint">{desc}</div>
      </div>
      <button
        role="switch"
        aria-checked={on}
        disabled={disabled}
        onClick={() => onChange(!on)}
        className={`inline-flex h-5 w-9 shrink-0 items-center rounded-full px-0.5 transition-colors ${
          on ? "bg-oct-accent" : "bg-oct-border-strong"
        }`}
      >
        <span
          className={`h-4 w-4 rounded-full bg-white transition-transform ${
            on ? "translate-x-4" : "translate-x-0"
          }`}
        />
      </button>
    </div>
  );
}

function ScopeToggle({
  scope,
  onChange,
}: {
  scope: BindScope;
  onChange: (s: BindScope) => void;
}) {
  return (
    <div
      className="flex overflow-hidden rounded-lg border border-oct-border-strong text-[11px]"
      title={
        scope === "global"
          ? "System-wide capture (works when OCTAVE is in the background) is coming soon — for now this behaves as in-app."
          : "Works while OCTAVE is focused"
      }
    >
      {(["local", "global"] as BindScope[]).map((s) => (
        <button
          key={s}
          onClick={() => onChange(s)}
          className={`px-2.5 py-1 transition-colors ${
            scope === s
              ? "bg-oct-elevated text-oct-text"
              : "text-oct-faint hover:text-oct-muted"
          }`}
        >
          {s === "local" ? "In-app" : "Global"}
        </button>
      ))}
    </div>
  );
}
