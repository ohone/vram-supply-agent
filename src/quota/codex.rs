use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

use super::openai;

const CODEX_API_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// A streaming Codex session that calls the OpenAI consumer backend API.
/// Inference-only: no tools, no function calls, store: false.
pub struct CodexSession {
    response: reqwest::Response,
    buffer: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CodexUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

impl CodexSession {
    /// Start a Codex inference request.
    pub async fn start(prompt: &str, model: &str) -> Result<Self> {
        let connection = openai::load_connection()
            .context("OpenAI connection not found. Run: vramsply connect openai")?;

        // Build a messages array from the prompt string
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": prompt,
        })];

        // Build the request body — inference-only, no tools
        let body = serde_json::json!({
            "model": model,
            "instructions": "",
            "input": messages,
            "store": false,
            "stream": true,
        });

        debug!("Starting Codex session: model={}", model);

        let client = reqwest::Client::new();
        let response = client
            .post(CODEX_API_URL)
            .header(
                "Authorization",
                format!("Bearer {}", connection.access_token),
            )
            .header("chatgpt-account-id", &connection.chatgpt_account_id)
            .header("Content-Type", "application/json")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "vramsply")
            .json(&body)
            .send()
            .await
            .context("Failed to send Codex request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                anyhow::bail!("auth_expired: Codex returned {}: {}", status, text);
            }
            if status.as_u16() == 429 {
                anyhow::bail!("rate_limited: Codex returned 429: {}", text);
            }
            anyhow::bail!("Codex returned {}: {}", status, text);
        }

        Ok(Self {
            response,
            buffer: String::new(),
        })
    }

    /// Read the next SSE event and translate it into a JSON Value
    /// compatible with the quota relay protocol.
    ///
    /// Returns:
    ///  - Some(Ok(event)) for each content delta or usage chunk
    ///  - None at stream end
    pub async fn next_event(&mut self) -> Option<Result<Value>> {
        loop {
            let chunk = match self.response.chunk().await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return None,
                Err(e) => return Some(Err(anyhow::anyhow!("Stream read error: {}", e))),
            };

            self.buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete lines
            while let Some(newline_pos) = self.buffer.find('\n') {
                let line = self.buffer[..newline_pos].to_string();
                self.buffer = self.buffer[newline_pos + 1..].to_string();

                let line = line.trim();
                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }
                let data = &line[6..];
                if data == "[DONE]" {
                    return None;
                }

                let parsed: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Failed to parse Codex SSE chunk: {}", e);
                        continue;
                    }
                };

                // Translate Codex response events into relay-compatible JSON.
                // Codex uses the Responses API format: event types like
                // "response.output_text.delta", "response.completed", etc.
                let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match event_type {
                    "response.output_text.delta" => {
                        if let Some(delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                            // Emit as a content text block event (matches what Claude emits)
                            let relay_event = serde_json::json!({
                                "type": "content_block_delta",
                                "delta": { "type": "text_delta", "text": delta },
                            });
                            return Some(Ok(relay_event));
                        }
                    }
                    "response.completed" => {
                        // Extract usage if present
                        if let Some(response) = parsed.get("response") {
                            if let Some(usage) = response.get("usage") {
                                let input = usage
                                    .get("input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let output = usage
                                    .get("output_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let relay_event = serde_json::json!({
                                    "usage": {
                                        "input_tokens": input,
                                        "output_tokens": output,
                                    }
                                });
                                return Some(Ok(relay_event));
                            }
                        }
                    }
                    _ => {
                        // Skip other event types (response.created, response.output_item.added, etc.)
                        debug!("Skipping Codex event type: {}", event_type);
                    }
                }
            }
        }
    }
}
