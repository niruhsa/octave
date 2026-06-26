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
