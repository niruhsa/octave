//! Typed payloads for the account-scoped equalizer mutation outbox.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::model::{
    now_string, DeleteProfileDisposition, DeleteProfileRequest, EntityRevision,
    EqualizerDeviceRule, EqualizerDeviceRuleInput, EqualizerProfile, EqualizerProfileInput,
    EqualizerState, Revision, RuleAction,
};
use crate::error::{AppError, AppResult};

pub mod op_type {
    pub const PROFILE_CREATE: &str = "equalizer.profile.create";
    pub const PROFILE_UPDATE: &str = "equalizer.profile.update";
    pub const PROFILE_DELETE: &str = "equalizer.profile.delete";
    pub const SETTINGS_UPDATE: &str = "equalizer.settings.update";
    pub const RULE_CREATE: &str = "equalizer.rule.create";
    pub const RULE_UPDATE: &str = "equalizer.rule.update";
    pub const RULE_DELETE: &str = "equalizer.rule.delete";
    pub const RULE_REORDER: &str = "equalizer.rule.reorder";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingEqualizerOpKind {
    ProfileCreate {
        profile: EqualizerProfileInput,
    },
    ProfileUpdate {
        profile_id: String,
        expected_revision: Revision,
        profile: EqualizerProfileInput,
    },
    ProfileDelete {
        request: DeleteProfileRequest,
    },
    SettingsUpdate {
        expected_settings_revision: Revision,
        default_profile_id: Option<String>,
    },
    RuleCreate {
        rule: EqualizerDeviceRuleInput,
    },
    RuleUpdate {
        rule_id: String,
        expected_revision: Revision,
        rule: EqualizerDeviceRuleInput,
    },
    RuleDelete {
        rule_id: String,
        expected_revision: Revision,
    },
    RuleReorder {
        rules: Vec<EntityRevision>,
    },
}

impl PendingEqualizerOpKind {
    pub fn op_type(&self) -> &'static str {
        match self {
            Self::ProfileCreate { .. } => op_type::PROFILE_CREATE,
            Self::ProfileUpdate { .. } => op_type::PROFILE_UPDATE,
            Self::ProfileDelete { .. } => op_type::PROFILE_DELETE,
            Self::SettingsUpdate { .. } => op_type::SETTINGS_UPDATE,
            Self::RuleCreate { .. } => op_type::RULE_CREATE,
            Self::RuleUpdate { .. } => op_type::RULE_UPDATE,
            Self::RuleDelete { .. } => op_type::RULE_DELETE,
            Self::RuleReorder { .. } => op_type::RULE_REORDER,
        }
    }

    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::ProfileCreate { profile } => Some(&profile.id),
            Self::ProfileUpdate { profile_id, .. } => Some(profile_id),
            Self::ProfileDelete { request } => Some(&request.profile_id),
            Self::SettingsUpdate { .. } | Self::RuleReorder { .. } => None,
            Self::RuleCreate { rule } => Some(&rule.id),
            Self::RuleUpdate { rule_id, .. } | Self::RuleDelete { rule_id, .. } => Some(rule_id),
        }
    }

    pub fn base_revision(&self) -> Option<Revision> {
        match self {
            Self::ProfileUpdate {
                expected_revision, ..
            }
            | Self::RuleUpdate {
                expected_revision, ..
            }
            | Self::RuleDelete {
                expected_revision, ..
            } => Some(*expected_revision),
            Self::ProfileDelete { request } => Some(request.expected_revision),
            Self::SettingsUpdate {
                expected_settings_revision,
                ..
            } => Some(*expected_settings_revision),
            Self::ProfileCreate { .. } | Self::RuleCreate { .. } | Self::RuleReorder { .. } => None,
        }
    }

    pub fn dependency_group(&self) -> String {
        match self {
            Self::ProfileCreate { profile } => format!("profile:{}", profile.id),
            Self::ProfileUpdate { profile_id, .. } => format!("profile:{profile_id}"),
            Self::ProfileDelete { request } => format!("profile:{}", request.profile_id),
            Self::SettingsUpdate {
                default_profile_id: Some(id),
                ..
            } => format!("profile:{id}"),
            Self::SettingsUpdate { .. } => "settings".to_string(),
            Self::RuleCreate { rule } => rule
                .action
                .profile_id()
                .map_or_else(|| format!("rule:{}", rule.id), |id| format!("profile:{id}")),
            Self::RuleUpdate { rule_id, rule, .. } => rule
                .action
                .profile_id()
                .map_or_else(|| format!("rule:{rule_id}"), |id| format!("profile:{id}")),
            Self::RuleDelete { rule_id, .. } => format!("rule:{rule_id}"),
            Self::RuleReorder { .. } => "rule-order".to_string(),
        }
    }

    /// Entity edges used to build a transitive FIFO/conflict component. A
    /// single operation can depend on both its own entity and referenced
    /// profiles/rules, so a display-oriented group string alone is not enough.
    pub fn dependency_entity_ids(&self) -> Vec<&str> {
        match self {
            Self::ProfileCreate { profile } => vec![profile.id.as_str()],
            Self::ProfileUpdate { profile_id, .. } => vec![profile_id.as_str()],
            Self::ProfileDelete { request } => {
                let mut ids = vec![request.profile_id.as_str()];
                ids.extend(
                    request
                        .referencing_rules
                        .iter()
                        .map(|rule| rule.id.as_str()),
                );
                if let DeleteProfileDisposition::ReplaceWithProfile { profile_id } =
                    &request.disposition
                {
                    ids.push(profile_id.as_str());
                }
                ids
            }
            Self::SettingsUpdate {
                default_profile_id, ..
            } => default_profile_id.iter().map(String::as_str).collect(),
            Self::RuleCreate { rule } => {
                let mut ids = vec![rule.id.as_str()];
                if let Some(profile_id) = rule.action.profile_id() {
                    ids.push(profile_id);
                }
                ids
            }
            Self::RuleUpdate { rule_id, rule, .. } => {
                let mut ids = vec![rule_id.as_str()];
                if let Some(profile_id) = rule.action.profile_id() {
                    ids.push(profile_id);
                }
                ids
            }
            Self::RuleDelete { rule_id, .. } => vec![rule_id.as_str()],
            Self::RuleReorder { rules } => rules.iter().map(|rule| rule.id.as_str()).collect(),
        }
    }

    /// Retarget a server-based operation to collision-safe IDs allocated while
    /// cloning the last clean snapshot into the local-only recovery layer.
    pub fn remap_for_local_recovery(
        &mut self,
        profile_ids: &HashMap<String, String>,
        rule_ids: &HashMap<String, String>,
    ) {
        match self {
            Self::ProfileCreate { profile } => remap_id(&mut profile.id, profile_ids),
            Self::ProfileUpdate {
                profile_id,
                profile,
                ..
            } => {
                remap_id(profile_id, profile_ids);
                remap_id(&mut profile.id, profile_ids);
            }
            Self::ProfileDelete { request } => {
                remap_id(&mut request.profile_id, profile_ids);
                for rule in &mut request.referencing_rules {
                    remap_id(&mut rule.id, rule_ids);
                }
                if let DeleteProfileDisposition::ReplaceWithProfile { profile_id } =
                    &mut request.disposition
                {
                    remap_id(profile_id, profile_ids);
                }
                if let Some(target) = &mut request.local_binding_disposition {
                    target.layer = super::model::ProfileLayer::LocalOnly;
                    remap_action(&mut target.action, profile_ids);
                }
            }
            Self::SettingsUpdate {
                default_profile_id, ..
            } => {
                if let Some(profile_id) = default_profile_id {
                    remap_id(profile_id, profile_ids);
                }
            }
            Self::RuleCreate { rule } => {
                remap_id(&mut rule.id, rule_ids);
                remap_action(&mut rule.action, profile_ids);
            }
            Self::RuleUpdate { rule_id, rule, .. } => {
                remap_id(rule_id, rule_ids);
                remap_id(&mut rule.id, rule_ids);
                remap_action(&mut rule.action, profile_ids);
            }
            Self::RuleDelete { rule_id, .. } => remap_id(rule_id, rule_ids),
            Self::RuleReorder { rules } => {
                for rule in rules {
                    remap_id(&mut rule.id, rule_ids);
                }
            }
        }
    }

    pub fn to_json(&self) -> AppResult<String> {
        serde_json::to_string(self)
            .map_err(|error| AppError::Internal(format!("encode EQ operation: {error}")))
    }

    pub fn from_json(value: &str) -> AppResult<Self> {
        serde_json::from_str(value)
            .map_err(|error| AppError::Internal(format!("decode EQ operation: {error}")))
    }

    /// Materialize one queued operation over the clean server mirror. No
    /// server revisions are invented by this optimistic layer.
    pub fn materialize(&self, state: &mut EqualizerState) -> AppResult<()> {
        match self {
            Self::ProfileCreate { profile } => {
                if state.profiles.iter().all(|row| row.id != profile.id) {
                    state
                        .profiles
                        .push(profile_to_overlay(profile, Revision(0)));
                }
            }
            Self::ProfileUpdate {
                profile_id,
                profile,
                ..
            } => {
                let current = state
                    .profiles
                    .iter_mut()
                    .find(|row| row.id == *profile_id)
                    .ok_or_else(|| {
                        AppError::Internal(format!("pending profile {profile_id} missing"))
                    })?;
                let revision = current.revision;
                let created_at = current.created_at.clone();
                *current = profile_to_overlay(profile, revision);
                current.created_at = created_at;
            }
            Self::ProfileDelete { request } => apply_profile_delete(state, request),
            Self::SettingsUpdate {
                default_profile_id, ..
            } => {
                state.default_profile_id.clone_from(default_profile_id);
            }
            Self::RuleCreate { rule } => {
                if state.device_rules.iter().all(|row| row.id != rule.id) {
                    let priority = state
                        .device_rules
                        .iter()
                        .map(|row| row.priority)
                        .min()
                        .unwrap_or(1)
                        - 1;
                    state
                        .device_rules
                        .push(rule_to_overlay(rule, Revision(0), priority));
                }
            }
            Self::RuleUpdate { rule_id, rule, .. } => {
                let current = state
                    .device_rules
                    .iter_mut()
                    .find(|row| row.id == *rule_id)
                    .ok_or_else(|| AppError::Internal(format!("pending rule {rule_id} missing")))?;
                let (revision, priority) = (current.revision, current.priority);
                *current = rule_to_overlay(rule, revision, priority);
            }
            Self::RuleDelete { rule_id, .. } => state.device_rules.retain(|row| row.id != *rule_id),
            Self::RuleReorder { rules } => {
                let count = rules.len() as i32;
                for (index, expected) in rules.iter().enumerate() {
                    if let Some(rule) = state
                        .device_rules
                        .iter_mut()
                        .find(|row| row.id == expected.id)
                    {
                        rule.priority = count - index as i32;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn rebase(&mut self, state: &EqualizerState) {
        match self {
            Self::ProfileUpdate {
                profile_id,
                expected_revision,
                ..
            } => {
                if let Some(profile) = state.profiles.iter().find(|row| row.id == *profile_id) {
                    *expected_revision = profile.revision;
                }
            }
            Self::ProfileDelete { request } => {
                if let Some(profile) = state
                    .profiles
                    .iter()
                    .find(|row| row.id == request.profile_id)
                {
                    request.expected_revision = profile.revision;
                }
                request.expected_settings_revision = state.settings_revision;
                request.referencing_rules = state
                    .device_rules
                    .iter()
                    .filter(|rule| rule.action.profile_id() == Some(&request.profile_id))
                    .map(|rule| EntityRevision {
                        id: rule.id.clone(),
                        expected_revision: rule.revision,
                    })
                    .collect();
            }
            Self::SettingsUpdate {
                expected_settings_revision,
                ..
            } => {
                *expected_settings_revision = state.settings_revision;
            }
            Self::RuleUpdate {
                rule_id,
                expected_revision,
                ..
            }
            | Self::RuleDelete {
                rule_id,
                expected_revision,
            } => {
                if let Some(rule) = state.device_rules.iter().find(|row| row.id == *rule_id) {
                    *expected_revision = rule.revision;
                }
            }
            Self::RuleReorder { rules } => {
                for expected in rules {
                    if let Some(rule) = state.device_rules.iter().find(|row| row.id == expected.id)
                    {
                        expected.expected_revision = rule.revision;
                    }
                }
            }
            Self::ProfileCreate { .. } | Self::RuleCreate { .. } => {}
        }
    }
}

fn remap_id(id: &mut String, mapping: &HashMap<String, String>) {
    if let Some(mapped) = mapping.get(id) {
        *id = mapped.clone();
    }
}

fn remap_action(action: &mut RuleAction, profile_ids: &HashMap<String, String>) {
    if let RuleAction::Profile { profile_id } = action {
        remap_id(profile_id, profile_ids);
    }
}

fn profile_to_overlay(input: &EqualizerProfileInput, revision: Revision) -> EqualizerProfile {
    let now = now_string();
    EqualizerProfile {
        id: input.id.clone(),
        name: input.name.clone(),
        format_version: input.format_version,
        preamp_db: input.preamp_db,
        auto_headroom_enabled: input.auto_headroom_enabled,
        bands: input.bands.clone(),
        revision,
        created_at: now.clone(),
        updated_at: now,
    }
}

fn rule_to_overlay(
    input: &EqualizerDeviceRuleInput,
    revision: Revision,
    priority: i32,
) -> EqualizerDeviceRule {
    EqualizerDeviceRule {
        id: input.id.clone(),
        label: input.label.clone(),
        action: input.action.clone(),
        selectors: input.selectors.clone(),
        priority,
        enabled: input.enabled,
        revision,
    }
}

fn apply_profile_delete(state: &mut EqualizerState, request: &DeleteProfileRequest) {
    let deleted = &request.profile_id;
    let replacement = match &request.disposition {
        DeleteProfileDisposition::ReplaceWithProfile { profile_id } => Some(profile_id.clone()),
        DeleteProfileDisposition::ReplaceWithFlat
        | DeleteProfileDisposition::RejectIfReferenced => None,
    };
    if state.default_profile_id.as_ref() == Some(deleted) {
        state.default_profile_id.clone_from(&replacement);
    }
    for rule in &mut state.device_rules {
        if rule.action.profile_id() == Some(deleted) {
            rule.action = replacement
                .as_ref()
                .map_or(RuleAction::Bypass, |profile_id| RuleAction::Profile {
                    profile_id: profile_id.clone(),
                });
        }
    }
    state.profiles.retain(|row| row.id != *deleted);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equalizer::model::EqualizerProfile;

    #[test]
    fn queued_edits_materialize_in_order() {
        let profile = EqualizerProfile::five_band_starter("First");
        let mut state = EqualizerState::default();
        PendingEqualizerOpKind::ProfileCreate {
            profile: (&profile).into(),
        }
        .materialize(&mut state)
        .unwrap();
        let mut changed: EqualizerProfileInput = (&profile).into();
        changed.name = "Changed".to_string();
        PendingEqualizerOpKind::ProfileUpdate {
            profile_id: profile.id,
            expected_revision: Revision(0),
            profile: changed,
        }
        .materialize(&mut state)
        .unwrap();
        assert_eq!(state.profiles[0].name, "Changed");
    }

    #[test]
    fn local_recovery_remaps_entity_and_reference_edges() {
        let profile = EqualizerProfile::five_band_starter("Remote");
        let mut op = PendingEqualizerOpKind::RuleUpdate {
            rule_id: "server-rule".into(),
            expected_revision: Revision(7),
            rule: EqualizerDeviceRuleInput {
                id: "server-rule".into(),
                label: "Output".into(),
                action: RuleAction::Profile {
                    profile_id: profile.id.clone(),
                },
                selectors: vec![],
                enabled: true,
            },
        };
        op.remap_for_local_recovery(
            &HashMap::from([(profile.id, "local-profile".into())]),
            &HashMap::from([("server-rule".into(), "local-rule".into())]),
        );
        let PendingEqualizerOpKind::RuleUpdate { rule_id, rule, .. } = op else {
            panic!("expected rule update");
        };
        assert_eq!(rule_id, "local-rule");
        assert_eq!(rule.id, "local-rule");
        assert_eq!(rule.action.profile_id(), Some("local-profile"));
    }
}
