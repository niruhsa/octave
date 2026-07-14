//! Transactional Postgres implementation of [`EqualizerRepo`].
//!
//! The settings row is the per-owner serialization point. Every real mutation
//! locks it, updates the aggregate, bumps `state_revision` once, records the
//! complete before/after audit envelope, and only then commits.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::equalizer_core::{
    RollbackDiff, apply_rollback_inverse, profile_name_key, rollback_after_image_matches,
    rollback_diff,
};
use crate::error::{AppError, Result};

use super::models::*;
use super::pg::PgRepos;
use super::repo::EqualizerRepo;

const STATE_FORMAT_VERSION: i32 = 2;
const SNAPSHOT_FORMAT_VERSION: i32 = 1;
const MAX_PROFILES: usize = 64;
const MAX_RULES: usize = 64;

#[derive(Debug, FromRow)]
struct SettingsRow {
    default_profile_id: Option<Uuid>,
    revision: i64,
    state_revision: i64,
}

#[derive(Debug, FromRow)]
struct ProfileRow {
    id: Uuid,
    name: String,
    format_version: i32,
    preamp_db: f64,
    auto_headroom_enabled: bool,
    revision: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct BandRow {
    profile_id: Uuid,
    position: i32,
    enabled: bool,
    filter_type: String,
    frequency_hz: f64,
    gain_db: f64,
    q: f64,
}

#[derive(Debug, FromRow)]
struct RuleRow {
    id: Uuid,
    profile_id: Option<Uuid>,
    action: String,
    label: String,
    selector_json: String,
    priority: i32,
    enabled: bool,
    bass_boost_percent: i32,
    treble_boost_percent: i32,
    revision: i64,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

fn db(e: sqlx::Error) -> AppError {
    AppError::Internal(format!("db error: {e}"))
}

fn conflict(code: &str, message: impl Into<String>) -> AppError {
    AppError::Conflict {
        code: code.to_string(),
        message: message.into(),
    }
}

fn constraint(e: &sqlx::Error) -> Option<&str> {
    e.as_database_error().and_then(|d| d.constraint())
}

fn profile_insert_error(e: sqlx::Error) -> AppError {
    match constraint(&e) {
        Some("equalizer_profiles_owner_id_name_key_key") => {
            conflict("name_taken", "an equalizer profile already uses that name")
        }
        Some("equalizer_profiles_pkey") | Some("equalizer_profiles_owner_id_id_key") => {
            conflict("uuid_collision", "equalizer profile id already exists")
        }
        _ => db(e),
    }
}

fn rule_insert_error(e: sqlx::Error) -> AppError {
    match constraint(&e) {
        Some("equalizer_device_rules_owner_id_selector_hash_key") => conflict(
            "selector_taken",
            "an equalizer device rule already uses that selector set",
        ),
        Some("equalizer_device_rules_pkey") | Some("equalizer_device_rules_owner_id_id_key") => {
            conflict("uuid_collision", "equalizer device rule id already exists")
        }
        _ => db(e),
    }
}

async fn owner_exists(tx: &mut Transaction<'_, Postgres>, owner_id: Uuid) -> Result<bool> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(owner_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)
}

async fn ensure_settings(tx: &mut Transaction<'_, Postgres>, owner_id: Uuid) -> Result<()> {
    if !owner_exists(tx, owner_id).await? {
        return Err(AppError::NotFound("equalizer owner not found".into()));
    }
    sqlx::query("INSERT INTO equalizer_user_settings (user_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

async fn begin_locked(pool: &PgPool, owner_id: Uuid) -> Result<Transaction<'_, Postgres>> {
    let mut tx = pool.begin().await.map_err(db)?;
    ensure_settings(&mut tx, owner_id).await?;
    sqlx::query("SELECT user_id FROM equalizer_user_settings WHERE user_id = $1 FOR UPDATE")
        .bind(owner_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
    Ok(tx)
}

async fn load_state(tx: &mut Transaction<'_, Postgres>, owner_id: Uuid) -> Result<EqualizerState> {
    let settings = sqlx::query_as::<_, SettingsRow>(
        "SELECT default_profile_id, revision, state_revision FROM equalizer_user_settings WHERE user_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .ok_or_else(|| AppError::NotFound("equalizer owner not found".into()))?;

    let profile_rows = sqlx::query_as::<_, ProfileRow>(
        r#"SELECT id, name, format_version, preamp_db, auto_headroom_enabled,
                  revision, created_at, updated_at
           FROM equalizer_profiles
           WHERE owner_id = $1
           ORDER BY name_key, id"#,
    )
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;

    let band_rows = sqlx::query_as::<_, BandRow>(
        r#"SELECT b.profile_id, b.position, b.enabled, b.filter_type,
                  b.frequency_hz, b.gain_db, b.q
           FROM equalizer_bands b
           JOIN equalizer_profiles p ON p.id = b.profile_id
           WHERE p.owner_id = $1
           ORDER BY b.profile_id, b.position"#,
    )
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    let mut bands: HashMap<Uuid, Vec<EqualizerBand>> = HashMap::new();
    for b in band_rows {
        bands.entry(b.profile_id).or_default().push(EqualizerBand {
            position: b.position,
            enabled: b.enabled,
            filter_type: b.filter_type,
            frequency_hz: b.frequency_hz,
            gain_db: b.gain_db,
            q: b.q,
        });
    }
    let profiles = profile_rows
        .into_iter()
        .map(|p| EqualizerProfile {
            id: p.id,
            name: p.name,
            format_version: p.format_version,
            preamp_db: p.preamp_db,
            auto_headroom_enabled: p.auto_headroom_enabled,
            bands: bands.remove(&p.id).unwrap_or_default(),
            revision: p.revision,
            created_at: p.created_at,
            updated_at: p.updated_at,
        })
        .collect();

    let rule_rows = sqlx::query_as::<_, RuleRow>(
        r#"SELECT id, profile_id, action, label, selector_json, priority,
                  enabled, bass_boost_percent, treble_boost_percent,
                  revision, created_at, updated_at
           FROM equalizer_device_rules
           WHERE owner_id = $1
           ORDER BY priority DESC, id"#,
    )
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    let mut device_rules = Vec::with_capacity(rule_rows.len());
    for r in rule_rows {
        let selectors: Vec<PortableDeviceSelector> = serde_json::from_str(&r.selector_json)
            .map_err(|e| AppError::Internal(format!("stored equalizer selector JSON: {e}")))?;
        let action = match (r.action.as_str(), r.profile_id) {
            ("profile", Some(profile_id)) => EqualizerRuleAction::Profile { profile_id },
            ("bypass", None) => EqualizerRuleAction::Bypass,
            _ => {
                return Err(AppError::Internal(
                    "stored equalizer rule has inconsistent action/profile".into(),
                ));
            }
        };
        device_rules.push(EqualizerDeviceRule {
            id: r.id,
            label: r.label,
            action,
            selectors,
            priority: r.priority,
            enabled: r.enabled,
            bass_boost_percent: r.bass_boost_percent,
            treble_boost_percent: r.treble_boost_percent,
            revision: r.revision,
            created_at: r.created_at,
            updated_at: r.updated_at,
        });
    }

    Ok(EqualizerState {
        state_format_version: STATE_FORMAT_VERSION,
        state_revision: settings.state_revision,
        settings_revision: settings.revision,
        default_profile_id: settings.default_profile_id,
        profiles,
        device_rules,
    })
}

async fn bump_state(tx: &mut Transaction<'_, Postgres>, owner_id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE equalizer_user_settings SET state_revision = state_revision + 1, updated_at = now() WHERE user_id = $1",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn finish_audited(
    mut tx: Transaction<'_, Postgres>,
    actor_id: Option<Uuid>,
    owner_id: Uuid,
    action: &str,
    resource_type: &str,
    resource_id: Option<Uuid>,
    disposition: Option<ProfileDeleteDisposition>,
    original_audit_id: Option<Uuid>,
    before: EqualizerState,
) -> Result<EqualizerMutationOutcome> {
    let after = load_state(&mut tx, owner_id).await?;
    let audit_id = insert_audit(
        &mut tx,
        actor_id,
        owner_id,
        action,
        resource_type,
        resource_id,
        disposition,
        original_audit_id,
        &before,
        &after,
    )
    .await?;
    tx.commit().await.map_err(db)?;
    Ok(EqualizerMutationOutcome {
        changed: true,
        audit_id: Some(audit_id),
        state: after,
    })
}

async fn finish_unchanged(
    tx: Transaction<'_, Postgres>,
    state: EqualizerState,
) -> Result<EqualizerMutationOutcome> {
    tx.commit().await.map_err(db)?;
    Ok(EqualizerMutationOutcome {
        changed: false,
        audit_id: None,
        state,
    })
}

#[allow(clippy::too_many_arguments)]
async fn insert_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Option<Uuid>,
    owner_id: Uuid,
    action: &str,
    resource_type: &str,
    resource_id: Option<Uuid>,
    disposition: Option<ProfileDeleteDisposition>,
    original_audit_id: Option<Uuid>,
    before: &EqualizerState,
    after: &EqualizerState,
) -> Result<Uuid> {
    let before_json = serde_json::to_string(&EqualizerAuditSnapshot {
        snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
        resource_type: resource_type.to_string(),
        resource_id,
        disposition: disposition.clone(),
        original_audit_id,
        state: before.clone(),
    })
    .map_err(|e| AppError::Internal(format!("equalizer audit JSON: {e}")))?;
    let after_json = serde_json::to_string(&EqualizerAuditSnapshot {
        snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
        resource_type: resource_type.to_string(),
        resource_id,
        disposition,
        original_audit_id,
        state: after.clone(),
    })
    .map_err(|e| AppError::Internal(format!("equalizer audit JSON: {e}")))?;
    sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO audit_log
             (actor_id, action, entity_type, entity_id, before_json, after_json)
           VALUES ($1, $2, 'equalizer_state', $3, $4, $5)
           RETURNING id"#,
    )
    .bind(actor_id)
    .bind(action)
    .bind(owner_id)
    .bind(before_json)
    .bind(after_json)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)
}

fn profile_matches_draft(p: &EqualizerProfile, d: &EqualizerProfileDraft) -> bool {
    p.id == d.id
        && p.name == d.name
        && p.format_version == d.format_version
        && p.preamp_db == d.preamp_db
        && p.auto_headroom_enabled == d.auto_headroom_enabled
        && p.bands == d.bands
}

fn rule_matches_draft(r: &EqualizerDeviceRule, d: &EqualizerDeviceRuleDraft) -> bool {
    r.id == d.id
        && r.label == d.label
        && r.action == d.action
        && r.selectors == d.selectors
        && r.enabled == d.enabled
        && r.bass_boost_percent == d.bass_boost_percent
        && r.treble_boost_percent == d.treble_boost_percent
}

fn ensure_profile_name_available(
    state: &EqualizerState,
    profile_id: Uuid,
    name_key: &str,
) -> Result<()> {
    if state
        .profiles
        .iter()
        .any(|p| p.id != profile_id && profile_name_key(&p.name) == name_key)
    {
        return Err(conflict(
            "name_taken",
            "an equalizer profile already uses that name",
        ));
    }
    Ok(())
}

fn ensure_rule_references_and_selectors(
    state: &EqualizerState,
    rule_id: Uuid,
    draft: &EqualizerDeviceRuleDraft,
) -> Result<()> {
    if let EqualizerRuleAction::Profile { profile_id } = draft.action
        && !state.profiles.iter().any(|p| p.id == profile_id)
    {
        return Err(AppError::NotFound(
            "equalizer rule target profile not found".into(),
        ));
    }
    if state
        .device_rules
        .iter()
        .filter(|r| r.id != rule_id)
        .any(|r| {
            r.selectors.iter().any(|existing| {
                draft
                    .selectors
                    .iter()
                    .any(|candidate| candidate == existing)
            })
        })
    {
        return Err(conflict(
            "selector_taken",
            "a portable selector is already used by another rule",
        ));
    }
    Ok(())
}

fn action_parts(action: &EqualizerRuleAction) -> (&'static str, Option<Uuid>) {
    match action {
        EqualizerRuleAction::Profile { profile_id } => ("profile", Some(*profile_id)),
        EqualizerRuleAction::Bypass => ("bypass", None),
    }
}

async fn insert_bands(
    tx: &mut Transaction<'_, Postgres>,
    profile_id: Uuid,
    bands: &[EqualizerBand],
) -> Result<()> {
    for b in bands {
        sqlx::query(
            r#"INSERT INTO equalizer_bands
                 (profile_id, position, enabled, filter_type, frequency_hz, gain_db, q)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(profile_id)
        .bind(b.position)
        .bind(b.enabled)
        .bind(&b.filter_type)
        .bind(b.frequency_hz)
        .bind(b.gain_db)
        .bind(b.q)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    }
    Ok(())
}

#[async_trait]
impl EqualizerRepo for PgRepos {
    async fn get_equalizer_state(&self, owner_id: Uuid) -> Result<EqualizerState> {
        let mut tx = self.pool().begin().await.map_err(db)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        ensure_settings(&mut tx, owner_id).await?;
        sqlx::query("SELECT user_id FROM equalizer_user_settings WHERE user_id = $1 FOR SHARE")
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        let state = load_state(&mut tx, owner_id).await?;
        tx.commit().await.map_err(db)?;
        Ok(state)
    }

    async fn create_equalizer_profile(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        profile: EqualizerProfileDraft,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        if let Some(existing) = before.profiles.iter().find(|p| p.id == profile.id) {
            if profile_matches_draft(existing, &profile) {
                return finish_unchanged(tx, before).await;
            }
            return Err(conflict(
                "uuid_collision",
                "equalizer profile id already exists with different content",
            ));
        }
        if before.profiles.len() >= MAX_PROFILES {
            return Err(AppError::InvalidArgument(format!(
                "at most {MAX_PROFILES} equalizer profiles are allowed"
            )));
        }
        ensure_profile_name_available(&before, profile.id, &profile.name_key)?;
        sqlx::query(
            r#"INSERT INTO equalizer_profiles
                 (id, owner_id, name, name_key, format_version, preamp_db,
                  auto_headroom_enabled)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(profile.id)
        .bind(owner_id)
        .bind(&profile.name)
        .bind(&profile.name_key)
        .bind(profile.format_version)
        .bind(profile.preamp_db)
        .bind(profile.auto_headroom_enabled)
        .execute(&mut *tx)
        .await
        .map_err(profile_insert_error)?;
        insert_bands(&mut tx, profile.id, &profile.bands).await?;
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.profile.create",
            "profile",
            Some(profile.id),
            None,
            None,
            before,
        )
        .await
    }

    async fn update_equalizer_profile(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        profile_id: Uuid,
        expected_revision: i64,
        profile: EqualizerProfileDraft,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        let existing = before
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| AppError::NotFound("equalizer profile not found".into()))?;
        if profile_matches_draft(existing, &profile) {
            return finish_unchanged(tx, before).await;
        }
        if existing.revision != expected_revision {
            return Err(conflict(
                "revision_mismatch",
                "equalizer profile revision is stale",
            ));
        }
        ensure_profile_name_available(&before, profile_id, &profile.name_key)?;
        sqlx::query(
            r#"UPDATE equalizer_profiles
               SET name = $3, name_key = $4, format_version = $5,
                   preamp_db = $6, auto_headroom_enabled = $7,
                   revision = revision + 1, updated_at = now()
               WHERE owner_id = $1 AND id = $2"#,
        )
        .bind(owner_id)
        .bind(profile_id)
        .bind(&profile.name)
        .bind(&profile.name_key)
        .bind(profile.format_version)
        .bind(profile.preamp_db)
        .bind(profile.auto_headroom_enabled)
        .execute(&mut *tx)
        .await
        .map_err(profile_insert_error)?;
        sqlx::query("DELETE FROM equalizer_bands WHERE profile_id = $1")
            .bind(profile_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        insert_bands(&mut tx, profile_id, &profile.bands).await?;
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.profile.update",
            "profile",
            Some(profile_id),
            None,
            None,
            before,
        )
        .await
    }

    async fn delete_equalizer_profile(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        request: DeleteEqualizerProfile,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        let Some(existing) = before.profiles.iter().find(|p| p.id == request.profile_id) else {
            return finish_unchanged(tx, before).await;
        };
        if existing.revision != request.expected_revision
            || before.settings_revision != request.expected_settings_revision
        {
            return Err(conflict(
                "revision_mismatch",
                "equalizer profile deletion preconditions are stale",
            ));
        }

        let mut actual_refs: Vec<EntityRevision> = before
            .device_rules
            .iter()
            .filter_map(|r| match r.action {
                EqualizerRuleAction::Profile { profile_id } if profile_id == request.profile_id => {
                    Some(EntityRevision {
                        id: r.id,
                        expected_revision: r.revision,
                    })
                }
                _ => None,
            })
            .collect();
        let mut expected_refs = request.referencing_rules.clone();
        actual_refs.sort_by_key(|r| r.id);
        expected_refs.sort_by_key(|r| r.id);
        if actual_refs != expected_refs {
            return Err(conflict(
                "revision_mismatch",
                "equalizer profile reference set changed",
            ));
        }
        let is_default = before.default_profile_id == Some(request.profile_id);

        match request.disposition.clone() {
            ProfileDeleteDisposition::RejectIfReferenced => {
                if is_default || !actual_refs.is_empty() {
                    return Err(conflict(
                        "profile_referenced",
                        "equalizer profile is still selected by settings or device rules",
                    ));
                }
            }
            ProfileDeleteDisposition::ReplaceWithProfile { profile_id } => {
                if profile_id == request.profile_id {
                    return Err(AppError::InvalidArgument(
                        "replacement profile must differ from deleted profile".into(),
                    ));
                }
                if !before.profiles.iter().any(|p| p.id == profile_id) {
                    return Err(AppError::NotFound(
                        "replacement equalizer profile not found".into(),
                    ));
                }
                if is_default {
                    sqlx::query(
                        r#"UPDATE equalizer_user_settings
                           SET default_profile_id = $2, revision = revision + 1,
                               updated_at = now()
                           WHERE user_id = $1"#,
                    )
                    .bind(owner_id)
                    .bind(profile_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                }
                sqlx::query(
                    r#"UPDATE equalizer_device_rules
                       SET profile_id = $3, revision = revision + 1, updated_at = now()
                       WHERE owner_id = $1 AND profile_id = $2"#,
                )
                .bind(owner_id)
                .bind(request.profile_id)
                .bind(profile_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
            ProfileDeleteDisposition::ReplaceWithFlat => {
                if is_default {
                    sqlx::query(
                        r#"UPDATE equalizer_user_settings
                           SET default_profile_id = NULL, revision = revision + 1,
                               updated_at = now()
                           WHERE user_id = $1"#,
                    )
                    .bind(owner_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                }
                sqlx::query(
                    r#"UPDATE equalizer_device_rules
                       SET action = 'bypass', profile_id = NULL,
                           revision = revision + 1, updated_at = now()
                       WHERE owner_id = $1 AND profile_id = $2"#,
                )
                .bind(owner_id)
                .bind(request.profile_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
        }

        sqlx::query("DELETE FROM equalizer_profiles WHERE owner_id = $1 AND id = $2")
            .bind(owner_id)
            .bind(request.profile_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.profile.delete",
            "profile",
            Some(request.profile_id),
            Some(request.disposition),
            None,
            before,
        )
        .await
    }

    async fn update_equalizer_settings(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        expected_settings_revision: i64,
        default_profile_id: Option<Uuid>,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        if before.default_profile_id == default_profile_id {
            return finish_unchanged(tx, before).await;
        }
        if before.settings_revision != expected_settings_revision {
            return Err(conflict(
                "revision_mismatch",
                "equalizer settings revision is stale",
            ));
        }
        if let Some(id) = default_profile_id
            && !before.profiles.iter().any(|p| p.id == id)
        {
            return Err(AppError::NotFound(
                "default equalizer profile not found".into(),
            ));
        }
        sqlx::query(
            r#"UPDATE equalizer_user_settings
               SET default_profile_id = $2, revision = revision + 1,
                   state_revision = state_revision + 1, updated_at = now()
               WHERE user_id = $1"#,
        )
        .bind(owner_id)
        .bind(default_profile_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.settings.update",
            "settings",
            None,
            None,
            None,
            before,
        )
        .await
    }

    async fn create_equalizer_rule(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        rule: EqualizerDeviceRuleDraft,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        if let Some(existing) = before.device_rules.iter().find(|r| r.id == rule.id) {
            if rule_matches_draft(existing, &rule) {
                return finish_unchanged(tx, before).await;
            }
            return Err(conflict(
                "uuid_collision",
                "equalizer device rule id already exists with different content",
            ));
        }
        if before.device_rules.len() >= MAX_RULES {
            return Err(AppError::InvalidArgument(format!(
                "at most {MAX_RULES} equalizer device rules are allowed"
            )));
        }
        ensure_rule_references_and_selectors(&before, rule.id, &rule)?;
        let priority = before
            .device_rules
            .iter()
            .map(|r| r.priority)
            .min()
            .map(|p| p.saturating_sub(1))
            .unwrap_or(1);
        let (action, profile_id) = action_parts(&rule.action);
        sqlx::query(
            r#"INSERT INTO equalizer_device_rules
                 (id, owner_id, profile_id, action, label, selector_json,
                  selector_hash, priority, enabled, bass_boost_percent,
                  treble_boost_percent)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
        )
        .bind(rule.id)
        .bind(owner_id)
        .bind(profile_id)
        .bind(action)
        .bind(&rule.label)
        .bind(&rule.selector_json)
        .bind(&rule.selector_hash)
        .bind(priority)
        .bind(rule.enabled)
        .bind(rule.bass_boost_percent)
        .bind(rule.treble_boost_percent)
        .execute(&mut *tx)
        .await
        .map_err(rule_insert_error)?;
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.rule.create",
            "device_rule",
            Some(rule.id),
            None,
            None,
            before,
        )
        .await
    }

    async fn update_equalizer_rule(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        rule_id: Uuid,
        expected_revision: i64,
        rule: EqualizerDeviceRuleDraft,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        let existing = before
            .device_rules
            .iter()
            .find(|r| r.id == rule_id)
            .ok_or_else(|| AppError::NotFound("equalizer device rule not found".into()))?;
        if rule_matches_draft(existing, &rule) {
            return finish_unchanged(tx, before).await;
        }
        if existing.revision != expected_revision {
            return Err(conflict(
                "revision_mismatch",
                "equalizer device rule revision is stale",
            ));
        }
        ensure_rule_references_and_selectors(&before, rule_id, &rule)?;
        let (action, profile_id) = action_parts(&rule.action);
        sqlx::query(
            r#"UPDATE equalizer_device_rules
               SET profile_id = $3, action = $4, label = $5, selector_json = $6,
                   selector_hash = $7, enabled = $8, bass_boost_percent = $9,
                   treble_boost_percent = $10,
                   revision = revision + 1, updated_at = now()
               WHERE owner_id = $1 AND id = $2"#,
        )
        .bind(owner_id)
        .bind(rule_id)
        .bind(profile_id)
        .bind(action)
        .bind(&rule.label)
        .bind(&rule.selector_json)
        .bind(&rule.selector_hash)
        .bind(rule.enabled)
        .bind(rule.bass_boost_percent)
        .bind(rule.treble_boost_percent)
        .execute(&mut *tx)
        .await
        .map_err(rule_insert_error)?;
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.rule.update",
            "device_rule",
            Some(rule_id),
            None,
            None,
            before,
        )
        .await
    }

    async fn delete_equalizer_rule(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        rule_id: Uuid,
        expected_revision: i64,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        let Some(existing) = before.device_rules.iter().find(|r| r.id == rule_id) else {
            return finish_unchanged(tx, before).await;
        };
        if existing.revision != expected_revision {
            return Err(conflict(
                "revision_mismatch",
                "equalizer device rule revision is stale",
            ));
        }
        sqlx::query("DELETE FROM equalizer_device_rules WHERE owner_id = $1 AND id = $2")
            .bind(owner_id)
            .bind(rule_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.rule.delete",
            "device_rule",
            Some(rule_id),
            None,
            None,
            before,
        )
        .await
    }

    async fn reorder_equalizer_rules(
        &self,
        actor_id: Option<Uuid>,
        owner_id: Uuid,
        rules: Vec<EntityRevision>,
    ) -> Result<EqualizerMutationOutcome> {
        let mut tx = begin_locked(self.pool(), owner_id).await?;
        let before = load_state(&mut tx, owner_id).await?;
        let current_order: Vec<Uuid> = before.device_rules.iter().map(|r| r.id).collect();
        let requested_order: Vec<Uuid> = rules.iter().map(|r| r.id).collect();
        let count = before.device_rules.len() as i32;
        let already_dense = before
            .device_rules
            .iter()
            .enumerate()
            .all(|(index, rule)| rule.priority == count - index as i32);
        if current_order == requested_order && already_dense {
            return finish_unchanged(tx, before).await;
        }
        let unique: HashSet<Uuid> = requested_order.iter().copied().collect();
        let current: HashSet<Uuid> = current_order.iter().copied().collect();
        if unique.len() != rules.len() || unique != current {
            return Err(conflict(
                "revision_mismatch",
                "rule reorder must contain the complete current rule set exactly once",
            ));
        }
        let revisions: HashMap<Uuid, i64> = before
            .device_rules
            .iter()
            .map(|r| (r.id, r.revision))
            .collect();
        if rules
            .iter()
            .any(|r| revisions.get(&r.id) != Some(&r.expected_revision))
        {
            return Err(conflict(
                "revision_mismatch",
                "one or more equalizer device rule revisions are stale",
            ));
        }
        let count = rules.len() as i32;
        for (index, item) in rules.iter().enumerate() {
            let priority = count - index as i32;
            let existing = before
                .device_rules
                .iter()
                .find(|r| r.id == item.id)
                .expect("validated complete rule set");
            if existing.priority != priority {
                sqlx::query(
                    r#"UPDATE equalizer_device_rules
                       SET priority = $3, revision = revision + 1, updated_at = now()
                       WHERE owner_id = $1 AND id = $2"#,
                )
                .bind(owner_id)
                .bind(item.id)
                .bind(priority)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
        }
        bump_state(&mut tx, owner_id).await?;
        finish_audited(
            tx,
            actor_id,
            owner_id,
            "equalizer.rule.reorder",
            "device_rule_order",
            None,
            None,
            None,
            before,
        )
        .await
    }

    async fn equalizer_ids_available_for_owner(
        &self,
        owner_id: Uuid,
        profile_ids: &[Uuid],
        rule_ids: &[Uuid],
    ) -> Result<bool> {
        let profile_collision = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM equalizer_profiles \
             WHERE id = ANY($1) AND owner_id <> $2)",
        )
        .bind(profile_ids)
        .bind(owner_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        if profile_collision {
            return Ok(false);
        }
        let rule_collision = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM equalizer_device_rules \
             WHERE id = ANY($1) AND owner_id <> $2)",
        )
        .bind(rule_ids)
        .bind(owner_id)
        .fetch_one(self.pool())
        .await
        .map_err(db)?;
        Ok(!rule_collision)
    }

    async fn rollback_equalizer_change(
        &self,
        actor_id: Option<Uuid>,
        audit_id: Uuid,
        expected_state_revision: i64,
    ) -> Result<EqualizerRollbackOutcome> {
        let mut tx = self.pool().begin().await.map_err(db)?;
        let audit = sqlx::query_as::<_, AuditEntry>(
            r#"SELECT id, actor_id, action, entity_type, entity_id,
                      before_json, after_json, created_at
               FROM audit_log WHERE id = $1"#,
        )
        .bind(audit_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::NotFound("equalizer audit change not found".into()))?;
        if audit.entity_type != "equalizer_state" || !audit.action.starts_with("equalizer.") {
            return Err(AppError::InvalidArgument(
                "audit change is not an equalizer mutation".into(),
            ));
        }
        let owner_id = audit
            .entity_id
            .ok_or_else(|| AppError::Internal("equalizer audit row has no owner".into()))?;
        let before_snapshot: EqualizerAuditSnapshot = serde_json::from_str(
            audit
                .before_json
                .as_deref()
                .ok_or_else(|| AppError::InvalidArgument("audit has no before snapshot".into()))?,
        )
        .map_err(|e| AppError::InvalidArgument(format!("unsupported audit snapshot: {e}")))?;
        let after_snapshot: EqualizerAuditSnapshot = serde_json::from_str(
            audit
                .after_json
                .as_deref()
                .ok_or_else(|| AppError::InvalidArgument("audit has no after snapshot".into()))?,
        )
        .map_err(|e| AppError::InvalidArgument(format!("unsupported audit snapshot: {e}")))?;
        if before_snapshot.snapshot_format_version != SNAPSHOT_FORMAT_VERSION
            || after_snapshot.snapshot_format_version != SNAPSHOT_FORMAT_VERSION
        {
            return Err(AppError::InvalidArgument(
                "unsupported equalizer audit snapshot version".into(),
            ));
        }

        ensure_settings(&mut tx, owner_id).await?;
        sqlx::query("SELECT user_id FROM equalizer_user_settings WHERE user_id = $1 FOR UPDATE")
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        let current = load_state(&mut tx, owner_id).await?;
        if current.state_revision != expected_state_revision {
            return Err(conflict(
                "revision_mismatch",
                "equalizer state changed since audit detail was loaded",
            ));
        }
        let diff = rollback_diff(&before_snapshot.state, &after_snapshot.state);
        if !rollback_after_image_matches(&current, &after_snapshot.state, &diff) {
            return Err(conflict(
                "revision_mismatch",
                "affected equalizer state no longer matches this change's after-image",
            ));
        }

        let target = apply_rollback_inverse(&current, &before_snapshot.state, &diff);
        let changed_resources = changed_resources(&current, &target);
        let restored = restore_state(&mut tx, owner_id, &current, &target, &diff).await?;
        let rollback_audit_id = insert_audit(
            &mut tx,
            actor_id,
            owner_id,
            "equalizer.rollback",
            "rollback",
            Some(audit_id),
            None,
            Some(audit_id),
            &current,
            &restored,
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(EqualizerRollbackOutcome {
            target_owner_id: owner_id,
            state_revision: restored.state_revision,
            audit_id: rollback_audit_id,
            changed_resources,
        })
    }
}

async fn restore_state(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    current: &EqualizerState,
    target: &EqualizerState,
    diff: &RollbackDiff,
) -> Result<EqualizerState> {
    let now = OffsetDateTime::now_utc();
    let mut restored = target.clone();
    restored.state_format_version = STATE_FORMAT_VERSION;
    restored.state_revision = current.state_revision + 1;
    restored.settings_revision = if diff.settings {
        current.settings_revision + 1
    } else {
        current.settings_revision
    };
    for profile in &mut restored.profiles {
        if !diff.profile_ids.contains(&profile.id) {
            continue;
        }
        let current_revision = current
            .profiles
            .iter()
            .find(|p| p.id == profile.id)
            .map(|p| p.revision)
            .unwrap_or(0);
        profile.revision = current_revision.max(profile.revision) + 1;
        profile.updated_at = now;
    }
    for rule in &mut restored.device_rules {
        if !diff.rule_ids.contains(&rule.id) {
            continue;
        }
        let current_revision = current
            .device_rules
            .iter()
            .find(|r| r.id == rule.id)
            .map(|r| r.revision)
            .unwrap_or(0);
        rule.revision = current_revision.max(rule.revision) + 1;
        rule.updated_at = now;
    }

    sqlx::query("DELETE FROM equalizer_device_rules WHERE owner_id = $1")
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    // Clear the deferred reference while profiles are replaced.
    sqlx::query("UPDATE equalizer_user_settings SET default_profile_id = NULL WHERE user_id = $1")
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    sqlx::query("DELETE FROM equalizer_profiles WHERE owner_id = $1")
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(db)?;

    for profile in &restored.profiles {
        sqlx::query(
            r#"INSERT INTO equalizer_profiles
                 (id, owner_id, name, name_key, format_version, preamp_db,
                  auto_headroom_enabled, revision, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(profile.id)
        .bind(owner_id)
        .bind(&profile.name)
        .bind(profile_name_key(&profile.name))
        .bind(profile.format_version)
        .bind(profile.preamp_db)
        .bind(profile.auto_headroom_enabled)
        .bind(profile.revision)
        .bind(profile.created_at)
        .bind(profile.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(profile_insert_error)?;
        insert_bands(tx, profile.id, &profile.bands).await?;
    }

    for rule in &restored.device_rules {
        let selector_json = serde_json::to_string(&rule.selectors)
            .map_err(|e| AppError::Internal(format!("equalizer selector JSON: {e}")))?;
        let selector_hash = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(selector_json.as_bytes()))
        };
        let (action, profile_id) = action_parts(&rule.action);
        sqlx::query(
            r#"INSERT INTO equalizer_device_rules
                 (id, owner_id, profile_id, action, label, selector_json,
                  selector_hash, priority, enabled, bass_boost_percent,
                  treble_boost_percent, revision, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
        )
        .bind(rule.id)
        .bind(owner_id)
        .bind(profile_id)
        .bind(action)
        .bind(&rule.label)
        .bind(selector_json)
        .bind(selector_hash)
        .bind(rule.priority)
        .bind(rule.enabled)
        .bind(rule.bass_boost_percent)
        .bind(rule.treble_boost_percent)
        .bind(rule.revision)
        .bind(rule.created_at)
        .bind(rule.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(rule_insert_error)?;
    }
    sqlx::query(
        r#"UPDATE equalizer_user_settings
           SET default_profile_id = $2, revision = $3, state_revision = $4,
               updated_at = now()
           WHERE user_id = $1"#,
    )
    .bind(owner_id)
    .bind(restored.default_profile_id)
    .bind(restored.settings_revision)
    .bind(restored.state_revision)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(restored)
}

fn changed_resources(
    current: &EqualizerState,
    target: &EqualizerState,
) -> Vec<EqualizerChangedResource> {
    let mut out = Vec::new();
    let profile_ids: HashSet<Uuid> = current
        .profiles
        .iter()
        .chain(target.profiles.iter())
        .map(|p| p.id)
        .collect();
    for id in profile_ids {
        let before = current.profiles.iter().find(|p| p.id == id);
        let after = target.profiles.iter().find(|p| p.id == id);
        if before != after {
            out.push(EqualizerChangedResource {
                resource_type: "profile".into(),
                resource_id: Some(id),
                change: match (before, after) {
                    (None, Some(_)) => "restored",
                    (Some(_), None) => "removed",
                    _ => "updated",
                }
                .into(),
            });
        }
    }
    let rule_ids: HashSet<Uuid> = current
        .device_rules
        .iter()
        .chain(target.device_rules.iter())
        .map(|r| r.id)
        .collect();
    for id in rule_ids {
        let before = current.device_rules.iter().find(|r| r.id == id);
        let after = target.device_rules.iter().find(|r| r.id == id);
        if before != after {
            out.push(EqualizerChangedResource {
                resource_type: "device_rule".into(),
                resource_id: Some(id),
                change: match (before, after) {
                    (None, Some(_)) => "restored",
                    (Some(_), None) => "removed",
                    _ => "updated",
                }
                .into(),
            });
        }
    }
    if current.default_profile_id != target.default_profile_id {
        out.push(EqualizerChangedResource {
            resource_type: "settings".into(),
            resource_id: None,
            change: "updated".into(),
        });
    }
    if out.is_empty() {
        out.push(EqualizerChangedResource {
            resource_type: "equalizer_state".into(),
            resource_id: None,
            change: "revision_restored".into(),
        });
    }
    out.sort_by(|a, b| {
        a.resource_type
            .cmp(&b.resource_type)
            .then_with(|| a.resource_id.cmp(&b.resource_id))
    });
    out
}
