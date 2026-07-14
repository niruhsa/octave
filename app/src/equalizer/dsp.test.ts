import { describe, expect, it } from "vitest";
import {
  calculateEqualizerResponse,
  dbToLinear,
  EQ_HEADROOM_MARGIN_DB,
  equalizerProfileAudioSignature,
  extraPercentToDb,
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
  it("deduplicates refreshed snapshots by audible profile values", () => {
    const now = new Date().toISOString();
    const original: EqualizerProfile = {
      id: "profile-1",
      name: "Headphones",
      format_version: 1,
      preamp_db: -2,
      auto_headroom_enabled: true,
      bands: [band(1, 1_000, 3, 1.2)],
      revision: "4",
      created_at: now,
      updated_at: now,
      source: "synced",
    };
    const refreshed = {
      ...original,
      bands: original.bands.map((value) => ({ ...value })),
      source: undefined,
      unsynced: false,
    };
    expect(equalizerProfileAudioSignature(original, false)).toBe(
      equalizerProfileAudioSignature(refreshed, false),
    );
    refreshed.bands[0].gain_db = 4;
    expect(equalizerProfileAudioSignature(original, false)).not.toBe(
      equalizerProfileAudioSignature(refreshed, false),
    );
    expect(equalizerProfileAudioSignature(original, true)).toBe("flat");
  });

  it("maps output-rule tone percentages to bounded amplitude gain", () => {
    expect(extraPercentToDb(0)).toBe(0);
    expect(extraPercentToDb(100)).toBeCloseTo(20 * Math.log10(2), 10);
    expect(extraPercentToDb(200)).toBeCloseTo(extraPercentToDb(100), 10);
    expect(equalizerProfileAudioSignature(null, false, 25, 0)).not.toBe("flat");
    expect(equalizerProfileAudioSignature(null, false, 25, 0)).not.toBe(
      equalizerProfileAudioSignature(null, false, 50, 0),
    );
  });

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
