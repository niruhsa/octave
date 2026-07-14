import { useEffect, useRef } from "react";
import { onEqualizerEffectiveChanged } from "../ipc";
import * as audioGraph from "../player/audioGraph";
import { useAppStore } from "../store";
import { useEqualizerStore } from "./store";

const PREVIEW_INTERVAL_MS = 1_000 / 30;

/**
 * One app-lifetime bridge from native account/output resolution to the shared
 * Web Audio stage. Native remains authoritative for persisted matching; only
 * an explicitly audible unsaved editor preview overlays its result.
 */
export function useEqualizerRuntime(): void {
  const session = useAppStore((state) => state.session);
  const load = useEqualizerStore((state) => state.load);
  const acceptResolvedEvent = useEqualizerStore((state) => state.acceptResolvedEvent);
  const setGraph = useEqualizerStore((state) => state.setGraph);
  const snapshot = useEqualizerStore((state) => state.snapshot);
  const resolved = useEqualizerStore((state) => state.resolved);
  const previewProfile = useEqualizerStore((state) => state.previewProfile);
  const previewAudible = useEqualizerStore((state) => state.previewAudible);
  const previewBypassed = useEqualizerStore((state) => state.previewBypassed);
  const previewImmediateToken = useEqualizerStore((state) => state.previewImmediateToken);

  const previewTimer = useRef<number | null>(null);
  const lastPreviewAt = useRef(0);
  const latestPreview = useRef(previewProfile);
  const lastImmediateToken = useRef(previewImmediateToken);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    onEqualizerEffectiveChanged((next) => acceptResolvedEvent(next))
      .then((off) => {
        if (disposed) off();
        else unlisten = off;
      })
      .catch(() => {
        /* Browser dev / old native build: snapshot error is surfaced in Settings. */
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [acceptResolvedEvent]);

  useEffect(() => audioGraph.onEqualizerDiagnostics(setGraph), [setGraph]);

  // Scope changes must go Flat before reading the next account namespace.
  const scopeKey = `${session?.kind ?? "none"}:${session?.user_id ?? "none"}`;
  useEffect(() => {
    audioGraph.setEqualizerProfile(null, { bypassed: true });
    // EQ reconciliation is intentionally independent from the broader sync:
    // a library/playlist failure must not hide this account's profiles. The
    // native probe also lets a current client recover a stale future-format
    // quarantine left by an older build.
    void load(true);
  }, [load, scopeKey]);

  useEffect(() => {
    latestPreview.current = previewProfile;
    const futureFormat = snapshot?.support_state === "future_format";
    const masterEnabled = (snapshot?.preferences.master_enabled ?? false) && !futureFormat;

    // An editor preview is never allowed to bypass the explicit local master
    // safety gate. Turning the master off immediately returns to Flat.
    if (!masterEnabled || !previewAudible || !previewProfile) {
      if (previewTimer.current != null) {
        window.clearTimeout(previewTimer.current);
        previewTimer.current = null;
      }
      audioGraph.setEqualizerProfile(masterEnabled ? resolved?.profile ?? null : null, {
        bypassed: !masterEnabled || resolved?.profile == null,
      });
      return;
    }

    const applyLatest = () => {
      previewTimer.current = null;
      lastPreviewAt.current = performance.now();
      audioGraph.setEqualizerProfile(latestPreview.current, {
        bypassed: previewBypassed || latestPreview.current == null,
      });
    };

    const immediate = previewImmediateToken !== lastImmediateToken.current || previewBypassed;
    lastImmediateToken.current = previewImmediateToken;
    if (immediate) {
      if (previewTimer.current != null) window.clearTimeout(previewTimer.current);
      applyLatest();
      return;
    }

    if (previewTimer.current == null) {
      const elapsed = performance.now() - lastPreviewAt.current;
      previewTimer.current = window.setTimeout(
        applyLatest,
        Math.max(0, PREVIEW_INTERVAL_MS - elapsed),
      );
    }
  }, [
    previewAudible,
    previewBypassed,
    previewImmediateToken,
    previewProfile,
    resolved,
    snapshot?.preferences.master_enabled,
    snapshot?.support_state,
  ]);

  useEffect(
    () => () => {
      if (previewTimer.current != null) window.clearTimeout(previewTimer.current);
      audioGraph.setEqualizerProfile(null, { bypassed: true });
    },
    [],
  );
}
