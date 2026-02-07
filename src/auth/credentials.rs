use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
}

fn credentials_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".vram-supply").join("credentials.json"))
}

pub fn save_credentials(creds: &Credentials) -> Result<()> {
    let path = credentials_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(creds).context("Failed to serialize credentials")?;
    fs::write(&path, json)
        .with_context(|| format!("Failed to write credentials to {}", path.display()))?;
    tracing::info!("Credentials saved to {}", path.display());
    Ok(())
}

pub fn load_credentials() -> Result<Credentials> {
    let path = credentials_path()?;
    let data = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read credentials from {}", path.display()))?;
    let creds: Credentials =
        serde_json::from_str(&data).context("Failed to parse credentials file")?;
    Ok(creds)
}

pub fn clear_credentials() -> Result<()> {
    let path = credentials_path()?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("Failed to delete credentials at {}", path.display()))?;
        tracing::info!("Credentials cleared");
    } else {
        tracing::info!("No credentials file found");
    }
    Ok(())
}
