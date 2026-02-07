use std::path::Path;

use anyhow::Result;

use crate::config::Config;

/// A locally available model.
#[derive(Debug)]
pub struct LocalModel {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// List all .gguf model files in the configured model directory.
pub fn list_local_models(config: &Config) -> Result<Vec<LocalModel>> {
    let model_dir = &config.model_dir;

    if !model_dir.exists() {
        tracing::info!("Model directory does not exist: {}", model_dir.display());
        return Ok(Vec::new());
    }

    let mut models = Vec::new();

    let entries = std::fs::read_dir(model_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to read model directory {}: {}",
            model_dir.display(),
            e
        )
    })?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
            let metadata = std::fs::metadata(&path)?;
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            models.push(LocalModel {
                name,
                path: path.to_string_lossy().to_string(),
                size_bytes: metadata.len(),
            });
        }
    }

    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

/// Pull a model from HuggingFace (not yet implemented).
pub fn pull_model(_hf_repo_id: &str) {
    println!("Model download not yet implemented");
}

/// Format bytes into a human-readable size string.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Derive a HuggingFace model ID from a GGUF file path.
///
/// Strips quantization suffixes (e.g., `.Q4_K_M`, `-q4_k_m`) and maps
/// known filename prefixes to their HuggingFace org/repo names.
pub fn normalize_model_name(model_path: &str) -> String {
    let stem = Path::new(model_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let stripped = strip_quantization_suffix(stem);

    match map_to_hf_repo(stripped) {
        Some(hf_id) => hf_id,
        None => stripped.to_string(),
    }
}

fn strip_quantization_suffix(name: &str) -> &str {
    for sep in ['.', '-'] {
        if let Some(pos) = name.rfind(sep) {
            let suffix = &name[pos + 1..];
            let upper = suffix.to_uppercase();
            if is_quantization_tag(&upper) {
                return strip_quantization_suffix(&name[..pos]);
            }
        }
    }
    name
}

fn is_quantization_tag(tag: &str) -> bool {
    if tag.starts_with('Q') || tag.starts_with("IQ") {
        return tag.len() >= 3;
    }
    matches!(tag, "F16" | "F32" | "BF16")
}

fn map_to_hf_repo(name: &str) -> Option<String> {
    let lower = name.to_lowercase();

    let mappings: &[(&str, &str)] = &[
        ("llama-3.1", "meta-llama"),
        ("llama-3.2", "meta-llama"),
        ("llama-3.3", "meta-llama"),
        ("llama-3", "meta-llama"),
        ("llama-2", "meta-llama"),
        ("mistral", "mistralai"),
        ("mixtral", "mistralai"),
        ("codestral", "mistralai"),
        ("qwen2.5", "qwen"),
        ("qwen2", "qwen"),
        ("gemma-2", "google"),
        ("gemma", "google"),
        ("phi-3", "microsoft"),
        ("phi-4", "microsoft"),
        ("deepseek-r1", "deepseek-ai"),
        ("deepseek-v3", "deepseek-ai"),
        ("deepseek-v2", "deepseek-ai"),
    ];

    for (prefix, org) in mappings {
        if lower.starts_with(prefix) {
            return Some(format!("{}/{}", org, lower));
        }
    }

    None
}

/// Find a model file by name or path. If the input is an absolute path that
/// exists, return it directly. Otherwise search the model directory.
pub fn find_model(config: &Config, name_or_path: &str) -> Result<String> {
    let as_path = Path::new(name_or_path);
    if as_path.is_absolute() && as_path.exists() {
        return Ok(name_or_path.to_string());
    }

    // Search model directory
    let model_dir = &config.model_dir;
    let candidates = [
        model_dir.join(name_or_path),
        model_dir.join(format!("{}.gguf", name_or_path)),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.to_string_lossy().to_string());
        }
    }

    anyhow::bail!(
        "Model '{}' not found. Checked: {}",
        name_or_path,
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}
