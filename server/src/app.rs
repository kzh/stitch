use anyhow::Context;
use serenity::all::ChannelId;
use serenity::http::Http as DiscordHttp;
use std::sync::Arc;
use tonic::transport::Server;
use tracing::{error, info};

use crate::adapters::db::{establish_pool, list_channels};
use crate::adapters::grpc::StitchGRPC;
use crate::adapters::twitch::TwitchAPI;
use crate::adapters::webhook::TwitchWebhook;
use crate::config::ServerConfig;
use proto::stitch::stitch_service_server::StitchServiceServer;

pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    let ServerConfig {
        database_url,
        discord_token,
        discord_channel,
        twitch_client_id,
        twitch_client_secret,
        webhook_url,
        webhook_secret,
        webhook_port,
        port,
    } = config;

    let pool = establish_pool(&database_url)
        .await
        .context("Failed to establish database pool")?;

    let channels = list_channels(&pool)
        .await
        .context("Failed to list channels from DB")?;

    let service_channels_map = Arc::new(
        channels
            .into_iter()
            .map(|c| (c.name, c.channel_id))
            .collect::<dashmap::DashMap<String, String>>(),
    );

    let api = Arc::new(
        TwitchAPI::new(
            twitch_client_id,
            twitch_client_secret,
            webhook_url,
            webhook_secret.clone(),
        )
        .await
        .context("Failed to initialize Twitch API client")?,
    );

    let discord_http = Arc::new(DiscordHttp::new(&discord_token));
    let webhook = TwitchWebhook::new(
        webhook_secret,
        webhook_port,
        Arc::clone(&api),
        pool.clone(),
        discord_http,
        ChannelId::new(discord_channel),
    )
    .await
    .context("Failed to initialize Twitch webhook")?;

    let addr_string = format!("0.0.0.0:{}", port);
    let addr = addr_string
        .parse()
        .with_context(|| format!("Invalid server address: {}", addr_string))?;

    let grpc = Server::builder().add_service(StitchServiceServer::new(StitchGRPC::new(
        crate::service::channel::ChannelService::new(pool.clone(), service_channels_map, api),
    )));
    info!("Stitch gRPC server listening: {}", addr);

    let shutdown = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        info!("Ctrl+C received, initiating shutdown...");
    };

    tokio::select! {
        result = grpc.serve(addr) => {
            if let Err(e) = result {
                error!(error = ?e, "gRPC server encountered an error");
                return Err(e.into());
            }
            info!("gRPC server shut down.");
        }
        result = webhook.serve() => {
            match result {
                Ok(()) => info!("Webhook server shut down cleanly."),
                Err(e) => error!(error = ?e, "Webhook server encountered an error during its operation."),
            }
        }
        _ = shutdown => {
            info!("Shutdown signal received. Servers are terminating.");
        }
    }

    Ok(())
}
