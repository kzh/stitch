use crate::service::channel::ChannelService;
use proto::stitch::stitch_service_server::StitchService;
use proto::stitch::{
    ListChannelsRequest, ListChannelsResponse, TrackChannelRequest, TrackChannelResponse,
    UntrackChannelRequest, UntrackChannelResponse,
};
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct StitchGRPC {
    service: ChannelService,
}

impl StitchGRPC {
    pub fn new(service: ChannelService) -> Self {
        Self { service }
    }
}

#[tonic::async_trait]
impl StitchService for StitchGRPC {
    async fn track_channel(
        &self,
        request: Request<TrackChannelRequest>,
    ) -> Result<Response<TrackChannelResponse>, Status> {
        let req = request.into_inner();
        self.service.track_channel(req.name).await?;
        Ok(Response::new(TrackChannelResponse {}))
    }

    async fn untrack_channel(
        &self,
        request: Request<UntrackChannelRequest>,
    ) -> Result<Response<UntrackChannelResponse>, Status> {
        let req = request.into_inner();
        self.service.untrack_channel(req.name).await?;
        Ok(Response::new(UntrackChannelResponse {}))
    }

    async fn list_channels(
        &self,
        _request: Request<ListChannelsRequest>,
    ) -> Result<Response<ListChannelsResponse>, Status> {
        let channels = self.service.list_channels().await?;
        Ok(Response::new(ListChannelsResponse { channels }))
    }
}
