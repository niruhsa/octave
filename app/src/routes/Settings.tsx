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
import { KeyIcon } from "../components/icons";

type SectionId = "keybinds";

const SECTIONS: { id: SectionId; label: string; Icon: typeof KeyIcon }[] = [
  { id: "keybinds", label: "Keybinds & Hotkeys", Icon: KeyIcon },
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
          {section === "keybinds" && <KeybindsSection />}
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
