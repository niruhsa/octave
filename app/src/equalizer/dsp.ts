import { EQ_LIMITS, type EqualizerBand, type EqualizerProfile } from "./types";

export const EQ_ASSUMED_SAMPLE_RATE = 48_000;
export const EQ_HEADROOM_MARGIN_DB = 1;
export const EQ_RESPONSE_POINTS = 2_048;

export type EqualizerResponse = {
  sampleRate: number;
  frequenciesHz: number[];
  /** Stored preamp + enabled filters, before automatic safety trim. */
  untrimmedDb: number[];
  /** Curve that the runtime applies after automatic safety trim. */
  appliedDb: number[];
  peakResponseDb: number;
  safetyTrimDb: number;
  effectivePreampDb: number;
  incompatibleBandPosition: number | null;
};

export const dbToLinear = (db: number): number => 10 ** (db / 20);
export const linearToDb = (linear: number): number =>
  linear > 0 ? 20 * Math.log10(linear) : Number.NEGATIVE_INFINITY;

/** 100% extra means twice the shelf range's linear amplitude (+6.02 dB). */
export function extraPercentToDb(percent: number): number {
  const bounded = Math.min(100, Math.max(0, Number.isFinite(percent) ? percent : 0));
  return linearToDb(1 + bounded / 100);
}

/**
 * Stable identity for the values that can change audible EQ output.
 *
 * Native snapshots are refreshed regularly and therefore arrive as new
 * objects even when the selected profile has not changed. Keeping transport
 * metadata out of this signature prevents those refreshes from rebuilding a
 * live Web Audio filter bank (and producing a needless transition).
 */
export function equalizerProfileAudioSignature(
  profile: EqualizerProfile | null,
  bypassed: boolean,
  bassBoostPercent = 0,
  trebleBoostPercent = 0,
): string {
  const bass = Math.round(Math.min(100, Math.max(0, bassBoostPercent)));
  const treble = Math.round(Math.min(100, Math.max(0, trebleBoostPercent)));
  if (bypassed || (profile == null && bass === 0 && treble === 0)) return "flat";
  return JSON.stringify([
    profile == null
      ? null
      : [
          profile.id,
          profile.format_version,
          profile.preamp_db,
          profile.auto_headroom_enabled,
          profile.bands.map((band) => [
            band.position,
            band.enabled,
            band.filter_kind,
            band.frequency_hz,
            band.gain_db,
            band.q,
          ]),
        ],
    bass,
    treble,
  ]);
}

function uniqueSorted(values: number[]): number[] {
  values.sort((a, b) => a - b);
  const result: number[] = [];
  for (const value of values) {
    const previous = result[result.length - 1];
    if (previous == null || Math.abs(value - previous) > Math.max(1e-8, value * 1e-10)) {
      result.push(value);
    }
  }
  return result;
}

/** Dense logarithmic grid plus every enabled filter's center and half-Q neighbors. */
export function buildFrequencyGrid(
  bands: EqualizerBand[],
  sampleRate: number,
  points = EQ_RESPONSE_POINTS,
): number[] {
  const nyquist = sampleRate / 2;
  const upper = nyquist * (1 - 1e-7);
  if (!(upper > EQ_LIMITS.frequencyHz.min) || points < 2) return [];
  const ratio = upper / EQ_LIMITS.frequencyHz.min;
  const values = Array.from({ length: points }, (_, index) =>
    EQ_LIMITS.frequencyHz.min * ratio ** (index / (points - 1)),
  );
  for (const band of bands) {
    if (!band.enabled) continue;
    values.push(band.frequency_hz);
    const offset = 2 ** (1 / (2 * band.q));
    values.push(band.frequency_hz / offset, band.frequency_hz * offset);
  }
  return uniqueSorted(
    values.filter((frequency) => frequency >= EQ_LIMITS.frequencyHz.min && frequency < nyquist),
  );
}

/**
 * RBJ Audio EQ Cookbook peaking-filter magnitude. Used by the editor before a
 * real AudioContext exists; the runtime recomputes against actual Web Audio
 * nodes and its actual sample rate.
 */
export function peakingMagnitude(
  band: Pick<EqualizerBand, "frequency_hz" | "gain_db" | "q">,
  atFrequencyHz: number,
  sampleRate: number,
): number {
  if (band.gain_db === 0) return 1;
  const a = 10 ** (band.gain_db / 40);
  const w0 = (2 * Math.PI * band.frequency_hz) / sampleRate;
  const alpha = Math.sin(w0) / (2 * band.q);
  const cos0 = Math.cos(w0);

  const a0 = 1 + alpha / a;
  const b0 = (1 + alpha * a) / a0;
  const b1 = (-2 * cos0) / a0;
  const b2 = (1 - alpha * a) / a0;
  const a1 = (-2 * cos0) / a0;
  const a2 = (1 - alpha / a) / a0;

  const w = (2 * Math.PI * atFrequencyHz) / sampleRate;
  const cosW = Math.cos(w);
  const sinW = Math.sin(w);
  const cos2W = Math.cos(2 * w);
  const sin2W = Math.sin(2 * w);
  const numeratorReal = b0 + b1 * cosW + b2 * cos2W;
  const numeratorImag = -b1 * sinW - b2 * sin2W;
  const denominatorReal = 1 + a1 * cosW + a2 * cos2W;
  const denominatorImag = -a1 * sinW - a2 * sin2W;
  return Math.sqrt(
    (numeratorReal ** 2 + numeratorImag ** 2) /
      (denominatorReal ** 2 + denominatorImag ** 2),
  );
}

export function calculateEqualizerResponse(
  profile: Pick<EqualizerProfile, "preamp_db" | "auto_headroom_enabled" | "bands">,
  sampleRate = EQ_ASSUMED_SAMPLE_RATE,
): EqualizerResponse {
  const nyquist = sampleRate / 2;
  const incompatible = profile.bands.find(
    (band) => band.enabled && band.frequency_hz >= nyquist,
  );
  const frequenciesHz = buildFrequencyGrid(profile.bands, sampleRate);
  if (incompatible || frequenciesHz.length === 0) {
    return {
      sampleRate,
      frequenciesHz,
      untrimmedDb: frequenciesHz.map(() => 0),
      appliedDb: frequenciesHz.map(() => 0),
      peakResponseDb: 0,
      safetyTrimDb: 0,
      effectivePreampDb: 0,
      incompatibleBandPosition: incompatible?.position ?? null,
    };
  }

  const enabled = profile.bands.filter((band) => band.enabled);
  const untrimmedDb = frequenciesHz.map((frequency) => {
    let responseDb = profile.preamp_db;
    for (const band of enabled) {
      responseDb += linearToDb(peakingMagnitude(band, frequency, sampleRate));
    }
    return responseDb;
  });
  const peakResponseDb = Math.max(...untrimmedDb);
  const safetyTrimDb =
    profile.auto_headroom_enabled && peakResponseDb > 0
      ? -(peakResponseDb + EQ_HEADROOM_MARGIN_DB)
      : 0;
  return {
    sampleRate,
    frequenciesHz,
    untrimmedDb,
    appliedDb: untrimmedDb.map((value) => value + safetyTrimDb),
    peakResponseDb,
    safetyTrimDb,
    effectivePreampDb: profile.preamp_db + safetyTrimDb,
    incompatibleBandPosition: null,
  };
}
