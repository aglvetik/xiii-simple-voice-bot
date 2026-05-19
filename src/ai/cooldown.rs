use std::time::Instant;

use serenity::all::UserId;

use crate::state::BotState;

impl BotState {
    pub(crate) async fn check_ai_cooldown(&self, user_id: UserId) -> bool {
        if self.config.ai.cooldown_seconds == 0 {
            return true;
        }

        let mut cooldowns = self.ai_cooldowns.lock().await;
        let now = Instant::now();
        if let Some(last_request) = cooldowns.get(&user_id) {
            if now.duration_since(*last_request).as_secs() < self.config.ai.cooldown_seconds {
                return false;
            }
        }

        cooldowns.insert(user_id, now);
        true
    }
}
