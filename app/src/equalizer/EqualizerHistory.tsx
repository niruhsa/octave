import { useCallback, useEffect, useState } from "react";
import {
  equalizerGetChange,
  equalizerListChanges,
  equalizerRollbackChange,
} from "../ipc";
import { useAppStore } from "../store";
import { btnGhostSm, btnPrimary, card, errorBox, input, label } from "../lib/ui";
import { useEqualizerStore } from "./store";
import type {
  EqualizerChangeDetail,
  EqualizerChangeSummary,
  EqualizerRollbackResponse,
} from "./types";

const formatError = (error: unknown) =>
  typeof error === "object" && error != null && "message" in error
    ? String((error as { message: unknown }).message)
    : String(error);

export function EqualizerHistory({ readOnly = false }: { readOnly?: boolean }) {
  const session = useAppStore((state) => state.session);
  const refreshActive = useEqualizerStore((state) => state.load);
  const isAdmin = session?.tier === "admin";
  const allowed = isAdmin || session?.tier === "manager";
  const [subject, setSubject] = useState("");
  const [changes, setChanges] = useState<EqualizerChangeSummary[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [detail, setDetail] = useState<EqualizerChangeDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState("");
  const [rollbackResult, setRollbackResult] = useState<EqualizerRollbackResponse | null>(null);

  const fetchPage = useCallback(
    async (cursor?: string, append = false) => {
      if (!allowed) return;
      setLoading(true);
      setError(null);
      try {
        const page = await equalizerListChanges(subject, cursor, 30);
        setChanges((current) => (append ? [...current, ...page.changes] : page.changes));
        setNextCursor(page.next_cursor);
      } catch (loadError) {
        setError(formatError(loadError));
      } finally {
        setLoading(false);
      }
    },
    [allowed, subject],
  );

  useEffect(() => {
    void fetchPage();
  }, [fetchPage]);

  if (!allowed) return null;

  const openDetail = async (change: EqualizerChangeSummary) => {
    setLoading(true);
    setError(null);
    setRollbackResult(null);
    setConfirmation("");
    try {
      setDetail(await equalizerGetChange(change.audit_id));
    } catch (loadError) {
      setError(formatError(loadError));
    } finally {
      setLoading(false);
    }
  };

  const rollback = async () => {
    if (!detail?.current_state_revision || confirmation !== "ROLLBACK") return;
    setLoading(true);
    setError(null);
    try {
      const result = await equalizerRollbackChange(
        detail.change.audit_id,
        detail.current_state_revision,
      );
      setRollbackResult(result);
      setDetail(null);
      setConfirmation("");
      await fetchPage();
      // A target-tagged Admin response must never be installed as the caller's
      // snapshot. Only ask native for an ordinary scoped refresh when the
      // target is this signed-in bearer user.
      if (session?.kind === "bearer" && session.user_id === result.target_owner_id) {
        await refreshActive();
      }
    } catch (rollbackError) {
      setError(formatError(rollbackError));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div>
        <div className={label}>AUDITED EQUALIZER CHANGES</div>
        <p className="mt-2 text-[12px] leading-relaxed text-oct-subtle">
          Managers can inspect redacted changes. Admins can inspect reversible snapshots and
          roll back only while the target state still matches the audited after-image.
        </p>
      </div>

      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          void fetchPage();
        }}
      >
        <input
          className={input}
          value={subject}
          onChange={(event) => setSubject(event.target.value)}
          placeholder="Optional owner user UUID"
          aria-label="Filter equalizer history by owner user ID"
        />
        <button type="submit" className={btnGhostSm} disabled={loading}>Filter</button>
      </form>

      {error && <div className={errorBox}>{error}</div>}
      {rollbackResult && (
        <div className="rounded-lg border border-oct-online/40 bg-oct-online/10 px-3 py-2 text-[12px] text-oct-online">
          Rolled back {rollbackResult.changed_resources.length} resource change
          {rollbackResult.changed_resources.length === 1 ? "" : "s"} for owner{" "}
          <span className="font-mono">{rollbackResult.target_owner_id}</span>. New state revision{" "}
          {rollbackResult.state_revision}.
        </div>
      )}

      <div className={`${card} divide-y divide-oct-border`}>
        {changes.map((change) => (
          <button
            type="button"
            key={change.audit_id}
            className="flex w-full items-center gap-3 px-4 py-3 text-left transition hover:bg-oct-elevated/50"
            onClick={() => void openDetail(change)}
          >
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] text-oct-text">{change.action}</div>
              <div className="truncate text-[10.5px] text-oct-faint">
                Owner {change.owner_id} · {new Date(change.created_at).toLocaleString()}
              </div>
            </div>
            <span className="font-mono text-[10px] text-oct-faint">
              {change.before_state_revision} → {change.after_state_revision}
            </span>
          </button>
        ))}
        {changes.length === 0 && !loading && (
          <div className="px-4 py-8 text-center text-[12px] text-oct-faint">
            No equalizer audit entries match this filter.
          </div>
        )}
      </div>

      {nextCursor && (
        <button type="button" className={btnGhostSm} disabled={loading} onClick={() => void fetchPage(nextCursor, true)}>
          {loading ? "Loading…" : "Load more"}
        </button>
      )}

      {detail && (
        <div className="fixed inset-0 z-50 flex items-end justify-center bg-black/70 sm:items-center sm:p-6" role="dialog" aria-modal="true" aria-labelledby="eq-history-detail-title">
          <div className="oct-scroll max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-t-2xl border border-oct-border-strong bg-oct-panel p-5 shadow-2xl sm:rounded-2xl">
            <div className="flex items-start justify-between gap-3">
              <div>
                <h2 id="eq-history-detail-title" className="text-[18px] font-semibold">{detail.change.action}</h2>
                <div className="mt-1 font-mono text-[10px] text-oct-faint">{detail.change.audit_id}</div>
              </div>
              <button type="button" className={btnGhostSm} onClick={() => setDetail(null)}>Close</button>
            </div>

            <dl className="mt-4 grid gap-2 text-[12px] sm:grid-cols-2">
              <DetailTerm title="Owner" value={detail.change.owner_id} />
              <DetailTerm title="Actor" value={detail.change.actor_id ?? "system / secret key"} />
              <DetailTerm title="Resource" value={`${detail.change.resource_type}${detail.change.resource_id ? ` · ${detail.change.resource_id}` : ""}`} />
              <DetailTerm title="State revision" value={`${detail.change.before_state_revision} → ${detail.change.after_state_revision}`} />
            </dl>

            {isAdmin && detail.before_json && detail.after_json ? (
              <div className="mt-4 grid gap-3 md:grid-cols-2">
                <JsonBox title="BEFORE" value={detail.before_json} />
                <JsonBox title="AFTER" value={detail.after_json} />
              </div>
            ) : (
              <div className="mt-4 rounded-lg bg-oct-elevated px-3 py-2 text-[11.5px] text-oct-faint">
                Reversible snapshot contents are redacted for Manager access.
              </div>
            )}

            {isAdmin && (
              <div className="mt-5 border-t border-oct-border pt-4">
                <div className="text-[13px] font-medium text-oct-text">Rollback this change</div>
                <p className="mt-1 text-[11px] leading-relaxed text-oct-faint">
                  Rollback creates a new audited mutation; it never restores old revision numbers.
                  It is refused if later work changed the affected state.
                </p>
                <div className="mt-3 flex flex-wrap items-center gap-2">
                  <input
                    className={`${input} min-w-[180px] flex-1`}
                    value={confirmation}
                    onChange={(event) => setConfirmation(event.target.value)}
                    placeholder="Type ROLLBACK"
                    aria-label="Type ROLLBACK to confirm"
                    disabled={readOnly}
                  />
                  <button
                    type="button"
                    className={btnPrimary}
                    disabled={readOnly || !detail.rollback_eligible || confirmation !== "ROLLBACK" || loading}
                    onClick={() => void rollback()}
                  >
                    Roll back
                  </button>
                </div>
                {!detail.rollback_eligible && (
                  <div className="mt-2 text-[11px] text-oct-danger">
                    This entry is not currently eligible for rollback. Refresh after resolving later changes.
                  </div>
                )}
                {readOnly && (
                  <div className="mt-2 text-[11px] text-oct-danger">
                    Rollback is disabled while this client cannot read the server's equalizer format.
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function DetailTerm({ title, value }: { title: string; value: string }) {
  return (
    <div className="rounded-lg bg-oct-elevated px-3 py-2">
      <dt className="text-[10px] text-oct-faint">{title}</dt>
      <dd className="mt-0.5 break-all text-oct-text">{value}</dd>
    </div>
  );
}

function JsonBox({ title, value }: { title: string; value: string }) {
  let formatted = value;
  try {
    formatted = JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    /* Preserve server text if an older audit format is not JSON. */
  }
  return (
    <div className="min-w-0">
      <div className={label}>{title}</div>
      <pre className="oct-scroll mt-2 max-h-64 overflow-auto rounded-lg bg-oct-card p-3 font-mono text-[10px] leading-relaxed text-oct-muted">
        {formatted}
      </pre>
    </div>
  );
}
