import { describe, expect, it } from "vitest";
import {
  EqualizerParseError,
  parseEqualizerText,
  serializeEqualizerText,
} from "./format";

const suppliedProfiles = [
  {
    preamp: -0.7576,
    bands: 8,
    text: `Preamp: -0.7576 dB
Filter 1: ON PK Fc 21 Hz Gain -0.7 dB Q 4.6
Filter 2: ON PK Fc 41 Hz Gain -8.6 dB Q 0.4
Filter 3: ON PK Fc 100 Hz Gain -1.4 dB Q 1.2
Filter 4: ON PK Fc 250 Hz Gain 2 dB Q 0.7
Filter 5: ON PK Fc 1600 Hz Gain 0.4 dB Q 3.7
Filter 6: ON PK Fc 2300 Hz Gain 0.4 dB Q 3.7
Filter 7: ON PK Fc 3600 Hz Gain -1.7 dB Q 3.7
Filter 8: ON PK Fc 5800 Hz Gain -1.6 dB Q 10`,
  },
  {
    preamp: -2.3185,
    bands: 6,
    text: `Preamp: -2.3185 dB
Filter 1: ON PK Fc 31 Hz Gain -4.1 dB Q 0.4
Filter 2: ON PK Fc 170 Hz Gain 2.2 dB Q 1
Filter 3: ON PK Fc 870 Hz Gain -0.9 dB Q 1.3
Filter 4: ON PK Fc 2300 Hz Gain -1.1 dB Q 1.9
Filter 5: ON PK Fc 5600 Hz Gain -1.1 dB Q 6.6
Filter 6: ON PK Fc 6000 Hz Gain 2.6 dB Q 0.8`,
  },
  {
    preamp: -1.7233,
    bands: 4,
    text: `Preamp: -1.7233 dB
Filter 1: ON PK Fc 41 Hz Gain -4.2 dB Q 0.3
Filter 2: ON PK Fc 300 Hz Gain 0.7 dB Q 1.6
Filter 3: ON PK Fc 1400 Hz Gain 0.9 dB Q 3
Filter 4: ON PK Fc 3100 Hz Gain 1.7 dB Q 3.8`,
  },
];

describe("ParametricEQ text", () => {
  it.each(suppliedProfiles)(
    "parses and semantically round-trips the supplied $bands-band profile",
    ({ preamp, bands, text }) => {
      const parsed = parseEqualizerText(text);
      expect(parsed.preamp_db).toBe(preamp);
      expect(parsed.bands).toHaveLength(bands);
      expect(parsed.bands.map((band) => band.position)).toEqual(
        Array.from({ length: bands }, (_, index) => index + 1),
      );
      expect(parseEqualizerText(serializeEqualizerText(parsed))).toEqual(parsed);
    },
  );

  it("accepts BOM, CRLF, comments and disabled PK filters", () => {
    const parsed = parseEqualizerText(
      "\uFEFF# exported\r\n; retained\r\nFilter 7: OFF pk Fc 123.5 Hz Gain -2.25 dB Q 1.75\r\n",
    );
    expect(parsed.preamp_db).toBe(0);
    expect(parsed.warnings).toHaveLength(1);
    expect(parsed.bands[0]).toMatchObject({
      position: 1,
      enabled: false,
      frequency_hz: 123.5,
      gain_db: -2.25,
      q: 1.75,
    });
  });

  it("preserves round-trip-safe decimal precision", () => {
    const source =
      "Preamp: -0.123456789012345 dB\nFilter 1: ON PK Fc 1234.567890123 Hz Gain 2.345678901234 dB Q 1.234567890123\n";
    const first = parseEqualizerText(source);
    expect(parseEqualizerText(serializeEqualizerText(first))).toEqual(first);
  });

  it.each([
    "Include: secret.txt",
    "Filter 1: ON LS Fc 100 Hz Gain 1 dB Q 1",
    "Filter 1: ON PK Fc NaN Hz Gain 1 dB Q 1",
    "Filter 1: ON PK Fc 100 Hz Gain 99 dB Q 1",
  ])("rejects unsupported or malformed input atomically: %s", (text) => {
    expect(() => parseEqualizerText(text)).toThrow(EqualizerParseError);
  });
});
