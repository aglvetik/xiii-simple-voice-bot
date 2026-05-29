pub(crate) mod cooldown;
pub(crate) mod deepseek;
pub(crate) mod history;

use anyhow::Result;
use serenity::all::{Context, Message, MessageType};
use tracing::{info, warn};

use crate::{discord::messages, state::BotState};

pub(crate) async fn handle_ai_message(
    state: &BotState,
    ctx: &Context,
    msg: &Message,
) -> Result<()> {
    if msg.channel_id != state.config.ai.channel_id {
        return Ok(());
    }

    if msg.author.bot {
        return Ok(());
    }

    if msg.kind != MessageType::Regular && msg.kind != MessageType::InlineReply {
        return Ok(());
    }

    if msg.content.trim().is_empty() {
        return Ok(());
    }

    if !state.check_ai_cooldown(msg.author.id).await {
        info!(
            channel_id = msg.channel_id.get(),
            user_id = msg.author.id.get(),
            "ignored AI message because user is on cooldown"
        );
        return Ok(());
    }

    let Some(api_key) = state.config.ai.api_key.as_deref() else {
        warn!("DEEPSEEK_API_KEY is not configured; AI replies are unavailable");
        messages::send_ai_error_message(ctx, msg.channel_id).await;
        return Ok(());
    };

    let _typing = msg.channel_id.start_typing(&ctx.http);
    let history = history::build_ai_history(ctx, msg, state.config.ai.history_limit).await?;

    match deepseek::request_deepseek(state, api_key, history).await {
        Ok(response) => messages::send_ai_response(ctx, msg, &response).await,
        Err(err) => {
            warn!(
                channel_id = msg.channel_id.get(),
                user_id = msg.author.id.get(),
                error = %err,
                "failed to fetch DeepSeek response"
            );
            messages::send_ai_error_message(ctx, msg.channel_id).await;
        }
    }

    Ok(())
}
