pub mod credentials;

use std::net::TcpListener;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::Config;
use credentials::{load_credentials, save_credentials, Credentials};

const OAUTH_CLIENT_ID: &str = "vram-supply-agent";
const TOKEN_REFRESH_BUFFER_SECS: u64 = 300;

/// Current Unix epoch timestamp in seconds.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_secs()
}

/// Serializes concurrent calls to `load_valid_credentials` so that only one
/// task performs a refresh-token exchange at a time.
static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
    REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Generate a cryptographically random code verifier (43-128 URL-safe chars).
fn generate_code_verifier() -> String {
    use uuid::Uuid;
    // Generate enough random material: 3 UUIDs gives us ~96 hex chars
    let raw = format!("{}{}{}", Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    // Keep only alphanumeric chars (URL-safe), truncate to 128
    let verifier: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(128)
        .collect();
    verifier
}

/// SHA256 hash the verifier and base64url-encode it.
fn compute_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

/// PKCE Authorization Code Flow.
pub async fn login_pkce(config: &Config) -> Result<()> {
    let code_verifier = generate_code_verifier();
    let code_challenge = compute_code_challenge(&code_verifier);
    let state = uuid::Uuid::new_v4().to_string();

    // Find a free port for the callback server
    let listener = TcpListener::bind("127.0.0.1:0").context("Failed to bind callback listener")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{}/callback", port);

    // Build authorization URL
    let mut auth_url = Url::parse(&format!("{}/oauth/authorize", config.platform_url))
        .context("Failed to parse platform URL")?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);

    tracing::info!("Opening browser for authentication...");
    println!("Opening browser to authenticate...");
    println!("If the browser doesn't open, visit:\n{}\n", auth_url);

    if let Err(e) = open::that(auth_url.as_str()) {
        tracing::warn!(
            "Failed to open browser: {}. Please open the URL manually.",
            e
        );
    }

    // Start tiny_http server to receive the callback
    let server = tiny_http::Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("Failed to create HTTP server: {}", e))?;

    println!("Waiting for authorization callback on port {}...", port);

    // Wait for the callback request
    let request = server
        .recv()
        .map_err(|e| anyhow::anyhow!("Failed to receive callback request: {}", e))?;

    let request_url = request.url().to_string();
    tracing::debug!("Received callback: {}", request_url);

    // Parse the query parameters from the callback
    let callback_url = Url::parse(&format!("http://localhost{}", request_url))
        .context("Failed to parse callback URL")?;

    let params: std::collections::HashMap<String, String> =
        callback_url.query_pairs().into_owned().collect();

    // Send a response to the browser
    let response = tiny_http::Response::from_string(
        "<html><body><h1>Authentication successful!</h1><p>You can close this window and return to the terminal.</p></body></html>"
    ).with_header(
        // Static string, infallible parse
        "Content-Type: text/html".parse::<tiny_http::Header>().expect("valid static header")
    );
    let _ = request.respond(response);

    // Verify state
    let received_state = params
        .get("state")
        .ok_or_else(|| anyhow::anyhow!("No state parameter in callback"))?;
    if received_state != &state {
        bail!("State mismatch in OAuth callback — possible CSRF attack");
    }

    // Check for error
    if let Some(error) = params.get("error") {
        let desc = params
            .get("error_description")
            .map(|s| s.as_str())
            .unwrap_or("Unknown error");
        bail!("Authorization failed: {} — {}", error, desc);
    }

    // Extract auth code
    let code = params
        .get("code")
        .ok_or_else(|| anyhow::anyhow!("No authorization code in callback"))?;

    tracing::info!("Received authorization code, exchanging for tokens...");

    // Exchange code for tokens
    let client = reqwest::Client::new();
    let token_response = client
        .post(format!("{}/oauth/token", config.platform_url))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", OAUTH_CLIENT_ID),
            ("code", code),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", &code_verifier),
        ])
        .send()
        .await
        .context("Failed to send token exchange request")?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let body = token_response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown".to_string());
        bail!("Token exchange failed with status {}: {}", status, body);
    }

    let token_data: TokenResponse = token_response
        .json()
        .await
        .context("Failed to parse token response")?;

    let now = unix_now();

    let creds = Credentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        expires_at: now + token_data.expires_in,
    };

    save_credentials(&creds)?;
    println!("Authentication successful! Credentials saved.");
    Ok(())
}

/// Device Code Flow for headless environments.
pub async fn login_device_code(config: &Config) -> Result<()> {
    let client = reqwest::Client::new();

    // Request device code
    let device_response = client
        .post(format!("{}/oauth/device", config.platform_url))
        .form(&[("client_id", OAUTH_CLIENT_ID)])
        .send()
        .await
        .context("Failed to request device code")?;

    if !device_response.status().is_success() {
        let status = device_response.status();
        let body = device_response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown".to_string());
        bail!(
            "Device code request failed with status {}: {}",
            status,
            body
        );
    }

    let device_data: DeviceCodeResponse = device_response
        .json()
        .await
        .context("Failed to parse device code response")?;

    println!("\nTo authenticate, visit:");
    println!("  {}", device_data.verification_uri);
    println!("\nAnd enter the code:");
    println!("  {}\n", device_data.user_code);

    let interval = Duration::from_secs(device_data.interval.unwrap_or(5));

    // Poll for authorization
    loop {
        tokio::time::sleep(interval).await;

        let poll_response = client
            .post(format!("{}/oauth/token", config.platform_url))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", OAUTH_CLIENT_ID),
                ("device_code", &device_data.device_code),
            ])
            .send()
            .await
            .context("Failed to poll for device authorization")?;

        if poll_response.status().is_success() {
            let token_data: TokenResponse = poll_response
                .json()
                .await
                .context("Failed to parse token response")?;

            let now = unix_now();

            let creds = Credentials {
                access_token: token_data.access_token,
                refresh_token: token_data.refresh_token,
                expires_at: now + token_data.expires_in,
            };

            save_credentials(&creds)?;
            println!("Authentication successful! Credentials saved.");
            return Ok(());
        }

        // Parse error response to check if we should keep polling
        let body = poll_response
            .text()
            .await
            .unwrap_or_else(|_| "{}".to_string());

        let error_response: serde_json::Value =
            serde_json::from_str(&body).unwrap_or(serde_json::json!({}));

        match error_response.get("error").and_then(|e| e.as_str()) {
            Some("authorization_pending") => {
                // User hasn't authorized yet, keep polling
                continue;
            }
            Some("slow_down") => {
                // Back off
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => {
                bail!("Device code expired. Please try again.");
            }
            Some(err) => {
                bail!("Device authorization failed: {}", err);
            }
            None => {
                bail!("Unexpected response during device authorization: {}", body);
            }
        }
    }
}

/// Load credentials, refreshing if they expire within 5 minutes.
///
/// A std::sync::Mutex serializes concurrent callers so only one task performs
/// the refresh-token exchange; others re-read the already-refreshed file.
pub async fn load_valid_credentials(config: &Config) -> Result<Credentials> {
    let _guard = refresh_lock().lock().await;

    let creds = load_credentials()?;

    let now = unix_now();

    // Refresh if expiring within 5 minutes
    if creds.expires_at <= now + TOKEN_REFRESH_BUFFER_SECS {
        tracing::info!("Access token expiring soon, refreshing...");
        return refresh_token(config, &creds.refresh_token).await;
    }

    Ok(creds)
}

async fn refresh_token(config: &Config, refresh_token: &str) -> Result<Credentials> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/oauth/token", config.platform_url))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", OAUTH_CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("Failed to send token refresh request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown".to_string());
        bail!("Token refresh failed with status {}: {}", status, body);
    }

    let token_data: TokenResponse = response
        .json()
        .await
        .context("Failed to parse refresh token response")?;

    let now = unix_now();

    let creds = Credentials {
        access_token: token_data.access_token,
        refresh_token: token_data.refresh_token,
        expires_at: now + token_data.expires_in,
    };

    save_credentials(&creds)?;
    tracing::info!("Token refreshed successfully");
    Ok(creds)
}

pub fn show_auth_status() -> Result<()> {
    match load_credentials() {
        Ok(creds) => {
            let now = unix_now();

            if creds.expires_at > now {
                let remaining = creds.expires_at - now;
                let hours = remaining / 3600;
                let minutes = (remaining % 3600) / 60;
                println!("Authenticated");
                println!("Token expires in: {}h {}m", hours, minutes);
            } else {
                println!("Authenticated (token expired — will refresh on next use)");
            }
        }
        Err(_) => {
            println!("Not authenticated. Run `vramsply auth login` to authenticate.");
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Option<u64>,
}

use serde::Deserialize;

/// Ensure valid credentials exist, triggering login if needed.
pub async fn ensure_authenticated(config: &Config, headless: bool) -> Result<Credentials> {
    match load_valid_credentials(config).await {
        Ok(creds) => Ok(creds),
        Err(_) => {
            tracing::info!("No valid credentials found, initiating login...");
            if headless {
                login_device_code(config).await?;
            } else {
                login_pkce(config).await?;
            }
            load_valid_credentials(config).await
        }
    }
}
