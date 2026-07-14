//! REST fallback for synchronized equalizer configuration. Revisions are
//! decimal strings so JavaScript never truncates an i64 compare-and-swap token.

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    self as m, EntityRevision, EqualizerRuleAction, PortableDeviceSelector,
    ProfileDeleteDisposition,
};
use crate::error::AppError;
use crate::rest::{ApiError, RestState};
use crate::services::{EqualizerDeviceRuleInput, EqualizerProfileInput};
use crate::time_fmt::rfc3339;

const BODY_LIMIT: usize = 64 * 1024;

pub fn router() -> Router<RestState> {
    Router::new()
        .route("/equalizer/state", get(get_state))
        .route("/equalizer/profiles", post(create_profile))
        .route("/equalizer/profiles/:id", put(update_profile))
        .route("/equalizer/profiles/:id/delete", post(delete_profile))
        .route("/equalizer/settings", put(update_settings))
        .route("/equalizer/device-rules", post(create_rule))
        .route("/equalizer/device-rules/order", put(reorder_rules))
        .route("/equalizer/device-rules/:id", put(update_rule))
        .route("/equalizer/device-rules/:id/delete", post(delete_rule))
        .route("/equalizer/changes", get(list_changes))
        .route("/equalizer/changes/:audit_id", get(get_change))
        .route(
            "/equalizer/changes/:audit_id/rollback",
            post(rollback_change),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

#[derive(Debug, Deserialize)]
struct StateQuery {
    known_state_revision: Option<String>,
}

async fn get_state(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Query(query): Query<StateQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let query_revision = query
        .known_state_revision
        .as_deref()
        .map(|v| parse_revision(v, "known_state_revision"))
        .transpose()?;
    let etag_revision = parse_etag(headers.get(header::IF_NONE_MATCH));
    let known = etag_revision.or(query_revision);
    let out = s.equalizer.get_state(&caller, known).await?;
    if out.not_modified {
        let mut response = if etag_revision.is_some() {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            Json(GetStateDto {
                not_modified: true,
                state: None,
            })
            .into_response()
        };
        if let Some(revision) = known {
            set_cache_headers(response.headers_mut(), revision);
        }
        return Ok(response);
    }
    let state = out
        .state
        .ok_or_else(|| AppError::Internal("equalizer state response missing state".into()))?;
    let revision = state.state_revision;
    let mut response = Json(GetStateDto {
        not_modified: false,
        state: Some(state_dto(state)),
    })
    .into_response();
    set_cache_headers(response.headers_mut(), revision);
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct ProfileInputDto {
    id: String,
    name: String,
    format_version: u32,
    preamp_db: f64,
    auto_headroom_enabled: bool,
    bands: Vec<BandDto>,
}

impl ProfileInputDto {
    fn into_input(self) -> Result<EqualizerProfileInput, AppError> {
        Ok(EqualizerProfileInput {
            id: parse_uuid(&self.id, "profile id")?,
            name: self.name,
            format_version: i32::try_from(self.format_version)
                .map_err(|_| AppError::InvalidArgument("format_version out of range".into()))?,
            preamp_db: self.preamp_db,
            auto_headroom_enabled: self.auto_headroom_enabled,
            bands: self
                .bands
                .into_iter()
                .map(|b| {
                    Ok(m::EqualizerBand {
                        position: i32::try_from(b.position).map_err(|_| {
                            AppError::InvalidArgument("band position out of range".into())
                        })?,
                        enabled: b.enabled,
                        filter_type: b.filter_type,
                        frequency_hz: b.frequency_hz,
                        gain_db: b.gain_db,
                        q: b.q,
                    })
                })
                .collect::<Result<Vec<_>, AppError>>()?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CreateProfileBody {
    profile: ProfileInputDto,
}

async fn create_profile(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Json(body): Json<CreateProfileBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let out = s
        .equalizer
        .create_profile(&caller, body.profile.into_input()?)
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
struct UpdateProfileBody {
    expected_revision: String,
    profile: ProfileInputDto,
}

async fn update_profile(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateProfileBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let out = s
        .equalizer
        .update_profile(
            &caller,
            id,
            parse_revision(&body.expected_revision, "expected_revision")?,
            body.profile.into_input()?,
        )
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
struct RevisionDto {
    id: String,
    expected_revision: String,
}

impl RevisionDto {
    fn into_model(self) -> Result<EntityRevision, AppError> {
        Ok(EntityRevision {
            id: parse_uuid(&self.id, "entity id")?,
            expected_revision: parse_revision(&self.expected_revision, "expected_revision")?,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DeleteDispositionDto {
    RejectIfReferenced,
    ReplaceWithProfile { profile_id: String },
    ReplaceWithFlat,
}

#[derive(Debug, Deserialize)]
struct DeleteProfileBody {
    expected_revision: String,
    expected_settings_revision: String,
    #[serde(default)]
    referencing_rules: Vec<RevisionDto>,
    disposition: DeleteDispositionDto,
}

async fn delete_profile(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<DeleteProfileBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let disposition = match body.disposition {
        DeleteDispositionDto::RejectIfReferenced => ProfileDeleteDisposition::RejectIfReferenced,
        DeleteDispositionDto::ReplaceWithProfile { profile_id } => {
            ProfileDeleteDisposition::ReplaceWithProfile {
                profile_id: parse_uuid(&profile_id, "replacement profile id")?,
            }
        }
        DeleteDispositionDto::ReplaceWithFlat => ProfileDeleteDisposition::ReplaceWithFlat,
    };
    let refs = body
        .referencing_rules
        .into_iter()
        .map(RevisionDto::into_model)
        .collect::<Result<Vec<_>, _>>()?;
    let out = s
        .equalizer
        .delete_profile(
            &caller,
            id,
            parse_revision(&body.expected_revision, "expected_revision")?,
            parse_revision(
                &body.expected_settings_revision,
                "expected_settings_revision",
            )?,
            refs,
            disposition,
        )
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DefaultAssignmentDto {
    Profile { profile_id: String },
    Flat,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsBody {
    expected_settings_revision: String,
    default_assignment: DefaultAssignmentDto,
}

async fn update_settings(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Json(body): Json<UpdateSettingsBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let default = match body.default_assignment {
        DefaultAssignmentDto::Profile { profile_id } => {
            Some(parse_uuid(&profile_id, "default profile id")?)
        }
        DefaultAssignmentDto::Flat => None,
    };
    let out = s
        .equalizer
        .update_settings(
            &caller,
            parse_revision(
                &body.expected_settings_revision,
                "expected_settings_revision",
            )?,
            default,
        )
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuleActionDto {
    Profile { profile_id: String },
    Bypass,
}

impl RuleActionDto {
    fn into_model(self) -> Result<EqualizerRuleAction, AppError> {
        Ok(match self {
            RuleActionDto::Profile { profile_id } => EqualizerRuleAction::Profile {
                profile_id: parse_uuid(&profile_id, "rule profile id")?,
            },
            RuleActionDto::Bypass => EqualizerRuleAction::Bypass,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SelectorInputDto {
    normalization_version: u32,
    route_kind: String,
    normalized_name: String,
    vendor_id: Option<String>,
    product_id: Option<String>,
    platform_scope: Option<String>,
    trigger: String,
}

#[derive(Debug, Deserialize)]
struct RuleInputDto {
    id: String,
    label: String,
    action: RuleActionDto,
    selectors: Vec<SelectorInputDto>,
    enabled: bool,
    #[serde(default)]
    bass_boost_percent: i32,
    #[serde(default)]
    treble_boost_percent: i32,
}

impl RuleInputDto {
    fn into_input(self) -> Result<EqualizerDeviceRuleInput, AppError> {
        Ok(EqualizerDeviceRuleInput {
            id: parse_uuid(&self.id, "rule id")?,
            label: self.label,
            action: self.action.into_model()?,
            selectors: self
                .selectors
                .into_iter()
                .map(|s| PortableDeviceSelector {
                    normalization_version: i32::try_from(s.normalization_version)
                        .unwrap_or(i32::MAX),
                    route_kind: s.route_kind,
                    normalized_name: s.normalized_name,
                    vendor_id: s.vendor_id,
                    product_id: s.product_id,
                    platform_scope: s.platform_scope,
                    trigger: s.trigger,
                })
                .collect(),
            enabled: self.enabled,
            bass_boost_percent: self.bass_boost_percent,
            treble_boost_percent: self.treble_boost_percent,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CreateRuleBody {
    rule: RuleInputDto,
}

async fn create_rule(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Json(body): Json<CreateRuleBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let out = s
        .equalizer
        .create_rule(&caller, body.rule.into_input()?)
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
struct UpdateRuleBody {
    expected_revision: String,
    rule: RuleInputDto,
}

async fn update_rule(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateRuleBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let out = s
        .equalizer
        .update_rule(
            &caller,
            id,
            parse_revision(&body.expected_revision, "expected_revision")?,
            body.rule.into_input()?,
        )
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
struct DeleteRuleBody {
    expected_revision: String,
}

async fn delete_rule(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<DeleteRuleBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let out = s
        .equalizer
        .delete_rule(
            &caller,
            id,
            parse_revision(&body.expected_revision, "expected_revision")?,
        )
        .await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
struct ReorderRulesBody {
    rules: Vec<RevisionDto>,
}

async fn reorder_rules(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Json(body): Json<ReorderRulesBody>,
) -> Result<Json<MutationDto>, ApiError> {
    let rules = body
        .rules
        .into_iter()
        .map(RevisionDto::into_model)
        .collect::<Result<Vec<_>, _>>()?;
    let out = s.equalizer.reorder_rules(&caller, rules).await?;
    Ok(Json(mutation_dto(out)))
}

#[derive(Debug, Deserialize)]
struct ChangesQuery {
    subject_user_id: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

async fn list_changes(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Query(query): Query<ChangesQuery>,
) -> Result<Json<ChangesPageDto>, ApiError> {
    let subject = query
        .subject_user_id
        .as_deref()
        .map(|id| parse_uuid(id, "subject_user_id"))
        .transpose()?;
    let out = s
        .equalizer
        .list_changes(&caller, subject, query.cursor.as_deref(), query.limit)
        .await?;
    Ok(Json(ChangesPageDto {
        changes: out.changes.into_iter().map(change_dto).collect(),
        next_cursor: out.next_cursor,
    }))
}

async fn get_change(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Path(audit_id): Path<Uuid>,
) -> Result<Json<ChangeDetailDto>, ApiError> {
    let out = s.equalizer.get_change(&caller, audit_id).await?;
    Ok(Json(ChangeDetailDto {
        change: change_dto(out.change),
        before_json: out.before_json,
        after_json: out.after_json,
        current_state_revision: out.current_state_revision.map(|r| r.to_string()),
        rollback_eligible: out.rollback_eligible,
    }))
}

#[derive(Debug, Deserialize)]
struct RollbackBody {
    expected_state_revision: String,
}

async fn rollback_change(
    State(s): State<RestState>,
    Extension(caller): Extension<Identity>,
    Path(audit_id): Path<Uuid>,
    Json(body): Json<RollbackBody>,
) -> Result<Json<RollbackDto>, ApiError> {
    let out = s
        .equalizer
        .rollback_change(
            &caller,
            audit_id,
            parse_revision(&body.expected_state_revision, "expected_state_revision")?,
        )
        .await?;
    Ok(Json(RollbackDto {
        target_owner_id: out.target_owner_id.to_string(),
        state_revision: out.state_revision.to_string(),
        audit_id: out.audit_id.to_string(),
        changed_resources: out
            .changed_resources
            .into_iter()
            .map(|r| ChangedResourceDto {
                resource_type: r.resource_type,
                resource_id: r.resource_id.map(|id| id.to_string()),
                change: r.change,
            })
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
struct GetStateDto {
    not_modified: bool,
    state: Option<StateDto>,
}

#[derive(Debug, Serialize)]
struct MutationDto {
    changed: bool,
    audit_id: Option<String>,
    state: StateDto,
}

#[derive(Debug, Serialize)]
struct StateDto {
    state_format_version: i32,
    state_revision: String,
    settings_revision: String,
    default_profile_id: Option<String>,
    profiles: Vec<ProfileDto>,
    device_rules: Vec<RuleDto>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BandDto {
    position: u32,
    enabled: bool,
    filter_type: String,
    frequency_hz: f64,
    gain_db: f64,
    q: f64,
}

#[derive(Debug, Serialize)]
struct ProfileDto {
    id: String,
    name: String,
    format_version: i32,
    preamp_db: f64,
    auto_headroom_enabled: bool,
    bands: Vec<BandDto>,
    revision: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuleActionOutDto {
    Profile { profile_id: String },
    Bypass,
}

#[derive(Debug, Serialize)]
struct SelectorDto {
    normalization_version: i32,
    route_kind: String,
    normalized_name: String,
    vendor_id: Option<String>,
    product_id: Option<String>,
    platform_scope: Option<String>,
    trigger: String,
}

#[derive(Debug, Serialize)]
struct RuleDto {
    id: String,
    label: String,
    action: RuleActionOutDto,
    selectors: Vec<SelectorDto>,
    priority: i32,
    enabled: bool,
    bass_boost_percent: i32,
    treble_boost_percent: i32,
    revision: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
struct ChangesPageDto {
    changes: Vec<ChangeSummaryDto>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChangeSummaryDto {
    audit_id: String,
    action: String,
    actor_id: Option<String>,
    owner_id: String,
    resource_type: String,
    resource_id: Option<String>,
    created_at: String,
    before_state_revision: String,
    after_state_revision: String,
}

#[derive(Debug, Serialize)]
struct ChangeDetailDto {
    change: ChangeSummaryDto,
    before_json: Option<String>,
    after_json: Option<String>,
    current_state_revision: Option<String>,
    rollback_eligible: bool,
}

#[derive(Debug, Serialize)]
struct ChangedResourceDto {
    resource_type: String,
    resource_id: Option<String>,
    change: String,
}

#[derive(Debug, Serialize)]
struct RollbackDto {
    target_owner_id: String,
    state_revision: String,
    audit_id: String,
    changed_resources: Vec<ChangedResourceDto>,
}

fn mutation_dto(value: m::EqualizerMutationOutcome) -> MutationDto {
    MutationDto {
        changed: value.changed,
        audit_id: value.audit_id.map(|id| id.to_string()),
        state: state_dto(value.state),
    }
}

fn state_dto(value: m::EqualizerState) -> StateDto {
    StateDto {
        state_format_version: value.state_format_version,
        state_revision: value.state_revision.to_string(),
        settings_revision: value.settings_revision.to_string(),
        default_profile_id: value.default_profile_id.map(|id| id.to_string()),
        profiles: value.profiles.into_iter().map(profile_dto).collect(),
        device_rules: value.device_rules.into_iter().map(rule_dto).collect(),
    }
}

fn profile_dto(value: m::EqualizerProfile) -> ProfileDto {
    ProfileDto {
        id: value.id.to_string(),
        name: value.name,
        format_version: value.format_version,
        preamp_db: value.preamp_db,
        auto_headroom_enabled: value.auto_headroom_enabled,
        bands: value
            .bands
            .into_iter()
            .map(|b| BandDto {
                position: b.position as u32,
                enabled: b.enabled,
                filter_type: b.filter_type,
                frequency_hz: b.frequency_hz,
                gain_db: b.gain_db,
                q: b.q,
            })
            .collect(),
        revision: value.revision.to_string(),
        created_at: rfc3339(value.created_at),
        updated_at: rfc3339(value.updated_at),
    }
}

fn rule_dto(value: m::EqualizerDeviceRule) -> RuleDto {
    RuleDto {
        id: value.id.to_string(),
        label: value.label,
        action: match value.action {
            EqualizerRuleAction::Profile { profile_id } => RuleActionOutDto::Profile {
                profile_id: profile_id.to_string(),
            },
            EqualizerRuleAction::Bypass => RuleActionOutDto::Bypass,
        },
        selectors: value
            .selectors
            .into_iter()
            .map(|s| SelectorDto {
                normalization_version: s.normalization_version,
                route_kind: s.route_kind,
                normalized_name: s.normalized_name,
                vendor_id: s.vendor_id,
                product_id: s.product_id,
                platform_scope: s.platform_scope,
                trigger: s.trigger,
            })
            .collect(),
        priority: value.priority,
        enabled: value.enabled,
        bass_boost_percent: value.bass_boost_percent,
        treble_boost_percent: value.treble_boost_percent,
        revision: value.revision.to_string(),
        created_at: rfc3339(value.created_at),
        updated_at: rfc3339(value.updated_at),
    }
}

fn change_dto(value: m::EqualizerChangeSummary) -> ChangeSummaryDto {
    ChangeSummaryDto {
        audit_id: value.audit_id.to_string(),
        action: value.action,
        actor_id: value.actor_id.map(|id| id.to_string()),
        owner_id: value.owner_id.to_string(),
        resource_type: value.resource_type,
        resource_id: value.resource_id.map(|id| id.to_string()),
        created_at: rfc3339(value.created_at),
        before_state_revision: value.before_state_revision.to_string(),
        after_state_revision: value.after_state_revision.to_string(),
    }
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| AppError::InvalidArgument(format!("invalid {field} uuid")))
}

fn parse_revision(value: &str, field: &str) -> Result<i64, AppError> {
    value
        .parse::<i64>()
        .map_err(|_| AppError::InvalidArgument(format!("invalid {field}")))
}

fn parse_etag(value: Option<&HeaderValue>) -> Option<i64> {
    let value = value?.to_str().ok()?.trim();
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    value.strip_prefix("eq-")?.parse().ok()
}

fn set_cache_headers(headers: &mut HeaderMap, revision: i64) {
    if let Ok(value) = HeaderValue::from_str(&format!("\"eq-{revision}\"")) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Authorization"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_round_trip_and_revision_parse() {
        let value = HeaderValue::from_static("\"eq-9223372036854775807\"");
        assert_eq!(parse_etag(Some(&value)), Some(i64::MAX));
        assert_eq!(
            parse_revision("9223372036854775807", "revision").unwrap(),
            i64::MAX
        );
        assert!(parse_revision("1.5", "revision").is_err());
    }

    #[test]
    fn cache_headers_are_private_and_vary_by_auth() {
        let mut headers = HeaderMap::new();
        set_cache_headers(&mut headers, 42);
        assert_eq!(headers[header::ETAG], "\"eq-42\"");
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-cache");
        assert_eq!(headers[header::VARY], "Authorization");
    }
}
