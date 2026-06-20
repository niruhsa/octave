import { useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  authConfigureServer,
  authLogin,
  authRefreshOnline,
  authSetSecretKey,
} from "../ipc";
import { useAppStore } from "../store";

type Mode = "password" | "secret_key";

/**
 * Phase 2 login screen.
 *
 * Two paths:
 *   1. Server URL + username/password → bearer token.
 *   2. Server URL + `SECRET_KEY` → effective Admin.
 *
 * Both verified against the server before we persist anything; we do not
 * trust user input as proof of access.
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

  return (
    <form onSubmit={submit} className="flex max-w-md flex-col gap-3">
      <h1 className="text-2xl font-semibold">Sign in</h1>

      <label className="flex flex-col gap-1 text-sm">
        <span className="text-neutral-400">Server URL (REST)</span>
        <input
          type="url"
          required
          value={restUrl}
          onChange={(e) => setRestUrl(e.target.value)}
          className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
        />
        <span className="text-xs text-neutral-500">
          Dev default: REST :8080, gRPC :50051 (auto-derived).
        </span>
      </label>

      <button
        type="button"
        onClick={() => setShowAdvanced((v) => !v)}
        className="self-start text-xs text-neutral-400 underline"
      >
        {showAdvanced ? "Hide" : "Show"} advanced
      </button>
      {showAdvanced && (
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-neutral-400">gRPC URL (optional)</span>
          <input
            type="url"
            value={grpcUrl}
            placeholder="http://localhost:50051"
            onChange={(e) => setGrpcUrl(e.target.value)}
            className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
          />
        </label>
      )}

      <div className="flex gap-2 text-sm">
        <button
          type="button"
          className={`rounded px-2 py-1 ${mode === "password" ? "bg-neutral-700" : "bg-neutral-900 border border-neutral-700"}`}
          onClick={() => setMode("password")}
        >
          Username + password
        </button>
        <button
          type="button"
          className={`rounded px-2 py-1 ${mode === "secret_key" ? "bg-neutral-700" : "bg-neutral-900 border border-neutral-700"}`}
          onClick={() => setMode("secret_key")}
        >
          SECRET_KEY
        </button>
      </div>

      {mode === "password" ? (
        <>
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
            <span className="text-neutral-400">Password</span>
            <input
              type="password"
              required
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
            />
          </label>
        </>
      ) : (
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-neutral-400">SECRET_KEY</span>
          <input
            type="password"
            required
            value={secretKey}
            onChange={(e) => setSecretKey(e.target.value)}
            className="rounded border border-neutral-700 bg-neutral-900 px-2 py-1"
          />
        </label>
      )}

      {err && (
        <p className="rounded border border-red-700 bg-red-900/30 p-2 text-sm text-red-200">
          {err}
        </p>
      )}

      <button
        type="submit"
        disabled={busy}
        className="rounded bg-blue-600 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      >
        {busy ? "Signing in…" : "Sign in"}
      </button>
    </form>
  );
}
