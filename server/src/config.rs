use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "server", about = "Run the Stitch gRPC server")]
pub struct ServerConfig {
    #[arg(short, long, env, default_value_t = 50051)]
    pub port: u16,

    #[arg(
        long,
        env,
        default_value = "postgres://postgres:password@localhost:5432/stitch"
    )]
    pub database_url: String,

    #[arg(long, env)]
    pub webhook_url: String,

    #[arg(long, env)]
    pub webhook_secret: String,

    #[arg(long, env, default_value_t = 50052)]
    pub webhook_port: u16,

    #[arg(long, env, default_value_t = 50053)]
    pub tokio_console_port: u16,

    #[arg(long, env)]
    pub twitch_client_id: String,

    #[arg(long, env)]
    pub twitch_client_secret: String,

    #[arg(long, env)]
    pub discord_token: String,

    #[arg(long, env)]
    pub discord_channel: u64,
}
