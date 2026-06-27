// Tiny formatting helpers shared across library views.

// `H:MM:SS` once the length reaches an hour (podcasts, long mixes), otherwise
// `M:SS`. The leading unit stays unpadded so short songs read `3:05`, not
// `03:05`, while an hour-plus episode reads `2:19:55` instead of `139:55`.
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = s.toString().padStart(2, "0");
  if (h > 0) return `${h}:${m.toString().padStart(2, "0")}:${ss}`;
  return `${m}:${ss}`;
}

/**
 * Human-readable byte size using binary units (KiB-scale, labelled KB/MB/GB/TB
 * the way storage UIs conventionally do). `0` and nullish render as "0 B".
 * One decimal place from MB up so totals read e.g. "12.4 GB"; bytes/KB stay
 * whole.
 */
export function byteSize(bytes: number | null | undefined): string {
  const n = bytes ?? 0;
  if (n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  // Whole numbers for B/KB; one decimal from MB upward (but trim a trailing .0).
  const decimals = i >= 2 ? 1 : 0;
  const s = v.toFixed(decimals).replace(/\.0$/, "");
  return `${s} ${units[i]}`;
}
