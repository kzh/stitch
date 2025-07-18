use crate::adapters::db::{
    list_channels as db_list, track_channel as db_track, untrack_channel as db_untrack, Pool,
};
use crate::adapters::twitch::TwitchAPI;
use dashmap::DashMap;
use proto::stitch::Channel as ProtoChannel;
use std::sync::Arc;
use tonic::Status;
use tracing::instrument;

#[derive(Clone)]
pub struct ChannelService {
    pool: Pool,
    channels: Arc<DashMap<String, String>>,
    twitch_api: Arc<TwitchAPI>,
}

impl ChannelService {
    pub fn new(
        pool: Pool,
        channels: Arc<DashMap<String, String>>,
        twitch_api: Arc<TwitchAPI>,
    ) -> Self {
        Self {
            pool,
            channels,
            twitch_api,
        }
    }

    #[instrument(skip(self, name))]
    pub async fn track_channel(&self, name: String) -> Result<ProtoChannel, Status> {
        if self.channels.contains_key(&name) {
            return Err(Status::already_exists("Channel already tracked"));
        }
        let channel = self
            .twitch_api
            .get_channel_by_name(&name)
            .await
            .map_err(|e| Status::internal(format!("get_channel_id failed: {e}")))?;
        let db_channel = db_track(&self.pool, &name, &channel.display_name, &channel.id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "db_track failed");
                Status::internal(format!("db_track failed: {e:#}"))
            })?;
        self.twitch_api
            .subscribe(&channel.id)
            .await
            .map_err(|e| Status::internal(format!("subscribe failed: {e}")))?;
        self.channels.insert(name.clone(), channel.id);
        Ok(ProtoChannel {
            id: db_channel.id,
            name: db_channel.name,
        })
    }

    #[instrument(skip(self, name))]
    pub async fn untrack_channel(&self, name: String) -> Result<(), Status> {
        if !self.channels.contains_key(&name) {
            return Err(Status::not_found("Channel not tracked"));
        }
        let channel = self
            .twitch_api
            .get_channel_by_name(&name)
            .await
            .map_err(|e| Status::internal(format!("get_channel failed: {e}")))?;
        db_untrack(&self.pool, &name)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "db_untrack failed");
                Status::internal(format!("db_untrack failed: {e:#}"))
            })?;
        self.twitch_api
            .unsubscribe(&channel.id)
            .await
            .map_err(|e| Status::internal(format!("unsubscribe failed: {e}")))?;
        self.channels.remove(&name);
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn list_channels(&self) -> Result<Vec<ProtoChannel>, Status> {
        let db_channels = db_list(&self.pool)
            .await
            .map_err(|e| Status::internal(format!("db_list failed: {e}")))?;
        Ok(db_channels
            .into_iter()
            .map(|c| ProtoChannel {
                id: c.id,
                name: c.name,
            })
            .collect())
    }
}
