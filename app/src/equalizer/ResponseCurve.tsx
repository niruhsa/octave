import { useMemo } from "react";
import {
  calculateEqualizerResponse,
  EQ_ASSUMED_SAMPLE_RATE,
  type EqualizerResponse,
} from "./dsp";
import type { EqualizerProfile } from "./types";
import type { EqualizerGraphDiagnostics } from "../player/audioGraph";

const WIDTH = 640;
const HEIGHT = 190;
const PAD = { left: 42, right: 14, top: 14, bottom: 28 };
const MIN_DRAW_HZ = 20;
const MAX_DRAW_HZ = 20_000;
const MIN_DB = -24;
const MAX_DB = 24;

const frequencyX = (frequency: number) => {
  const portion =
    Math.log10(frequency / MIN_DRAW_HZ) / Math.log10(MAX_DRAW_HZ / MIN_DRAW_HZ);
  return PAD.left + portion * (WIDTH - PAD.left - PAD.right);
};

const gainY = (gain: number) => {
  const clamped = Math.max(MIN_DB, Math.min(MAX_DB, gain));
  return PAD.top + ((MAX_DB - clamped) / (MAX_DB - MIN_DB)) * (HEIGHT - PAD.top - PAD.bottom);
};

const formatFrequency = (frequency: number) =>
  frequency >= 1_000 ? `${frequency / 1_000}k` : String(frequency);

const signed = (value: number) => `${value > 0 ? "+" : ""}${value.toFixed(1)} dB`;

function responsePath(response: EqualizerResponse): string {
  const points = response.frequenciesHz
    .map((frequency, index) => ({ frequency, gain: response.appliedDb[index] }))
    .filter(({ frequency }) => frequency >= MIN_DRAW_HZ && frequency <= MAX_DRAW_HZ);
  return points
    .map(({ frequency, gain }, index) =>
      `${index === 0 ? "M" : "L"}${frequencyX(frequency).toFixed(2)},${gainY(gain).toFixed(2)}`,
    )
    .join(" ");
}

export function ResponseCurve({
  profile,
  sampleRate,
  runtime,
}: {
  profile: EqualizerProfile;
  sampleRate?: number | null;
  runtime?: EqualizerGraphDiagnostics | null;
}) {
  const assumed = sampleRate == null;
  const response = useMemo(
    () => calculateEqualizerResponse(profile, sampleRate ?? EQ_ASSUMED_SAMPLE_RATE),
    [profile, sampleRate],
  );
  const path = useMemo(() => responsePath(response), [response]);
  const peak = runtime?.appliedProfileId === profile.id ? runtime.peakResponseDb : response.peakResponseDb;
  const trim = runtime?.appliedProfileId === profile.id ? runtime.safetyTrimDb : response.safetyTrimDb;

  return (
    <div className="flex flex-col gap-3 rounded-xl border border-oct-border-strong bg-oct-card p-3">
      <svg
        viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
        role="img"
        aria-label={`Equalizer frequency response from 20 hertz to 20 kilohertz; modeled peak ${signed(peak)}`}
        className="h-auto w-full overflow-visible"
      >
        <defs>
          <linearGradient id="eq-curve" x1="0" x2="1">
            <stop offset="0" stopColor="#c99a5a" />
            <stop offset="0.55" stopColor="#f0c074" />
            <stop offset="1" stopColor="#e0a84b" />
          </linearGradient>
        </defs>
        {[-12, 0, 12].map((gain) => (
          <g key={gain}>
            <line
              x1={PAD.left}
              x2={WIDTH - PAD.right}
              y1={gainY(gain)}
              y2={gainY(gain)}
              stroke={gain === 0 ? "#54585f" : "#26282d"}
              strokeDasharray={gain === 0 ? undefined : "3 5"}
            />
            <text x={PAD.left - 7} y={gainY(gain) + 3} textAnchor="end" fill="#6b6f76" fontSize="9">
              {gain > 0 ? `+${gain}` : gain}
            </text>
          </g>
        ))}
        {[20, 100, 1_000, 10_000, 20_000].map((frequency) => (
          <g key={frequency}>
            <line
              x1={frequencyX(frequency)}
              x2={frequencyX(frequency)}
              y1={PAD.top}
              y2={HEIGHT - PAD.bottom}
              stroke="#1f2127"
            />
            <text
              x={frequencyX(frequency)}
              y={HEIGHT - 9}
              textAnchor="middle"
              fill="#6b6f76"
              fontSize="9"
            >
              {formatFrequency(frequency)}
            </text>
          </g>
        ))}
        {path && (
          <path
            d={path}
            fill="none"
            stroke="url(#eq-curve)"
            strokeWidth="2.25"
            strokeLinejoin="round"
            strokeLinecap="round"
          />
        )}
      </svg>

      <div className="grid grid-cols-2 gap-2 text-[11.5px] sm:grid-cols-4">
        <Metric label="Stored preamp" value={signed(profile.preamp_db)} />
        <Metric label="Steady-state peak" value={signed(peak)} />
        <Metric label="Safety trim" value={signed(trim)} />
        <Metric label="Effective preamp" value={signed(profile.preamp_db + trim)} />
      </div>

      <p className="text-[10.5px] leading-relaxed text-oct-faint">
        {assumed
          ? `Preview modeled at ${EQ_ASSUMED_SAMPLE_RATE / 1_000} kHz; playback recalculates from the real audio context.`
          : `Calculated for ${(response.sampleRate / 1_000).toFixed(1)} kHz output.`}{" "}
        Auto headroom models steady-state EQ peaks with a 1 dB margin; it is not a true-peak,
        transient, or hearing-safety limiter.
      </p>

      {response.incompatibleBandPosition != null && (
        <div className="rounded-lg border border-oct-offline/50 bg-oct-offline/10 px-3 py-2 text-[12px] text-oct-danger">
          Band {response.incompatibleBandPosition} is at or above this output&apos;s Nyquist limit.
          Playback uses Flat instead of silently changing the correction.
        </div>
      )}
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg bg-oct-elevated px-2.5 py-2">
      <div className="text-oct-faint">{label}</div>
      <div className="mt-0.5 font-mono text-oct-text">{value}</div>
    </div>
  );
}

