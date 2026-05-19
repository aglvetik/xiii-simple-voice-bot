use std::{sync::Arc, time::Duration};

use anyhow::{Context as AnyhowContext, Result};
use reqwest::Client as ReqwestClient;
use rusqlite::Connection;
use serenity::{all::GatewayIntents, client::Client};
use tracing_subscriber::EnvFilter;

mod ai;
mod config;
mod db;
mod discord;
mod panel;
mod state;
mod temp_channels;
mod utils;
mod voice;

use config::Config;
use db::{init_db, prepare_database_path};
use discord::handler::Handler;
use state::BotState;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    prepare_database_path(&config.database_path)?;
    let db = Connection::open(&config.database_path)
        .with_context(|| format!("failed to open SQLite database at {}", config.database_path))?;
    init_db(&db)?;
    let ai_http = ReqwestClient::builder()
        .timeout(Duration::from_secs(config.ai.timeout_seconds.max(1)))
        .build()
        .context("failed to build DeepSeek HTTP client")?;

    let state = Arc::new(BotState::new(config.clone(), db, ai_http));
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(Handler { state })
        .await
        .context("failed to create Discord client")?;

    client.start().await.context("Discord client failed")?;
    Ok(())
}
