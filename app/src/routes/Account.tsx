import { useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  authChangePassword,
  authChangeServer,
  authDeleteUser,
  authListUsers,
  authLogout,
  authRefreshTransports,
  authServerConfig,
  pushUnregister,
} from "../ipc";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";
import { broadcastInvalidate } from "../App";
import { btnPrimary, errorBox, input, label, okBox } from "../lib/ui";
import { OfflineGate } from "../components/OfflineGate";
import { PowerIcon } from "../components/icons";
import { Skeleton } from "../components/Skeleton";
import { TransportStatus } from "../components/TransportStatus";
import { DiscographyCoverage } from "../components/DiscographyCoverage";
import { useEqualizerStore } from "../equalizer/store";

/**
 * /account — change your own password, or (admin/secret-key) manage any user:
 * reset password via dropdown, or delete an account. Self-change (non-admin)
 * locks target to your id and verifies the old password server-side.
 */
const MIN_PASSWORD = 8;

export default function Account() {
  const session = useAppStore((s) => s.session);
  const tier = useAppStore((s) => s.tier);
  const setSession = useAppStore((s) => s.setSession);
  const qc = useQueryClient();

  const isAdmin = tier === "admin";
  const isManager = tier === "admin" || tier === "manager";
  const selfId = session?.user_id ?? "";

  const usersQ = useQuery({ queryKey: ["users"], queryFn: authListUsers, enabled: isAdmin, staleTime: 30_000 });

  const [pwTarget, setPwTarget] = useState(selfId);
  const [oldPw, setOldPw] = useState("");
  const [newPw, setNewPw] = useState("");
  const [confirm, setConfirm] = useState("");
  const [pwBusy, setPwBusy] = useState(false);
  const [pwErr, setPwErr] = useState<string | null>(null);
  const [pwDone, setPwDone] = useState(false);

  const [delTarget, setDelTarget] = useState("");
  const [delTyped, setDelTyped] = useState("");
  const [delBusy, setDelBusy] = useState(false);
  const [delErr, setDelErr] = useState<string | null>(null);
  const [delDone, setDelDone] = useState(false);

  if (!session) {
    return (
      <section className="flex max-w-md flex-col gap-3 p-6 md:p-8">
        <h1 className="text-[27px] font-semibold tracking-tight">Account</h1>
        <p className="text-sm text-oct-subtle">Sign in first.</p>
        <Link to="/login" className="font-mono text-[11px] text-oct-accent hover:underline">→ Sign in</Link>
      </section>
    );
  }

  const pwSelected = usersQ.data?.find((u) => u.id === pwTarget);
  const delSelected = usersQ.data?.find((u) => u.id === delTarget);
  const pwIsSelf = pwTarget === selfId;
  const delIsSelf = delTarget === selfId;
  const oldRequired = !isAdmin;
  const pwPasswordsDiffer = newPw !== confirm;
  const pwTooShort = newPw.length > 0 && newPw.length < MIN_PASSWORD;
  const pwCanSubmit =
    !pwBusy && !!pwTarget && !(oldRequired && !oldPw) && newPw.length >= MIN_PASSWORD && !pwPasswordsDiffer;
  const delCanDelete = !delBusy && !!delTarget && delTyped === (delSelected?.username ?? "");

  async function submitPassword(e: React.FormEvent) {
    e.preventDefault();
    if (!pwCanSubmit) return;
    setPwBusy(true);
    setPwErr(null);
    setPwDone(false);
    try {
      await authChangePassword(pwTarget, oldPw, newPw);
      setPwDone(true);
      setOldPw("");
      setNewPw("");
      setConfirm("");
      if (!pwIsSelf) setPwTarget(selfId);
    } catch (caught) {
      setPwErr(formatError(caught));
    } finally {
      setPwBusy(false);
    }
  }

  async function doDelete() {
    if (!delCanDelete) return;
    setDelBusy(true);
    setDelErr(null);
    setDelDone(false);
    try {
      await authDeleteUser(delTarget);
      setDelDone(true);
      setDelTyped("");
      broadcastInvalidate(["users"]);
      await qc.invalidateQueries({ queryKey: ["users"] });
      if (delIsSelf) {
        await authLogout();
        setSession(null);
        window.location.href = "/login";
      }
    } catch (caught) {
      setDelErr(formatError(caught));
    } finally {
      setDelBusy(false);
    }
  }

  return (
    <section className="flex max-w-md flex-col gap-8 p-6 md:p-8">
      <ServerSession />
      <OfflineGate feature="Account management">
      <div className="flex flex-col gap-8">
      <form onSubmit={submitPassword} className="flex flex-col gap-3.5">
        <div>
          <h1 className="text-[27px] font-semibold tracking-tight">
            {isAdmin ? "Reset password" : "Change password"}
          </h1>
          <p className="mt-1 text-xs text-oct-subtle">
            {isAdmin
              ? "Admin/secret-key: pick a user to reset their password (old password not required)."
              : "Self-change: your old password is verified server-side."}
          </p>
        </div>

        {isAdmin && (
          <label className="flex flex-col gap-1.5">
            <span className={label}>TARGET USER</span>
            {usersQ.isLoading ? (
              <Skeleton className="h-9 w-full rounded-lg" />
            ) : usersQ.isError ? (
              <span className="text-xs text-oct-danger">{formatError(usersQ.error)}</span>
            ) : (
              <select value={pwTarget} onChange={(e) => setPwTarget(e.target.value)} className={input}>
                <option value="" disabled>Select a user…</option>
                {usersQ.data?.map((u) => (
                  <option key={u.id} value={u.id}>{u.username}{u.id === selfId ? " (you)" : ""} — {u.level}</option>
                ))}
              </select>
            )}
            {pwIsSelf && <span className="text-xs text-oct-faint">(your own account — old password optional for admin)</span>}
          </label>
        )}

        {oldRequired && (
          <label className="flex flex-col gap-1.5">
            <span className={label}>CURRENT PASSWORD</span>
            <input type="password" required value={oldPw} onChange={(e) => setOldPw(e.target.value)} className={input} />
          </label>
        )}

        <label className="flex flex-col gap-1.5">
          <span className={label}>NEW PASSWORD (≥ {MIN_PASSWORD} CHARS)</span>
          <input type="password" required value={newPw} onChange={(e) => setNewPw(e.target.value)} className={input} />
          {pwTooShort && <span className="text-xs text-oct-accent-bright">Must be at least {MIN_PASSWORD} characters.</span>}
        </label>

        <label className="flex flex-col gap-1.5">
          <span className={label}>CONFIRM NEW PASSWORD</span>
          <input type="password" required value={confirm} onChange={(e) => setConfirm(e.target.value)} className={input} />
          {pwPasswordsDiffer && <span className="text-xs text-oct-accent-bright">Passwords don't match.</span>}
        </label>

        {pwErr && <p className={errorBox}>{pwErr}</p>}
        {pwDone && (
          <p className={okBox}>
            Password changed{pwSelected ? ` for ${pwSelected.username}` : ""}. The new password works for the next sign-in; your current session stays valid.
          </p>
        )}

        <button type="submit" disabled={!pwCanSubmit} className={btnPrimary}>
          {pwBusy ? "Saving…" : isAdmin && !pwIsSelf ? (pwSelected ? `Reset for ${pwSelected.username}` : "Reset password") : "Change password"}
        </button>
      </form>

      {isManager && <DiscographyCoverage />}

      {isAdmin && (
        <section className="border-t border-oct-border pt-6">
          <h2 className="mb-2 text-lg font-semibold text-oct-danger">Delete account</h2>
          <p className="mb-3 text-xs text-oct-subtle">
            Admin/secret-key: permanently delete a user account. Removes the user, their sessions, playlists, and follows
            (cascade). Audit-log records are preserved (actor set to null). Downloaded content stays in the local cache.
          </p>

          <div className="flex flex-col gap-3.5">
            <label className="flex flex-col gap-1.5">
              <span className={label}>USER TO DELETE</span>
              {usersQ.isLoading ? (
                <Skeleton className="h-9 w-full rounded-lg" />
              ) : (
                <select
                  value={delTarget}
                  onChange={(e) => { setDelTarget(e.target.value); setDelTyped(""); setDelErr(null); }}
                  className={input}
                >
                  <option value="" disabled>Select a user…</option>
                  {usersQ.data?.map((u) => (
                    <option key={u.id} value={u.id}>{u.username}{u.id === selfId ? " (you)" : ""} — {u.level}</option>
                  ))}
                </select>
              )}
            </label>

            {delTarget && (
              <label className="flex flex-col gap-1.5">
                <span className="text-sm text-oct-muted">
                  Type <code className="font-mono text-oct-danger">{delSelected?.username ?? delTarget}</code> to confirm:
                </span>
                <input value={delTyped} onChange={(e) => setDelTyped(e.target.value)} className={`${input} font-mono`} autoFocus />
              </label>
            )}

            {delErr && <p className={errorBox}>{delErr}</p>}
            {delDone && <p className={okBox}>Account deleted{delSelected ? ` (${delSelected.username})` : ""}.</p>}

            <button
              type="button"
              onClick={doDelete}
              disabled={!delCanDelete}
              className="inline-flex items-center justify-center rounded-full bg-oct-offline px-4 py-2.5 text-[13.5px] font-medium text-white transition-colors hover:opacity-90 disabled:opacity-40"
            >
              {delBusy
                ? "Deleting…"
                : delIsSelf
                  ? "Yes, delete my account"
                  : delSelected
                    ? `Yes, delete ${delSelected.username}'s account`
                    : "Delete account"}
            </button>
          </div>
        </section>
      )}
      </div>
      </OfflineGate>
    </section>
  );
}

/**
 * Server & session controls — always available (even offline), since you may
 * need to switch servers precisely *because* the current one is unreachable,
 * and sign-out must work regardless. Sits outside the OfflineGate.
 */
function ServerSession() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const online = useAppStore((s) => s.online);
  const transports = useAppStore((s) => s.transports);
  const setSession = useAppStore((s) => s.setSession);
  const setTransports = useAppStore((s) => s.setTransports);
  const setServerConfigured = useAppStore((s) => s.setServerConfigured);

  const [restUrl, setRestUrl] = useState("");
  const [grpcUrl, setGrpcUrl] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [signingOut, setSigningOut] = useState(false);

  // Pre-fill with the server we're currently pointed at. Restore (and reveal)
  // the gRPC URL only when it was an explicit override, so a derived one keeps
  // tracking the REST URL.
  useEffect(() => {
    authServerConfig()
      .then((c) => {
        if (!c) return;
        setRestUrl(c.rest_url);
        if (c.grpc_explicit) {
          setGrpcUrl(c.grpc_url);
          setShowAdvanced(true);
        }
      })
      .catch(() => {
        /* no server configured yet */
      });
  }, []);

  async function changeServer(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setErr(null);
    setDone(null);
    const equalizer = useEqualizerStore.getState();
    // Remove the previous server's audible correction before native changes
    // account namespaces. This also makes an unreachable destination Flat.
    equalizer.resetForScopeChange();
    try {
      // Send the gRPC URL whenever one is set (it's prefilled from the saved
      // override), so collapsing the panel doesn't silently drop it. An empty
      // field means "derive from the REST URL".
      const grpc = grpcUrl.trim() || undefined;
      const session = await authChangeServer(restUrl.trim(), grpc);
      setServerConfigured(true);
      setTransports(await authRefreshTransports());
      qc.clear(); // drop caches tied to the previous server
      if (session) {
        setSession(session);
        setDone("Connected to the new server.");
      } else {
        // The saved credential isn't valid on the new server — re-authenticate.
        setSession(null);
        navigate("/login");
      }
      await equalizer.load(true);
    } catch (caught) {
      setErr(formatError(caught));
      // Re-read whichever native scope ultimately remained configured.
      void equalizer.load(true);
    } finally {
      setBusy(false);
    }
  }

  async function signOut() {
    setSigningOut(true);
    // Drop this device's push token first (needs the still-valid credential).
    await pushUnregister().catch(() => {});
    try {
      await authLogout();
    } catch {
      /* best-effort server revocation; clear locally regardless */
    } finally {
      setSession(null);
      navigate("/login");
    }
  }

  return (
    <section className="flex flex-col gap-3.5">
      <div>
        <h2 className="text-lg font-semibold">Server &amp; session</h2>
        <p className="mt-1 text-xs text-oct-subtle">
          Point the app at a different server, or sign out. Available even while offline.
        </p>
      </div>

      <form onSubmit={changeServer} className="flex flex-col gap-3.5">
        <label className="flex flex-col gap-1.5">
          <span className={label}>SERVER URL (REST)</span>
          <input
            type="url"
            required
            value={restUrl}
            onChange={(e) => setRestUrl(e.target.value)}
            className={input}
          />
        </label>

        <button
          type="button"
          onClick={() => setShowAdvanced((v) => !v)}
          className="self-start text-[11px] text-oct-subtle underline decoration-oct-border-strong hover:text-oct-muted"
        >
          {showAdvanced ? "Hide" : "Show"} advanced
        </button>
        {showAdvanced && (
          <label className="flex flex-col gap-1.5">
            <span className={label}>GRPC URL (OPTIONAL)</span>
            <input
              type="url"
              value={grpcUrl}
              placeholder="auto-derived from REST URL"
              onChange={(e) => setGrpcUrl(e.target.value)}
              className={input}
            />
          </label>
        )}

        {err && <p className={errorBox}>{err}</p>}
        {done && <p className={okBox}>{done}</p>}

        <button type="submit" disabled={busy} className={btnPrimary}>
          {busy ? "Connecting…" : "Change server"}
        </button>
      </form>

      <div className="mt-1 flex items-center justify-between border-t border-oct-border pt-4">
        <div className="flex flex-col gap-2">
          <span className="text-[13px] text-oct-subtle">
            Signed in{online ? "" : " · offline"}
          </span>
          <TransportStatus transports={transports} />
        </div>
        <button
          onClick={signOut}
          disabled={signingOut}
          className="inline-flex items-center gap-2 rounded-full border border-oct-border-strong px-4 py-2 text-[13px] text-oct-danger transition-colors hover:bg-oct-offline/15 disabled:opacity-50"
        >
          <PowerIcon size={14} />
          {signingOut ? "Signing out…" : "Sign out"}
        </button>
      </div>
    </section>
  );
}
