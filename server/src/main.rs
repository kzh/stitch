pub mod adapters;
pub mod app;
pub mod config;
pub mod service;

pub(crate) mod utils;

use anyhow::Result;
use clap::Parser;
use dotenv::dotenv;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{filter, fmt, layer::SubscriberExt, util::SubscriberInitExt, Layer};

use crate::config::ServerConfig;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let console_layer = console_subscriber::spawn();
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
    let cfg = ServerConfig::parse();

    app::run(cfg).await?;
    Ok(())
}
