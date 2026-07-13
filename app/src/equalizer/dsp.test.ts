import { describe, expect, it } from "vitest";
import {
  calculateEqualizerResponse,
  dbToLinear,
  EQ_HEADROOM_MARGIN_DB,
  peakingMagnitude,
} from "./dsp";
import type { EqualizerBand, EqualizerProfile } from "./types";

const band = (
  position: number,
  frequency_hz: number,
  gain_db: number,
  q = 1,
  enabled = true,
): EqualizerBand => ({
  position,
  enabled,
  filter_kind: "peaking",
  frequency_hz,
  gain_db,
  q,
});

const profile = (
  bands: EqualizerBand[],
  preamp_db = 0,
  auto_headroom_enabled = true,
): Pick<EqualizerProfile, "preamp_db" | "auto_headroom_enabled" | "bands"> => ({
  preamp_db,
  auto_headroom_enabled,
  bands,
});

describe("parametric EQ response model", () => {
  it("keeps a flat cascade at exact unity with zero safety trim", () => {
    const response = calculateEqualizerResponse(
      profile([band(1, 100, 0), band(2, 1_000, 0), band(3, 10_000, 0)]),
    );
    expect(response.peakResponseDb).toBe(0);
    expect(response.safetyTrimDb).toBe(0);
    expect(response.effectivePreampDb).toBe(0);
    expect(response.appliedDb.every((value) => value === 0)).toBe(true);
  });

  it("models preamp-only gain without inventing headroom when disabled", () => {
    const response = calculateEqualizerResponse(profile([band(1, 1_000, 0)], 3.25, false));
    expect(response.peakResponseDb).toBe(3.25);
    expect(response.safetyTrimDb).toBe(0);
    expect(response.appliedDb.every((value) => value === 3.25)).toBe(true);
  });

  it("hits the requested peaking-filter gain at its center frequency", () => {
    for (const gain of [-18, -3, 3, 18]) {
      expect(peakingMagnitude(band(1, 1_250, gain, 12), 1_250, 48_000)).toBeCloseTo(
        dbToLinear(gain),
        10,
      );
    }
  });

  it("ignores disabled filters and remains finite for a 32-filter cascade", () => {
    const bands = Array.from({ length: 32 }, (_, index) =>
      band(index + 1, 20 * (20_000 / 20) ** (index / 31), index % 2 === 0 ? 1.5 : -1, 8),
    );
    bands[0].enabled = false;
    const response = calculateEqualizerResponse(profile(bands, 2));
    expect(response.incompatibleBandPosition).toBeNull();
    expect(response.appliedDb.every(Number.isFinite)).toBe(true);
    expect(Math.max(...response.appliedDb)).toBeCloseTo(-EQ_HEADROOM_MARGIN_DB, 8);
  });

  it("fails closed to Flat when an enabled band reaches Nyquist", () => {
    const response = calculateEqualizerResponse(profile([band(1, 20_000, 8)]), 32_000);
    expect(response.incompatibleBandPosition).toBe(1);
    expect(response.safetyTrimDb).toBe(0);
    expect(response.appliedDb.every((value) => value === 0)).toBe(true);
  });
});
