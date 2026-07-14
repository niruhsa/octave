//! SQLite persistence for the EQ clean mirror, device-only layer, and outbox.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use sqlx::{FromRow, Row, Sqlite, SqlitePool, Transaction};

use super::model::*;
use super::ops::PendingEqualizerOpKind;
use crate::error::{AppError, AppResult};

#[derive(Debug, Default)]
pub struct EqualizerRecoveryMap {
    pub profile_ids: HashMap<String, String>,
    pub rule_ids: HashMap<String, String>,
}

#[derive(Debug, FromRow)]
struct ProfileRow {
    id: String,
    name: String,
    format_version: i64,
    preamp_db: f64,
    auto_headroom: i64,
    revision: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, FromRow)]
struct BandRow {
    position: i64,
    enabled: i64,
    filter_kind: String,
    frequency_hz: f64,
    gain_db: f64,
    q: f64,
}

#[derive(Debug, FromRow)]
struct RuleRow {
    id: String,
    label: String,
    action: String,
    profile_id: Option<String>,
    selectors_json: String,
    priority: i32,
    enabled: i64,
    bass_boost_percent: i64,
    treble_boost_percent: i64,
    revision: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
struct PendingRow {
    id: i64,
    operation_uuid: String,
    account_scope: String,
    op_type: String,
    entity_id: Option<String>,
    base_revision: Option<String>,
    dependency_group: String,
    payload_json: String,
    created_at: String,
    attempts: i64,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqualizerSyncState {
    pub state_revision: Revision,
    pub etag: Option<String>,
    pub has_complete_snapshot: bool,
    pub support_state: SupportState,
    pub last_probe_at: Option<String>,
    pub last_probe_app_version: Option<String>,
    pub synced_at: Option<String>,
}

impl Default for EqualizerSyncState {
    fn default() -> Self {
        Self {
            state_revision: Revision(0),
            etag: None,
            has_complete_snapshot: false,
            support_state: SupportState::Unknown,
            last_probe_at: None,
            last_probe_app_version: None,
            synced_at: None,
        }
    }
}

pub async fn get_sync_state(pool: &SqlitePool, scope: &str) -> AppResult<EqualizerSyncState> {
    let row = sqlx::query(
        "SELECT state_revision, etag, has_complete_snapshot, support_state, \
         last_probe_at, last_probe_app_version, synced_at \
         FROM equalizer_sync_state WHERE account_scope = ?1",
    )
    .bind(scope)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(EqualizerSyncState::default());
    };
    Ok(EqualizerSyncState {
        state_revision: parse_revision(&row.get::<String, _>("state_revision"))?,
        etag: row.get("etag"),
        has_complete_snapshot: row.get::<i64, _>("has_complete_snapshot") != 0,
        support_state: parse_support(&row.get::<String, _>("support_state"))?,
        last_probe_at: row.get("last_probe_at"),
        last_probe_app_version: row.get("last_probe_app_version"),
        synced_at: row.get("synced_at"),
    })
}

pub async fn set_support_state(
    pool: &SqlitePool,
    scope: &str,
    support: SupportState,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO equalizer_sync_state \
         (account_scope, support_state, last_probe_at, last_probe_app_version) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(account_scope) DO UPDATE SET support_state = excluded.support_state, \
         last_probe_at = excluded.last_probe_at, last_probe_app_version = excluded.last_probe_app_version",
    )
    .bind(scope)
    .bind(support_string(support))
    .bind(now_string())
    .bind(env!("CARGO_PKG_VERSION"))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_preferences(pool: &SqlitePool, scope: &str) -> AppResult<LocalPreferences> {
    let row = sqlx::query(
        "SELECT master_enabled, automatic_switching_enabled \
         FROM equalizer_local_preferences WHERE account_scope = ?1",
    )
    .bind(scope)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map_or_else(LocalPreferences::default, |row| LocalPreferences {
            master_enabled: row.get::<i64, _>("master_enabled") != 0,
            automatic_switching_enabled: row.get::<i64, _>("automatic_switching_enabled") != 0,
        }),
    )
}

pub async fn set_preferences(
    pool: &SqlitePool,
    scope: &str,
    preferences: &LocalPreferences,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO equalizer_local_preferences \
         (account_scope, master_enabled, automatic_switching_enabled, updated_at) \
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(account_scope) DO UPDATE SET \
         master_enabled = excluded.master_enabled, \
         automatic_switching_enabled = excluded.automatic_switching_enabled, \
         updated_at = excluded.updated_at",
    )
    .bind(scope)
    .bind(i64::from(preferences.master_enabled))
    .bind(i64::from(preferences.automatic_switching_enabled))
    .bind(now_string())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn load_synced_state(pool: &SqlitePool, scope: &str) -> AppResult<EqualizerState> {
    let settings = sqlx::query(
        "SELECT state_format_version, state_revision, settings_revision, default_profile_id \
         FROM equalizer_synced_user_settings WHERE account_scope = ?1",
    )
    .bind(scope)
    .fetch_optional(pool)
    .await?;
    let mut state = if let Some(row) = settings {
        EqualizerState {
            state_format_version: row.get::<i64, _>("state_format_version") as u32,
            state_revision: parse_revision(&row.get::<String, _>("state_revision"))?,
            settings_revision: parse_revision(&row.get::<String, _>("settings_revision"))?,
            default_profile_id: row.get("default_profile_id"),
            profiles: Vec::new(),
            device_rules: Vec::new(),
        }
    } else {
        EqualizerState::default()
    };
    state.profiles = load_profiles(pool, scope, true).await?;
    state.device_rules = load_rules(pool, scope, true).await?;
    Ok(state)
}

pub async fn load_materialized_synced_state(
    pool: &SqlitePool,
    scope: &str,
) -> AppResult<EqualizerState> {
    let mut state = load_synced_state(pool, scope).await?;
    for row in list_pending_ops(pool, scope).await? {
        PendingEqualizerOpKind::from_json(&row.payload_json)?.materialize(&mut state)?;
    }
    Ok(state)
}

pub async fn load_local_state(pool: &SqlitePool, scope: &str) -> AppResult<LocalEqualizerState> {
    let default_profile_id = sqlx::query_scalar::<_, Option<String>>(
        "SELECT default_profile_id FROM equalizer_local_user_settings WHERE account_scope = ?1",
    )
    .bind(scope)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(LocalEqualizerState {
        default_profile_id,
        profiles: load_profiles(pool, scope, false).await?,
        device_rules: load_rules(pool, scope, false).await?,
    })
}

async fn load_profiles(
    pool: &SqlitePool,
    scope: &str,
    synced: bool,
) -> AppResult<Vec<EqualizerProfile>> {
    let table = if synced {
        "equalizer_synced_profiles"
    } else {
        "equalizer_local_profiles"
    };
    let revision = if synced {
        "revision"
    } else {
        "NULL AS revision"
    };
    let sql = format!(
        "SELECT id, name, format_version, preamp_db, auto_headroom, {revision}, created_at, updated_at \
         FROM {table} WHERE account_scope = ?1 {} ORDER BY name_key, id",
        if synced { "AND supported_v1 = 1" } else { "" }
    );
    let rows = sqlx::query_as::<_, ProfileRow>(&sql)
        .bind(scope)
        .fetch_all(pool)
        .await?;
    let mut profiles = Vec::with_capacity(rows.len());
    for row in rows {
        let bands_table = if synced {
            "equalizer_synced_bands"
        } else {
            "equalizer_local_bands"
        };
        let bands_sql = format!(
            "SELECT position, enabled, filter_kind, frequency_hz, gain_db, q FROM {bands_table} \
             WHERE account_scope = ?1 AND profile_id = ?2 ORDER BY position"
        );
        let band_rows = sqlx::query_as::<_, BandRow>(&bands_sql)
            .bind(scope)
            .bind(&row.id)
            .fetch_all(pool)
            .await?;
        let bands = band_rows
            .into_iter()
            .map(band_from_row)
            .collect::<AppResult<Vec<_>>>()?;
        profiles.push(EqualizerProfile {
            id: row.id,
            name: row.name,
            format_version: row.format_version as u32,
            preamp_db: row.preamp_db,
            auto_headroom_enabled: row.auto_headroom != 0,
            bands,
            revision: row
                .revision
                .as_deref()
                .map(parse_revision)
                .transpose()?
                .unwrap_or_default(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        });
    }
    Ok(profiles)
}

async fn load_rules(
    pool: &SqlitePool,
    scope: &str,
    synced: bool,
) -> AppResult<Vec<EqualizerDeviceRule>> {
    let table = if synced {
        "equalizer_synced_device_rules"
    } else {
        "equalizer_local_device_rules"
    };
    let revision = if synced {
        "revision"
    } else {
        "NULL AS revision"
    };
    let sql = format!(
        "SELECT id, label, action, profile_id, selectors_json, priority, enabled, \
         bass_boost_percent, treble_boost_percent, {revision} \
         FROM {table} WHERE account_scope = ?1 {} ORDER BY priority DESC, id",
        if synced { "AND supported_v1 = 1" } else { "" }
    );
    let rows = sqlx::query_as::<_, RuleRow>(&sql)
        .bind(scope)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(rule_from_row).collect()
}

pub async fn replace_synced_state(
    pool: &SqlitePool,
    scope: &str,
    state: &EqualizerState,
    etag: Option<&str>,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM equalizer_synced_device_rules WHERE account_scope = ?1")
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM equalizer_synced_user_settings WHERE account_scope = ?1")
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM equalizer_synced_profiles WHERE account_scope = ?1")
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    for profile in &state.profiles {
        insert_synced_profile(&mut tx, scope, profile).await?;
    }
    sqlx::query(
        "INSERT INTO equalizer_synced_user_settings \
         (account_scope, state_format_version, state_revision, settings_revision, default_profile_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(scope).bind(i64::from(state.state_format_version))
    .bind(state.state_revision.to_string()).bind(state.settings_revision.to_string())
    .bind(&state.default_profile_id).execute(&mut *tx).await?;
    for rule in &state.device_rules {
        insert_synced_rule(&mut tx, scope, rule).await?;
    }
    sqlx::query(
        "INSERT INTO equalizer_sync_state \
         (account_scope, state_revision, etag, has_complete_snapshot, support_state, synced_at, \
          last_probe_at, last_probe_app_version) VALUES (?1, ?2, ?3, 1, 'supported', ?4, ?4, ?5) \
         ON CONFLICT(account_scope) DO UPDATE SET state_revision=excluded.state_revision, \
         etag=excluded.etag, has_complete_snapshot=1, support_state='supported', \
         synced_at=excluded.synced_at, last_probe_at=excluded.last_probe_at, \
         last_probe_app_version=excluded.last_probe_app_version",
    )
    .bind(scope)
    .bind(state.state_revision.to_string())
    .bind(etag)
    .bind(now_string())
    .bind(env!("CARGO_PKG_VERSION"))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_synced_profile(
    tx: &mut Transaction<'_, Sqlite>,
    scope: &str,
    profile: &EqualizerProfile,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO equalizer_synced_profiles \
         (account_scope,id,name,name_key,format_version,preamp_db,auto_headroom,revision,created_at,updated_at,supported_v1) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1)",
    ).bind(scope).bind(&profile.id).bind(&profile.name).bind(name_key(&profile.name))
      .bind(i64::from(profile.format_version)).bind(profile.preamp_db)
      .bind(i64::from(profile.auto_headroom_enabled)).bind(profile.revision.to_string())
      .bind(&profile.created_at).bind(&profile.updated_at).execute(&mut **tx).await?;
    for band in &profile.bands {
        insert_band(tx, "equalizer_synced_bands", scope, &profile.id, band).await?;
    }
    Ok(())
}

async fn insert_synced_rule(
    tx: &mut Transaction<'_, Sqlite>,
    scope: &str,
    rule: &EqualizerDeviceRule,
) -> AppResult<()> {
    let (action, profile_id) = action_columns(&rule.action);
    let selectors = serde_json::to_string(&rule.selectors)
        .map_err(|e| AppError::Internal(format!("encode EQ selectors: {e}")))?;
    sqlx::query(
        "INSERT INTO equalizer_synced_device_rules \
         (account_scope,id,label,action,profile_id,selectors_json,priority,enabled, \
          bass_boost_percent,treble_boost_percent,revision,supported_v1) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1)",
    )
    .bind(scope)
    .bind(&rule.id)
    .bind(&rule.label)
    .bind(action)
    .bind(profile_id)
    .bind(selectors)
    .bind(rule.priority)
    .bind(i64::from(rule.enabled))
    .bind(i64::from(rule.bass_boost_percent))
    .bind(i64::from(rule.treble_boost_percent))
    .bind(rule.revision.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn upsert_local_profile(
    pool: &SqlitePool,
    scope: &str,
    profile: &EqualizerProfile,
) -> AppResult<()> {
    validate_profile(profile).map_err(|e| AppError::Internal(e.message))?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO equalizer_local_profiles \
         (account_scope,id,name,name_key,format_version,preamp_db,auto_headroom,created_at,updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(account_scope,id) DO UPDATE SET \
         name=excluded.name,name_key=excluded.name_key,format_version=excluded.format_version, \
         preamp_db=excluded.preamp_db,auto_headroom=excluded.auto_headroom,updated_at=excluded.updated_at",
    ).bind(scope).bind(&profile.id).bind(&profile.name).bind(name_key(&profile.name))
      .bind(i64::from(profile.format_version)).bind(profile.preamp_db)
      .bind(i64::from(profile.auto_headroom_enabled)).bind(&profile.created_at)
      .bind(&profile.updated_at).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM equalizer_local_bands WHERE account_scope=?1 AND profile_id=?2")
        .bind(scope)
        .bind(&profile.id)
        .execute(&mut *tx)
        .await?;
    for band in &profile.bands {
        insert_band(&mut tx, "equalizer_local_bands", scope, &profile.id, band).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete_local_profile(
    pool: &SqlitePool,
    scope: &str,
    profile_id: &str,
    replacement_profile_id: Option<&str>,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE equalizer_local_user_settings SET default_profile_id=?3,updated_at=?4 \
         WHERE account_scope=?1 AND default_profile_id=?2",
    )
    .bind(scope)
    .bind(profile_id)
    .bind(replacement_profile_id)
    .bind(now_string())
    .execute(&mut *tx)
    .await?;
    if let Some(replacement) = replacement_profile_id {
        sqlx::query(
            "UPDATE equalizer_local_device_rules SET profile_id=?3,updated_at=?4 \
             WHERE account_scope=?1 AND profile_id=?2",
        )
        .bind(scope)
        .bind(profile_id)
        .bind(replacement)
        .bind(now_string())
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE equalizer_local_device_rules SET action='bypass',profile_id=NULL,updated_at=?3 \
             WHERE account_scope=?1 AND profile_id=?2",
        )
        .bind(scope)
        .bind(profile_id)
        .bind(now_string())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM equalizer_local_profiles WHERE account_scope=?1 AND id=?2")
        .bind(scope)
        .bind(profile_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_local_default(
    pool: &SqlitePool,
    scope: &str,
    profile_id: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO equalizer_local_user_settings (account_scope,default_profile_id,updated_at) \
         VALUES (?1,?2,?3) ON CONFLICT(account_scope) DO UPDATE SET \
         default_profile_id=excluded.default_profile_id,updated_at=excluded.updated_at",
    )
    .bind(scope)
    .bind(profile_id)
    .bind(now_string())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_local_rule(
    pool: &SqlitePool,
    scope: &str,
    rule: &EqualizerDeviceRule,
) -> AppResult<()> {
    validate_rule(rule).map_err(|e| AppError::Internal(e.message))?;
    let (action, profile_id) = action_columns(&rule.action);
    let selectors = serde_json::to_string(&rule.selectors)
        .map_err(|e| AppError::Internal(format!("encode EQ selectors: {e}")))?;
    let now = now_string();
    sqlx::query(
        "INSERT INTO equalizer_local_device_rules \
         (account_scope,id,label,action,profile_id,selectors_json,priority,enabled, \
          bass_boost_percent,treble_boost_percent,created_at,updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11) ON CONFLICT(account_scope,id) DO UPDATE SET \
         label=excluded.label,action=excluded.action,profile_id=excluded.profile_id, \
         selectors_json=excluded.selectors_json,priority=excluded.priority, \
         enabled=excluded.enabled,bass_boost_percent=excluded.bass_boost_percent, \
         treble_boost_percent=excluded.treble_boost_percent,updated_at=excluded.updated_at",
    ).bind(scope).bind(&rule.id).bind(&rule.label).bind(action).bind(profile_id)
      .bind(selectors).bind(rule.priority).bind(i64::from(rule.enabled))
      .bind(i64::from(rule.bass_boost_percent)).bind(i64::from(rule.treble_boost_percent)).bind(now)
      .execute(pool).await?;
    Ok(())
}

pub async fn delete_local_rule(pool: &SqlitePool, scope: &str, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM equalizer_local_device_rules WHERE account_scope=?1 AND id=?2")
        .bind(scope)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Merge the last acknowledged server snapshot into the device-local layer
/// before a supported server is downgraded/rolled back. Existing local rows
/// are retained; equivalent rows are reused and collisions receive recovery
/// UUIDs/names. The local default and exact bindings are remapped atomically so
/// the previously effective configuration remains playable.
pub async fn preserve_synced_state_as_local(
    pool: &SqlitePool,
    scope: &str,
    synced: &EqualizerState,
) -> AppResult<EqualizerRecoveryMap> {
    let local = load_local_state(pool, scope).await?;
    let mut used_profile_ids = local
        .profiles
        .iter()
        .map(|profile| profile.id.clone())
        .collect::<HashSet<_>>();
    let mut used_name_keys = local
        .profiles
        .iter()
        .map(|profile| name_key(&profile.name))
        .collect::<HashSet<_>>();
    let mut profile_map = HashMap::<String, String>::new();
    let mut profiles_to_insert = Vec::<EqualizerProfile>::new();
    let now = now_string();

    for server_profile in &synced.profiles {
        if let Some(existing) = local.profiles.iter().find(|profile| {
            (profile.id == server_profile.id
                || name_key(&profile.name) == name_key(&server_profile.name))
                && profile_payload_eq(profile, server_profile)
        }) {
            profile_map.insert(server_profile.id.clone(), existing.id.clone());
            continue;
        }

        let (id, reused_recovery) = if used_profile_ids.insert(server_profile.id.clone()) {
            (server_profile.id.clone(), false)
        } else {
            let mut salt = 0_u32;
            loop {
                let candidate = recovery_profile_id(scope, &server_profile.id, salt);
                if let Some(existing) = local
                    .profiles
                    .iter()
                    .find(|profile| profile.id == candidate)
                {
                    if profile_payload_eq(existing, server_profile) {
                        break (existing.id.clone(), true);
                    }
                } else if used_profile_ids.insert(candidate.clone()) {
                    break (candidate, false);
                }
                salt = salt.saturating_add(1);
            }
        };
        if reused_recovery {
            profile_map.insert(server_profile.id.clone(), id);
            continue;
        }
        let name = unique_recovery_name(&server_profile.name, &mut used_name_keys);
        let mut recovered = server_profile.clone();
        recovered.id = id.clone();
        recovered.name = name;
        recovered.revision = Revision(0);
        recovered.created_at = now.clone();
        recovered.updated_at = now.clone();
        validate_profile(&recovered).map_err(|error| AppError::Internal(error.message))?;
        profile_map.insert(server_profile.id.clone(), id);
        profiles_to_insert.push(recovered);
    }

    let mut used_rule_ids = local
        .device_rules
        .iter()
        .map(|rule| rule.id.clone())
        .collect::<HashSet<_>>();
    let mut rules_to_insert = Vec::<EqualizerDeviceRule>::new();
    let mut rule_priority_updates = Vec::<(String, i32)>::new();
    let mut rule_map = HashMap::<String, String>::new();
    for (index, server_rule) in synced.device_rules.iter().enumerate() {
        let mut recovered = server_rule.clone();
        if let RuleAction::Profile { profile_id } = &mut recovered.action {
            let Some(mapped) = profile_map.get(profile_id) else {
                continue;
            };
            *profile_id = mapped.clone();
        }
        recovered.priority = i32::MAX.saturating_sub(index as i32);
        recovered.revision = Revision(0);

        if let Some(existing) = local
            .device_rules
            .iter()
            .find(|rule| rule_payload_eq(rule, &recovered))
        {
            rule_priority_updates.push((existing.id.clone(), recovered.priority));
            rule_map.insert(server_rule.id.clone(), existing.id.clone());
            continue;
        }
        if !used_rule_ids.insert(recovered.id.clone()) {
            loop {
                let candidate = uuid::Uuid::new_v4().to_string();
                if used_rule_ids.insert(candidate.clone()) {
                    recovered.id = candidate;
                    break;
                }
            }
        }
        validate_rule(&recovered).map_err(|error| AppError::Internal(error.message))?;
        rule_map.insert(server_rule.id.clone(), recovered.id.clone());
        rules_to_insert.push(recovered);
    }

    let recovered_default = synced
        .default_profile_id
        .as_ref()
        .and_then(|id| profile_map.get(id))
        .cloned();
    let mut tx = pool.begin().await?;
    for profile in &profiles_to_insert {
        sqlx::query(
            "INSERT INTO equalizer_local_profiles \
             (account_scope,id,name,name_key,format_version,preamp_db,auto_headroom,created_at,updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .bind(scope)
        .bind(&profile.id)
        .bind(&profile.name)
        .bind(name_key(&profile.name))
        .bind(i64::from(profile.format_version))
        .bind(profile.preamp_db)
        .bind(i64::from(profile.auto_headroom_enabled))
        .bind(&profile.created_at)
        .bind(&profile.updated_at)
        .execute(&mut *tx)
        .await?;
        for band in &profile.bands {
            insert_band(&mut tx, "equalizer_local_bands", scope, &profile.id, band).await?;
        }
    }
    for (id, priority) in rule_priority_updates {
        sqlx::query(
            "UPDATE equalizer_local_device_rules SET priority=?3,updated_at=?4 \
             WHERE account_scope=?1 AND id=?2",
        )
        .bind(scope)
        .bind(id)
        .bind(priority)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    for rule in &rules_to_insert {
        let (action, profile_id) = action_columns(&rule.action);
        let selectors = serde_json::to_string(&rule.selectors)
            .map_err(|error| AppError::Internal(format!("encode EQ selectors: {error}")))?;
        sqlx::query(
            "INSERT INTO equalizer_local_device_rules \
             (account_scope,id,label,action,profile_id,selectors_json,priority,enabled, \
              bass_boost_percent,treble_boost_percent,created_at,updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
        )
        .bind(scope)
        .bind(&rule.id)
        .bind(&rule.label)
        .bind(action)
        .bind(profile_id)
        .bind(selectors)
        .bind(rule.priority)
        .bind(i64::from(rule.enabled))
        .bind(i64::from(rule.bass_boost_percent))
        .bind(i64::from(rule.treble_boost_percent))
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO equalizer_local_user_settings (account_scope,default_profile_id,updated_at) \
         VALUES (?1,?2,?3) ON CONFLICT(account_scope) DO UPDATE SET \
         default_profile_id=excluded.default_profile_id,updated_at=excluded.updated_at",
    )
    .bind(scope)
    .bind(recovered_default)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE equalizer_local_device_overrides SET target_layer='local_only',updated_at=?2 \
         WHERE account_scope=?1 AND target_layer='synced' AND action='bypass'",
    )
    .bind(scope)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    for (server_id, local_id) in &profile_map {
        sqlx::query(
            "UPDATE equalizer_local_device_overrides SET target_layer='local_only',profile_id=?3, \
             orphaned=0,updated_at=?4 WHERE account_scope=?1 AND target_layer='synced' AND profile_id=?2",
        )
        .bind(scope)
        .bind(server_id)
        .bind(local_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(EqualizerRecoveryMap {
        profile_ids: profile_map,
        rule_ids: rule_map,
    })
}

pub async fn list_exact_bindings(pool: &SqlitePool, scope: &str) -> AppResult<Vec<ExactBinding>> {
    let rows = sqlx::query(
        "SELECT endpoint_key,display_label,target_layer,action,profile_id,orphaned \
         FROM equalizer_local_device_overrides WHERE account_scope=?1 ORDER BY display_label,endpoint_key",
    ).bind(scope).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            let layer = match row.get::<String, _>("target_layer").as_str() {
                "synced" => ProfileLayer::Synced,
                "local_only" => ProfileLayer::LocalOnly,
                other => {
                    return Err(AppError::Database(format!(
                        "invalid EQ target layer {other}"
                    )));
                }
            };
            let action =
                action_from_columns(&row.get::<String, _>("action"), row.get("profile_id"))?;
            Ok(ExactBinding {
                endpoint_key: row.get("endpoint_key"),
                display_label: row.get("display_label"),
                target: ProfileTarget { layer, action },
                orphaned: row.get::<i64, _>("orphaned") != 0,
            })
        })
        .collect()
}

pub async fn upsert_exact_binding(
    pool: &SqlitePool,
    scope: &str,
    binding: &ExactBinding,
) -> AppResult<()> {
    let (action, profile_id) = action_columns(&binding.target.action);
    let layer = match binding.target.layer {
        ProfileLayer::Synced => "synced",
        ProfileLayer::LocalOnly => "local_only",
    };
    sqlx::query(
        "INSERT INTO equalizer_local_device_overrides \
         (account_scope,endpoint_key,display_label,target_layer,action,profile_id,orphaned,updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(account_scope,endpoint_key) DO UPDATE SET \
         display_label=excluded.display_label,target_layer=excluded.target_layer,action=excluded.action, \
         profile_id=excluded.profile_id,orphaned=excluded.orphaned,updated_at=excluded.updated_at",
    ).bind(scope).bind(&binding.endpoint_key).bind(&binding.display_label).bind(layer).bind(action)
      .bind(profile_id).bind(i64::from(binding.orphaned)).bind(now_string()).execute(pool).await?;
    Ok(())
}

pub async fn delete_exact_binding(
    pool: &SqlitePool,
    scope: &str,
    endpoint_key: &str,
) -> AppResult<()> {
    sqlx::query(
        "DELETE FROM equalizer_local_device_overrides WHERE account_scope=?1 AND endpoint_key=?2",
    )
    .bind(scope)
    .bind(endpoint_key)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remap_exact_profile_bindings(
    pool: &SqlitePool,
    scope: &str,
    from_layer: ProfileLayer,
    from_profile_id: &str,
    to_layer: ProfileLayer,
    to_profile_id: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE equalizer_local_device_overrides SET target_layer=?4,profile_id=?5, \
         orphaned=0,updated_at=?6 WHERE account_scope=?1 AND target_layer=?2 AND profile_id=?3",
    )
    .bind(scope)
    .bind(layer_string(from_layer))
    .bind(from_profile_id)
    .bind(layer_string(to_layer))
    .bind(to_profile_id)
    .bind(now_string())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn enqueue_op(
    pool: &SqlitePool,
    scope: &str,
    op: &PendingEqualizerOpKind,
) -> AppResult<i64> {
    let mut tx = pool.begin().await?;
    let proposed_group = op.dependency_group();
    let dependencies = op
        .dependency_entity_ids()
        .into_iter()
        .collect::<HashSet<_>>();
    let rows = sqlx::query(
        "SELECT id,dependency_group,payload_json FROM pending_equalizer_ops \
         WHERE account_scope=?1 ORDER BY id",
    )
    .bind(scope)
    .fetch_all(&mut *tx)
    .await?;
    let mut matched = Vec::<(i64, String)>::new();
    for row in rows {
        let id = row.get::<i64, _>("id");
        let group = row.get::<String, _>("dependency_group");
        let payload = row.get::<String, _>("payload_json");
        let existing = PendingEqualizerOpKind::from_json(&payload)?;
        if group == proposed_group
            || existing
                .dependency_entity_ids()
                .into_iter()
                .any(|entity| dependencies.contains(entity))
        {
            matched.push((id, group));
        }
    }
    let dependency_group = matched
        .iter()
        .min_by_key(|(id, _)| *id)
        .map(|(_, group)| group.clone())
        .unwrap_or(proposed_group);
    let matched_groups = matched
        .into_iter()
        .map(|(_, group)| group)
        .collect::<HashSet<_>>();
    for group in matched_groups {
        if group == dependency_group {
            continue;
        }
        sqlx::query(
            "UPDATE pending_equalizer_ops SET dependency_group=?3 \
             WHERE account_scope=?1 AND dependency_group=?2",
        )
        .bind(scope)
        .bind(group)
        .bind(&dependency_group)
        .execute(&mut *tx)
        .await?;
    }
    let result = sqlx::query(
        "INSERT INTO pending_equalizer_ops \
         (operation_uuid,account_scope,op_type,entity_id,base_revision,dependency_group,payload_json) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
    ).bind(uuid::Uuid::new_v4().to_string()).bind(scope).bind(op.op_type())
      .bind(op.entity_id()).bind(op.base_revision().map(|r| r.to_string()))
      .bind(&dependency_group).bind(op.to_json()?).execute(&mut *tx).await?;
    let id = result.last_insert_rowid();
    tx.commit().await?;
    Ok(id)
}

pub async fn list_pending_ops(
    pool: &SqlitePool,
    scope: &str,
) -> AppResult<Vec<PendingEqualizerOp>> {
    let rows = sqlx::query_as::<_, PendingRow>(
        "SELECT id,operation_uuid,account_scope,op_type,entity_id,base_revision,dependency_group, \
         payload_json,created_at,attempts,last_error FROM pending_equalizer_ops \
         WHERE account_scope=?1 ORDER BY id",
    )
    .bind(scope)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(pending_from_row).collect()
}

pub async fn count_pending_ops(pool: &SqlitePool, scope: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM pending_equalizer_ops WHERE account_scope=?1")
            .bind(scope)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn mark_op_failed(pool: &SqlitePool, id: i64, error: &str) -> AppResult<()> {
    sqlx::query("UPDATE pending_equalizer_ops SET attempts=attempts+1,last_error=?2 WHERE id=?1")
        .bind(id)
        .bind(error)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_pending_op(pool: &SqlitePool, scope: &str, id: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM pending_equalizer_ops WHERE account_scope=?1 AND id=?2")
        .bind(scope)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Atomically installs the authoritative response, removes the acknowledged
/// FIFO row, and rebases later rows. This closes the crash window where a
/// server-applied operation could otherwise remain queued locally.
pub async fn replace_synced_state_and_acknowledge(
    pool: &SqlitePool,
    scope: &str,
    acknowledged_id: i64,
    response: &EqualizerState,
    etag: Option<&str>,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM equalizer_synced_device_rules WHERE account_scope = ?1")
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM equalizer_synced_user_settings WHERE account_scope = ?1")
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM equalizer_synced_profiles WHERE account_scope = ?1")
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    for profile in &response.profiles {
        insert_synced_profile(&mut tx, scope, profile).await?;
    }
    sqlx::query(
        "INSERT INTO equalizer_synced_user_settings \
         (account_scope,state_format_version,state_revision,settings_revision,default_profile_id) \
         VALUES (?1,?2,?3,?4,?5)",
    )
    .bind(scope)
    .bind(i64::from(response.state_format_version))
    .bind(response.state_revision.to_string())
    .bind(response.settings_revision.to_string())
    .bind(&response.default_profile_id)
    .execute(&mut *tx)
    .await?;
    for rule in &response.device_rules {
        insert_synced_rule(&mut tx, scope, rule).await?;
    }
    sqlx::query(
        "INSERT INTO equalizer_sync_state \
         (account_scope,state_revision,etag,has_complete_snapshot,support_state,synced_at, \
          last_probe_at,last_probe_app_version) VALUES (?1,?2,?3,1,'supported',?4,?4,?5) \
         ON CONFLICT(account_scope) DO UPDATE SET state_revision=excluded.state_revision, \
         etag=excluded.etag,has_complete_snapshot=1,support_state='supported', \
         synced_at=excluded.synced_at,last_probe_at=excluded.last_probe_at, \
         last_probe_app_version=excluded.last_probe_app_version",
    )
    .bind(scope)
    .bind(response.state_revision.to_string())
    .bind(etag)
    .bind(now_string())
    .bind(env!("CARGO_PKG_VERSION"))
    .execute(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM pending_equalizer_ops WHERE id=?1 AND account_scope=?2")
        .bind(acknowledged_id)
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query_as::<_, PendingRow>(
        "SELECT id,operation_uuid,account_scope,op_type,entity_id,base_revision,dependency_group, \
         payload_json,created_at,attempts,last_error FROM pending_equalizer_ops \
         WHERE account_scope=?1 AND id>?2 ORDER BY id",
    )
    .bind(scope)
    .bind(acknowledged_id)
    .fetch_all(&mut *tx)
    .await?;
    for row in rows {
        let mut op = PendingEqualizerOpKind::from_json(&row.payload_json)?;
        op.rebase(response);
        sqlx::query(
            "UPDATE pending_equalizer_ops SET payload_json=?2,base_revision=?3 WHERE id=?1",
        )
        .bind(row.id)
        .bind(op.to_json()?)
        .bind(op.base_revision().map(|revision| revision.to_string()))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn acknowledge_and_rebase(
    pool: &SqlitePool,
    scope: &str,
    acknowledged_id: i64,
    response: &EqualizerState,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM pending_equalizer_ops WHERE id=?1 AND account_scope=?2")
        .bind(acknowledged_id)
        .bind(scope)
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query_as::<_, PendingRow>(
        "SELECT id,operation_uuid,account_scope,op_type,entity_id,base_revision,dependency_group, \
         payload_json,created_at,attempts,last_error FROM pending_equalizer_ops \
         WHERE account_scope=?1 AND id>?2 ORDER BY id",
    )
    .bind(scope)
    .bind(acknowledged_id)
    .fetch_all(&mut *tx)
    .await?;
    for row in rows {
        let mut op = PendingEqualizerOpKind::from_json(&row.payload_json)?;
        op.rebase(response);
        sqlx::query(
            "UPDATE pending_equalizer_ops SET payload_json=?2,base_revision=?3 WHERE id=?1",
        )
        .bind(row.id)
        .bind(op.to_json()?)
        .bind(op.base_revision().map(|r| r.to_string()))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn move_dependency_to_conflicts(
    pool: &SqlitePool,
    scope: &str,
    from_id: i64,
    dependency_group: &str,
    code: &str,
    message: &str,
    server_revision: Option<Revision>,
) -> AppResult<i64> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query_as::<_, PendingRow>(
        "SELECT id,operation_uuid,account_scope,op_type,entity_id,base_revision,dependency_group, \
         payload_json,created_at,attempts,last_error FROM pending_equalizer_ops \
         WHERE account_scope=?1 AND dependency_group=?2 AND id>=?3 ORDER BY id",
    )
    .bind(scope)
    .bind(dependency_group)
    .bind(from_id)
    .fetch_all(&mut *tx)
    .await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO equalizer_conflicts \
             (account_scope,dependency_group,op_type,entity_id,payload_json,base_revision,server_revision, \
              error_code,error_message) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        ).bind(scope).bind(&row.dependency_group).bind(&row.op_type).bind(&row.entity_id)
          .bind(&row.payload_json).bind(&row.base_revision).bind(server_revision.map(|r| r.to_string()))
          .bind(code).bind(message).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM pending_equalizer_ops WHERE id=?1")
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(rows.len() as i64)
}

pub async fn list_conflicts(pool: &SqlitePool, scope: &str) -> AppResult<Vec<EqualizerConflict>> {
    let rows = sqlx::query(
        "SELECT id,dependency_group,op_type,entity_id,payload_json,base_revision,server_revision, \
         error_code,error_message,created_at FROM equalizer_conflicts WHERE account_scope=?1 ORDER BY id",
    ).bind(scope).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(EqualizerConflict {
                id: row.get("id"),
                dependency_group: row.get("dependency_group"),
                op_type: row.get("op_type"),
                entity_id: row.get("entity_id"),
                payload_json: row.get("payload_json"),
                base_revision: row
                    .get::<Option<String>, _>("base_revision")
                    .as_deref()
                    .map(parse_revision)
                    .transpose()?,
                server_revision: row
                    .get::<Option<String>, _>("server_revision")
                    .as_deref()
                    .map(parse_revision)
                    .transpose()?,
                error_code: row.get("error_code"),
                error_message: row.get("error_message"),
                created_at: row.get("created_at"),
            })
        })
        .collect()
}

pub async fn delete_conflict(pool: &SqlitePool, scope: &str, id: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM equalizer_conflicts WHERE account_scope=?1 AND id=?2")
        .bind(scope)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn count_conflicts(pool: &SqlitePool, scope: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM equalizer_conflicts WHERE account_scope=?1")
            .bind(scope)
            .fetch_one(pool)
            .await?,
    )
}

async fn insert_band(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    scope: &str,
    profile_id: &str,
    band: &EqualizerBand,
) -> AppResult<()> {
    let sql = format!(
        "INSERT INTO {table} \
         (account_scope,profile_id,position,enabled,filter_kind,frequency_hz,gain_db,q) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"
    );
    sqlx::query(&sql)
        .bind(scope)
        .bind(profile_id)
        .bind(i64::from(band.position))
        .bind(i64::from(band.enabled))
        .bind(band.filter_kind.as_str())
        .bind(band.frequency_hz)
        .bind(band.gain_db)
        .bind(band.q)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn profile_payload_eq(left: &EqualizerProfile, right: &EqualizerProfile) -> bool {
    left.format_version == right.format_version
        && left.preamp_db == right.preamp_db
        && left.auto_headroom_enabled == right.auto_headroom_enabled
        && left.bands == right.bands
}

fn recovery_profile_id(scope: &str, server_profile_id: &str, salt: u32) -> String {
    let digest = Sha256::digest(format!(
        "octave.equalizer.recovery.v1\0{scope}\0{server_profile_id}\0{salt}"
    ));
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn rule_payload_eq(left: &EqualizerDeviceRule, right: &EqualizerDeviceRule) -> bool {
    left.label == right.label
        && left.action == right.action
        && left.selectors == right.selectors
        && left.enabled == right.enabled
        && left.bass_boost_percent == right.bass_boost_percent
        && left.treble_boost_percent == right.treble_boost_percent
}

fn unique_recovery_name(base: &str, used: &mut HashSet<String>) -> String {
    let base = base.trim();
    if used.insert(name_key(base)) {
        return base.to_string();
    }
    for number in 1..=999 {
        let marker = if number == 1 {
            " (server recovery)".to_string()
        } else {
            format!(" (server recovery {number})")
        };
        let keep = MAX_NAME_CHARS.saturating_sub(marker.chars().count());
        let candidate = format!("{}{}", base.chars().take(keep).collect::<String>(), marker);
        if used.insert(name_key(&candidate)) {
            return candidate;
        }
    }
    loop {
        let candidate = format!("Recovered {}", uuid::Uuid::new_v4().simple())
            .chars()
            .take(MAX_NAME_CHARS)
            .collect::<String>();
        if used.insert(name_key(&candidate)) {
            return candidate;
        }
    }
}

fn band_from_row(row: BandRow) -> AppResult<EqualizerBand> {
    Ok(EqualizerBand {
        position: u32::try_from(row.position)
            .map_err(|_| AppError::Database("invalid EQ position".into()))?,
        enabled: row.enabled != 0,
        filter_kind: row
            .filter_kind
            .parse()
            .map_err(|e: EqualizerValidationError| AppError::Database(e.message))?,
        frequency_hz: row.frequency_hz,
        gain_db: row.gain_db,
        q: row.q,
    })
}

fn rule_from_row(row: RuleRow) -> AppResult<EqualizerDeviceRule> {
    Ok(EqualizerDeviceRule {
        id: row.id,
        label: row.label,
        action: action_from_columns(&row.action, row.profile_id)?,
        selectors: serde_json::from_str(&row.selectors_json)
            .map_err(|e| AppError::Database(format!("decode EQ selectors: {e}")))?,
        priority: row.priority,
        enabled: row.enabled != 0,
        bass_boost_percent: u32::try_from(row.bass_boost_percent)
            .map_err(|_| AppError::Database("invalid EQ bass boost percentage".into()))?,
        treble_boost_percent: u32::try_from(row.treble_boost_percent)
            .map_err(|_| AppError::Database("invalid EQ treble boost percentage".into()))?,
        revision: row
            .revision
            .as_deref()
            .map(parse_revision)
            .transpose()?
            .unwrap_or_default(),
    })
}

fn pending_from_row(row: PendingRow) -> AppResult<PendingEqualizerOp> {
    Ok(PendingEqualizerOp {
        id: row.id,
        operation_uuid: row.operation_uuid,
        account_scope: row.account_scope,
        op_type: row.op_type,
        entity_id: row.entity_id,
        base_revision: row
            .base_revision
            .as_deref()
            .map(parse_revision)
            .transpose()?,
        dependency_group: row.dependency_group,
        payload_json: row.payload_json,
        created_at: row.created_at,
        attempts: row.attempts,
        last_error: row.last_error,
    })
}

fn action_columns(action: &RuleAction) -> (&'static str, Option<&str>) {
    match action {
        RuleAction::Profile { profile_id } => ("profile", Some(profile_id)),
        RuleAction::Bypass => ("bypass", None),
    }
}

fn layer_string(layer: ProfileLayer) -> &'static str {
    match layer {
        ProfileLayer::Synced => "synced",
        ProfileLayer::LocalOnly => "local_only",
    }
}

fn action_from_columns(action: &str, profile_id: Option<String>) -> AppResult<RuleAction> {
    match (action, profile_id) {
        ("profile", Some(profile_id)) => Ok(RuleAction::Profile { profile_id }),
        ("bypass", None) => Ok(RuleAction::Bypass),
        _ => Err(AppError::Database(
            "invalid EQ rule action/reference".into(),
        )),
    }
}

fn parse_revision(value: &str) -> AppResult<Revision> {
    value
        .parse()
        .map_err(|e| AppError::Database(format!("invalid EQ revision '{value}': {e}")))
}

fn support_string(value: SupportState) -> &'static str {
    match value {
        SupportState::Unknown => "unknown",
        SupportState::Supported => "supported",
        SupportState::Unsupported => "unsupported",
        SupportState::FutureFormat => "future_format",
    }
}

fn parse_support(value: &str) -> AppResult<SupportState> {
    match value {
        "unknown" => Ok(SupportState::Unknown),
        "supported" => Ok(SupportState::Supported),
        "unsupported" => Ok(SupportState::Unsupported),
        "future_format" => Ok(SupportState::FutureFormat),
        other => Err(AppError::Database(format!(
            "invalid EQ support state {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn pool() -> SqlitePool {
        db::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn local_and_synced_layers_do_not_overwrite_each_other() {
        let pool = pool().await;
        let local = EqualizerProfile::five_band_starter("Local");
        upsert_local_profile(&pool, "scope", &local).await.unwrap();
        let mut synced = EqualizerState::default();
        let mut remote = EqualizerProfile::five_band_starter("Remote");
        remote.revision = Revision(3);
        synced.profiles.push(remote);
        synced.state_revision = Revision(4);
        replace_synced_state(&pool, "scope", &synced, Some("eq-4"))
            .await
            .unwrap();
        assert_eq!(
            load_local_state(&pool, "scope")
                .await
                .unwrap()
                .profiles
                .len(),
            1
        );
        assert_eq!(
            load_synced_state(&pool, "scope")
                .await
                .unwrap()
                .profiles
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn pending_rows_are_scope_partitioned_and_materialized() {
        let pool = pool().await;
        let profile = EqualizerProfile::five_band_starter("Queued");
        enqueue_op(
            &pool,
            "a",
            &PendingEqualizerOpKind::ProfileCreate {
                profile: (&profile).into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(count_pending_ops(&pool, "a").await.unwrap(), 1);
        assert_eq!(count_pending_ops(&pool, "b").await.unwrap(), 0);
        assert_eq!(
            load_materialized_synced_state(&pool, "a")
                .await
                .unwrap()
                .profiles
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn acknowledge_installs_mirror_and_rebases_tail_in_one_commit() {
        let pool = pool().await;
        let mut profile = EqualizerProfile::five_band_starter("Queued");
        let first = enqueue_op(
            &pool,
            "scope",
            &PendingEqualizerOpKind::ProfileCreate {
                profile: (&profile).into(),
            },
        )
        .await
        .unwrap();
        let mut changed: EqualizerProfileInput = (&profile).into();
        changed.name = "Later edit".into();
        enqueue_op(
            &pool,
            "scope",
            &PendingEqualizerOpKind::ProfileUpdate {
                profile_id: profile.id.clone(),
                expected_revision: Revision(0),
                profile: changed,
            },
        )
        .await
        .unwrap();

        profile.revision = Revision(7);
        let response = EqualizerState {
            state_revision: Revision(8),
            profiles: vec![profile],
            ..EqualizerState::default()
        };
        replace_synced_state_and_acknowledge(&pool, "scope", first, &response, Some("eq-8"))
            .await
            .unwrap();

        assert_eq!(load_synced_state(&pool, "scope").await.unwrap(), response);
        let tail = list_pending_ops(&pool, "scope").await.unwrap();
        assert_eq!(tail.len(), 1);
        let op = PendingEqualizerOpKind::from_json(&tail[0].payload_json).unwrap();
        assert_eq!(op.base_revision(), Some(Revision(7)));
    }

    #[tokio::test]
    async fn dependency_groups_union_rule_retargets_profiles_and_reorder() {
        let pool = pool().await;
        let profile_a = EqualizerProfile::five_band_starter("A");
        let profile_b = EqualizerProfile::five_band_starter("B");
        for profile in [&profile_a, &profile_b] {
            enqueue_op(
                &pool,
                "scope",
                &PendingEqualizerOpKind::ProfileCreate {
                    profile: profile.into(),
                },
            )
            .await
            .unwrap();
        }
        let rule_id = uuid::Uuid::new_v4().to_string();
        let make_rule = |profile_id: &str| EqualizerDeviceRuleInput {
            id: rule_id.clone(),
            label: "Headphones".into(),
            action: RuleAction::Profile {
                profile_id: profile_id.into(),
            },
            selectors: vec![PortableDeviceSelector {
                normalization_version: EQ_NORMALIZATION_VERSION,
                route_kind: RouteKind::Bluetooth,
                normalized_name: "headphones".into(),
                vendor_id: None,
                product_id: None,
                platform_scope: None,
                trigger: TriggerKind::ActiveOutput,
            }],
            enabled: true,
            bass_boost_percent: 0,
            treble_boost_percent: 0,
        };
        enqueue_op(
            &pool,
            "scope",
            &PendingEqualizerOpKind::RuleCreate {
                rule: make_rule(&profile_a.id),
            },
        )
        .await
        .unwrap();
        enqueue_op(
            &pool,
            "scope",
            &PendingEqualizerOpKind::RuleUpdate {
                rule_id: rule_id.clone(),
                expected_revision: Revision(0),
                rule: make_rule(&profile_b.id),
            },
        )
        .await
        .unwrap();
        enqueue_op(
            &pool,
            "scope",
            &PendingEqualizerOpKind::RuleReorder {
                rules: vec![EntityRevision {
                    id: rule_id,
                    expected_revision: Revision(0),
                }],
            },
        )
        .await
        .unwrap();

        let groups = list_pending_ops(&pool, "scope")
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.dependency_group)
            .collect::<HashSet<_>>();
        assert_eq!(
            groups.len(),
            1,
            "the full transitive chain must quarantine together"
        );
    }

    #[tokio::test]
    async fn downgrade_recovery_is_collision_safe_atomic_and_idempotent() {
        let pool = pool().await;
        let local = EqualizerProfile::five_band_starter("Shared name");
        upsert_local_profile(&pool, "scope", &local).await.unwrap();
        set_local_default(&pool, "scope", Some(&local.id))
            .await
            .unwrap();

        let mut remote = local.clone();
        remote.bands[0].gain_db = 3.0;
        remote.revision = Revision(4);
        let rule = EqualizerDeviceRule {
            id: uuid::Uuid::new_v4().to_string(),
            label: "Recovered output".into(),
            action: RuleAction::Profile {
                profile_id: remote.id.clone(),
            },
            selectors: vec![PortableDeviceSelector {
                normalization_version: EQ_NORMALIZATION_VERSION,
                route_kind: RouteKind::Usb,
                normalized_name: "usb dac".into(),
                vendor_id: None,
                product_id: None,
                platform_scope: None,
                trigger: TriggerKind::ActiveOutput,
            }],
            priority: 1,
            enabled: true,
            bass_boost_percent: 35,
            treble_boost_percent: 60,
            revision: Revision(2),
        };
        let synced = EqualizerState {
            state_revision: Revision(5),
            settings_revision: Revision(3),
            default_profile_id: Some(remote.id.clone()),
            profiles: vec![remote.clone()],
            device_rules: vec![rule],
            ..EqualizerState::default()
        };
        replace_synced_state(&pool, "scope", &synced, None)
            .await
            .unwrap();
        upsert_exact_binding(
            &pool,
            "scope",
            &ExactBinding {
                endpoint_key: "opaque-hmac".into(),
                display_label: Some("DAC".into()),
                target: ProfileTarget {
                    layer: ProfileLayer::Synced,
                    action: RuleAction::Profile {
                        profile_id: remote.id.clone(),
                    },
                },
                orphaned: false,
            },
        )
        .await
        .unwrap();

        preserve_synced_state_as_local(&pool, "scope", &synced)
            .await
            .unwrap();
        preserve_synced_state_as_local(&pool, "scope", &synced)
            .await
            .unwrap();

        let recovered = load_local_state(&pool, "scope").await.unwrap();
        assert_eq!(recovered.profiles.len(), 2);
        assert_eq!(recovered.device_rules.len(), 1);
        let recovered_id = recovered.default_profile_id.unwrap();
        assert_ne!(recovered_id, local.id);
        assert_eq!(
            recovered.device_rules[0].action.profile_id(),
            Some(recovered_id.as_str())
        );
        let bindings = list_exact_bindings(&pool, "scope").await.unwrap();
        assert_eq!(bindings[0].target.layer, ProfileLayer::LocalOnly);
        assert_eq!(
            bindings[0].target.action.profile_id(),
            Some(recovered_id.as_str())
        );
    }
}
