import { useEffect, useMemo, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { btnDanger, btnGhostSm, btnPrimary, card, errorBox, label } from "../lib/ui";
import { DownloadIcon, PlusIcon, UploadIcon } from "../components/icons";
import { DeviceRules } from "./DeviceRules";
import { EqualizerHistory } from "./EqualizerHistory";
import { profileNameFromFile } from "./format";
import { ProfileEditor } from "./ProfileEditor";
import {
  activeEqualizerDefault,
  activeEqualizerProfiles,
  activeEqualizerRules,
  profileTarget,
  useEqualizerStore,
} from "./store";
import {
  cloneEqualizerProfile,
  createEqualizerId,
  createStarterProfile,
  EQ_LIMITS,
  type EqualizerDeleteProfileDisposition,
  type EqualizerProfile,
} from "./types";
import { useAppStore } from "../store";
import { equalizerExportFile, equalizerImportFile } from "../ipc";

type Panel = "profiles" | "devices" | "history";

const reasonLabel: Record<string, string> = {
  disabled: "Equalizer disabled",
  manual: "Temporary manual selection",
  local_exact: "This-device exact binding",
  portable_rule: "Automatic output rule",
  connected_fallback: "Explicit connected-device fallback",
  default: "Account default",
  flat: "No profile selected",
  unsupported: "Unsupported profile/state",
};

function uniqueName(base: string, profiles: EqualizerProfile[]): string {
  const names = new Set(profiles.map((profile) => profile.name.trim().toLocaleLowerCase()));
  if (!names.has(base.trim().toLocaleLowerCase())) return base;
  for (let number = 2; number <= 999; number += 1) {
    const candidate = `${base} (${number})`;
    if (!names.has(candidate.toLocaleLowerCase())) return candidate;
  }
  return `${base} ${Date.now()}`;
}

function outputConfidence(accuracy: string | undefined): string {
  switch (accuracy) {
    case "exact":
      return "Octave output";
    case "predicted":
      return "Predicted output";
    case "default":
      return "System default output";
    case "connected_only":
      return "Connected device";
    default:
      return "Output unavailable";
  }
}

export function EqualizerSettings() {
  const session = useAppStore((state) => state.session);
  const snapshot = useEqualizerStore((state) => state.snapshot);
  const resolved = useEqualizerStore((state) => state.resolved);
  const graph = useEqualizerStore((state) => state.graph);
  const conflicts = useEqualizerStore((state) => state.conflicts);
  const loading = useEqualizerStore((state) => state.loading);
  const saving = useEqualizerStore((state) => state.saving);
  const error = useEqualizerStore((state) => state.error);
  const previewStoppedMessage = useEqualizerStore((state) => state.previewStoppedMessage);
  const clearPreviewMessage = useEqualizerStore((state) => state.clearPreviewMessage);
  const load = useEqualizerStore((state) => state.load);
  const setPreferences = useEqualizerStore((state) => state.setPreferences);
  const createProfile = useEqualizerStore((state) => state.createProfile);
  const updateProfile = useEqualizerStore((state) => state.updateProfile);
  const deleteProfile = useEqualizerStore((state) => state.deleteProfile);
  const setDefault = useEqualizerStore((state) => state.setDefault);
  const setManualOverride = useEqualizerStore((state) => state.setManualOverride);
  const clearManualOverride = useEqualizerStore((state) => state.clearManualOverride);
  const preview = useEqualizerStore((state) => state.preview);
  const stopPreview = useEqualizerStore((state) => state.stopPreview);
  const setPreviewBypassed = useEqualizerStore((state) => state.setPreviewBypassed);
  const resolveConflict = useEqualizerStore((state) => state.resolveConflict);
  const promoteLocalProfile = useEqualizerStore((state) => state.promoteLocalProfile);

  const [panel, setPanel] = useState<Panel>("profiles");
  const [editing, setEditing] = useState<EqualizerProfile | null>(null);
  const [importNote, setImportNote] = useState<string | null>(null);
  const [deleteCandidate, setDeleteCandidate] = useState<EqualizerProfile | null>(null);
  const [deleteMode, setDeleteMode] = useState<"reject" | "flat" | string>("reject");
  const [promotionDefault, setPromotionDefault] = useState(false);
  const [promotionBindings, setPromotionBindings] = useState(false);

  const profiles = useMemo(() => activeEqualizerProfiles(snapshot), [snapshot]);
  const rules = useMemo(() => activeEqualizerRules(snapshot), [snapshot]);
  const defaultId = activeEqualizerDefault(snapshot);
  const canViewHistory = session?.tier === "manager" || session?.tier === "admin";
  const conflictGroups = useMemo(
    () =>
      [...new Map(conflicts.map((conflict) => [conflict.dependency_group, conflict])).values()],
    [conflicts],
  );

  useEffect(() => {
    if (!editing || editing.revision === "0") return;
    const fresh = profiles.find((profile) => profile.id === editing.id);
    if (fresh && fresh.revision !== editing.revision) setEditing(cloneEqualizerProfile(fresh));
  }, [editing, profiles]);

  const newProfile = () => {
    const starter = createStarterProfile(uniqueName("New equalizer", profiles));
    setEditing(starter);
    setPanel("profiles");
  };

  const duplicateProfile = (profile: EqualizerProfile) => {
    const now = new Date().toISOString();
    setEditing({
      ...cloneEqualizerProfile(profile),
      id: createEqualizerId(),
      name: uniqueName(`${profile.name} copy`, profiles),
      revision: "0",
      created_at: now,
      updated_at: now,
      source: undefined,
      unsynced: undefined,
      conflict: undefined,
    });
  };

  const importFile = async () => {
    setImportNote(null);
    try {
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "Parametric equalizer", extensions: ["txt"] }],
      });
      const pathOrUri = Array.isArray(selected) ? selected[0] : selected;
      if (!pathOrUri) return;
      const tail = decodeURIComponent(pathOrUri.split(/[\\/]/u).pop() ?? "");
      const suggested = profileNameFromFile(tail);
      const proposedName =
        pathOrUri.startsWith("content://") && (!tail || /^\d+$/u.test(tail))
          ? window.prompt("Name this imported equalizer profile", "Imported equalizer")
          : suggested;
      if (proposedName == null) return;
      const parsed = await equalizerImportFile(pathOrUri, proposedName);
      setEditing({
        ...cloneEqualizerProfile(parsed.profile),
        name: uniqueName(parsed.profile.name || proposedName, profiles),
        revision: "0",
      });
      setPanel("profiles");
      setImportNote(
        parsed.warnings.length
          ? parsed.warnings.map((warning) => warning.message).join(" ")
          : `Parsed ${parsed.profile.bands.length} PK filters. Review and create the profile to finish importing.`,
      );
    } catch (parseError) {
      setImportNote(parseError instanceof Error ? parseError.message : String(parseError));
    }
  };

  const exportProfile = async (profile: EqualizerProfile) => {
    setImportNote(null);
    try {
      const fileName = `${profile.name.replace(/[<>:"/\\|?*\u0000-\u001f]/gu, "_") || "equalizer"}.txt`;
      const destination = await save({
        defaultPath: fileName,
        filters: [{ name: "Parametric equalizer", extensions: ["txt"] }],
      });
      if (!destination) return;
      await equalizerExportFile(profile.id, destination);
      setImportNote(`Exported ${profile.name}.`);
    } catch (exportError) {
      setImportNote(exportError instanceof Error ? exportError.message : String(exportError));
    }
  };

  const saveProfile = async (profile: EqualizerProfile) => {
    if (profiles.some((candidate) => candidate.id === profile.id)) await updateProfile(profile);
    else await createProfile(profile);
    const fresh = activeEqualizerProfiles(useEqualizerStore.getState().snapshot).find(
      (candidate) => candidate.id === profile.id,
    );
    setEditing(fresh ? cloneEqualizerProfile(fresh) : null);
    setImportNote(null);
  };

  const confirmDelete = async () => {
    if (!deleteCandidate) return;
    const disposition: EqualizerDeleteProfileDisposition =
      deleteMode === "reject"
        ? { kind: "reject_if_referenced" }
        : deleteMode === "flat"
          ? { kind: "replace_with_flat" }
          : { kind: "replace_with_profile", profile_id: deleteMode };
    await deleteProfile(deleteCandidate, disposition);
    if (editing?.id === deleteCandidate.id) setEditing(null);
    setDeleteCandidate(null);
  };

  if (!snapshot && loading) {
    return <div className="py-12 text-center text-[13px] text-oct-faint">Loading equalizer…</div>;
  }

  if (!snapshot) {
    return (
      <div className="flex flex-col gap-3">
        <div className={errorBox}>{error ?? "Equalizer service is not available in this build."}</div>
        <button type="button" className={btnGhostSm} onClick={() => void load(true)}>Retry</button>
      </div>
    );
  }

  const preferences = snapshot.preferences;
  const output = resolved?.output_summary;
  const effectiveName = resolved?.profile?.name ?? "Flat";
  const currentReferences = deleteCandidate
    ? rules.filter(
        (rule) => rule.action.kind === "profile" && rule.action.profile_id === deleteCandidate.id,
      )
    : [];
  const deletingDefault = deleteCandidate?.id === defaultId;
  const mustResolveReferences = deletingDefault || currentReferences.length > 0;
  const readOnly = snapshot.support_state === "future_format";
  const promotableLocalProfiles =
    snapshot.active_layer === "synced" ? snapshot.local.profiles : [];

  return (
    <div className="flex flex-col gap-5">
      <p className="text-[13px] leading-relaxed text-oct-subtle">
        Parametric output correction is applied once after gapless/crossfading tracks are mixed.
        Profiles sync with your account; the master switch, previews, and exact device bindings stay local.
      </p>

      {error && <div className={errorBox}>{error}</div>}
      {readOnly && (
        <div className="rounded-lg border border-oct-danger/40 bg-oct-offline/10 px-3 py-2 text-[12px] leading-relaxed text-oct-danger">
          This server uses a newer equalizer format. Octave preserved the cached data, applies Flat,
          and keeps profile/rule editing read-only until this client is upgraded.
        </div>
      )}
      {previewStoppedMessage && (
        <div className="flex items-start justify-between gap-3 rounded-lg border border-oct-accent/35 bg-oct-accent/10 px-3 py-2 text-[12px] text-oct-accent-bright">
          <span>{previewStoppedMessage}</span>
          <button type="button" className="text-oct-muted hover:text-oct-text" onClick={clearPreviewMessage}>×</button>
        </div>
      )}

      <div className="flex flex-col gap-2">
        <div className={label}>PLAYBACK</div>
        <div className={`${card} divide-y divide-oct-border`}>
          <SwitchRow
            title="Equalizer"
            description="Local master switch. Off always plays Flat without deleting synced profiles or rules."
            checked={preferences.master_enabled}
            onChange={(master_enabled) => void setPreferences({ ...preferences, master_enabled })}
          />
          <SwitchRow
            title="Automatic output switching"
            description="Allow confirmed portable and this-device rules to select a profile. Starts disabled."
            checked={preferences.automatic_switching_enabled}
            disabled={!preferences.master_enabled}
            onChange={(automatic_switching_enabled) =>
              void setPreferences({ ...preferences, automatic_switching_enabled })
            }
          />
        </div>
      </div>

      <div className="grid gap-2 sm:grid-cols-2">
        <StatusCard
          title={outputConfidence(output?.accuracy)}
          value={output?.display_name ?? "Unknown output"}
          detail={output ? `${output.route_kind} · ${output.binding_stability.replace("_", " ")}` : "Default/manual EQ remains available"}
          active={!!output}
        />
        <StatusCard
          title="Effective correction"
          value={preferences.master_enabled ? effectiveName : "Flat"}
          detail={reasonLabel[resolved?.reason ?? "flat"] ?? resolved?.reason ?? "No selection"}
          active={preferences.master_enabled && !!resolved?.profile}
        />
        <StatusCard
          title="Configuration source"
          value={snapshot.active_layer === "synced" ? "Synced account" : "Local only"}
          detail={`${snapshot.support_state} server support · state ${snapshot.synced.state_revision}`}
        />
        <StatusCard
          title="Audio graph"
          value={graph?.capability ?? "pending"}
          detail={graph?.warning ?? (graph?.sampleRate ? `${graph.sampleRate / 1_000} kHz context` : "Starts with playback")}
          active={graph?.capability === "supported"}
        />
      </div>

      {(snapshot.pending_count > 0 || snapshot.conflict_count > 0) && (
        <div className="rounded-lg border border-oct-border-strong bg-oct-panel px-3 py-2 text-[11.5px] text-oct-muted">
          {snapshot.pending_count} unsynced equalizer change{snapshot.pending_count === 1 ? "" : "s"}
          {snapshot.conflict_count > 0 && ` · ${snapshot.conflict_count} conflict${snapshot.conflict_count === 1 ? "" : "s"} need attention`}
        </div>
      )}

      {conflictGroups.length > 0 && (
        <div className="flex flex-col gap-2">
          <div className={label}>SYNC CONFLICTS</div>
          <div className={`${card} divide-y divide-oct-border`}>
            {conflictGroups.map((conflict) => (
              <div key={conflict.dependency_group} className="flex flex-col gap-3 px-4 py-3">
                <div>
                  <div className="text-[13px] text-oct-text">
                    {conflict.op_type.replace("equalizer.", "").replace(".", " ")}
                  </div>
                  <div className="mt-0.5 text-[11px] text-oct-faint">
                    {conflict.error_code}: {conflict.error_message}
                  </div>
                </div>
                <div className="flex flex-wrap gap-2">
                  <button
                    type="button"
                    className={btnGhostSm}
                    disabled={saving}
                    onClick={() => void resolveConflict(conflict.id, "keep_server")}
                  >
                    Keep server
                  </button>
                  <button
                    type="button"
                    className={btnGhostSm}
                    disabled={saving}
                    onClick={() => void resolveConflict(conflict.id, "keep_local_copy")}
                  >
                    Save as local copy
                  </button>
                  <button
                    type="button"
                    className={btnPrimary}
                    disabled={saving || snapshot.support_state === "future_format"}
                    onClick={() => void resolveConflict(conflict.id, "retry")}
                  >
                    Reapply on server
                  </button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="grid gap-3 sm:grid-cols-2">
        <label>
          <span className={label}>DEFAULT PROFILE</span>
          <select
            className="mt-2 w-full rounded-lg border border-oct-border-strong bg-oct-card px-3 py-2 text-[13px] text-oct-text focus:border-oct-accent focus:outline-none"
            value={defaultId ?? ""}
            onChange={(event) => void setDefault(event.target.value || null)}
            disabled={saving || readOnly}
          >
            <option value="">Flat</option>
            {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name}</option>)}
          </select>
        </label>
        <label>
          <span className={label}>TEMPORARY SELECTION</span>
          <select
            className="mt-2 w-full rounded-lg border border-oct-border-strong bg-oct-card px-3 py-2 text-[13px] text-oct-text focus:border-oct-accent focus:outline-none"
            value={resolved?.reason === "manual" ? resolved.profile?.id ?? "flat" : "follow"}
            disabled={readOnly}
            onChange={(event) => {
              const value = event.target.value;
              if (value === "follow") void clearManualOverride();
              else void setManualOverride(profileTarget(snapshot.active_layer, value === "flat" ? null : value));
            }}
          >
            <option value="follow">Follow rules / default</option>
            <option value="flat">Bypass (Flat)</option>
            {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name}</option>)}
          </select>
        </label>
      </div>

      <div className={`grid ${canViewHistory ? "grid-cols-3" : "grid-cols-2"} gap-1 rounded-lg bg-oct-elevated p-1`}>
        <button type="button" onClick={() => setPanel("profiles")} className={`rounded-md px-3 py-2 text-[12.5px] transition ${panel === "profiles" ? "bg-oct-accent text-white" : "text-oct-subtle hover:text-oct-text"}`}>Profiles</button>
        <button type="button" onClick={() => setPanel("devices")} className={`rounded-md px-3 py-2 text-[12.5px] transition ${panel === "devices" ? "bg-oct-accent text-white" : "text-oct-subtle hover:text-oct-text"}`}>Output rules</button>
        {canViewHistory && (
          <button type="button" onClick={() => setPanel("history")} className={`rounded-md px-3 py-2 text-[12.5px] transition ${panel === "history" ? "bg-oct-accent text-white" : "text-oct-subtle hover:text-oct-text"}`}>History</button>
        )}
      </div>

      {panel === "history" && canViewHistory ? (
        <EqualizerHistory readOnly={readOnly} />
      ) : panel === "devices" ? (
        <DeviceRules readOnly={readOnly} />
      ) : editing ? (
        <>
          {importNote && (
            <div className="rounded-lg border border-oct-accent/30 bg-oct-accent/10 px-3 py-2 text-[11.5px] text-oct-accent-bright">{importNote}</div>
          )}
          <ProfileEditor
            profile={editing}
            graph={graph}
            saving={saving || readOnly}
            isNew={!profiles.some((profile) => profile.id === editing.id)}
            previewEnabled={preferences.master_enabled}
            onSave={saveProfile}
            onCancel={() => { setEditing(null); setImportNote(null); }}
            onPreview={preview}
            onStopPreview={stopPreview}
            onPreviewBypass={setPreviewBypassed}
          />
        </>
      ) : (
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <div className={label}>PROFILES</div>
              <div className="mt-1 text-[11px] text-oct-faint">{profiles.length} of {EQ_LIMITS.profiles}</div>
            </div>
            <div className="flex flex-wrap gap-2">
              <button type="button" className={btnGhostSm} disabled={readOnly} onClick={() => void importFile()}>
                <UploadIcon size={13} /> Import text
              </button>
              <button type="button" className={btnPrimary} disabled={readOnly || profiles.length >= EQ_LIMITS.profiles} onClick={newProfile}>
                <PlusIcon size={13} /> New profile
              </button>
            </div>
          </div>

          {importNote && !editing && <div className={errorBox}>{importNote}</div>}

          <div className={`${card} divide-y divide-oct-border`}>
            {profiles.map((profile) => (
              <div key={profile.id} className="flex flex-wrap items-center gap-2 px-4 py-3">
                <button type="button" className="min-w-0 flex-1 text-left" disabled={readOnly} onClick={() => setEditing(cloneEqualizerProfile(profile))}>
                  <div className="flex items-center gap-2">
                    <span className="truncate text-[13.5px] text-oct-text">{profile.name}</span>
                    {profile.id === defaultId && <span className="rounded bg-oct-accent/10 px-1.5 py-0.5 text-[9.5px] text-oct-accent">DEFAULT</span>}
                    {profile.id === resolved?.profile?.id && <span className="rounded bg-oct-online/10 px-1.5 py-0.5 text-[9.5px] text-oct-online">ACTIVE</span>}
                    {profile.unsynced && <span className="rounded bg-oct-elevated px-1.5 py-0.5 text-[9.5px] text-oct-muted">UNSYNCED</span>}
                    {profile.conflict && <span className="rounded bg-oct-offline/15 px-1.5 py-0.5 text-[9.5px] text-oct-danger">CONFLICT</span>}
                  </div>
                  <div className="mt-0.5 text-[10.5px] text-oct-faint">
                    {profile.bands.length} PK bands · preamp {profile.preamp_db > 0 ? "+" : ""}{profile.preamp_db} dB · auto headroom {profile.auto_headroom_enabled ? "on" : "off"}
                  </div>
                </button>
                <button type="button" className={btnGhostSm} disabled={readOnly} onClick={() => duplicateProfile(profile)}>Duplicate</button>
                <button type="button" className={btnGhostSm} onClick={() => void exportProfile(profile)} title="Export ParametricEQ text">
                  <DownloadIcon size={12} /> Export
                </button>
                <button
                  type="button"
                  className="rounded-lg px-2 py-1 text-[11px] text-oct-danger transition hover:bg-oct-offline/10"
                  disabled={readOnly}
                  onClick={() => {
                    const referenced = profile.id === defaultId || rules.some((rule) => rule.action.kind === "profile" && rule.action.profile_id === profile.id);
                    setDeleteMode(referenced ? "flat" : "reject");
                    setDeleteCandidate(profile);
                  }}
                >
                  Delete
                </button>
              </div>
            ))}
            {profiles.length === 0 && (
              <div className="px-4 py-8 text-center text-[12px] text-oct-faint">No profiles yet. Start with five neutral peaking bands or import a ParametricEQ text file.</div>
            )}
          </div>

          {promotableLocalProfiles.length > 0 && (
            <div className="flex flex-col gap-2">
              <div>
                <div className={label}>LOCAL-ONLY PROFILES</div>
                <div className="mt-1 text-[11px] text-oct-faint">
                  Copy profiles preserved from offline or older-server use into this account. The
                  local original is retained; name and UUID collisions are resolved safely.
                </div>
              </div>
              <div className="flex flex-wrap gap-4 text-[11.5px] text-oct-subtle">
                <label className="flex items-center gap-2">
                  <input type="checkbox" checked={promotionDefault} disabled={readOnly || saving} onChange={(event) => setPromotionDefault(event.target.checked)} />
                  Make promoted copy the account default
                </label>
                <label className="flex items-center gap-2">
                  <input type="checkbox" checked={promotionBindings} disabled={readOnly || saving} onChange={(event) => setPromotionBindings(event.target.checked)} />
                  Remap matching this-device bindings
                </label>
              </div>
              <div className={`${card} divide-y divide-oct-border`}>
                {promotableLocalProfiles.map((profile) => (
                  <div key={profile.id} className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
                    <div>
                      <div className="text-[13px] text-oct-text">{profile.name}</div>
                      <div className="text-[10.5px] text-oct-faint">{profile.bands.length} PK bands · retained locally after copy</div>
                    </div>
                    <button
                      type="button"
                      className={btnPrimary}
                      disabled={readOnly || saving || snapshot.pending_count > 0 || profiles.length >= EQ_LIMITS.profiles}
                      onClick={() => void promoteLocalProfile(profile.id, promotionDefault, promotionBindings)}
                    >
                      Sync copy to account
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {deleteCandidate && (
        <div className="fixed inset-0 z-50 flex items-end justify-center bg-black/70 p-0 sm:items-center sm:p-6" role="dialog" aria-modal="true" aria-labelledby="delete-eq-title">
          <div className="w-full max-w-md rounded-t-2xl border border-oct-border-strong bg-oct-panel p-5 shadow-2xl sm:rounded-2xl">
            <h2 id="delete-eq-title" className="text-[18px] font-semibold">Delete “{deleteCandidate.name}”?</h2>
            <p className="mt-2 text-[12px] leading-relaxed text-oct-subtle">
              {mustResolveReferences
                ? `This profile is ${deletingDefault ? "the default" : "not the default"} and is used by ${currentReferences.length} output rule${currentReferences.length === 1 ? "" : "s"}. Choose how every reference is resolved atomically.`
                : "The profile is not the default and no portable rule uses it."}
            </p>
            <label className="mt-4 block">
              <span className={label}>REFERENCE DISPOSITION</span>
              <select className="mt-2 w-full rounded-lg border border-oct-border-strong bg-oct-card px-3 py-2 text-[13px] text-oct-text" value={deleteMode} onChange={(event) => setDeleteMode(event.target.value)}>
                {!mustResolveReferences && <option value="reject">Delete only if still unreferenced</option>}
                <option value="flat">Replace references with Flat</option>
                {profiles.filter((profile) => profile.id !== deleteCandidate.id).map((profile) => (
                  <option key={profile.id} value={profile.id}>Replace with {profile.name}</option>
                ))}
              </select>
            </label>
            <div className="mt-5 flex justify-end gap-2">
              <button type="button" className={btnGhostSm} onClick={() => setDeleteCandidate(null)}>Cancel</button>
              <button type="button" className={btnDanger} disabled={saving} onClick={() => void confirmDelete()}>{saving ? "Deleting…" : "Delete profile"}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function SwitchRow({
  title,
  description,
  checked,
  disabled = false,
  onChange,
}: {
  title: string;
  description: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className={`flex items-center justify-between gap-4 px-4 py-3 ${disabled ? "opacity-40" : ""}`}>
      <div>
        <div className="text-[13.5px] text-oct-text">{title}</div>
        <div className="text-[11.5px] text-oct-faint">{description}</div>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onChange(!checked)}
        className={`inline-flex h-5 w-9 shrink-0 items-center rounded-full px-0.5 transition-colors ${checked ? "bg-oct-accent" : "bg-oct-border-strong"}`}
      >
        <span className={`h-4 w-4 rounded-full bg-white transition-transform ${checked ? "translate-x-4" : ""}`} />
      </button>
    </div>
  );
}

function StatusCard({
  title,
  value,
  detail,
  active = false,
}: {
  title: string;
  value: string;
  detail: string;
  active?: boolean;
}) {
  return (
    <div className={`${card} flex items-start gap-2.5 p-3`}>
      <span className={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${active ? "bg-oct-online" : "bg-oct-line"}`} />
      <div className="min-w-0">
        <div className="text-[10.5px] text-oct-faint">{title}</div>
        <div className="truncate text-[13px] text-oct-text">{value}</div>
        <div className="truncate text-[10px] text-oct-faint">{detail}</div>
      </div>
    </div>
  );
}
