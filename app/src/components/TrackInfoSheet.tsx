// Per-song media-info panel — opened from the Information action next to a
// track. Reuses the ActionSheet modal shell (bottom sheet on phones, centered
// dialog on sm+) but renders a read-only stat list instead of action buttons.

import { ActionSheet } from "./ActionSheet";
import type { MergedTrack } from "../ipc";
import { byteSize, formatDuration } from "../lib/format";
import { qualityLabel, sampleRateKHz } from "../lib/visual";

function channelsLabel(n: number | null): string | null {
  if (!n) return null;
  if (n === 1) return "Mono";
  if (n === 2) return "Stereo";
  return `${n} channels`;
}

function InfoRow({ label, value }: { label: string; value: string | null | undefined }) {
  if (!value) return null;
  return (
    <div className="flex items-baseline justify-between gap-4 px-5 py-2">
      <span className="shrink-0 font-mono text-[11px] tracking-wide text-oct-faint">{label}</span>
      <span className="min-w-0 truncate text-right text-[13px] text-oct-text" title={value}>
        {value}
      </span>
    </div>
  );
}

/** Media-info popup for a single track. */
export function TrackInfoSheet({ track, onClose }: { track: MergedTrack; onClose: () => void }) {
  const fileType = (track.codec || "").toUpperCase() || "Unknown";
  const khz = sampleRateKHz(track.sample_rate_hz);
  return (
    <ActionSheet title={track.title} subtitle="Media information" onClose={onClose}>
      <InfoRow label="FILE TYPE" value={fileType} />
      <InfoRow label="QUALITY" value={qualityLabel(track)} />
      <InfoRow label="SIZE" value={track.file_size ? byteSize(track.file_size) : null} />
      <InfoRow label="DURATION" value={formatDuration(track.duration_ms)} />
      <InfoRow
        label="BITRATE"
        value={track.bitrate_kbps ? `${track.bitrate_kbps} kbps` : null}
      />
      <InfoRow label="SAMPLE RATE" value={khz ? `${khz} kHz` : null} />
      <InfoRow label="BIT DEPTH" value={track.bit_depth ? `${track.bit_depth}-bit` : null} />
      <InfoRow label="CHANNELS" value={channelsLabel(track.channels)} />
      <InfoRow
        label="AVAILABILITY"
        value={track.downloaded ? "Downloaded (offline)" : "Stream-only"}
      />
      <InfoRow label="PATH" value={track.local_file_path || track.file_path} />
    </ActionSheet>
  );
}
