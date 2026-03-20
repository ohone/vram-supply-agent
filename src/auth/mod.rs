pub fn show_auth_status() {
    match std::env::var("VRAM_SUPPLY_API_KEY") {
        Ok(key) if !key.is_empty() => {
            let prefix = if key.len() > 4 { &key[..4] } else { "****" };
            println!("API key configured: {}...", prefix);
        }
        _ => {
            println!("No API key configured. Set VRAM_SUPPLY_API_KEY to authenticate.");
            println!("Create an API key at https://vram.supply/keys");
        }
    }
}
