// Dual-<audio> playback deck: gapless track handoff + optional crossfade.
//
// Two persistent elements with swappable roles. The *active* element is
// audible and its events drive the player store; the *standby* element
// silently preloads the upcoming track so the boundary is a local,
// already-buffered play() instead of the old teardown-and-reload (an audible
// 50–300 ms gap). Design doc: ../../GAPLESS_CROSSFADE.md.
//
// The store owns queue logic (what plays next, records, persistence); the
// deck owns element mechanics (preload, handoff, fades). Every boundary the
// deck can't take (standby not ready, feature off, errors) falls back to the
// store's legacy load path — pre-deck behavior, unchanged.
//
// Timing rules: never requestAnimationFrame (frozen while the page is hidden —
// screen-off Android). Fades run on setInterval with wall-clock math, so a
// starved/coalesced tick still lands on the correct trajectory; `timeupdate`
// (the crossfade trigger) keeps firing for audible media in the background.

import { playbackPrefs } from "../settings/playback";
import type { QueueItem } from "./store";

/** Fade tick cadence — coarse enough to be cheap, fine enough to be smooth. */
const RAMP_TICK_MS = 50;

/** Manual skips fade briefly even with a long crossfade — skips must feel snappy. */
export const MANUAL_FADE_MAX_SEC = 1.5;

/** Tracks shorter than this never crossfade (a 12 s fade on a 15 s jingle is chaos). */
const MIN_TRACK_SEC_FOR_FADE = 10;

// Gapless tuning note: the swap fires exactly at `ended`. If a consistent
// residual gap is ever measured on target hardware, the fix is a
// high-frequency watchdog (armed by `timeupdate` in the final ~500 ms) that
// starts the standby a few ms *before* the end — an overlap knob, deliberately
// not implemented until measurement demands it (overlap alters musical
// content on continuous albums).

/**
 * iOS WebKit ignores `el.volume` writes — a crossfade there would be a
 * full-volume overlap, so iOS always takes the gapless cut (the platform is
 * best-effort per project docs).
 */
const IS_IOS =
  typeof navigator !== "undefined" && /iP(hone|ad|od)/.test(navigator.userAgent);

export const clamp01 = (n: number) => Math.max(0, Math.min(1, n));

/** Equal-power fade-in gain: in² + out² = 1 → constant perceived loudness. */
export const fadeInGain = (t: number) => Math.sin(clamp01(t) * (Math.PI / 2));

/** Equal-power fade-out gain (see {@link fadeInGain}). */
export const fadeOutGain = (t: number) => Math.cos(clamp01(t) * (Math.PI / 2));

/**
 * Consecutive tracks of one album (same disc, adjacent track numbers) — the
 * "smart album" pair that transitions gaplessly even when crossfade is on, so
 * continuous albums (live records, DJ mixes) are never smeared by a fade.
 */
export function isSmartAlbumPair(out: QueueItem, inc: QueueItem): boolean {
  return (
    out.album_id !== "" &&
    out.album_id === inc.album_id &&
    out.track_no != null &&
    inc.track_no != null &&
    inc.track_no === out.track_no + 1 &&
    (out.disc_no ?? null) === (inc.disc_no ?? null)
  );
}

type Slot = { item: QueueItem; index: number };

export type DeckCallbacks = {
  /** Active element started playing. */
  onPlay: () => void;
  /**
   * Active element paused. `atEnd` = the pause that fires right before
   * `ended` — the store must ignore it (Android wake-lock guard).
   */
  onPause: (atEnd: boolean, posSec: number, durSec: number) => void;
  /** Active element `timeupdate`. */
  onTime: (posSec: number, durSec: number) => void;
  /** Active element learned its duration. */
  onDurationChange: (durSec: number) => void;
  /** Active element errored (playback cannot continue). */
  onActiveError: (code: number | undefined) => void;
  /** A play() call on the active element was rejected. */
  onPlayRejected: (e: unknown) => void;
  /**
   * The deck swapped the preloaded standby in as the new current track at
   * `index`. Fired at handoff start (fade start for crossfades), so the UI
   * flips to the incoming track while the outgoing tail plays out.
   */
  onSwapped: (index: number, item: QueueItem) => void;
  /**
   * The outgoing item of a deck-initiated *natural* handoff finished (ended,
   * or its fade-out completed / was cut short). Fired after `onSwapped` for
   * the same boundary. `reachedEnd` = it played to within 1 s of the end.
   * Manual-skip fades retire silently (no record — matching the legacy skip).
   */
  onRetired: (item: QueueItem, posSec: number, durSec: number, reachedEnd: boolean) => void;
  /**
   * Natural end with no usable standby — the store must record + advance via
   * the legacy load path (exactly the pre-deck `ended` behavior).
   */
  onAdvanceFallback: () => void;
  /**
   * A gapless swap's play() was rejected *after* the swap was announced — the
   * store should reload its (already swapped-to) current track via the legacy
   * path.
   */
  onRecover: () => void;
};

type Retiring = { el: HTMLAudioElement; item: QueueItem; notify: boolean };
type PendingPreload = { item: QueueItem; index: number; url: string };

export class Deck {
  private readonly a: HTMLAudioElement;
  private readonly b: HTMLAudioElement;
  private readonly cb: DeckCallbacks;
  private activeEl: HTMLAudioElement;
  private masterVolume = 1;

  /**
   * What the active element is playing. Null until the first `playNow` — a
   * restored-session prime loads the element without deck bookkeeping, and
   * its boundary deliberately takes the fallback path.
   */
  private current: Slot | null = null;
  /** What the standby element has (or is) preloading. */
  private preloaded: Slot | null = null;
  private standbyReady = false;
  /** A crossfade already started for the current boundary (one per boundary). */
  private handoffArmed = false;
  /** The element fading out after a swap, still audible. */
  private retiring: Retiring | null = null;
  /** Preload requested while both elements were busy (mid-fade) — applied on retire. */
  private pendingPreload: PendingPreload | null = null;

  private readonly ramps = new Map<HTMLAudioElement, number>();
  private readonly unbind: () => void;
  private destroyed = false;

  constructor(a: HTMLAudioElement, b: HTMLAudioElement, cb: DeckCallbacks) {
    this.a = a;
    this.b = b;
    this.cb = cb;
    // Rebinds (StrictMode double-mount, HMR) must adopt whichever element is
    // actually sounding; on a cold start both are idle and `a` wins.
    this.activeEl = !b.paused ? b : a;
    this.activeEl.volume = this.masterVolume;
    this.other(this.activeEl).volume = 0;

    const offs: Array<() => void> = [];
    for (const el of [a, b]) {
      const on = (type: string, fn: () => void) => {
        el.addEventListener(type, fn);
        offs.push(() => el.removeEventListener(type, fn));
      };
      on("play", () => {
        if (el === this.activeEl) this.cb.onPlay();
      });
      on("pause", () => this.handlePause(el));
      on("timeupdate", () => this.handleTime(el));
      on("durationchange", () => {
        if (el === this.activeEl) this.cb.onDurationChange(el.duration || 0);
      });
      on("ended", () => this.handleEnded(el));
      on("error", () => this.handleError(el));
      on("canplaythrough", () => {
        if (el !== this.activeEl && this.preloaded) this.standbyReady = true;
      });
    }
    this.unbind = () => offs.forEach((f) => f());
  }

  /** The audible element — the store aliases its `audio` field to this. */
  get active(): HTMLAudioElement {
    return this.activeEl;
  }

  private other(el: HTMLAudioElement): HTMLAudioElement {
    return el === this.a ? this.b : this.a;
  }

  // ── public transport ───────────────────────────────────────────────────

  /**
   * Load + play `item` on the deck (initial play, manual jumps, fallback
   * advance). `fadeSec > 0` requests a manual-skip crossfade: the outgoing
   * track keeps playing and ramps down on its element while the target loads
   * on the other and ramps in — two independent ramps, no sync to get wrong.
   */
  playNow(item: QueueItem, index: number, url: string, opts?: { fadeSec?: number }) {
    if (this.destroyed) return;
    const outEl = this.activeEl;
    const outgoing = this.current;
    const fadeSec = Math.min(opts?.fadeSec ?? 0, MANUAL_FADE_MAX_SEC);
    const canFade =
      fadeSec > 0 &&
      !IS_IOS &&
      !outEl.paused &&
      !outEl.ended &&
      outgoing != null &&
      outgoing.item.mediaKind !== "episode" &&
      item.mediaKind !== "episode";

    // Either path repurposes the standby: drop its preload state (the store
    // re-arms for the new "next" right after this call).
    this.handoffArmed = false;
    this.preloaded = null;
    this.standbyReady = false;
    this.pendingPreload = null;

    if (!canFade) {
      // Instant cut on the current active element — the legacy switch,
      // byte-for-byte (no explicit pause; assigning src stops the element).
      this.finalizeRetiringNow();
      this.cancelRamp(outEl);
      this.current = { item, index };
      outEl.volume = this.masterVolume;
      outEl.src = url;
      outEl.currentTime = 0;
      this.applyResume(outEl, item);
      void outEl.play().catch((e) => this.cb.onPlayRejected(e));
      return;
    }

    const inEl = this.other(outEl);
    this.finalizeRetiringNow(); // at most one retiring element at a time
    this.cancelRamp(inEl);
    this.current = { item, index };
    this.activeEl = inEl;
    // Silent retire: a manual skip records nothing for the outgoing listen
    // (threshold-based history only), exactly like the legacy instant skip.
    this.retiring = { el: outEl, item: outgoing.item, notify: false };
    inEl.volume = 0;
    inEl.src = url;
    inEl.currentTime = 0;
    this.applyResume(inEl, item);
    void inEl
      .play()
      .then(() => {
        if (this.destroyed || this.activeEl !== inEl) return;
        this.startRamp(inEl, fadeInGain, fadeSec);
      })
      .catch((e) => this.cb.onPlayRejected(e));
    this.startRamp(outEl, fadeOutGain, fadeSec, () => this.finalizeRetiring(outEl));
  }

  /**
   * (Re)arm the standby element for the upcoming item. Idempotent by item id;
   * `null` clears the slot. While a fade is running both elements are busy,
   * so the request parks in `pendingPreload` and applies on retire.
   */
  syncPreload(item: QueueItem | null, index: number, url: string | null) {
    if (this.destroyed) return;
    if (!item || url == null) {
      this.preloaded = null;
      this.standbyReady = false;
      this.pendingPreload = null;
      return;
    }
    if (this.preloaded?.item.id === item.id) return;
    if (this.retiring) {
      this.pendingPreload = { item, index, url };
      return;
    }
    const standby = this.other(this.activeEl);
    this.cancelRamp(standby);
    this.preloaded = { item, index };
    this.standbyReady = false;
    standby.volume = 0;
    standby.preload = "auto";
    standby.src = url;
    standby.load();
    this.applyResume(standby, item);
  }

  /** Master volume 0..1 — active tracks it directly; live ramps rescale next tick. */
  setMasterVolume(v: number) {
    this.masterVolume = clamp01(v);
    if (!this.ramps.has(this.activeEl)) this.activeEl.volume = this.masterVolume;
  }

  /** Pause playback. Mid-fade, the fade resolves instantly (outgoing retired). */
  pause() {
    this.finalizeRetiringNow();
    this.activeEl.pause();
  }

  /** Resume the active element. */
  resume(): Promise<void> {
    return this.activeEl.play();
  }

  /** Seek the active element (allowed mid-fade — it's the incoming track). */
  seekTo(sec: number) {
    this.activeEl.currentTime = sec;
  }

  /** Stop + empty both elements (clearQueue). No retire records — the queue is gone. */
  reset() {
    this.pendingPreload = null;
    this.preloaded = null;
    this.standbyReady = false;
    this.handoffArmed = false;
    this.retiring = null;
    for (const el of [this.a, this.b]) {
      this.cancelRamp(el);
      el.pause();
      el.removeAttribute("src");
      el.load();
      el.volume = el === this.activeEl ? this.masterVolume : 0;
    }
  }

  /** Detach listeners + timers (unbind / StrictMode re-mount). */
  destroy() {
    this.destroyed = true;
    for (const el of [this.a, this.b]) this.cancelRamp(el);
    this.retiring = null;
    this.preloaded = null;
    this.pendingPreload = null;
    this.unbind();
  }

  // ── element events ─────────────────────────────────────────────────────

  private handlePause(el: HTMLAudioElement) {
    if (el !== this.activeEl) return; // standby/retiring pauses are internal
    const atEnd =
      el.ended || (el.duration > 0 && el.currentTime >= el.duration - 0.5);
    this.cb.onPause(atEnd, el.currentTime, el.duration || 0);
  }

  private handleTime(el: HTMLAudioElement) {
    if (el !== this.activeEl) return;
    this.cb.onTime(el.currentTime, el.duration || 0);
    this.maybeStartCrossfade(el);
  }

  private handleEnded(el: HTMLAudioElement) {
    if (el !== this.activeEl) {
      // The fading-out side reached its end before the ramp finished.
      this.finalizeRetiring(el);
      return;
    }
    // Natural end of the active element: gapless swap when the standby is
    // armed + ready, else hand back to the store's legacy advance. (A
    // crossfade boundary never reaches here as active — roles already
    // swapped at fade start.)
    const cur = this.current;
    const pre = this.preloaded;
    if (cur && pre && this.isStandbyReady() && playbackPrefs().gaplessEnabled) {
      this.gaplessSwap(cur, pre);
    } else {
      this.cb.onAdvanceFallback();
    }
  }

  private handleError(el: HTMLAudioElement) {
    if (el === this.activeEl) {
      this.cb.onActiveError(el.error?.code);
      return;
    }
    if (this.retiring?.el === el) {
      this.finalizeRetiring(el);
      return;
    }
    // Standby preload failed — discard silently; the boundary falls back to
    // the legacy path, which surfaces a real error if it also fails.
    if (this.preloaded) {
      this.preloaded = null;
      this.standbyReady = false;
    }
  }

  // ── handoffs ───────────────────────────────────────────────────────────

  private isStandbyReady(): boolean {
    const standby = this.other(this.activeEl);
    return (
      this.preloaded != null &&
      (this.standbyReady || standby.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA)
    );
  }

  private gaplessSwap(cur: Slot, pre: Slot) {
    const outEl = this.activeEl;
    const inEl = this.other(outEl);
    this.cancelRamp(inEl);
    inEl.volume = this.masterVolume;
    const started = inEl.play();
    // Bookkeeping swap — audio start is driven by play() above; callback
    // ordering below doesn't affect the gap. onSwapped before onRetired so
    // the store migrates the outgoing listen's play-record guard first.
    this.preloaded = null;
    this.standbyReady = false;
    this.handoffArmed = false;
    this.retiring = null;
    this.current = pre;
    this.activeEl = inEl;
    this.cb.onSwapped(pre.index, pre.item);
    this.cb.onRetired(
      cur.item,
      outEl.duration || outEl.currentTime,
      outEl.duration || 0,
      true,
    );
    started.catch(() => {
      // The local standby refused to start (rare). The swap is already
      // announced — have the store reload its current track the legacy way.
      if (this.destroyed || this.activeEl !== inEl) return;
      this.cb.onRecover();
    });
  }

  private maybeStartCrossfade(el: HTMLAudioElement) {
    if (this.handoffArmed || el.paused) return;
    const cur = this.current;
    const pre = this.preloaded;
    if (!cur || !pre || !this.isStandbyReady()) return;
    const fadeSec = this.crossfadeFor(cur.item, pre.item, el);
    if (fadeSec <= 0) return;
    const remaining = (el.duration || 0) - el.currentTime;
    // Too early → keep waiting; a hair from the end → let `ended` take it.
    if (remaining > fadeSec || remaining <= 0.1) return;
    this.startCrossfade(cur, pre, Math.min(fadeSec, remaining));
  }

  /** Crossfade seconds for this natural boundary, or 0 when it must be gapless. */
  private crossfadeFor(out: QueueItem, inc: QueueItem, el: HTMLAudioElement): number {
    if (IS_IOS) return 0;
    const prefs = playbackPrefs();
    if (!prefs.gaplessEnabled || !(prefs.crossfadeSec > 0)) return 0;
    if (out.mediaKind === "episode" || inc.mediaKind === "episode") return 0;
    if (prefs.smartAlbumGapless && isSmartAlbumPair(out, inc)) return 0;
    const dur = el.duration;
    if (!Number.isFinite(dur) || dur < MIN_TRACK_SEC_FOR_FADE) return 0;
    // Never fade longer than half the track (short-track sanity).
    return Math.min(prefs.crossfadeSec, dur / 2);
  }

  private startCrossfade(cur: Slot, pre: Slot, fadeSec: number) {
    const outEl = this.activeEl;
    const inEl = this.other(outEl);
    this.handoffArmed = true;
    this.cancelRamp(inEl);
    inEl.volume = 0;
    inEl
      .play()
      .then(() => {
        if (this.destroyed) return;
        // A rapid manual action may have superseded this boundary while
        // play() spun up — if so, silence the stray start and bow out.
        if (this.preloaded !== pre || this.activeEl !== outEl) {
          if (this.activeEl !== inEl) inEl.pause();
          return;
        }
        this.finalizeRetiringNow(); // resolve any older fade first
        this.preloaded = null;
        this.standbyReady = false;
        this.handoffArmed = false; // the incoming track's own boundary re-arms
        this.current = pre;
        this.activeEl = inEl;
        this.retiring = { el: outEl, item: cur.item, notify: true };
        this.cb.onSwapped(pre.index, pre.item);
        this.startRamp(inEl, fadeInGain, fadeSec);
        this.startRamp(outEl, fadeOutGain, fadeSec, () => this.finalizeRetiring(outEl));
      })
      .catch(() => {
        // Couldn't start the incoming side — disarm; `ended` will fall back.
        this.handoffArmed = false;
        this.standbyReady = false;
      });
  }

  /** Settle the fading-out element: silence, stop, surface `onRetired` once. */
  private finalizeRetiring(el: HTMLAudioElement | null) {
    if (!el || this.retiring?.el !== el) return;
    const { item, notify } = this.retiring;
    this.retiring = null;
    this.cancelRamp(el);
    el.pause();
    el.volume = 0;
    if (notify) {
      const dur = el.duration || 0;
      const reachedEnd = el.ended || (dur > 0 && el.currentTime >= dur - 1);
      this.cb.onRetired(item, el.currentTime, dur, reachedEnd);
    }
    // A preload that arrived mid-fade now has a free element.
    const p = this.pendingPreload;
    if (p) {
      this.pendingPreload = null;
      this.syncPreload(p.item, p.index, p.url);
    }
  }

  private finalizeRetiringNow() {
    const r = this.retiring;
    if (r) this.finalizeRetiring(r.el);
  }

  // ── fade engine ────────────────────────────────────────────────────────

  /**
   * Volume ramp on `el` over `durSec`, computed from the wall clock each tick
   * so coarse/late ticks (background timer granularity) still land on the
   * correct trajectory. Gains rescale against the live master volume, so a
   * mid-fade volume change applies within one tick.
   */
  private startRamp(
    el: HTMLAudioElement,
    gain: (t: number) => number,
    durSec: number,
    onDone?: () => void,
  ) {
    this.cancelRamp(el);
    const t0 = performance.now();
    el.volume = clamp01(gain(0) * this.masterVolume);
    const timer = window.setInterval(() => {
      const t = (performance.now() - t0) / (durSec * 1000);
      el.volume = clamp01(gain(t) * this.masterVolume);
      if (t >= 1) {
        this.cancelRamp(el);
        onDone?.();
      }
    }, RAMP_TICK_MS);
    this.ramps.set(el, timer);
  }

  private cancelRamp(el: HTMLAudioElement) {
    const timer = this.ramps.get(el);
    if (timer != null) {
      window.clearInterval(timer);
      this.ramps.delete(el);
    }
  }

  // ── misc ───────────────────────────────────────────────────────────────

  /** Seek an episode to its resume position once metadata allows it. */
  private applyResume(el: HTMLAudioElement, item: QueueItem) {
    if (item.mediaKind !== "episode" || !item.resumeMs || item.resumeMs <= 0) return;
    const sec = item.resumeMs / 1000;
    const apply = () => {
      try {
        el.currentTime = sec;
      } catch {
        /* metadata not ready — leave at 0 */
      }
    };
    if (el.readyState >= HTMLMediaElement.HAVE_METADATA) {
      apply();
    } else {
      const once = () => {
        el.removeEventListener("loadedmetadata", once);
        apply();
      };
      el.addEventListener("loadedmetadata", once);
    }
  }
}
