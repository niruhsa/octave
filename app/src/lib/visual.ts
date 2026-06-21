// Visual helpers shared by the OCTAVE UI.

import type { MergedTrack } from "../ipc";

/** The OCTAVE album-art gradient pairs (lifted from the design comps). */
const GRADIENTS: [string, string][] = [
  ["#3b4a6b", "#161b29"], // blue
  ["#6b3b46", "#2a161c"], // wine
  ["#5a4a6b", "#211a2e"], // violet
  ["#2f5a57", "#13302e"], // teal
  ["#6b5a3b", "#2a2317"], // gold
  ["#44556b", "#191f29"], // steel
  ["#41474d", "#1c1e22"], // graphite
  ["#4a5a52", "#1a221d"], // sage
];

/** Stable string hash (djb2) → non-negative int. */
function hash(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  return Math.abs(h);
}

/**
 * Deterministic `linear-gradient(...)` for an album/track id, so the same
 * record always renders the same artwork-stand-in (and adjacent records
 * read as distinct). Used as the cover placeholder + small thumbnails.
 */
export function gradientFor(id: string | undefined | null): string {
  const [a, b] = GRADIENTS[hash(id ?? "octave") % GRADIENTS.length];
  return `linear-gradient(140deg, ${a}, ${b})`;
}

/**
 * Mono quality readout, e.g. "FLAC · 96kHz" → here we only reliably know the
 * codec and (sometimes) bitrate, so render what we have. The design shows
 * "FLAC 96/24"; we approximate with codec + kbps when present.
 */
export function qualityLabel(track: Pick<MergedTrack, "codec" | "bitrate_kbps">): string {
  const codec = (track.codec || "").toUpperCase();
  if (track.bitrate_kbps) return `${codec} ${track.bitrate_kbps}k`.trim();
  return codec || "—";
}
