use anyhow::Context;
use serenity::all::ChannelId;
use serenity::http::Http as DiscordHttp;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
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
    let webhook = Arc::new(
        TwitchWebhook::new(
            webhook_secret,
            webhook_port,
            Arc::clone(&api),
            pool.clone(),
            discord_http,
            ChannelId::new(discord_channel),
        )
        .await
        .context("Failed to initialize Twitch webhook")?,
    );

    let addr_string = format!("0.0.0.0:{}", port);
    let addr = addr_string
        .parse()
        .with_context(|| format!("Invalid server address: {}", addr_string))?;

    let grpc = Server::builder().add_service(StitchServiceServer::new(StitchGRPC::new(
        crate::service::channel::ChannelService::new(pool.clone(), service_channels_map, api),
    )));
    info!("Stitch gRPC server listening: {}", addr);

    let cancel = shutdown_token();
    tokio::select! {
        result = {
            let tok = cancel.clone();
            grpc.serve_with_shutdown(addr, tok.cancelled_owned())
        } => {
            if let Err(e) = result {
                error!(error = ?e, "gRPC server encountered an error");
                return Err(e.into());
            }
            info!("gRPC server shut down.");
        }
        result = {
            let tok = cancel.clone();
            webhook.serve(tok.cancelled_owned())
        } => {
            match result {
                Ok(()) => info!("Webhook server shut down cleanly."),
                Err(e) => error!(error = ?e, "Webhook server encountered an error during its operation."),
            }
        }
    }

    Ok(())
}

fn shutdown_token() -> CancellationToken {
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install TERM signal handler");
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("Failed to install SIGINT signal handler");

        tokio::select! {
            _ = sigterm.recv() => (),
            _ = sigint.recv() => (),
        }
        cancel.cancel();
    });
    token
}
