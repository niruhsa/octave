// Animated 3-bar equalizer shown on the currently-playing row (OCTAVE). The
// bars are paused (flat) when the track is loaded but not playing.
export function EqBars({ playing = true, className = "" }: { playing?: boolean; className?: string }) {
  return (
    <span className={`flex h-3 items-end gap-[1.5px] ${className}`}>
      {[0, 0.3, 0.6].map((delay) => (
        <span
          key={delay}
          className="w-[2px] origin-bottom bg-oct-accent"
          style={{
            height: "100%",
            animation: playing ? `eqbar 0.9s ease-in-out infinite ${delay}s` : "none",
            transform: playing ? undefined : "scaleY(0.4)",
          }}
        />
      ))}
    </span>
  );
}
