use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::process::{Child, Command};

const HEALTH_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_RESTART_BACKOFF: Duration = Duration::from_secs(60);
const INITIAL_RESTART_BACKOFF: Duration = Duration::from_secs(1);
const SLOTS_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

pub struct LlamaServer {
    child: Option<Child>,
    port: u16,
    model_path: String,
    llama_server_path: String,
    gpu_layers: u32,
    context_length: u32,
    restart_backoff: Duration,
}

impl LlamaServer {
    pub fn new(
        model_path: String,
        port: u16,
        llama_server_path: String,
        gpu_layers: u32,
        context_length: u32,
    ) -> Self {
        LlamaServer {
            child: None,
            port,
            model_path,
            llama_server_path,
            gpu_layers,
            context_length,
            restart_backoff: INITIAL_RESTART_BACKOFF,
        }
    }

    /// Best-effort estimate of currently active requests from /slots.
    pub async fn active_requests(&self) -> Result<u32> {
        let url = format!("http://127.0.0.1:{}/slots", self.port);
        let client = reqwest::Client::builder()
            .timeout(SLOTS_REQUEST_TIMEOUT)
            .build()?;

        let response = client.get(&url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!("/slots returned HTTP {}", response.status());
        }

        let body: Value = response.json().await?;
        let slots = if let Some(array) = body.as_array() {
            Some(array)
        } else {
            body.get("slots").and_then(|v| v.as_array())
        };

        let Some(slots) = slots else {
            return Ok(0);
        };

        let mut active = 0u32;
        for slot in slots {
            let is_processing = slot
                .get("is_processing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let processing_i64 = slot
                .get("is_processing")
                .and_then(|v| v.as_i64())
                .map(|v| v > 0)
                .unwrap_or(false);
            let state = slot
                .get("state")
                .and_then(|v| v.as_str())
                .map(|v| matches!(v, "processing" | "running" | "active"))
                .unwrap_or(false);

            if is_processing || processing_i64 || state {
                active += 1;
            }
        }

        Ok(active)
    }

    /// Start the llama-server subprocess.
    pub async fn start(&mut self) -> Result<()> {
        tracing::info!(
            "Starting llama-server: {} -m {} --host 127.0.0.1 --port {} -ngl {} --ctx-size {}",
            self.llama_server_path,
            self.model_path,
            self.port,
            self.gpu_layers,
            self.context_length,
        );

        let child = Command::new(&self.llama_server_path)
            .arg("-m")
            .arg(&self.model_path)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(self.port.to_string())
            .arg("-ngl")
            .arg(self.gpu_layers.to_string())
            .arg("--ctx-size")
            .arg(self.context_length.to_string())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn llama-server at '{}'",
                    self.llama_server_path
                )
            })?;

        let pid = child.id().unwrap_or(0);
        tracing::info!("llama-server started with PID {}", pid);
        self.child = Some(child);

        // Wait for the server to become healthy
        self.wait_for_healthy(HEALTH_STARTUP_TIMEOUT).await?;

        self.restart_backoff = INITIAL_RESTART_BACKOFF;
        Ok(())
    }

    /// Stop the llama-server subprocess. Sends SIGTERM, then SIGKILL after 5 seconds.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            let pid = child.id();
            tracing::info!("Stopping llama-server (PID {:?})", pid);

            // Send SIGTERM via kill
            #[cfg(unix)]
            if let Some(pid) = pid {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }

            match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::info!("llama-server exited with status: {}", status);
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error waiting for llama-server: {}", e);
                }
                Err(_) => {
                    // Timeout — force kill
                    tracing::warn!("llama-server did not exit within {:?}, sending SIGKILL", GRACEFUL_SHUTDOWN_TIMEOUT);
                    let _ = child.kill().await;
                }
            }

            self.child = None;
        }
        Ok(())
    }

    /// Check if the llama-server process is still running.
    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(None) => true,     // Still running
                Ok(Some(_)) => false, // Exited
                Err(_) => false,      // Error checking
            }
        } else {
            false
        }
    }

    /// Health check against the llama-server HTTP endpoint.
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("http://127.0.0.1:{}/health", self.port);
        let client = reqwest::Client::builder()
            .timeout(HEALTH_CHECK_TIMEOUT)
            .build()?;

        match client.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    /// Wait for the server to become healthy, polling every 500ms.
    async fn wait_for_healthy(&self, timeout: Duration) -> Result<()> {
        let start = tokio::time::Instant::now();
        loop {
            if start.elapsed() > timeout {
                anyhow::bail!("llama-server did not become healthy within {:?}", timeout);
            }
            if self.health_check().await? {
                tracing::info!("llama-server is healthy");
                return Ok(());
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    /// Restart the server with exponential backoff.
    pub async fn restart_with_backoff(&mut self) -> Result<()> {
        tracing::warn!(
            "Restarting llama-server after backoff of {:?}",
            self.restart_backoff
        );
        tokio::time::sleep(self.restart_backoff).await;
        self.restart_backoff = (self.restart_backoff * 2).min(MAX_RESTART_BACKOFF);
        self.stop().await?;
        self.start().await
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            // Best-effort synchronous kill on drop
            let _ = child.start_kill();
            // Reap the zombie so we don't leak a process table entry
            let _ = child.try_wait();
        }
    }
}
