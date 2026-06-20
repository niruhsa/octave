//! gRPC auth interceptor: pulls credentials from request metadata and exposes
//! `resolve()` for handlers to turn them into an [`Identity`].

use tonic::service::Interceptor;
use tonic::{Request, Status};

use crate::auth::service::{AuthService, Credential};
use crate::auth::Identity;
use crate::error::AppError;

#[derive(Clone)]
pub struct AuthInterceptor {
    auth: AuthService,
}

impl AuthInterceptor {
    pub fn new(auth: AuthService) -> Self {
        Self { auth }
    }

    pub async fn resolve<T>(&self, request: &Request<T>) -> std::result::Result<Identity, Status> {
        let cred = extract_credential(request.metadata())
            .ok_or_else(|| Status::unauthenticated("missing Authorization metadata"))?;
        self.auth.resolve(cred).await.map_err(|e| match e {
            AppError::Unauthenticated(m) => Status::unauthenticated(m),
            AppError::PermissionDenied(m) => Status::permission_denied(m),
            other => Status::internal(other.to_string()),
        })
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, request: Request<()>) -> std::result::Result<Request<()>, Status> {
        if extract_credential(request.metadata()).is_none() {
            return Err(Status::unauthenticated("missing Authorization metadata"));
        }
        Ok(request)
    }
}

pub fn extract_credential(meta: &tonic::metadata::MetadataMap) -> Option<Credential> {
    if let Some(v) = meta.get("authorization").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if let Some(rest) = ci_strip(v, "Bearer ") {
            return Some(Credential::Bearer(rest.trim().to_string()));
        }
        if let Some(rest) = ci_strip(v, "SecretKey ") {
            return Some(Credential::SecretKey(rest.trim().to_string()));
        }
    }
    if let Some(v) = meta.get("x-secret-key").and_then(|v| v.to_str().ok()) {
        return Some(Credential::SecretKey(v.trim().to_string()));
    }
    None
}

fn ci_strip<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}
