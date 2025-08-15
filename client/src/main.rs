mod animations;
mod config;
mod tui;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use owo_colors::OwoColorize;
use proto::stitch::stitch_service_client::StitchServiceClient;
use proto::stitch::*;
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Duration;
use tabled::{settings::Style as TableStyle, Table, Tabled};
use tokio::time::sleep;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request};

use config::CliConfig;

#[derive(ValueEnum, Clone, Debug)]
enum OutputFormat {
    Json,
    Table,
}

#[derive(Tabled)]
struct ChannelDisplay {
    #[tabled(rename = "ID")]
    id: i32,
    #[tabled(rename = "Name")]
    name: String,
}

#[derive(Subcommand)]
enum Command {
    #[command(alias = "ls")]
    List,

    Track {
        name: String,
    },

    #[command(alias = "rm")]
    Untrack {
        name: String,

        #[arg(long, short = 'y')]
        yes: bool,
    },

    Completions {
        shell: clap_complete::Shell,
    },

    Setup,
}

#[derive(Parser)]
#[command(
    name = "stitch",
    version,
    about = "Stitch - Stream channel management CLI",
    long_about = "A powerful CLI for managing Twitch stream channels with advanced features\n\nBy default, launches in interactive mode. Specify a command to use CLI mode."
)]
struct Cli {
    #[arg(long, env = "STITCH_SERVER", default_value = "http://127.0.0.1:50051")]
    server: String,

    #[arg(long, short, value_enum, env = "STITCH_OUTPUT", default_value_t = OutputFormat::Table)]
    output: OutputFormat,

    #[arg(long, short, action = ArgAction::Count)]
    verbose: u8,

    #[arg(long, env = "NO_COLOR")]
    no_color: bool,

    #[arg(long, default_value_t = 30)]
    timeout: u64,

    #[arg(long, default_value_t = 3)]
    retries: u32,

    #[arg(long, value_delimiter = ',', hide = true)]
    headers: Option<Vec<String>>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(not(debug_assertions))]
    human_panic::setup_panic!();

    let mut cli = Cli::parse();

    let config = match CliConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Warning: Failed to load config: {}", e);
            CliConfig::default()
        }
    };

    if cli.server == "http://127.0.0.1:50051" && !config.server.is_empty() {
        cli.server = config.server.clone();
    }
    if matches!(cli.output, OutputFormat::Table) && !config.output_format.is_empty() {
        cli.output = match config.output_format.as_str() {
            "json" => OutputFormat::Json,
            _ => OutputFormat::Table,
        };
    }

    if cli.no_color || !config.color {
        owo_colors::set_override(false);
    }

    let log_level = match cli.verbose {
        0 => tracing::Level::ERROR,
        1 => tracing::Level::WARN,
        2 => tracing::Level::INFO,
        3 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };

    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .without_time()
        .init();

    if let Some(ref command) = cli.command {
        if let Command::Completions { shell } = command {
            generate_completions(*shell);
            return Ok(());
        }

        if let Command::Setup = command {
            return setup_wizard().await;
        }
    }

    let result = execute_command(&cli, &config).await;
    result
}

async fn execute_command(cli: &Cli, _config: &CliConfig) -> Result<()> {
    let client = create_client_with_retry(cli).await?;
    let ctx = CliContext {
        client,
        output_format: cli.output.clone(),
        headers: parse_headers(cli.headers.clone()),
        timeout: Duration::from_secs(cli.timeout),
    };

    match &cli.command {
        None => interactive_mode(&ctx).await,
        Some(command) => match command {
            Command::List => list_channels(&ctx).await,
            Command::Track { name } => track_channel(&ctx, name).await,
            Command::Untrack { name, yes } => untrack_channel(&ctx, name, *yes).await,
            Command::Completions { .. } => unreachable!(),
            Command::Setup => unreachable!(),
        },
    }
}

#[derive(Clone)]
struct CliContext {
    client: StitchServiceClient<Channel>,
    output_format: OutputFormat,
    headers: HashMap<String, String>,
    timeout: Duration,
}

impl CliContext {
    fn create_request<T>(&self, request: T) -> Request<T> {
        let mut req = Request::new(request);
        req.set_timeout(self.timeout);

        for (key, value) in &self.headers {
            if let (Ok(k), Ok(v)) = (
                tonic::metadata::MetadataKey::from_bytes(key.as_bytes()),
                tonic::metadata::MetadataValue::try_from(value),
            ) {
                req.metadata_mut().insert(k, v);
            }
        }

        req
    }
}

fn parse_headers(headers: Option<Vec<String>>) -> HashMap<String, String> {
    headers
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| {
            let parts: Vec<&str> = h.splitn(2, '=').collect();
            if parts.len() == 2 {
                Some((parts[0].to_string(), parts[1].to_string()))
            } else {
                None
            }
        })
        .collect()
}

fn print_success(message: &str) {
    println!("{}", message.green());
}

fn print_error(message: &str) {
    eprintln!("{}", message.red());
}

fn print_warning(message: &str) {
    eprintln!("{}", message.yellow());
}

fn print_info(message: &str) {
    println!("{}", message);
}

async fn create_client_with_retry(cli: &Cli) -> Result<StitchServiceClient<Channel>> {
    let endpoint = Endpoint::from_shared(cli.server.clone()).context("Invalid server URL")?;

    let mut retries = cli.retries;
    let mut last_error = None;

    while retries > 0 {
        match StitchServiceClient::connect(endpoint.clone()).await {
            Ok(client) => return Ok(client),
            Err(e) => {
                last_error = Some(e);
                retries -= 1;
                if retries > 0 {
                    print_warning(&format!(
                        "Connection failed, retrying... ({} attempts left)",
                        retries
                    ));
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    print_error(&format!(
        "Failed to connect to Stitch server at {}",
        cli.server
    ));
    eprintln!(
        "\n{}:\n  • Start the server: {}\n  • Check if server is running on a different port\n  • Verify network connectivity\n\nYou can specify a different server with: {}",
        "Possible solutions".bold(),
        "cargo run --bin server".cyan(),
        "--server http://HOST:PORT".cyan()
    );

    Err(last_error.unwrap().into())
}

async fn list_channels(ctx: &CliContext) -> Result<()> {
    let mut client = ctx.client.clone();

    let request = ctx.create_request(ListChannelsRequest {});

    let response = client
        .list_channels(request)
        .await
        .context("Failed to list channels")?;
    let channels = response.into_inner().channels;
    let total_channels = channels.len();

    match ctx.output_format {
        OutputFormat::Json => {
            println!("{{");
            println!("  \"channels\": [");
            for (i, channel) in channels.iter().enumerate() {
                println!("    {{");
                println!("      \"id\": {},", channel.id);
                println!("      \"name\": \"{}\",", channel.name);
                print!("    }}");
                if i < channels.len() - 1 {
                    println!(",");
                } else {
                    println!();
                }
            }
            println!("  ],");
            println!("  \"total\": {}", total_channels);
            println!("}}");
        }
        OutputFormat::Table => {
            if channels.is_empty() {
                print_info("No channels found");
                return Ok(());
            }

            let display_channels: Vec<ChannelDisplay> = channels
                .into_iter()
                .map(|c| ChannelDisplay {
                    id: c.id,
                    name: c.name,
                })
                .collect();

            let table = Table::new(&display_channels)
                .with(TableStyle::modern())
                .to_string();

            println!("{}", table);

            print_info(&format!("Total channels: {}", total_channels));
        }
    }

    Ok(())
}

async fn track_channel(ctx: &CliContext, name: &str) -> Result<()> {
    let mut client = ctx.client.clone();

    let request = ctx.create_request(TrackChannelRequest {
        name: name.to_string(),
    });

    match client.track_channel(request).await {
        Ok(_) => {
            print_success(&format!("Successfully tracked channel: {}", name));
        }
        Err(e) => {
            if e.code() == Code::AlreadyExists {
                print_info(&format!("Channel '{}' is already being tracked", name));
            } else {
                print_error(&format!(
                    "Failed to track channel '{}': {}",
                    name,
                    e.message()
                ));
                return Err(e.into());
            }
        }
    }

    Ok(())
}

async fn untrack_channel(ctx: &CliContext, name: &str, yes: bool) -> Result<()> {
    if !yes {
        print!("Are you sure you want to untrack '{}'? [y/N] ", name);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            print_info("Operation cancelled");
            return Ok(());
        }
    }

    let mut client = ctx.client.clone();

    let request = ctx.create_request(UntrackChannelRequest {
        name: name.to_string(),
    });

    match client.untrack_channel(request).await {
        Ok(_) => {
            print_success(&format!("Successfully untracked channel: {}", name));
        }
        Err(e) => {
            print_error(&format!(
                "Failed to untrack channel '{}': {}",
                name,
                e.message()
            ));
            return Err(e.into());
        }
    }

    Ok(())
}

async fn interactive_mode(ctx: &CliContext) -> Result<()> {
    animations::show_welcome_animation().await?;
    tui::run_tui(ctx.clone()).await
}

fn generate_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    use clap_complete::generate;

    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut io::stdout());
}

async fn setup_wizard() -> Result<()> {
    use dialoguer::{theme::ColorfulTheme, Confirm, Select};

    println!("{}", "Welcome to Stitch Setup Wizard!".bold().cyan());
    println!("This wizard will help you configure Stitch for first-time use.\n");

    let config_path = CliConfig::config_path()?;
    if config_path.exists() {
        let overwrite = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Configuration file already exists. Overwrite?")
            .default(false)
            .interact()?;

        if !overwrite {
            print_info("Setup cancelled. Your existing configuration was preserved.");
            return Ok(());
        }
    }

    // Use simple stdin for server address to avoid paste glitches
    println!("{}:", "Stitch server address".bold());
    println!("{}", "(default: http://127.0.0.1:50051)".bright_black());
    print!("> ");
    io::stdout().flush()?;

    let mut server_input = String::new();
    io::stdin().read_line(&mut server_input)?;
    let server = server_input.trim();
    let server = if server.is_empty() {
        "http://127.0.0.1:50051".to_string()
    } else {
        if !server.starts_with("http://") && !server.starts_with("https://") {
            print_warning("Server address should start with http:// or https://");
            return Err(anyhow::anyhow!("Invalid server address"));
        }
        server.to_string()
    };

    let formats = vec!["table", "json"];
    let output_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Default output format")
        .default(0)
        .items(&formats)
        .interact()?;
    let output_format = formats[output_idx].to_string();

    let color = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Enable colored output?")
        .default(true)
        .interact()?;

    let mut config = CliConfig::default();
    config.server = server;
    config.output_format = output_format;
    config.color = color;

    config.save()?;

    print_success(&format!("Configuration saved to {:?}", config_path));

    let shells = vec!["bash", "zsh", "fish", "powershell", "skip"];
    let shell_idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Generate shell completions for")
        .default(4)
        .items(&shells)
        .interact()?;

    if shell_idx < 4 {
        let shell_name = shells[shell_idx];
        print_info(&format!("\nTo install completions for {}:", shell_name));

        match shell_name {
            "bash" => {
                println!("  # Create directory if it doesn't exist:");
                println!("  mkdir -p ~/.local/share/bash-completion/completions/\n");
                println!("  # Generate and install completions:");
                println!("  stitch completions bash > ~/.local/share/bash-completion/completions/stitch\n");
                println!("  # Reload your shell:");
                println!("  source ~/.bashrc\n");
            }
            "zsh" => {
                println!("  # Create directory if it doesn't exist:");
                println!("  mkdir -p ~/.zsh/completions/\n");
                println!("  # Generate and install completions:");
                println!("  stitch completions zsh > ~/.zsh/completions/_stitch\n");
                println!("  # Add to ~/.zshrc if not already present:");
                println!("  echo 'fpath=(~/.zsh/completions $fpath)' >> ~/.zshrc\n");
                println!("  # Reload your shell:");
                println!("  source ~/.zshrc\n");
            }
            "fish" => {
                println!("  # Fish automatically creates the directory, just run:");
                println!("  stitch completions fish > ~/.config/fish/completions/stitch.fish\n");
            }
            "powershell" => {
                println!("  # Add to your PowerShell profile:");
                println!("  stitch completions powershell >> $PROFILE\n");
                println!("  # Then reload your profile:");
                println!("  . $PROFILE\n");
            }
            _ => {}
        }

        println!(
            "{}",
            "After installation, try pressing TAB after typing 'stitch' to see completions!"
                .bright_black()
        );
    }

    let test = Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Test connection to server?")
        .default(true)
        .interact()?;

    if test {
        let spinner = animations::show_spinner("Testing connection...");

        match create_client_with_retry(&Cli {
            server: config.server.clone(),
            output: OutputFormat::Table,
            verbose: 0,
            no_color: false,
            timeout: 5,
            retries: 1,
            headers: None,
            command: Some(Command::Setup),
        })
        .await
        {
            Ok(_) => {
                spinner.success("Successfully connected to server!");
            }
            Err(e) => {
                spinner.error(&format!("Failed to connect: {}", e));
                print_warning("\nMake sure the Stitch server is running.");
                print_info("Start the server with: cargo run --bin server\n");
            }
        }
    }

    println!("{}", "Setup complete!".green().bold());
    println!("\nGet started with:");
    println!("  {} - List all channels", "stitch list".cyan());
    println!("  {} - Track a new channel", "stitch track <name>".cyan());
    println!("  {} - Interactive mode", "stitch -i".cyan());
    println!("  {} - Show help\n", "stitch --help".cyan());

    Ok(())
}
