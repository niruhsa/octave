import { useState } from "react";
import { Link } from "react-router-dom";
import { authRegister, type PermissionTier } from "../ipc";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";

/**
 * /register — admin-only account creation.
 *
 * The server's `POST /auth/register` is gated to Admin callers (or
 * `SECRET_KEY`, which is effective Admin) — there is no public sign-up.
 * So this screen is linked only when the active session's tier is Admin,
 * and the server re-enforces the check on every call. The new account is
 * not logged in here; the admin stays signed in, and the new user signs in
 * via the normal Login flow.
 *
 * Server validation: username non-empty, password ≥ 8 chars, username
 * unique. We mirror the ≥ 8-char rule client-side for a faster failure.
 */
const MIN_PASSWORD = 8;

const TIERS: { value: PermissionTier; label: string; hint: string }[] = [
  { value: "user", label: "User", hint: "read-only; can download + archive" },
  { value: "manager", label: "Manager", hint: "manage library (CRUD, metadata)" },
  { value: "admin", label: "Admin", hint: "full access incl. other accounts" },
];

export default function Register() {
  const tier = useAppStore((s) => s.tier);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [newTier, setNewTier] = useState<PermissionTier>("user");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [created, setCreated] = useState<string | null>(null);

  // Gate the whole screen client-side; the server re-checks. A non-admin
  // reaching here (e.g. stale tier) gets a clear message instead of a 403
  // they'd have to decode.
  if (tier !== "admin") {
    return (
      <section className="flex max-w-md flex-col gap-3">
        <h1 className="text-2xl font-semibold">Create account</h1>
        <p className="text-sm text-neutral-400">
          Account creation is admin-only. Sign in with an admin account (or
          a <code className="text-neutral-300">SECRET_KEY</code>) first.
        </p>
        <Link to="/login" className="text-sm text-blue-400 hover:underline">
          → Sign in
        </Link>
      </section>
    );
  }

  const passwordsDiffer = password !== confirm;
  const passwordTooShort = password.length > 0 && password.length < MIN_PASSWORD;
  const canSubmit =
    username.trim().length > 0 &&
    password.length >= MIN_PASSWORD &&
    !passwordsDiffer &&
    !busy;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true);
    setErr(null);
    setCreated(null);
    try {
      const userId = await authRegister(username.trim(), password, newTier);
      setCreated(userId);
      setUsername("");
      setPassword("");
      setConfirm("");
    } catch (caught) {
      setErr(formatError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="flex max-w-md flex-col gap-3">
      <h1 className="text-2xl font-semibold">Create account</h1>
      <p className="text-xs text-neutral-500">
        Admin-only. The new account can sign in via the normal login screen.
      </p>

      <label className="flex flex-col gap-1 text-sm">
        <span className="text-neutral-400">Username</span>
        <input
          required
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
        />
      </label>

      <label className="flex flex-col gap-1 text-sm">
        <span className="text-neutral-400">Password (≥ {MIN_PASSWORD} chars)</span>
        <input
          type="password"
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
        />
        {passwordTooShort && (
          <span className="text-xs text-amber-300">
            Must be at least {MIN_PASSWORD} characters.
          </span>
        )}
      </label>

      <label className="flex flex-col gap-1 text-sm">
        <span className="text-neutral-400">Confirm password</span>
        <input
          type="password"
          required
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
          className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
        />
        {passwordsDiffer && (
          <span className="text-xs text-amber-300">Passwords don't match.</span>
        )}
      </label>

      <fieldset className="flex flex-col gap-1 text-sm">
        <legend className="text-neutral-400">Permission tier</legend>
        {TIERS.map((t) => (
          <label
            key={t.value}
            className="flex items-center gap-2 rounded border border-neutral-800 px-2 py-1"
          >
            <input
              type="radio"
              name="tier"
              value={t.value}
              checked={newTier === t.value}
              onChange={() => setNewTier(t.value)}
            />
            <span className="font-medium">{t.label}</span>
            <span className="text-xs text-neutral-500">— {t.hint}</span>
          </label>
        ))}
      </fieldset>

      {err && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {err}
        </p>
      )}
      {created && (
        <p className="rounded border border-emerald-700 bg-emerald-900/30 p-2 text-sm text-emerald-200">
          Account created. User id:{" "}
          <code className="text-emerald-100">{created}</code>. The user can
          now sign in.
        </p>
      )}

      <button
        type="submit"
        disabled={!canSubmit}
        className="rounded bg-blue-600 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      >
        {busy ? "Creating…" : "Create account"}
      </button>
    </form>
  );
}
