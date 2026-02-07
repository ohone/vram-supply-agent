use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::config::Config;
use crate::identity::AgentIdentity;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPresenceStatus {
    Unavailable,
    Idle,
    LoadingModel,
    Ready,
    Serving,
    Degraded,
    Error,
}

#[derive(Debug, Clone)]
pub struct AgentPresenceState {
    pub status: AgentPresenceStatus,
    pub current_model: Option<String>,
    pub loading_progress_pct: Option<u8>,
    pub active_requests: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl AgentPresenceState {
    pub fn new(status: AgentPresenceStatus, current_model: Option<String>) -> Self {
        AgentPresenceState {
            status,
            current_model,
            loading_progress_pct: None,
            active_requests: 0,
            error_code: None,
            error_message: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct PresencePayload {
    agent_uid: String,
    device_name: String,
    platform: String,
    arch: String,
    agent_version: String,
    status: AgentPresenceStatus,
    current_model: Option<String>,
    loading_progress_pct: Option<u8>,
    active_requests: u32,
    error_code: Option<String>,
    error_message: Option<String>,
}

fn make_payload(agent: &AgentIdentity, state: &AgentPresenceState) -> PresencePayload {
    PresencePayload {
        agent_uid: agent.agent_uid.clone(),
        device_name: agent.device_name.clone(),
        platform: agent.platform.clone(),
        arch: agent.arch.clone(),
        agent_version: agent.agent_version.clone(),
        status: state.status.clone(),
        current_model: state.current_model.clone(),
        loading_progress_pct: state.loading_progress_pct,
        active_requests: state.active_requests,
        error_code: state.error_code.clone(),
        error_message: state.error_message.clone(),
    }
}

pub async fn send_presence_once(
    client: &reqwest::Client,
    config: &Config,
    access_token: &str,
    agent: &AgentIdentity,
    state: &AgentPresenceState,
) -> Result<()> {
    let url = format!("{}/v1/agents/presence", config.platform_url);
    let payload = make_payload(agent, state);

    let res = client
        .post(url)
        .header("Authorization", format!("Bearer {}", access_token))
        .json(&payload)
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        bail!("Presence update failed ({}): {}", status, body);
    }

    Ok(())
}

pub fn spawn_presence_loop(
    client: reqwest::Client,
    config: Config,
    token: Arc<tokio::sync::Mutex<String>>,
    agent: AgentIdentity,
    state: Arc<tokio::sync::Mutex<AgentPresenceState>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            let current_token = token.lock().await.clone();
            let snapshot = state.lock().await.clone();

            if let Err(e) =
                send_presence_once(&client, &config, &current_token, &agent, &snapshot).await
            {
                tracing::warn!("Presence heartbeat failed: {}", e);
            }
        }
    })
}
