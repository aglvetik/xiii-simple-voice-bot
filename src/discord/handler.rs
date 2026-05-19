use std::sync::{atomic::Ordering, Arc};

use anyhow::Result;
use serenity::{
    all::{Context, Message, Ready, VoiceState},
    async_trait,
    client::EventHandler,
};
use tokio::time::{self, Duration};
use tracing::{error, info, warn};

use crate::{ai, state::BotState, temp_channels, utils::now_unix, voice};

pub(crate) struct Handler {
    pub(crate) state: Arc<BotState>,
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
        if let Err(err) = voice::handle_voice_state_update(&self.state, &ctx, old, new).await {
            warn!(error = %err, "voice state update handling failed");
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if let Err(err) = ai::handle_ai_message(&self.state, &ctx, &msg).await {
            warn!(
                channel_id = msg.channel_id.get(),
                message_id = msg.id.get(),
                user_id = msg.author.id.get(),
                error = %err,
                "AI message handling failed"
            );
        }
    }
}

impl Handler {
    async fn startup(&self, ctx: &Context) -> Result<()> {
        let now = now_unix();

        voice::load_known_members(&self.state, ctx, now).await;

        let closed = self.state.close_all_stale_sessions(now).await?;
        if closed > 0 {
            info!(sessions = closed, "closed stale active sessions");
        }

        voice::reopen_current_voice_sessions(&self.state, ctx, now).await?;
        temp_channels::cleanup_known_temp_channels(&self.state, ctx).await?;
        self.state.refresh_panel(ctx).await?;

        Ok(())
    }
}
