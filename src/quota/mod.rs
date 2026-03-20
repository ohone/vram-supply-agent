mod codex;
mod openai;
mod prereqs;
mod relay;
mod sandbox;
mod session;

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::Config;

pub use prereqs::{check_prerequisites, print_prereq_status};

pub async fn handle_connect(client: &reqwest::Client, provider: &str) -> Result<()> {
    match provider {
        "openai" | "openai-codex" => openai::connect_openai(client).await,
        other => anyhow::bail!(
            "Unsupported provider '{}'. Try: vramsupply connect openai",
            other
        ),
    }
}

/// Handle the sell-quota status command
pub async fn handle_sell_quota_status(_config: &Config, provider: &str) -> Result<()> {
    let status = check_prerequisites(provider).await;
    print_prereq_status(&status);

    if !prereqs::all_ok(&status) {
        std::process::exit(1);
    }

    info!("All prerequisites satisfied for sell-quota mode");
    Ok(())
}

/// Handle the sell-quota command
pub async fn handle_sell_quota(
    client: &reqwest::Client,
    config: &Config,
    provider: &str,
    max_concurrent: u32,
    max_budget_usd: f64,
    model: &str,
) -> Result<()> {
    let prereq_status = check_prerequisites(provider).await;
    print_prereq_status(&prereq_status);

    if !prereqs::all_ok(&prereq_status) {
        anyhow::bail!("Prerequisites not met. Run with --status for more details.");
    }

    info!("Starting vram.supply agent in sell-quota mode");
    info!(
        "Configuration: provider={}, max_concurrent={}, max_budget_usd={}, model={}",
        provider, max_concurrent, max_budget_usd, model
    );

    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for shutdown signal");
        info!("Received shutdown signal");
        shutdown_clone.cancel();
    });

    relay::run_relay(
        client,
        config,
        provider,
        max_concurrent,
        model,
        max_budget_usd,
        shutdown,
    )
    .await?;

    info!("Sell-quota mode shutdown complete");
    Ok(())
}
