use anyhow::{anyhow, Context as AnyhowContext, Result};
use serde_json::{json, Value};

use crate::{ai::history::AiConversationMessage, state::BotState, utils::truncate_chars};

pub(crate) async fn request_deepseek(
    state: &BotState,
    api_key: &str,
    history: Vec<AiConversationMessage>,
) -> Result<String> {
    let request_body = json!({
        "model": &state.config.ai.model,
        "messages": history
            .into_iter()
            .map(|message| json!({
                "role": message.role,
                "content": message.content,
            }))
            .collect::<Vec<_>>(),
        "max_tokens": state.config.ai.max_tokens,
    });

    let url = format!("{}/chat/completions", state.config.ai.base_url);
    let response = state
        .ai_http
        .post(&url)
        .bearer_auth(api_key)
        .json(&request_body)
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                anyhow!(err).context("DeepSeek request timed out")
            } else {
                anyhow!(err).context("DeepSeek request failed")
            }
        })?;

    let status = response.status();
    let response_body = response
        .text()
        .await
        .context("failed to read DeepSeek response body")?;

    if !status.is_success() {
        let body_excerpt = truncate_chars(response_body.trim(), 300);
        return Err(anyhow!(
            "DeepSeek API returned status {} with body: {}",
            status,
            body_excerpt
        ));
    }

    let payload: Value =
        serde_json::from_str(&response_body).context("failed to parse DeepSeek response")?;
    let content = payload
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .ok_or_else(|| anyhow!("DeepSeek response did not contain assistant content"))?;

    Ok(content.to_string())
}
