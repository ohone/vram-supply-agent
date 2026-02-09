mod auth;
mod backend;
mod config;
mod identity;
mod models;
mod presence;
mod verification;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use presence::{AgentPresenceStatus, PresenceHandle};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "vramsply",
    about = "vram.supply provider agent — connect your model inference node to the marketplace",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check API key authentication status
    Auth,
    /// Start providing model inference
    Serve {
        /// Path to the model file to serve
        #[arg(long)]
        model: Option<String>,

        /// Override the model name sent to the platform (e.g., "meta-llama/llama-3.1-8b-instruct")
        #[arg(long)]
        model_name: Option<String>,

        /// HuggingFace repository ID for model verification (e.g., TheBloke/Llama-2-7B-GGUF)
        #[arg(long)]
        hf_repo: Option<String>,

        /// Skip model integrity verification
        #[arg(long)]
        skip_verify: bool,
    },
    /// Model management commands
    Models {
        #[command(subcommand)]
        command: ModelCommands,
    },
    /// Run a benchmark on a model
    Benchmark {
        /// Path to the model file to benchmark
        model_path: String,
    },
    /// Show current agent status
    Status,
}

#[derive(Subcommand)]
enum ModelCommands {
    /// List locally available models
    List,
    /// Download a model from HuggingFace
    Pull {
        /// HuggingFace repository ID (e.g., TheBloke/Llama-2-7B-GGUF)
        hf_repo_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let config = config::Config::load()?;

    match cli.command {
        Commands::Auth => {
            auth::show_auth_status();
        }

        Commands::Serve {
            model,
            model_name,
            hf_repo,
            skip_verify,
        } => {
            run_serve(&config, model, model_name, hf_repo, skip_verify).await?;
        }

        Commands::Models { command } => match command {
            ModelCommands::List => {
                let local_models = models::list_local_models(&config)?;
                if local_models.is_empty() {
                    println!("No local models found in {}", config.model_dir.display());
                    println!("Download models with: vramsply models pull <hf_repo_id>");
                } else {
                    println!("Local models ({}):", config.model_dir.display());
                    for m in &local_models {
                        println!(
                            "  {} — {} ({})",
                            m.name,
                            m.path,
                            models::format_size(m.size_bytes)
                        );
                    }
                }
            }
            ModelCommands::Pull { hf_repo_id } => {
                models::pull_model(&hf_repo_id)?;
            }
        },

        Commands::Benchmark { model_path } => {
            println!("Benchmarking model: {}", model_path);
            println!("Benchmark not yet implemented");
        }

        Commands::Status => {
            println!("Agent status:");
            auth::show_auth_status();

            let local_models = models::list_local_models(&config)?;
            println!("Local models: {}", local_models.len());
        }
    }

    Ok(())
}

#[derive(serde::Serialize)]
struct RegisterRequest {
    endpoint_url: String,
    model: String,
    max_concurrent: u32,
    context_length_offered: u32,
    input_price_per_million: u32,
    output_price_per_million: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_sha256: Option<String>,
}

#[derive(serde::Deserialize)]
struct RegisterResponse {
    id: String,
    status: String,
}

async fn run_serve(
    config: &config::Config,
    model_arg: Option<String>,
    model_name_override: Option<String>,
    hf_repo: Option<String>,
    skip_verify: bool,
) -> Result<()> {
    let shutdown = CancellationToken::new();

    let token = Arc::new(tokio::sync::Mutex::new(config.api_key.clone()));
    let identity = identity::load_or_create_identity()?;
    let client = reqwest::Client::new();

    // Determine which model to serve
    let model_path = match model_arg {
        Some(m) => models::find_model(config, &m)?,
        None => {
            let local = models::list_local_models(config)?;
            if local.is_empty() {
                anyhow::bail!(
                    "No models found. Specify --model or download one with: vramsply models pull <hf_repo_id>"
                );
            }
            if local.len() > 1 {
                println!("Multiple models found, using first one: {}", local[0].name);
                println!("Use --model to specify a different one.");
            }
            local[0].path.clone()
        }
    };
    tracing::info!("Serving model: {}", model_path);

    // Verify model integrity
    let model_sha256 = if skip_verify {
        verification::verify_model(&model_path, "", true).await?
    } else {
        let hf_repo_id = hf_repo.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Model verification requires --hf-repo <repo_id> \
                 (e.g., --hf-repo TheBloke/Llama-2-7B-GGUF).\n\
                 Use --skip-verify to bypass verification."
            )
        })?;
        let sha = verification::verify_model(&model_path, hf_repo_id, false).await?;
        println!("Model verified: {} (SHA-256: {})", hf_repo_id, sha);
        sha
    };

    let model_name = match model_name_override {
        Some(name) => name,
        None => models::normalize_model_name(&model_path),
    };

    // Create presence handle and start heartbeat loop
    let presence = PresenceHandle::new(
        Some(model_name.clone()),
        client.clone(),
        config.clone(),
        Arc::clone(&token),
        identity.clone(),
    );
    presence.publish().await;
    let presence_handle = presence.spawn_loop(shutdown.clone());

    // Start llama-server
    presence.transition(AgentPresenceStatus::LoadingModel).await;
    let llama = Arc::new(tokio::sync::Mutex::new(backend::LlamaServer::new(
        model_path.clone(),
        config.port,
        config.llama_server_path.clone(),
        config.gpu_layers,
        config.context_length_offered,
    )));
    if let Err(e) = llama.lock().await.start().await {
        presence
            .report_error("llama_start_failed", &e.to_string())
            .await;
        shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), presence_handle).await;
        return Err(e);
    }
    tracing::info!("llama-server is healthy on port {}", config.port);

    // Register with platform
    let model_sha256_field = if model_sha256 == "unverified" {
        None
    } else {
        Some(model_sha256)
    };
    let reg = register_with_platform(
        &client,
        config,
        &token,
        &model_name,
        model_sha256_field,
        &presence,
    )
    .await?;
    let deregister_url = format!("{}/v1/providers/{}", config.platform_url, reg.id);

    presence.transition(AgentPresenceStatus::Ready).await;
    println!("vram.supply provider runtime is running. Press Ctrl+C to stop.");
    println!("  Model: {}", model_name);
    println!("  Endpoint: {}", config.public_url);
    println!("  Instance ID: {}", reg.id);

    // Spawn background tasks
    let heartbeat_handle = spawn_heartbeat_loop(
        client.clone(),
        config.clone(),
        Arc::clone(&token),
        shutdown.clone(),
    );
    let monitor_handle =
        spawn_health_monitor(Arc::clone(&llama), presence.clone(), shutdown.clone());

    // Wait for shutdown signal
    tokio::signal::ctrl_c()
        .await
        .context("Failed to listen for Ctrl+C")?;

    tracing::info!("Shutting down...");
    println!("\nShutting down...");

    // Signal all tasks to stop
    shutdown.cancel();

    // Explicitly stop llama-server before waiting on tasks
    if let Err(e) = llama.lock().await.stop().await {
        tracing::warn!("Error stopping llama-server: {}", e);
    }

    presence.transition(AgentPresenceStatus::Unavailable).await;

    // Deregister (best-effort on shutdown path — log but don't propagate)
    let current_token = token.lock().await.clone();
    match client
        .delete(&deregister_url)
        .header("Authorization", format!("Bearer {}", current_token))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!("Deregistered from platform");
        }
        Ok(resp) => {
            tracing::warn!("Deregister returned HTTP {}", resp.status());
        }
        Err(e) => {
            tracing::warn!("Failed to deregister from platform: {}", e);
        }
    }

    // Wait for tasks to finish (with timeout)
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let (r1, r2, r3) = tokio::join!(heartbeat_handle, monitor_handle, presence_handle);
        let _ = (r1, r2, r3);
    })
    .await;

    Ok(())
}

/// Register this provider instance with the platform, returning the response.
async fn register_with_platform(
    client: &reqwest::Client,
    config: &config::Config,
    token: &Arc<tokio::sync::Mutex<String>>,
    model_name: &str,
    model_sha256: Option<String>,
    presence: &PresenceHandle,
) -> Result<RegisterResponse> {
    let register_url = format!("{}/v1/providers/register", config.platform_url);
    let register_body = RegisterRequest {
        endpoint_url: config.public_url.clone(),
        model: model_name.to_string(),
        max_concurrent: config.max_concurrent,
        context_length_offered: config.context_length_offered,
        input_price_per_million: config.input_price_per_million,
        output_price_per_million: config.output_price_per_million,
        model_sha256,
    };

    let reg_token = token.lock().await.clone();
    let res = client
        .post(&register_url)
        .header("Authorization", format!("Bearer {}", reg_token))
        .json(&register_body)
        .send()
        .await
        .map_err(|e| {
            // Fire-and-forget: presence will be updated after this returns Err
            tracing::error!("Registration request failed: {}", e);
            e
        })?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        presence
            .report_error(
                "provider_register_failed",
                &format!("status {}: {}", status, body),
            )
            .await;
        anyhow::bail!("Registration failed ({}): {}", status, body);
    }

    let reg: RegisterResponse = res.json().await.map_err(|e| {
        tracing::error!("Invalid registration response: {}", e);
        e
    })?;

    tracing::info!(
        "Registered with platform: id={}, status={}",
        reg.id,
        reg.status
    );
    Ok(reg)
}

/// Spawn a heartbeat loop that pings the platform periodically.
fn spawn_heartbeat_loop(
    client: reqwest::Client,
    config: config::Config,
    token: Arc<tokio::sync::Mutex<String>>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let heartbeat_url = format!("{}/v1/providers/heartbeat", config.platform_url);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }

            let current_token = token.lock().await.clone();
            let res = client
                .post(&heartbeat_url)
                .header("Authorization", format!("Bearer {}", current_token))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    tracing::trace!("Heartbeat sent");
                }
                Ok(r) => {
                    tracing::warn!("Heartbeat failed: {}", r.status());
                }
                Err(e) => {
                    tracing::warn!("Heartbeat error: {}", e);
                }
            }
        }
    })
}

/// Spawn a health monitor that checks llama-server status and restarts it if needed.
fn spawn_health_monitor(
    llama: Arc<tokio::sync::Mutex<backend::LlamaServer>>,
    presence: PresenceHandle,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }
            let mut llama = llama.lock().await;
            if !llama.is_running() {
                tracing::warn!("llama-server has stopped, attempting restart...");
                presence
                    .report_degraded("llama_stopped", "llama-server process stopped unexpectedly")
                    .await;
                if let Err(e) = llama.restart_with_backoff().await {
                    tracing::error!("Failed to restart llama-server: {}", e);
                    presence
                        .report_error("llama_restart_failed", &e.to_string())
                        .await;
                    continue;
                }
                presence.transition(AgentPresenceStatus::Ready).await;
            } else {
                match llama.active_requests().await {
                    Ok(active) => {
                        presence.update_active_requests(active).await;
                    }
                    Err(e) => {
                        tracing::debug!("Failed to inspect active request count: {}", e);
                    }
                }
            }
        }
    })
}
