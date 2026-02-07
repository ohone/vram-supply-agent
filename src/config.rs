use std::path::PathBuf;

use anyhow::Result;

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

        let platform_url = std::env::var("VRAM_SUPPLY_PLATFORM_URL")
            .unwrap_or_else(|_| "https://api.vram.supply".to_string());

        let public_url = std::env::var("VRAM_SUPPLY_PUBLIC_URL").unwrap_or_else(|_| {
            format!(
                "http://localhost:{}",
                std::env::var("VRAM_SUPPLY_PORT").unwrap_or_else(|_| "8080".to_string())
            )
        });

        let llama_server_path = std::env::var("VRAM_SUPPLY_LLAMA_SERVER_PATH")
            .unwrap_or_else(|_| "llama-server".to_string());

        let gpu_layers: u32 = std::env::var("VRAM_SUPPLY_GPU_LAYERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(99);

        let port: u16 = std::env::var("VRAM_SUPPLY_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);

        let max_concurrent: u32 = std::env::var("VRAM_SUPPLY_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);

        let context_length_offered: u32 = std::env::var("VRAM_SUPPLY_CONTEXT_LENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);

        let input_price_per_million: u32 = std::env::var("VRAM_SUPPLY_INPUT_PRICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let output_price_per_million: u32 = std::env::var("VRAM_SUPPLY_OUTPUT_PRICE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);

        Ok(Config {
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
        })
    }
}
