// Offline gating for connection-only surfaces.
//
// `OfflineGate` wraps a route whose feature *requires* a live server (Upload,
// Account, Create account). When offline it dims the content, blocks
// interaction, and overlays a notice with a re-check button. When online it
// renders children untouched.
//
// `OFFLINE_MSG` + `offlineAttrs` are for finer-grained gating: individual
// server-only actions (delete, rescan, download) on otherwise offline-capable
// pages get `disabled` + a hover tooltip explaining why.

import { useState } from "react";
import { authRefreshOnline } from "../ipc";
import { useAppStore } from "../store";
import { CloudOffIcon, SyncIcon } from "./icons";

export const OFFLINE_MSG = "Unavailable in offline mode — reconnect to the server";

/**
 * Spread onto a server-only `<button>` to disable it (and explain why) while
 * offline. Pass the button's normal disabled state + title to preserve them
 * when online. e.g. `<button {...offlineAttrs(online, busy, "Delete")}>`.
 */
export function offlineAttrs(
  online: boolean,
  baseDisabled = false,
  baseTitle?: string,
): { disabled: boolean; title: string | undefined } {
  return {
    disabled: !online || baseDisabled,
    title: !online ? OFFLINE_MSG : baseTitle,
  };
}

export function OfflineGate({
  feature,
  children,
}: {
  feature: string;
  children: React.ReactNode;
}) {
  const online = useAppStore((s) => s.online);
  const setOnline = useAppStore((s) => s.setOnline);
  const [checking, setChecking] = useState(false);

  if (online) return <>{children}</>;

  async function recheck() {
    setChecking(true);
    try {
      setOnline(await authRefreshOnline());
    } catch {
      /* no manager configured — stays offline */
    } finally {
      setChecking(false);
    }
  }

  return (
    <div className="relative min-h-full">
      <div aria-hidden className="pointer-events-none select-none opacity-25 blur-[1.5px]">
        {children}
      </div>
      <div className="absolute inset-0 grid place-items-center bg-oct-bg/55 p-6 backdrop-blur-[2px]">
        <div className="flex max-w-sm flex-col items-center gap-4 rounded-2xl border border-oct-border-strong bg-oct-surface/95 p-7 text-center shadow-[0_24px_60px_-18px_rgba(0,0,0,0.55)]">
          <span className="grid h-14 w-14 place-items-center rounded-full bg-oct-offline/15 text-oct-offline">
            <CloudOffIcon size={26} />
          </span>
          <div>
            <div className="font-mono text-[10.5px] tracking-[0.16em] text-oct-faint">OFFLINE MODE</div>
            <h2 className="mt-1.5 text-lg font-semibold text-oct-text">{feature} needs a connection</h2>
            <p className="mt-2 text-[13px] leading-relaxed text-oct-subtle">
              This feature talks to the server directly and isn't available offline. Reconnect to continue.
            </p>
          </div>
          <button
            onClick={recheck}
            disabled={checking}
            className="inline-flex items-center gap-2 rounded-full border border-oct-border-strong px-4 py-2 text-[13px] text-oct-muted transition-colors hover:border-oct-line hover:text-oct-text disabled:opacity-50"
          >
            <SyncIcon size={14} className={checking ? "animate-octspin" : ""} />
            {checking ? "Checking…" : "Re-check connection"}
          </button>
        </div>
      </div>
    </div>
  );
}
