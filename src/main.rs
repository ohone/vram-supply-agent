mod auth;
mod backend;
mod config;
mod identity;
mod models;
mod presence;

use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use presence::{AgentPresenceState, AgentPresenceStatus};

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
    /// Authentication commands
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Start providing model inference
    Serve {
        /// Path to the model file to serve
        #[arg(long)]
        model: Option<String>,

        /// Override the model name sent to the platform (e.g., "meta-llama/llama-3.1-8b-instruct")
        #[arg(long)]
        model_name: Option<String>,

        /// Use device code flow instead of browser-based login
        #[arg(long)]
        headless: bool,
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
enum AuthCommands {
    /// Authenticate with the platform
    Login {
        /// Use device code flow instead of browser-based login
        #[arg(long)]
        headless: bool,
    },
    /// Show current authentication status
    Status,
    /// Clear stored credentials
    Logout,
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
        Commands::Auth { command } => match command {
            AuthCommands::Login { headless } => {
                if headless {
                    auth::login_device_code(&config).await?;
                } else {
                    auth::login_pkce(&config).await?;
                }
            }
            AuthCommands::Status => {
                auth::show_auth_status()?;
            }
            AuthCommands::Logout => {
                auth::credentials::clear_credentials()?;
                println!("Logged out successfully.");
            }
        },

        Commands::Serve {
            model,
            model_name,
            headless,
        } => {
            run_serve(&config, model, model_name, headless).await?;
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
                models::pull_model(&hf_repo_id);
            }
        },

        Commands::Benchmark { model_path } => {
            println!("Benchmarking model: {}", model_path);
            println!("Benchmark not yet implemented");
        }

        Commands::Status => {
            println!("Agent status:");
            auth::show_auth_status()?;

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
    headless: bool,
) -> Result<()> {
    // Authenticate (loads existing credentials or triggers login)
    let creds = auth::ensure_authenticated(config, headless).await?;
    let token = Arc::new(tokio::sync::Mutex::new(creds.access_token));
    let identity = identity::load_or_create_identity()?;
    let client = reqwest::Client::new();

    // 1. Determine which model to serve
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

    let model_name = match model_name_override {
        Some(name) => name,
        None => models::normalize_model_name(&model_path),
    };

    // Create an in-memory presence state and publish the initial idle state.
    let presence_state = Arc::new(tokio::sync::Mutex::new(AgentPresenceState::new(
        AgentPresenceStatus::Idle,
        Some(model_name.clone()),
    )));
    send_presence_snapshot(
        &client,
        config,
        &token,
        &identity,
        Arc::clone(&presence_state),
    )
    .await;
    let presence_handle = presence::spawn_presence_loop(
        client.clone(),
        config.clone(),
        Arc::clone(&token),
        identity.clone(),
        Arc::clone(&presence_state),
    );

    {
        let mut state = presence_state.lock().await;
        state.status = AgentPresenceStatus::LoadingModel;
        state.loading_progress_pct = None;
        state.active_requests = 0;
        state.error_code = None;
        state.error_message = None;
    }
    send_presence_snapshot(
        &client,
        config,
        &token,
        &identity,
        Arc::clone(&presence_state),
    )
    .await;

    // 2. Start llama-server
    let mut llama = backend::LlamaServer::new(
        model_path.clone(),
        config.port,
        config.llama_server_path.clone(),
        config.gpu_layers,
    );
    if let Err(e) = llama.start().await {
        {
            let mut state = presence_state.lock().await;
            state.status = AgentPresenceStatus::Error;
            state.error_code = Some("llama_start_failed".to_string());
            state.error_message = Some(e.to_string());
        }
        send_presence_snapshot(
            &client,
            config,
            &token,
            &identity,
            Arc::clone(&presence_state),
        )
        .await;
        presence_handle.abort();
        return Err(e);
    }
    tracing::info!("llama-server is healthy on port {}", config.port);

    // 3. Register with platform via HTTP
    let register_url = format!("{}/v1/providers/register", config.platform_url);

    let register_body = RegisterRequest {
        endpoint_url: config.public_url.clone(),
        model: model_name.clone(),
        max_concurrent: config.max_concurrent,
        context_length_offered: config.context_length_offered,
        input_price_per_million: config.input_price_per_million,
        output_price_per_million: config.output_price_per_million,
    };

    let reg_token = token.lock().await.clone();
    let res = match client
        .post(&register_url)
        .header("Authorization", format!("Bearer {}", reg_token))
        .json(&register_body)
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            {
                let mut state = presence_state.lock().await;
                state.status = AgentPresenceStatus::Error;
                state.error_code = Some("provider_register_request_failed".to_string());
                state.error_message = Some(e.to_string());
            }
            send_presence_snapshot(
                &client,
                config,
                &token,
                &identity,
                Arc::clone(&presence_state),
            )
            .await;
            presence_handle.abort();
            return Err(e.into());
        }
    };

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        {
            let mut state = presence_state.lock().await;
            state.status = AgentPresenceStatus::Error;
            state.error_code = Some("provider_register_failed".to_string());
            state.error_message = Some(format!("status {}: {}", status, body));
        }
        send_presence_snapshot(
            &client,
            config,
            &token,
            &identity,
            Arc::clone(&presence_state),
        )
        .await;
        presence_handle.abort();
        anyhow::bail!("Registration failed ({}): {}", status, body);
    }

    let reg: RegisterResponse = match res.json().await {
        Ok(reg) => reg,
        Err(e) => {
            {
                let mut state = presence_state.lock().await;
                state.status = AgentPresenceStatus::Error;
                state.error_code = Some("provider_register_response_invalid".to_string());
                state.error_message = Some(e.to_string());
            }
            send_presence_snapshot(
                &client,
                config,
                &token,
                &identity,
                Arc::clone(&presence_state),
            )
            .await;
            presence_handle.abort();
            return Err(e.into());
        }
    };
    tracing::info!(
        "Registered with platform: id={}, status={}",
        reg.id,
        reg.status
    );

    {
        let mut state = presence_state.lock().await;
        state.status = AgentPresenceStatus::Ready;
        state.loading_progress_pct = None;
        state.active_requests = 0;
        state.error_code = None;
        state.error_message = None;
    }
    send_presence_snapshot(
        &client,
        config,
        &token,
        &identity,
        Arc::clone(&presence_state),
    )
    .await;
    println!("vram.supply provider runtime is running. Press Ctrl+C to stop.");
    println!("  Model: {}", model_name);
    println!("  Endpoint: {}", config.public_url);
    println!("  Instance ID: {}", reg.id);

    // 4. Heartbeat loop with token refresh
    let heartbeat_url = format!("{}/v1/providers/heartbeat", config.platform_url);
    let deregister_url = format!("{}/v1/providers/{}", config.platform_url, reg.id);

    let heartbeat_client = client.clone();
    let heartbeat_token = Arc::clone(&token);
    let heartbeat_config = config.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;

            // Refresh token if expiring soon
            match auth::load_valid_credentials(&heartbeat_config).await {
                Ok(creds) => {
                    let mut t = heartbeat_token.lock().await;
                    *t = creds.access_token;
                }
                Err(e) => {
                    tracing::warn!("Failed to refresh credentials: {}", e);
                }
            }

            let current_token = heartbeat_token.lock().await.clone();
            let res = heartbeat_client
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
    });

    // 5. Health monitor for llama-server
    let monitor_presence_state = Arc::clone(&presence_state);
    let monitor_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            interval.tick().await;
            if !llama.is_running() {
                tracing::warn!("llama-server has stopped, attempting restart...");
                {
                    let mut state = monitor_presence_state.lock().await;
                    state.status = AgentPresenceStatus::Degraded;
                    state.active_requests = 0;
                    state.error_code = Some("llama_stopped".to_string());
                    state.error_message =
                        Some("llama-server process stopped unexpectedly".to_string());
                }
                if let Err(e) = llama.restart_with_backoff().await {
                    tracing::error!("Failed to restart llama-server: {}", e);
                    let mut state = monitor_presence_state.lock().await;
                    state.status = AgentPresenceStatus::Error;
                    state.active_requests = 0;
                    state.error_code = Some("llama_restart_failed".to_string());
                    state.error_message = Some(e.to_string());
                    continue;
                }
                let mut state = monitor_presence_state.lock().await;
                state.status = AgentPresenceStatus::Ready;
                state.active_requests = 0;
                state.error_code = None;
                state.error_message = None;
            } else {
                match llama.active_requests().await {
                    Ok(active) => {
                        let mut state = monitor_presence_state.lock().await;
                        state.active_requests = active;
                        if active > 0 {
                            state.status = AgentPresenceStatus::Serving;
                        } else if matches!(
                            state.status,
                            AgentPresenceStatus::Ready
                                | AgentPresenceStatus::Serving
                                | AgentPresenceStatus::Idle
                                | AgentPresenceStatus::LoadingModel
                        ) {
                            state.status = AgentPresenceStatus::Ready;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Failed to inspect active request count: {}", e);
                    }
                }
            }
        }
    });

    // Wait for shutdown signal
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    tracing::info!("Shutting down...");
    println!("\nShutting down...");

    {
        let mut state = presence_state.lock().await;
        state.status = AgentPresenceStatus::Unavailable;
        state.active_requests = 0;
        state.loading_progress_pct = None;
        state.error_code = None;
        state.error_message = None;
    }
    send_presence_snapshot(
        &client,
        config,
        &token,
        &identity,
        Arc::clone(&presence_state),
    )
    .await;

    // Deregister
    let current_token = token.lock().await.clone();
    let _ = client
        .delete(&deregister_url)
        .header("Authorization", format!("Bearer {}", current_token))
        .send()
        .await;
    tracing::info!("Deregistered from platform");

    // Abort tasks
    heartbeat_handle.abort();
    monitor_handle.abort();
    presence_handle.abort();

    Ok(())
}

async fn send_presence_snapshot(
    client: &reqwest::Client,
    config: &config::Config,
    token: &Arc<tokio::sync::Mutex<String>>,
    identity: &identity::AgentIdentity,
    state: Arc<tokio::sync::Mutex<AgentPresenceState>>,
) {
    let current_token = token.lock().await.clone();
    let snapshot = state.lock().await.clone();
    if let Err(e) =
        presence::send_presence_once(client, config, &current_token, identity, &snapshot).await
    {
        tracing::warn!("Presence update failed: {}", e);
    }
}
