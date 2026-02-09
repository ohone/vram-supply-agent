use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

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

impl AgentPresenceStatus {
    /// Returns whether transitioning from `self` to `target` is valid.
    ///
    /// ```text
    /// Idle         → LoadingModel, Unavailable, Error
    /// LoadingModel → Ready, Error, Unavailable
    /// Ready        → Serving, LoadingModel, Degraded, Error, Unavailable
    /// Serving      → Ready, Degraded, Error, Unavailable
    /// Degraded     → Ready, LoadingModel, Error, Unavailable
    /// Error        → LoadingModel, Unavailable
    /// ```
    fn can_transition_to(&self, target: &AgentPresenceStatus) -> bool {
        use AgentPresenceStatus::*;
        matches!(
            (self, target),
            (Idle, LoadingModel | Unavailable | Error)
                | (LoadingModel, Ready | Error | Unavailable)
                | (
                    Ready,
                    Serving | LoadingModel | Degraded | Error | Unavailable
                )
                | (Serving, Ready | Degraded | Error | Unavailable)
                | (Degraded, Ready | LoadingModel | Error | Unavailable)
                | (Error, LoadingModel | Unavailable)
        )
    }
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

/// Wrapper around presence state with methods to transition status and publish.
/// All fields are Arc-wrapped so this is cheap to Clone.
#[derive(Clone)]
pub struct PresenceHandle {
    state: Arc<tokio::sync::Mutex<AgentPresenceState>>,
    client: reqwest::Client,
    config: Config,
    token: Arc<tokio::sync::Mutex<String>>,
    identity: AgentIdentity,
}

impl PresenceHandle {
    pub fn new(
        model_name: Option<String>,
        client: reqwest::Client,
        config: Config,
        token: Arc<tokio::sync::Mutex<String>>,
        identity: AgentIdentity,
    ) -> Self {
        let state = Arc::new(tokio::sync::Mutex::new(AgentPresenceState::new(
            AgentPresenceStatus::Idle,
            model_name,
        )));
        PresenceHandle {
            state,
            client,
            config,
            token,
            identity,
        }
    }

    /// Transition to a new status, clearing error fields and publishing.
    ///
    /// Returns an error if the transition is not allowed from the current state.
    /// Invalid transitions indicate a programming bug in the caller.
    pub async fn transition(&self, status: AgentPresenceStatus) -> Result<()> {
        {
            let mut s = self.state.lock().await;
            if !s.status.can_transition_to(&status) {
                bail!("Invalid presence transition: {:?} → {:?}", s.status, status);
            }
            s.status = status;
            s.loading_progress_pct = None;
            s.error_code = None;
            s.error_message = None;
        }
        self.publish().await;
        Ok(())
    }

    /// Report an error status with code and message, then publish.
    ///
    /// Unlike `transition()`, this bypasses state validation — errors can occur
    /// from any state. Preserves `active_requests` because the error may have
    /// occurred mid-request; dropping the count would lose track of in-flight work.
    pub async fn report_error(&self, code: &str, msg: &str) {
        {
            let mut s = self.state.lock().await;
            s.status = AgentPresenceStatus::Error;
            s.error_code = Some(code.to_string());
            s.error_message = Some(msg.to_string());
        }
        self.publish().await;
    }

    /// Report a degraded status with code and message, then publish.
    ///
    /// Unlike `transition()`, this bypasses state validation — degraded can be
    /// entered from any state. Zeros `active_requests` because degraded means
    /// "I'm impaired, stop routing to me" — any in-flight work is assumed lost.
    pub async fn report_degraded(&self, code: &str, msg: &str) {
        {
            let mut s = self.state.lock().await;
            s.status = AgentPresenceStatus::Degraded;
            s.active_requests = 0;
            s.error_code = Some(code.to_string());
            s.error_message = Some(msg.to_string());
        }
        self.publish().await;
    }

    /// Update the active request count and toggle Ready/Serving, then publish.
    pub async fn update_active_requests(&self, n: u32) {
        let mut s = self.state.lock().await;
        s.active_requests = n;
        if n > 0 {
            s.status = AgentPresenceStatus::Serving;
        } else if matches!(
            s.status,
            AgentPresenceStatus::Ready
                | AgentPresenceStatus::Serving
                | AgentPresenceStatus::Idle
                | AgentPresenceStatus::LoadingModel
        ) {
            s.status = AgentPresenceStatus::Ready;
        }
        // Drop lock before publish — publish will re-lock to snapshot.
        drop(s);
        self.publish().await;
    }

    /// Publish the current state snapshot to the platform.
    pub async fn publish(&self) {
        let current_token = self.token.lock().await.clone();
        let snapshot = self.state.lock().await.clone();
        if let Err(e) = send_presence_once(
            &self.client,
            &self.config,
            &current_token,
            &self.identity,
            &snapshot,
        )
        .await
        {
            tracing::warn!("Presence update failed: {}", e);
        }
    }

    /// Spawn the periodic presence heartbeat loop (every 15s).
    ///
    /// This sends the full agent state (status, model, active requests, errors)
    /// to `/v1/agents/presence`. It is distinct from the provider heartbeat in
    /// `spawn_heartbeat_loop` (main.rs), which is an empty-body liveness ping
    /// to `/v1/providers/heartbeat` every 30s at the provider/instance level.
    pub fn spawn_loop(&self, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
        let handle = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = interval.tick() => {}
                }
                handle.publish().await;
            }
        })
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

async fn send_presence_once(
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
