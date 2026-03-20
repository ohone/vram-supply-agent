mod auth;
mod backend;
mod config;
mod identity;
mod models;
mod presence;
mod quota;
mod verification;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use presence::{AgentPresenceStatus, PresenceHandle};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "vramsupply",
    about = "vram.supply provider agent — connect your model inference node to the marketplace",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SellQuotaProviderArg {
    Claude,
    Codex,
}

impl SellQuotaProviderArg {
    fn backend_name(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "openai-codex",
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Check API key authentication status
    Auth,
    /// Start providing model inference
    Serve {
        /// Model to serve: a local GGUF path or a canonical HuggingFace model ID (e.g., "qwen/qwen3.5-9b")
        #[arg(long)]
        model: Option<String>,

        /// Quantization level (e.g., Q4_K_M, Q8_0). Required when --model is a canonical model ID.
        #[arg(long)]
        quant: Option<String>,

        /// Override the model name sent to the platform (e.g., "meta-llama/llama-3.1-8b-instruct")
        #[arg(long)]
        model_name: Option<String>,

        /// HuggingFace repository ID for model verification (e.g., TheBloke/Llama-2-7B-GGUF)
        #[arg(long)]
        hf_repo: Option<String>,

        /// Skip model integrity verification
        #[arg(long)]
        skip_verify: bool,

        /// Input token price in cents per million tokens (overrides VRAM_SUPPLY_INPUT_PRICE)
        #[arg(long)]
        input_price: Option<u32>,

        /// Output token price in cents per million tokens (overrides VRAM_SUPPLY_OUTPUT_PRICE)
        #[arg(long)]
        output_price: Option<u32>,
    },
    /// Connect a third-party subscription account
    Connect {
        /// Provider to connect (e.g., openai)
        provider: String,
    },
    /// Sell unused subscription quota via the vram.supply marketplace
    #[command(arg_required_else_help = true, after_help = "Examples:\n  vramsupply sell-quota claude --status\n  vramsupply sell-quota claude --input-price 300 --output-price 1500\n  vramsupply connect openai\n  vramsupply sell-quota codex --status")]
    SellQuota {
        /// Subscription backend to sell (`claude` or `codex`)
        #[arg(value_enum, value_name = "PROVIDER")]
        provider: SellQuotaProviderArg,

        /// Maximum concurrent sessions
        #[arg(long, default_value = "1")]
        max_concurrent: u32,

        /// Maximum spend per request in USD
        #[arg(long, default_value = "1.00")]
        max_budget_usd: f64,

        /// Model alias (e.g. sonnet, opus, gpt-4o)
        #[arg(long, default_value = "sonnet")]
        model: String,

        /// Check prerequisites and show status only
        #[arg(long)]
        status: bool,

        /// Input token price in cents per million tokens (overrides VRAM_SUPPLY_INPUT_PRICE)
        #[arg(long)]
        input_price: Option<u32>,

        /// Output token price in cents per million tokens (overrides VRAM_SUPPLY_OUTPUT_PRICE)
        #[arg(long)]
        output_price: Option<u32>,
    },
    /// Model management commands
    Models {
        #[command(subcommand)]
        command: ModelCommands,
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

        /// Specific GGUF filename to download (when repo contains multiple)
        #[arg(long)]
        file: Option<String>,
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

    // Shared HTTP client for connection pooling across all subsystems
    let http_client = reqwest::Client::new();

    match cli.command {
        Commands::Auth => {
            auth::show_auth_status();
        }

        Commands::Serve {
            model,
            quant,
            model_name,
            hf_repo,
            skip_verify,
            input_price,
            output_price,
        } => {
            let mut config = config::Config::load()?;
            if let Some(p) = input_price {
                config.input_price_per_million = p;
            }
            if let Some(p) = output_price {
                config.output_price_per_million = p;
            }
            run_serve(&http_client, &config, model, quant, model_name, hf_repo, skip_verify).await?;
        }

        Commands::Connect { provider } => {
            quota::handle_connect(&http_client, &provider).await?;
        }

        Commands::SellQuota {
            provider,
            max_concurrent,
            max_budget_usd,
            model,
            status,
            input_price,
            output_price,
        } => {
            let provider = provider.backend_name();
            let mut config = config::Config::load()?;
            if let Some(p) = input_price {
                config.input_price_per_million = p;
            }
            if let Some(p) = output_price {
                config.output_price_per_million = p;
            }
            if status {
                quota::handle_sell_quota_status(&config, provider).await?;
            } else {
                quota::handle_sell_quota(
                    &http_client,
                    &config,
                    provider,
                    max_concurrent,
                    max_budget_usd,
                    &model,
                )
                .await?;
            }
        }

        Commands::Models { command } => match command {
            ModelCommands::List => {
                let config = config::Config::load()?;
                let local_models = models::list_local_models(&config)?;
                if local_models.is_empty() {
                    println!("No local models found in {}", config.model_dir.display());
                    println!("Download models with: vramsupply models pull <hf_repo_id>");
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
            ModelCommands::Pull { hf_repo_id, file } => {
                models::pull_model(&http_client, &hf_repo_id, file.as_deref()).await?;
            }
        },

        Commands::Status => {
            let config = config::Config::load()?;
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
    client: &reqwest::Client,
    config: &config::Config,
    model_arg: Option<String>,
    quant: Option<String>,
    model_name_override: Option<String>,
    hf_repo: Option<String>,
    skip_verify: bool,
) -> Result<()> {
    let shutdown = CancellationToken::new();

    let token = Arc::new(config.api_key.clone());
    let identity = identity::load_or_create_identity()?;

    // Determine which model to serve
    let resolved = match model_arg {
        Some(m) => models::resolve_model(client, config, &m, quant.as_deref()).await?,
        None => {
            let local = models::list_local_models(config)?;
            if local.is_empty() {
                anyhow::bail!(
                    "No models found. Specify --model or download one with: vramsupply models pull <hf_repo_id>"
                );
            }
            if local.len() > 1 {
                println!("Multiple models found, using first one: {}", local[0].name);
                println!("Use --model to specify a different one.");
            }
            models::ResolvedModel {
                path: local[0].path.clone(),
                canonical_name: models::normalize_model_name(&local[0].path),
                gguf_repo: None,
            }
        }
    };
    let model_path = &resolved.path;
    tracing::info!("Serving model: {}", model_path);

    // Determine verification repo: explicit --hf-repo > resolved gguf_repo > None
    let effective_hf_repo = hf_repo.or(resolved.gguf_repo);

    // Verify model integrity
    let model_sha256 = if skip_verify {
        verification::verify_model(client, model_path, "", true).await?
    } else {
        match effective_hf_repo.as_ref() {
            Some(hf_repo_id) => {
                let sha = verification::verify_model(client, model_path, hf_repo_id, false).await?;
                println!("Model verified: {} (SHA-256: {})", hf_repo_id, sha);
                sha
            }
            None => {
                tracing::info!("No --hf-repo provided and model was not resolved from a canonical ID; skipping verification");
                verification::verify_model(client, model_path, "", true).await?
            }
        }
    };

    let model_name = match model_name_override {
        Some(name) => name,
        None => resolved.canonical_name.clone(),
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
    presence
        .transition(AgentPresenceStatus::LoadingModel)
        .await
        .expect("Idle → LoadingModel transition must be valid");
    let llama = Arc::new(tokio::sync::Mutex::new(backend::LlamaServer::new(
        model_path.clone(),
        config.port,
        config.llama_server_path.clone(),
        config.gpu_layers,
        config.context_length_offered,
        client.clone(),
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
        client,
        config,
        &token,
        &model_name,
        model_sha256_field,
        &presence,
    )
    .await?;
    let deregister_url = format!("{}/v1/providers/{}", config.platform_url, reg.id);

    presence
        .transition(AgentPresenceStatus::Ready)
        .await
        .expect("LoadingModel → Ready transition must be valid");
    println!("vram.supply provider runtime is running. Press Ctrl+C to stop.");
    println!("  Model: {}", model_name);
    println!("  Endpoint: {}", config.public_url);
    println!(
        "  Pricing: {} / {} ¢ per M tokens (input / output)",
        config.input_price_per_million, config.output_price_per_million
    );
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

    presence
        .transition(AgentPresenceStatus::Unavailable)
        .await
        .expect("Any → Unavailable transition must be valid");

    // Deregister (best-effort on shutdown path — log but don't propagate)
    match client
        .delete(&deregister_url)
        .header("Authorization", format!("Bearer {}", &*token))
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
    token: &Arc<String>,
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

    let res = client
        .post(&register_url)
        .header("Authorization", format!("Bearer {}", token))
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

/// Spawn the provider heartbeat loop (every 30s).
///
/// This sends an empty-body liveness ping to `/v1/providers/heartbeat` at the
/// provider/instance level. It is distinct from the presence loop in
/// `PresenceHandle::spawn_loop`, which sends the full agent state (status,
/// model, active requests, errors) to `/v1/agents/presence` every 15s.
fn spawn_heartbeat_loop(
    client: reqwest::Client,
    config: config::Config,
    token: Arc<String>,
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

            let res = client
                .post(&heartbeat_url)
                .header("Authorization", format!("Bearer {}", &*token))
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
            let mut guard = llama.lock().await;
            if !guard.is_running() {
                let backoff = guard.next_backoff();
                drop(guard);

                presence
                    .report_degraded("llama_stopped", "llama-server process stopped unexpectedly")
                    .await;
                tracing::warn!("Restarting llama-server after backoff of {:?}", backoff);

                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(backoff) => {}
                }

                let mut guard = llama.lock().await;

                // Shutdown may have fired while we slept or waited for the lock.
                if shutdown.is_cancelled() {
                    break;
                }

                if let Err(e) = guard.stop().await {
                    tracing::warn!("Error stopping llama-server before restart: {}", e);
                }
                match guard.start().await {
                    Ok(()) => presence
                        .transition(AgentPresenceStatus::Ready)
                        .await
                        .expect("Degraded → Ready transition must be valid"),
                    Err(e) => {
                        tracing::error!("Failed to restart llama-server: {}", e);
                        presence
                            .report_error("llama_restart_failed", &e.to_string())
                            .await;
                    }
                }
            } else {
                match guard.active_requests().await {
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
