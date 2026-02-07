use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

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
        #[cfg(unix)]
        {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
    }
    let json = serde_json::to_string_pretty(creds).context("Failed to serialize credentials")?;
    #[cfg(unix)]
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("Failed to open credentials at {}", path.display()))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("Failed to write credentials to {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&path, json)
            .with_context(|| format!("Failed to write credentials to {}", path.display()))?;
    }
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
