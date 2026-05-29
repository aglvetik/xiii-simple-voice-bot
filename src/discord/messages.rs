use serenity::{
    all::{ChannelId, Context, Message},
    builder::{CreateAllowedMentions, CreateEmbed, CreateMessage},
};
use tracing::warn;

const DISCORD_MESSAGE_CHUNK_LIMIT: usize = 1900;
const AI_ERROR_MESSAGE: &str =
    "Сейчас не получилось получить ответ от AI. Попробуйте ещё раз чуть позже.";

pub(crate) async fn send_log_embed(ctx: &Context, channel_id: ChannelId, embed: CreateEmbed) {
    if let Err(err) = channel_id
        .send_message(&ctx.http, CreateMessage::new().embed(embed))
        .await
    {
        warn!(error = %err, "failed to send voice log embed");
    }
}

pub(crate) async fn send_ai_response(ctx: &Context, trigger: &Message, response: &str) {
    let mut chunks = split_message_chunks(response, DISCORD_MESSAGE_CHUNK_LIMIT).into_iter();
    let Some(first_chunk) = chunks.next() else {
        return;
    };

    if let Err(err) = trigger
        .channel_id
        .send_message(
            &ctx.http,
            CreateMessage::new()
                .content(first_chunk.clone())
                .reference_message(trigger)
                .allowed_mentions(CreateAllowedMentions::new().replied_user(false)),
        )
        .await
    {
        warn!(
            channel_id = trigger.channel_id.get(),
            message_id = trigger.id.get(),
            error = %err,
            "failed to send AI response as a reply; falling back to normal message"
        );

        if let Err(fallback_err) = trigger
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().content(first_chunk))
            .await
        {
            warn!(
                channel_id = trigger.channel_id.get(),
                error = %fallback_err,
                "failed to send AI response fallback chunk"
            );
            return;
        }
    }

    for chunk in chunks {
        if let Err(err) = trigger
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().content(chunk))
            .await
        {
            warn!(
                channel_id = trigger.channel_id.get(),
                error = %err,
                "failed to send AI response chunk"
            );
            break;
        }
    }
}

pub(crate) async fn send_ai_error_message(ctx: &Context, channel_id: ChannelId) {
    if let Err(err) = channel_id
        .send_message(&ctx.http, CreateMessage::new().content(AI_ERROR_MESSAGE))
        .await
    {
        warn!(
            channel_id = channel_id.get(),
            error = %err,
            "failed to send AI error message"
        );
    }
}

fn split_message_chunks(text: &str, chunk_limit: usize) -> Vec<String> {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return Vec::new();
    }

    characters
        .chunks(chunk_limit)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}
