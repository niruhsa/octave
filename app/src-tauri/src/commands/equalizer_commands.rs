//! Thin Tauri bridge for the native equalizer service.

use std::io::{Read, Write};
use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};

use crate::equalizer::parser::ParsedEqualizerProfile;
use crate::equalizer::{
    AudioOutputSummary, ChangePage, DeleteProfileRequest, EntityRevision, EqualizerChangeDetail,
    EqualizerConflict, EqualizerDeviceRuleInput, EqualizerMutationResponse, EqualizerProfileInput,
    EqualizerRollbackResponse, EqualizerService, EqualizerSnapshot, LocalPreferences, ProfileLayer,
    ProfileTarget, ResolvedEqualizer, Revision,
};
use crate::error::{AppError, AppResult};

#[tauri::command]
pub async fn equalizer_snapshot(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<EqualizerSnapshot> {
    service.snapshot().await
}

#[tauri::command]
pub async fn equalizer_sync_now(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<EqualizerSnapshot> {
    service.sync_now().await
}

#[tauri::command]
pub async fn equalizer_get_local_preferences(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<LocalPreferences> {
    Ok(service.snapshot().await?.preferences)
}

#[tauri::command]
pub async fn equalizer_set_local_preferences(
    service: State<'_, Arc<EqualizerService>>,
    preferences: LocalPreferences,
) -> AppResult<LocalPreferences> {
    Ok(service.set_preferences(preferences).await?.preferences)
}

#[tauri::command]
pub async fn equalizer_set_manual_override(
    service: State<'_, Arc<EqualizerService>>,
    target: ProfileTarget,
) -> AppResult<ResolvedEqualizer> {
    Ok(service.set_manual_override(Some(target)).await?.resolved)
}

#[tauri::command]
pub async fn equalizer_clear_manual_override(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<ResolvedEqualizer> {
    Ok(service.set_manual_override(None).await?.resolved)
}

#[tauri::command]
pub async fn equalizer_create_profile(
    service: State<'_, Arc<EqualizerService>>,
    profile: EqualizerProfileInput,
) -> AppResult<EqualizerMutationResponse> {
    service.create_profile(profile).await
}

#[tauri::command]
pub async fn equalizer_update_profile(
    service: State<'_, Arc<EqualizerService>>,
    profile: EqualizerProfileInput,
    expected_revision: Revision,
) -> AppResult<EqualizerMutationResponse> {
    service.update_profile(expected_revision, profile).await
}

#[tauri::command]
pub async fn equalizer_delete_profile(
    service: State<'_, Arc<EqualizerService>>,
    request: DeleteProfileRequest,
) -> AppResult<EqualizerMutationResponse> {
    service.delete_profile(request).await
}

#[tauri::command]
pub async fn equalizer_set_default(
    service: State<'_, Arc<EqualizerService>>,
    profile_id: Option<String>,
    expected_settings_revision: Revision,
) -> AppResult<EqualizerMutationResponse> {
    service
        .set_default_profile(expected_settings_revision, profile_id)
        .await
}

#[tauri::command]
pub async fn equalizer_create_device_rule(
    service: State<'_, Arc<EqualizerService>>,
    rule: EqualizerDeviceRuleInput,
) -> AppResult<EqualizerMutationResponse> {
    service.create_rule(rule).await
}

#[tauri::command]
pub async fn equalizer_update_device_rule(
    service: State<'_, Arc<EqualizerService>>,
    rule: EqualizerDeviceRuleInput,
    expected_revision: Revision,
) -> AppResult<EqualizerMutationResponse> {
    service.update_rule(expected_revision, rule).await
}

#[tauri::command]
pub async fn equalizer_delete_device_rule(
    service: State<'_, Arc<EqualizerService>>,
    id: String,
    expected_revision: Revision,
) -> AppResult<EqualizerMutationResponse> {
    service.delete_rule(id, expected_revision).await
}

#[tauri::command]
pub async fn equalizer_reorder_device_rules(
    service: State<'_, Arc<EqualizerService>>,
    ordered_rules: Vec<EntityRevision>,
) -> AppResult<EqualizerMutationResponse> {
    service.reorder_rules(ordered_rules).await
}

#[tauri::command]
pub async fn equalizer_promote_local_profile(
    service: State<'_, Arc<EqualizerService>>,
    local_profile_id: String,
    assign_default: bool,
    remap_exact_bindings: bool,
) -> AppResult<EqualizerMutationResponse> {
    service
        .promote_local_profile(&local_profile_id, assign_default, remap_exact_bindings)
        .await
}

/// Native resolves and HMACs the current endpoint entirely in Rust. No key or
/// raw route id is returned to JavaScript.
#[tauri::command]
pub async fn equalizer_attach_current_output(
    service: State<'_, Arc<EqualizerService>>,
    target: ProfileTarget,
) -> AppResult<()> {
    service.attach_current_output(target).await?;
    Ok(())
}

/// Native resolves the selected output's private key; JavaScript receives no
/// endpoint identifier and can only detach the currently selected route.
#[tauri::command]
pub async fn equalizer_detach_current_output(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<()> {
    service.detach_current_output().await?;
    Ok(())
}

#[tauri::command]
pub async fn equalizer_audio_outputs(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<Vec<AudioOutputSummary>> {
    Ok(service
        .current_outputs()
        .await
        .iter()
        .map(AudioOutputSummary::from)
        .collect())
}

#[tauri::command]
pub async fn equalizer_current_output(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<Option<AudioOutputSummary>> {
    Ok(service
        .current_outputs()
        .await
        .iter()
        .find(|output| output.selected)
        .map(AudioOutputSummary::from))
}

#[tauri::command]
pub async fn equalizer_conflicts(
    service: State<'_, Arc<EqualizerService>>,
) -> AppResult<Vec<EqualizerConflict>> {
    service.list_conflicts().await
}

#[tauri::command]
pub async fn equalizer_resolve_conflict(
    service: State<'_, Arc<EqualizerService>>,
    conflict_id: i64,
    resolution: String,
) -> AppResult<()> {
    service.resolve_conflict(conflict_id, &resolution).await
}

#[tauri::command]
pub fn equalizer_parse_text(
    text: String,
    proposed_name: String,
) -> AppResult<ParsedEqualizerProfile> {
    crate::equalizer::parser::parse_equalizer_text(&text, &proposed_name)
        .map_err(|error| AppError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn equalizer_import_file(
    app: AppHandle,
    path_or_uri: String,
    proposed_name: Option<String>,
) -> AppResult<ParsedEqualizerProfile> {
    let fp: FilePath = path_or_uri.parse().expect("FilePath parsing is infallible");
    let text = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut options = OpenOptions::new();
        options.read(true);
        let file = app.fs().open(fp, options)?;
        let mut bytes = Vec::with_capacity(crate::equalizer::parser::MAX_IMPORT_BYTES + 1);
        file.take((crate::equalizer::parser::MAX_IMPORT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > crate::equalizer::parser::MAX_IMPORT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "equalizer import exceeds 64 KiB",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
    .await
    .map_err(|error| AppError::Internal(format!("read EQ task: {error}")))?
    .map_err(|error| AppError::Internal(format!("read EQ file: {error}")))?;
    let name = proposed_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Imported equalizer");
    crate::equalizer::parser::parse_equalizer_text(&text, name)
        .map_err(|error| AppError::Internal(error.to_string()))
}

#[tauri::command]
pub async fn equalizer_export_file(
    app: AppHandle,
    service: State<'_, Arc<EqualizerService>>,
    profile_id: String,
    destination: String,
) -> AppResult<()> {
    let snapshot = service.snapshot().await?;
    let profiles = match snapshot.active_layer {
        ProfileLayer::Synced => &snapshot.synced.profiles,
        ProfileLayer::LocalOnly => &snapshot.local.profiles,
    };
    let profile = profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| AppError::Internal(format!("EQ profile {profile_id} not found")))?;
    let text = crate::equalizer::parser::export_equalizer_text(profile)
        .map_err(|error| AppError::Internal(error.to_string()))?;
    let fp: FilePath = destination.parse().expect("FilePath parsing is infallible");
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let mut file = app.fs().open(fp, options)?;
        file.write_all(text.as_bytes())?;
        file.flush()
    })
    .await
    .map_err(|error| AppError::Internal(format!("write EQ task: {error}")))?
    .map_err(|error| AppError::Internal(format!("write EQ file: {error}")))
}

#[tauri::command]
pub async fn equalizer_list_changes(
    service: State<'_, Arc<EqualizerService>>,
    subject_user_id: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> AppResult<ChangePage> {
    service
        .list_changes(subject_user_id.as_deref(), cursor.as_deref(), limit)
        .await
}

#[tauri::command]
pub async fn equalizer_get_change(
    service: State<'_, Arc<EqualizerService>>,
    audit_id: String,
) -> AppResult<EqualizerChangeDetail> {
    service.get_change(&audit_id).await
}

#[tauri::command]
pub async fn equalizer_rollback_change(
    service: State<'_, Arc<EqualizerService>>,
    audit_id: String,
    expected_state_revision: Revision,
) -> AppResult<EqualizerRollbackResponse> {
    service
        .rollback_change(&audit_id, expected_state_revision)
        .await
}
