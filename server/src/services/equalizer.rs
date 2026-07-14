//! Cross-device equalizer configuration service.
//!
//! The server validates and synchronizes configuration only. Audio DSP stays
//! client-side. All owner-scoped operations derive the owner from a bearer
//! identity; `SECRET_KEY` is accepted only by the explicitly administrative
//! audit/detail/rollback methods.

use std::collections::HashSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    AuditEntry, DeleteEqualizerProfile, EntityRevision, EqualizerAuditSnapshot, EqualizerBand,
    EqualizerChangeDetail, EqualizerChangeSummary, EqualizerDeviceRuleDraft,
    EqualizerMutationOutcome, EqualizerProfileDraft, EqualizerRollbackOutcome, EqualizerRuleAction,
    EqualizerState, PermissionLevel, PortableDeviceSelector, ProfileDeleteDisposition,
};
use crate::db::repo::{AuditRepo, EqualizerRepo};
use crate::equalizer_core::{
    apply_rollback_inverse, normalize_device_name, profile_name_key, rollback_after_image_matches,
    rollback_diff,
};
use crate::error::{AppError, Result};

pub const STATE_FORMAT_VERSION: i32 = 1;
pub const PROFILE_FORMAT_VERSION: i32 = 1;
pub const SELECTOR_NORMALIZATION_VERSION: i32 = 1;
pub const MAX_PROFILES: usize = 64;
pub const MAX_RULES: usize = 64;
pub const MAX_BANDS: usize = 32;
pub const MAX_SELECTORS: usize = 8;
const MAX_LABEL_SCALARS: usize = 100;
const MAX_MATCHER_SCALARS: usize = 160;
const MAX_SELECTOR_JSON_BYTES: usize = 8 * 1024;

const ROUTE_KINDS: [&str; 7] = [
    "bluetooth",
    "wired",
    "usb",
    "hdmi",
    "airplay",
    "builtin",
    "unknown",
];
const PLATFORMS: [&str; 5] = ["windows", "android", "macos", "linux", "ios"];
const TRIGGERS: [&str; 2] = ["active_output", "connected"];

#[derive(Debug, Clone)]
pub struct EqualizerProfileInput {
    pub id: Uuid,
    pub name: String,
    pub format_version: i32,
    pub preamp_db: f64,
    pub auto_headroom_enabled: bool,
    pub bands: Vec<EqualizerBand>,
}

#[derive(Debug, Clone)]
pub struct EqualizerDeviceRuleInput {
    pub id: Uuid,
    pub label: String,
    pub action: EqualizerRuleAction,
    pub selectors: Vec<PortableDeviceSelector>,
    pub enabled: bool,
    pub bass_boost_percent: i32,
    pub treble_boost_percent: i32,
}

#[derive(Debug, Clone)]
pub struct GetEqualizerStateOutcome {
    pub not_modified: bool,
    pub state: Option<EqualizerState>,
}

#[derive(Debug, Clone)]
pub struct EqualizerChangesPage {
    pub changes: Vec<EqualizerChangeSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone)]
pub struct EqualizerService {
    equalizer: Arc<dyn EqualizerRepo>,
    audit: Arc<dyn AuditRepo>,
}

impl EqualizerService {
    pub fn new(equalizer: Arc<dyn EqualizerRepo>, audit: Arc<dyn AuditRepo>) -> Self {
        Self { equalizer, audit }
    }

    pub async fn get_state(
        &self,
        caller: &Identity,
        known_state_revision: Option<i64>,
    ) -> Result<GetEqualizerStateOutcome> {
        let owner_id = owner(caller)?;
        let state = self.equalizer.get_equalizer_state(owner_id).await?;
        if known_state_revision == Some(state.state_revision) {
            Ok(GetEqualizerStateOutcome {
                not_modified: true,
                state: None,
            })
        } else {
            Ok(GetEqualizerStateOutcome {
                not_modified: false,
                state: Some(state),
            })
        }
    }

    pub async fn create_profile(
        &self,
        caller: &Identity,
        input: EqualizerProfileInput,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        let draft = validate_profile(input)?;
        self.equalizer
            .create_equalizer_profile(caller.user_id(), owner_id, draft)
            .await
    }

    pub async fn update_profile(
        &self,
        caller: &Identity,
        profile_id: Uuid,
        expected_revision: i64,
        input: EqualizerProfileInput,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        if expected_revision < 1 {
            return Err(AppError::InvalidArgument(
                "expected profile revision must be at least 1".into(),
            ));
        }
        if input.id != profile_id {
            return Err(AppError::InvalidArgument(
                "profile id in body must match target id".into(),
            ));
        }
        let draft = validate_profile(input)?;
        self.equalizer
            .update_equalizer_profile(
                caller.user_id(),
                owner_id,
                profile_id,
                expected_revision,
                draft,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn delete_profile(
        &self,
        caller: &Identity,
        profile_id: Uuid,
        expected_revision: i64,
        expected_settings_revision: i64,
        referencing_rules: Vec<EntityRevision>,
        disposition: ProfileDeleteDisposition,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        if expected_revision < 1 || expected_settings_revision < 0 {
            return Err(AppError::InvalidArgument(
                "invalid equalizer deletion revision".into(),
            ));
        }
        let mut ids = HashSet::new();
        if referencing_rules
            .iter()
            .any(|r| r.expected_revision < 1 || !ids.insert(r.id))
        {
            return Err(AppError::InvalidArgument(
                "referencing rules must be unique with positive revisions".into(),
            ));
        }
        if matches!(
            disposition,
            ProfileDeleteDisposition::ReplaceWithProfile { profile_id: id } if id == profile_id
        ) {
            return Err(AppError::InvalidArgument(
                "replacement profile must differ from deleted profile".into(),
            ));
        }
        self.equalizer
            .delete_equalizer_profile(
                caller.user_id(),
                owner_id,
                DeleteEqualizerProfile {
                    profile_id,
                    expected_revision,
                    expected_settings_revision,
                    referencing_rules,
                    disposition,
                },
            )
            .await
    }

    pub async fn update_settings(
        &self,
        caller: &Identity,
        expected_settings_revision: i64,
        default_profile_id: Option<Uuid>,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        if expected_settings_revision < 0 {
            return Err(AppError::InvalidArgument(
                "expected settings revision cannot be negative".into(),
            ));
        }
        self.equalizer
            .update_equalizer_settings(
                caller.user_id(),
                owner_id,
                expected_settings_revision,
                default_profile_id,
            )
            .await
    }

    pub async fn create_rule(
        &self,
        caller: &Identity,
        input: EqualizerDeviceRuleInput,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        let draft = validate_rule(input)?;
        self.equalizer
            .create_equalizer_rule(caller.user_id(), owner_id, draft)
            .await
    }

    pub async fn update_rule(
        &self,
        caller: &Identity,
        rule_id: Uuid,
        expected_revision: i64,
        input: EqualizerDeviceRuleInput,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        if expected_revision < 1 {
            return Err(AppError::InvalidArgument(
                "expected rule revision must be at least 1".into(),
            ));
        }
        if input.id != rule_id {
            return Err(AppError::InvalidArgument(
                "rule id in body must match target id".into(),
            ));
        }
        let draft = validate_rule(input)?;
        self.equalizer
            .update_equalizer_rule(
                caller.user_id(),
                owner_id,
                rule_id,
                expected_revision,
                draft,
            )
            .await
    }

    pub async fn delete_rule(
        &self,
        caller: &Identity,
        rule_id: Uuid,
        expected_revision: i64,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        if expected_revision < 1 {
            return Err(AppError::InvalidArgument(
                "expected rule revision must be at least 1".into(),
            ));
        }
        self.equalizer
            .delete_equalizer_rule(caller.user_id(), owner_id, rule_id, expected_revision)
            .await
    }

    pub async fn reorder_rules(
        &self,
        caller: &Identity,
        rules: Vec<EntityRevision>,
    ) -> Result<EqualizerMutationOutcome> {
        let owner_id = owner(caller)?;
        if rules.len() > MAX_RULES {
            return Err(AppError::InvalidArgument(format!(
                "at most {MAX_RULES} rules may be reordered"
            )));
        }
        let mut ids = HashSet::new();
        if rules
            .iter()
            .any(|r| r.expected_revision < 1 || !ids.insert(r.id))
        {
            return Err(AppError::InvalidArgument(
                "rule order must contain unique ids with positive revisions".into(),
            ));
        }
        self.equalizer
            .reorder_equalizer_rules(caller.user_id(), owner_id, rules)
            .await
    }

    pub async fn list_changes(
        &self,
        caller: &Identity,
        subject_user_id: Option<Uuid>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<EqualizerChangesPage> {
        caller.require(PermissionLevel::Manager)?;
        let limit = limit.unwrap_or(50).clamp(1, 100) as usize;
        let (before_created, before_id) = match cursor {
            Some(value) => {
                let (time, id) = decode_cursor(value)?;
                (Some(time), Some(id))
            }
            None => (None, None),
        };
        let mut rows = self
            .audit
            .list_equalizer_changes(
                subject_user_id,
                before_created,
                before_id,
                (limit + 1) as i64,
            )
            .await?;
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if has_more {
            rows.last().map(|e| encode_cursor(e.created_at, e.id))
        } else {
            None
        };
        let changes = rows
            .into_iter()
            .map(|entry| summary_from_entry(&entry))
            .collect::<Result<Vec<_>>>()?;
        Ok(EqualizerChangesPage {
            changes,
            next_cursor,
        })
    }

    pub async fn get_change(
        &self,
        caller: &Identity,
        audit_id: Uuid,
    ) -> Result<EqualizerChangeDetail> {
        caller.require(PermissionLevel::Manager)?;
        let entry = self
            .audit
            .get_by_id(audit_id)
            .await?
            .filter(|e| e.entity_type == "equalizer_state" && e.action.starts_with("equalizer."))
            .ok_or_else(|| AppError::NotFound("equalizer audit change not found".into()))?;
        let before = parse_snapshot(entry.before_json.as_deref())?;
        let after = parse_snapshot(entry.after_json.as_deref())?;
        let owner_id = entry
            .entity_id
            .ok_or_else(|| AppError::Internal("equalizer audit row has no owner".into()))?;
        let current = match self.equalizer.get_equalizer_state(owner_id).await {
            Ok(state) => Some(state),
            Err(AppError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let diff = rollback_diff(&before.state, &after.state);
        let prospective = current
            .as_ref()
            .map(|state| apply_rollback_inverse(state, &before.state, &diff));
        let structurally_eligible =
            current
                .as_ref()
                .zip(prospective.as_ref())
                .is_some_and(|(state, target)| {
                    rollback_after_image_matches(state, &after.state, &diff)
                        && prospective_rollback_is_valid(target)
                })
                && before.snapshot_format_version == 1
                && after.snapshot_format_version == 1;
        let rollback_eligible = if structurally_eligible {
            let target = prospective.as_ref().expect("eligible target exists");
            let profile_ids = target
                .profiles
                .iter()
                .map(|profile| profile.id)
                .collect::<Vec<_>>();
            let rule_ids = target
                .device_rules
                .iter()
                .map(|rule| rule.id)
                .collect::<Vec<_>>();
            self.equalizer
                .equalizer_ids_available_for_owner(owner_id, &profile_ids, &rule_ids)
                .await?
        } else {
            false
        };
        let is_admin = caller.level().satisfies(PermissionLevel::Admin);
        Ok(EqualizerChangeDetail {
            change: summary_from_entry(&entry)?,
            before_json: is_admin.then(|| entry.before_json.clone()).flatten(),
            after_json: is_admin.then(|| entry.after_json.clone()).flatten(),
            current_state_revision: current.as_ref().map(|s| s.state_revision),
            rollback_eligible,
        })
    }

    pub async fn rollback_change(
        &self,
        caller: &Identity,
        audit_id: Uuid,
        expected_state_revision: i64,
    ) -> Result<EqualizerRollbackOutcome> {
        caller.require(PermissionLevel::Admin)?;
        if expected_state_revision < 0 {
            return Err(AppError::InvalidArgument(
                "expected state revision cannot be negative".into(),
            ));
        }
        self.equalizer
            .rollback_equalizer_change(caller.user_id(), audit_id, expected_state_revision)
            .await
    }
}

fn prospective_rollback_is_valid(state: &EqualizerState) -> bool {
    if state.profiles.len() > MAX_PROFILES || state.device_rules.len() > MAX_RULES {
        return false;
    }
    let profile_ids = state
        .profiles
        .iter()
        .map(|profile| profile.id)
        .collect::<HashSet<_>>();
    if profile_ids.len() != state.profiles.len()
        || state
            .profiles
            .iter()
            .map(|profile| profile_name_key(&profile.name))
            .collect::<HashSet<_>>()
            .len()
            != state.profiles.len()
        || state
            .default_profile_id
            .is_some_and(|id| !profile_ids.contains(&id))
    {
        return false;
    }
    let mut rule_ids = HashSet::new();
    let mut selector_hashes = HashSet::new();
    state.device_rules.iter().all(|rule| {
        let reference_valid = match &rule.action {
            EqualizerRuleAction::Profile { profile_id } => profile_ids.contains(profile_id),
            EqualizerRuleAction::Bypass => true,
        };
        let selector_json = serde_json::to_vec(&rule.selectors).ok();
        let selector_hash = selector_json.map(|json| Sha256::digest(json).to_vec());
        reference_valid
            && rule_ids.insert(rule.id)
            && selector_hash.is_some_and(|hash| selector_hashes.insert(hash))
    })
}

fn owner(caller: &Identity) -> Result<Uuid> {
    caller.require(PermissionLevel::User)?;
    caller.user_id().ok_or_else(|| {
        AppError::InvalidArgument(
            "SECRET_KEY has no user to own synchronized equalizer state".into(),
        )
    })
}

fn validate_profile(input: EqualizerProfileInput) -> Result<EqualizerProfileDraft> {
    if input.id.is_nil() {
        return Err(AppError::InvalidArgument(
            "equalizer profile id cannot be nil".into(),
        ));
    }
    let name = validate_visible(&input.name, "profile name", MAX_LABEL_SCALARS)?;
    if input.format_version != PROFILE_FORMAT_VERSION {
        return Err(AppError::InvalidArgument(format!(
            "profile format_version must be {PROFILE_FORMAT_VERSION}"
        )));
    }
    finite_range(input.preamp_db, -30.0, 12.0, "preamp_db")?;
    if input.bands.is_empty() || input.bands.len() > MAX_BANDS {
        return Err(AppError::InvalidArgument(format!(
            "profile must contain 1..={MAX_BANDS} bands"
        )));
    }
    let mut bands = input.bands;
    bands.sort_by_key(|b| b.position);
    for (index, band) in bands.iter().enumerate() {
        if band.position != (index + 1) as i32 {
            return Err(AppError::InvalidArgument(
                "band positions must be contiguous and 1-based".into(),
            ));
        }
        if band.filter_type != "peaking" {
            return Err(AppError::InvalidArgument(
                "version 1 supports only peaking filters".into(),
            ));
        }
        finite_range(band.frequency_hz, 10.0, 20_000.0, "frequency_hz")?;
        finite_range(band.gain_db, -24.0, 24.0, "gain_db")?;
        finite_range(band.q, 0.1, 30.0, "q")?;
    }
    Ok(EqualizerProfileDraft {
        id: input.id,
        name_key: profile_name_key(&name),
        name,
        format_version: input.format_version,
        preamp_db: input.preamp_db,
        auto_headroom_enabled: input.auto_headroom_enabled,
        bands,
    })
}

fn validate_rule(input: EqualizerDeviceRuleInput) -> Result<EqualizerDeviceRuleDraft> {
    if input.id.is_nil() {
        return Err(AppError::InvalidArgument(
            "equalizer device rule id cannot be nil".into(),
        ));
    }
    let label = validate_visible(&input.label, "rule label", MAX_LABEL_SCALARS)?;
    if !(0..=100).contains(&input.bass_boost_percent)
        || !(0..=100).contains(&input.treble_boost_percent)
    {
        return Err(AppError::InvalidArgument(
            "rule bass and treble boost percentages must be between 0 and 100".into(),
        ));
    }
    if input.selectors.is_empty() || input.selectors.len() > MAX_SELECTORS {
        return Err(AppError::InvalidArgument(format!(
            "rule must contain 1..={MAX_SELECTORS} selectors"
        )));
    }
    let mut selectors = Vec::with_capacity(input.selectors.len());
    for mut selector in input.selectors {
        if selector.normalization_version != SELECTOR_NORMALIZATION_VERSION {
            return Err(AppError::InvalidArgument(format!(
                "selector normalization_version must be {SELECTOR_NORMALIZATION_VERSION}"
            )));
        }
        selector.route_kind = selector.route_kind.trim().to_ascii_lowercase();
        if !ROUTE_KINDS.contains(&selector.route_kind.as_str()) {
            return Err(AppError::InvalidArgument(
                "invalid selector route_kind".into(),
            ));
        }
        if contains_forbidden_text(&selector.normalized_name) {
            return Err(AppError::InvalidArgument(
                "device matcher contains a control character".into(),
            ));
        }
        selector.normalized_name = normalize_device_name(&selector.normalized_name);
        let matcher_len = selector.normalized_name.chars().count();
        if matcher_len == 0 || matcher_len > MAX_MATCHER_SCALARS {
            return Err(AppError::InvalidArgument(format!(
                "normalized device matcher must contain 1..={MAX_MATCHER_SCALARS} characters"
            )));
        }
        if looks_like_private_identifier(&selector.normalized_name) {
            return Err(AppError::InvalidArgument(
                "raw hardware or endpoint identifiers cannot be synchronized".into(),
            ));
        }
        selector.vendor_id = validate_token(selector.vendor_id, "vendor_id")?;
        selector.product_id = validate_token(selector.product_id, "product_id")?;
        selector.platform_scope = match selector.platform_scope {
            Some(value) => {
                let value = value.trim().to_ascii_lowercase();
                if !PLATFORMS.contains(&value.as_str()) {
                    return Err(AppError::InvalidArgument(
                        "invalid selector platform_scope".into(),
                    ));
                }
                Some(value)
            }
            None => None,
        };
        selector.trigger = selector.trigger.trim().to_ascii_lowercase();
        if !TRIGGERS.contains(&selector.trigger.as_str()) {
            return Err(AppError::InvalidArgument(
                "selector trigger must be active_output or connected".into(),
            ));
        }
        selectors.push(selector);
    }
    selectors.sort_by(|a, b| selector_key(a).cmp(&selector_key(b)));
    if selectors.windows(2).any(|w| w[0] == w[1]) {
        return Err(AppError::InvalidArgument(
            "duplicate selectors are not allowed within one rule".into(),
        ));
    }
    let selector_json = serde_json::to_string(&selectors)
        .map_err(|e| AppError::Internal(format!("equalizer selector JSON: {e}")))?;
    if selector_json.len() > MAX_SELECTOR_JSON_BYTES {
        return Err(AppError::InvalidArgument(format!(
            "canonical selector JSON exceeds {MAX_SELECTOR_JSON_BYTES} bytes"
        )));
    }
    let selector_hash = format!("{:x}", Sha256::digest(selector_json.as_bytes()));
    Ok(EqualizerDeviceRuleDraft {
        id: input.id,
        label,
        action: input.action,
        selectors,
        selector_json,
        selector_hash,
        enabled: input.enabled,
        bass_boost_percent: input.bass_boost_percent,
        treble_boost_percent: input.treble_boost_percent,
    })
}

fn selector_key(
    selector: &PortableDeviceSelector,
) -> (
    i32,
    &str,
    &str,
    Option<&str>,
    Option<&str>,
    Option<&str>,
    &str,
) {
    (
        selector.normalization_version,
        selector.route_kind.as_str(),
        selector.normalized_name.as_str(),
        selector.vendor_id.as_deref(),
        selector.product_id.as_deref(),
        selector.platform_scope.as_deref(),
        selector.trigger.as_str(),
    )
}

fn validate_visible(value: &str, field: &str, max: usize) -> Result<String> {
    let value = value.trim();
    let count = value.chars().count();
    if count == 0 || count > max {
        return Err(AppError::InvalidArgument(format!(
            "{field} must contain 1..={max} characters"
        )));
    }
    if contains_forbidden_text(value) {
        return Err(AppError::InvalidArgument(format!(
            "{field} contains a control character"
        )));
    }
    Ok(value.to_string())
}

fn contains_forbidden_text(value: &str) -> bool {
    value.chars().any(|c| c == '\0' || c.is_control())
}

fn finite_range(value: f64, min: f64, max: f64, field: &str) -> Result<()> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(AppError::InvalidArgument(format!(
            "{field} must be finite and between {min} and {max}"
        )));
    }
    Ok(())
}

fn validate_token(value: Option<String>, field: &str) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim().to_string();
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(AppError::InvalidArgument(format!(
            "{field} must be 1..=32 lowercase ASCII [a-z0-9._-] characters"
        )));
    }
    Ok(Some(value))
}

fn looks_like_private_identifier(value: &str) -> bool {
    if Uuid::parse_str(value).is_ok() || value.starts_with("\\\\?\\") {
        return true;
    }
    for separator in [':', '-'] {
        let parts: Vec<&str> = value.split(separator).collect();
        if parts.len() == 6
            && parts
                .iter()
                .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return true;
        }
    }
    false
}

fn parse_snapshot(json: Option<&str>) -> Result<EqualizerAuditSnapshot> {
    let value = json.ok_or_else(|| {
        AppError::InvalidArgument("equalizer audit entry has no reversible snapshot".into())
    })?;
    serde_json::from_str(value)
        .map_err(|e| AppError::InvalidArgument(format!("unsupported audit snapshot: {e}")))
}

fn summary_from_entry(entry: &AuditEntry) -> Result<EqualizerChangeSummary> {
    let before = parse_snapshot(entry.before_json.as_deref())?;
    let after = parse_snapshot(entry.after_json.as_deref())?;
    let owner_id = entry
        .entity_id
        .ok_or_else(|| AppError::Internal("equalizer audit row has no owner".into()))?;
    Ok(EqualizerChangeSummary {
        audit_id: entry.id,
        action: entry.action.clone(),
        actor_id: entry.actor_id,
        owner_id,
        resource_type: after.resource_type,
        resource_id: after.resource_id,
        created_at: entry.created_at,
        before_state_revision: before.state.state_revision,
        after_state_revision: after.state.state_revision,
    })
}

fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> String {
    format!(
        "eq1.{:032x}.{}",
        created_at.unix_timestamp_nanos(),
        id.simple()
    )
}

fn decode_cursor(value: &str) -> Result<(OffsetDateTime, Uuid)> {
    let mut parts = value.split('.');
    if parts.next() != Some("eq1") {
        return Err(AppError::InvalidArgument("invalid equalizer cursor".into()));
    }
    let nanos = parts
        .next()
        .and_then(|v| i128::from_str_radix(v, 16).ok())
        .ok_or_else(|| AppError::InvalidArgument("invalid equalizer cursor".into()))?;
    let id = parts
        .next()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| AppError::InvalidArgument("invalid equalizer cursor".into()))?;
    if parts.next().is_some() {
        return Err(AppError::InvalidArgument("invalid equalizer cursor".into()));
    }
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| AppError::InvalidArgument("invalid equalizer cursor".into()))?;
    Ok((created_at, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(position: i32) -> EqualizerBand {
        EqualizerBand {
            position,
            enabled: true,
            filter_type: "peaking".into(),
            frequency_hz: 1_000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }

    fn profile() -> EqualizerProfileInput {
        EqualizerProfileInput {
            id: Uuid::new_v4(),
            name: "  Straße  ".into(),
            format_version: 1,
            preamp_db: -1.0,
            auto_headroom_enabled: true,
            bands: vec![band(2), band(1)],
        }
    }

    fn selector(name: &str) -> PortableDeviceSelector {
        PortableDeviceSelector {
            normalization_version: 1,
            route_kind: "BLUETOOTH".into(),
            normalized_name: name.into(),
            vendor_id: Some("054c".into()),
            product_id: Some("0be3".into()),
            platform_scope: Some("ANDROID".into()),
            trigger: "ACTIVE_OUTPUT".into(),
        }
    }

    #[test]
    fn profile_is_trimmed_case_keyed_and_band_sorted() {
        let draft = validate_profile(profile()).unwrap();
        assert_eq!(draft.name, "Straße");
        assert_eq!(draft.name_key, "strasse");
        assert_eq!(draft.bands[0].position, 1);
        assert_eq!(draft.bands[1].position, 2);
    }

    #[test]
    fn non_finite_and_out_of_range_values_fail() {
        let mut p = profile();
        p.preamp_db = f64::NAN;
        assert!(validate_profile(p).is_err());
        let mut p = profile();
        p.bands[0].q = f64::INFINITY;
        assert!(validate_profile(p).is_err());
    }

    #[test]
    fn positions_must_be_contiguous() {
        let mut p = profile();
        p.bands = vec![band(1), band(3)];
        assert!(validate_profile(p).is_err());
    }

    #[test]
    fn unsupported_filter_and_format_fail_closed() {
        let mut p = profile();
        p.format_version = 2;
        assert!(validate_profile(p).is_err());
        let mut p = profile();
        p.bands[0].filter_type = "lowshelf".into();
        assert!(validate_profile(p).is_err());
    }

    #[test]
    fn visible_text_rejects_control_characters() {
        let mut p = profile();
        p.name = "bad\nname".into();
        assert!(validate_profile(p).is_err());
    }

    #[test]
    fn rule_canonicalizes_and_sorts_selectors() {
        let rule = EqualizerDeviceRuleInput {
            id: Uuid::new_v4(),
            label: "  Headphones ".into(),
            action: EqualizerRuleAction::Bypass,
            selectors: vec![selector("  ＳＯＮＹ\u{2003}XM5 ")],
            enabled: true,
            bass_boost_percent: 25,
            treble_boost_percent: 50,
        };
        let draft = validate_rule(rule).unwrap();
        assert_eq!(draft.label, "Headphones");
        assert_eq!(draft.selectors[0].normalized_name, "sony xm5");
        assert_eq!(draft.selectors[0].route_kind, "bluetooth");
        assert_eq!(draft.selector_hash.len(), 64);
        assert_eq!(draft.bass_boost_percent, 25);
        assert_eq!(draft.treble_boost_percent, 50);
    }

    #[test]
    fn rule_tone_percentages_are_bounded() {
        let mut rule = EqualizerDeviceRuleInput {
            id: Uuid::new_v4(),
            label: "Headphones".into(),
            action: EqualizerRuleAction::Bypass,
            selectors: vec![selector("headphones")],
            enabled: true,
            bass_boost_percent: 101,
            treble_boost_percent: 0,
        };
        assert!(validate_rule(rule.clone()).is_err());
        rule.bass_boost_percent = 0;
        rule.treble_boost_percent = -1;
        assert!(validate_rule(rule).is_err());
    }

    #[test]
    fn raw_mac_and_uuid_matchers_are_rejected() {
        for value in ["aa:bb:cc:dd:ee:ff", "550e8400-e29b-41d4-a716-446655440000"] {
            let rule = EqualizerDeviceRuleInput {
                id: Uuid::new_v4(),
                label: "x".into(),
                action: EqualizerRuleAction::Bypass,
                selectors: vec![selector(value)],
                enabled: true,
                bass_boost_percent: 0,
                treble_boost_percent: 0,
            };
            assert!(validate_rule(rule).is_err());
        }
    }

    #[test]
    fn duplicate_selectors_and_uppercase_tokens_fail() {
        let duplicate = selector("Sony XM5");
        let rule = EqualizerDeviceRuleInput {
            id: Uuid::new_v4(),
            label: "x".into(),
            action: EqualizerRuleAction::Bypass,
            selectors: vec![duplicate.clone(), duplicate],
            enabled: true,
            bass_boost_percent: 0,
            treble_boost_percent: 0,
        };
        assert!(validate_rule(rule).is_err());

        let mut bad = selector("Sony XM5");
        bad.vendor_id = Some("ABCD".into());
        let rule = EqualizerDeviceRuleInput {
            id: Uuid::new_v4(),
            label: "x".into(),
            action: EqualizerRuleAction::Bypass,
            selectors: vec![bad],
            enabled: true,
            bass_boost_percent: 0,
            treble_boost_percent: 0,
        };
        assert!(validate_rule(rule).is_err());
    }

    #[test]
    fn cursor_round_trips() {
        let now = OffsetDateTime::now_utc();
        let id = Uuid::new_v4();
        let (decoded_time, decoded_id) = decode_cursor(&encode_cursor(now, id)).unwrap();
        assert_eq!(decoded_time, now);
        assert_eq!(decoded_id, id);
        assert!(decode_cursor("not-a-cursor").is_err());
    }

    #[test]
    fn secret_key_has_no_synced_owner_but_is_still_admin() {
        assert!(matches!(
            owner(&Identity::SecretKey),
            Err(AppError::InvalidArgument(_))
        ));
        assert!(Identity::SecretKey.require(PermissionLevel::Admin).is_ok());
    }
}
