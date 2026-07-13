import { useMemo, useState } from "react";
import { btnDangerSm, btnGhostSm, btnPrimary, card, input, label } from "../lib/ui";
import { TrashIcon } from "../components/icons";
import {
  activeEqualizerProfiles,
  activeEqualizerRules,
  profileTarget,
  useEqualizerStore,
} from "./store";
import {
  createEqualizerId,
  type EqualizerDeviceRuleInput,
  type EqualizerOutputSummary,
  type EqualizerTriggerKind,
} from "./types";

const accuracyLabel: Record<EqualizerOutputSummary["accuracy"], string> = {
  exact: "Octave output",
  predicted: "Predicted output",
  default: "System default output",
  connected_only: "Connected device",
  unavailable: "Output unavailable",
};

const normalizeMatcherPreview = (value: string) =>
  value.normalize("NFKC").toLocaleLowerCase("en-US").trim().replace(/\s+/gu, " ");

function sameOutput(a: EqualizerOutputSummary, b: EqualizerOutputSummary | null): boolean {
  return (
    b != null &&
    a.display_name === b.display_name &&
    a.route_kind === b.route_kind &&
    a.accuracy === b.accuracy
  );
}

export function DeviceRules({ readOnly = false }: { readOnly?: boolean }) {
  const snapshot = useEqualizerStore((state) => state.snapshot);
  const currentOutput = useEqualizerStore((state) => state.currentOutput);
  const outputs = useEqualizerStore((state) => state.outputs);
  const saving = useEqualizerStore((state) => state.saving);
  const createRule = useEqualizerStore((state) => state.createRule);
  const updateRule = useEqualizerStore((state) => state.updateRule);
  const deleteRule = useEqualizerStore((state) => state.deleteRule);
  const reorderRules = useEqualizerStore((state) => state.reorderRules);
  const attachCurrentOutput = useEqualizerStore((state) => state.attachCurrentOutput);
  const detachCurrentOutput = useEqualizerStore((state) => state.detachCurrentOutput);

  const profiles = useMemo(() => activeEqualizerProfiles(snapshot), [snapshot]);
  const rules = useMemo(() => activeEqualizerRules(snapshot), [snapshot]);
  const [showCreate, setShowCreate] = useState(false);
  const [labelText, setLabelText] = useState("");
  const [matcher, setMatcher] = useState("");
  const [profileId, setProfileId] = useState<string>("");
  const [trigger, setTrigger] = useState<EqualizerTriggerKind>("active_output");

  const beginCreate = () => {
    setLabelText(currentOutput?.display_name ?? "New output rule");
    setMatcher(normalizeMatcherPreview(currentOutput?.display_name ?? ""));
    setProfileId("");
    setTrigger(currentOutput?.accuracy === "connected_only" ? "connected" : "active_output");
    setShowCreate(true);
  };

  const submit = async () => {
    if (!currentOutput || !labelText.trim() || !matcher.trim()) return;
    const rule: EqualizerDeviceRuleInput = {
      id: createEqualizerId(),
      label: labelText.trim(),
      action: profileId ? { kind: "profile", profile_id: profileId } : { kind: "bypass" },
      enabled: true,
      selectors: [
        {
          normalization_version: 1,
          route_kind: currentOutput.route_kind,
          normalized_name: normalizeMatcherPreview(matcher),
          vendor_id: null,
          product_id: null,
          platform_scope: null,
          trigger,
        },
      ],
    };
    await createRule(rule);
    setShowCreate(false);
  };

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-2">
        <div className={label}>DETECTED OUTPUTS</div>
        <div className={`${card} divide-y divide-oct-border`}>
          {(outputs.length ? outputs : currentOutput ? [currentOutput] : []).map((output, index) => {
            const current = sameOutput(output, currentOutput);
            return (
              <div key={`${output.display_name}-${output.route_kind}-${index}`} className="flex items-center gap-3 px-4 py-3">
                <span
                  className={`h-2 w-2 rounded-full ${current ? "bg-oct-online" : "bg-oct-line"}`}
                  aria-hidden="true"
                />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13.5px] text-oct-text">{output.display_name}</div>
                  <div className="text-[11px] text-oct-faint">
                    {accuracyLabel[output.accuracy]} · {output.route_kind.replace("_", " ")} ·{" "}
                    {output.binding_stability.replace("_", " ")}
                  </div>
                </div>
                {current && (
                  <span className="rounded-full bg-oct-online/10 px-2 py-0.5 text-[10.5px] text-oct-online">
                    Active
                  </span>
                )}
              </div>
            );
          })}
          {!currentOutput && outputs.length === 0 && (
            <div className="px-4 py-3 text-[12px] text-oct-faint">
              Output detection is unavailable. Default and manual equalizer selection still work.
            </div>
          )}
        </div>
        {currentOutput && currentOutput.accuracy !== "exact" && (
          <p className="text-[10.5px] leading-relaxed text-oct-faint">
            {accuracyLabel[currentOutput.accuracy]} is an honest routing confidence label. Octave
            will not claim a connected device is the active route; use a connected rule only when
            you explicitly want that fallback.
          </p>
        )}
      </div>

      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between gap-3">
          <div>
            <div className={label}>AUTOMATIC PROFILE RULES</div>
            <div className="mt-1 text-[11px] text-oct-faint">Highest priority matching rule wins.</div>
          </div>
          <button type="button" className={btnGhostSm} disabled={readOnly || !currentOutput} onClick={beginCreate}>
            Add current output
          </button>
        </div>

        {showCreate && currentOutput && (
          <div className={`${card} flex flex-col gap-3 p-4`}>
            <div className="text-[13.5px] font-medium text-oct-text">New portable rule</div>
            <label>
              <span className="text-[11px] text-oct-faint">Rule label</span>
              <input disabled={readOnly} className={`${input} mt-1`} value={labelText} onChange={(event) => setLabelText(event.target.value)} />
            </label>
            <label>
              <span className="text-[11px] text-oct-faint">Confirmed device-name matcher</span>
              <input disabled={readOnly} className={`${input} mt-1 font-mono text-[12px]`} value={matcher} onChange={(event) => setMatcher(event.target.value)} />
            </label>
            <div className="grid gap-3 sm:grid-cols-2">
              <label>
                <span className="text-[11px] text-oct-faint">Action</span>
                <select disabled={readOnly} className={`${input} mt-1`} value={profileId} onChange={(event) => setProfileId(event.target.value)}>
                  <option value="">Bypass (Flat)</option>
                  {profiles.map((profile) => (
                    <option key={profile.id} value={profile.id}>{profile.name}</option>
                  ))}
                </select>
              </label>
              <label>
                <span className="text-[11px] text-oct-faint">Trigger</span>
                <select disabled={readOnly} className={`${input} mt-1`} value={trigger} onChange={(event) => setTrigger(event.target.value as EqualizerTriggerKind)}>
                  <option value="active_output">Active output</option>
                  <option value="connected">Connected (explicit fallback)</option>
                </select>
              </label>
            </div>
            <div className="flex justify-end gap-2">
              <button type="button" className={btnGhostSm} onClick={() => setShowCreate(false)}>Cancel</button>
              <button type="button" className={btnPrimary} disabled={readOnly || saving || !labelText.trim() || !matcher.trim()} onClick={() => void submit()}>
                Create rule
              </button>
            </div>
          </div>
        )}

        <div className={`${card} divide-y divide-oct-border`}>
          {rules.map((rule, index) => {
            const actionProfileId = rule.action.kind === "profile" ? rule.action.profile_id : null;
            const actionName =
              actionProfileId == null
                ? "Flat"
                : profiles.find((profile) => profile.id === actionProfileId)?.name ??
                  "Missing profile";
            return (
              <div key={rule.id} className="flex items-center gap-3 px-4 py-3">
                <button
                  type="button"
                  role="switch"
                  aria-checked={rule.enabled}
                  aria-label={`Enable ${rule.label}`}
                    disabled={readOnly || saving}
                    onClick={() => void updateRule({ ...rule, enabled: !rule.enabled })}
                  className={`inline-flex h-5 w-9 shrink-0 items-center rounded-full px-0.5 transition-colors ${
                    rule.enabled ? "bg-oct-accent" : "bg-oct-border-strong"
                  }`}
                >
                  <span className={`h-4 w-4 rounded-full bg-white transition-transform ${rule.enabled ? "translate-x-4" : ""}`} />
                </button>
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13px] text-oct-text">{rule.label}</div>
                  <div className="truncate text-[10.5px] text-oct-faint">
                    {actionName} · {rule.selectors.map((selector) => selector.normalized_name).join(", ")}
                  </div>
                </div>
                <div className="flex items-center gap-1">
                  <span className="mr-1 font-mono text-[10px] text-oct-faint">#{index + 1}</span>
                  <button
                    type="button"
                    className={btnGhostSm}
                    disabled={readOnly || saving || index === 0}
                    onClick={() => {
                      const ordered = [...rules];
                      [ordered[index - 1], ordered[index]] = [ordered[index], ordered[index - 1]];
                      void reorderRules(ordered);
                    }}
                    aria-label={`Move ${rule.label} up`}
                  >
                    ↑
                  </button>
                  <button
                    type="button"
                    className={btnGhostSm}
                    disabled={readOnly || saving || index === rules.length - 1}
                    onClick={() => {
                      const ordered = [...rules];
                      [ordered[index], ordered[index + 1]] = [ordered[index + 1], ordered[index]];
                      void reorderRules(ordered);
                    }}
                    aria-label={`Move ${rule.label} down`}
                  >
                    ↓
                  </button>
                </div>
                <button type="button" className={btnDangerSm} disabled={readOnly || saving} onClick={() => void deleteRule(rule)} aria-label={`Delete ${rule.label}`}>
                  <TrashIcon size={12} />
                </button>
              </div>
            );
          })}
          {rules.length === 0 && (
            <div className="px-4 py-3 text-[12px] text-oct-faint">No automatic output rules yet.</div>
          )}
        </div>
      </div>

      {currentOutput?.binding_stability === "persistent_exact" && (
        <div className="flex flex-col gap-2">
          <div className={label}>THIS DEVICE ONLY</div>
          <div className={`${card} flex flex-wrap items-center justify-between gap-3 px-4 py-3`}>
            <div>
              <div className="text-[13px] text-oct-text">Attach current output exactly</div>
              <div className="text-[11px] text-oct-faint">
                The native endpoint identity is keyed and stays on this device; React never sees a raw hardware ID.
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <select
                className="rounded-lg border border-oct-border-strong bg-oct-card px-2.5 py-1.5 text-[12px] text-oct-text"
                defaultValue=""
                disabled={readOnly || saving}
                onChange={(event) => {
                  const id = event.target.value;
                  if (!snapshot || !id) return;
                  void attachCurrentOutput(profileTarget(snapshot.active_layer, id === "flat" ? null : id));
                  event.target.value = "";
                }}
                aria-label="Attach current output to profile"
              >
                <option value="">Choose…</option>
                <option value="flat">Bypass (Flat)</option>
                {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name}</option>)}
              </select>
              <button
                type="button"
                className={btnGhostSm}
                disabled={readOnly || saving}
                onClick={() => void detachCurrentOutput()}
              >
                Remove current binding
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
