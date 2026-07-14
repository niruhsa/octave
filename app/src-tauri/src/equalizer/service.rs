//! Account-scoped local-first orchestration for equalizer state.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, RwLock};

use super::model::*;
use super::ops::PendingEqualizerOpKind;
use super::{parser, repo, resolver};
use crate::auth::{AuthManager, StoredCredentialKind};
use crate::error::{AppError, AppResult};

pub struct EqualizerService {
    pool: SqlitePool,
    auth: Arc<RwLock<Option<Arc<AuthManager>>>>,
    outputs: RwLock<Vec<AudioOutput>>,
    device_secret: RwLock<Option<Vec<u8>>>,
    last_resolved: RwLock<Option<ResolvedEqualizer>>,
    manual_override: RwLock<Option<ProfileTarget>>,
    current_scope: RwLock<String>,
    scope_epoch: AtomicI64,
    resolution_generation: AtomicI64,
    mutation_lock: Mutex<()>,
}

impl EqualizerService {
    /// Non-blocking constructor. The shared auth slot is the same one held by
    /// `AppStateHandle`, so login/server changes are observed immediately.
    pub fn new(pool: SqlitePool, auth: Arc<RwLock<Option<Arc<AuthManager>>>>) -> Self {
        Self {
            pool,
            auth,
            outputs: RwLock::new(Vec::new()),
            device_secret: RwLock::new(None),
            last_resolved: RwLock::new(None),
            manual_override: RwLock::new(None),
            current_scope: RwLock::new(String::new()),
            scope_epoch: AtomicI64::new(1),
            resolution_generation: AtomicI64::new(1),
            mutation_lock: Mutex::new(()),
        }
    }

    pub async fn current_outputs(&self) -> Vec<AudioOutput> {
        self.outputs.read().await.clone()
    }

    pub async fn update_outputs(
        &self,
        mut outputs: Vec<AudioOutput>,
    ) -> AppResult<ResolvedEqualizer> {
        let secret = self.device_secret().await?;
        for output in &mut outputs {
            output.display_name = output.display_name.trim().to_string();
            output.vendor_id = normalize_hardware_id(output.vendor_id.as_deref());
            output.product_id = normalize_hardware_id(output.product_id.as_deref());
            output.local_endpoint_key = output
                .runtime_id
                .as_deref()
                .map(|runtime_id| endpoint_hmac(&secret, runtime_id));
            // The raw platform id has served its only purpose. Never retain it
            // in service state, persistence, logs, events, or IPC payloads.
            output.runtime_id = None;
        }
        let mut current = self.outputs.write().await;
        if *current == outputs {
            if let Some(resolved) = self.last_resolved.read().await.clone() {
                return Ok(resolved);
            }
        } else {
            *self.manual_override.write().await = None;
            *current = outputs;
        }
        drop(current);
        self.resolve_current().await
    }

    pub async fn snapshot(&self) -> AppResult<EqualizerSnapshot> {
        let (scope, scope_epoch) = self.account_context().await;
        let sync = repo::get_sync_state(&self.pool, &scope).await?;
        let synced = repo::load_materialized_synced_state(&self.pool, &scope).await?;
        let local = repo::load_local_state(&self.pool, &scope).await?;
        let preferences = repo::get_preferences(&self.pool, &scope).await?;
        let resolved = self
            .resolve_with(
                &scope,
                scope_epoch,
                sync.support_state,
                &synced,
                &local,
                &preferences,
            )
            .await?;
        Ok(EqualizerSnapshot {
            support_state: sync.support_state,
            active_layer: if sync.support_state == SupportState::Supported {
                ProfileLayer::Synced
            } else {
                ProfileLayer::LocalOnly
            },
            synced,
            local,
            preferences,
            resolved,
            pending_count: repo::count_pending_ops(&self.pool, &scope).await?,
            conflict_count: repo::count_conflicts(&self.pool, &scope).await?,
        })
    }

    pub async fn resolve_current(&self) -> AppResult<ResolvedEqualizer> {
        Ok(self.snapshot().await?.resolved)
    }

    pub async fn set_preferences(
        &self,
        preferences: LocalPreferences,
    ) -> AppResult<EqualizerSnapshot> {
        let scope = self.account_scope().await;
        repo::set_preferences(&self.pool, &scope, &preferences).await?;
        self.snapshot().await
    }

    /// A manual choice is intentionally session-only and is cleared on
    /// account/server scope changes.
    pub async fn set_manual_override(
        &self,
        target: Option<ProfileTarget>,
    ) -> AppResult<EqualizerSnapshot> {
        *self.manual_override.write().await = target;
        self.snapshot().await
    }

    pub async fn create_profile(
        &self,
        mut input: EqualizerProfileInput,
    ) -> AppResult<EqualizerMutationResponse> {
        input.name = input.name.trim().to_string();
        validate_profile_input(&input)?;
        let op = PendingEqualizerOpKind::ProfileCreate {
            profile: input.clone(),
        };
        self.apply_mutation_or_local(op, |scope| async move {
            let local = repo::load_local_state(&self.pool, &scope).await?;
            if local.profiles.iter().all(|profile| profile.id != input.id)
                && local.profiles.len() >= MAX_PROFILES
            {
                return Err(AppError::Internal(format!(
                    "a device may contain at most {MAX_PROFILES} local equalizer profiles"
                )));
            }
            repo::upsert_local_profile(&self.pool, &scope, &input.into_local_profile()).await
        })
        .await
    }

    pub async fn update_profile(
        &self,
        expected_revision: Revision,
        mut input: EqualizerProfileInput,
    ) -> AppResult<EqualizerMutationResponse> {
        input.name = input.name.trim().to_string();
        validate_profile_input(&input)?;
        let op = PendingEqualizerOpKind::ProfileUpdate {
            profile_id: input.id.clone(),
            expected_revision,
            profile: input.clone(),
        };
        self.apply_mutation_or_local(op, |scope| async move {
            let local = repo::load_local_state(&self.pool, &scope).await?;
            if local.profiles.iter().all(|profile| profile.id != input.id)
                && local.profiles.len() >= MAX_PROFILES
            {
                return Err(AppError::Internal(format!(
                    "a device may contain at most {MAX_PROFILES} local equalizer profiles"
                )));
            }
            repo::upsert_local_profile(&self.pool, &scope, &input.into_local_profile()).await
        })
        .await
    }

    pub async fn delete_profile(
        &self,
        request: DeleteProfileRequest,
    ) -> AppResult<EqualizerMutationResponse> {
        if request.disposition == DeleteProfileDisposition::RejectIfReferenced {
            let snapshot = self.snapshot().await?;
            let (default_profile_id, rules) = match snapshot.active_layer {
                ProfileLayer::Synced => (
                    snapshot.synced.default_profile_id.as_deref(),
                    snapshot.synced.device_rules.as_slice(),
                ),
                ProfileLayer::LocalOnly => (
                    snapshot.local.default_profile_id.as_deref(),
                    snapshot.local.device_rules.as_slice(),
                ),
            };
            let scope = self.account_scope().await;
            let exact_reference = repo::list_exact_bindings(&self.pool, &scope)
                .await?
                .iter()
                .any(|binding| {
                    binding.target.action.profile_id() == Some(request.profile_id.as_str())
                });
            if default_profile_id == Some(request.profile_id.as_str())
                || rules
                    .iter()
                    .any(|rule| rule.action.profile_id() == Some(request.profile_id.as_str()))
                || exact_reference
            {
                return Err(AppError::Conflict {
                    code: "profile_referenced".into(),
                    message: "profile is still referenced by a default, rule, or exact binding"
                        .into(),
                });
            }
        }
        let replacement = match &request.disposition {
            DeleteProfileDisposition::ReplaceWithProfile { profile_id } => Some(profile_id.clone()),
            DeleteProfileDisposition::ReplaceWithFlat
            | DeleteProfileDisposition::RejectIfReferenced => None,
        };
        let local_id = request.profile_id.clone();
        let deleted_id = local_id.clone();
        let binding_replacement = request.local_binding_disposition.clone();
        let op = PendingEqualizerOpKind::ProfileDelete { request };
        let mutation = self
            .apply_mutation_or_local(op, |scope| async move {
                repo::delete_local_profile(&self.pool, &scope, &local_id, replacement.as_deref())
                    .await
            })
            .await?;

        // Exact bindings are device-local, so the server-side delete cannot
        // rewrite them. Preserve the explicit local disposition or mark the
        // binding orphaned (and therefore ineligible for resolution).
        let scope = self.account_scope().await;
        for mut binding in repo::list_exact_bindings(&self.pool, &scope).await? {
            if binding.target.action.profile_id() != Some(deleted_id.as_str()) {
                continue;
            }
            if let Some(target) = &binding_replacement {
                binding.target = target.clone();
                binding.orphaned = false;
            } else {
                binding.orphaned = true;
            }
            repo::upsert_exact_binding(&self.pool, &scope, &binding).await?;
        }
        Ok(mutation)
    }

    pub async fn set_default_profile(
        &self,
        expected_settings_revision: Revision,
        default_profile_id: Option<String>,
    ) -> AppResult<EqualizerMutationResponse> {
        let local_default = default_profile_id.clone();
        let op = PendingEqualizerOpKind::SettingsUpdate {
            expected_settings_revision,
            default_profile_id,
        };
        self.apply_mutation_or_local(op, |scope| async move {
            repo::set_local_default(&self.pool, &scope, local_default.as_deref()).await
        })
        .await
    }

    pub async fn create_rule(
        &self,
        mut input: EqualizerDeviceRuleInput,
    ) -> AppResult<EqualizerMutationResponse> {
        canonicalize_rule(&mut input);
        let validation_rule = rule_from_input(&input, Revision(0), 0);
        validate_rule(&validation_rule).map_err(validation_error)?;
        let local_input = input.clone();
        let op = PendingEqualizerOpKind::RuleCreate { rule: input };
        self.apply_mutation_or_local(op, |scope| async move {
            let local = repo::load_local_state(&self.pool, &scope).await?;
            if local
                .device_rules
                .iter()
                .all(|rule| rule.id != local_input.id)
                && local.device_rules.len() >= MAX_RULES
            {
                return Err(AppError::Internal(format!(
                    "a device may contain at most {MAX_RULES} local equalizer rules"
                )));
            }
            let priority = local
                .device_rules
                .iter()
                .map(|rule| rule.priority)
                .min()
                .unwrap_or(1)
                - 1;
            let local_rule = rule_from_input(&local_input, Revision(0), priority);
            repo::upsert_local_rule(&self.pool, &scope, &local_rule).await
        })
        .await
    }

    pub async fn update_rule(
        &self,
        expected_revision: Revision,
        mut input: EqualizerDeviceRuleInput,
    ) -> AppResult<EqualizerMutationResponse> {
        canonicalize_rule(&mut input);
        let scope = self.account_scope().await;
        let current = repo::load_local_state(&self.pool, &scope).await?;
        let priority = current
            .device_rules
            .iter()
            .find(|rule| rule.id == input.id)
            .map(|rule| rule.priority)
            .unwrap_or(1);
        let local_rule = rule_from_input(&input, Revision(0), priority);
        validate_rule(&local_rule).map_err(validation_error)?;
        let op = PendingEqualizerOpKind::RuleUpdate {
            rule_id: input.id.clone(),
            expected_revision,
            rule: input,
        };
        self.apply_mutation_or_local(op, |scope| async move {
            let local = repo::load_local_state(&self.pool, &scope).await?;
            if local
                .device_rules
                .iter()
                .all(|rule| rule.id != local_rule.id)
                && local.device_rules.len() >= MAX_RULES
            {
                return Err(AppError::Internal(format!(
                    "a device may contain at most {MAX_RULES} local equalizer rules"
                )));
            }
            repo::upsert_local_rule(&self.pool, &scope, &local_rule).await
        })
        .await
    }

    pub async fn delete_rule(
        &self,
        rule_id: String,
        expected_revision: Revision,
    ) -> AppResult<EqualizerMutationResponse> {
        let local_id = rule_id.clone();
        let op = PendingEqualizerOpKind::RuleDelete {
            rule_id,
            expected_revision,
        };
        self.apply_mutation_or_local(op, |scope| async move {
            repo::delete_local_rule(&self.pool, &scope, &local_id).await
        })
        .await
    }

    pub async fn reorder_rules(
        &self,
        rules: Vec<EntityRevision>,
    ) -> AppResult<EqualizerMutationResponse> {
        let local_rules = rules.clone();
        self.apply_mutation_or_local(
            PendingEqualizerOpKind::RuleReorder { rules },
            |scope| async move {
                let mut local = repo::load_local_state(&self.pool, &scope).await?;
                let count = local_rules.len() as i32;
                for (index, expected) in local_rules.iter().enumerate() {
                    if let Some(rule) = local
                        .device_rules
                        .iter_mut()
                        .find(|rule| rule.id == expected.id)
                    {
                        rule.priority = count - index as i32;
                        repo::upsert_local_rule(&self.pool, &scope, rule).await?;
                    }
                }
                Ok(())
            },
        )
        .await
    }

    /// Copy a preserved device-local profile into the active bearer account.
    /// The original is intentionally retained until the user deletes it.
    pub async fn promote_local_profile(
        &self,
        local_profile_id: &str,
        assign_default: bool,
        remap_exact_bindings: bool,
    ) -> AppResult<EqualizerMutationResponse> {
        let _guard = self.mutation_lock.lock().await;
        let scope = self.account_scope().await;
        let (auth, credential) = self
            .authenticated_bearer_for_scope(&scope)
            .await?
            .ok_or_else(|| {
                AppError::AuthNotConfigured(
                    "syncing a local equalizer profile requires a user login".into(),
                )
            })?;
        let support = repo::get_sync_state(&self.pool, &scope)
            .await?
            .support_state;
        if support == SupportState::FutureFormat {
            return Err(AppError::Unsupported(
                "this server uses a newer equalizer format; update Octave before promoting".into(),
            ));
        }
        if matches!(support, SupportState::Unknown | SupportState::Unsupported) {
            let probed = self.probe_support(&scope, &auth, &credential).await?;
            if probed != SupportState::Supported {
                return Err(AppError::Unsupported(
                    "the configured server does not support synchronized equalizer profiles".into(),
                ));
            }
        }
        if repo::count_pending_ops(&self.pool, &scope).await? > 0 {
            return Err(AppError::Conflict {
                code: "sync_pending".into(),
                message: "sync queued equalizer edits before promoting a local profile".into(),
            });
        }
        let fetch = match auth.server().equalizer_state(&credential, None).await {
            Ok(fetch) => fetch,
            Err(error @ AppError::Unsupported(_)) if endpoint_unimplemented(&error) => {
                let _ = self.preserve_synced_as_local(&scope).await?;
                repo::set_support_state(&self.pool, &scope, SupportState::Unsupported).await?;
                return Err(AppError::Unsupported(
                    "the configured server no longer supports synchronized equalizer profiles"
                        .into(),
                ));
            }
            Err(error @ AppError::Unsupported(_)) => {
                repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if let Some(state) = fetch.state {
            if !state_is_supported_v1(&state) {
                repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
                return Err(AppError::Unsupported(
                    "response_format: the server uses a newer equalizer format".into(),
                ));
            }
            repo::replace_synced_state(&self.pool, &scope, &state, fetch.etag.as_deref()).await?;
        }
        let local = repo::load_local_state(&self.pool, &scope).await?;
        let source = local
            .profiles
            .iter()
            .find(|profile| profile.id == local_profile_id)
            .cloned()
            .ok_or_else(|| AppError::Internal("local equalizer profile not found".into()))?;
        let synced = repo::load_synced_state(&self.pool, &scope).await?;
        let mut input: EqualizerProfileInput = (&source).into();
        input.id = promotion_profile_id(&scope, local_profile_id);
        let existing = synced
            .profiles
            .iter()
            .find(|profile| profile.id == input.id);
        input.name = existing.map_or_else(
            || unique_promotion_name(&input.name, &synced.profiles),
            |profile| profile.name.clone(),
        );
        validate_profile_input(&input)?;
        let promoted_id = input.id.clone();
        let mut response = if let Some(existing) = existing {
            if profile_matches_input(existing, &input) {
                EqualizerMutationResponse {
                    changed: false,
                    audit_id: None,
                    state: synced.clone(),
                }
            } else {
                auth.server()
                    .equalizer_update_profile(&credential, existing.revision, input)
                    .await?
            }
        } else {
            auth.server()
                .equalizer_create_profile(&credential, input)
                .await?
        };
        if !state_is_supported_v1(&response.state) {
            repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
            return Err(AppError::Unsupported(
                "response_format: the server committed the promotion but returned a newer equalizer format"
                    .into(),
            ));
        }
        repo::replace_synced_state(&self.pool, &scope, &response.state, None).await?;
        if assign_default
            && response.state.default_profile_id.as_deref() != Some(promoted_id.as_str())
        {
            response = auth
                .server()
                .equalizer_update_settings(
                    &credential,
                    response.state.settings_revision,
                    Some(promoted_id.clone()),
                )
                .await?;
            if !state_is_supported_v1(&response.state) {
                repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
                return Err(AppError::Unsupported(
                    "response_format: the server committed the default change but returned a newer equalizer format"
                        .into(),
                ));
            }
            repo::replace_synced_state(&self.pool, &scope, &response.state, None).await?;
        }
        if remap_exact_bindings {
            repo::remap_exact_profile_bindings(
                &self.pool,
                &scope,
                ProfileLayer::LocalOnly,
                local_profile_id,
                ProfileLayer::Synced,
                &promoted_id,
            )
            .await?;
        }
        Ok(response)
    }

    pub async fn attach_current_output(
        &self,
        target: ProfileTarget,
    ) -> AppResult<EqualizerSnapshot> {
        let _guard = self.mutation_lock.lock().await;
        let output = self
            .outputs
            .read()
            .await
            .iter()
            .find(|output| output.selected)
            .cloned()
            .ok_or_else(|| AppError::Internal("no selected audio output".into()))?;
        if output.binding_stability != BindingStability::PersistentExact {
            return Err(AppError::Internal(
                "the current output does not expose a stable exact binding".into(),
            ));
        }
        let endpoint_key = output.local_endpoint_key.ok_or_else(|| {
            AppError::Internal("the current output has no private endpoint key".into())
        })?;
        let scope = self.account_scope().await;
        repo::upsert_exact_binding(
            &self.pool,
            &scope,
            &ExactBinding {
                endpoint_key,
                display_label: Some(output.display_name),
                target,
                orphaned: false,
            },
        )
        .await?;
        self.snapshot().await
    }

    /// Remove the binding for the selected persistent output without exposing
    /// its keyed endpoint identity beyond the native process.
    pub async fn detach_current_output(&self) -> AppResult<EqualizerSnapshot> {
        let _guard = self.mutation_lock.lock().await;
        let output = self
            .outputs
            .read()
            .await
            .iter()
            .find(|output| output.selected)
            .cloned()
            .ok_or_else(|| AppError::Internal("no selected audio output".into()))?;
        if output.binding_stability != BindingStability::PersistentExact {
            return Err(AppError::Internal(
                "the current output does not expose a stable exact binding".into(),
            ));
        }
        let endpoint_key = output.local_endpoint_key.ok_or_else(|| {
            AppError::Internal("the current output has no private endpoint key".into())
        })?;
        let scope = self.account_scope().await;
        repo::delete_exact_binding(&self.pool, &scope, &endpoint_key).await?;
        self.snapshot().await
    }

    pub async fn list_conflicts(&self) -> AppResult<Vec<EqualizerConflict>> {
        repo::list_conflicts(&self.pool, &self.account_scope().await).await
    }

    pub async fn discard_conflict(&self, id: i64) -> AppResult<EqualizerSnapshot> {
        let scope = self.account_scope().await;
        repo::delete_conflict(&self.pool, &scope, id).await?;
        self.snapshot().await
    }

    pub async fn resolve_conflict(&self, id: i64, resolution: &str) -> AppResult<()> {
        let scope = self.account_scope().await;
        let conflicts = repo::list_conflicts(&self.pool, &scope).await?;
        let conflict = conflicts
            .iter()
            .find(|conflict| conflict.id == id)
            .ok_or_else(|| AppError::Internal(format!("EQ conflict {id} not found")))?;
        let dependency_group = conflict.dependency_group.clone();
        let group = conflicts
            .into_iter()
            .filter(|conflict| conflict.dependency_group == dependency_group)
            .collect::<Vec<_>>();
        match resolution {
            "keep_server" => {}
            "keep_local_copy" => {
                for conflict in &group {
                    let op = PendingEqualizerOpKind::from_json(&conflict.payload_json)?;
                    apply_op_to_local(&self.pool, &scope, op).await?;
                }
            }
            "retry" => {
                let current = repo::load_synced_state(&self.pool, &scope).await?;
                for conflict in &group {
                    let mut op = PendingEqualizerOpKind::from_json(&conflict.payload_json)?;
                    op.rebase(&current);
                    repo::enqueue_op(&self.pool, &scope, &op).await?;
                }
            }
            other => {
                return Err(AppError::Internal(format!(
                    "unknown EQ conflict resolution '{other}'"
                )));
            }
        }
        for conflict in group {
            repo::delete_conflict(&self.pool, &scope, conflict.id).await?;
        }
        Ok(())
    }

    pub fn import_apo(&self, text: &str, name: &str) -> AppResult<EqualizerProfileInput> {
        parser::parse_equalizer_text(text, name)
            .map(|parsed| (&parsed.profile).into())
            .map_err(|error| AppError::Internal(error.to_string()))
    }

    pub fn export_apo(&self, profile: &EqualizerProfile) -> AppResult<String> {
        parser::export_equalizer_text(profile)
            .map_err(|error| AppError::Internal(error.to_string()))
    }

    pub async fn list_changes(
        &self,
        subject_user_id: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> AppResult<ChangePage> {
        let (auth, credential, _) = self.authenticated_audit().await?.ok_or_else(|| {
            AppError::AuthNotConfigured("EQ audit history requires a user login".into())
        })?;
        auth.server()
            .equalizer_list_changes(&credential, subject_user_id, cursor, limit)
            .await
    }

    pub async fn get_change(&self, audit_id: &str) -> AppResult<EqualizerChangeDetail> {
        let (auth, credential, _) = self.authenticated_audit().await?.ok_or_else(|| {
            AppError::AuthNotConfigured("EQ audit history requires a user login".into())
        })?;
        auth.server()
            .equalizer_get_change(&credential, audit_id)
            .await
    }

    pub async fn rollback_change(
        &self,
        audit_id: &str,
        expected_state_revision: Revision,
    ) -> AppResult<EqualizerRollbackResponse> {
        let (auth, credential, session) = self.authenticated_audit().await?.ok_or_else(|| {
            AppError::AuthNotConfigured("EQ rollback requires a user login".into())
        })?;
        let response = auth
            .server()
            .equalizer_rollback_change(&credential, audit_id, expected_state_revision)
            .await?;
        if session.kind == StoredCredentialKind::Bearer
            && session.user_id.as_deref() == Some(response.target_owner_id.as_str())
        {
            self.sync_now().await?;
        }
        Ok(response)
    }

    /// Pull, FIFO replay, then pull again. A CAS conflict moves the failed op
    /// and its dependency group aside; transport failure leaves the queue intact.
    pub async fn sync_now(&self) -> AppResult<EqualizerSnapshot> {
        let _guard = self.mutation_lock.lock().await;
        let scope = self.account_scope().await;
        let Some((auth, credential)) = self.authenticated_bearer_for_scope(&scope).await? else {
            return self.snapshot().await;
        };

        let support = repo::get_sync_state(&self.pool, &scope)
            .await?
            .support_state;
        // Future-format quarantine must be fail-closed, but it must not be an
        // absorbing state. A previous client (or a transient REST decode
        // failure) may have persisted the verdict; always give the current
        // server/client pair a non-mutating state probe so an upgraded or
        // repaired client can recover the account snapshot by itself.
        if support_requires_probe(support) {
            match self.probe_support(&scope, &auth, &credential).await {
                Ok(SupportState::Supported) => {}
                Ok(_) | Err(AppError::Transport(_)) => return self.snapshot().await,
                Err(error) => return Err(error),
            }
        }

        loop {
            let Some(pending) = repo::list_pending_ops(&self.pool, &scope)
                .await?
                .into_iter()
                .next()
            else {
                break;
            };
            let op = PendingEqualizerOpKind::from_json(&pending.payload_json)?;
            match push_op(&auth, &credential, op).await {
                Ok(response) if state_is_supported_v1(&response.state) => {
                    repo::replace_synced_state_and_acknowledge(
                        &self.pool,
                        &scope,
                        pending.id,
                        &response.state,
                        None,
                    )
                    .await?;
                }
                Ok(_) => {
                    repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
                    repo::move_dependency_to_conflicts(
                        &self.pool,
                        &scope,
                        pending.id,
                        &pending.dependency_group,
                        "future_format",
                        "the server committed the change but returned a newer equalizer format",
                        None,
                    )
                    .await?;
                    return self.snapshot().await;
                }
                Err(AppError::Conflict { code, message }) => {
                    let server_revision = repo::get_sync_state(&self.pool, &scope)
                        .await?
                        .state_revision;
                    repo::move_dependency_to_conflicts(
                        &self.pool,
                        &scope,
                        pending.id,
                        &pending.dependency_group,
                        &code,
                        &message,
                        Some(server_revision),
                    )
                    .await?;
                }
                Err(error @ AppError::Transport(_)) => {
                    repo::mark_op_failed(&self.pool, pending.id, &error.to_string()).await?;
                    break;
                }
                Err(error @ AppError::Unsupported(_)) if endpoint_unimplemented(&error) => {
                    let recovery = self.preserve_synced_as_local(&scope).await?;
                    self.preserve_pending_as_local(&scope, Some(&recovery))
                        .await?;
                    repo::set_support_state(&self.pool, &scope, SupportState::Unsupported).await?;
                    return self.snapshot().await;
                }
                Err(error @ AppError::Unsupported(_)) => {
                    repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
                    repo::move_dependency_to_conflicts(
                        &self.pool,
                        &scope,
                        pending.id,
                        &pending.dependency_group,
                        "future_format",
                        &error.to_string(),
                        None,
                    )
                    .await?;
                    return self.snapshot().await;
                }
                Err(error) => return Err(error),
            }
        }

        // Never pull over an optimistic overlay. The next reconnect retries
        // FIFO first and only then refreshes the clean mirror.
        if repo::count_pending_ops(&self.pool, &scope).await? == 0 {
            let known = repo::get_sync_state(&self.pool, &scope).await?;
            match auth
                .server()
                .equalizer_state(
                    &credential,
                    known.has_complete_snapshot.then_some(known.state_revision),
                )
                .await
            {
                Ok(fetch) => {
                    if let Some(state) = fetch.state {
                        if state_is_supported_v1(&state) {
                            repo::replace_synced_state(
                                &self.pool,
                                &scope,
                                &state,
                                fetch.etag.as_deref(),
                            )
                            .await?;
                        } else {
                            repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat)
                                .await?;
                        }
                    } else {
                        repo::set_support_state(&self.pool, &scope, SupportState::Supported)
                            .await?;
                    }
                }
                Err(error @ AppError::Unsupported(_)) if endpoint_unimplemented(&error) => {
                    let _ = self.preserve_synced_as_local(&scope).await?;
                    repo::set_support_state(&self.pool, &scope, SupportState::Unsupported).await?;
                }
                Err(AppError::Unsupported(_)) => {
                    repo::set_support_state(&self.pool, &scope, SupportState::FutureFormat).await?;
                }
                Err(error) => return Err(error),
            }
        }
        self.snapshot().await
    }

    async fn apply_mutation_or_local<F, Fut>(
        &self,
        op: PendingEqualizerOpKind,
        local: F,
    ) -> AppResult<EqualizerMutationResponse>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = AppResult<()>>,
    {
        let _guard = self.mutation_lock.lock().await;
        let scope = self.account_scope().await;
        let support = repo::get_sync_state(&self.pool, &scope)
            .await?
            .support_state;
        if support == SupportState::FutureFormat {
            return Err(AppError::Unsupported(
                "this server uses a newer equalizer format; update Octave before editing".into(),
            ));
        }
        if support == SupportState::Unknown {
            if let Some((auth, credential)) = self.authenticated_bearer_for_scope(&scope).await? {
                match self.probe_support(&scope, &auth, &credential).await {
                    Ok(_) | Err(AppError::Transport(_)) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let response = if self.uses_local_layer(&scope).await? {
            local(scope).await?;
            None
        } else {
            self.apply_synced_op(&scope, op).await?
        };
        if let Some(response) = response {
            Ok(response)
        } else {
            Ok(synthetic_mutation(self.snapshot().await?))
        }
    }

    async fn apply_synced_op(
        &self,
        scope: &str,
        op: PendingEqualizerOpKind,
    ) -> AppResult<Option<EqualizerMutationResponse>> {
        // A newly-online edit must never overtake older offline operations.
        // Enqueue behind the active scope's FIFO and let the shared scheduler
        // replay/rebase the entire chain in order.
        if repo::count_pending_ops(&self.pool, scope).await? > 0 {
            repo::enqueue_op(&self.pool, scope, &op).await?;
            return Ok(None);
        }
        let Some((auth, credential)) = self.authenticated_bearer_for_scope(scope).await? else {
            repo::enqueue_op(&self.pool, scope, &op).await?;
            return Ok(None);
        };
        match push_op(&auth, &credential, op.clone()).await {
            Ok(response) if state_is_supported_v1(&response.state) => {
                repo::set_support_state(&self.pool, scope, SupportState::Supported).await?;
                repo::replace_synced_state(&self.pool, scope, &response.state, None).await?;
                Ok(Some(response))
            }
            Ok(_) => {
                repo::set_support_state(&self.pool, scope, SupportState::FutureFormat).await?;
                Err(AppError::Unsupported(
                    "response_format: the server committed the change but returned a newer equalizer format"
                        .into(),
                ))
            }
            Err(error @ AppError::Unsupported(_)) if endpoint_unimplemented(&error) => {
                let recovery = self.preserve_synced_as_local(scope).await?;
                repo::set_support_state(&self.pool, scope, SupportState::Unsupported).await?;
                let mut local_op = op;
                local_op.remap_for_local_recovery(&recovery.profile_ids, &recovery.rule_ids);
                apply_op_to_local(&self.pool, scope, local_op).await?;
                Ok(None)
            }
            Err(error @ AppError::Unsupported(_)) => {
                repo::set_support_state(&self.pool, scope, SupportState::FutureFormat).await?;
                Err(error)
            }
            Err(AppError::Transport(_)) | Err(AppError::AuthNotConfigured(_)) => {
                let state = repo::get_sync_state(&self.pool, scope).await?;
                if state.support_state == SupportState::Unknown || !state.has_complete_snapshot {
                    apply_op_to_local(&self.pool, scope, op).await?;
                } else {
                    repo::enqueue_op(&self.pool, scope, &op).await?;
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    async fn uses_local_layer(&self, scope: &str) -> AppResult<bool> {
        let support = repo::get_sync_state(&self.pool, scope)
            .await
            .map(|state| state.support_state)
            .unwrap_or(SupportState::Unknown);
        let Some(auth) = self.auth.read().await.clone() else {
            return Ok(true);
        };
        let session = auth.current().await;
        if scope_for_auth(&auth, session.as_ref()) != scope {
            return Err(scope_changed());
        }
        Ok(match session {
            None => true,
            Some(session) if session.kind == StoredCredentialKind::SecretKey => true,
            Some(_) => support == SupportState::Unsupported,
        })
    }

    async fn probe_support(
        &self,
        scope: &str,
        auth: &AuthManager,
        credential: &crate::transport::Credential,
    ) -> AppResult<SupportState> {
        match auth.server().equalizer_state(credential, None).await {
            Ok(fetch) => {
                if let Some(state) = fetch.state {
                    if !state_is_supported_v1(&state) {
                        repo::set_support_state(&self.pool, scope, SupportState::FutureFormat)
                            .await?;
                        return Ok(SupportState::FutureFormat);
                    }
                    repo::replace_synced_state(&self.pool, scope, &state, fetch.etag.as_deref())
                        .await?;
                } else {
                    repo::set_support_state(&self.pool, scope, SupportState::Supported).await?;
                }
                Ok(SupportState::Supported)
            }
            Err(error @ AppError::Unsupported(_)) if endpoint_unimplemented(&error) => {
                let _ = self.preserve_synced_as_local(scope).await?;
                repo::set_support_state(&self.pool, scope, SupportState::Unsupported).await?;
                Ok(SupportState::Unsupported)
            }
            Err(AppError::Unsupported(_)) => {
                repo::set_support_state(&self.pool, scope, SupportState::FutureFormat).await?;
                Ok(SupportState::FutureFormat)
            }
            Err(error) => Err(error),
        }
    }

    async fn preserve_pending_as_local(
        &self,
        scope: &str,
        recovery: Option<&repo::EqualizerRecoveryMap>,
    ) -> AppResult<()> {
        for pending in repo::list_pending_ops(&self.pool, scope).await? {
            let mut op = PendingEqualizerOpKind::from_json(&pending.payload_json)?;
            if let Some(recovery) = recovery {
                op.remap_for_local_recovery(&recovery.profile_ids, &recovery.rule_ids);
            }
            apply_op_to_local(&self.pool, scope, op).await?;
            repo::delete_pending_op(&self.pool, scope, pending.id).await?;
        }
        Ok(())
    }

    async fn preserve_synced_as_local(&self, scope: &str) -> AppResult<repo::EqualizerRecoveryMap> {
        let synced = repo::load_synced_state(&self.pool, scope).await?;
        repo::preserve_synced_state_as_local(&self.pool, scope, &synced).await
    }

    async fn authenticated_bearer_for_scope(
        &self,
        expected_scope: &str,
    ) -> AppResult<Option<(Arc<AuthManager>, crate::transport::Credential)>> {
        let Some(auth) = self.auth.read().await.clone() else {
            return if expected_scope == "device:unconfigured" {
                Ok(None)
            } else {
                Err(scope_changed())
            };
        };
        let Some(session) = auth.current().await else {
            return if scope_for_auth(&auth, None) == expected_scope {
                Ok(None)
            } else {
                Err(scope_changed())
            };
        };
        if scope_for_auth(&auth, Some(&session)) != expected_scope {
            return Err(scope_changed());
        }
        if session.kind != StoredCredentialKind::Bearer {
            return Ok(None);
        }
        let credential = auth.credential().await?;
        let still_current = self.auth.read().await.clone();
        if !still_current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &auth))
            || scope_for_auth(&auth, auth.current().await.as_ref()) != expected_scope
        {
            return Err(scope_changed());
        }
        Ok(Some((auth, credential)))
    }

    async fn authenticated_audit(
        &self,
    ) -> AppResult<
        Option<(
            Arc<AuthManager>,
            crate::transport::Credential,
            crate::auth::AuthSession,
        )>,
    > {
        let Some(auth) = self.auth.read().await.clone() else {
            return Ok(None);
        };
        let Some(session) = auth.current().await else {
            return Ok(None);
        };
        let credential = auth.credential().await?;
        Ok(Some((auth, credential, session)))
    }

    async fn account_scope(&self) -> String {
        self.account_context().await.0
    }

    /// Resolve the account namespace and its epoch as one serialized context.
    /// Holding the scope write lock while sampling auth prevents an older
    /// in-flight lookup from installing its scope after a newer server switch.
    async fn account_context(&self) -> (String, Revision) {
        let mut current = self.current_scope.write().await;
        let auth = self.auth.read().await.clone();
        let scope = if let Some(auth) = auth {
            let session = auth.current().await;
            scope_for_auth(&auth, session.as_ref())
        } else {
            "device:unconfigured".to_string()
        };
        if *current != scope {
            *current = scope.clone();
            self.scope_epoch.fetch_add(1, Ordering::SeqCst);
            *self.manual_override.write().await = None;
        }
        (scope, Revision(self.scope_epoch.load(Ordering::SeqCst)))
    }

    async fn resolve_with(
        &self,
        scope: &str,
        scope_epoch: Revision,
        support_state: SupportState,
        synced: &EqualizerState,
        local: &LocalEqualizerState,
        preferences: &LocalPreferences,
    ) -> AppResult<ResolvedEqualizer> {
        let exact = repo::list_exact_bindings(&self.pool, scope).await?;
        let outputs = self.outputs.read().await.clone();
        let current = self.current_scope.read().await;
        let still_current = current.as_str() == scope
            && Revision(self.scope_epoch.load(Ordering::SeqCst)) == scope_epoch;
        let manual = if still_current {
            self.manual_override.read().await.clone()
        } else {
            None
        };
        drop(current);
        let generation = self.resolution_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let resolved = resolver::resolve(resolver::ResolveInput {
            preferences,
            support_state,
            synced,
            local,
            manual_override: manual.as_ref(),
            exact_bindings: &exact,
            outputs: &outputs,
            scope_epoch,
            resolution_generation: Revision(generation),
        });
        let current = self.current_scope.read().await;
        if current.as_str() == scope
            && Revision(self.scope_epoch.load(Ordering::SeqCst)) == scope_epoch
        {
            *self.last_resolved.write().await = Some(resolved.clone());
        }
        Ok(resolved)
    }

    async fn device_secret(&self) -> AppResult<Vec<u8>> {
        let mut cached = self.device_secret.write().await;
        if let Some(secret) = cached.clone() {
            return Ok(secret);
        }
        const KEY: &str = "equalizer.endpoint_hmac_secret.v1";
        let encoded = crate::cache::repo::get_setting(&self.pool, KEY).await?;
        let generated = encoded.is_none();
        let secret = encoded.unwrap_or_else(|| {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        });
        if generated {
            crate::cache::repo::set_setting(&self.pool, KEY, &secret).await?;
        }
        let bytes = secret.into_bytes();
        *cached = Some(bytes.clone());
        Ok(bytes)
    }
}

fn endpoint_unimplemented(error: &AppError) -> bool {
    matches!(error, AppError::Unsupported(message) if message.starts_with("endpoint_unimplemented:"))
}

fn support_requires_probe(support: SupportState) -> bool {
    matches!(
        support,
        SupportState::Unknown | SupportState::Unsupported | SupportState::FutureFormat
    )
}

fn state_is_supported_v1(state: &EqualizerState) -> bool {
    state.state_format_version == EQ_STATE_FORMAT_VERSION
        && state
            .profiles
            .iter()
            .all(|profile| profile.format_version == EQ_PROFILE_FORMAT_VERSION)
}

fn unique_promotion_name(base: &str, existing: &[EqualizerProfile]) -> String {
    let keys = existing
        .iter()
        .map(|profile| normalize_matcher(&profile.name))
        .collect::<std::collections::HashSet<_>>();
    if !keys.contains(&normalize_matcher(base)) {
        return base.to_string();
    }
    for suffix in 2..=999 {
        let marker = format!(" ({suffix})");
        let keep = MAX_NAME_CHARS.saturating_sub(marker.chars().count());
        let candidate = format!("{}{}", base.chars().take(keep).collect::<String>(), marker);
        if !keys.contains(&normalize_matcher(&candidate)) {
            return candidate;
        }
    }
    format!("Local {}", uuid::Uuid::new_v4().simple())
        .chars()
        .take(MAX_NAME_CHARS)
        .collect()
}

fn promotion_profile_id(scope: &str, local_profile_id: &str) -> String {
    let digest = Sha256::digest(format!(
        "octave.equalizer.promotion.v1\0{scope}\0{local_profile_id}"
    ));
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 4122 variant + a deterministic name-based version marker.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn profile_matches_input(profile: &EqualizerProfile, input: &EqualizerProfileInput) -> bool {
    profile.name == input.name
        && profile.format_version == input.format_version
        && profile.preamp_db == input.preamp_db
        && profile.auto_headroom_enabled == input.auto_headroom_enabled
        && profile.bands == input.bands
}

fn synthetic_mutation(snapshot: EqualizerSnapshot) -> EqualizerMutationResponse {
    let state = match snapshot.active_layer {
        ProfileLayer::Synced => snapshot.synced,
        ProfileLayer::LocalOnly => EqualizerState {
            state_format_version: EQ_STATE_FORMAT_VERSION,
            state_revision: Revision(0),
            settings_revision: Revision(0),
            default_profile_id: snapshot.local.default_profile_id,
            profiles: snapshot.local.profiles,
            device_rules: snapshot.local.device_rules,
        },
    };
    EqualizerMutationResponse {
        changed: true,
        audit_id: None,
        state,
    }
}

async fn push_op(
    auth: &AuthManager,
    credential: &crate::transport::Credential,
    op: PendingEqualizerOpKind,
) -> AppResult<EqualizerMutationResponse> {
    let server = auth.server();
    match op {
        PendingEqualizerOpKind::ProfileCreate { profile } => {
            server.equalizer_create_profile(credential, profile).await
        }
        PendingEqualizerOpKind::ProfileUpdate {
            expected_revision,
            profile,
            ..
        } => {
            server
                .equalizer_update_profile(credential, expected_revision, profile)
                .await
        }
        PendingEqualizerOpKind::ProfileDelete { request } => {
            server.equalizer_delete_profile(credential, request).await
        }
        PendingEqualizerOpKind::SettingsUpdate {
            expected_settings_revision,
            default_profile_id,
        } => {
            server
                .equalizer_update_settings(
                    credential,
                    expected_settings_revision,
                    default_profile_id,
                )
                .await
        }
        PendingEqualizerOpKind::RuleCreate { rule } => {
            server.equalizer_create_rule(credential, rule).await
        }
        PendingEqualizerOpKind::RuleUpdate {
            expected_revision,
            rule,
            ..
        } => {
            server
                .equalizer_update_rule(credential, expected_revision, rule)
                .await
        }
        PendingEqualizerOpKind::RuleDelete {
            rule_id,
            expected_revision,
        } => {
            server
                .equalizer_delete_rule(credential, &rule_id, expected_revision)
                .await
        }
        PendingEqualizerOpKind::RuleReorder { rules } => {
            server.equalizer_reorder_rules(credential, rules).await
        }
    }
}

async fn apply_op_to_local(
    pool: &SqlitePool,
    scope: &str,
    op: PendingEqualizerOpKind,
) -> AppResult<()> {
    match op {
        PendingEqualizerOpKind::ProfileCreate { profile }
        | PendingEqualizerOpKind::ProfileUpdate { profile, .. } => {
            repo::upsert_local_profile(pool, scope, &profile.into_local_profile()).await
        }
        PendingEqualizerOpKind::ProfileDelete { request } => {
            let replacement = match request.disposition {
                DeleteProfileDisposition::ReplaceWithProfile { profile_id } => Some(profile_id),
                _ => None,
            };
            repo::delete_local_profile(pool, scope, &request.profile_id, replacement.as_deref())
                .await
        }
        PendingEqualizerOpKind::SettingsUpdate {
            default_profile_id, ..
        } => repo::set_local_default(pool, scope, default_profile_id.as_deref()).await,
        PendingEqualizerOpKind::RuleCreate { rule }
        | PendingEqualizerOpKind::RuleUpdate { rule, .. } => {
            let local = repo::load_local_state(pool, scope).await?;
            let priority = local
                .device_rules
                .iter()
                .find(|item| item.id == rule.id)
                .map(|item| item.priority)
                .unwrap_or_else(|| {
                    local
                        .device_rules
                        .iter()
                        .map(|item| item.priority)
                        .min()
                        .unwrap_or(1)
                        - 1
                });
            repo::upsert_local_rule(pool, scope, &rule_from_input(&rule, Revision(0), priority))
                .await
        }
        PendingEqualizerOpKind::RuleDelete { rule_id, .. } => {
            repo::delete_local_rule(pool, scope, &rule_id).await
        }
        PendingEqualizerOpKind::RuleReorder { rules } => {
            let mut local = repo::load_local_state(pool, scope).await?;
            let count = rules.len() as i32;
            for (index, expected) in rules.iter().enumerate() {
                if let Some(rule) = local
                    .device_rules
                    .iter_mut()
                    .find(|rule| rule.id == expected.id)
                {
                    rule.priority = count - index as i32;
                    repo::upsert_local_rule(pool, scope, rule).await?;
                }
            }
            Ok(())
        }
    }
}

fn canonicalize_rule(rule: &mut EqualizerDeviceRuleInput) {
    rule.label = rule.label.trim().to_string();
    for selector in &mut rule.selectors {
        selector.normalization_version = EQ_NORMALIZATION_VERSION;
        selector.normalized_name = normalize_matcher(&selector.normalized_name);
        selector.vendor_id = normalize_hardware_id(selector.vendor_id.as_deref());
        selector.product_id = normalize_hardware_id(selector.product_id.as_deref());
    }
}

fn rule_from_input(
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

fn validate_profile_input(input: &EqualizerProfileInput) -> AppResult<()> {
    validate_profile(&input.clone().into_local_profile()).map_err(validation_error)
}

fn validation_error(error: EqualizerValidationError) -> AppError {
    AppError::Internal(error.message)
}

fn scope_for_auth(auth: &AuthManager, session: Option<&crate::auth::AuthSession>) -> String {
    let server_hash = hex_hash(&auth.server_config().rest_url);
    match session {
        Some(session) if session.kind == StoredCredentialKind::Bearer => format!(
            "server:{server_hash}:user:{}",
            session.user_id.as_deref().unwrap_or("unknown"),
        ),
        Some(_) => format!("server:{server_hash}:secret-device"),
        None => format!("server:{server_hash}:signed-out-device"),
    }
}

fn scope_changed() -> AppError {
    AppError::Conflict {
        code: "scope_changed".into(),
        message: "the equalizer account/server scope changed while the operation was starting"
            .into(),
    }
}

fn hex_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn endpoint_hmac(secret: &[u8], runtime_id: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(runtime_id.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(runtime_id: &str) -> AudioOutput {
        AudioOutput {
            runtime_id: Some(runtime_id.into()),
            local_endpoint_key: None,
            display_name: "Private Headphones".into(),
            route_kind: RouteKind::Bluetooth,
            vendor_id: None,
            product_id: None,
            connected: true,
            selected: true,
            accuracy: RouteAccuracy::Exact,
            binding_stability: BindingStability::PersistentExact,
        }
    }

    #[test]
    fn endpoint_hmac_is_stable_keyed_and_not_the_raw_id() {
        let first = endpoint_hmac(b"device secret", "raw-platform-id");
        assert_eq!(first, endpoint_hmac(b"device secret", "raw-platform-id"));
        assert_ne!(first, endpoint_hmac(b"other secret", "raw-platform-id"));
        assert!(!first.contains("raw-platform-id"));
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn promotion_ids_are_deterministic_and_scope_partitioned() {
        let first = promotion_profile_id("server:a:user:1", "local-profile");
        assert_eq!(
            first,
            promotion_profile_id("server:a:user:1", "local-profile")
        );
        assert_ne!(
            first,
            promotion_profile_id("server:b:user:1", "local-profile")
        );
        assert!(uuid::Uuid::parse_str(&first).is_ok());
    }

    #[test]
    fn future_format_quarantine_is_reprobed() {
        assert!(!support_requires_probe(SupportState::Supported));
        assert!(support_requires_probe(SupportState::Unknown));
        assert!(support_requires_probe(SupportState::Unsupported));
        assert!(support_requires_probe(SupportState::FutureFormat));
    }

    #[tokio::test]
    async fn route_dedup_preserves_manual_override_and_raw_keys_never_serialize() {
        let pool = crate::db::open_in_memory().await.unwrap();
        let auth = Arc::new(RwLock::new(None));
        let service = EqualizerService::new(pool, auth);
        service
            .set_preferences(LocalPreferences {
                master_enabled: true,
                automatic_switching_enabled: true,
            })
            .await
            .unwrap();
        let profile = EqualizerProfile::five_band_starter("Manual");
        service.create_profile((&profile).into()).await.unwrap();

        service
            .update_outputs(vec![output("raw-platform-id")])
            .await
            .unwrap();
        let target = ProfileTarget {
            layer: ProfileLayer::LocalOnly,
            action: RuleAction::Profile {
                profile_id: profile.id.clone(),
            },
        };
        service.set_manual_override(Some(target)).await.unwrap();
        let duplicate = service
            .update_outputs(vec![output("raw-platform-id")])
            .await
            .unwrap();
        assert_eq!(duplicate.reason, ResolveReason::Manual);

        let retained = service.current_outputs().await;
        assert!(retained[0].runtime_id.is_none());
        assert!(retained[0].local_endpoint_key.is_some());
        let json = serde_json::to_string(&retained).unwrap();
        assert!(!json.contains("raw-platform-id"));
        assert!(!json.contains(retained[0].local_endpoint_key.as_deref().unwrap()));

        let changed = service
            .update_outputs(vec![output("different-id")])
            .await
            .unwrap();
        assert_ne!(changed.reason, ResolveReason::Manual);
    }
}
