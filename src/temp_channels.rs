use anyhow::{anyhow, Context as AnyhowContext, Result};
use serenity::{
    all::{Channel, ChannelId, ChannelType, Context, Member},
    builder::CreateChannel,
};
use tracing::{info, warn};

use crate::{
    state::BotState,
    utils::{cached_channel_voice_count, now_unix, truncate_chars},
};

pub(crate) async fn cleanup_known_temp_channels(state: &BotState, ctx: &Context) -> Result<()> {
    for channel_id in state.temp_channels().await? {
        let channel_exists = match channel_id.to_channel(&ctx.http).await {
            Ok(Channel::Guild(_)) => true,
            Ok(_) => false,
            Err(err) => {
                warn!(
                    channel_id = channel_id.get(),
                    error = %err,
                    "stored temp channel could not be fetched; removing it from database"
                );
                state.remove_temp_channel(channel_id).await?;
                false
            }
        };

        if !channel_exists {
            state.remove_temp_channel(channel_id).await?;
            continue;
        }

        cleanup_temp_channel_if_empty(state, ctx, channel_id).await?;
    }

    Ok(())
}

pub(crate) async fn create_personal_room(
    state: &BotState,
    ctx: &Context,
    member: &Member,
) -> Result<ChannelId> {
    let source_channel = state
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

    let room = state
        .config
        .guild_id
        .create_channel(&ctx.http, create_channel)
        .await
        .context("failed to create temporary voice channel")?;

    state
        .add_temp_channel(room.id, member.user.id, now_unix())
        .await?;

    if let Err(err) = state
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
        state.remove_temp_channel(room.id).await?;
        return Err(anyhow!(err).context("failed to move member into temporary voice channel"));
    }

    Ok(room.id)
}

pub(crate) async fn cleanup_temp_channel_if_empty(
    state: &BotState,
    ctx: &Context,
    channel_id: ChannelId,
) -> Result<()> {
    if !state.is_temp_channel(channel_id).await? {
        return Ok(());
    }

    let Some(member_count) = cached_channel_voice_count(ctx, state.config.guild_id, channel_id)
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

    state.remove_temp_channel(channel_id).await?;
    Ok(())
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
