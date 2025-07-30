pub mod adapters;
pub mod app;
pub mod config;
pub mod service;

pub(crate) mod utils;

use anyhow::Result;
use clap::Parser;
use console_subscriber::ConsoleLayer;
use dotenv::dotenv;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{filter, fmt, layer::SubscriberExt, util::SubscriberInitExt, Layer};

use crate::config::ServerConfig;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let cfg = ServerConfig::parse();

    let console_layer = ConsoleLayer::builder()
        .server_addr(([0, 0, 0, 0], cfg.tokio_console_port))
        .spawn();
    let fmt_layer = fmt::layer().with_filter(
        filter::Targets::new()
            .with_target("stitch", LevelFilter::INFO)
            .with_target("tokio", LevelFilter::OFF)
            .with_target("runtime", LevelFilter::OFF)
            .with_target("console_subscriber", LevelFilter::OFF)
            .with_default(LevelFilter::INFO),
    );

    tracing_subscriber::registry()
        .with(console_layer)
        .with(fmt_layer)
        .init();

    app::run(cfg).await?;
    Ok(())
}
