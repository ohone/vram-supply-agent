use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;
use tokio::fs;
use tracing::debug;
use uuid::Uuid;

pub struct SandboxSession {
    pub tmpdir: PathBuf,
    pub config_path: PathBuf,
}

impl SandboxSession {
    /// Create a new sandbox session with tmpdir and config file
    pub async fn create(deny_read_paths: &[String]) -> Result<Self> {
        // Create temporary directory
        let tmpdir = std::env::temp_dir().join(format!("vram-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmpdir)
            .await
            .with_context(|| format!("Failed to create tmpdir: {}", tmpdir.display()))?;

        debug!("Created sandbox tmpdir: {}", tmpdir.display());

        // Create SRT config file
        let config_path = tmpdir.join(".srt-config.json");
        let config = create_srt_config(&tmpdir, deny_read_paths)?;

        fs::write(&config_path, config)
            .await
            .with_context(|| format!("Failed to write SRT config: {}", config_path.display()))?;

        debug!("Created SRT config: {}", config_path.display());

        Ok(SandboxSession {
            tmpdir,
            config_path,
        })
    }

    /// Clean up the tmpdir and all contents
    pub async fn cleanup(&self) -> Result<()> {
        if self.tmpdir.exists() {
            fs::remove_dir_all(&self.tmpdir)
                .await
                .with_context(|| format!("Failed to cleanup tmpdir: {}", self.tmpdir.display()))?;
            debug!("Cleaned up sandbox tmpdir: {}", self.tmpdir.display());
        }
        Ok(())
    }
}

impl Drop for SandboxSession {
    fn drop(&mut self) {
        if self.tmpdir.exists() {
            let tmpdir = self.tmpdir.clone();
            tokio::spawn(async move {
                if let Err(e) = fs::remove_dir_all(&tmpdir).await {
                    tracing::warn!("Failed to cleanup tmpdir in Drop: {}", e);
                }
            });
        }
    }
}

/// Create the SRT configuration JSON with security restrictions
fn create_srt_config(tmpdir: &Path, additional_deny_read: &[String]) -> Result<String> {
    // Convert tmpdir to absolute path for config
    let tmpdir_abs = tmpdir
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize tmpdir: {}", tmpdir.display()))?;

    // Build deny_read list with security-critical defaults
    let mut deny_read_paths = get_default_deny_read_paths();
    deny_read_paths.extend(additional_deny_read.iter().cloned());

    let config = json!({
        "network": {
            "allowedDomains": [
                "api.anthropic.com",
                "statsig.anthropic.com"
            ],
            "deniedDomains": []
        },
        "filesystem": {
            "allowWrite": [format!("//{}", tmpdir_abs.display())],
            "denyRead": deny_read_paths,
            "denyWrite": []
        }
    });

    serde_json::to_string_pretty(&config).context("Failed to serialize SRT config to JSON")
}

/// Get default security-critical paths that should be denied read access.
///
/// Paths use the SRT `//` prefix convention (absolute path marker for the
/// sandbox runtime). Only `dirs::home_dir()`-based paths are included —
/// hardcoded `/home/user/` entries were removed because they don't match
/// real home directories on macOS (`/Users/<name>/`) or Linux systems where
/// the username isn't `user`.
///
/// Additional paths can be added via the `VRAM_SUPPLY_QUOTA_DENY_READ`
/// environment variable (comma-separated absolute paths).
fn get_default_deny_read_paths() -> Vec<String> {
    let mut paths = Vec::new();

    // Add user home directory sensitive paths (detected at runtime)
    if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();

        // Credentials and keys
        paths.extend([
            format!("//{home_str}/.ssh"),
            format!("//{home_str}/.aws"),
            format!("//{home_str}/.gnupg"),
            format!("//{home_str}/.netrc"),
            format!("//{home_str}/.npmrc"),
            format!("//{home_str}/.gitconfig"),
        ]);

        // Application config that may contain secrets
        paths.extend([
            format!("//{home_str}/.config"),
            format!("//{home_str}/.kube"),
            format!("//{home_str}/.docker"),
        ]);

        // vram.supply agent config (contains API key)
        paths.extend([
            format!("//{home_str}/.vram-supply"),
            format!("//{home_str}/.claude"),
        ]);

        // Dotenv files
        paths.push(format!("//{home_str}/.env"));
    }

    // Add the agent process working directory to prevent the sandbox
    // from reading the agent's own config or source
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.display().to_string();
        paths.push(format!("//{cwd_str}"));
    }

    // Add environment-specific deny paths from VRAM_SUPPLY_QUOTA_DENY_READ
    if let Ok(env_paths) = std::env::var("VRAM_SUPPLY_QUOTA_DENY_READ") {
        for path in env_paths.split(',') {
            let path = path.trim();
            if !path.is_empty() {
                // Accept both with and without // prefix from user input
                if path.starts_with("//") {
                    paths.push(path.to_string());
                } else {
                    paths.push(format!("//{path}"));
                }
            }
        }
    }

    paths
}
