import {
  EQ_LIMITS,
  type EqualizerBand,
  type EqualizerProfile,
  type ParsedEqualizerText,
} from "./types";

const NUMBER = "[+-]?(?:\\d+(?:\\.\\d*)?|\\.\\d+)(?:[eE][+-]?\\d+)?";
const PREAMP_RE = new RegExp(`^Preamp\\s*:\\s*(${NUMBER})\\s*dB\\s*$`, "i");
const FILTER_RE = new RegExp(
  `^Filter\\s+(\\d+)\\s*:\\s*(ON|OFF)\\s+(\\S+)\\s+Fc\\s+(${NUMBER})\\s+Hz\\s+Gain\\s+(${NUMBER})\\s+dB\\s+Q\\s+(${NUMBER})\\s*$`,
  "i",
);

export type EqualizerParseErrorCode =
  | "too_large"
  | "too_many_lines"
  | "duplicate_preamp"
  | "duplicate_filter"
  | "invalid_filter_number"
  | "unsupported_filter"
  | "invalid_number"
  | "out_of_range"
  | "too_many_filters"
  | "missing_filters"
  | "unknown_line";

export class EqualizerParseError extends Error {
  readonly code: EqualizerParseErrorCode;
  readonly line: number | null;

  constructor(code: EqualizerParseErrorCode, message: string, line: number | null = null) {
    super(line == null ? message : `Line ${line}: ${message}`);
    this.name = "EqualizerParseError";
    this.code = code;
    this.line = line;
  }
}

function checkedNumber(
  source: string,
  field: string,
  line: number,
  min: number,
  max: number,
): number {
  const value = Number(source);
  if (!Number.isFinite(value)) {
    throw new EqualizerParseError("invalid_number", `${field} must be finite`, line);
  }
  if (value < min || value > max) {
    throw new EqualizerParseError(
      "out_of_range",
      `${field} must be between ${min} and ${max}`,
      line,
    );
  }
  return Object.is(value, -0) ? 0 : value;
}

/**
 * Parse the strict version-1 ParametricEQ subset. Parsing is atomic: no partial
 * profile is returned when any non-comment line is unsupported or malformed.
 */
export function parseEqualizerText(input: string): ParsedEqualizerText {
  const bytes = new TextEncoder().encode(input).byteLength;
  if (bytes > EQ_LIMITS.importBytes) {
    throw new EqualizerParseError(
      "too_large",
      `Equalizer text exceeds ${EQ_LIMITS.importBytes / 1024} KiB`,
    );
  }

  const normalized = input.startsWith("\uFEFF") ? input.slice(1) : input;
  const lines = normalized.split(/\r?\n/);
  if (lines.length > EQ_LIMITS.importLines) {
    throw new EqualizerParseError(
      "too_many_lines",
      `Equalizer text exceeds ${EQ_LIMITS.importLines} lines`,
    );
  }

  let preamp: number | null = null;
  const filterNumbers = new Set<number>();
  const bands: EqualizerBand[] = [];

  for (let index = 0; index < lines.length; index += 1) {
    const lineNo = index + 1;
    const line = lines[index].trim();
    if (line === "" || line.startsWith("#") || line.startsWith(";")) continue;

    const preampMatch = PREAMP_RE.exec(line);
    if (preampMatch) {
      if (preamp != null) {
        throw new EqualizerParseError("duplicate_preamp", "Preamp may appear only once", lineNo);
      }
      preamp = checkedNumber(
        preampMatch[1],
        "Preamp",
        lineNo,
        EQ_LIMITS.preampDb.min,
        EQ_LIMITS.preampDb.max,
      );
      continue;
    }

    const filterMatch = FILTER_RE.exec(line);
    if (filterMatch) {
      const number = Number(filterMatch[1]);
      if (!Number.isSafeInteger(number) || number <= 0) {
        throw new EqualizerParseError(
          "invalid_filter_number",
          "Filter number must be a positive integer",
          lineNo,
        );
      }
      if (filterNumbers.has(number)) {
        throw new EqualizerParseError(
          "duplicate_filter",
          `Filter number ${number} is duplicated`,
          lineNo,
        );
      }
      if (filterMatch[3].toUpperCase() !== "PK") {
        throw new EqualizerParseError(
          "unsupported_filter",
          `Filter kind ${filterMatch[3]} is unsupported; version 1 accepts PK only`,
          lineNo,
        );
      }
      if (bands.length >= EQ_LIMITS.bands) {
        throw new EqualizerParseError(
          "too_many_filters",
          `A profile may contain at most ${EQ_LIMITS.bands} filters`,
          lineNo,
        );
      }

      filterNumbers.add(number);
      bands.push({
        position: bands.length + 1,
        enabled: filterMatch[2].toUpperCase() === "ON",
        filter_kind: "peaking",
        frequency_hz: checkedNumber(
          filterMatch[4],
          "Frequency",
          lineNo,
          EQ_LIMITS.frequencyHz.min,
          EQ_LIMITS.frequencyHz.max,
        ),
        gain_db: checkedNumber(
          filterMatch[5],
          "Gain",
          lineNo,
          EQ_LIMITS.gainDb.min,
          EQ_LIMITS.gainDb.max,
        ),
        q: checkedNumber(filterMatch[6], "Q", lineNo, EQ_LIMITS.q.min, EQ_LIMITS.q.max),
      });
      continue;
    }

    // Give unsupported filter shapes a more useful error than a generic line
    // while still rejecting OFF filters (dropping them would break round-trip).
    if (/^Filter\b/i.test(line)) {
      throw new EqualizerParseError(
        "unsupported_filter",
        "Unsupported or malformed filter; expected `Filter N: ON|OFF PK Fc … Hz Gain … dB Q …`",
        lineNo,
      );
    }
    throw new EqualizerParseError("unknown_line", "Unsupported equalizer directive", lineNo);
  }

  if (bands.length === 0) {
    throw new EqualizerParseError("missing_filters", "An equalizer profile needs at least one filter");
  }

  return {
    preamp_db: preamp ?? 0,
    bands,
    warnings:
      preamp == null
        ? [{ code: "missing_preamp", message: "No Preamp line was present; using 0 dB." }]
        : [],
  };
}

function finiteDecimal(value: number): string {
  if (!Number.isFinite(value)) throw new TypeError("Equalizer values must be finite");
  return Object.is(value, -0) ? "0" : value.toString();
}

/** Canonical, lossless version-1 export. */
export function serializeEqualizerText(
  profile: Pick<EqualizerProfile, "preamp_db" | "bands"> | ParsedEqualizerText,
): string {
  const lines = [`Preamp: ${finiteDecimal(profile.preamp_db)} dB`];
  profile.bands.forEach((band, index) => {
    if (band.filter_kind !== "peaking") {
      throw new TypeError(`Unsupported filter kind: ${band.filter_kind}`);
    }
    lines.push(
      `Filter ${index + 1}: ${band.enabled ? "ON" : "OFF"} PK Fc ${finiteDecimal(band.frequency_hz)} Hz Gain ${finiteDecimal(band.gain_db)} dB Q ${finiteDecimal(band.q)}`,
    );
  });
  return `${lines.join("\n")}\n`;
}

/** Safe display proposal from an imported file name; never silently overwrites. */
export function profileNameFromFile(fileName: string): string {
  const stem = fileName.replace(/\.[^.]*$/, "");
  const clean = [...stem]
    .filter((char) => {
      const code = char.codePointAt(0) ?? 0;
      return code !== 0 && !(code <= 0x1f || (code >= 0x7f && code <= 0x9f));
    })
    .join("")
    .trim();
  return [...(clean || "Imported equalizer")].slice(0, EQ_LIMITS.nameScalars).join("");
}
