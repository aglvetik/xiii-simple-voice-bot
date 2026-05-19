use std::{
    env, fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context as AnyhowContext, Result};
use chrono::{Local, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serenity::{
    all::{
        Channel, ChannelId, ChannelType, Context, GatewayIntents, GuildId, Member, MessageId,
        Ready, Timestamp, UserId, VoiceState,
    },
    async_trait,
    builder::{CreateChannel, CreateEmbed, CreateEmbedFooter, CreateMessage, EditMessage},
    client::{Client, EventHandler},
};
use tokio::{sync::Mutex, time};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const PANEL_SETTING_KEY: &str = "panel_message_id";
const PANEL_CHANNEL_SETTING_KEY: &str = "panel_channel_id";

#[derive(Clone)]
struct Config {
    discord_token: String,
    guild_id: GuildId,
    panel_channel_id: ChannelId,
    log_channel_id: ChannelId,
    create_voice_channel_id: ChannelId,
    database_path: String,
    panel_update_seconds: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
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
        })
    }
}

struct BotState {
    config: Config,
    db: Mutex<Connection>,
    panel_lock: Mutex<()>,
    started: AtomicBool,
}

impl BotState {
    fn new(config: Config, db: Connection) -> Self {
        Self {
            config,
            db: Mutex::new(db),
            panel_lock: Mutex::new(()),
            started: AtomicBool::new(false),
        }
    }

    async fn upsert_member(&self, member: &Member, now: i64) -> Result<()> {
        let display_name = member.display_name().to_string();
        let user_id = member.user.id.get().to_string();
        let is_bot = i64::from(member.user.bot);

        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO members (user_id, display_name, is_bot, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET
                display_name = excluded.display_name,
                is_bot = excluded.is_bot,
                updated_at = excluded.updated_at",
            params![user_id, display_name, is_bot, now],
        )?;

        Ok(())
    }

    async fn open_session(
        &self,
        user_id: UserId,
        channel_id: ChannelId,
        joined_at: i64,
    ) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO active_sessions (user_id, channel_id, joined_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET
                channel_id = excluded.channel_id,
                joined_at = excluded.joined_at",
            params![
                user_id.get().to_string(),
                channel_id.get().to_string(),
                joined_at
            ],
        )?;

        Ok(())
    }

    async fn close_session(&self, user_id: UserId, now: i64) -> Result<Option<ClosedSession>> {
        let user_id_text = user_id.get().to_string();
        let db = self.db.lock().await;

        let session = db
            .query_row(
                "SELECT channel_id, joined_at FROM active_sessions WHERE user_id = ?1",
                params![user_id_text],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;

        let Some((channel_id_text, joined_at)) = session else {
            return Ok(None);
        };

        let duration_seconds = (now - joined_at).max(0);
        db.execute(
            "INSERT INTO voice_totals (user_id, total_seconds, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET
                total_seconds = voice_totals.total_seconds + excluded.total_seconds,
                updated_at = excluded.updated_at",
            params![user_id_text, duration_seconds, now],
        )?;
        db.execute(
            "DELETE FROM active_sessions WHERE user_id = ?1",
            params![user_id.get().to_string()],
        )?;

        let channel_id = channel_id_text.parse::<u64>().ok().map(ChannelId::new);

        Ok(Some(ClosedSession {
            channel_id,
            joined_at,
            duration_seconds,
        }))
    }

    async fn close_all_stale_sessions(&self, now: i64) -> Result<usize> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare("SELECT user_id, joined_at FROM active_sessions")?;
        let sessions = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        for (user_id, joined_at) in &sessions {
            let duration_seconds = (now - *joined_at).max(0);
            db.execute(
                "INSERT INTO voice_totals (user_id, total_seconds, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id) DO UPDATE SET
                    total_seconds = voice_totals.total_seconds + excluded.total_seconds,
                    updated_at = excluded.updated_at",
                params![user_id, duration_seconds, now],
            )?;
        }

        db.execute("DELETE FROM active_sessions", [])?;
        Ok(sessions.len())
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let db = self.db.lock().await;
        let value = db
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        Ok(value)
    }

    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO settings (key, value)
             VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;

        Ok(())
    }

    async fn stored_panel_reference(&self) -> Result<Option<(ChannelId, MessageId)>> {
        let message_id = self
            .get_setting(PANEL_SETTING_KEY)
            .await?
            .and_then(|value| value.parse::<u64>().ok())
            .map(MessageId::new);

        let channel_id = self
            .get_setting(PANEL_CHANNEL_SETTING_KEY)
            .await?
            .and_then(|value| value.parse::<u64>().ok())
            .map(ChannelId::new)
            .unwrap_or(self.config.panel_channel_id);

        Ok(message_id.map(|message_id| (channel_id, message_id)))
    }

    async fn save_panel_reference(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> Result<()> {
        self.set_setting(PANEL_CHANNEL_SETTING_KEY, &channel_id.get().to_string())
            .await?;
        self.set_setting(PANEL_SETTING_KEY, &message_id.get().to_string())
            .await?;
        Ok(())
    }

    async fn panel_rows(&self, now: i64) -> Result<Vec<PanelRow>> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare(
            "SELECT
                m.user_id,
                COALESCE(t.total_seconds, 0)
                    + CASE
                        WHEN a.joined_at IS NULL THEN 0
                        ELSE MAX(0, ?1 - a.joined_at)
                      END AS display_seconds
             FROM members m
             LEFT JOIN voice_totals t ON t.user_id = m.user_id
             LEFT JOIN active_sessions a ON a.user_id = m.user_id
             WHERE m.is_bot = 0
             ORDER BY display_seconds DESC, m.user_id ASC",
        )?;

        let rows = stmt
            .query_map(params![now], |row| {
                Ok(PanelRow {
                    user_id: row.get(0)?,
                    total_seconds: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(rows)
    }

    async fn add_temp_channel(
        &self,
        channel_id: ChannelId,
        owner_user_id: UserId,
        created_at: i64,
    ) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT OR REPLACE INTO temp_channels (channel_id, owner_user_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![
                channel_id.get().to_string(),
                owner_user_id.get().to_string(),
                created_at
            ],
        )?;

        Ok(())
    }

    async fn remove_temp_channel(&self, channel_id: ChannelId) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "DELETE FROM temp_channels WHERE channel_id = ?1",
            params![channel_id.get().to_string()],
        )?;

        Ok(())
    }

    async fn is_temp_channel(&self, channel_id: ChannelId) -> Result<bool> {
        let db = self.db.lock().await;
        let exists = db
            .query_row(
                "SELECT 1 FROM temp_channels WHERE channel_id = ?1",
                params![channel_id.get().to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        Ok(exists)
    }

    async fn temp_channels(&self) -> Result<Vec<ChannelId>> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare("SELECT channel_id FROM temp_channels")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|id| id.parse::<u64>().ok().map(ChannelId::new))
            .collect();

        Ok(ids)
    }

    async fn refresh_panel(&self, ctx: &Context) -> Result<()> {
        let _guard = self.panel_lock.lock().await;
        let now = now_unix();
        let description = self.panel_description(now).await?;
        if let Some((channel_id, message_id)) = self.stored_panel_reference().await? {
            match channel_id.message(&ctx.http, message_id).await {
                Ok(_) => {
                    let edit =
                        EditMessage::new().embed(build_panel_embed(description.clone(), now));
                    match channel_id.edit_message(&ctx.http, message_id, edit).await {
                        Ok(_) => {
                            self.save_panel_reference(channel_id, message_id).await?;
                            return Ok(());
                        }
                        Err(err) if is_not_found_error(&err) => {
                            warn!(
                                channel_id = channel_id.get(),
                                message_id = message_id.get(),
                                error = %err,
                                "stored panel message disappeared during update; recreating it"
                            );
                        }
                        Err(err) => {
                            return Err(anyhow!(err).context("failed to edit stored panel message"));
                        }
                    }
                }
                Err(err) if is_not_found_error(&err) => {
                    warn!(
                        channel_id = channel_id.get(),
                        message_id = message_id.get(),
                        error = %err,
                        "stored panel message was not found; creating a new one"
                    );
                }
                Err(err) => {
                    return Err(anyhow!(err).context("failed to fetch stored panel message"));
                }
            }
        }

        let message = self
            .config
            .panel_channel_id
            .send_message(
                &ctx.http,
                CreateMessage::new().embed(build_panel_embed(description, now)),
            )
            .await
            .context("failed to create panel message")?;

        self.save_panel_reference(self.config.panel_channel_id, message.id)
            .await?;
        Ok(())
    }

    async fn panel_description(&self, now: i64) -> Result<String> {
        let rows = self.panel_rows(now).await?;
        if rows.is_empty() {
            return Ok("Пока нет голосовой активности.".to_string());
        }

        let lines = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                format!(
                    "{}. <@{}> — {}",
                    index + 1,
                    row.user_id,
                    format_duration(row.total_seconds)
                )
            })
            .collect::<Vec<_>>();

        Ok(lines.join("\n"))
    }

    async fn send_log_embed(&self, ctx: &Context, embed: CreateEmbed) {
        if let Err(err) = self
            .config
            .log_channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await
        {
            warn!(error = %err, "failed to send voice log embed");
        }
    }
}

struct Handler {
    state: Arc<BotState>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "connected to Discord");

        if self.state.started.swap(true, Ordering::SeqCst) {
            return;
        }

        if let Err(err) = self.startup(&ctx).await {
            error!(error = %err, "startup recovery failed");
        }

        let state = Arc::clone(&self.state);
        let interval_ctx = ctx.clone();
        tokio::spawn(async move {
            let refresh_every = Duration::from_secs(state.config.panel_update_seconds.max(1));

            loop {
                time::sleep(refresh_every).await;
                if let Err(err) = state.refresh_panel(&interval_ctx).await {
                    warn!(error = %err, "scheduled panel refresh failed");
                }
            }
        });
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        if let Err(err) = self.handle_voice_state_update(&ctx, old, new).await {
            warn!(error = %err, "voice state update handling failed");
        }
    }
}

impl Handler {
    async fn startup(&self, ctx: &Context) -> Result<()> {
        let now = now_unix();

        self.load_known_members(ctx, now).await;

        let closed = self.state.close_all_stale_sessions(now).await?;
        if closed > 0 {
            info!(sessions = closed, "closed stale active sessions");
        }

        self.reopen_current_voice_sessions(ctx, now).await?;
        self.cleanup_known_temp_channels(ctx).await?;
        self.state.refresh_panel(ctx).await?;

        Ok(())
    }

    async fn load_known_members(&self, ctx: &Context, now: i64) {
        match self
            .state
            .config
            .guild_id
            .members(&ctx.http, Some(1000), None)
            .await
        {
            Ok(members) => {
                for member in members {
                    if let Err(err) = self.state.upsert_member(&member, now).await {
                        warn!(
                            user_id = member.user.id.get(),
                            error = %err,
                            "failed to save guild member"
                        );
                    }
                }
            }
            Err(err) => {
                warn!(error = %err, "failed to load guild members at startup");
            }
        }
    }

    async fn reopen_current_voice_sessions(&self, ctx: &Context, now: i64) -> Result<()> {
        let voice_states = cached_voice_states(ctx, self.state.config.guild_id);

        for (user_id, channel_id) in voice_states {
            if channel_id == self.state.config.create_voice_channel_id {
                continue;
            }

            let Some(member) = self.member_for_user(ctx, user_id).await else {
                continue;
            };

            if member.user.bot {
                continue;
            }

            self.state.upsert_member(&member, now).await?;
            self.state.open_session(user_id, channel_id, now).await?;
        }

        Ok(())
    }

    async fn cleanup_known_temp_channels(&self, ctx: &Context) -> Result<()> {
        for channel_id in self.state.temp_channels().await? {
            let channel_exists = match channel_id.to_channel(&ctx.http).await {
                Ok(Channel::Guild(_)) => true,
                Ok(_) => false,
                Err(err) => {
                    warn!(
                        channel_id = channel_id.get(),
                        error = %err,
                        "stored temp channel could not be fetched; removing it from database"
                    );
                    self.state.remove_temp_channel(channel_id).await?;
                    false
                }
            };

            if !channel_exists {
                self.state.remove_temp_channel(channel_id).await?;
                continue;
            }

            self.cleanup_temp_channel_if_empty(ctx, channel_id).await?;
        }

        Ok(())
    }

    async fn handle_voice_state_update(
        &self,
        ctx: &Context,
        old: Option<VoiceState>,
        new: VoiceState,
    ) -> Result<()> {
        let old_channel = old.as_ref().and_then(|state| state.channel_id);
        let new_channel = new.channel_id;

        if old_channel == new_channel {
            return Ok(());
        }

        if old_channel == Some(self.state.config.create_voice_channel_id) {
            if let Some(channel_id) = new_channel {
                if self.state.is_temp_channel(channel_id).await? {
                    return Ok(());
                }
            }
        }

        let Some(member) = self.member_for_voice_state(ctx, &new).await else {
            return Ok(());
        };

        if member.user.bot {
            return Ok(());
        }

        let now = now_unix();
        self.state.upsert_member(&member, now).await?;

        if new_channel == Some(self.state.config.create_voice_channel_id) {
            match self.create_personal_room(ctx, &member).await {
                Ok(room_id) => {
                    info!(
                        user_id = member.user.id.get(),
                        channel_id = room_id.get(),
                        "created temporary voice room"
                    );
                    let closed = if old_channel.is_some() {
                        self.state.close_session(member.user.id, now).await?
                    } else {
                        None
                    };
                    self.state
                        .open_session(member.user.id, room_id, now)
                        .await?;
                    if let Some(channel_id) = old_channel {
                        self.log_move(ctx, &member, channel_id, room_id, closed.as_ref(), now)
                            .await;
                        self.cleanup_temp_channel_if_empty(ctx, channel_id).await?;
                    } else {
                        self.log_join(ctx, &member, room_id, now).await;
                    }
                    self.state.refresh_panel(ctx).await?;
                    return Ok(());
                }
                Err(err) => {
                    warn!(
                        user_id = member.user.id.get(),
                        error = %err,
                        "failed to create personal voice room; tracking original voice state"
                    );
                }
            }
        }

        match (old_channel, new_channel) {
            (None, Some(channel_id)) => {
                self.state
                    .open_session(member.user.id, channel_id, now)
                    .await?;
                self.log_join(ctx, &member, channel_id, now).await;
            }
            (Some(channel_id), None) => {
                let closed = self.state.close_session(member.user.id, now).await?;
                let logged_channel_id = closed
                    .as_ref()
                    .and_then(|session| session.channel_id)
                    .unwrap_or(channel_id);
                self.log_leave(ctx, &member, logged_channel_id, closed.as_ref(), now)
                    .await;
                self.cleanup_temp_channel_if_empty(ctx, channel_id).await?;
            }
            (Some(old_channel_id), Some(new_channel_id)) => {
                let closed = self.state.close_session(member.user.id, now).await?;
                self.state
                    .open_session(member.user.id, new_channel_id, now)
                    .await?;
                self.log_move(
                    ctx,
                    &member,
                    old_channel_id,
                    new_channel_id,
                    closed.as_ref(),
                    now,
                )
                .await;
                self.cleanup_temp_channel_if_empty(ctx, old_channel_id)
                    .await?;
            }
            (None, None) => {}
        }

        self.state.refresh_panel(ctx).await?;
        Ok(())
    }

    async fn create_personal_room(&self, ctx: &Context, member: &Member) -> Result<ChannelId> {
        let source_channel = self
            .state
            .config
            .create_voice_channel_id
            .to_channel(&ctx.http)
            .await
            .context("failed to fetch create-voice channel")?;

        let parent_id = match source_channel {
            Channel::Guild(channel) => channel.parent_id,
            _ => None,
        };

        let room_name = personal_room_name(member.display_name());
        let mut create_channel = CreateChannel::new(room_name).kind(ChannelType::Voice);
        if let Some(parent_id) = parent_id {
            create_channel = create_channel.category(parent_id);
        }

        let room = self
            .state
            .config
            .guild_id
            .create_channel(&ctx.http, create_channel)
            .await
            .context("failed to create temporary voice channel")?;

        self.state
            .add_temp_channel(room.id, member.user.id, now_unix())
            .await?;

        if let Err(err) = self
            .state
            .config
            .guild_id
            .move_member(&ctx.http, member.user.id, room.id)
            .await
        {
            warn!(
                channel_id = room.id.get(),
                user_id = member.user.id.get(),
                error = %err,
                "failed to move member into temporary room; deleting created room"
            );
            if let Err(delete_err) = room.id.delete(&ctx.http).await {
                warn!(
                    channel_id = room.id.get(),
                    error = %delete_err,
                    "failed to delete temporary room after move failure"
                );
            }
            self.state.remove_temp_channel(room.id).await?;
            return Err(anyhow!(err).context("failed to move member into temporary voice channel"));
        }

        Ok(room.id)
    }

    async fn cleanup_temp_channel_if_empty(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
    ) -> Result<()> {
        if !self.state.is_temp_channel(channel_id).await? {
            return Ok(());
        }

        let Some(member_count) =
            cached_channel_voice_count(ctx, self.state.config.guild_id, channel_id)
        else {
            warn!(
                channel_id = channel_id.get(),
                "guild voice cache unavailable; skipping temporary channel cleanup"
            );
            return Ok(());
        };

        if member_count > 0 {
            return Ok(());
        }

        match channel_id.delete(&ctx.http).await {
            Ok(_) => info!(
                channel_id = channel_id.get(),
                "deleted empty temporary voice room"
            ),
            Err(err) => warn!(
                channel_id = channel_id.get(),
                error = %err,
                "failed to delete empty temporary voice room"
            ),
        }

        self.state.remove_temp_channel(channel_id).await?;
        Ok(())
    }

    async fn member_for_voice_state(
        &self,
        ctx: &Context,
        voice_state: &VoiceState,
    ) -> Option<Member> {
        if let Some(member) = &voice_state.member {
            return Some(member.clone());
        }

        self.member_for_user(ctx, voice_state.user_id).await
    }

    async fn member_for_user(&self, ctx: &Context, user_id: UserId) -> Option<Member> {
        match self.state.config.guild_id.member(&ctx.http, user_id).await {
            Ok(member) => Some(member),
            Err(err) => {
                warn!(
                    user_id = user_id.get(),
                    error = %err,
                    "failed to fetch guild member; ignoring voice event"
                );
                None
            }
        }
    }

    async fn log_join(&self, ctx: &Context, member: &Member, channel_id: ChannelId, now: i64) {
        let embed = CreateEmbed::new()
            .title("Вход в голосовой канал")
            .field("Пользователь", user_mention(member.user.id), false)
            .field("Канал", channel_mention(channel_id), false)
            .field("Вошёл", format_timestamp(now), false)
            .timestamp(timestamp(now))
            .color(0x57F287);

        self.state.send_log_embed(ctx, embed).await;
    }

    async fn log_leave(
        &self,
        ctx: &Context,
        member: &Member,
        channel_id: ChannelId,
        closed: Option<&ClosedSession>,
        now: i64,
    ) {
        let joined_at = closed
            .map(|session| format_timestamp(session.joined_at))
            .unwrap_or_else(|| "неизвестно".to_string());
        let duration = closed
            .map(|session| format_duration(session.duration_seconds))
            .unwrap_or_else(|| "неизвестно".to_string());

        let embed = CreateEmbed::new()
            .title("Выход из голосового канала")
            .field("Пользователь", user_mention(member.user.id), false)
            .field("Канал", channel_mention(channel_id), false)
            .field("Вошёл", joined_at, false)
            .field("Вышел", format_timestamp(now), false)
            .field("Пробыл", duration, false)
            .timestamp(timestamp(now))
            .color(0xED4245);

        self.state.send_log_embed(ctx, embed).await;
    }

    async fn log_move(
        &self,
        ctx: &Context,
        member: &Member,
        old_channel_id: ChannelId,
        new_channel_id: ChannelId,
        closed: Option<&ClosedSession>,
        now: i64,
    ) {
        let duration = closed
            .map(|session| format_duration(session.duration_seconds))
            .unwrap_or_else(|| "неизвестно".to_string());

        let embed = CreateEmbed::new()
            .title("Переход между голосовыми каналами")
            .field("Пользователь", user_mention(member.user.id), false)
            .field("Из", channel_mention(old_channel_id), false)
            .field("В", channel_mention(new_channel_id), false)
            .field("Время", format_timestamp(now), false)
            .field("В прошлом канале пробыл", duration, false)
            .timestamp(timestamp(now))
            .color(0xFEE75C);

        self.state.send_log_embed(ctx, embed).await;
    }
}

struct ClosedSession {
    channel_id: Option<ChannelId>,
    joined_at: i64,
    duration_seconds: i64,
}

struct PanelRow {
    user_id: String,
    total_seconds: i64,
}

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

    let state = Arc::new(BotState::new(config.clone(), db));
    let intents =
        GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES | GatewayIntents::GUILD_MEMBERS;

    let mut client = Client::builder(&config.discord_token, intents)
        .event_handler(Handler { state })
        .await
        .context("failed to create Discord client")?;

    client.start().await.context("Discord client failed")?;
    Ok(())
}

fn init_db(db: &Connection) -> Result<()> {
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS members (
            user_id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            is_bot INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS voice_totals (
            user_id TEXT PRIMARY KEY,
            total_seconds INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS active_sessions (
            user_id TEXT PRIMARY KEY,
            channel_id TEXT NOT NULL,
            joined_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS temp_channels (
            channel_id TEXT PRIMARY KEY,
            owner_user_id TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        ",
    )?;

    Ok(())
}

fn prepare_database_path(database_path: &str) -> Result<()> {
    if let Some(parent) = Path::new(database_path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create database directory {:?}", parent))?;
        }
    }

    Ok(())
}

fn require_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("{name} is required"))
}

fn parse_env_u64(name: &str) -> Result<u64> {
    require_env(name)?
        .parse::<u64>()
        .with_context(|| format!("{name} must be a Discord snowflake ID"))
}

fn now_unix() -> i64 {
    Utc::now().timestamp()
}

fn timestamp(unix_seconds: i64) -> Timestamp {
    Timestamp::from_unix_timestamp(unix_seconds).unwrap_or_else(|_| Timestamp::now())
}

fn build_panel_embed(description: String, now: i64) -> CreateEmbed {
    CreateEmbed::new()
        .title("Голосовая активность — всё время")
        .description(description)
        .footer(CreateEmbedFooter::new(format!(
            "Обновлено: {}",
            format_timestamp(now)
        )))
        .timestamp(timestamp(now))
        .color(0x5865F2)
}

fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return "меньше минуты".to_string();
    }

    let total_minutes = seconds / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours > 0 {
        format!("{hours} ч {minutes} мин")
    } else {
        format!("{minutes} мин")
    }
}

fn format_timestamp(unix_seconds: i64) -> String {
    Local
        .timestamp_opt(unix_seconds, 0)
        .single()
        .unwrap_or_else(Local::now)
        .format("%d.%m.%Y %H:%M")
        .to_string()
}

fn is_not_found_error(error: &serenity::Error) -> bool {
    matches!(
        error,
        serenity::Error::Http(http_error)
            if http_error.status_code() == Some(serenity::http::StatusCode::NOT_FOUND)
    )
}

fn user_mention(user_id: UserId) -> String {
    format!("<@{}>", user_id.get())
}

fn channel_mention(channel_id: ChannelId) -> String {
    format!("<#{}>", channel_id.get())
}

fn personal_room_name(display_name: &str) -> String {
    let mut cleaned = String::new();
    let mut previous_was_space = false;

    for character in display_name
        .chars()
        .filter(|character| !character.is_control())
    {
        let character = match character {
            '/' | '\\' | '@' | '#' | ':' | '`' | '"' => ' ',
            other => other,
        };

        if character.is_whitespace() {
            if !previous_was_space {
                cleaned.push(' ');
                previous_was_space = true;
            }
        } else {
            cleaned.push(character);
            previous_was_space = false;
        }
    }

    let cleaned = cleaned.trim();
    let fallback = if cleaned.is_empty() {
        "Member"
    } else {
        cleaned
    };
    let base = truncate_chars(fallback, 80);
    format!("{base}'s room")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn cached_voice_states(ctx: &Context, guild_id: GuildId) -> Vec<(UserId, ChannelId)> {
    let Some(guild) = ctx.cache.guild(guild_id) else {
        return Vec::new();
    };

    guild
        .voice_states
        .iter()
        .filter_map(|(user_id, voice_state)| {
            voice_state
                .channel_id
                .map(|channel_id| (*user_id, channel_id))
        })
        .collect()
}

fn cached_channel_voice_count(
    ctx: &Context,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Option<usize> {
    let guild = ctx.cache.guild(guild_id)?;
    Some(
        guild
            .voice_states
            .values()
            .filter(|voice_state| voice_state.channel_id == Some(channel_id))
            .count(),
    )
}
