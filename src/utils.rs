use chrono::{Local, TimeZone, Utc};
use serenity::all::{ChannelId, Context, GuildId, Timestamp, UserId};

pub(crate) fn now_unix() -> i64 {
    Utc::now().timestamp()
}

pub(crate) fn timestamp(unix_seconds: i64) -> Timestamp {
    Timestamp::from_unix_timestamp(unix_seconds).unwrap_or_else(|_| Timestamp::now())
}

pub(crate) fn format_duration(seconds: i64) -> String {
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

pub(crate) fn format_timestamp(unix_seconds: i64) -> String {
    Local
        .timestamp_opt(unix_seconds, 0)
        .single()
        .unwrap_or_else(Local::now)
        .format("%d.%m.%Y %H:%M")
        .to_string()
}

pub(crate) fn is_not_found_error(error: &serenity::Error) -> bool {
    matches!(
        error,
        serenity::Error::Http(http_error)
            if http_error.status_code() == Some(serenity::http::StatusCode::NOT_FOUND)
    )
}

pub(crate) fn user_mention(user_id: UserId) -> String {
    format!("<@{}>", user_id.get())
}

pub(crate) fn channel_mention(channel_id: ChannelId) -> String {
    format!("<#{}>", channel_id.get())
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(crate) fn cached_voice_states(ctx: &Context, guild_id: GuildId) -> Vec<(UserId, ChannelId)> {
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

pub(crate) fn cached_channel_voice_count(
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
