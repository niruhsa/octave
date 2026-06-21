import { useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  authConfigureServer,
  authLogin,
  authRefreshOnline,
  authSetSecretKey,
} from "../ipc";
import { useAppStore } from "../store";
import { btnPrimary, errorBox, input, label } from "../lib/ui";
import { KeyIcon, ArtistIcon } from "../components/icons";

type Mode = "password" | "secret_key";

/**
 * Phase 2 login screen, OCTAVE-branded. Two paths — username/password →
 * bearer token, or `SECRET_KEY` → effective Admin. Both verified against the
 * server before anything persists.
 */
export default function Login() {
  const setSession = useAppStore((s) => s.setSession);
  const setOnline = useAppStore((s) => s.setOnline);
  const setServerConfigured = useAppStore((s) => s.setServerConfigured);
  const navigate = useNavigate();

  const [mode, setMode] = useState<Mode>("password");
  const [restUrl, setRestUrl] = useState("http://localhost:8080");
  const [grpcUrl, setGrpcUrl] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [secretKey, setSecretKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setErr(null);
    try {
      await authConfigureServer(restUrl, grpcUrl.trim() || undefined);
      setServerConfigured(true);
      setOnline(await authRefreshOnline());

      const session =
        mode === "password"
          ? await authLogin(username, password)
          : await authSetSecretKey(secretKey);
      setSession(session);
      navigate("/");
    } catch (caught) {
      const e = caught as { kind?: string; message?: string } | Error;
      setErr(
        "kind" in e && e.kind
          ? `${e.kind}: ${e.message ?? ""}`
          : (e as Error).message ?? String(caught),
      );
    } finally {
      setBusy(false);
    }
  }

  const tab = (m: Mode, text: string, Icon: typeof KeyIcon) => (
    <button
      type="button"
      onClick={() => setMode(m)}
      className={`flex flex-1 items-center justify-center gap-2 rounded-lg px-3 py-2 text-[13px] transition-colors ${
        mode === m ? "bg-oct-elevated text-oct-text" : "text-oct-subtle hover:text-oct-muted"
      }`}
    >
      <Icon size={14} />
      {text}
    </button>
  );

  return (
    <div className="flex min-h-full items-center justify-center bg-oct-bg p-6">
      <form
        onSubmit={submit}
        className="flex w-full max-w-sm flex-col gap-4 rounded-2xl border border-oct-border-strong bg-oct-surface p-7 shadow-[0_24px_60px_-18px_rgba(0,0,0,0.55)]"
      >
        <div className="flex flex-col items-center gap-3 pb-1">
          <span className="block h-10 w-10 rounded-lg bg-oct-accent" />
          <div className="text-center">
            <div className="text-lg font-semibold tracking-[0.18em]">OCTAVE</div>
            <div className="mt-1 font-mono text-[10.5px] tracking-[0.14em] text-oct-faint">
              SIGN IN TO YOUR LIBRARY
            </div>
          </div>
        </div>

        <label className="flex flex-col gap-1.5">
          <span className={label}>SERVER URL (REST)</span>
          <input
            type="url"
            required
            value={restUrl}
            onChange={(e) => setRestUrl(e.target.value)}
            className={input}
          />
          <span className="text-[11px] text-oct-faint">
            Dev default: REST :8080, gRPC :50051 (auto-derived).
          </span>
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
              placeholder="http://localhost:50051"
              onChange={(e) => setGrpcUrl(e.target.value)}
              className={input}
            />
          </label>
        )}

        <div className="flex gap-1 rounded-xl border border-oct-border bg-oct-card p-1">
          {tab("password", "Password", ArtistIcon)}
          {tab("secret_key", "SECRET_KEY", KeyIcon)}
        </div>

        {mode === "password" ? (
          <>
            <label className="flex flex-col gap-1.5">
              <span className={label}>USERNAME</span>
              <input
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className={input}
              />
            </label>
            <label className="flex flex-col gap-1.5">
              <span className={label}>PASSWORD</span>
              <input
                type="password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className={input}
              />
            </label>
          </>
        ) : (
          <label className="flex flex-col gap-1.5">
            <span className={label}>SECRET_KEY</span>
            <input
              type="password"
              required
              value={secretKey}
              onChange={(e) => setSecretKey(e.target.value)}
              className={input}
            />
          </label>
        )}

        {err && <p className={errorBox}>{err}</p>}

        <button type="submit" disabled={busy} className={`${btnPrimary} mt-1`}>
          {busy ? "Signing in…" : "Sign in"}
        </button>
      </form>
    </div>
  );
}
