// Normalise the various error shapes a Tauri command can return into a
// single human-readable string. Rust's `AppError` serialises to
// `{ kind: "...", message: "..." }`; reqwest/network errors arrive as
// plain `Error` instances; everything else gets `String(e)`.

export function formatError(e: unknown): string {
  if (typeof e === "object" && e !== null) {
    const obj = e as { kind?: string; message?: string };
    if (obj.kind) {
      return obj.message ? `${obj.kind}: ${obj.message}` : obj.kind;
    }
    if (e instanceof Error) return e.message;
  }
  return String(e);
}
