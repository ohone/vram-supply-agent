use std::process::Stdio;

use tokio::process::Command;
use tracing::{debug, warn};

use super::openai;

#[derive(Debug)]
pub struct PrereqStatus {
    pub provider: String,
    pub claude_version: Option<String>,
    pub srt_version: Option<String>,
    pub api_key_set: bool,
    pub claude_authenticated: bool,
    pub openai_connected: bool,
}

/// Check all prerequisites for sell-quota mode
pub async fn check_prerequisites(provider: &str) -> PrereqStatus {
    let api_key_set = check_api_key_set();

    let (claude_version, srt_version, claude_authenticated, openai_connected) = match provider {
        "openai-codex" => (None, None, false, openai::has_connection()),
        _ => (
            check_claude_version().await,
            check_srt_version().await,
            check_claude_auth().await,
            false,
        ),
    };

    PrereqStatus {
        provider: provider.to_string(),
        claude_version,
        srt_version,
        api_key_set,
        claude_authenticated,
        openai_connected,
    }
}

/// Print prerequisite status with actionable instructions
pub fn print_prereq_status(status: &PrereqStatus) {
    println!("Checking prerequisites...");

    if status.provider == "openai-codex" {
        if status.api_key_set {
            println!("  ✓ VRAM_SUPPLY_API_KEY configured");
        } else {
            println!("  ✗ VRAM_SUPPLY_API_KEY not set");
            println!();
            println!("  Set your API key:");
            println!("    export VRAM_SUPPLY_API_KEY=your_key_here");
            println!("    # Get your key from https://vram.supply/keys");
            println!();
        }

        if status.openai_connected {
            println!("  ✓ OpenAI Codex connected");
        } else {
            println!("  ✗ OpenAI Codex not connected");
            println!();
            println!("  Connect OpenAI first:");
            println!("    vramsupply connect openai");
            println!();
        }

        if !all_ok(status) {
            println!("  Then re-run: vramsupply sell-quota --provider openai-codex");
            println!();
        }
        return;
    }

    match &status.claude_version {
        Some(version) => println!("  ✓ claude {}", version),
        None => {
            println!("  ✗ claude CLI not found");
            println!();
            println!("  Install Claude Desktop or CLI:");
            println!("    https://claude.ai/download");
            println!();
        }
    }

    match &status.srt_version {
        Some(version) => println!("  ✓ sandbox-runtime {}", version),
        None => {
            println!("  ✗ sandbox-runtime not found");
            println!();
            println!("  Install sandbox-runtime:");
            println!("    npm install -g @anthropic-ai/sandbox-runtime");
            println!();
        }
    }

    if status.api_key_set {
        println!("  ✓ VRAM_SUPPLY_API_KEY configured");
    } else {
        println!("  ✗ VRAM_SUPPLY_API_KEY not set");
        println!();
        println!("  Set your API key:");
        println!("    export VRAM_SUPPLY_API_KEY=your_key_here");
        println!("    # Get your key from https://vram.supply/keys");
        println!();
    }

    if status.claude_authenticated {
        println!("  ✓ Claude Code authenticated");
    } else {
        println!("  ✗ Claude Code not authenticated");
        println!();
        println!("  Authenticate with Claude:");
        println!("    claude auth login");
        println!();
    }

    if !all_ok(status) {
        println!("  Then re-run: vramsupply sell-quota");
        println!();
    }
}

/// Check if all prerequisites are satisfied
pub fn all_ok(status: &PrereqStatus) -> bool {
    if status.provider == "openai-codex" {
        return status.api_key_set && status.openai_connected;
    }

    status.claude_version.is_some()
        && status.srt_version.is_some()
        && status.api_key_set
        && status.claude_authenticated
}

async fn check_claude_version() -> Option<String> {
    match Command::new("claude")
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim();
            if !version.is_empty() {
                debug!("Found Claude CLI version: {}", version);
                Some(version.to_string())
            } else {
                warn!("Claude CLI found but version output was empty");
                None
            }
        }
        Ok(output) => {
            warn!("Claude CLI exited with status: {}", output.status);
            None
        }
        Err(e) => {
            debug!("Claude CLI not found: {}", e);
            None
        }
    }
}

async fn check_srt_version() -> Option<String> {
    // Try npx first, then direct srt command
    for cmd in &["npx", "srt"] {
        let args = if *cmd == "npx" {
            vec!["@anthropic-ai/sandbox-runtime", "--version"]
        } else {
            vec!["--version"]
        };

        match Command::new(cmd)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                let version = version.trim();
                if !version.is_empty() {
                    debug!("Found sandbox-runtime version: {} (via {})", version, cmd);
                    return Some(version.to_string());
                }
            }
            Ok(output) => {
                debug!(
                    "Sandbox-runtime via {} exited with status: {}",
                    cmd, output.status
                );
            }
            Err(e) => {
                debug!("Sandbox-runtime via {} not found: {}", cmd, e);
            }
        }
    }
    None
}

fn check_api_key_set() -> bool {
    std::env::var("VRAM_SUPPLY_API_KEY")
        .map(|val| !val.trim().is_empty())
        .unwrap_or(false)
}

async fn check_claude_auth() -> bool {
    match Command::new("claude")
        .args(["auth", "status"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let status = String::from_utf8_lossy(&output.stdout);
            let is_authenticated =
                status.contains("authenticated") || status.contains("\"loggedIn\": true");
            debug!("Claude auth status: {}", status.trim());
            is_authenticated
        }
        Ok(output) => {
            warn!("Claude auth status exited with: {}", output.status);
            false
        }
        Err(e) => {
            debug!("Failed to check Claude auth status: {}", e);
            false
        }
    }
}
