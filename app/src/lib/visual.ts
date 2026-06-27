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

/** Codecs whose audio is bit-exact (no perceptual loss). lofty reports the
 * container as a debug string, e.g. "Flac", "Wav", "Aiff", "WavPack", "Ape". */
const LOSSLESS_CODECS = new Set([
  "FLAC",
  "WAV",
  "WAVE",
  "AIFF",
  "AIF",
  "APE",
  "WAVPACK",
  "WV",
  "ALAC",
]);

export function isLossless(codec: string | null | undefined): boolean {
  return LOSSLESS_CODECS.has((codec || "").toUpperCase());
}

/** Sample rate in kHz, trimmed (44100 → "44.1", 48000 → "48", 192000 → "192"). */
export function sampleRateKHz(hz: number | null | undefined): string | null {
  if (!hz) return null;
  return (hz / 1000).toFixed(1).replace(/\.0$/, "");
}

/**
 * Mono quality readout. For lossless formats we show the studio-style
 * bit-depth/sample-rate pair when known (e.g. "Lossless · 24/96"); for lossy
 * formats we show codec + bitrate (e.g. "MP3 320k"). Falls back gracefully
 * when the probe didn't report the finer detail.
 */
export function qualityLabel(
  track: Pick<MergedTrack, "codec" | "bitrate_kbps"> &
    Partial<Pick<MergedTrack, "sample_rate_hz" | "bit_depth">>,
): string {
  const codec = (track.codec || "").toUpperCase();
  const khz = sampleRateKHz(track.sample_rate_hz);
  if (isLossless(codec)) {
    if (track.bit_depth && khz) return `Lossless · ${track.bit_depth}/${khz}`;
    if (khz) return `Lossless · ${khz}kHz`;
    return "Lossless";
  }
  if (track.bitrate_kbps) return `${codec} ${track.bitrate_kbps}k`.trim();
  return codec || "—";
}
