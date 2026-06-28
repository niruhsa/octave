// In-app hotkey dispatcher.
//
// Mounted once in `RootLayout`. Listens for keydown on the window, matches the
// event against the user's bindings, and runs the corresponding command. Both
// "local" and "global"-scoped bindings fire here while the window is focused;
// the "global" scope additionally implies system-wide capture, which is a
// future OS hook (see keybinds.ts) — until then there's no behavioral split.

import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { usePlayerStore } from "../player/store";
import { useSyncStore } from "../sync/useSync";
import { useQuickSearchStore } from "../quicksearch/store";
import { useKeybindStore, eventMatches, type CommandId } from "./keybinds";

const VOLUME_STEP = 0.05;

/** True when focus is in a text field — let the keystroke through untouched. */
function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    target.isContentEditable
  );
}

export function useHotkeys() {
  const navigate = useNavigate();

  useEffect(() => {
    function run(id: CommandId) {
      const player = usePlayerStore.getState();
      switch (id) {
        case "play":
          player.togglePlay();
          break;
        case "prev":
          player.prev();
          break;
        case "next":
          player.next();
          break;
        case "volumeUp":
          player.setVolume(player.volume + VOLUME_STEP);
          break;
        case "volumeDown":
          player.setVolume(player.volume - VOLUME_STEP);
          break;
        case "toggleShuffle":
          player.toggleShuffle();
          break;
        case "toggleRepeat":
          player.cycleRepeat();
          break;
        case "sync":
          void useSyncStore.getState().run();
          break;
        case "quickSearch":
          useQuickSearchStore.getState().toggle();
          break;
        case "home":
          navigate("/");
          break;
        case "library":
          navigate("/library");
          break;
        case "upload":
          navigate("/upload");
          break;
        case "uploadReports":
          navigate("/uploads");
          break;
        case "account":
          navigate("/account");
          break;
      }
    }

    function onKeyDown(e: KeyboardEvent) {
      // Don't hijack keystrokes meant for a text field, and ignore auto-repeat
      // so a held key doesn't fire the command dozens of times (volume stepping
      // is the one exception — repeat there feels natural).
      if (isEditable(e.target)) return;

      const { bindings } = useKeybindStore.getState();
      for (const id of Object.keys(bindings) as CommandId[]) {
        const bnd = bindings[id];
        if (!bnd) continue;
        if (eventMatches(e, bnd)) {
          const isVolume = id === "volumeUp" || id === "volumeDown";
          if (e.repeat && !isVolume) return;
          e.preventDefault();
          run(id);
          return;
        }
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [navigate]);
}
