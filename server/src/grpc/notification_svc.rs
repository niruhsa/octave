//! gRPC NotificationService implementation (Phase 10 — follows & notifications).

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::notification as pb;
use crate::services::NotificationService;

#[derive(Clone)]
pub struct NotificationServer {
    pub notifications: NotificationService,
    pub interceptor: AuthInterceptor,
}

impl NotificationServer {
    pub fn into_service(self) -> pb::notification_service_server::NotificationServiceServer<Self> {
        pb::notification_service_server::NotificationServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn artist_to_pb(a: m::Artist) -> pb::FollowedArtist {
    pb::FollowedArtist {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name.unwrap_or_default(),
        image_path: a.image_path.unwrap_or_default(),
    }
}

fn notification_to_pb(n: m::Notification) -> pb::Notification {
    pb::Notification {
        id: n.id.to_string(),
        kind: n.kind,
        artist_id: n.artist_id.map(|id| id.to_string()).unwrap_or_default(),
        album_id: n.album_id.map(|id| id.to_string()).unwrap_or_default(),
        title: n.title,
        body: n.body.unwrap_or_default(),
        read: n.read_at.is_some(),
        created_at: n.created_at.to_string(),
        podcast_id: n.podcast_id.map(|id| id.to_string()).unwrap_or_default(),
        episode_id: n.episode_id.map(|id| id.to_string()).unwrap_or_default(),
    }
}

/// 0 (the proto default) means "unset" → let the service apply its default.
fn opt_i64(v: i32) -> Option<i64> {
    if v > 0 { Some(v as i64) } else { None }
}

#[tonic::async_trait]
impl pb::notification_service_server::NotificationService for NotificationServer {
    async fn follow_artist(
        &self,
        req: Request<pb::FollowRequest>,
    ) -> Result<Response<pb::FollowResponse>, Status> {
        let caller = self.caller(&req).await?;
        let artist_id = parse_uuid(&req.into_inner().artist_id, "artist")?;
        self.notifications
            .follow(&caller, artist_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FollowResponse { following: true }))
    }

    async fn unfollow_artist(
        &self,
        req: Request<pb::FollowRequest>,
    ) -> Result<Response<pb::FollowResponse>, Status> {
        let caller = self.caller(&req).await?;
        let artist_id = parse_uuid(&req.into_inner().artist_id, "artist")?;
        self.notifications
            .unfollow(&caller, artist_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FollowResponse { following: false }))
    }

    async fn is_following(
        &self,
        req: Request<pb::FollowRequest>,
    ) -> Result<Response<pb::FollowResponse>, Status> {
        let caller = self.caller(&req).await?;
        let artist_id = parse_uuid(&req.into_inner().artist_id, "artist")?;
        let following = self
            .notifications
            .is_following(&caller, artist_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FollowResponse { following }))
    }

    async fn list_following(
        &self,
        req: Request<pb::ListFollowingRequest>,
    ) -> Result<Response<pb::ListFollowingResponse>, Status> {
        let caller = self.caller(&req).await?;
        let rows = self
            .notifications
            .list_following(&caller)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListFollowingResponse {
            artists: rows.into_iter().map(artist_to_pb).collect(),
            total,
        }))
    }

    async fn list_notifications(
        &self,
        req: Request<pb::ListNotificationsRequest>,
    ) -> Result<Response<pb::ListNotificationsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let rows = self
            .notifications
            .list_notifications(
                &caller,
                body.unread_only,
                opt_i64(body.limit),
                opt_i64(body.offset),
            )
            .await
            .map_err(map_err)?;
        let unread_count = self
            .notifications
            .unread_count(&caller)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListNotificationsResponse {
            notifications: rows.into_iter().map(notification_to_pb).collect(),
            total,
            unread_count,
        }))
    }

    async fn get_unread_count(
        &self,
        req: Request<pb::GetUnreadCountRequest>,
    ) -> Result<Response<pb::GetUnreadCountResponse>, Status> {
        let caller = self.caller(&req).await?;
        let unread_count = self
            .notifications
            .unread_count(&caller)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetUnreadCountResponse { unread_count }))
    }

    async fn mark_notification_read(
        &self,
        req: Request<pb::MarkReadRequest>,
    ) -> Result<Response<pb::MarkReadResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "notification")?;
        self.notifications
            .mark_read(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::MarkReadResponse {}))
    }

    async fn mark_all_notifications_read(
        &self,
        req: Request<pb::MarkAllReadRequest>,
    ) -> Result<Response<pb::MarkAllReadResponse>, Status> {
        let caller = self.caller(&req).await?;
        let marked = self
            .notifications
            .mark_all_read(&caller)
            .await
            .map_err(map_err)? as i64;
        Ok(Response::new(pb::MarkAllReadResponse { marked }))
    }

    async fn register_device(
        &self,
        req: Request<pb::RegisterDeviceRequest>,
    ) -> Result<Response<pb::RegisterDeviceResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        self.notifications
            .register_device(&caller, &body.token, &body.platform)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::RegisterDeviceResponse {}))
    }

    async fn unregister_device(
        &self,
        req: Request<pb::UnregisterDeviceRequest>,
    ) -> Result<Response<pb::UnregisterDeviceResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        self.notifications
            .unregister_device(&caller, &body.token)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::UnregisterDeviceResponse {}))
    }
}
