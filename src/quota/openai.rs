use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};
use url::Url;
use uuid::Uuid;

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const OPENAI_SCOPES: &str = "openid profile email offline_access";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenAiConnection {
    pub provider: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub chatgpt_account_id: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

pub fn connection_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    Ok(home
        .join(".vram-supply")
        .join("connections")
        .join("openai.json"))
}

pub fn has_connection() -> bool {
    connection_path().map(|path| path.exists()).unwrap_or(false)
}

pub fn load_connection() -> Result<OpenAiConnection> {
    let path = connection_path()?;
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))
}

fn save_connection(connection: &OpenAiConnection) -> Result<()> {
    let path = connection_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(connection)?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, permissions)
            .with_context(|| format!("Failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

pub async fn connect_openai() -> Result<()> {
    let state = Uuid::new_v4().to_string();
    let code_verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(code_verifier.as_bytes()));

    let mut authorize_url = Url::parse(OPENAI_AUTHORIZE_URL)?;
    authorize_url
        .query_pairs_mut()
        .append_pair("client_id", OPENAI_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", OPENAI_REDIRECT_URI)
        .append_pair("scope", OPENAI_SCOPES)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);

    println!("Opening browser for OpenAI authentication...");
    println!("If the browser does not open, visit:\n  {}", authorize_url);
    let _ = open_browser(authorize_url.as_str());

    let listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .context("Failed to bind local callback server on 127.0.0.1:1455")?;

    let (code, returned_state) = wait_for_callback(listener).await?;
    if returned_state != state {
        bail!("OAuth state mismatch; aborting")
    }

    let client = reqwest::Client::new();
    let token = client
        .post(OPENAI_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CLIENT_ID),
            ("code", &code),
            ("redirect_uri", OPENAI_REDIRECT_URI),
            ("code_verifier", &code_verifier),
        ])
        .send()
        .await
        .context("Failed to exchange authorization code for token")?
        .error_for_status()
        .context("OpenAI token exchange failed")?
        .json::<TokenResponse>()
        .await
        .context("Failed to parse OpenAI token response")?;

    let chatgpt_account_id = extract_chatgpt_account_id(&token.access_token)?;
    let connection = OpenAiConnection {
        provider: "openai-codex".to_string(),
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: chrono_like_now() + token.expires_in,
        chatgpt_account_id,
    };

    save_connection(&connection)?;
    println!(
        "Connected OpenAI Codex. Stored credentials in {}",
        connection_path()?.display()
    );
    println!("Then run: vramsply sell-quota --provider openai-codex");
    Ok(())
}

fn extract_chatgpt_account_id(access_token: &str) -> Result<String> {
    let payload = access_token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow!("Access token did not look like a JWT"))?;

    let decoded = decode_base64url(payload)?;
    let value: serde_json::Value =
        serde_json::from_slice(&decoded).context("Failed to parse OpenAI access token payload")?;

    let auth_claim = value
        .get("https://api.openai.com/auth")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("Missing https://api.openai.com/auth claim"))?;

    auth_claim
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("Missing chatgpt_account_id claim"))
}

fn decode_base64url(input: &str) -> Result<Vec<u8>> {
    let remainder = input.len() % 4;
    let padded = if remainder == 0 {
        input.to_string()
    } else {
        format!("{}{}", input, "=".repeat(4 - remainder))
    };

    base64::engine::general_purpose::URL_SAFE
        .decode(padded)
        .context("Failed to decode JWT payload")
}

async fn wait_for_callback(listener: TcpListener) -> Result<(String, String)> {
    let (mut stream, _) = timeout(Duration::from_secs(180), listener.accept())
        .await
        .context("Timed out waiting for OAuth callback")?
        .context("Failed to accept OAuth callback connection")?;

    let mut buffer = [0_u8; 4096];
    let read = stream
        .read(&mut buffer)
        .await
        .context("Failed to read callback")?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("Missing HTTP request line in callback"))?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("Malformed callback request line"))?;

    let callback_url = Url::parse(&format!("http://127.0.0.1{}", path))?;
    let code = callback_url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| anyhow!("Missing code in OAuth callback"))?;
    let state = callback_url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .ok_or_else(|| anyhow!("Missing state in OAuth callback"))?;

    let response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "Connection: close\r\n\r\n",
        "<html><body><p>OpenAI connected. You can close this window.</p></body></html>"
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("Failed to write OAuth callback response")?;

    Ok((code, state))
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .status()
            .context("Failed to run open")?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(url)
            .status()
            .context("Failed to run xdg-open")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
            .context("Failed to run start")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Ok(())
}

fn chrono_like_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
