use anyhow::Result;
use serenity::all::{ChannelId, Context, Member, UserId, VoiceState};
use serenity::builder::CreateEmbed;
use tracing::{info, warn};

use crate::{
    db::ClosedSession,
    discord::messages,
    state::BotState,
    temp_channels,
    utils::{
        cached_voice_states, channel_mention, format_duration, format_timestamp, now_unix,
        timestamp, user_mention,
    },
};

pub(crate) async fn load_known_members(state: &BotState, ctx: &Context, now: i64) {
    match state
        .config
        .guild_id
        .members(&ctx.http, Some(1000), None)
        .await
    {
        Ok(members) => {
            for member in members {
                if let Err(err) = state.upsert_member(&member, now).await {
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

pub(crate) async fn reopen_current_voice_sessions(
    state: &BotState,
    ctx: &Context,
    now: i64,
) -> Result<()> {
    let voice_states = cached_voice_states(ctx, state.config.guild_id);

    for (user_id, channel_id) in voice_states {
        if channel_id == state.config.create_voice_channel_id {
            continue;
        }

        let Some(member) = member_for_user(state, ctx, user_id).await else {
            continue;
        };

        if member.user.bot {
            continue;
        }

        state.upsert_member(&member, now).await?;
        state.open_session(user_id, channel_id, now).await?;
    }

    Ok(())
}

pub(crate) async fn handle_voice_state_update(
    state: &BotState,
    ctx: &Context,
    old: Option<VoiceState>,
    new: VoiceState,
) -> Result<()> {
    let old_channel = old.as_ref().and_then(|state| state.channel_id);
    let new_channel = new.channel_id;

    if old_channel == new_channel {
        return Ok(());
    }

    if old_channel == Some(state.config.create_voice_channel_id) {
        if let Some(channel_id) = new_channel {
            if state.is_temp_channel(channel_id).await? {
                return Ok(());
            }
        }
    }

    let Some(member) = member_for_voice_state(state, ctx, &new).await else {
        return Ok(());
    };

    if member.user.bot {
        return Ok(());
    }

    let now = now_unix();
    state.upsert_member(&member, now).await?;

    if new_channel == Some(state.config.create_voice_channel_id) {
        match temp_channels::create_personal_room(state, ctx, &member).await {
            Ok(room_id) => {
                info!(
                    user_id = member.user.id.get(),
                    channel_id = room_id.get(),
                    "created temporary voice room"
                );
                let closed = if old_channel.is_some() {
                    state.close_session(member.user.id, now).await?
                } else {
                    None
                };
                state.open_session(member.user.id, room_id, now).await?;
                if let Some(channel_id) = old_channel {
                    log_move(
                        state,
                        ctx,
                        &member,
                        channel_id,
                        room_id,
                        closed.as_ref(),
                        now,
                    )
                    .await;
                    temp_channels::cleanup_temp_channel_if_empty(state, ctx, channel_id).await?;
                } else {
                    log_join(state, ctx, &member, room_id, now).await;
                }
                state.refresh_panel(ctx).await?;
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
            state.open_session(member.user.id, channel_id, now).await?;
            log_join(state, ctx, &member, channel_id, now).await;
        }
        (Some(channel_id), None) => {
            let closed = state.close_session(member.user.id, now).await?;
            let logged_channel_id = closed
                .as_ref()
                .and_then(|session| session.channel_id)
                .unwrap_or(channel_id);
            log_leave(state, ctx, &member, logged_channel_id, closed.as_ref(), now).await;
            temp_channels::cleanup_temp_channel_if_empty(state, ctx, channel_id).await?;
        }
        (Some(old_channel_id), Some(new_channel_id)) => {
            let closed = state.close_session(member.user.id, now).await?;
            state
                .open_session(member.user.id, new_channel_id, now)
                .await?;
            log_move(
                state,
                ctx,
                &member,
                old_channel_id,
                new_channel_id,
                closed.as_ref(),
                now,
            )
            .await;
            temp_channels::cleanup_temp_channel_if_empty(state, ctx, old_channel_id).await?;
        }
        (None, None) => {}
    }

    state.refresh_panel(ctx).await?;
    Ok(())
}

async fn member_for_voice_state(
    state: &BotState,
    ctx: &Context,
    voice_state: &VoiceState,
) -> Option<Member> {
    if let Some(member) = &voice_state.member {
        return Some(member.clone());
    }

    member_for_user(state, ctx, voice_state.user_id).await
}

async fn member_for_user(state: &BotState, ctx: &Context, user_id: UserId) -> Option<Member> {
    match state.config.guild_id.member(&ctx.http, user_id).await {
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

async fn log_join(
    state: &BotState,
    ctx: &Context,
    member: &Member,
    channel_id: ChannelId,
    now: i64,
) {
    let embed = CreateEmbed::new()
        .title("Вход в голосовой канал")
        .field("Пользователь", user_mention(member.user.id), false)
        .field("Канал", channel_mention(channel_id), false)
        .field("Вошёл", format_timestamp(now), false)
        .timestamp(timestamp(now))
        .color(0x57F287);

    messages::send_log_embed(ctx, state.config.log_channel_id, embed).await;
}

async fn log_leave(
    state: &BotState,
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

    messages::send_log_embed(ctx, state.config.log_channel_id, embed).await;
}

async fn log_move(
    state: &BotState,
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

    messages::send_log_embed(ctx, state.config.log_channel_id, embed).await;
}
