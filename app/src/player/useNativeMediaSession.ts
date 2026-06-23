// Android system media notification + lock-screen controls.
//
// The Web Media Session API (`useMediaSession`) drives desktop OS integration,
// but a bare Android WebView doesn't surface it to the system notification —
// so on Android we bridge to a native MediaSession + foreground service
// (Kotlin `MediaSessionPlugin`). This hook:
//   - pushes now-playing metadata + artwork on track change,
//   - pushes play/pause + position on state changes and a light heartbeat
//     (the system extrapolates the scrubber between updates),
//   - clears the session when playback ends,
//   - turns native transport-button presses back into store actions.
//
// Android-only; a no-op elsewhere. Mounted once, from `PlayerBar`.

import { useEffect, useMemo, useRef } from "react";
import {
  mediaSessionClear,
  mediaSessionSetPlayback,
  mediaSessionUpdate,
  onMediaSessionAction,
  playerActionUrlBase,
  playerCoverUrl,
} from "../ipc";
import type { MergedTrack } from "../ipc";
import { usePlayerStore } from "./store";
import type { NowPlayingMeta } from "./useNowPlayingMeta";

export function useNativeMediaSession(current: MergedTrack | null, meta: NowPlayingMeta) {
  const isAndroid = useMemo(
    () => typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent),
    [],
  );

  const isPlaying = usePlayerStore((s) => s.isPlaying);
  const durationSec = usePlayerStore((s) => s.durationSec);
  const trackId = current?.id ?? null;
  const albumId = current?.album_id ?? null;

  // Cache the resolved cover URL per album so re-pushing metadata (e.g. when
  // duration is learned) doesn't re-resolve it.
  const artCache = useRef<{ albumId: string | null; url: string | null }>({
    albumId: null,
    url: null,
  });
  // Loopback base the native side posts transport presses to (stable per launch).
  const actionBase = useRef<string | null>(null);

  // ── native transport buttons → store actions (mount once) ────────────────
  useEffect(() => {
    if (!isAndroid) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onMediaSessionAction((a) => {
      const st = usePlayerStore.getState();
      switch (a.action) {
        case "play":
        case "pause":
        case "playpause":
          st.togglePlay();
          break;
        case "next":
          st.next();
          break;
        case "prev":
          st.prev();
          break;
        case "stop":
          st.clearQueue();
          void mediaSessionClear();
          break;
        case "seek":
          if (typeof a.positionMs === "number") st.seekTo(a.positionMs / 1000);
          break;
      }
    })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch(() => {
        /* plugin unavailable */
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [isAndroid]);

  // ── metadata push (track change, and again once duration is known) ───────
  useEffect(() => {
    if (!isAndroid) return;
    if (!current) {
      void mediaSessionClear();
      return;
    }
    let cancelled = false;
    void (async () => {
      // Resolve artwork once per album.
      if (albumId && artCache.current.albumId !== albumId) {
        let url: string | null = null;
        try {
          url = await playerCoverUrl(albumId);
        } catch {
          url = null;
        }
        artCache.current = { albumId, url };
      } else if (!albumId) {
        artCache.current = { albumId: null, url: null };
      }
      // Resolve the loopback action base once (stable per launch).
      if (actionBase.current === null) {
        try {
          actionBase.current = await playerActionUrlBase();
        } catch {
          actionBase.current = "";
        }
      }
      if (cancelled) return;

      const st = usePlayerStore.getState();
      const durMs = Math.round((st.durationSec || current.duration_ms / 1000) * 1000);
      await mediaSessionUpdate({
        title: current.title,
        artist: meta.artistName ?? "",
        album: meta.albumTitle ?? "",
        artworkUrl: artCache.current.url,
        actionBaseUrl: actionBase.current ?? "",
        durationMs: durMs,
        positionMs: Math.round(st.positionSec * 1000),
        playing: st.isPlaying,
      }).catch(() => {});
    })();
    return () => {
      cancelled = true;
    };
    // durationSec re-runs this once the real duration loads (0 → D), so the
    // notification scrubber learns the track length.
  }, [isAndroid, trackId, albumId, durationSec, meta.artistName, meta.albumTitle, current]);

  // ── playback state push (play/pause + heartbeat while playing) ───────────
  useEffect(() => {
    if (!isAndroid || !current) return;
    const push = () => {
      const st = usePlayerStore.getState();
      const durMs = Math.round((st.durationSec || current.duration_ms / 1000) * 1000);
      void mediaSessionSetPlayback({
        positionMs: Math.round(st.positionSec * 1000),
        durationMs: durMs,
        playing: st.isPlaying,
      }).catch(() => {});
    };
    push();
    if (!isPlaying) return;
    // Correct scrubber drift periodically; the OS extrapolates between these.
    const iv = window.setInterval(push, 5000);
    return () => window.clearInterval(iv);
  }, [isAndroid, isPlaying, current]);
}
