use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::Config;

/// A locally available model.
#[derive(Debug)]
pub struct LocalModel {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// List all .gguf model files in the configured model directory (recursive).
pub fn list_local_models(config: &Config) -> Result<Vec<LocalModel>> {
    let model_dir = &config.model_dir;

    if !model_dir.exists() {
        tracing::info!("Model directory does not exist: {}", model_dir.display());
        return Ok(Vec::new());
    }

    let mut models = Vec::new();
    collect_gguf_files(model_dir, &mut models)?;
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

/// Recursively collect .gguf files from a directory.
fn collect_gguf_files(dir: &Path, models: &mut Vec<LocalModel>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("Failed to read directory {}: {}", dir.display(), e))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_gguf_files(&path, models)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
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
    Ok(())
}

/// Download a single file from a HuggingFace repository to a local path.
///
/// Handles partial downloads, size verification, SHA-256 verification (when
/// LFS metadata is available), and cleanup on failure.
pub async fn download_hf_file(
    hf_repo_id: &str,
    entry: &crate::verification::HfFileEntry,
    dest_path: &Path,
) -> Result<()> {
    let expected_size = entry.size;

    // Check if file already exists with correct size
    if dest_path.exists() {
        let existing_size = fs::metadata(dest_path)
            .with_context(|| format!("Failed to read metadata for {}", dest_path.display()))?
            .len();
        if existing_size == expected_size {
            println!(
                "{} already exists with correct size, skipping download",
                dest_path.display()
            );
            return Ok(());
        }
    }

    // Ensure parent directory exists
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }

    let lfs = entry.lfs.as_ref();
    let expected_sha = lfs.map(|l| l.oid.strip_prefix("sha256:").unwrap_or(&l.oid).to_string());

    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        hf_repo_id, entry.path
    );
    let partial = dest_path.with_extension("gguf.partial");

    println!(
        "Downloading {} ({})",
        entry.path,
        format_size(expected_size)
    );

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

        if downloaded != expected_size {
            anyhow::bail!(
                "Size mismatch: expected {} bytes, got {} bytes",
                expected_size,
                downloaded
            );
        }

        if let Some(expected) = &expected_sha {
            eprint!("Verifying SHA-256...");
            let actual = crate::verification::compute_sha256(
                partial
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Partial path is not valid UTF-8"))?,
            )?;
            if actual != *expected {
                anyhow::bail!("SHA-256 mismatch: expected {}, got {}", expected, actual);
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

    fs::rename(&partial, dest_path).with_context(|| {
        format!(
            "Failed to rename {} → {}",
            partial.display(),
            dest_path.display()
        )
    })?;

    println!("Saved to {}", dest_path.display());
    Ok(())
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
        anyhow::bail!(
            "No .gguf files found in HuggingFace repository '{}'",
            hf_repo_id
        );
    }

    // Select which file to download
    let entry = if let Some(name) = file {
        gguf_entries
            .into_iter()
            .find(|e| e.path == name)
            .ok_or_else(|| {
                anyhow::anyhow!("File '{}' not found in repository '{}'", name, hf_repo_id)
            })?
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
    download_hf_file(hf_repo_id, &entry, &dest).await
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
        ("qwen3.5", "qwen"),
        ("qwen3", "qwen"),
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
#[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Canonical model ID resolution (F68)
// ---------------------------------------------------------------------------

/// Result of resolving a model argument to a local GGUF file.
pub struct ResolvedModel {
    /// Local filesystem path to the GGUF file.
    pub path: String,
    /// Canonical model name for platform registration.
    pub canonical_name: String,
    /// GGUF artifact repo used for verification (if resolved from a canonical ID).
    pub gguf_repo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HfSearchResult {
    id: String,
    #[serde(default)]
    downloads: u64,
}

/// Blocklist terms for derivative repos — rejected when NOT present in the
/// canonical model name itself.
const DERIVATIVE_TERMS: &[&str] = &[
    "distill",
    "distil",
    "uncensored",
    "abliterated",
    "merge",
    "roleplay",
    "heretic",
    "franken",
];

/// Normalize a name for comparison: lowercase, strip trailing `-gguf`,
/// replace `.` and `_` with `-`.
fn normalize_for_comparison(name: &str) -> String {
    let mut s = name.to_lowercase();
    // Strip trailing -gguf
    if let Some(stripped) = s.strip_suffix("-gguf") {
        s = stripped.to_string();
    }
    // Replace . and _ with -
    s = s.replace(['.', '_'], "-");
    s
}

/// Check if a repo name looks like a derivative/finetuned model.
fn is_derivative_repo(repo_name: &str, canonical_name: &str) -> bool {
    let repo_lower = repo_name.to_lowercase();
    let canonical_lower = canonical_name.to_lowercase();
    for term in DERIVATIVE_TERMS {
        if repo_lower.contains(term) && !canonical_lower.contains(term) {
            return true;
        }
    }
    false
}

/// Extract a quant tag from a GGUF filename (e.g., "Q4_K_M" from
/// "Llama-3.2-3B-Instruct-Q4_K_M.gguf").
pub fn extract_quant_tag(filename: &str) -> Option<String> {
    // Strip from first .gguf occurrence (handles both single and multipart)
    let stem = match filename.find(".gguf") {
        Some(pos) => &filename[..pos],
        None => filename,
    };

    // Split the stem into segments by both `-` and `.`, then check each.
    // We use a unified split to avoid separator-order bias.
    let segments: Vec<&str> = stem.split(['-', '.']).collect();
    // Walk from the end — quant tags are typically the last meaningful segment.
    for segment in segments.iter().rev() {
        let upper = segment.to_uppercase();
        // Require underscore in tag (e.g., Q4_K_M, IQ4_XS) or exact known tags
        // to avoid false positives on model name segments like "Qwen3".
        if upper.contains('_') && is_quantization_tag(&upper) {
            return Some(upper);
        }
        // Exact short tags without underscores (Q8, F16, etc.)
        if matches!(upper.as_str(), "F16" | "F32" | "BF16") {
            return Some(upper);
        }
        // Simple quant tags like Q8_0 already contain underscore.
        // Also match tags like IQ3M → but those also have underscore in canonical form.
    }
    None
}

/// Check if a GGUF filename is a multipart shard.
fn is_multipart_file(filename: &str) -> bool {
    // Pattern: *.gguf-00001-of-00006.gguf or similar
    filename.contains("-of-") && filename.contains(".gguf")
}

/// Fetch the file tree for a HuggingFace repo (thin wrapper for readability).
async fn fetch_gguf_tree(repo_id: &str) -> Result<Vec<crate::verification::HfFileEntry>> {
    crate::verification::fetch_hf_tree(repo_id).await
}

/// Search HuggingFace for the best GGUF repo matching a canonical model ID.
///
/// Checks the canonical repo first, then searches for community GGUF repos.
/// Ranks by normalized name match (exact first), rejects derivatives, and
/// uses downloads as a tiebreaker.
pub async fn search_gguf_repo(canonical_id: &str) -> Result<String> {
    let (_org, name) = canonical_id
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Invalid model ID '{}': expected org/name", canonical_id))?;

    // 1. Check canonical repo tree for GGUFs
    match crate::verification::fetch_hf_tree(canonical_id).await {
        Ok(entries) => {
            let has_gguf = entries.iter().any(|e| e.path.ends_with(".gguf"));
            if has_gguf {
                tracing::info!("Canonical repo '{}' contains GGUFs directly", canonical_id);
                return Ok(canonical_id.to_string());
            }
        }
        Err(e) => {
            tracing::debug!("Could not check canonical repo '{}': {}", canonical_id, e);
        }
    }

    // 2. Search HuggingFace for GGUF repos
    let canonical_norm = normalize_for_comparison(name);
    let client = reqwest::Client::new();

    let search_terms = [format!("{}-GGUF", name), name.to_string()];

    let mut all_candidates: Vec<HfSearchResult> = Vec::new();
    for term in &search_terms {
        let url = format!(
            "https://huggingface.co/api/models?search={}&filter=gguf&sort=downloads&direction=-1&limit=10",
            urlencoding::encode(term)
        );
        match client
            .get(&url)
            .header("User-Agent", "vramsply")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(results) = resp.json::<Vec<HfSearchResult>>().await {
                    for r in results {
                        if !all_candidates.iter().any(|c| c.id == r.id) {
                            all_candidates.push(r);
                        }
                    }
                }
            }
            Ok(resp) => {
                tracing::debug!("HuggingFace search for '{}' returned HTTP {}", term, resp.status());
            }
            Err(e) => {
                tracing::debug!("HuggingFace search for '{}' failed: {}", term, e);
            }
        }
        // If we already have exact matches after first search, skip fallback
        if all_candidates.iter().any(|c| {
            let repo_name = c.id.split('/').nth(1).unwrap_or(&c.id);
            normalize_for_comparison(repo_name) == canonical_norm
        }) {
            break;
        }
    }

    if all_candidates.is_empty() {
        anyhow::bail!(
            "No GGUF repository found for '{}'. \
             Use --model with a local .gguf file path instead.",
            canonical_id
        );
    }

    // 3. Rank candidates
    // Filter out derivatives
    all_candidates.retain(|c| {
        let repo_name = c.id.split('/').nth(1).unwrap_or(&c.id);
        !is_derivative_repo(repo_name, name)
    });

    if all_candidates.is_empty() {
        anyhow::bail!(
            "No canonical GGUF repository found for '{}' (all candidates were derivative repos). \
             Use --model with a local .gguf file path instead.",
            canonical_id
        );
    }

    // Sort: exact normalized match first, then by downloads
    all_candidates.sort_by(|a, b| {
        let a_name = a.id.split('/').nth(1).unwrap_or(&a.id);
        let b_name = b.id.split('/').nth(1).unwrap_or(&b.id);
        let a_norm = normalize_for_comparison(a_name);
        let b_norm = normalize_for_comparison(b_name);
        let a_exact = a_norm == canonical_norm;
        let b_exact = b_norm == canonical_norm;
        // Exact matches first
        b_exact
            .cmp(&a_exact)
            .then_with(|| {
                // Then prefix matches
                let a_prefix = a_norm.starts_with(&canonical_norm);
                let b_prefix = b_norm.starts_with(&canonical_norm);
                b_prefix.cmp(&a_prefix)
            })
            .then_with(|| {
                // Then by downloads
                b.downloads.cmp(&a.downloads)
            })
    });

    let best = &all_candidates[0];
    tracing::info!(
        "Resolved '{}' → '{}' ({} downloads)",
        canonical_id,
        best.id,
        best.downloads
    );
    Ok(best.id.clone())
}

/// Find the GGUF filename for a specific quant in a HuggingFace repo.
///
/// Returns the filename (not a full path) of the matching GGUF file.
/// Errors if the quant resolves to a multipart artifact or is not found.
#[allow(dead_code)]
pub async fn find_quant_file(gguf_repo: &str, quant: &str) -> Result<String> {
    let entries = fetch_gguf_tree(gguf_repo).await?;
    find_quant_file_in_entries(&entries, gguf_repo, quant)
}

/// Core quant-file matching logic operating on already-fetched entries.
fn find_quant_file_in_entries(
    entries: &[crate::verification::HfFileEntry],
    gguf_repo: &str,
    quant: &str,
) -> Result<String> {
    let gguf_files: Vec<_> = entries
        .iter()
        .filter(|e| e.path.ends_with(".gguf") || e.path.contains(".gguf-"))
        .collect();

    if gguf_files.is_empty() {
        anyhow::bail!("No GGUF files found in repository '{}'", gguf_repo);
    }

    let quant_upper = quant.to_uppercase();

    // Find files matching the requested quant
    let matching: Vec<_> = gguf_files
        .iter()
        .filter(|e| {
            extract_quant_tag(&e.path)
                .map(|t| t == quant_upper)
                .unwrap_or(false)
        })
        .collect();

    if matching.is_empty() {
        // Collect available quants for the error message
        let mut available: Vec<String> = gguf_files
            .iter()
            .filter_map(|e| extract_quant_tag(&e.path))
            .collect();
        available.sort();
        available.dedup();

        anyhow::bail!(
            "Quant '{}' not found in '{}'. Available quants: {}",
            quant,
            gguf_repo,
            if available.is_empty() {
                "(none detected)".to_string()
            } else {
                available.join(", ")
            }
        );
    }

    // Check for multipart
    if matching.iter().any(|e| is_multipart_file(&e.path)) || matching.len() > 1 {
        // Multiple files for the same quant → likely sharded
        if matching.iter().any(|e| is_multipart_file(&e.path)) {
            anyhow::bail!(
                "Quant '{}' in '{}' is a multipart GGUF (split across {} files). \
                 Multipart GGUFs are not supported yet. \
                 Use a smaller quant or download manually and pass a local path with --model.",
                quant,
                gguf_repo,
                matching.len()
            );
        }
    }

    Ok(matching[0].path.clone())
}

/// Resolve a model argument to a local GGUF file.
///
/// Accepts local paths, bare filenames, or canonical HuggingFace model IDs.
/// For canonical IDs, resolves the GGUF repo via HuggingFace search, finds
/// the quant-specific file, and downloads it if needed.
pub async fn resolve_model(
    config: &Config,
    name_or_path: &str,
    quant: Option<&str>,
) -> Result<ResolvedModel> {
    let as_path = Path::new(name_or_path);

    // 1. Absolute path that exists → serve directly
    if as_path.is_absolute() && as_path.exists() {
        return Ok(ResolvedModel {
            path: name_or_path.to_string(),
            canonical_name: normalize_model_name(name_or_path),
            gguf_repo: None,
        });
    }

    // 2. Contains '/' → treat as canonical model ID (if not an existing path)
    if name_or_path.contains('/') {
        // Check if it happens to be a relative path that exists
        if as_path.exists() {
            return Ok(ResolvedModel {
                path: name_or_path.to_string(),
                canonical_name: normalize_model_name(name_or_path),
                gguf_repo: None,
            });
        }

        // Canonical model ID flow
        let quant = quant.ok_or_else(|| {
            anyhow::anyhow!(
                "--quant is required when serving by model ID.\n\
                 Example: vramsply serve --model \"{}\" --quant Q4_K_M",
                name_or_path
            )
        })?;

        // Check if already downloaded locally
        let local_dir = config.model_dir.join(name_or_path);
        if local_dir.exists() {
            // Look for a file matching this quant
            if let Ok(entries) = std::fs::read_dir(&local_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(tag) =
                        extract_quant_tag(path.file_name().and_then(|f| f.to_str()).unwrap_or(""))
                    {
                        if tag == quant.to_uppercase() {
                            tracing::info!("Found locally cached model: {}", path.display());
                            return Ok(ResolvedModel {
                                path: path.to_string_lossy().to_string(),
                                canonical_name: name_or_path.to_string(),
                                gguf_repo: None,
                            });
                        }
                    }
                }
            }
        }

        // Resolve GGUF repo and file
        let gguf_repo = search_gguf_repo(name_or_path).await?;

        // Fetch tree once and use it for both quant matching and download
        let entries = fetch_gguf_tree(&gguf_repo).await?;
        let gguf_file = find_quant_file_in_entries(&entries, &gguf_repo, quant)?;
        let entry = entries
            .iter()
            .find(|e| e.path == gguf_file)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "File '{}' disappeared from repo '{}' during resolution",
                    gguf_file,
                    gguf_repo
                )
            })?;

        let dest = config.model_dir.join(name_or_path).join(&gguf_file);
        download_hf_file(&gguf_repo, entry, &dest).await?;

        return Ok(ResolvedModel {
            path: dest.to_string_lossy().to_string(),
            canonical_name: name_or_path.to_string(),
            gguf_repo: Some(gguf_repo),
        });
    }

    // 3. Bare name → search model_dir (existing behavior)
    let model_dir = &config.model_dir;
    let candidates = [
        model_dir.join(name_or_path),
        model_dir.join(format!("{}.gguf", name_or_path)),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(ResolvedModel {
                path: candidate.to_string_lossy().to_string(),
                canonical_name: normalize_model_name(&candidate.to_string_lossy()),
                gguf_repo: None,
            });
        }
    }

    anyhow::bail!(
        "Model '{}' not found. Checked: {}\n\
         Tip: use a canonical HuggingFace model ID (e.g., qwen/qwen3.5-9b) \
         with --quant to auto-download.",
        name_or_path,
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_for_comparison() {
        assert_eq!(
            normalize_for_comparison("Llama-3.2-3B-Instruct-GGUF"),
            "llama-3-2-3b-instruct"
        );
        assert_eq!(normalize_for_comparison("qwen3.5-9b"), "qwen3-5-9b");
        assert_eq!(
            normalize_for_comparison("Qwen_Qwen3.5-9B-GGUF"),
            "qwen-qwen3-5-9b"
        );
        assert_eq!(
            normalize_for_comparison("llama-3.2-3b-instruct"),
            "llama-3-2-3b-instruct"
        );
        // Case insensitive
        assert_eq!(
            normalize_for_comparison("LLAMA-3.2-3B-INSTRUCT-GGUF"),
            "llama-3-2-3b-instruct"
        );
    }

    #[test]
    fn test_is_derivative_repo() {
        assert!(is_derivative_repo(
            "Llama-3.2-3B-Instruct-uncensored-GGUF",
            "llama-3.2-3b-instruct"
        ));
        assert!(is_derivative_repo(
            "gpt-4o-distil-Llama-3.3-70B-Instruct-GGUF",
            "llama-3.3-70b-instruct"
        ));
        assert!(is_derivative_repo(
            "Qwen3.5-9B-Claude-4.6-Opus-Reasoning-Distilled-GGUF",
            "qwen3.5-9b"
        ));
        assert!(is_derivative_repo("Some-Model-Heretic-GGUF", "some-model"));
        // Not derivative
        assert!(!is_derivative_repo(
            "Llama-3.2-3B-Instruct-GGUF",
            "llama-3.2-3b-instruct"
        ));
        assert!(!is_derivative_repo("Qwen3.5-9B-GGUF", "qwen3.5-9b"));
    }

    #[test]
    fn test_extract_quant_tag() {
        assert_eq!(
            extract_quant_tag("Llama-3.2-3B-Instruct-Q4_K_M.gguf"),
            Some("Q4_K_M".to_string())
        );
        assert_eq!(
            extract_quant_tag("Qwen3-14B.Q5_K_M.gguf"),
            Some("Q5_K_M".to_string())
        );
        assert_eq!(
            extract_quant_tag("Model-Q8_0.gguf"),
            Some("Q8_0".to_string())
        );
        assert_eq!(
            extract_quant_tag("Model-IQ4_XS.gguf"),
            Some("IQ4_XS".to_string())
        );
        assert_eq!(extract_quant_tag("Model-F16.gguf"), Some("F16".to_string()));
        // No quant tag
        assert_eq!(extract_quant_tag("model.gguf"), None);
    }

    #[test]
    fn test_multipart_detection() {
        assert!(is_multipart_file(
            "Llama-3.3-70B-Instruct.Q8_0.gguf-00001-of-00006.gguf"
        ));
        assert!(!is_multipart_file("Llama-3.2-3B-Instruct-Q4_K_M.gguf"));
    }

    #[test]
    fn test_extract_quant_tag_multipart() {
        assert_eq!(
            extract_quant_tag("Llama-3.3-70B-Instruct.Q6_K.gguf-00001-of-00006.gguf"),
            Some("Q6_K".to_string())
        );
    }
}
