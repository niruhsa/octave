import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  authChangePassword,
  authDeleteUser,
  authListUsers,
  authLogout,
} from "../ipc";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";

/**
 * /account — change your own password, or (admin/secret-key) manage any
 * user: reset password via dropdown, or delete an account.
 *
 * Self-change (non-admin): target locked to `session.user_id`, old
 * password required + verified server-side.
 *
 * Admin: fetches the user list (`GET /users`) on mount. Password-reset
 * dropdown — old password optional. Delete section — confirm by typing the
 * target username (self-delete signs the admin out afterward).
 */
const MIN_PASSWORD = 8;

export default function Account() {
  const session = useAppStore((s) => s.session);
  const tier = useAppStore((s) => s.tier);
  const setSession = useAppStore((s) => s.setSession);

  const isAdmin = tier === "admin";
  const selfId = session?.user_id ?? "";

  // Fetch user list for admins.
  const usersQ = useQuery({
    queryKey: ["users"],
    queryFn: authListUsers,
    enabled: isAdmin,
    staleTime: 30_000,
  });

  // ---- password change state ----
  const [pwTarget, setPwTarget] = useState(selfId);
  const [oldPw, setOldPw] = useState("");
  const [newPw, setNewPw] = useState("");
  const [confirm, setConfirm] = useState("");
  const [pwBusy, setPwBusy] = useState(false);
  const [pwErr, setPwErr] = useState<string | null>(null);
  const [pwDone, setPwDone] = useState(false);

  // ---- delete state ----
  const [delTarget, setDelTarget] = useState("");
  const [delTyped, setDelTyped] = useState("");
  const [delBusy, setDelBusy] = useState(false);
  const [delErr, setDelErr] = useState<string | null>(null);
  const [delDone, setDelDone] = useState(false);

  if (!session) {
    return (
      <section className="flex max-w-md flex-col gap-3">
        <h1 className="text-2xl font-semibold">Account</h1>
        <p className="text-sm text-neutral-400">Sign in first.</p>
        <Link to="/login" className="text-sm text-blue-400 hover:underline">
          → Sign in
        </Link>
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
    !pwBusy &&
    !!pwTarget &&
    !(oldRequired && !oldPw) &&
    newPw.length >= MIN_PASSWORD &&
    !pwPasswordsDiffer;
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
    <section className="flex max-w-md flex-col gap-6">
      {/* ---- password form ---- */}
      <form onSubmit={submitPassword} className="flex flex-col gap-3">
        <h1 className="text-2xl font-semibold">
          {isAdmin ? "Reset password" : "Change your password"}
        </h1>
        <p className="text-xs text-neutral-500">
          {isAdmin
            ? "Admin/secret-key: pick a user to reset their password (old password not required)."
            : "Self-change: your old password is verified server-side."}
        </p>

        {isAdmin && (
          <label className="flex flex-col gap-1 text-sm">
            <span className="text-neutral-400">Target user</span>
            {usersQ.isLoading ? (
              <span className="text-xs text-neutral-500">Loading users…</span>
            ) : usersQ.isError ? (
              <span className="text-xs text-red-400">
                {formatError(usersQ.error)}
              </span>
            ) : (
              <select
                value={pwTarget}
                onChange={(e) => setPwTarget(e.target.value)}
                className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm"
              >
                <option value="" disabled>
                  Select a user…
                </option>
                {usersQ.data?.map((u) => (
                  <option key={u.id} value={u.id}>
                    {u.username}
                    {u.id === selfId ? " (you)" : ""} — {u.level}
                  </option>
                ))}
              </select>
            )}
            {pwIsSelf && (
              <span className="text-xs text-neutral-500">
                (your own account — old password optional for admin)
              </span>
            )}
          </label>
        )}

        {oldRequired && (
          <label className="flex flex-col gap-1 text-sm">
            <span className="text-neutral-400">Current password</span>
            <input
              type="password"
              required
              value={oldPw}
              onChange={(e) => setOldPw(e.target.value)}
              className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
            />
          </label>
        )}

        <label className="flex flex-col gap-1 text-sm">
          <span className="text-neutral-400">
            New password (≥ {MIN_PASSWORD} chars)
          </span>
          <input
            type="password"
            required
            value={newPw}
            onChange={(e) => setNewPw(e.target.value)}
            className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
          />
          {pwTooShort && (
            <span className="text-xs text-amber-300">
              Must be at least {MIN_PASSWORD} characters.
            </span>
          )}
        </label>

        <label className="flex flex-col gap-1 text-sm">
          <span className="text-neutral-400">Confirm new password</span>
          <input
            type="password"
            required
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
          />
          {pwPasswordsDiffer && (
            <span className="text-xs text-amber-300">
              Passwords don't match.
            </span>
          )}
        </label>

        {pwErr && (
          <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
            {pwErr}
          </p>
        )}
        {pwDone && (
          <p className="rounded border border-emerald-700 bg-emerald-900/30 p-2 text-sm text-emerald-200">
            Password changed
            {pwSelected ? ` for ${pwSelected.username}` : ""}. The new password
            works for the next sign-in; your current session stays valid.
          </p>
        )}

        <button
          type="submit"
          disabled={!pwCanSubmit}
          className="rounded bg-blue-600 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
        >
          {pwBusy
            ? "Saving…"
            : isAdmin && !pwIsSelf
              ? pwSelected
                ? `Reset password for ${pwSelected.username}`
                : "Reset password"
              : "Change password"}
        </button>
      </form>

      {/* ---- admin only: delete account ---- */}
      {isAdmin && (
        <section className="border-t border-neutral-800 pt-6">
          <h2 className="mb-2 text-lg font-semibold text-red-400">
            Delete account
          </h2>
          <p className="mb-3 text-xs text-neutral-500">
            Admin/secret-key: permanently delete a user account. Removes the
            user, their sessions, playlists, and follows (cascade). Audit-log
            records are preserved (actor set to null). Downloaded content
            stays in the local cache.
          </p>

          <div className="flex flex-col gap-3">
            <label className="flex flex-col gap-1 text-sm">
              <span className="text-neutral-400">User to delete</span>
              {usersQ.isLoading ? (
                <span className="text-xs text-neutral-500">Loading users…</span>
              ) : (
                <select
                  value={delTarget}
                  onChange={(e) => {
                    setDelTarget(e.target.value);
                    setDelTyped("");
                    setDelErr(null);
                  }}
                  className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-sm"
                >
                  <option value="" disabled>
                    Select a user…
                  </option>
                  {usersQ.data?.map((u) => (
                    <option key={u.id} value={u.id}>
                      {u.username}
                      {u.id === selfId ? " (you)" : ""} — {u.level}
                    </option>
                  ))}
                </select>
              )}
            </label>

            {delTarget && (
              <label className="flex flex-col gap-1 text-sm">
                <span className="text-neutral-400">
                  Type{" "}
                  <code className="text-red-300">
                    {delSelected?.username ?? delTarget}
                  </code>{" "}
                  to confirm:
                </span>
                <input
                  value={delTyped}
                  onChange={(e) => setDelTyped(e.target.value)}
                  className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1 font-mono text-sm"
                  autoFocus
                />
              </label>
            )}

            {delErr && (
              <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
                {delErr}
              </p>
            )}
            {delDone && (
              <p className="rounded border border-emerald-700 bg-emerald-900/30 p-2 text-sm text-emerald-200">
                Account deleted{delSelected ? ` (${delSelected.username})` : ""}.
              </p>
            )}

            <button
              type="button"
              onClick={doDelete}
              disabled={!delCanDelete}
              className="rounded bg-red-700 px-3 py-1.5 text-sm text-white hover:bg-red-600 disabled:opacity-50"
            >
              {delBusy
                ? "Deleting…"
                : delIsSelf
                  ? `Yes, delete my account`
                  : delSelected
                    ? `Yes, delete ${delSelected.username}'s account`
                    : "Delete account"}
            </button>
          </div>
        </section>
      )}
    </section>
  );
}
