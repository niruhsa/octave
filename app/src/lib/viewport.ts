// Keep the app sized to the *visual* viewport so the on-screen keyboard
// (Android / iOS) can never hide a focused input.
//
// The shell lays out at `height: 100%`, i.e. the **layout** viewport. When the
// soft keyboard opens, Chromium's default behaviour (`resizes-visual`) shrinks
// only the **visual** viewport — the layout viewport keeps its full height. So
// the bottom of the UI, and any field focused there, ends up *underneath* the
// keyboard, and the scroll container has no extra room to lift it into view.
//
// We mirror `visualViewport.height` into a `--app-height` CSS variable that the
// root elements use for their height (see index.css). The shell then collapses
// to the visible region when the keyboard appears, letting the scroll container
// bring the focused field above the keyboard.

/**
 * Install the visual-viewport → `--app-height` sync and a focus assist. Call
 * once at startup, before/while React mounts. Safe on desktop (where the
 * visual and layout viewports match, so it's a no-op beyond setting the var).
 */
export function installViewportSync(): void {
  const vv = window.visualViewport;
  const root = document.documentElement;

  const apply = () => {
    // Clamp to the layout viewport: the visible (visual) viewport is normally a
    // subset of it, but pinch-zoom-out (and some emulators) can report it
    // *larger* — using that verbatim would make the shell overflow the window.
    const layout = window.innerHeight;
    const visible = vv ? Math.min(vv.height, layout) : layout;
    root.style.setProperty("--app-height", `${Math.round(visible)}px`);
  };

  apply();

  if (vv) {
    // `resize` fires as the keyboard animates in/out; `scroll` covers the rare
    // case where the engine pans the visual viewport instead of resizing.
    vv.addEventListener("resize", apply);
    vv.addEventListener("scroll", apply);
  } else {
    window.addEventListener("resize", apply);
  }

  // On touch devices, focusing a field almost always opens the keyboard. The
  // browser's own scroll-into-view often runs *before* the viewport finishes
  // shrinking, so re-scroll the focused field into the visible area once things
  // settle. Gated to coarse pointers so desktop focus behaviour is untouched.
  const coarsePointer = window.matchMedia?.("(pointer: coarse)").matches ?? false;
  if (coarsePointer) {
    window.addEventListener("focusin", (e) => {
      const el = e.target as HTMLElement | null;
      if (!el || !el.matches("input, textarea, select, [contenteditable]")) return;
      // Defer past the keyboard animation + viewport resize.
      window.setTimeout(() => {
        el.scrollIntoView({ block: "center", behavior: "smooth" });
      }, 300);
    });
  }
}
