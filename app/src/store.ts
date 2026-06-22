// Global UI/session state. Kept minimal — most data flows through TanStack
// Query (server cache) and Rust commands. Use this store only for state that
// is genuinely client-only and cross-cutting (auth tier, online/offline, etc).

import { create } from "zustand";
import type { AuthSession, PermissionTier, TransportHealth } from "./ipc";

export type TierOrAnon = PermissionTier | "anonymous";

export type AppState = {
  /** Server reachable (last known) on *either* transport. Independent of
   *  `navigator.onLine`. Kept in sync with `transports`. */
  online: boolean;
  /** Per-transport reachability — gRPC (primary) + REST (fallback). */
  transports: TransportHealth;
  /** Active session, or null when anonymous. */
  session: AuthSession | null;
  /** Convenience: tier with an "anonymous" sentinel. */
  tier: TierOrAnon;
  /** Whether the user has pointed us at a server yet. */
  serverConfigured: boolean;

  setOnline: (online: boolean) => void;
  /** Set the full transport breakdown; derives `online = rest || grpc`. */
  setTransports: (transports: TransportHealth) => void;
  setSession: (session: AuthSession | null) => void;
  setServerConfigured: (configured: boolean) => void;
};

export const useAppStore = create<AppState>((set) => ({
  online: false,
  transports: { rest: false, grpc: false },
  session: null,
  tier: "anonymous",
  serverConfigured: false,

  setOnline: (online) => set({ online }),
  setTransports: (transports) =>
    set({ transports, online: transports.rest || transports.grpc }),
  setSession: (session) =>
    set({ session, tier: session?.tier ?? "anonymous" }),
  setServerConfigured: (configured) => set({ serverConfigured: configured }),
}));
