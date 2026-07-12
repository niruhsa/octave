// Web Audio gain graph for loudness normalization (Phase 16 — ReplayGain / EBU
// R128) + crossfade.
//
// The player plays two persistent `<audio>` elements (see `deck.ts`). This
// module taps each through a Web Audio graph so gain can exceed 1.0 — true
// ReplayGain that can *raise* a quiet track, which a bare `HTMLAudioElement.volume`
// (clamped [0,1]) cannot. Per element the chain is:
//
//     MediaElementSource → fade (crossfade envelope) → replay (per-track gain)
//                        → master (global volume) → destination
//
// `master` is a single shared node (global volume); `fade` and `replay` are
// per-element. Once `createMediaElementSource(el)` is called the element's audio
// routes through the graph, so the deck must NOT also write `el.volume` for a
// graphed element — the nodes own the gain. Requires the media stream to be
// CORS-clean (the loopback server sends `Access-Control-Allow-Origin` and the
// elements set `crossOrigin="anonymous"`), else the tap taints to silence.
//
// If Web Audio is unavailable the module reports `hasGraph(el) === false` and the
// deck falls back to the legacy `el.volume` path (attenuation-only, no boost).

import { playbackPrefs, type PlaybackPrefs } from "../settings/playback";
import type { QueueItem } from "./store";

type Nodes = {
  source: MediaElementAudioSourceNode;
  fade: GainNode;
  replay: GainNode;
};

let ctx: AudioContext | null = null;
let master: GainNode | null = null;
let unavailable = false;
const graphs = new WeakMap<HTMLAudioElement, Nodes>();

function ensureContext(): AudioContext | null {
  if (unavailable) return null;
  if (ctx) return ctx;
  try {
    const Ctor: typeof AudioContext | undefined =
      window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctor) {
      unavailable = true;
      return null;
    }
    ctx = new Ctor();
    master = ctx.createGain();
    master.gain.value = 1;
    master.connect(ctx.destination);
    return ctx;
  } catch {
    unavailable = true;
    return null;
  }
}

/**
 * Build (once) the node chain for `el`. Idempotent — a second call for the same
 * element returns the existing chain (so StrictMode/HMR re-binds don't call
 * `createMediaElementSource` twice, which throws). Returns `false` when Web
 * Audio is unavailable so the deck degrades to `el.volume`.
 */
export function attach(el: HTMLAudioElement): boolean {
  const c = ensureContext();
  if (!c || !master) return false;
  if (graphs.has(el)) return true;
  try {
    const source = c.createMediaElementSource(el);
    const fade = c.createGain();
    const replay = c.createGain();
    source.connect(fade);
    fade.connect(replay);
    replay.connect(master);
    fade.gain.value = 1;
    replay.gain.value = 1;
    graphs.set(el, { source, fade, replay });
    return true;
  } catch {
    // createMediaElementSource can throw (already tapped, or blocked) — degrade.
    return false;
  }
}

/** Whether `el` is routed through the graph (vs. the `el.volume` fallback). */
export function hasGraph(el: HTMLAudioElement): boolean {
  return graphs.has(el);
}

/** Resume the context after a user gesture (autoplay policy). No-op otherwise. */
export function resume(): void {
  if (ctx && ctx.state === "suspended") void ctx.resume();
}

/** Global volume (0..1). */
export function setMaster(v: number): void {
  if (master) master.gain.value = Math.max(0, v);
}

/** Crossfade envelope for `el` (0..1). */
export function setFade(el: HTMLAudioElement, v: number): void {
  const g = graphs.get(el);
  if (g) g.fade.gain.value = Math.max(0, v);
}

/** Per-track ReplayGain multiplier for `el` (may exceed 1). */
export function setReplay(el: HTMLAudioElement, v: number): void {
  const g = graphs.get(el);
  if (g) g.replay.gain.value = Math.max(0, v);
}

/**
 * The per-track linear gain from a track's loudness + the current prefs. `1`
 * means unity (no change): normalization off, an unmeasured track, or an
 * episode. In `album` mode the album's loudness is the reference (so intra-album
 * dynamics survive); `track` mode uses the track's own. A peak-based clip guard
 * keeps the gained signal from exceeding 0 dBFS.
 */
export function trackGain(item: QueueItem | null, prefs: PlaybackPrefs = playbackPrefs()): number {
  if (!item || prefs.loudnessMode === "off") return 1;
  const ref =
    prefs.loudnessMode === "album"
      ? item.album_loudness_lufs ?? item.loudness_lufs
      : item.loudness_lufs;
  if (ref == null) return 1; // unmeasured → play unchanged
  const gainDb = prefs.loudnessTargetLufs - ref + prefs.loudnessPreampDb;
  let g = Math.pow(10, gainDb / 20);
  // Clip guard: never push the track's own peak past full scale.
  const peak = item.loudness_peak;
  if (peak != null && peak > 0) g = Math.min(g, 1 / peak);
  return g;
}
