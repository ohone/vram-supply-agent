use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdentityFile {
    agent_uid: String,
}

#[derive(Debug, Clone)]
pub struct AgentIdentity {
    pub agent_uid: String,
    pub device_name: String,
    pub platform: String,
    pub arch: String,
    pub agent_version: String,
}

fn identity_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".vram-supply").join("vramsply.json"))
}

fn detect_hostname() -> Option<String> {
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn read_agent_uid(path: &PathBuf) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed reading identity file {}", path.display()))?;
    let data: IdentityFile = serde_json::from_str(&raw)
        .with_context(|| format!("Failed parsing identity file {}", path.display()))?;
    Ok(Some(data.agent_uid))
}

fn write_agent_uid(path: &PathBuf, agent_uid: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed creating directory {}", parent.display()))?;
    }

    let data = IdentityFile {
        agent_uid: agent_uid.to_string(),
    };
    let json = serde_json::to_string_pretty(&data)?;
    fs::write(path, json)
        .with_context(|| format!("Failed writing identity file {}", path.display()))?;

    Ok(())
}

pub fn load_or_create_identity() -> Result<AgentIdentity> {
    let path = identity_path()?;
    let agent_uid = match read_agent_uid(&path)? {
        Some(uid) => uid,
        None => {
            let uid = Uuid::new_v4().to_string();
            write_agent_uid(&path, &uid)?;
            uid
        }
    };

    let hostname = detect_hostname().unwrap_or_else(|| "unknown-host".to_string());
    let platform = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();

    Ok(AgentIdentity {
        agent_uid,
        device_name: format!("{} ({})", hostname, platform),
        platform,
        arch,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
