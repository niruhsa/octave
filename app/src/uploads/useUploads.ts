// Global upload state + event wiring.
//
// The active upload's state lives here (Zustand), NOT in the Upload route's
// component state — so it survives navigating away from and back to the tab,
// and a single global listener refreshes the library + reports on completion
// no matter which tab is open (the previous component-local listener silently
// dropped the refresh once you left the Upload tab).

import { useEffect } from "react";
import { create } from "zustand";
import {
  onUploadComplete,
  onUploadProgress,
  uploadsList,
  type UploadCompleteEvent,
  type UploadProgressEvent,
} from "../ipc";
import { broadcastInvalidate } from "../App";

type UploadStoreState = {
  /** Live progress of the in-flight upload (null when idle). */
  progress: UploadProgressEvent | null;
  /** Most recent completion summary (drives the "done" card). */
  lastComplete: UploadCompleteEvent | null;
  /** Id of the active (in-flight) upload session, or null. */
  activeUploadId: string | null;
  /** True between clicking upload and the first progress/complete event. */
  starting: boolean;
  setStarting: (v: boolean) => void;
  dismissComplete: () => void;
};

export const useUploadStore = create<UploadStoreState>((set) => ({
  progress: null,
  lastComplete: null,
  activeUploadId: null,
  starting: false,
  setStarting: (v) => set({ starting: v }),
  dismissComplete: () => set({ lastComplete: null }),
}));

/** True when an upload is in flight — gates the picker (one upload at a time). */
export function useUploadBusy(): boolean {
  return useUploadStore(
    (s) => s.starting || s.progress !== null || s.activeUploadId !== null,
  );
}

/** Register the global upload listeners once (call in the root layout). */
export function useUploadEvents() {
  useEffect(() => {
    let unProgress: (() => void) | undefined;
    let unComplete: (() => void) | undefined;

    onUploadProgress((e) => {
      if (e.phase === "done") {
        useUploadStore.setState({ progress: null, starting: false });
      } else {
        useUploadStore.setState((s) => ({
          progress: e,
          starting: false,
          activeUploadId: e.uploadId ?? s.activeUploadId,
        }));
      }
    }).then((f) => (unProgress = f));

    onUploadComplete((e) => {
      useUploadStore.setState({
        progress: null,
        starting: false,
        lastComplete: e,
        activeUploadId: null,
      });
      // Refresh the library + reports regardless of the open tab — this is the
      // fix for "uploads succeed but never appear in the library".
      broadcastInvalidate(["library"]);
      broadcastInvalidate(["uploads"]);
    }).then((f) => (unComplete = f));

    // Rehydrate an upload still in flight from a prior app session.
    uploadsList({ state: "uploading" })
      .then((rows) => {
        if (rows[0]) {
          useUploadStore.setState((s) =>
            s.activeUploadId ? s : { activeUploadId: rows[0].id },
          );
        }
      })
      .catch(() => {
        /* not logged in / offline — no active upload to restore */
      });

    return () => {
      unProgress?.();
      unComplete?.();
    };
  }, []);
}
