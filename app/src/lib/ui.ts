// Shared OCTAVE class-name presets so buttons / inputs / panels read
// identically across every route. Compose with template strings where a
// route needs extra utilities.

/** Amber pill — primary call to action (Play, Create, Sign in). */
export const btnPrimary =
  "inline-flex items-center justify-center gap-2 rounded-full bg-oct-accent px-5 py-2.5 text-[13.5px] font-medium text-oct-bg transition-colors hover:bg-oct-accent-bright disabled:opacity-50";

/** Bordered ghost pill — secondary action (Shuffle, Download, Rename). */
export const btnGhost =
  "inline-flex items-center justify-center gap-2 rounded-full border border-oct-border-strong px-4 py-2.5 text-[13.5px] text-oct-muted transition-colors hover:border-oct-line hover:text-oct-text disabled:opacity-50";

/** Small bordered ghost — compact row actions. */
export const btnGhostSm =
  "inline-flex items-center justify-center gap-1.5 rounded-lg border border-oct-border-strong px-2.5 py-1 text-xs text-oct-muted transition-colors hover:border-oct-line hover:text-oct-text disabled:opacity-40";

/** Destructive bordered button. */
export const btnDanger =
  "inline-flex items-center justify-center gap-2 rounded-full border border-oct-offline/60 px-4 py-2.5 text-[13.5px] text-oct-danger transition-colors hover:bg-oct-offline/15 disabled:opacity-50";

export const btnDangerSm =
  "inline-flex items-center justify-center gap-1.5 rounded-lg border border-oct-offline/50 px-2 py-1 text-xs text-oct-danger transition-colors hover:bg-oct-offline/15 disabled:opacity-40";

/** Dark form input / select. */
export const input =
  "w-full rounded-lg border border-oct-border-strong bg-oct-card px-3 py-2 text-sm text-oct-text placeholder:text-oct-faint focus:border-oct-accent focus:outline-none";

/** Card / panel surface. */
export const card = "rounded-xl border border-oct-border-strong bg-oct-panel";

/** Section label (mono, tracked, faint). */
export const label = "font-mono text-[10.5px] tracking-[0.16em] text-oct-faint";

/** Error banner. */
export const errorBox =
  "rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-sm text-oct-danger";

/** Success banner. */
export const okBox =
  "rounded-lg border border-oct-online/40 bg-oct-online/10 px-3 py-2 text-sm text-oct-online";
