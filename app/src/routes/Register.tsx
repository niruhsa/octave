import { useState } from "react";
import { Link } from "react-router-dom";
import { authRegister, type PermissionTier } from "../ipc";
import { formatError } from "../lib/error";
import { useAppStore } from "../store";
import { btnPrimary, errorBox, input, label, okBox } from "../lib/ui";
import { OfflineGate } from "../components/OfflineGate";

/**
 * /register — admin-only account creation. The server's register endpoint is
 * gated to Admin callers (or `SECRET_KEY`); there is no public sign-up. The
 * new account is not logged in here — the admin stays signed in.
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

  if (tier !== "admin") {
    return (
      <section className="flex max-w-md flex-col gap-3 p-6 md:p-8">
        <h1 className="text-[27px] font-semibold tracking-tight">Create account</h1>
        <p className="text-sm text-oct-subtle">
          Account creation is admin-only. Sign in with an admin account (or a{" "}
          <code className="font-mono text-oct-muted">SECRET_KEY</code>) first.
        </p>
        <Link to="/login" className="font-mono text-[11px] text-oct-accent hover:underline">→ Sign in</Link>
      </section>
    );
  }

  const passwordsDiffer = password !== confirm;
  const passwordTooShort = password.length > 0 && password.length < MIN_PASSWORD;
  const canSubmit =
    username.trim().length > 0 && password.length >= MIN_PASSWORD && !passwordsDiffer && !busy;

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
    <OfflineGate feature="Account creation">
    <form onSubmit={submit} className="flex max-w-md flex-col gap-3.5 p-6 md:p-8">
      <div>
        <h1 className="text-[27px] font-semibold tracking-tight">Create account</h1>
        <p className="mt-1 text-xs text-oct-subtle">Admin-only. The new account can sign in via the normal login screen.</p>
      </div>

      <label className="flex flex-col gap-1.5">
        <span className={label}>USERNAME</span>
        <input required value={username} onChange={(e) => setUsername(e.target.value)} className={input} />
      </label>

      <label className="flex flex-col gap-1.5">
        <span className={label}>PASSWORD (≥ {MIN_PASSWORD} CHARS)</span>
        <input type="password" required value={password} onChange={(e) => setPassword(e.target.value)} className={input} />
        {passwordTooShort && <span className="text-xs text-oct-accent-bright">Must be at least {MIN_PASSWORD} characters.</span>}
      </label>

      <label className="flex flex-col gap-1.5">
        <span className={label}>CONFIRM PASSWORD</span>
        <input type="password" required value={confirm} onChange={(e) => setConfirm(e.target.value)} className={input} />
        {passwordsDiffer && <span className="text-xs text-oct-accent-bright">Passwords don't match.</span>}
      </label>

      <fieldset className="flex flex-col gap-1.5">
        <legend className={`${label} mb-1`}>PERMISSION TIER</legend>
        {TIERS.map((t) => (
          <label
            key={t.value}
            className={`flex cursor-pointer items-center gap-2.5 rounded-lg border px-3 py-2 transition-colors ${
              newTier === t.value ? "border-oct-accent/50 bg-oct-accent/10" : "border-oct-border-strong hover:border-oct-line"
            }`}
          >
            <input
              type="radio"
              name="tier"
              value={t.value}
              checked={newTier === t.value}
              onChange={() => setNewTier(t.value)}
              className="accent-oct-accent"
            />
            <span className="text-sm font-medium">{t.label}</span>
            <span className="text-xs text-oct-faint">— {t.hint}</span>
          </label>
        ))}
      </fieldset>

      {err && <p className={errorBox}>{err}</p>}
      {created && (
        <p className={okBox}>
          Account created. User id: <code className="font-mono">{created}</code>. The user can now sign in.
        </p>
      )}

      <button type="submit" disabled={!canSubmit} className={btnPrimary}>
        {busy ? "Creating…" : "Create account"}
      </button>
    </form>
    </OfflineGate>
  );
}
