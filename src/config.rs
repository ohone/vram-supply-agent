use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{bail, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub platform_url: String,
    pub public_url: String,
    pub model_dir: PathBuf,
    pub llama_server_path: String,
    pub gpu_layers: u32,
    pub port: u16,
    pub max_concurrent: u32,
    pub context_length_offered: u32,
    pub input_price_per_million: u32,
    pub output_price_per_million: u32,
}

/// Read an environment variable, returning `default` when the var is unset.
/// Fails with a clear message if the var is set but cannot be parsed.
fn env_or<T: FromStr>(var: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(var) {
        Ok(val) => val
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid value for {}: '{}' ({})", var, val, e)),
        Err(_) => Ok(default),
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let model_dir = match std::env::var("VRAM_SUPPLY_MODEL_DIR") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => {
                let home = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
                home.join(".vram-supply").join("models")
            }
        };

        let platform_url =
            env_or("VRAM_SUPPLY_PLATFORM_URL", "https://api.vram.supply".to_string())?;
        let port: u16 = env_or("VRAM_SUPPLY_PORT", 8080)?;
        let public_url = env_or(
            "VRAM_SUPPLY_PUBLIC_URL",
            format!("http://localhost:{}", port),
        )?;
        let llama_server_path =
            env_or("VRAM_SUPPLY_LLAMA_SERVER_PATH", "llama-server".to_string())?;
        let gpu_layers: u32 = env_or("VRAM_SUPPLY_GPU_LAYERS", 99)?;
        let max_concurrent: u32 = env_or("VRAM_SUPPLY_MAX_CONCURRENT", 1)?;
        let context_length_offered: u32 = env_or("VRAM_SUPPLY_CONTEXT_LENGTH", 8192)?;
        let input_price_per_million: u32 = env_or("VRAM_SUPPLY_INPUT_PRICE", 100)?;
        let output_price_per_million: u32 = env_or("VRAM_SUPPLY_OUTPUT_PRICE", 200)?;

        let config = Config {
            platform_url,
            public_url,
            model_dir,
            llama_server_path,
            gpu_layers,
            port,
            max_concurrent,
            context_length_offered,
            input_price_per_million,
            output_price_per_million,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.port == 0 {
            bail!("VRAM_SUPPLY_PORT must be > 0");
        }
        if self.max_concurrent == 0 {
            bail!("VRAM_SUPPLY_MAX_CONCURRENT must be > 0");
        }
        if self.context_length_offered == 0 {
            bail!("VRAM_SUPPLY_CONTEXT_LENGTH must be > 0");
        }
        if self.platform_url.is_empty() {
            bail!("VRAM_SUPPLY_PLATFORM_URL must not be empty");
        }
        if self.public_url.is_empty() {
            bail!("VRAM_SUPPLY_PUBLIC_URL must not be empty");
        }
        Ok(())
    }
}
