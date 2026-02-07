use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

#[derive(Debug, Deserialize)]
struct HfFileEntry {
    path: String,
    #[allow(dead_code)]
    size: u64,
    lfs: Option<LfsInfo>,
}

#[derive(Debug, Deserialize)]
struct LfsInfo {
    oid: String,
    #[allow(dead_code)]
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VerificationCacheEntry {
    file_size: u64,
    mtime_secs: i64,
    sha256: String,
    hf_repo_id: String,
    verified_at: u64,
}

/// Verify a model file against HuggingFace LFS metadata.
///
/// Returns the SHA-256 hex string of the model, or `"unverified"` if
/// `skip_verify` is true.
pub async fn verify_model(model_path: &str, hf_repo_id: &str, skip_verify: bool) -> Result<String> {
    if skip_verify {
        return Ok("unverified".to_string());
    }

    let metadata = fs::metadata(model_path)
        .with_context(|| format!("Failed to read metadata for {}", model_path))?;
    let file_size = metadata.len();
    let mtime_secs = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Check cache
    let mut cache = load_cache();
    if let Some(entry) = cache.get(model_path) {
        if entry.file_size == file_size
            && entry.mtime_secs == mtime_secs
            && entry.hf_repo_id == hf_repo_id
        {
            tracing::info!("Verification cache hit for {}", model_path);
            return Ok(entry.sha256.clone());
        }
    }

    // Fetch expected hash from HuggingFace
    let gguf_filename = crate::models::gguf_filename(model_path)?;
    let lfs_info = fetch_hf_file_metadata(hf_repo_id, &gguf_filename).await?;

    // HuggingFace LFS oid may be prefixed with "sha256:"
    let expected_hash = lfs_info
        .oid
        .strip_prefix("sha256:")
        .unwrap_or(&lfs_info.oid)
        .to_string();

    // Compute local hash
    println!("Verifying model integrity (this may take a moment for large files)...");
    let local_sha256 = compute_sha256(model_path)?;

    if local_sha256 != expected_hash {
        anyhow::bail!(
            "Model verification failed!\n  \
             Expected SHA-256: {}\n  \
             Computed SHA-256: {}\n  \
             The local file does not match the HuggingFace repository '{}'.",
            expected_hash,
            local_sha256,
            hf_repo_id
        );
    }

    // Update cache
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    cache.insert(
        model_path.to_string(),
        VerificationCacheEntry {
            file_size,
            mtime_secs,
            sha256: local_sha256.clone(),
            hf_repo_id: hf_repo_id.to_string(),
            verified_at: now,
        },
    );
    save_cache(&cache);

    Ok(local_sha256)
}

/// Fetch LFS metadata for a specific file from a HuggingFace repository.
async fn fetch_hf_file_metadata(repo_id: &str, gguf_filename: &str) -> Result<LfsInfo> {
    let url = format!("https://huggingface.co/api/models/{}/tree/main", repo_id);

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "vramsply")
        .send()
        .await
        .with_context(|| format!("Failed to fetch HuggingFace tree API for '{}'", repo_id))?;

    if matches!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::UNAUTHORIZED
    ) {
        anyhow::bail!(
            "HuggingFace repository '{}' not found or not accessible (HTTP {}). \
             Check the --hf-repo value.",
            repo_id,
            resp.status()
        );
    }

    if !resp.status().is_success() {
        anyhow::bail!(
            "HuggingFace tree API returned HTTP {} for '{}'",
            resp.status(),
            repo_id
        );
    }

    let entries: Vec<HfFileEntry> = resp
        .json()
        .await
        .context("Failed to parse HuggingFace tree API response")?;

    let entry = entries
        .into_iter()
        .find(|e| e.path == gguf_filename)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "File '{}' not found in HuggingFace repository '{}'",
                gguf_filename,
                repo_id
            )
        })?;

    entry.lfs.ok_or_else(|| {
        anyhow::anyhow!(
            "File '{}' in '{}' has no LFS metadata (not an LFS-tracked file)",
            gguf_filename,
            repo_id
        )
    })
}

/// Compute the SHA-256 hash of a file by streaming it in 1 MB chunks.
fn compute_sha256(path: &str) -> Result<String> {
    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open file for hashing: {}", path))?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];

    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("Failed to read file during hashing: {}", path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn cache_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".vram-supply").join("verification-cache.json"))
}

fn load_cache() -> HashMap<String, VerificationCacheEntry> {
    let Some(path) = cache_path() else {
        return HashMap::new();
    };
    let Ok(data) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_cache(cache: &HashMap<String, VerificationCacheEntry>) {
    let Some(path) = cache_path() else {
        tracing::warn!("Could not determine cache path, skipping cache save");
        return;
    };
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        {
            if let Err(e) = fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
            {
                tracing::warn!("Failed to create cache directory: {}", e);
                return;
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(e) = fs::create_dir_all(parent) {
                tracing::warn!("Failed to create cache directory: {}", e);
                return;
            }
        }
    }
    let Ok(json) = serde_json::to_string_pretty(cache) else {
        tracing::warn!("Failed to serialize verification cache");
        return;
    };
    #[cfg(unix)]
    {
        use std::io::Write;
        match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(json.as_bytes()) {
                    tracing::warn!("Failed to write verification cache: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to open verification cache for writing: {}", e);
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(e) = fs::write(&path, json) {
            tracing::warn!("Failed to write verification cache: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_sha256_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        fs::write(&path, b"hello world").unwrap();

        let hash = compute_sha256(path.to_str().unwrap()).unwrap();
        // SHA-256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_compute_sha256_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        fs::write(&path, b"").unwrap();

        let hash = compute_sha256(path.to_str().unwrap()).unwrap();
        // SHA-256 of empty string
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_verify_model_skip() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(verify_model("/nonexistent", "", true));
        assert_eq!(result.unwrap(), "unverified");
    }

    #[test]
    fn test_cache_round_trip() {
        // Save and load a cache entry
        let mut cache = HashMap::new();
        cache.insert(
            "/tmp/test-model.gguf".to_string(),
            VerificationCacheEntry {
                file_size: 12345,
                mtime_secs: 1000000,
                sha256: "abc123".to_string(),
                hf_repo_id: "test/repo".to_string(),
                verified_at: 99999,
            },
        );
        save_cache(&cache);
        let loaded = load_cache();
        let entry = loaded.get("/tmp/test-model.gguf").unwrap();
        assert_eq!(entry.sha256, "abc123");
        assert_eq!(entry.hf_repo_id, "test/repo");
    }

    /// Integration test: fetch HF metadata for a known repo.
    /// Requires network access — ignored in CI.
    #[test]
    #[ignore]
    fn test_fetch_hf_metadata_real() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let lfs = rt
            .block_on(fetch_hf_file_metadata(
                "CompendiumLabs/bge-small-en-v1.5-gguf",
                "bge-small-en-v1.5-q4_k_m.gguf",
            ))
            .unwrap();
        assert_eq!(
            lfs.oid,
            "363a0a4855dff6c653e06efe3209157debcf7f74e52d0d7c71e2747cd523043e"
        );
    }

    /// Integration test: fetch HF metadata for non-existent repo → 404.
    #[test]
    #[ignore]
    fn test_fetch_hf_metadata_bad_repo() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(fetch_hf_file_metadata(
                "nonexistent-org/nonexistent-repo-xyz-12345",
                "model.gguf",
            ))
            .unwrap_err();
        assert!(
            err.to_string().contains("not found or not accessible"),
            "unexpected error: {}",
            err
        );
    }

    /// Integration test: fetch HF metadata for wrong filename → file not found.
    #[test]
    #[ignore]
    fn test_fetch_hf_metadata_wrong_file() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(fetch_hf_file_metadata(
                "CompendiumLabs/bge-small-en-v1.5-gguf",
                "nonexistent-file.gguf",
            ))
            .unwrap_err();
        assert!(
            err.to_string().contains("not found in HuggingFace"),
            "unexpected error: {}",
            err
        );
    }

    /// Full end-to-end test: verify a real downloaded model file.
    /// Requires /tmp/bge-small-en-v1.5-q4_k_m.gguf to exist.
    #[test]
    #[ignore]
    fn test_verify_model_real() {
        let model_path = "/tmp/bge-small-en-v1.5-q4_k_m.gguf";
        if !std::path::Path::new(model_path).exists() {
            eprintln!("Skipping: {} not found", model_path);
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sha = rt
            .block_on(verify_model(
                model_path,
                "CompendiumLabs/bge-small-en-v1.5-gguf",
                false,
            ))
            .unwrap();
        assert_eq!(
            sha,
            "363a0a4855dff6c653e06efe3209157debcf7f74e52d0d7c71e2747cd523043e"
        );
    }

    /// End-to-end test: verify with wrong repo → hash mismatch.
    #[test]
    #[ignore]
    fn test_verify_model_wrong_repo() {
        let model_path = "/tmp/bge-small-en-v1.5-q4_k_m.gguf";
        if !std::path::Path::new(model_path).exists() {
            eprintln!("Skipping: {} not found", model_path);
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Use a repo that has a file with the same name but different content
        // For now, just verify that passing a repo without this file fails
        let err = rt
            .block_on(verify_model(
                model_path,
                "ggml-org/gte-small-Q8_0-GGUF",
                false,
            ))
            .unwrap_err();
        // Should fail because the filename doesn't exist in that repo
        assert!(
            err.to_string().contains("not found in HuggingFace"),
            "unexpected error: {}",
            err
        );
    }
}
