import { create } from "zustand";
import {
  equalizerAttachCurrentOutput,
  equalizerAudioOutputs,
  equalizerClearManualOverride,
  equalizerConflicts,
  equalizerCreateDeviceRule,
  equalizerCreateProfile,
  equalizerCurrentOutput,
  equalizerDeleteDeviceRule,
  equalizerDeleteProfile,
  equalizerDetachCurrentOutput,
  equalizerReorderDeviceRules,
  equalizerPromoteLocalProfile,
  equalizerResolveConflict,
  equalizerSetDefault,
  equalizerSetLocalPreferences,
  equalizerSetManualOverride,
  equalizerSnapshot,
  equalizerUpdateDeviceRule,
  equalizerUpdateProfile,
} from "../ipc";
import type { EqualizerGraphDiagnostics } from "../player/audioGraph";
import {
  cloneEqualizerProfile,
  EQ_STATE_FORMAT_VERSION,
  type EqualizerDeleteProfileDisposition,
  type EqualizerConflict,
  type EqualizerDeviceRule,
  type EqualizerDeviceRuleInput,
  type EqualizerLocalPreferences,
  type EqualizerOutputSummary,
  type EqualizerProfile,
  type EqualizerProfileInput,
  type EqualizerProfileLayer,
  type EqualizerProfileTarget,
  type EqualizerSnapshot,
  type ResolvedEqualizer,
} from "./types";

const profileInput = (profile: EqualizerProfile): EqualizerProfileInput => ({
  id: profile.id,
  name: profile.name.trim(),
  format_version: profile.format_version,
  preamp_db: profile.preamp_db,
  auto_headroom_enabled: profile.auto_headroom_enabled,
  bands: profile.bands.map((band, index) => ({ ...band, position: index + 1 })),
});

const ruleInput = (rule: EqualizerDeviceRule): EqualizerDeviceRuleInput => ({
  id: rule.id,
  label: rule.label.trim(),
  action: rule.action,
  selectors: rule.selectors.map((selector) => ({ ...selector })),
  enabled: rule.enabled,
});

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error != null && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  if (typeof error === "object" && error != null) {
    const structured = error as { kind?: unknown; code?: unknown };
    const fields = [structured.kind, structured.code].filter(
      (value): value is string => typeof value === "string" && value.length > 0,
    );
    if (fields.length > 0) return fields.join(": ");
    try {
      return JSON.stringify(error);
    } catch {
      return "Unknown equalizer error";
    }
  }
  return String(error);
}

function generationIsNewer(next: string, current: string): boolean {
  try {
    return BigInt(next) > BigInt(current);
  } catch {
    return false;
  }
}

export function activeEqualizerLayer(snapshot: EqualizerSnapshot | null) {
  if (!snapshot) return null;
  return snapshot.active_layer === "local_only" ? snapshot.local : snapshot.synced;
}

export function activeEqualizerProfiles(snapshot: EqualizerSnapshot | null): EqualizerProfile[] {
  return activeEqualizerLayer(snapshot)?.profiles ?? [];
}

export function activeEqualizerRules(snapshot: EqualizerSnapshot | null): EqualizerDeviceRule[] {
  return activeEqualizerLayer(snapshot)?.device_rules ?? [];
}

export function activeEqualizerDefault(snapshot: EqualizerSnapshot | null): string | null {
  return activeEqualizerLayer(snapshot)?.default_profile_id ?? null;
}

type EqualizerStore = {
  snapshot: EqualizerSnapshot | null;
  resolved: ResolvedEqualizer | null;
  outputs: EqualizerOutputSummary[];
  conflicts: EqualizerConflict[];
  currentOutput: EqualizerOutputSummary | null;
  graph: EqualizerGraphDiagnostics | null;
  loading: boolean;
  saving: boolean;
  error: string | null;
  previewProfile: EqualizerProfile | null;
  previewAudible: boolean;
  previewBypassed: boolean;
  previewImmediateToken: number;
  previewStoppedMessage: string | null;

  load: () => Promise<void>;
  refreshOutputs: () => Promise<void>;
  acceptResolvedEvent: (resolved: ResolvedEqualizer) => void;
  setGraph: (graph: EqualizerGraphDiagnostics) => void;
  preview: (profile: EqualizerProfile, immediate?: boolean) => void;
  stopPreview: () => void;
  setPreviewBypassed: (bypassed: boolean) => void;
  clearPreviewMessage: () => void;
  resetForScopeChange: () => void;

  createProfile: (profile: EqualizerProfile) => Promise<void>;
  updateProfile: (profile: EqualizerProfile) => Promise<void>;
  deleteProfile: (
    profile: EqualizerProfile,
    disposition: EqualizerDeleteProfileDisposition,
  ) => Promise<void>;
  setDefault: (profileId: string | null) => Promise<void>;
  setPreferences: (preferences: EqualizerLocalPreferences) => Promise<void>;
  setManualOverride: (target: EqualizerProfileTarget) => Promise<void>;
  clearManualOverride: () => Promise<void>;
  attachCurrentOutput: (target: EqualizerProfileTarget) => Promise<void>;
  detachCurrentOutput: () => Promise<void>;
  createRule: (rule: EqualizerDeviceRuleInput) => Promise<void>;
  updateRule: (rule: EqualizerDeviceRule) => Promise<void>;
  deleteRule: (rule: EqualizerDeviceRule) => Promise<void>;
  reorderRules: (rules: EqualizerDeviceRule[]) => Promise<void>;
  resolveConflict: (
    conflictId: number,
    resolution: "keep_server" | "keep_local_copy" | "retry",
  ) => Promise<void>;
  promoteLocalProfile: (
    profileId: string,
    assignDefault: boolean,
    remapExactBindings: boolean,
  ) => Promise<void>;
};

async function refreshedSnapshot(): Promise<EqualizerSnapshot> {
  return equalizerSnapshot();
}

let scopeLoadGeneration = 0;

export const useEqualizerStore = create<EqualizerStore>((set, get) => ({
  snapshot: null,
  resolved: null,
  outputs: [],
  conflicts: [],
  currentOutput: null,
  graph: null,
  loading: false,
  saving: false,
  error: null,
  previewProfile: null,
  previewAudible: false,
  previewBypassed: false,
  previewImmediateToken: 0,
  previewStoppedMessage: null,

  load: async () => {
    const generation = ++scopeLoadGeneration;
    set({ loading: true, error: null });
    try {
      const [snapshot, conflicts] = await Promise.all([
        refreshedSnapshot(),
        equalizerConflicts(),
      ]);
      if (generation !== scopeLoadGeneration) return;
      const topLevelSupported =
        snapshot.support_state !== "future_format" &&
        (snapshot.active_layer === "local_only" ||
          snapshot.synced.state_format_version === EQ_STATE_FORMAT_VERSION);
      set({
        snapshot,
        conflicts,
        resolved: topLevelSupported ? snapshot.resolved : { ...snapshot.resolved, profile: null },
        loading: false,
        error: topLevelSupported
          ? null
          : "This equalizer state requires a newer Octave client. Playback is Flat.",
      });
      void get().refreshOutputs();
    } catch (error) {
      if (generation === scopeLoadGeneration) {
        set({ loading: false, error: errorMessage(error) });
      }
    }
  },

  refreshOutputs: async () => {
    try {
      const [outputs, currentOutput] = await Promise.all([
        equalizerAudioOutputs(),
        equalizerCurrentOutput(),
      ]);
      set({ outputs, currentOutput });
    } catch {
      // Route discovery is optional. Default/manual EQ remains fully usable.
      set({ outputs: [], currentOutput: get().resolved?.output_summary ?? null });
    }
  },

  acceptResolvedEvent: (next) => {
    const current = get().resolved;
    // Never adopt an epoch from an unsolicited event. A snapshot response is
    // the only operation allowed to establish/switch the account scope.
    if (!current || next.scope_epoch !== current.scope_epoch) return;
    if (!generationIsNewer(next.resolution_generation, current.resolution_generation)) return;
    const previewStopped = get().previewAudible;
    set({
      resolved: next,
      currentOutput: next.output_summary,
      previewAudible: false,
      previewBypassed: false,
      previewStoppedMessage: previewStopped
        ? "The output route changed. Audible preview stopped; your unsaved draft is preserved."
        : get().previewStoppedMessage,
    });
  },

  setGraph: (graph) => set({ graph }),

  preview: (profile, immediate = false) =>
    set((state) => ({
      previewProfile: cloneEqualizerProfile(profile),
      previewAudible: true,
      previewStoppedMessage: null,
      previewImmediateToken: immediate
        ? state.previewImmediateToken + 1
        : state.previewImmediateToken,
    })),

  stopPreview: () =>
    set({ previewProfile: null, previewAudible: false, previewBypassed: false }),

  setPreviewBypassed: (previewBypassed) => set({ previewBypassed }),
  clearPreviewMessage: () => set({ previewStoppedMessage: null }),
  resetForScopeChange: () => {
    scopeLoadGeneration += 1;
    set({
      snapshot: null,
      resolved: null,
      outputs: [],
      conflicts: [],
      currentOutput: null,
      previewProfile: null,
      previewAudible: false,
      previewBypassed: false,
      previewStoppedMessage: null,
      loading: false,
      saving: false,
      error: null,
    });
  },

  createProfile: async (profile) => {
    set({ saving: true, error: null });
    try {
      await equalizerCreateProfile(profileInput(profile));
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false, previewProfile: null, previewAudible: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  updateProfile: async (profile) => {
    set({ saving: true, error: null });
    try {
      await equalizerUpdateProfile(profileInput(profile), profile.revision);
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false, previewProfile: null, previewAudible: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  deleteProfile: async (profile, disposition) => {
    const snapshot = get().snapshot;
    if (!snapshot) return;
    const activeRules = activeEqualizerRules(snapshot);
    const referencing = activeRules
      .filter((rule) => rule.action.kind === "profile" && rule.action.profile_id === profile.id)
      .map((rule) => ({ id: rule.id, expected_revision: rule.revision }));
    set({ saving: true, error: null });
    try {
      await equalizerDeleteProfile({
        profile_id: profile.id,
        expected_revision: profile.revision,
        expected_settings_revision:
          snapshot.active_layer === "synced" ? snapshot.synced.settings_revision : "0",
        referencing_rules: referencing,
        disposition,
        local_binding_disposition:
          disposition.kind === "replace_with_profile"
            ? profileTarget(snapshot.active_layer, disposition.profile_id)
            : disposition.kind === "replace_with_flat"
              ? profileTarget(snapshot.active_layer, null)
              : null,
      });
      const fresh = await refreshedSnapshot();
      set({ snapshot: fresh, resolved: fresh.resolved, saving: false, previewProfile: null, previewAudible: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  setDefault: async (profileId) => {
    const snapshot = get().snapshot;
    if (!snapshot) return;
    set({ saving: true, error: null });
    try {
      await equalizerSetDefault(profileId, snapshot.synced.settings_revision);
      const fresh = await refreshedSnapshot();
      set({ snapshot: fresh, resolved: fresh.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  setPreferences: async (preferences) => {
    const before = get().snapshot;
    if (before) set({ snapshot: { ...before, preferences } });
    try {
      await equalizerSetLocalPreferences(preferences);
      const fresh = await refreshedSnapshot();
      set({ snapshot: fresh, resolved: fresh.resolved, error: null });
    } catch (error) {
      set({ snapshot: before, error: errorMessage(error) });
      throw error;
    }
  },

  setManualOverride: async (target) => {
    try {
      const resolved = await equalizerSetManualOverride(target);
      set({ resolved, error: null });
    } catch (error) {
      set({ error: errorMessage(error) });
      throw error;
    }
  },

  clearManualOverride: async () => {
    try {
      const resolved = await equalizerClearManualOverride();
      set({ resolved, error: null });
    } catch (error) {
      set({ error: errorMessage(error) });
      throw error;
    }
  },

  attachCurrentOutput: async (target) => {
    try {
      await equalizerAttachCurrentOutput(target);
      await get().load();
    } catch (error) {
      set({ error: errorMessage(error) });
      throw error;
    }
  },

  detachCurrentOutput: async () => {
    set({ saving: true, error: null });
    try {
      await equalizerDetachCurrentOutput();
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  createRule: async (rule) => {
    set({ saving: true, error: null });
    try {
      await equalizerCreateDeviceRule(rule);
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  updateRule: async (rule) => {
    set({ saving: true, error: null });
    try {
      await equalizerUpdateDeviceRule(ruleInput(rule), rule.revision);
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  deleteRule: async (rule) => {
    set({ saving: true, error: null });
    try {
      await equalizerDeleteDeviceRule(rule.id, rule.revision);
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  reorderRules: async (rules) => {
    set({ saving: true, error: null });
    try {
      await equalizerReorderDeviceRules(
        rules.map((rule) => ({ id: rule.id, expected_revision: rule.revision })),
      );
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  resolveConflict: async (conflictId, resolution) => {
    set({ saving: true, error: null });
    try {
      await equalizerResolveConflict(conflictId, resolution);
      const [snapshot, conflicts] = await Promise.all([
        refreshedSnapshot(),
        equalizerConflicts(),
      ]);
      set({ snapshot, resolved: snapshot.resolved, conflicts, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },

  promoteLocalProfile: async (profileId, assignDefault, remapExactBindings) => {
    set({ saving: true, error: null });
    try {
      await equalizerPromoteLocalProfile(profileId, assignDefault, remapExactBindings);
      const snapshot = await refreshedSnapshot();
      set({ snapshot, resolved: snapshot.resolved, saving: false });
    } catch (error) {
      set({ saving: false, error: errorMessage(error) });
      throw error;
    }
  },
}));

export function profileTarget(layer: EqualizerProfileLayer, profileId: string | null): EqualizerProfileTarget {
  return {
    layer,
    action: profileId == null ? { kind: "bypass" } : { kind: "profile", profile_id: profileId },
  };
}
