// OS media-notification integration via the Media Session API.
//
// This is what drives the platform media controls — and on Android, the
// system MediaStyle notification (design C): the compact form shown in the
// notification shade by default, expandable to the full layout, and rendered
// on the lock screen. We don't draw that notification ourselves; Chromium's
// WebView maps an active MediaSession + the playing `<audio>` element onto the
// native notification, and the OS renders the compact / expanded / lock-screen
// forms. Our job is to keep the session rich and current:
//
//   - metadata: title, artist, album, and album **artwork** (so art shows on
//     the lock screen + in the shade), via the `cover://` proxy URL;
//   - position state: duration + position, so the notification scrubber tracks
//     playback (the progress bar in the design);
//   - action handlers: play / pause / prev / next / stop + seek, so the
//     notification + lock-screen + Bluetooth/headset transport drive the store;
//   - playback state: playing / paused, so the play/pause glyph stays correct.
//
// Desktop benefits too (macOS Now Playing, Windows SMTC, media keys). Mounted
// once, from `PlayerBar`.

import { useEffect } from "react";
import { coverUrl } from "../ipc";
import type { MergedTrack } from "../ipc";
import { usePlayerStore } from "./store";
import type { NowPlayingMeta } from "./useNowPlayingMeta";

const ALL_ACTIONS: MediaSessionAction[] = [
  "play",
  "pause",
  "previoustrack",
  "nexttrack",
  "stop",
  "seekto",
  "seekbackward",
  "seekforward",
];

export function useMediaSession(current: MergedTrack | null, meta: NowPlayingMeta) {
  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const positionSec = usePlayerStore((s) => s.positionSec);
  const durationSec = usePlayerStore((s) => s.durationSec);

  // ── metadata (title / artist / album / artwork) ──────────────────────────
  useEffect(() => {
    if (!("mediaSession" in navigator)) return;
    if (!current) {
      navigator.mediaSession.metadata = null;
      return;
    }
    // Same square cover the rest of the UI uses; the renderer fetches it and
    // hands the bitmap to the OS. A few size hints let the platform pick.
    const art = current.album_id ? coverUrl(current.album_id) : null;
    const artwork = art
      ? ["256x256", "384x384", "512x512"].map((sizes) => ({
          src: art,
          sizes,
          type: "image/jpeg",
        }))
      : [];
    navigator.mediaSession.metadata = new MediaMetadata({
      title: current.title,
      artist: meta.artistName ?? "",
      album: meta.albumTitle ?? "",
      artwork,
    });
  }, [current, meta.artistName, meta.albumTitle]);

  // ── transport action handlers ────────────────────────────────────────────
  // Registered once; they read fresh state via `getState()` so they never go
  // stale and don't need re-binding on every track change.
  useEffect(() => {
    if (!("mediaSession" in navigator)) return;
    const ms = navigator.mediaSession;
    const store = usePlayerStore.getState;
    const set = (action: MediaSessionAction, handler: MediaSessionActionHandler) => {
      try {
        ms.setActionHandler(action, handler);
      } catch {
        /* action unsupported on this platform — ignore */
      }
    };

    set("play", () => store().togglePlay());
    set("pause", () => store().togglePlay());
    set("previoustrack", () => store().prev());
    set("nexttrack", () => store().next());
    set("stop", () => store().clearQueue());
    set("seekto", (d) => {
      if (typeof d.seekTime === "number") store().seekTo(d.seekTime);
    });
    set("seekbackward", (d) => {
      const by = d.seekOffset ?? 10;
      store().seekTo(Math.max(0, store().positionSec - by));
    });
    set("seekforward", (d) => {
      const by = d.seekOffset ?? 10;
      store().seekTo(store().positionSec + by);
    });

    return () => {
      for (const action of ALL_ACTIONS) {
        try {
          ms.setActionHandler(action, null);
        } catch {
          /* ignore */
        }
      }
    };
  }, []);

  // ── playback state (drives the play/pause glyph) ─────────────────────────
  useEffect(() => {
    if ("mediaSession" in navigator) {
      navigator.mediaSession.playbackState = isPlaying ? "playing" : "paused";
    }
  }, [isPlaying]);

  // ── position state (drives the notification scrubber) ────────────────────
  useEffect(() => {
    if (!("mediaSession" in navigator) || !navigator.mediaSession.setPositionState) return;
    if (!current || !(durationSec > 0)) return;
    try {
      navigator.mediaSession.setPositionState({
        duration: durationSec,
        position: Math.min(Math.max(positionSec, 0), durationSec),
        playbackRate: 1,
      });
    } catch {
      // setPositionState throws on inconsistent values (e.g. a transient
      // position > duration during a track switch) — safe to skip a frame.
    }
  }, [current, positionSec, durationSec]);
}
