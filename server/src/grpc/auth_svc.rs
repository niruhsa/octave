//! gRPC AuthService implementation.

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::auth::service::{AuthService, Credential};
use crate::db::models::PermissionLevel;
use crate::error::AppError;
use crate::grpc::{interceptor::AuthInterceptor, proto::auth as pb};
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct AuthServer {
    pub auth: AuthService,
    pub interceptor: AuthInterceptor,
}

impl AuthServer {
    pub fn into_service(self) -> pb::auth_service_server::AuthServiceServer<Self> {
        pb::auth_service_server::AuthServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl pb::auth_service_server::AuthService for AuthServer {
    async fn login(
        &self,
        req: Request<pb::LoginRequest>,
    ) -> Result<Response<pb::LoginResponse>, Status> {
        let body = req.into_inner();
        let out = self
            .auth
            .login(&body.username, &body.password)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::LoginResponse {
            token: out.token,
            user_id: out.user_id.to_string(),
            level: to_pb_level(out.level) as i32,
            expires_at: rfc3339(out.expires_at),
        }))
    }

    async fn logout(
        &self,
        req: Request<pb::LogoutRequest>,
    ) -> Result<Response<pb::LogoutResponse>, Status> {
        if let Some(Credential::Bearer(t)) = current_credential(&req) {
            self.auth.logout(&t).await.map_err(map_err)?;
        }
        Ok(Response::new(pb::LogoutResponse {}))
    }

    async fn who_am_i(
        &self,
        req: Request<pb::WhoAmIRequest>,
    ) -> Result<Response<pb::WhoAmIResponse>, Status> {
        let id = self.interceptor.resolve(&req).await?;
        let resp = match id {
            Identity::SecretKey => pb::WhoAmIResponse {
                kind: "secret_key".into(),
                user_id: String::new(),
                username: String::new(),
                level: pb::PermissionLevel::Admin as i32,
            },
            Identity::User {
                id,
                username,
                level,
            } => pb::WhoAmIResponse {
                kind: "user".into(),
                user_id: id.to_string(),
                username,
                level: to_pb_level(level) as i32,
            },
        };
        Ok(Response::new(resp))
    }

    async fn register(
        &self,
        req: Request<pb::RegisterRequest>,
    ) -> Result<Response<pb::RegisterResponse>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let body = req.into_inner();
        let level = from_pb_level(body.level())?;
        let id = self
            .auth
            .register(&caller, &body.username, &body.password, level)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::RegisterResponse {
            user_id: id.to_string(),
        }))
    }

    async fn change_password(
        &self,
        req: Request<pb::ChangePasswordRequest>,
    ) -> Result<Response<pb::ChangePasswordResponse>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let body = req.into_inner();
        let target_id = Uuid::parse_str(&body.user_id)
            .map_err(|e| Status::invalid_argument(format!("invalid user_id: {e}")))?;
        let old = if body.old_password.is_empty() {
            None
        } else {
            Some(body.old_password.as_str())
        };
        self.auth
            .change_password(&caller, target_id, old, &body.new_password)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ChangePasswordResponse {}))
    }

    async fn list_users(
        &self,
        req: Request<pb::ListUsersRequest>,
    ) -> Result<Response<pb::ListUsersResponse>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let users = self.auth.list_users(&caller).await.map_err(map_err)?;
        Ok(Response::new(pb::ListUsersResponse {
            users: users
                .into_iter()
                .map(|u| pb::list_users_response::UserEntry {
                    id: u.id.to_string(),
                    username: u.username,
                    level: to_pb_level(u.level) as i32,
                })
                .collect(),
        }))
    }

    async fn delete_user(
        &self,
        req: Request<pb::DeleteUserRequest>,
    ) -> Result<Response<pb::DeleteUserResponse>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let target_id = Uuid::parse_str(&req.into_inner().user_id)
            .map_err(|e| Status::invalid_argument(format!("invalid user_id: {e}")))?;
        self.auth
            .delete_user(&caller, target_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::DeleteUserResponse {}))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn current_credential<T>(req: &Request<T>) -> Option<Credential> {
    super::interceptor::extract_credential(req.metadata())
}

pub fn to_pb_level(level: PermissionLevel) -> pb::PermissionLevel {
    match level {
        PermissionLevel::User => pb::PermissionLevel::User,
        PermissionLevel::Manager => pb::PermissionLevel::Manager,
        PermissionLevel::Admin => pb::PermissionLevel::Admin,
    }
}

pub fn from_pb_level(level: pb::PermissionLevel) -> Result<PermissionLevel, Status> {
    match level {
        pb::PermissionLevel::User => Ok(PermissionLevel::User),
        pb::PermissionLevel::Manager => Ok(PermissionLevel::Manager),
        pb::PermissionLevel::Admin => Ok(PermissionLevel::Admin),
        pb::PermissionLevel::Unspecified => {
            Err(Status::invalid_argument("permission level required"))
        }
    }
}

pub fn map_err(e: AppError) -> Status {
    match e {
        AppError::Unauthenticated(m) => Status::unauthenticated(m),
        AppError::PermissionDenied(m) => Status::permission_denied(m),
        AppError::NotFound(m) => Status::not_found(m),
        AppError::InvalidArgument(m) => Status::invalid_argument(m),
        AppError::Conflict { code, message } => {
            let mut status = Status::aborted(message);
            if let Ok(value) = code.parse() {
                status.metadata_mut().insert("x-octave-error-code", value);
            }
            status
        }
        other => Status::internal(other.to_string()),
    }
}
