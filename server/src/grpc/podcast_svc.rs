//! gRPC PodcastService implementation.

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::podcast as pb;
use crate::services::podcast_dir::PodcastCandidate;
use crate::services::PodcastService;
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct PodcastServer {
    /// `None` when podcasts are disabled (no `PODCAST_PATH`); handlers then
    /// return `FAILED_PRECONDITION`, like uploads/artwork when unconfigured.
    pub podcasts: Option<PodcastService>,
    pub interceptor: AuthInterceptor,
}

impl PodcastServer {
    pub fn into_service(self) -> pb::podcast_service_server::PodcastServiceServer<Self> {
        pb::podcast_service_server::PodcastServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }

    fn service(&self) -> Result<&PodcastService, Status> {
        self.podcasts
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("podcasts are not enabled (set PODCAST_PATH)"))
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

/// 0 (proto default) → "unset" for limit/offset → let the service default.
fn opt_i64(v: i32) -> Option<i64> {
    if v > 0 { Some(v as i64) } else { None }
}

fn podcast_to_pb(p: m::Podcast) -> pb::Podcast {
    pb::Podcast {
        id: p.id.to_string(),
        feed_url: p.feed_url,
        title: p.title,
        author: p.author.unwrap_or_default(),
        description: p.description.unwrap_or_default(),
        image_url: p.image_url.unwrap_or_default(),
        link: p.link.unwrap_or_default(),
        language: p.language.unwrap_or_default(),
        categories: serde_json::from_str(&p.categories).unwrap_or_default(),
        itunes_id: p.itunes_id.unwrap_or(0),
        podcastindex_id: p.podcastindex_id.unwrap_or(0),
        auto_download: p.auto_download,
        last_refreshed_at: p.last_refreshed_at.map(rfc3339).unwrap_or_default(),
        created_at: rfc3339(p.created_at),
        updated_at: rfc3339(p.updated_at),
        storage_bytes: p.storage_bytes,
    }
}

fn candidate_to_pb(c: PodcastCandidate) -> pb::PodcastCandidate {
    pb::PodcastCandidate {
        feed_url: c.feed_url,
        title: c.title,
        author: c.author.unwrap_or_default(),
        description: c.description.unwrap_or_default(),
        image_url: c.image_url.unwrap_or_default(),
        categories: c.categories,
        itunes_id: c.itunes_id.unwrap_or(0),
        podcastindex_id: c.podcastindex_id.unwrap_or(0),
    }
}

fn episode_to_pb(e: m::PodcastEpisode) -> pb::Episode {
    let downloaded = e.file_path.is_some();
    pb::Episode {
        id: e.id.to_string(),
        podcast_id: e.podcast_id.to_string(),
        guid: e.guid,
        title: e.title,
        description: e.description.unwrap_or_default(),
        enclosure_url: e.enclosure_url,
        enclosure_type: e.enclosure_type.unwrap_or_default(),
        episode_no: e.episode_no.unwrap_or(0),
        season_no: e.season_no.unwrap_or(0),
        duration_ms: e.duration_ms.unwrap_or(0),
        codec: e.codec.unwrap_or_default(),
        bitrate_kbps: e.bitrate_kbps.unwrap_or(0),
        file_path: e.file_path.unwrap_or_default(),
        file_size: e.file_size.unwrap_or(0),
        image_url: e.image_path.unwrap_or_default(),
        published_at: e.published_at.map(rfc3339).unwrap_or_default(),
        downloaded,
    }
}

fn progress_to_pb(p: m::EpisodeProgress) -> pb::EpisodeProgress {
    pb::EpisodeProgress {
        episode_id: p.episode_id.to_string(),
        position_ms: p.position_ms,
        completed: p.completed,
        updated_at: rfc3339(p.updated_at),
    }
}

#[tonic::async_trait]
impl pb::podcast_service_server::PodcastService for PodcastServer {
    async fn search_podcasts(
        &self,
        req: Request<pb::SearchPodcastsRequest>,
    ) -> Result<Response<pb::SearchPodcastsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let body = req.into_inner();
        let cands = svc
            .search(&caller, &body.term, opt_i64(body.limit).unwrap_or(50))
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::SearchPodcastsResponse {
            candidates: cands.into_iter().map(candidate_to_pb).collect(),
        }))
    }

    async fn subscribe_feed(
        &self,
        req: Request<pb::SubscribeFeedRequest>,
    ) -> Result<Response<pb::Podcast>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let body = req.into_inner();
        let feed_url = (!body.feed_url.trim().is_empty()).then_some(body.feed_url);
        let itunes_id = (body.itunes_id != 0).then_some(body.itunes_id);
        let p = svc
            .subscribe_feed(&caller, feed_url.as_deref(), itunes_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(podcast_to_pb(p)))
    }

    async fn list_podcasts(
        &self,
        req: Request<pb::ListPodcastsRequest>,
    ) -> Result<Response<pb::ListPodcastsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let body = req.into_inner();
        let (items, total) = svc
            .list(&caller, opt_i64(body.limit), opt_i64(body.offset))
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListPodcastsResponse {
            podcasts: items.into_iter().map(podcast_to_pb).collect(),
            total,
        }))
    }

    async fn get_podcast(
        &self,
        req: Request<pb::PodcastIdRequest>,
    ) -> Result<Response<pb::Podcast>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        let p = svc.get(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(podcast_to_pb(p)))
    }

    async fn delete_podcast(
        &self,
        req: Request<pb::PodcastIdRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        svc.delete(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::DeleteResponse { deleted: true }))
    }

    async fn refresh_podcast(
        &self,
        req: Request<pb::PodcastIdRequest>,
    ) -> Result<Response<pb::RefreshReport>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        let report = svc.refresh(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::RefreshReport {
            podcast_id: report.podcast_id.to_string(),
            new_episodes: report.new_episodes as i64,
            not_modified: report.not_modified,
        }))
    }

    async fn set_auto_download(
        &self,
        req: Request<pb::SetAutoDownloadRequest>,
    ) -> Result<Response<pb::Podcast>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let body = req.into_inner();
        let id = parse_uuid(&body.podcast_id, "podcast")?;
        let p = svc
            .set_auto_download(&caller, id, body.auto_download)
            .await
            .map_err(map_err)?;
        Ok(Response::new(podcast_to_pb(p)))
    }

    async fn list_episodes(
        &self,
        req: Request<pb::ListEpisodesRequest>,
    ) -> Result<Response<pb::ListEpisodesResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let body = req.into_inner();
        let id = parse_uuid(&body.podcast_id, "podcast")?;
        let eps = svc
            .list_episodes(&caller, id, opt_i64(body.limit), opt_i64(body.offset))
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListEpisodesResponse {
            episodes: eps.into_iter().map(episode_to_pb).collect(),
        }))
    }

    async fn get_episode(
        &self,
        req: Request<pb::EpisodeIdRequest>,
    ) -> Result<Response<pb::Episode>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().episode_id, "episode")?;
        let e = svc.get_episode(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(episode_to_pb(e)))
    }

    async fn download_episode(
        &self,
        req: Request<pb::EpisodeIdRequest>,
    ) -> Result<Response<pb::Episode>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().episode_id, "episode")?;
        let e = svc.download_episode(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(episode_to_pb(e)))
    }

    async fn subscribe(
        &self,
        req: Request<pb::PodcastIdRequest>,
    ) -> Result<Response<pb::SubscribeResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        svc.subscribe(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::SubscribeResponse { subscribed: true }))
    }

    async fn unsubscribe(
        &self,
        req: Request<pb::PodcastIdRequest>,
    ) -> Result<Response<pb::SubscribeResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        svc.unsubscribe(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::SubscribeResponse { subscribed: false }))
    }

    async fn is_subscribed(
        &self,
        req: Request<pb::PodcastIdRequest>,
    ) -> Result<Response<pb::SubscribeResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        let subscribed = svc.is_subscribed(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::SubscribeResponse { subscribed }))
    }

    async fn list_subscriptions(
        &self,
        req: Request<pb::ListSubscriptionsRequest>,
    ) -> Result<Response<pb::ListPodcastsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let items = svc.list_subscriptions(&caller).await.map_err(map_err)?;
        let total = items.len() as i64;
        Ok(Response::new(pb::ListPodcastsResponse {
            podcasts: items.into_iter().map(podcast_to_pb).collect(),
            total,
        }))
    }

    async fn record_episode_progress(
        &self,
        req: Request<pb::RecordEpisodeProgressRequest>,
    ) -> Result<Response<pb::EpisodeProgress>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let body = req.into_inner();
        let id = parse_uuid(&body.episode_id, "episode")?;
        let p = svc
            .record_progress(&caller, id, body.position_ms, body.completed)
            .await
            .map_err(map_err)?;
        Ok(Response::new(progress_to_pb(p)))
    }

    async fn list_episode_progress(
        &self,
        req: Request<pb::ListEpisodeProgressRequest>,
    ) -> Result<Response<pb::ListEpisodeProgressResponse>, Status> {
        let caller = self.caller(&req).await?;
        let svc = self.service()?;
        let id = parse_uuid(&req.into_inner().podcast_id, "podcast")?;
        let items = svc.list_progress(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::ListEpisodeProgressResponse {
            progress: items.into_iter().map(progress_to_pb).collect(),
        }))
    }
}
