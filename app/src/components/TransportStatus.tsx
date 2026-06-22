import type { TransportHealth } from "../ipc";

/**
 * Per-transport reachability chips — gRPC (primary) and REST (fallback) shown
 * separately so the user can tell "fully connected" from "REST only" (gRPC
 * down) from "offline". A green dot = that transport answered the last probe.
 *
 * `compact` shrinks it for tight chrome (the mobile top bar); the default size
 * suits the desktop status panel and the Account screen.
 */
export function TransportStatus({
  transports,
  compact = false,
  className = "",
}: {
  transports: TransportHealth;
  compact?: boolean;
  className?: string;
}) {
  const chip = (label: string, up: boolean) => (
    <span
      className={`flex items-center gap-1.5 ${
        compact ? "" : "rounded-md border border-oct-border bg-oct-card px-2 py-1"
      }`}
      title={`${label}: ${up ? "connected" : "unreachable"}`}
    >
      <span
        className={`inline-block rounded-full ${compact ? "h-1.5 w-1.5" : "h-2 w-2"} ${
          up ? "bg-oct-online" : "bg-oct-offline"
        }`}
      />
      <span
        className={`font-mono ${compact ? "text-[10px]" : "text-[11px]"} ${
          up ? "text-oct-muted" : "text-oct-subtle"
        }`}
      >
        {label}
      </span>
    </span>
  );

  return (
    <div className={`flex items-center ${compact ? "gap-2" : "gap-2.5"} ${className}`}>
      {chip("gRPC", transports.grpc)}
      {chip("REST", transports.rest)}
    </div>
  );
}
