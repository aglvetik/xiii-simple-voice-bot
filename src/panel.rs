use anyhow::{anyhow, Context as AnyhowContext, Result};
use rusqlite::params;
use serenity::{
    all::{ChannelId, Context, Embed, Message, MessageId, UserId},
    builder::{CreateEmbed, CreateEmbedFooter, CreateMessage, EditMessage, GetMessages},
};
use tracing::{info, warn};

use crate::{
    state::BotState,
    utils::{format_duration, format_timestamp, is_not_found_error, timestamp},
};

const PANEL_SETTING_KEY: &str = "panel_message_id";
const PANEL_CHANNEL_SETTING_KEY: &str = "panel_channel_id";
const PANEL_MARKER: &str = "voice-stats-panel:v1";
const PANEL_TITLE: &str = "Голосовая активность — всё время";
const PANEL_HISTORY_SCAN_LIMIT: u8 = 100;

struct PanelRow {
    user_id: String,
    total_seconds: i64,
}

enum PanelUpdateOutcome {
    Edited,
    Missing,
}

impl BotState {
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

    async fn panel_candidates_from_history(&self, ctx: &Context) -> Result<Vec<Message>> {
        let bot_user_id = ctx.cache.current_user().id;
        let messages = self
            .config
            .panel_channel_id
            .messages(
                &ctx.http,
                GetMessages::new().limit(PANEL_HISTORY_SCAN_LIMIT),
            )
            .await
            .context("failed to scan panel channel history")?;

        Ok(messages
            .into_iter()
            .filter(|message| is_panel_candidate_message(message, bot_user_id))
            .collect())
    }

    async fn try_edit_panel_message(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
        message_id: MessageId,
        description: &str,
        now: i64,
    ) -> Result<PanelUpdateOutcome> {
        match channel_id.message(&ctx.http, message_id).await {
            Ok(_) => {
                let edit =
                    EditMessage::new().embed(build_panel_embed(description.to_string(), now));
                match channel_id.edit_message(&ctx.http, message_id, edit).await {
                    Ok(_) => Ok(PanelUpdateOutcome::Edited),
                    Err(err) if is_not_found_error(&err) => Ok(PanelUpdateOutcome::Missing),
                    Err(err) => Err(anyhow!(err).context("failed to edit panel message")),
                }
            }
            Err(err) if is_not_found_error(&err) => Ok(PanelUpdateOutcome::Missing),
            Err(err) => Err(anyhow!(err).context("failed to fetch panel message")),
        }
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

    pub(crate) async fn refresh_panel(&self, ctx: &Context) -> Result<()> {
        let _guard = self.panel_lock.lock().await;
        let now = crate::utils::now_unix();
        let description = self.panel_description(now).await?;
        let stored_reference = self.stored_panel_reference().await?;

        if let Some((channel_id, message_id)) = stored_reference {
            if channel_id != self.config.panel_channel_id {
                warn!(
                    stored_channel_id = channel_id.get(),
                    configured_channel_id = self.config.panel_channel_id.get(),
                    message_id = message_id.get(),
                    "stored panel channel differs from configured panel channel; scanning configured channel history"
                );
            } else {
                match self
                    .try_edit_panel_message(ctx, channel_id, message_id, &description, now)
                    .await?
                {
                    PanelUpdateOutcome::Edited => {
                        self.save_panel_reference(channel_id, message_id).await?;
                        info!(
                            channel_id = channel_id.get(),
                            message_id = message_id.get(),
                            "edited existing stats panel from stored reference"
                        );
                        return Ok(());
                    }
                    PanelUpdateOutcome::Missing => {
                        warn!(
                            channel_id = channel_id.get(),
                            message_id = message_id.get(),
                            "stored stats panel reference is stale; scanning channel history"
                        );
                    }
                }
            }
        }

        let preferred_message_id = stored_reference
            .filter(|(channel_id, _)| *channel_id == self.config.panel_channel_id)
            .map(|(_, message_id)| message_id);
        let mut candidates = order_panel_candidates(
            self.panel_candidates_from_history(ctx).await?,
            preferred_message_id,
        );

        if !candidates.is_empty() {
            if candidates.len() > 1 {
                warn!(
                    count = candidates.len(),
                    "found duplicate stats panels in channel history"
                );
            }

            let mut active_message_id = None;
            for candidate in &candidates {
                match self
                    .try_edit_panel_message(
                        ctx,
                        self.config.panel_channel_id,
                        candidate.id,
                        &description,
                        now,
                    )
                    .await?
                {
                    PanelUpdateOutcome::Edited => {
                        active_message_id = Some(candidate.id);
                        self.save_panel_reference(self.config.panel_channel_id, candidate.id)
                            .await?;
                        info!(
                            channel_id = self.config.panel_channel_id.get(),
                            message_id = candidate.id.get(),
                            "recovered stats panel from channel history and updated it"
                        );
                        break;
                    }
                    PanelUpdateOutcome::Missing => {
                        warn!(
                            channel_id = self.config.panel_channel_id.get(),
                            message_id = candidate.id.get(),
                            "panel candidate disappeared during recovery"
                        );
                    }
                }
            }

            if let Some(active_message_id) = active_message_id {
                for duplicate in candidates
                    .drain(..)
                    .filter(|message| message.id != active_message_id)
                {
                    match self
                        .config
                        .panel_channel_id
                        .delete_message(&ctx.http, duplicate.id)
                        .await
                    {
                        Ok(_) => info!(
                            channel_id = self.config.panel_channel_id.get(),
                            message_id = duplicate.id.get(),
                            "deleted duplicate stats panel message"
                        ),
                        Err(err) if is_not_found_error(&err) => warn!(
                            channel_id = self.config.panel_channel_id.get(),
                            message_id = duplicate.id.get(),
                            "duplicate stats panel was already missing during cleanup"
                        ),
                        Err(err) => warn!(
                            channel_id = self.config.panel_channel_id.get(),
                            message_id = duplicate.id.get(),
                            error = %err,
                            "failed to delete duplicate stats panel message"
                        ),
                    }
                }

                return Ok(());
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
        info!(
            channel_id = self.config.panel_channel_id.get(),
            message_id = message.id.get(),
            "created a new stats panel because no valid panel was found"
        );
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
}

fn build_panel_embed(description: String, now: i64) -> CreateEmbed {
    CreateEmbed::new()
        .title(PANEL_TITLE)
        .description(description)
        .footer(CreateEmbedFooter::new(format!(
            "Обновлено: {} | {}",
            format_timestamp(now),
            PANEL_MARKER
        )))
        .timestamp(timestamp(now))
        .color(0x5865F2)
}

fn is_panel_candidate_message(message: &Message, bot_user_id: UserId) -> bool {
    message.author.id == bot_user_id && message.embeds.iter().any(is_panel_embed_candidate)
}

fn is_panel_embed_candidate(embed: &Embed) -> bool {
    has_panel_marker(embed) || embed.title.as_deref() == Some(PANEL_TITLE)
}

fn has_panel_marker(embed: &Embed) -> bool {
    embed
        .footer
        .as_ref()
        .is_some_and(|footer| footer.text.contains(PANEL_MARKER))
}

fn order_panel_candidates(
    mut candidates: Vec<Message>,
    preferred_message_id: Option<MessageId>,
) -> Vec<Message> {
    candidates.sort_by_key(|message| std::cmp::Reverse(message.id.get()));

    if let Some(preferred_message_id) = preferred_message_id {
        if let Some(index) = candidates
            .iter()
            .position(|message| message.id == preferred_message_id)
        {
            let preferred = candidates.remove(index);
            candidates.insert(0, preferred);
        }
    }

    candidates
}
