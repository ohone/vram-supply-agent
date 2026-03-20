use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, warn};

use super::sandbox::SandboxSession;

pub struct ClaudeSession {
    child: Option<Child>,
    stdout: BufReader<ChildStdout>,
    stdin: Option<ChildStdin>,
}

impl ClaudeSession {
    /// Spawn a new Claude Code session in the sandbox
    pub async fn spawn(
        sandbox: &SandboxSession,
        prompt: &str,
        model: &str,
        max_budget_usd: f64,
    ) -> Result<Self> {
        // Build the claude command to run inside srt.
        // Shell-quote interpolated values to guard against injection via
        // paths with spaces, unexpected model names, etc.
        let tmpdir_str = sandbox.tmpdir.display().to_string();
        let quoted_tmpdir = shlex::try_quote(&tmpdir_str)
            .map_err(|e| anyhow::anyhow!("Failed to shell-quote tmpdir: {e}"))?;
        let quoted_model = shlex::try_quote(model)
            .map_err(|e| anyhow::anyhow!("Failed to shell-quote model: {e}"))?;

        let claude_cmd = format!(
            "cd {quoted_tmpdir} && claude -p \
             --output-format stream-json \
             --dangerously-skip-permissions \
             --no-session-persistence \
             --model {quoted_model} \
             --max-budget-usd {max_budget_usd}",
        );

        debug!(
            "Spawning Claude session: model={}, budget=${}, tmpdir={}",
            model,
            max_budget_usd,
            sandbox.tmpdir.display()
        );

        // Spawn srt with the claude command
        let mut child = Command::new("srt")
            .arg("-s")
            .arg(&sandbox.config_path)
            .arg("-c")
            .arg(&claude_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn srt process")?;

        // Get stdin and stdout handles
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to get stdin handle"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to get stdout handle"))?;

        let stdout = BufReader::new(stdout);

        let mut session = ClaudeSession {
            child: Some(child),
            stdout,
            stdin: Some(stdin),
        };

        // Send the prompt to Claude
        session.send_prompt(prompt).await?;

        Ok(session)
    }

    /// Send the prompt to Claude's stdin
    async fn send_prompt(&mut self, prompt: &str) -> Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("Failed to write prompt to stdin")?;
            stdin.flush().await.context("Failed to flush stdin")?;
            // Close stdin to signal end of input
            drop(stdin);
        }
        Ok(())
    }

    /// Read the next streaming JSON event from stdout
    pub async fn next_event(&mut self) -> Option<Result<serde_json::Value>> {
        loop {
            let mut line = String::new();
            match self.stdout.read_line(&mut line).await {
                Ok(0) => return None, // EOF
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue; // Skip empty lines
                    }

                    debug!("Received Claude output line: {}", line);

                    match serde_json::from_str(line) {
                        Ok(value) => return Some(Ok(value)),
                        Err(e) => {
                            warn!("Failed to parse JSON line '{}': {}", line, e);
                            return Some(Err(anyhow::anyhow!("Invalid JSON: {}", e)));
                        }
                    }
                }
                Err(e) => return Some(Err(anyhow::anyhow!("Failed to read from stdout: {}", e))),
            }
        }
    }

    /// Kill the process and clean up
    pub async fn abort(&mut self) -> Result<()> {
        debug!("Aborting Claude session");

        if let Some(ref mut child) = self.child {
            // Kill the child process
            match child.kill().await {
                Ok(()) => debug!("Successfully killed Claude process"),
                Err(e) => warn!("Failed to kill Claude process: {}", e),
            }

            // Wait for the process to exit
            match child.wait().await {
                Ok(status) => debug!("Claude process exited with status: {}", status),
                Err(e) => warn!("Failed to wait for Claude process: {}", e),
            }
        }

        Ok(())
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(e) = child.start_kill() {
                warn!("Failed to kill Claude process in Drop: {}", e);
                return;
            }
            // Spawn an async task to reap the child so we don't leave a zombie
            // in the process table.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}
