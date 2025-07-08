pub mod adapters;
pub mod app;
pub mod config;
pub mod service;

pub(crate) mod utils;

use anyhow::Result;
use clap::Parser;
use dotenv::dotenv;
use tracing_subscriber::fmt;

use crate::config::ServerConfig;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    fmt::init();

    let cfg = ServerConfig::parse();

    app::run(cfg).await?;
    Ok(())
}
