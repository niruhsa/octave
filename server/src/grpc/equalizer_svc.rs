//! gRPC transport for synchronized equalizer configuration and its audit log.

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, EntityRevision, EqualizerRuleAction, ProfileDeleteDisposition};
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::equalizer as pb;
use crate::services::{EqualizerDeviceRuleInput, EqualizerProfileInput, EqualizerService};
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct EqualizerServer {
    pub equalizer: EqualizerService,
    pub interceptor: AuthInterceptor,
}

impl EqualizerServer {
    pub fn into_service(self) -> pb::equalizer_service_server::EqualizerServiceServer<Self> {
        pb::equalizer_service_server::EqualizerServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }
}

#[tonic::async_trait]
impl pb::equalizer_service_server::EqualizerService for EqualizerServer {
    async fn get_equalizer_state(
        &self,
        req: Request<pb::GetEqualizerStateRequest>,
    ) -> Result<Response<pb::GetEqualizerStateResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let out = self
            .equalizer
            .get_state(&caller, body.known_state_revision)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetEqualizerStateResponse {
            not_modified: out.not_modified,
            state: out.state.map(state_to_pb),
        }))
    }

    async fn create_equalizer_profile(
        &self,
        req: Request<pb::CreateEqualizerProfileRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let input = profile_input(required(req.into_inner().profile, "profile")?)?;
        let out = self
            .equalizer
            .create_profile(&caller, input)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn update_equalizer_profile(
        &self,
        req: Request<pb::UpdateEqualizerProfileRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.profile_id, "profile_id")?;
        let input = profile_input(required(body.profile, "profile")?)?;
        let out = self
            .equalizer
            .update_profile(&caller, id, body.expected_revision, input)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn delete_equalizer_profile(
        &self,
        req: Request<pb::DeleteEqualizerProfileRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.profile_id, "profile_id")?;
        let refs = body
            .referencing_rules
            .into_iter()
            .map(entity_revision)
            .collect::<Result<Vec<_>, _>>()?;
        let disposition = match body.disposition {
            Some(pb::delete_equalizer_profile_request::Disposition::RejectIfReferenced(true)) => {
                ProfileDeleteDisposition::RejectIfReferenced
            }
            Some(pb::delete_equalizer_profile_request::Disposition::ReplaceWithProfileId(id)) => {
                ProfileDeleteDisposition::ReplaceWithProfile {
                    profile_id: parse_uuid(&id, "replacement profile id")?,
                }
            }
            Some(pb::delete_equalizer_profile_request::Disposition::ReplaceWithFlat(true)) => {
                ProfileDeleteDisposition::ReplaceWithFlat
            }
            _ => {
                return Err(Status::invalid_argument(
                    "profile delete disposition required",
                ));
            }
        };
        let out = self
            .equalizer
            .delete_profile(
                &caller,
                id,
                body.expected_revision,
                body.expected_settings_revision,
                refs,
                disposition,
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn update_equalizer_settings(
        &self,
        req: Request<pb::UpdateEqualizerSettingsRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let default_profile_id = match body.default_assignment {
            Some(pb::update_equalizer_settings_request::DefaultAssignment::DefaultProfileId(
                id,
            )) => Some(parse_uuid(&id, "default profile id")?),
            Some(pb::update_equalizer_settings_request::DefaultAssignment::Flat(true)) => None,
            _ => return Err(Status::invalid_argument("default assignment required")),
        };
        let out = self
            .equalizer
            .update_settings(&caller, body.expected_settings_revision, default_profile_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn create_equalizer_device_rule(
        &self,
        req: Request<pb::CreateEqualizerDeviceRuleRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let input = rule_input(required(req.into_inner().rule, "rule")?)?;
        let out = self
            .equalizer
            .create_rule(&caller, input)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn update_equalizer_device_rule(
        &self,
        req: Request<pb::UpdateEqualizerDeviceRuleRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.rule_id, "rule_id")?;
        let input = rule_input(required(body.rule, "rule")?)?;
        let out = self
            .equalizer
            .update_rule(&caller, id, body.expected_revision, input)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn delete_equalizer_device_rule(
        &self,
        req: Request<pb::DeleteEqualizerDeviceRuleRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.rule_id, "rule_id")?;
        let out = self
            .equalizer
            .delete_rule(&caller, id, body.expected_revision)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn reorder_equalizer_device_rules(
        &self,
        req: Request<pb::ReorderEqualizerDeviceRulesRequest>,
    ) -> Result<Response<pb::EqualizerMutationResponse>, Status> {
        let caller = self.caller(&req).await?;
        let rules = req
            .into_inner()
            .rules
            .into_iter()
            .map(entity_revision)
            .collect::<Result<Vec<_>, _>>()?;
        let out = self
            .equalizer
            .reorder_rules(&caller, rules)
            .await
            .map_err(map_err)?;
        Ok(Response::new(mutation_to_pb(out)))
    }

    async fn list_equalizer_changes(
        &self,
        req: Request<pb::ListEqualizerChangesRequest>,
    ) -> Result<Response<pb::ListEqualizerChangesResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let subject = body
            .subject_user_id
            .as_deref()
            .map(|id| parse_uuid(id, "subject_user_id"))
            .transpose()?;
        let out = self
            .equalizer
            .list_changes(
                &caller,
                subject,
                body.cursor.as_deref(),
                (body.limit != 0).then_some(body.limit),
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListEqualizerChangesResponse {
            changes: out.changes.into_iter().map(change_to_pb).collect(),
            next_cursor: out.next_cursor,
        }))
    }

    async fn get_equalizer_change(
        &self,
        req: Request<pb::GetEqualizerChangeRequest>,
    ) -> Result<Response<pb::EqualizerChangeDetail>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().audit_id, "audit_id")?;
        let out = self
            .equalizer
            .get_change(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::EqualizerChangeDetail {
            change: Some(change_to_pb(out.change)),
            before_json: out.before_json,
            after_json: out.after_json,
            current_state_revision: out.current_state_revision,
            rollback_eligible: out.rollback_eligible,
        }))
    }

    async fn rollback_equalizer_change(
        &self,
        req: Request<pb::RollbackEqualizerChangeRequest>,
    ) -> Result<Response<pb::EqualizerRollbackResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.audit_id, "audit_id")?;
        let out = self
            .equalizer
            .rollback_change(&caller, id, body.expected_state_revision)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::EqualizerRollbackResponse {
            target_owner_id: out.target_owner_id.to_string(),
            state_revision: out.state_revision,
            audit_id: out.audit_id.to_string(),
            changed_resources: out
                .changed_resources
                .into_iter()
                .map(|r| pb::EqualizerChangedResource {
                    resource_type: r.resource_type,
                    resource_id: r.resource_id.map(|id| id.to_string()),
                    change: r.change,
                })
                .collect(),
        }))
    }
}

fn required<T>(value: Option<T>, field: &str) -> Result<T, Status> {
    value.ok_or_else(|| Status::invalid_argument(format!("{field} is required")))
}

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(value).map_err(|_| Status::invalid_argument(format!("invalid {field} uuid")))
}

fn profile_input(value: pb::EqualizerProfileInput) -> Result<EqualizerProfileInput, Status> {
    Ok(EqualizerProfileInput {
        id: parse_uuid(&value.id, "profile id")?,
        name: value.name,
        format_version: i32::try_from(value.format_version)
            .map_err(|_| Status::invalid_argument("format_version out of range"))?,
        preamp_db: value.preamp_db,
        auto_headroom_enabled: value.auto_headroom_enabled,
        bands: value
            .bands
            .into_iter()
            .map(|b| {
                Ok(m::EqualizerBand {
                    position: i32::try_from(b.position)
                        .map_err(|_| Status::invalid_argument("band position out of range"))?,
                    enabled: b.enabled,
                    filter_type: b.filter_type,
                    frequency_hz: b.frequency_hz,
                    gain_db: b.gain_db,
                    q: b.q,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?,
    })
}

fn rule_input(value: pb::EqualizerDeviceRuleInput) -> Result<EqualizerDeviceRuleInput, Status> {
    let action = action_from_pb(required(value.action, "rule action")?)?;
    Ok(EqualizerDeviceRuleInput {
        id: parse_uuid(&value.id, "rule id")?,
        label: value.label,
        action,
        selectors: value
            .selectors
            .into_iter()
            .map(|s| m::PortableDeviceSelector {
                normalization_version: i32::try_from(s.normalization_version).unwrap_or(i32::MAX),
                route_kind: s.route_kind,
                normalized_name: s.normalized_name,
                vendor_id: s.vendor_id,
                product_id: s.product_id,
                platform_scope: s.platform_scope,
                trigger: s.trigger,
            })
            .collect(),
        enabled: value.enabled,
    })
}

fn action_from_pb(value: pb::EqualizerRuleAction) -> Result<EqualizerRuleAction, Status> {
    match value.action {
        Some(pb::equalizer_rule_action::Action::ProfileId(id)) => {
            Ok(EqualizerRuleAction::Profile {
                profile_id: parse_uuid(&id, "rule profile id")?,
            })
        }
        Some(pb::equalizer_rule_action::Action::Bypass(true)) => Ok(EqualizerRuleAction::Bypass),
        _ => Err(Status::invalid_argument("rule action required")),
    }
}

fn action_to_pb(value: EqualizerRuleAction) -> pb::EqualizerRuleAction {
    let action = match value {
        EqualizerRuleAction::Profile { profile_id } => {
            pb::equalizer_rule_action::Action::ProfileId(profile_id.to_string())
        }
        EqualizerRuleAction::Bypass => pb::equalizer_rule_action::Action::Bypass(true),
    };
    pb::EqualizerRuleAction {
        action: Some(action),
    }
}

fn entity_revision(value: pb::EntityRevision) -> Result<EntityRevision, Status> {
    Ok(EntityRevision {
        id: parse_uuid(&value.id, "entity id")?,
        expected_revision: value.expected_revision,
    })
}

fn state_to_pb(value: m::EqualizerState) -> pb::EqualizerState {
    pb::EqualizerState {
        state_format_version: value.state_format_version as u32,
        state_revision: value.state_revision,
        settings_revision: value.settings_revision,
        default_profile_id: value.default_profile_id.map(|id| id.to_string()),
        profiles: value.profiles.into_iter().map(profile_to_pb).collect(),
        device_rules: value.device_rules.into_iter().map(rule_to_pb).collect(),
    }
}

fn profile_to_pb(value: m::EqualizerProfile) -> pb::EqualizerProfile {
    pb::EqualizerProfile {
        id: value.id.to_string(),
        name: value.name,
        format_version: value.format_version as u32,
        preamp_db: value.preamp_db,
        auto_headroom_enabled: value.auto_headroom_enabled,
        bands: value
            .bands
            .into_iter()
            .map(|b| pb::EqualizerBand {
                position: b.position as u32,
                enabled: b.enabled,
                filter_type: b.filter_type,
                frequency_hz: b.frequency_hz,
                gain_db: b.gain_db,
                q: b.q,
            })
            .collect(),
        revision: value.revision,
        created_at: rfc3339(value.created_at),
        updated_at: rfc3339(value.updated_at),
    }
}

fn rule_to_pb(value: m::EqualizerDeviceRule) -> pb::EqualizerDeviceRule {
    pb::EqualizerDeviceRule {
        id: value.id.to_string(),
        label: value.label,
        action: Some(action_to_pb(value.action)),
        selectors: value
            .selectors
            .into_iter()
            .map(|s| pb::PortableDeviceSelector {
                normalization_version: s.normalization_version as u32,
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
        revision: value.revision,
        created_at: rfc3339(value.created_at),
        updated_at: rfc3339(value.updated_at),
    }
}

fn mutation_to_pb(value: m::EqualizerMutationOutcome) -> pb::EqualizerMutationResponse {
    pb::EqualizerMutationResponse {
        changed: value.changed,
        audit_id: value.audit_id.map(|id| id.to_string()),
        state: Some(state_to_pb(value.state)),
    }
}

fn change_to_pb(value: m::EqualizerChangeSummary) -> pb::EqualizerChangeSummary {
    pb::EqualizerChangeSummary {
        audit_id: value.audit_id.to_string(),
        action: value.action,
        actor_id: value.actor_id.map(|id| id.to_string()),
        owner_id: value.owner_id.to_string(),
        resource_type: value.resource_type,
        resource_id: value.resource_id.map(|id| id.to_string()),
        created_at: rfc3339(value.created_at),
        before_state_revision: value.before_state_revision,
        after_state_revision: value.after_state_revision,
    }
}
