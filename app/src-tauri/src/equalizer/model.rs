use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

pub const EQ_STATE_FORMAT_VERSION: u32 = 1;
pub const EQ_PROFILE_FORMAT_VERSION: u32 = 1;
pub const EQ_NORMALIZATION_VERSION: u32 = 1;
pub const MAX_PROFILES: usize = 64;
pub const MAX_RULES: usize = 64;
pub const MAX_SELECTORS_PER_RULE: usize = 8;
pub const MAX_BANDS: usize = 32;
pub const MAX_NAME_CHARS: usize = 100;

/// Lossless server revision token. REST/Tauri serialize it as a decimal
/// string so JavaScript never rounds an `i64` compare-and-swap value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub i64);

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Revision {
    type Err = std::num::ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

impl Serialize for Revision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Revision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            String(String),
            Integer(i64),
        }

        match Repr::deserialize(deserializer)? {
            Repr::String(value) => value.parse().map_err(serde::de::Error::custom),
            // Tolerate integer input from gRPC-adjacent tests/old REST servers;
            // serialization is always a string.
            Repr::Integer(value) => Ok(Self(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    Peaking,
}

impl FilterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Peaking => "peaking",
        }
    }
}

impl FromStr for FilterKind {
    type Err = EqualizerValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "peaking" | "PK" | "pk" => Ok(Self::Peaking),
            other => Err(EqualizerValidationError::new(format!(
                "unsupported filter kind '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerBand {
    pub position: u32,
    pub enabled: bool,
    pub filter_kind: FilterKind,
    pub frequency_hz: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerProfile {
    pub id: String,
    pub name: String,
    pub format_version: u32,
    pub preamp_db: f64,
    pub auto_headroom_enabled: bool,
    pub bands: Vec<EqualizerBand>,
    pub revision: Revision,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerProfileInput {
    pub id: String,
    pub name: String,
    #[serde(default = "profile_format_version")]
    pub format_version: u32,
    pub preamp_db: f64,
    #[serde(default = "default_true")]
    pub auto_headroom_enabled: bool,
    pub bands: Vec<EqualizerBand>,
}

fn profile_format_version() -> u32 {
    EQ_PROFILE_FORMAT_VERSION
}

fn default_true() -> bool {
    true
}

impl EqualizerProfileInput {
    pub fn into_local_profile(self) -> EqualizerProfile {
        let now = now_string();
        EqualizerProfile {
            id: self.id,
            name: self.name,
            format_version: self.format_version,
            preamp_db: self.preamp_db,
            auto_headroom_enabled: self.auto_headroom_enabled,
            bands: self.bands,
            revision: Revision(0),
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

impl From<&EqualizerProfile> for EqualizerProfileInput {
    fn from(value: &EqualizerProfile) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            format_version: value.format_version,
            preamp_db: value.preamp_db,
            auto_headroom_enabled: value.auto_headroom_enabled,
            bands: value.bands.clone(),
        }
    }
}

impl EqualizerProfile {
    pub fn new_local(name: impl Into<String>, bands: Vec<EqualizerBand>) -> Self {
        let now = now_string();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            format_version: EQ_PROFILE_FORMAT_VERSION,
            preamp_db: 0.0,
            auto_headroom_enabled: true,
            bands,
            revision: Revision(0),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn five_band_starter(name: impl Into<String>) -> Self {
        let frequencies = [60.0, 250.0, 1_000.0, 4_000.0, 12_000.0];
        let bands = frequencies
            .into_iter()
            .enumerate()
            .map(|(index, frequency_hz)| EqualizerBand {
                position: (index + 1) as u32,
                enabled: true,
                filter_kind: FilterKind::Peaking,
                frequency_hz,
                gain_db: 0.0,
                q: 1.0,
            })
            .collect();
        Self::new_local(name, bands)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    Bluetooth,
    Wired,
    Usb,
    Hdmi,
    Airplay,
    Builtin,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteAccuracy {
    Exact,
    Predicted,
    Default,
    ConnectedOnly,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStability {
    PersistentExact,
    SessionOnly,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    Android,
    Macos,
    Linux,
    Ios,
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    ActiveOutput,
    Connected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableDeviceSelector {
    #[serde(default = "normalization_version")]
    pub normalization_version: u32,
    pub route_kind: RouteKind,
    pub normalized_name: String,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub platform_scope: Option<Platform>,
    pub trigger: TriggerKind,
}

fn normalization_version() -> u32 {
    EQ_NORMALIZATION_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleAction {
    Profile { profile_id: String },
    Bypass,
}

impl RuleAction {
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::Profile { profile_id } => Some(profile_id),
            Self::Bypass => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerDeviceRule {
    pub id: String,
    pub label: String,
    pub action: RuleAction,
    pub selectors: Vec<PortableDeviceSelector>,
    pub priority: i32,
    pub enabled: bool,
    pub revision: Revision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerDeviceRuleInput {
    pub id: String,
    pub label: String,
    pub action: RuleAction,
    pub selectors: Vec<PortableDeviceSelector>,
    pub enabled: bool,
}

impl From<&EqualizerDeviceRule> for EqualizerDeviceRuleInput {
    fn from(value: &EqualizerDeviceRule) -> Self {
        Self {
            id: value.id.clone(),
            label: value.label.clone(),
            action: value.action.clone(),
            selectors: value.selectors.clone(),
            enabled: value.enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerState {
    pub state_format_version: u32,
    pub state_revision: Revision,
    pub settings_revision: Revision,
    pub default_profile_id: Option<String>,
    pub profiles: Vec<EqualizerProfile>,
    pub device_rules: Vec<EqualizerDeviceRule>,
}

impl Default for EqualizerState {
    fn default() -> Self {
        Self {
            state_format_version: EQ_STATE_FORMAT_VERSION,
            state_revision: Revision(0),
            settings_revision: Revision(0),
            default_profile_id: None,
            profiles: Vec::new(),
            device_rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerMutationResponse {
    pub changed: bool,
    pub audit_id: Option<String>,
    pub state: EqualizerState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerStateFetch {
    pub not_modified: bool,
    pub state: Option<EqualizerState>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRevision {
    pub id: String,
    pub expected_revision: Revision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeleteProfileDisposition {
    RejectIfReferenced,
    ReplaceWithProfile { profile_id: String },
    ReplaceWithFlat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteProfileRequest {
    pub profile_id: String,
    pub expected_revision: Revision,
    pub expected_settings_revision: Revision,
    pub referencing_rules: Vec<EntityRevision>,
    pub disposition: DeleteProfileDisposition,
    #[serde(default)]
    pub local_binding_disposition: Option<ProfileTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePage {
    pub changes: Vec<EqualizerChangeSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerChangeSummary {
    pub audit_id: String,
    pub action: String,
    pub actor_id: Option<String>,
    pub owner_id: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub created_at: String,
    pub before_state_revision: Revision,
    pub after_state_revision: Revision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerChangeDetail {
    pub change: EqualizerChangeSummary,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub current_state_revision: Option<Revision>,
    pub rollback_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerChangedResource {
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub change: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerRollbackResponse {
    pub target_owner_id: String,
    pub state_revision: Revision,
    pub audit_id: String,
    pub changed_resources: Vec<EqualizerChangedResource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LocalEqualizerState {
    pub default_profile_id: Option<String>,
    pub profiles: Vec<EqualizerProfile>,
    pub device_rules: Vec<EqualizerDeviceRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileLayer {
    Synced,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileTarget {
    pub layer: ProfileLayer,
    pub action: RuleAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    #[default]
    Unknown,
    Supported,
    Unsupported,
    /// Endpoint exists, but this build cannot safely interpret its schema.
    FutureFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LocalPreferences {
    pub master_enabled: bool,
    pub automatic_switching_enabled: bool,
}

/// Native route descriptor. Per-unit identifiers are never serialized into
/// an IPC payload; the adapter may use them only to derive a local HMAC key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOutput {
    #[serde(default, skip_serializing, skip_deserializing)]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing, skip_deserializing)]
    pub local_endpoint_key: Option<String>,
    pub display_name: String,
    pub route_kind: RouteKind,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub connected: bool,
    pub selected: bool,
    pub accuracy: RouteAccuracy,
    pub binding_stability: BindingStability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOutputSummary {
    pub display_name: String,
    pub route_kind: RouteKind,
    pub accuracy: RouteAccuracy,
    pub binding_stability: BindingStability,
}

impl From<&AudioOutput> for AudioOutputSummary {
    fn from(value: &AudioOutput) -> Self {
        Self {
            display_name: value.display_name.clone(),
            route_kind: value.route_kind,
            accuracy: value.accuracy,
            binding_stability: value.binding_stability,
        }
    }
}

/// Private persistence-only binding. Deliberately has no serde implementation:
/// endpoint keys must never become an IPC or server DTO by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactBinding {
    pub endpoint_key: String,
    pub display_label: Option<String>,
    pub target: ProfileTarget,
    pub orphaned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveReason {
    Disabled,
    Manual,
    LocalExact,
    PortableRule,
    ConnectedFallback,
    Default,
    Flat,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedEqualizer {
    pub profile: Option<EqualizerProfile>,
    pub layer: Option<ProfileLayer>,
    pub reason: ResolveReason,
    pub output_summary: Option<AudioOutputSummary>,
    pub state_revision: Revision,
    /// Opaque local ordering tokens, serialized as decimal strings.
    pub scope_epoch: Revision,
    pub resolution_generation: Revision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqualizerSnapshot {
    pub support_state: SupportState,
    pub synced: EqualizerState,
    pub local: LocalEqualizerState,
    pub active_layer: ProfileLayer,
    pub preferences: LocalPreferences,
    pub resolved: ResolvedEqualizer,
    pub pending_count: i64,
    pub conflict_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EqualizerConflict {
    pub id: i64,
    pub dependency_group: String,
    pub op_type: String,
    pub entity_id: Option<String>,
    pub payload_json: String,
    pub base_revision: Option<Revision>,
    pub server_revision: Option<Revision>,
    pub error_code: String,
    pub error_message: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEqualizerOp {
    pub id: i64,
    pub operation_uuid: String,
    pub account_scope: String,
    pub op_type: String,
    pub entity_id: Option<String>,
    pub base_revision: Option<Revision>,
    pub dependency_group: String,
    pub payload_json: String,
    pub created_at: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EqualizerValidationError {
    pub message: String,
}

impl EqualizerValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub fn validate_profile(profile: &EqualizerProfile) -> Result<(), EqualizerValidationError> {
    validate_name(&profile.name, "profile name")?;
    if uuid::Uuid::parse_str(&profile.id).is_err() {
        return Err(EqualizerValidationError::new("profile id must be a UUID"));
    }
    if profile.format_version != EQ_PROFILE_FORMAT_VERSION {
        return Err(EqualizerValidationError::new(format!(
            "unsupported profile format version {}",
            profile.format_version
        )));
    }
    finite_range(profile.preamp_db, -30.0, 12.0, "preamp")?;
    if profile.bands.is_empty() || profile.bands.len() > MAX_BANDS {
        return Err(EqualizerValidationError::new(format!(
            "profile must contain 1 to {MAX_BANDS} bands"
        )));
    }
    for (index, band) in profile.bands.iter().enumerate() {
        let expected = (index + 1) as u32;
        if band.position != expected {
            return Err(EqualizerValidationError::new(format!(
                "band positions must be contiguous from 1 (expected {expected}, got {})",
                band.position
            )));
        }
        if band.filter_kind != FilterKind::Peaking {
            return Err(EqualizerValidationError::new(
                "only peaking filters are supported",
            ));
        }
        finite_range(band.frequency_hz, 10.0, 20_000.0, "frequency")?;
        finite_range(band.gain_db, -24.0, 24.0, "gain")?;
        finite_range(band.q, 0.1, 30.0, "Q")?;
    }
    Ok(())
}

pub fn validate_rule(rule: &EqualizerDeviceRule) -> Result<(), EqualizerValidationError> {
    validate_name(&rule.label, "rule label")?;
    if uuid::Uuid::parse_str(&rule.id).is_err() {
        return Err(EqualizerValidationError::new("rule id must be a UUID"));
    }
    if rule.selectors.is_empty() || rule.selectors.len() > MAX_SELECTORS_PER_RULE {
        return Err(EqualizerValidationError::new(format!(
            "rule must contain 1 to {MAX_SELECTORS_PER_RULE} selectors"
        )));
    }
    for selector in &rule.selectors {
        if selector.normalization_version != EQ_NORMALIZATION_VERSION {
            return Err(EqualizerValidationError::new(
                "unsupported selector normalization version",
            ));
        }
        validate_name(&selector.normalized_name, "device matcher")?;
        if normalize_matcher(&selector.normalized_name) != selector.normalized_name {
            return Err(EqualizerValidationError::new(
                "device matcher is not canonically normalized",
            ));
        }
    }
    Ok(())
}

pub fn validate_name(value: &str, field: &str) -> Result<(), EqualizerValidationError> {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    if count == 0 || count > MAX_NAME_CHARS {
        return Err(EqualizerValidationError::new(format!(
            "{field} must contain 1 to {MAX_NAME_CHARS} characters"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(EqualizerValidationError::new(format!(
            "{field} contains a control character"
        )));
    }
    Ok(())
}

fn finite_range(
    value: f64,
    min: f64,
    max: f64,
    field: &str,
) -> Result<(), EqualizerValidationError> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(EqualizerValidationError::new(format!(
            "{field} must be finite and between {min} and {max}"
        )));
    }
    Ok(())
}

/// NFKC + full default (non-Turkic) case folding + whitespace collapse.
pub fn normalize_matcher(value: &str) -> String {
    let normalized: String = value.nfkc().collect();
    let folded: String = normalized.as_str().case_fold().collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn name_key(value: &str) -> String {
    normalize_matcher(value.trim())
}

pub fn normalize_hardware_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_start_matches("0x").to_ascii_lowercase())
}

pub fn now_string() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_a_json_string() {
        let json = serde_json::to_string(&Revision(i64::MAX)).unwrap();
        assert_eq!(json, format!("\"{}\"", i64::MAX));
        assert_eq!(
            serde_json::from_str::<Revision>(&json).unwrap(),
            Revision(i64::MAX)
        );
    }

    #[cfg(any())]
    fn normalization_is_nfkc_full_casefold_and_space_collapsed() {
        assert_eq!(normalize_matcher("  STRAÃŸE\tï¼¡  "), "strasse a");
    }

    #[test]
    fn normalization_handles_casefold_and_compatibility_characters() {
        assert_eq!(
            normalize_matcher("  STRA\u{00df}E\t\u{ff21}  "),
            "strasse a"
        );
    }

    #[test]
    fn starter_has_five_unity_bands() {
        let profile = EqualizerProfile::five_band_starter("Flat five");
        assert_eq!(profile.bands.len(), 5);
        assert!(profile.bands.iter().all(|band| band.gain_db == 0.0));
        validate_profile(&profile).unwrap();
    }

    #[test]
    fn rejects_non_contiguous_bands() {
        let mut profile = EqualizerProfile::five_band_starter("Bad");
        profile.bands[2].position = 9;
        assert!(validate_profile(&profile).is_err());
    }

    #[test]
    fn audio_output_never_serializes_or_accepts_native_keys() {
        let output = AudioOutput {
            runtime_id: Some("native-7".into()),
            local_endpoint_key: Some("hmac".into()),
            display_name: "Headphones".into(),
            route_kind: RouteKind::Bluetooth,
            vendor_id: None,
            product_id: None,
            connected: true,
            selected: true,
            accuracy: RouteAccuracy::Predicted,
            binding_stability: BindingStability::SessionOnly,
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(!json.contains("native-7"));
        assert!(!json.contains("hmac"));
        let injected = json.trim_end_matches('}').to_string()
            + ",\"runtime_id\":\"evil\",\"local_endpoint_key\":\"evil\"}";
        let decoded: AudioOutput = serde_json::from_str(&injected).unwrap();
        assert!(decoded.runtime_id.is_none());
        assert!(decoded.local_endpoint_key.is_none());
    }
}
