import { useEffect, useState } from "react";

/**
 * An `<img>` that renders nothing when its source fails to load — so a
 * placeholder behind it shows through — and, crucially, **retries when `src`
 * changes**.
 *
 * This replaces the old imperative `onError → e.currentTarget.style.display =
 * "none"` pattern, which left the element hidden even after `src` updated:
 * after an artwork re-upload bumped the `?v=` cache-bust token, React swapped
 * the `src` attribute but kept the stale inline `display:none`, so the new
 * image never appeared until a full page reload. Tracking failure in React
 * state (reset whenever `src` changes) fixes that.
 */
export function FallbackImg({
  src,
  className,
  alt = "",
  style,
  loading = "lazy",
  onLoad,
}: {
  src: string | null | undefined;
  className?: string;
  alt?: string;
  style?: React.CSSProperties;
  loading?: "lazy" | "eager";
  onLoad?: () => void;
}) {
  const [failed, setFailed] = useState(false);
  // Retry on every src change (new id, or a bumped ?v= after re-upload).
  useEffect(() => setFailed(false), [src]);
  if (!src || failed) return null;
  return (
    <img
      src={src}
      alt={alt}
      className={className}
      style={style}
      loading={loading}
      onError={() => setFailed(true)}
      onLoad={onLoad}
    />
  );
}
