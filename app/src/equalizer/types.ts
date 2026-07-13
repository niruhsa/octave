/**
 * Equalizer domain types shared by the Settings UI, native IPC boundary, and
 * Web Audio runtime. Revision tokens deliberately remain decimal strings:
 * JavaScript numbers cannot losslessly carry every signed 64-bit revision.
 */

export const EQ_FORMAT_VERSION = 1 as const;
export const EQ_STATE_FORMAT_VERSION = 1 as const;

export const EQ_LIMITS = {
  profiles: 64,
  rules: 64,
  selectorsPerRule: 8,
  nameScalars: 100,
  bands: 32,
  preampDb: { min: -30, max: 12 },
  frequencyHz: { min: 10, max: 20_000 },
  gainDb: { min: -24, max: 24 },
  q: { min: 0.1, max: 30 },
  importBytes: 64 * 1024,
  importLines: 512,
} as const;

export type EqualizerBand = {
  /** Canonical, contiguous, one-based order. */
  position: number;
  enabled: boolean;
  /** Version 1 deliberately supports only parametric peaking filters. */
  filter_kind: "peaking";
  frequency_hz: number;
  gain_db: number;
  q: number;
};

export type EqualizerProfile = {
  id: string;
  name: string;
  format_version: number;
  preamp_db: number;
  auto_headroom_enabled: boolean;
  bands: EqualizerBand[];
  revision: string;
  created_at: string;
  updated_at: string;
  /** Client-only presentation metadata returned by the native cache layer. */
  source?: "synced" | "local_only";
  unsynced?: boolean;
  conflict?: boolean;
};

export type EqualizerProfileInput = Pick<
  EqualizerProfile,
  "id" | "name" | "format_version" | "preamp_db" | "auto_headroom_enabled" | "bands"
>;

export type EqualizerRouteKind =
  | "bluetooth"
  | "wired"
  | "usb"
  | "hdmi"
  | "airplay"
  | "builtin"
  | "unknown";

export type EqualizerRouteAccuracy =
  | "exact"
  | "predicted"
  | "default"
  | "connected_only"
  | "unavailable";

export type EqualizerBindingStability =
  | "persistent_exact"
  | "session_only"
  | "unavailable";

export type EqualizerPlatform =
  | "windows"
  | "android"
  | "macos"
  | "linux"
  | "ios"
  | "web";

export type EqualizerTriggerKind = "active_output" | "connected";

export type EqualizerDeviceSelector = {
  normalization_version: number;
  route_kind: EqualizerRouteKind;
  normalized_name: string;
  vendor_id: string | null;
  product_id: string | null;
  platform_scope: EqualizerPlatform | null;
  trigger: EqualizerTriggerKind;
};

export type EqualizerRuleAction =
  | { kind: "profile"; profile_id: string }
  | { kind: "bypass" };

export type EqualizerDeviceRule = {
  id: string;
  label: string;
  action: EqualizerRuleAction;
  selectors: EqualizerDeviceSelector[];
  /** Higher-precedence-first read order; writes use the dedicated reorder call. */
  priority: number;
  enabled: boolean;
  revision: string;
  source?: "synced" | "local_only";
  unsynced?: boolean;
  conflict?: boolean;
};

export type EqualizerDeviceRuleInput = Pick<
  EqualizerDeviceRule,
  "id" | "label" | "action" | "selectors" | "enabled"
>;

export type EqualizerState = {
  state_format_version: number;
  state_revision: string;
  settings_revision: string;
  default_profile_id: string | null;
  profiles: EqualizerProfile[];
  device_rules: EqualizerDeviceRule[];
  /** Native cache diagnostics; absent on the server wire model. */
  source?: "server" | "cache" | "local_only";
  last_synced_at?: string | null;
  support_state?: "unknown" | "supported" | "unsupported" | "future_format";
};

export type EqualizerLocalPreferences = {
  /** The local safety gate. Fresh installs and upgrades start disabled. */
  master_enabled: boolean;
  /** Portable/local rules are ignored until explicitly enabled. */
  automatic_switching_enabled: boolean;
};

/** Redacted output information that is safe to expose to the WebView. */
export type EqualizerOutputSummary = {
  display_name: string;
  route_kind: EqualizerRouteKind;
  accuracy: EqualizerRouteAccuracy;
  binding_stability: EqualizerBindingStability;
};

export type EqualizerResolutionReason =
  | "disabled"
  | "manual"
  | "local_exact"
  | "portable_rule"
  | "connected_fallback"
  | "default"
  | "flat"
  | "unsupported";

export type EqualizerProfileLayer = "synced" | "local_only";

export type EqualizerProfileTarget = {
  layer: EqualizerProfileLayer;
  action: EqualizerRuleAction;
};

export type ResolvedEqualizer = {
  profile: EqualizerProfile | null;
  layer: EqualizerProfileLayer | null;
  reason: EqualizerResolutionReason;
  output_summary: EqualizerOutputSummary | null;
  state_revision: string;
  scope_epoch: string;
  resolution_generation: string;
};

export type EqualizerSnapshot = {
  support_state: "unknown" | "supported" | "unsupported" | "future_format";
  synced: EqualizerState;
  local: EqualizerLocalState;
  active_layer: EqualizerProfileLayer;
  preferences: EqualizerLocalPreferences;
  resolved: ResolvedEqualizer;
  pending_count: number;
  conflict_count: number;
};

export type EqualizerLocalState = {
  default_profile_id: string | null;
  profiles: EqualizerProfile[];
  device_rules: EqualizerDeviceRule[];
};

export type EqualizerMutationResponse = {
  changed: boolean;
  audit_id: string | null;
  state: EqualizerState;
  resolved?: ResolvedEqualizer;
};

export type EqualizerEntityRevision = {
  id: string;
  expected_revision: string;
};

export type EqualizerDeleteProfileDisposition =
  | { kind: "reject_if_referenced" }
  | { kind: "replace_with_profile"; profile_id: string }
  | { kind: "replace_with_flat" };

export type EqualizerDeleteProfileRequest = {
  profile_id: string;
  expected_revision: string;
  expected_settings_revision: string;
  referencing_rules: EqualizerEntityRevision[];
  disposition: EqualizerDeleteProfileDisposition;
  local_binding_disposition: EqualizerProfileTarget | null;
};

export type EqualizerConflict = {
  id: number;
  dependency_group: string;
  op_type: string;
  entity_id: string | null;
  payload_json: string;
  base_revision: string | null;
  server_revision: string | null;
  error_code: string;
  error_message: string;
  created_at: string;
};

export type NativeEqualizerParseWarning = {
  line: number | null;
  message: string;
};

export type NativeParsedEqualizerProfile = {
  profile: EqualizerProfile;
  warnings: NativeEqualizerParseWarning[];
};

export type EqualizerChangeSummary = {
  audit_id: string;
  action: string;
  actor_id: string | null;
  owner_id: string;
  resource_type: string;
  resource_id: string | null;
  created_at: string;
  before_state_revision: string;
  after_state_revision: string;
};

export type EqualizerChangePage = {
  changes: EqualizerChangeSummary[];
  next_cursor: string | null;
};

export type EqualizerChangeDetail = {
  change: EqualizerChangeSummary;
  /** Present only for Admin detail; Manager detail stays redacted. */
  before_json: string | null;
  after_json: string | null;
  current_state_revision: string | null;
  rollback_eligible: boolean;
};

export type EqualizerChangedResource = {
  resource_type: string;
  resource_id: string | null;
  change: string;
};

export type EqualizerRollbackResponse = {
  target_owner_id: string;
  state_revision: string;
  audit_id: string;
  changed_resources: EqualizerChangedResource[];
};

export type EqualizerParseWarning = {
  code: "missing_preamp";
  message: string;
};

export type ParsedEqualizerText = {
  preamp_db: number;
  bands: EqualizerBand[];
  warnings: EqualizerParseWarning[];
};

export function createEqualizerId(): string {
  if (typeof globalThis.crypto !== "undefined" && "randomUUID" in globalThis.crypto) {
    return globalThis.crypto.randomUUID();
  }
  // Browser fallback for older WebViews. This preserves UUID shape; native and
  // server still validate it before persistence.
  return "10000000-1000-4000-8000-100000000000".replace(/[018]/g, (c) =>
    (
      Number(c) ^
      ((typeof globalThis.crypto !== "undefined"
        ? globalThis.crypto.getRandomValues(new Uint8Array(1))[0]
        : Math.floor(Math.random() * 256)) &
        (15 >> (Number(c) / 4)))
    ).toString(16),
  );
}

export function createStarterProfile(name = "New equalizer"): EqualizerProfile {
  const now = new Date().toISOString();
  const frequencies = [60, 250, 1_000, 4_000, 12_000];
  return {
    id: createEqualizerId(),
    name,
    format_version: EQ_FORMAT_VERSION,
    preamp_db: 0,
    auto_headroom_enabled: true,
    bands: frequencies.map((frequency_hz, index) => ({
      position: index + 1,
      enabled: true,
      filter_kind: "peaking" as const,
      frequency_hz,
      gain_db: 0,
      q: 1,
    })),
    revision: "0",
    created_at: now,
    updated_at: now,
  };
}

export function cloneEqualizerProfile(profile: EqualizerProfile): EqualizerProfile {
  return { ...profile, bands: profile.bands.map((band) => ({ ...band })) };
}
