use anyhow;
use clap::{Parser, Subcommand, ValueEnum};
use proto::stitch::stitch_service_client::StitchServiceClient;
use proto::stitch::{ListChannelsRequest, TrackChannelRequest, UntrackChannelRequest};
use tonic::Code;

fn map_status(e: tonic::Status, action: &str, target: Option<&str>) -> anyhow::Error {
    let name = target.unwrap_or("");
    let msg = match e.code() {
        Code::NotFound => format!("{} '{}' not found", action, name),
        Code::AlreadyExists => format!("{} '{}' already exists", action, name),
        Code::InvalidArgument => format!("Invalid argument for {} '{}'", action, name),
        Code::Unavailable => format!(
            "Server is temporarily unavailable. Please try {} later.",
            action
        ),
        _ => format!("Failed to {} '{}': {}", action, name, e.message()),
    };
    anyhow::anyhow!(msg)
}

#[derive(ValueEnum, Clone)]
enum OutputFormat {
    Json,
    Table,
}

#[derive(Subcommand)]
enum Command {
    List,
    Track { name: String },
    Untrack { name: String },
}

#[derive(Parser)]
#[command(name = "stitch", version, about)]
struct Cli {
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    server: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut client = StitchServiceClient::connect(cli.server.clone())
        .await
        .unwrap_or_else(|_| {
            eprintln!("❌ Failed to connect to Stitch server at {}", cli.server);
            eprintln!(
                "\nPossible solutions:\n  • Start the server: cargo run --bin server\n  • Check if server is running on a different port\n  • Verify network connectivity\n\n💡 You can specify a different server with: --server http://HOST:PORT"
            );
            std::process::exit(1);
        });

    match cli.command {
        Command::List => list_channels(&mut client).await?,
        Command::Track { name } => track_channel(&mut client, &name).await?,
        Command::Untrack { name } => untrack_channel(&mut client, &name).await?,
    }

    Ok(())
}

async fn list_channels(
    client: &mut StitchServiceClient<tonic::transport::Channel>,
) -> anyhow::Result<()> {
    let response = client
        .list_channels(ListChannelsRequest {})
        .await
        .map_err(|e| map_status(e, "list channels", None))?;
    let channels = response.into_inner().channels;
    println!("Channels\n========");
    for channel in channels {
        println!("{} {}", channel.id, channel.name);
    }
    Ok(())
}

async fn track_channel(
    client: &mut StitchServiceClient<tonic::transport::Channel>,
    name: &str,
) -> anyhow::Result<()> {
    client
        .track_channel(TrackChannelRequest {
            name: name.to_string(),
        })
        .await
        .map_err(|e| map_status(e, "track channel", Some(name)))?;
    println!("✅ Successfully tracked channel: {}", name);
    Ok(())
}

async fn untrack_channel(
    client: &mut StitchServiceClient<tonic::transport::Channel>,
    name: &str,
) -> anyhow::Result<()> {
    client
        .untrack_channel(UntrackChannelRequest {
            name: name.to_string(),
        })
        .await
        .map_err(|e| map_status(e, "untrack channel", Some(name)))?;
    println!("✅ Successfully untracked channel: {}", name);
    Ok(())
}
