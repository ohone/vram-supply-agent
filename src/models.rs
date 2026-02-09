use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

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

/// Download a GGUF model file from a HuggingFace repository.
pub async fn pull_model(hf_repo_id: &str, file: Option<&str>) -> Result<()> {
    let model_dir = crate::config::model_dir()?;
    fs::create_dir_all(&model_dir)
        .with_context(|| format!("Failed to create model directory {}", model_dir.display()))?;

    // Fetch repo tree and filter to .gguf files
    let entries = crate::verification::fetch_hf_tree(hf_repo_id).await?;
    let gguf_entries: Vec<_> = entries
        .into_iter()
        .filter(|e| e.path.ends_with(".gguf"))
        .collect();

    if gguf_entries.is_empty() {
        anyhow::bail!("No .gguf files found in HuggingFace repository '{}'", hf_repo_id);
    }

    // Select which file to download
    let entry = if let Some(name) = file {
        gguf_entries
            .into_iter()
            .find(|e| e.path == name)
            .ok_or_else(|| anyhow::anyhow!("File '{}' not found in repository '{}'", name, hf_repo_id))?
    } else if gguf_entries.len() == 1 {
        gguf_entries.into_iter().next().unwrap()
    } else {
        println!("Multiple .gguf files found in '{}':", hf_repo_id);
        for e in &gguf_entries {
            println!("  {} ({})", e.path, format_size(e.size));
        }
        anyhow::bail!("Use --file <filename> to select one");
    };

    let dest = model_dir.join(&entry.path);
    let expected_size = entry.size;

    // Check if file already exists with correct size
    if dest.exists() {
        let existing_size = fs::metadata(&dest)
            .with_context(|| format!("Failed to read metadata for {}", dest.display()))?
            .len();
        if existing_size == expected_size {
            println!("{} already exists with correct size, skipping download", dest.display());
            return Ok(());
        }
    }

    let lfs = entry.lfs.as_ref();
    let expected_sha = lfs.map(|l| {
        l.oid
            .strip_prefix("sha256:")
            .unwrap_or(&l.oid)
            .to_string()
    });

    // Download
    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        hf_repo_id, entry.path
    );
    let partial = dest.with_extension("gguf.partial");

    println!("Downloading {} ({})", entry.path, format_size(expected_size));

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "vramsply")
        .send()
        .await
        .with_context(|| format!("Failed to start download from {}", url))?;

    if !resp.status().is_success() {
        anyhow::bail!("Download failed: HTTP {} from {}", resp.status(), url);
    }

    let download_result = async {
        let mut file = fs::File::create(&partial)
            .with_context(|| format!("Failed to create {}", partial.display()))?;

        let mut downloaded: u64 = 0;
        let mut response = resp;

        while let Some(chunk) = response
            .chunk()
            .await
            .context("Failed to read download chunk")?
        {
            file.write_all(&chunk)
                .with_context(|| format!("Failed to write to {}", partial.display()))?;
            downloaded += chunk.len() as u64;
            eprint!(
                "\r  {}/{} ({:.0}%)",
                format_size(downloaded),
                format_size(expected_size),
                downloaded as f64 / expected_size as f64 * 100.0
            );
        }
        eprintln!();

        // Verify size
        if downloaded != expected_size {
            anyhow::bail!(
                "Size mismatch: expected {} bytes, got {} bytes",
                expected_size,
                downloaded
            );
        }

        // Verify SHA-256 if LFS metadata available
        if let Some(expected) = &expected_sha {
            eprint!("Verifying SHA-256...");
            let actual = crate::verification::compute_sha256(partial.to_str().ok_or_else(|| {
                anyhow::anyhow!("Partial path is not valid UTF-8")
            })?)?;
            if actual != *expected {
                anyhow::bail!(
                    "SHA-256 mismatch: expected {}, got {}",
                    expected,
                    actual
                );
            }
            eprintln!(" ok");
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(e) = download_result {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }

    // Rename .partial → final
    fs::rename(&partial, &dest)
        .with_context(|| format!("Failed to rename {} → {}", partial.display(), dest.display()))?;

    println!("Saved to {}", dest.display());
    Ok(())
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

/// Extract the GGUF filename from a model path.
pub fn gguf_filename(model_path: &str) -> Result<String> {
    Path::new(model_path)
        .file_name()
        .and_then(|f| f.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Could not extract filename from path: {}", model_path))
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
