// Shared Web Audio graph for crossfade, loudness normalization, and output EQ.
//
//     element A -> fade A -> replay A --\
//                                       +-> mix -> dual-bank EQ -> master -> output
//     element B -> fade B -> replay B --/
//
// Per-element fade/replay nodes preserve the existing gapless/crossfade and
// ReplayGain semantics. One post-mix EQ corrects the selected output equally
// for both sides of a crossfade. Flat is a unity bank, never graph destruction.

import {
  buildFrequencyGrid,
  dbToLinear,
  EQ_HEADROOM_MARGIN_DB,
  equalizerProfileAudioSignature,
  extraPercentToDb,
} from "../equalizer/dsp";
import {
  cloneEqualizerProfile,
  EQ_FORMAT_VERSION,
  EQ_LIMITS,
  type EqualizerProfile,
} from "../equalizer/types";
import { playbackPrefs, type PlaybackPrefs } from "../settings/playback";
import type { QueueItem } from "./store";

type Nodes = {
  source: MediaElementAudioSourceNode;
  fade: GainNode;
  replay: GainNode;
};

type EqualizerRequest = {
  profile: EqualizerProfile | null;
  bypassed: boolean;
  bassBoostPercent: number;
  trebleBoostPercent: number;
};

export type EqualizerGraphCapability = "pending" | "supported" | "unsupported";

export type EqualizerGraphDiagnostics = {
  capability: EqualizerGraphCapability;
  sampleRate: number | null;
  requestedProfileId: string | null;
  appliedProfileId: string | null;
  appliedProfileName: string;
  bypassed: boolean;
  peakResponseDb: number;
  safetyTrimDb: number;
  effectivePreampDb: number;
  warning: string | null;
};

type EqualizerBank = {
  input: GainNode;
  filters: BiquadFilterNode[];
  output: GainNode;
  diagnostics: EqualizerGraphDiagnostics;
};

type BankTransition = {
  token: number;
  old: EqualizerBank;
  next: EqualizerBank;
  endTime: number;
};

const EQ_SWITCH_SECONDS = 0.04;

let ctx: AudioContext | null = null;
let mix: GainNode | null = null;
let master: GainNode | null = null;
let masterValue = 1;
let unavailable = false;
const graphs = new WeakMap<HTMLAudioElement, Nodes>();
let attachedCount = 0;
let attachmentFailed = false;

let desiredEqualizer: EqualizerRequest = {
  profile: null,
  bypassed: true,
  bassBoostPercent: 0,
  trebleBoostPercent: 0,
};
let desiredEqualizerSignature = "flat";
let activeBank: EqualizerBank | null = null;
let transition: BankTransition | null = null;
let pendingEqualizer: EqualizerRequest | null = null;
let transitionToken = 0;
const diagnosticsListeners = new Set<(diagnostics: EqualizerGraphDiagnostics) => void>();
let diagnostics: EqualizerGraphDiagnostics = {
  capability: "pending",
  sampleRate: null,
  requestedProfileId: null,
  appliedProfileId: null,
  appliedProfileName: "Flat",
  bypassed: true,
  peakResponseDb: 0,
  safetyTrimDb: 0,
  effectivePreampDb: 0,
  warning: null,
};

function publishDiagnostics(next: EqualizerGraphDiagnostics): void {
  diagnostics = next;
  diagnosticsListeners.forEach((listener) => listener(next));
}

function pendingDiagnostics(request: EqualizerRequest): EqualizerGraphDiagnostics {
  const unsupported = unavailable || attachmentFailed;
  return {
    capability: unsupported ? "unsupported" : "pending",
    sampleRate: ctx?.sampleRate ?? null,
    requestedProfileId: request.profile?.id ?? null,
    appliedProfileId: null,
    appliedProfileName: "Flat",
    bypassed:
      request.bypassed ||
      (request.profile == null && request.bassBoostPercent === 0 && request.trebleBoostPercent === 0),
    peakResponseDb: 0,
    safetyTrimDb: 0,
    effectivePreampDb: 0,
    warning: unsupported
      ? "Web Audio could not attach to both playback elements; equalizer is unavailable."
      : "Equalizer will initialize with the playback audio graph.",
  };
}

function isSupportedProfile(profile: EqualizerProfile): boolean {
  return (
    profile.format_version === EQ_FORMAT_VERSION &&
    profile.bands.length >= 1 &&
    profile.bands.length <= EQ_LIMITS.bands &&
    Number.isFinite(profile.preamp_db) &&
    profile.preamp_db >= EQ_LIMITS.preampDb.min &&
    profile.preamp_db <= EQ_LIMITS.preampDb.max &&
    profile.bands.every(
      (band, index) =>
        band.position === index + 1 &&
        band.filter_kind === "peaking" &&
        Number.isFinite(band.frequency_hz) &&
        band.frequency_hz >= EQ_LIMITS.frequencyHz.min &&
        band.frequency_hz <= EQ_LIMITS.frequencyHz.max &&
        Number.isFinite(band.gain_db) &&
        band.gain_db >= EQ_LIMITS.gainDb.min &&
        band.gain_db <= EQ_LIMITS.gainDb.max &&
        Number.isFinite(band.q) &&
        band.q >= EQ_LIMITS.q.min &&
        band.q <= EQ_LIMITS.q.max,
    )
  );
}

function flatBank(
  c: AudioContext,
  request: EqualizerRequest,
  warning: string | null = null,
  initialOutputGain = 1,
): EqualizerBank {
  if (!mix || !master) throw new Error("audio graph not initialized");
  const input = c.createGain();
  const output = c.createGain();
  input.gain.value = 1;
  // A standby branch must be silent before it is connected. In particular,
  // WebKit may render between graph mutations; connecting at the GainNode's
  // default value (1) and muting it afterward can leak one full-scale quantum.
  output.gain.value = initialOutputGain;
  input.connect(output);
  mix.connect(input);
  output.connect(master);
  return {
    input,
    filters: [],
    output,
    diagnostics: {
      capability: attachmentFailed ? "unsupported" : "supported",
      sampleRate: c.sampleRate,
      requestedProfileId: request.profile?.id ?? null,
      appliedProfileId: null,
      appliedProfileName: "Flat",
      bypassed: true,
      peakResponseDb: 0,
      safetyTrimDb: 0,
      effectivePreampDb: 0,
      warning,
    },
  };
}

/** Build a complete connected bank. The caller owns its output envelope. */
function buildBank(
  c: AudioContext,
  request: EqualizerRequest,
  initialOutputGain = 1,
): EqualizerBank {
  const profile = request.profile;
  const hasTone = request.bassBoostPercent > 0 || request.trebleBoostPercent > 0;
  if (request.bypassed || (!profile && !hasTone)) {
    return flatBank(c, request, null, initialOutputGain);
  }
  if (profile && !isSupportedProfile(profile)) {
    return flatBank(
      c,
      request,
      "This profile format is unsupported or invalid; playing Flat.",
      initialOutputGain,
    );
  }

  const incompatible = profile?.bands.find(
    (band) => band.enabled && band.frequency_hz >= c.sampleRate / 2,
  );
  if (incompatible) {
    return flatBank(
      c,
      request,
      `Band ${incompatible.position} (${incompatible.frequency_hz} Hz) is at or above this output's Nyquist limit; playing Flat.`,
      initialOutputGain,
    );
  }
  if (!mix || !master) throw new Error("audio graph not initialized");

  const input = c.createGain();
  const output = c.createGain();
  // Set before connecting for the same render-quantum safety as flatBank.
  output.gain.value = initialOutputGain;
  const filters = (profile?.bands ?? [])
    .filter((band) => band.enabled)
    .map((band) => {
      const node = c.createBiquadFilter();
      node.type = "peaking";
      node.frequency.value = band.frequency_hz;
      node.gain.value = band.gain_db;
      node.Q.value = band.q;
      return node;
    });
  if (request.bassBoostPercent > 0) {
    const bass = c.createBiquadFilter();
    bass.type = "lowshelf";
    bass.frequency.value = 120;
    bass.gain.value = extraPercentToDb(request.bassBoostPercent);
    filters.push(bass);
  }
  if (request.trebleBoostPercent > 0) {
    const treble = c.createBiquadFilter();
    treble.type = "highshelf";
    treble.frequency.value = Math.min(8_000, c.sampleRate * 0.4);
    treble.gain.value = extraPercentToDb(request.trebleBoostPercent);
    filters.push(treble);
  }

  let previous: AudioNode = input;
  for (const filter of filters) {
    previous.connect(filter);
    previous = filter;
  }
  previous.connect(output);
  mix.connect(input);
  output.connect(master);

  // Use the actual Web Audio nodes and sample rate for runtime headroom. The
  // editor's pure RBJ helper is only a pre-context approximation.
  const frequencies = Float32Array.from(buildFrequencyGrid(profile?.bands ?? [], c.sampleRate));
  const aggregateDb = new Float64Array(frequencies.length);
  const storedPreampDb = profile?.preamp_db ?? 0;
  aggregateDb.fill(storedPreampDb);
  const magnitude = new Float32Array(frequencies.length);
  const phase = new Float32Array(frequencies.length);
  for (const filter of filters) {
    filter.getFrequencyResponse(frequencies, magnitude, phase);
    for (let index = 0; index < magnitude.length; index += 1) {
      aggregateDb[index] += 20 * Math.log10(Math.max(magnitude[index], Number.EPSILON));
    }
  }
  const peakResponseDb = aggregateDb.length > 0 ? Math.max(...aggregateDb) : storedPreampDb;
  const safetyTrimDb =
    (hasTone || profile?.auto_headroom_enabled) && peakResponseDb > 0
      ? -(peakResponseDb + EQ_HEADROOM_MARGIN_DB)
      : 0;
  const effectivePreampDb = storedPreampDb + safetyTrimDb;
  input.gain.value = dbToLinear(effectivePreampDb);

  return {
    input,
    filters,
    output,
    diagnostics: {
      capability: "supported",
      sampleRate: c.sampleRate,
      requestedProfileId: profile?.id ?? null,
      appliedProfileId: profile?.id ?? null,
      appliedProfileName: profile ? profile.name : "Output tone",
      bypassed: false,
      peakResponseDb,
      safetyTrimDb,
      effectivePreampDb,
      warning: null,
    },
  };
}

function disposeBank(bank: EqualizerBank): void {
  try {
    mix?.disconnect(bank.input);
  } catch {
    /* already disconnected */
  }
  try {
    bank.input.disconnect();
  } catch {
    /* already disconnected */
  }
  for (const filter of bank.filters) {
    try {
      filter.disconnect();
    } catch {
      /* already disconnected */
    }
  }
  try {
    bank.output.disconnect();
  } catch {
    /* already disconnected */
  }
}

function settleTransition(): void {
  const current = transition;
  if (!current) return;
  current.old.output.gain.cancelScheduledValues(0);
  current.next.output.gain.cancelScheduledValues(0);
  current.old.output.gain.value = 0;
  current.next.output.gain.value = 1;
  disposeBank(current.old);
  activeBank = current.next;
  transition = null;
}

function replaceBankImmediately(request: EqualizerRequest): void {
  if (!ctx) return;
  settleTransition();
  if (activeBank) disposeBank(activeBank);
  activeBank = buildBank(ctx, request);
  activeBank.output.gain.value = 1;
  pendingEqualizer = null;
  publishDiagnostics(activeBank.diagnostics);
}

function finishTransition(token: number): void {
  const c = ctx;
  const current = transition;
  if (!c || !current || current.token !== token) return;
  if (c.state !== "running") {
    settleTransition();
  } else if (c.currentTime + 0.002 < current.endTime) {
    // Background WebViews throttle timers; audio time, not wall time, decides
    // when a scheduled ramp is safe to dispose.
    window.setTimeout(() => finishTransition(token), 12);
    return;
  } else {
    current.old.output.gain.value = 0;
    current.next.output.gain.value = 1;
    disposeBank(current.old);
    activeBank = current.next;
    transition = null;
  }
  if (activeBank) publishDiagnostics(activeBank.diagnostics);
  const pending = pendingEqualizer;
  pendingEqualizer = null;
  if (pending) switchBank(pending);
}

function switchBank(request: EqualizerRequest): void {
  const c = ctx;
  if (!c || !mix || !master) return;
  if (c.state !== "running") {
    // With frozen audio time there is nothing to crossfade. Settle directly so
    // the next resume starts from the latest complete bank.
    replaceBankImmediately(request);
    return;
  }
  if (transition) {
    // Keep at most two banks. Rapid edits and route events are latest-wins.
    pendingEqualizer = request;
    return;
  }
  if (!activeBank) {
    replaceBankImmediately(request);
    return;
  }

  const next = buildBank(c, request, 0);
  const now = c.currentTime;
  const midpoint = now + EQ_SWITCH_SECONDS / 2;
  const endTime = now + EQ_SWITCH_SECONDS;
  next.output.gain.cancelScheduledValues(now);
  next.output.gain.setValueAtTime(0, now);
  next.output.gain.setValueAtTime(0, midpoint);
  next.output.gain.linearRampToValueAtTime(1, endTime);
  activeBank.output.gain.cancelScheduledValues(now);
  activeBank.output.gain.setValueAtTime(1, now);
  activeBank.output.gain.linearRampToValueAtTime(0, midpoint);
  activeBank.output.gain.setValueAtTime(0, endTime);
  transitionToken += 1;
  const token = transitionToken;
  transition = { token, old: activeBank, next, endTime };
  // Fade out, then fade in. Different IIR banks do not have complementary
  // phase, so overlapping them can comb-filter or spike even when their gain
  // envelopes add to one. The short zero crossing is click-free and avoids
  // summing two differently filtered copies on WebKit/macOS.
  window.setTimeout(() => finishTransition(token), EQ_SWITCH_SECONDS * 1000 + 12);
}

function forceFlatUnsupported(): void {
  if (!ctx) {
    publishDiagnostics(pendingDiagnostics(desiredEqualizer));
    return;
  }
  replaceBankImmediately({
    profile: null,
    bypassed: true,
    bassBoostPercent: 0,
    trebleBoostPercent: 0,
  });
  publishDiagnostics({
    ...diagnostics,
    capability: "unsupported",
    requestedProfileId: desiredEqualizer.profile?.id ?? null,
    warning: "Web Audio could not attach to both playback elements; equalizer is unavailable.",
  });
}

function ensureContext(): AudioContext | null {
  if (unavailable) return null;
  if (ctx) return ctx;
  try {
    const Ctor: typeof AudioContext | undefined =
      window.AudioContext ??
      (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctor) {
      unavailable = true;
      publishDiagnostics(pendingDiagnostics(desiredEqualizer));
      return null;
    }
    ctx = new Ctor();
    mix = ctx.createGain();
    master = ctx.createGain();
    mix.gain.value = 1;
    master.gain.value = masterValue;
    master.connect(ctx.destination);
    // Start Flat until both deck elements prove they can join this graph.
    activeBank = flatBank(ctx, {
      profile: null,
      bypassed: true,
      bassBoostPercent: 0,
      trebleBoostPercent: 0,
    });
    activeBank.output.gain.value = 1;
    return ctx;
  } catch {
    unavailable = true;
    publishDiagnostics(pendingDiagnostics(desiredEqualizer));
    return null;
  }
}

/**
 * Tap an element once. If either persistent deck element cannot attach, EQ is
 * globally disabled rather than correcting only one side of a handoff.
 */
export function attach(el: HTMLAudioElement): boolean {
  const c = ensureContext();
  if (!c || !mix) return false;
  if (graphs.has(el)) return true;
  try {
    const source = c.createMediaElementSource(el);
    const fade = c.createGain();
    const replay = c.createGain();
    source.connect(fade);
    fade.connect(replay);
    replay.connect(mix);
    fade.gain.value = 1;
    replay.gain.value = 1;
    graphs.set(el, { source, fade, replay });
    attachedCount += 1;
    if (attachedCount >= 2 && !attachmentFailed) switchBank(desiredEqualizer);
    return true;
  } catch {
    attachmentFailed = true;
    forceFlatUnsupported();
    return false;
  }
}

export function hasGraph(el: HTMLAudioElement): boolean {
  return graphs.has(el);
}

/** Resume after a user gesture. Profile changes made while suspended are settled already. */
export function resume(): void {
  if (ctx && ctx.state === "suspended") void ctx.resume();
}

/** Global volume (0..1), after EQ. */
export function setMaster(v: number): void {
  masterValue = Math.max(0, v);
  if (master) master.gain.value = masterValue;
}

/** Crossfade envelope for one deck element. */
export function setFade(el: HTMLAudioElement, v: number): void {
  const graph = graphs.get(el);
  if (graph) graph.fade.gain.value = Math.max(0, v);
}

/** Per-track ReplayGain multiplier (may exceed 1). */
export function setReplay(el: HTMLAudioElement, v: number): void {
  const graph = graphs.get(el);
  if (graph) graph.replay.gain.value = Math.max(0, v);
}

/**
 * Select the shared profile plus the active output rule's bounded tone shelves.
 * Bypassed means true Flat; a null profile can still carry output tone.
 */
export function setEqualizerProfile(
  profile: EqualizerProfile | null,
  options: {
    bypassed?: boolean;
    bassBoostPercent?: number;
    trebleBoostPercent?: number;
  } = {},
): void {
  const boundedPercent = (value: number | undefined) =>
    Math.round(Math.min(100, Math.max(0, Number.isFinite(value) ? (value ?? 0) : 0)));
  const bassBoostPercent = boundedPercent(options.bassBoostPercent);
  const trebleBoostPercent = boundedPercent(options.trebleBoostPercent);
  const next = {
    profile: profile ? cloneEqualizerProfile(profile) : null,
    bypassed:
      options.bypassed ?? (profile == null && bassBoostPercent === 0 && trebleBoostPercent === 0),
    bassBoostPercent,
    trebleBoostPercent,
  };
  const signature = equalizerProfileAudioSignature(
    next.profile,
    next.bypassed,
    next.bassBoostPercent,
    next.trebleBoostPercent,
  );
  // Snapshot polling returns fresh objects. Do not turn an identical profile
  // into a recurring live-bank transition every time account state refreshes.
  if (signature === desiredEqualizerSignature) {
    desiredEqualizer = next;
    return;
  }
  desiredEqualizer = next;
  desiredEqualizerSignature = signature;
  if (!ctx || attachedCount < 2) {
    publishDiagnostics(pendingDiagnostics(desiredEqualizer));
    return;
  }
  if (attachmentFailed) {
    forceFlatUnsupported();
    return;
  }
  switchBank(desiredEqualizer);
}

export function equalizerDiagnostics(): EqualizerGraphDiagnostics {
  return diagnostics;
}

export function onEqualizerDiagnostics(
  listener: (next: EqualizerGraphDiagnostics) => void,
): () => void {
  diagnosticsListeners.add(listener);
  listener(diagnostics);
  return () => diagnosticsListeners.delete(listener);
}

/**
 * Per-track gain from loudness metadata and current preferences. The existing
 * sample-peak guard remains intact; shared EQ headroom is a separate downstream
 * steady-state model and does not claim transient/true-peak protection.
 */
export function trackGain(item: QueueItem | null, prefs: PlaybackPrefs = playbackPrefs()): number {
  if (!item || prefs.loudnessMode === "off") return 1;
  const ref =
    prefs.loudnessMode === "album"
      ? item.album_loudness_lufs ?? item.loudness_lufs
      : item.loudness_lufs;
  if (ref == null) return 1;
  const gainDb = prefs.loudnessTargetLufs - ref + prefs.loudnessPreampDb;
  let gain = 10 ** (gainDb / 20);
  const peak = item.loudness_peak;
  if (peak != null && peak > 0) gain = Math.min(gain, 1 / peak);
  return gain;
}
