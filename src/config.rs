use std::env;

use anyhow::{Context as AnyhowContext, Result};
use serenity::all::{ChannelId, GuildId};

const DEFAULT_AI_CHANNEL_ID: u64 = 1506262103533817896;
const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-v4-flash";
const DEFAULT_AI_MAX_TOKENS: u32 = 800;
const DEFAULT_AI_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_AI_HISTORY_LIMIT: usize = 8;
const DEFAULT_AI_COOLDOWN_SECONDS: u64 = 3;

#[derive(Clone)]
pub(crate) struct AiConfig {
    pub(crate) channel_id: ChannelId,
    pub(crate) api_key: Option<String>,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) max_tokens: u32,
    pub(crate) timeout_seconds: u64,
    pub(crate) history_limit: usize,
    pub(crate) cooldown_seconds: u64,
}

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) discord_token: String,
    pub(crate) guild_id: GuildId,
    pub(crate) panel_channel_id: ChannelId,
    pub(crate) log_channel_id: ChannelId,
    pub(crate) create_voice_channel_id: ChannelId,
    pub(crate) database_path: String,
    pub(crate) panel_update_seconds: u64,
    pub(crate) ai: AiConfig,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            discord_token: require_env("DISCORD_TOKEN")?,
            guild_id: GuildId::new(parse_env_u64("GUILD_ID")?),
            panel_channel_id: ChannelId::new(parse_env_u64("PANEL_CHANNEL_ID")?),
            log_channel_id: ChannelId::new(parse_env_u64("LOG_CHANNEL_ID")?),
            create_voice_channel_id: ChannelId::new(parse_env_u64("CREATE_VOICE_CHANNEL_ID")?),
            database_path: env::var("DATABASE_PATH")
                .unwrap_or_else(|_| "data/voicebot.sqlite".to_string()),
            panel_update_seconds: env::var("PANEL_UPDATE_SECONDS")
                .unwrap_or_else(|_| "15".to_string())
                .parse()
                .context("PANEL_UPDATE_SECONDS must be an integer")?,
            ai: AiConfig {
                channel_id: ChannelId::new(parse_env_u64_or_default(
                    "AI_CHANNEL_ID",
                    DEFAULT_AI_CHANNEL_ID,
                )?),
                api_key: optional_env("DEEPSEEK_API_KEY"),
                base_url: env::var("DEEPSEEK_BASE_URL")
                    .unwrap_or_else(|_| DEFAULT_DEEPSEEK_BASE_URL.to_string())
                    .trim_end_matches('/')
                    .to_string(),
                model: env::var("DEEPSEEK_MODEL")
                    .unwrap_or_else(|_| DEFAULT_DEEPSEEK_MODEL.to_string()),
                max_tokens: parse_env_u32_or_default("AI_MAX_TOKENS", DEFAULT_AI_MAX_TOKENS)?,
                timeout_seconds: parse_env_u64_or_default(
                    "AI_TIMEOUT_SECONDS",
                    DEFAULT_AI_TIMEOUT_SECONDS,
                )?,
                history_limit: parse_env_usize_or_default(
                    "AI_HISTORY_LIMIT",
                    DEFAULT_AI_HISTORY_LIMIT,
                )?,
                cooldown_seconds: parse_env_u64_or_default(
                    "AI_COOLDOWN_SECONDS",
                    DEFAULT_AI_COOLDOWN_SECONDS,
                )?,
            },
        })
    }
}

fn require_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("{name} is required"))
}

fn parse_env_u64(name: &str) -> Result<u64> {
    require_env(name)?
        .parse::<u64>()
        .with_context(|| format!("{name} must be a Discord snowflake ID"))
}

fn parse_env_u64_or_default(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer")),
        Err(_) => Ok(default),
    }
}

fn parse_env_u32_or_default(name: &str, default: u32) -> Result<u32> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u32>()
            .with_context(|| format!("{name} must be an integer")),
        Err(_) => Ok(default),
    }
}

fn parse_env_usize_or_default(name: &str, default: usize) -> Result<usize> {
    match env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .with_context(|| format!("{name} must be an integer")),
        Err(_) => Ok(default),
    }
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
