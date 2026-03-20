use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::http::Request, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::identity;
use crate::presence::{AgentPresenceStatus, PresenceHandle};

use super::codex::CodexSession;
use super::sandbox::SandboxSession;
use super::session::ClaudeSession;

/// Shared context for handling incoming relay messages.
struct MessageContext<'a> {
    message_tx: &'a mpsc::UnboundedSender<OutgoingMessage>,
    semaphore: &'a Arc<Semaphore>,
    provider: &'a str,
    default_model: &'a str,
    default_budget: f64,
    presence: &'a PresenceHandle,
    shutdown: &'a CancellationToken,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum OutgoingMessage {
    Status {
        model: String,
        max_concurrent: u32,
        max_budget_usd: f64,
    },
    InferenceEvent {
        request_id: String,
        event: Value,
    },
    InferenceComplete {
        request_id: String,
        usage: UsageStats,
    },
    InferenceError {
        request_id: String,
        error: String,
        retryable: bool,
    },
    Pong,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum IncomingMessage {
    InferenceRequest {
        request_id: String,
        prompt: String,
        model: Option<String>,
        max_budget_usd: Option<f64>,
    },
    Ping,
}

#[derive(Debug, Serialize, Deserialize)]
struct UsageStats {
    input_tokens: u32,
    output_tokens: u32,
    total_tokens: u32,
}

pub async fn run_relay(
    config: &Config,
    provider: &str,
    max_concurrent: u32,
    model: &str,
    max_budget_usd: f64,
    shutdown: CancellationToken,
) -> Result<()> {
    let identity = identity::load_or_create_identity()?;
    let client = reqwest::Client::new();
    let token = Arc::new(tokio::sync::Mutex::new(config.api_key.clone()));

    // Create presence handle for quota mode
    let presence = PresenceHandle::new(
        Some(format!("{}-{}", provider, model)),
        client.clone(),
        config.clone(),
        Arc::clone(&token),
        identity.clone(),
    );

    // Start presence heartbeat loop
    let presence_handle = presence.spawn_loop(shutdown.clone());

    register_provider(&client, config, &token, provider, model, max_concurrent).await?;

    // Connect to platform WebSocket
    let ws_base = config
        .platform_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let ws_url = format!("{}/v1/quota/ws?provider={}", ws_base, provider);

    presence.publish().await;
    info!("Connecting to platform WebSocket: {}", ws_url);

    let ws_request = Request::builder()
        .uri(&ws_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Host", ws_url.split('/').nth(2).unwrap_or("api.vram.supply"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .context("Failed to build WebSocket request")?;

    let (ws_stream, _) = connect_async(ws_request)
        .await
        .context("Failed to connect to platform WebSocket")?;

    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    presence
        .transition(AgentPresenceStatus::QuotaReady)
        .await
        .expect("Idle → QuotaReady transition must be valid");

    // Send initial status message
    let status_msg = OutgoingMessage::Status {
        model: model.to_string(),
        max_concurrent,
        max_budget_usd,
    };

    let status_json =
        serde_json::to_string(&status_msg).context("Failed to serialize status message")?;

    ws_sink
        .send(Message::Text(status_json))
        .await
        .context("Failed to send status message")?;

    info!(
        "Quota relay started: model={}, max_concurrent={}, max_budget_usd={}",
        model, max_concurrent, max_budget_usd
    );

    // Create semaphore for concurrency control
    let semaphore = Arc::new(Semaphore::new(max_concurrent as usize));

    // Create channel for outgoing messages
    let (message_tx, mut message_rx) = mpsc::unbounded_channel::<OutgoingMessage>();

    // Start quota heartbeat loop (every 30s)
    let quota_heartbeat_handle = spawn_quota_heartbeat(
        client.clone(),
        config.clone(),
        Arc::clone(&token),
        provider.to_string(),
        shutdown.clone(),
    );

    // Main message processing loop
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("Shutting down quota relay");
                break;
            }

            // Handle incoming WebSocket messages
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let ctx = MessageContext {
                            message_tx: &message_tx,
                            semaphore: &semaphore,
                            provider,
                            default_model: model,
                            default_budget: max_budget_usd,
                            presence: &presence,
                            shutdown: &shutdown,
                        };
                        if let Err(e) = handle_incoming_message(&text, &ctx).await {
                            error!("Error handling incoming message: {}", e);
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket closed by server");
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                    _ => {
                        // Ignore other message types (binary, ping, pong)
                    }
                }
            }

            // Handle outgoing messages from inference tasks
            msg = message_rx.recv() => {
                match msg {
                    Some(outgoing_msg) => {
                        if let Ok(json) = serde_json::to_string(&outgoing_msg) {
                            if let Err(e) = ws_sink.send(Message::Text(json)).await {
                                error!("Failed to send WebSocket message: {}", e);
                                break;
                            }
                        }
                    }
                    None => {
                        debug!("Message channel closed");
                        break;
                    }
                }
            }
        }
    }

    let _ = deregister_provider(&client, config, &token, provider).await;

    // Cleanup
    presence
        .transition(AgentPresenceStatus::Unavailable)
        .await
        .expect("Any → Unavailable transition must be valid");

    // Wait for background tasks to finish
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let (_, _) = tokio::join!(presence_handle, quota_heartbeat_handle);
    })
    .await;

    Ok(())
}

async fn handle_incoming_message(text: &str, ctx: &MessageContext<'_>) -> Result<()> {
    let message: IncomingMessage =
        serde_json::from_str(text).context("Failed to parse incoming message")?;

    match message {
        IncomingMessage::Ping => {
            debug!("Received ping, sending pong");
            let pong = OutgoingMessage::Pong;
            ctx.message_tx
                .send(pong)
                .context("Failed to send pong message")?;
        }

        IncomingMessage::InferenceRequest {
            request_id,
            prompt,
            model,
            max_budget_usd,
        } => {
            let model = model.as_deref().unwrap_or(ctx.default_model);
            let budget = max_budget_usd.unwrap_or(ctx.default_budget);

            // Try to acquire semaphore permit for concurrency control
            match ctx.semaphore.clone().try_acquire_owned() {
                Ok(permit) => {
                    info!("Starting inference request: {}", request_id);

                    // Update presence to show we're serving
                    if let Err(e) = ctx
                        .presence
                        .transition(AgentPresenceStatus::QuotaServing)
                        .await
                    {
                        warn!("Failed to transition to QuotaServing: {}", e);
                    }

                    // Spawn task to handle the inference request
                    tokio::spawn(handle_inference_request(InferenceContext {
                        request_id: request_id.clone(),
                        provider: ctx.provider.to_string(),
                        prompt,
                        model: model.to_string(),
                        max_budget_usd: budget,
                        message_tx: ctx.message_tx.clone(),
                        _permit: permit,
                        presence: ctx.presence.clone(),
                        shutdown: ctx.shutdown.clone(),
                    }));
                }
                Err(_) => {
                    // At capacity, send backpressure error
                    warn!("At capacity for request: {}", request_id);
                    let error_msg = OutgoingMessage::InferenceError {
                        request_id,
                        error: "At capacity, please retry".to_string(),
                        retryable: true,
                    };
                    ctx.message_tx
                        .send(error_msg)
                        .context("Failed to send capacity error")?;
                }
            }
        }
    }

    Ok(())
}

/// Owned context for a spawned inference request task.
struct InferenceContext {
    request_id: String,
    provider: String,
    prompt: String,
    model: String,
    max_budget_usd: f64,
    message_tx: mpsc::UnboundedSender<OutgoingMessage>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    presence: PresenceHandle,
    shutdown: CancellationToken,
}

async fn handle_inference_request(ctx: InferenceContext) {
    let InferenceContext {
        request_id,
        provider,
        prompt,
        model,
        max_budget_usd,
        message_tx,
        _permit,
        presence,
        shutdown,
    } = ctx;
    let result = async {
        match provider.as_str() {
            "claude-code" => {
                // Claude Code: sandbox + srt execution with tool use
                let sandbox = SandboxSession::create(&[]).await?;
                let mut session =
                    ClaudeSession::spawn(&sandbox, &prompt, &model, max_budget_usd).await?;

                let mut usage = UsageStats {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                };

                while let Some(event_result) =
                    timeout(Duration::from_secs(60), session.next_event()).await?
                {
                    if shutdown.is_cancelled() {
                        session.abort().await?;
                        break;
                    }

                    let event = event_result?;
                    if let Some(event_usage) = extract_usage_from_event(&event) {
                        usage = event_usage;
                    }

                    let msg = OutgoingMessage::InferenceEvent {
                        request_id: request_id.clone(),
                        event,
                    };
                    message_tx.send(msg)?;
                }

                message_tx.send(OutgoingMessage::InferenceComplete {
                    request_id: request_id.clone(),
                    usage,
                })?;

                sandbox.cleanup().await?;
            }

            "openai-codex" => {
                // OpenAI Codex: inference-only, no sandbox, no tool use
                let mut session = CodexSession::start(&prompt, &model).await?;

                let mut usage = UsageStats {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                };

                while let Some(event_result) =
                    timeout(Duration::from_secs(120), session.next_event()).await?
                {
                    if shutdown.is_cancelled() {
                        break;
                    }

                    let event = event_result?;
                    if let Some(event_usage) = extract_usage_from_event(&event) {
                        usage = event_usage;
                    }

                    let msg = OutgoingMessage::InferenceEvent {
                        request_id: request_id.clone(),
                        event,
                    };
                    message_tx.send(msg)?;
                }

                message_tx.send(OutgoingMessage::InferenceComplete {
                    request_id: request_id.clone(),
                    usage,
                })?;
            }

            other => {
                anyhow::bail!("unsupported provider '{}'", other);
            }
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(e) = result {
        error!("Inference request {} failed: {}", request_id, e);

        let error_msg = OutgoingMessage::InferenceError {
            request_id: request_id.clone(),
            error: e.to_string(),
            retryable: false,
        };

        let _ = message_tx.send(error_msg);
    }

    // Transition back to QuotaReady when done
    if let Err(e) = presence.transition(AgentPresenceStatus::QuotaReady).await {
        warn!("Failed to transition back to QuotaReady: {}", e);
    }

    info!("Completed inference request: {}", request_id);
}

fn extract_usage_from_event(event: &Value) -> Option<UsageStats> {
    if let Some(usage_obj) = event.get("usage") {
        let input_tokens = usage_obj.get("input_tokens")?.as_u64().unwrap_or(0) as u32;
        let output_tokens = usage_obj.get("output_tokens")?.as_u64().unwrap_or(0) as u32;

        Some(UsageStats {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
        })
    } else {
        None
    }
}

async fn register_provider(
    client: &reqwest::Client,
    config: &Config,
    token: &Arc<tokio::sync::Mutex<String>>,
    provider: &str,
    model: &str,
    max_concurrent: u32,
) -> Result<()> {
    let current_token = token.lock().await.clone();
    client
        .post(format!("{}/v1/quota/register", config.platform_url))
        .header("Authorization", format!("Bearer {}", current_token))
        .json(&serde_json::json!({
            "provider": provider,
            "model": model,
            "max_concurrent": max_concurrent,
            "agent_version": env!("CARGO_PKG_VERSION"),
            "input_price_per_million": config.input_price_per_million,
            "output_price_per_million": config.output_price_per_million,
        }))
        .send()
        .await
        .context("Failed to register quota provider")?
        .error_for_status()
        .context("Quota provider registration failed")?;
    Ok(())
}

async fn deregister_provider(
    client: &reqwest::Client,
    config: &Config,
    token: &Arc<tokio::sync::Mutex<String>>,
    provider: &str,
) -> Result<()> {
    let current_token = token.lock().await.clone();
    client
        .delete(format!("{}/v1/quota/deregister", config.platform_url))
        .header("Authorization", format!("Bearer {}", current_token))
        .json(&serde_json::json!({ "provider": provider }))
        .send()
        .await
        .context("Failed to deregister quota provider")?
        .error_for_status()
        .context("Quota provider deregistration failed")?;
    Ok(())
}

fn spawn_quota_heartbeat(
    client: reqwest::Client,
    config: Config,
    token: Arc<tokio::sync::Mutex<String>>,
    provider: String,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let heartbeat_url = format!("{}/v1/quota/heartbeat", config.platform_url);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }

            let current_token = token.lock().await.clone();
            let res = client
                .post(&heartbeat_url)
                .header("Authorization", format!("Bearer {}", current_token))
                .json(&serde_json::json!({
                    "provider": provider,
                    "active_sessions": 0,
                }))
                .send()
                .await;

            match res {
                Ok(r) if r.status().is_success() => {
                    debug!("Quota heartbeat sent");
                }
                Ok(r) => {
                    warn!("Quota heartbeat failed: {}", r.status());
                }
                Err(e) => {
                    warn!("Quota heartbeat error: {}", e);
                }
            }
        }
    })
}
