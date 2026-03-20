# vram.supply Provider Agent

Connect your GPU to the [vram.supply](https://vram.supply) marketplace and earn by serving model inference.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ohone/vram-supply/main/provider-agent/install.sh | sh
```

Pin a specific version:

```bash
VRAM_SUPPLY_AGENT_VERSION=vX.Y.Z curl -fsSL https://raw.githubusercontent.com/ohone/vram-supply/main/provider-agent/install.sh | sh
```

The installer downloads the binary for your platform, verifies its SHA256 checksum, and installs it to `~/.local/bin/vramsupply`.

### Verify with Sigstore (optional)

Every release binary is signed with [Sigstore](https://sigstore.dev) keyless signing. To verify manually:

```bash
cosign verify-blob \
  --bundle vramsupply-x86_64-unknown-linux-gnu.bundle \
  vramsupply-x86_64-unknown-linux-gnu
```

## Quick start

```bash
# 1. Set your API key
export VRAM_SUPPLY_API_KEY=your-api-key

# 2. Start serving a model (canonical HuggingFace model ID — auto-downloads GGUF)
vramsupply serve --model "qwen/qwen3.5-9b" --quant Q4_K_M

# Or serve a local GGUF file directly
vramsupply serve --model ./my-model.gguf
```

The agent will:
1. Start a local `llama-server` process with your model
2. Register with the vram.supply platform
3. Send periodic heartbeats and presence updates
4. Accept inference requests routed by the platform

Press `Ctrl+C` to gracefully shut down (deregisters from the platform).

## Sell Subscription Quota

Earn by selling unused capacity from supported coding subscriptions.

### Supported providers

- **Claude Code** — local agent + Anthropic sandbox runtime
- **OpenAI Codex** — local agent + local OAuth connection (`inference-only` for now)

### Quick start

#### Claude Code

```bash
# 1. Set your API key
export VRAM_SUPPLY_API_KEY=your-api-key

# 2. Start selling Claude Code quota
vramsupply sell-quota claude
```

#### OpenAI Codex

```bash
# 1. Set your API key
export VRAM_SUPPLY_API_KEY=your-api-key

# 2. Connect your OpenAI account locally
vramsupply connect openai

# 3. Start sell-quota mode with the Codex backend
vramsupply sell-quota codex
```

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `VRAM_SUPPLY_QUOTA_MAX_CONCURRENT` | `1` | Max simultaneous sessions |
| `VRAM_SUPPLY_QUOTA_MAX_BUDGET_USD` | `1.00` | Max spend per request |
| `VRAM_SUPPLY_QUOTA_MODEL` | `sonnet` | Model alias |
| `VRAM_SUPPLY_QUOTA_DENY_READ` | `~/.ssh,~/.aws,~/.gnupg` | Dirs blocked from sandbox reads |
| `VRAM_SUPPLY_QUOTA_SESSION_TIMEOUT` | `300` | Session timeout in seconds |

## Commands

| Command | Description |
|---------|-------------|
| `vramsupply auth` | Show current authentication status |
| `vramsupply connect openai` | Connect an OpenAI Codex account locally via OAuth |
| `vramsupply serve --model <path-or-model-id>` | Start serving a model |
| `vramsupply serve --model <model-id> --quant <quant>` | Serve a model by canonical ID with a specific quantization |
| `vramsupply serve --model <path> --model-name <name>` | Serve with a custom model name |
| `vramsupply serve --model <path> --hf-repo <repo_id>` | Serve with model integrity verification |
| `vramsupply serve --model <path> --skip-verify` | Serve without model verification |
| `vramsupply serve --input-price 50 --output-price 150` | Serve with custom pricing (cents per million tokens) |
| `vramsupply sell-quota claude` | Start sell-quota mode with Claude Code |
| `vramsupply sell-quota codex` | Start sell-quota mode with OpenAI Codex |
| `vramsupply sell-quota claude --input-price 300 --output-price 1500` | Sell quota with custom pricing |
| `vramsupply models list` | List locally available GGUF models |
| `vramsupply models pull <hf_repo_id>` | Download a model from HuggingFace |
| `vramsupply status` | Show agent status |

## Configuration

All configuration is via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `VRAM_SUPPLY_API_KEY` | *(required)* | API key for platform authentication |
| `VRAM_SUPPLY_PLATFORM_URL` | `https://api.vram.supply` | Platform API endpoint |
| `VRAM_SUPPLY_PUBLIC_URL` | `http://localhost:$PORT` | Public HTTPS URL for your inference endpoint (see [Production TLS](#production-tls) below). The default is only suitable for local development. |
| `VRAM_SUPPLY_PORT` | `8080` | Port for llama-server |
| `VRAM_SUPPLY_MODEL_DIR` | `~/.vram-supply/models` | Directory to search for model files |
| `VRAM_SUPPLY_LLAMA_SERVER_PATH` | `llama-server` | Path to the llama-server binary |
| `VRAM_SUPPLY_GPU_LAYERS` | `99` | Number of layers to offload to GPU |
| `VRAM_SUPPLY_MAX_CONCURRENT` | `1` | Max concurrent inference requests |
| `VRAM_SUPPLY_CONTEXT_LENGTH` | `8192` | Context length offered |
| `VRAM_SUPPLY_INPUT_PRICE` | `100` | Input price per million tokens (cents). Also settable via `--input-price`. |
| `VRAM_SUPPLY_OUTPUT_PRICE` | `200` | Output price per million tokens (cents). Also settable via `--output-price`. |

## Production TLS

The vram.supply platform **requires HTTPS** for all public provider endpoints. Registrations using `http://` are rejected by the API.

The default `VRAM_SUPPLY_PUBLIC_URL` (`http://localhost:$PORT`) is intended for **local development only** and cannot be used for production registration.

To expose your llama-server over HTTPS, use one of the following approaches:

- **Cloudflare Tunnel** (`cloudflared`) — provides a public HTTPS URL that tunnels to your local llama-server. Free tier available. Easiest option.
  ```bash
  cloudflared tunnel --url http://localhost:8080
  # Then set VRAM_SUPPLY_PUBLIC_URL to the generated https:// URL
  ```
- **Caddy reverse proxy** — automatic TLS via Let's Encrypt with zero configuration.
  ```bash
  caddy reverse-proxy --from your-domain.example.com --to localhost:8080
  ```
- **nginx + certbot** — manual but widely understood. Obtain a certificate with `certbot` and configure an nginx `server` block that proxies to llama-server.

## Model resolution

`--model` accepts either a **local GGUF path** or a **canonical HuggingFace model ID**:

```bash
# Local path — serves directly
vramsupply serve --model ./my-model.gguf

# Canonical model ID — auto-downloads the right GGUF
vramsupply serve --model "meta-llama/llama-3.2-3b-instruct" --quant Q4_K_M
```

When you pass a canonical model ID (e.g., `qwen/qwen3.5-9b`):

1. The CLI checks whether the canonical repo itself contains GGUF files.
2. If not, it searches HuggingFace for community GGUF repos (e.g., `bartowski/…`, `unsloth/…`).
3. Candidates are ranked by name match (exact first, prefix second) and downloads as a tiebreaker. Derivative repos (distilled, uncensored, etc.) are excluded.
4. The `--quant` tag (e.g., `Q4_K_M`, `Q8_0`) selects the specific GGUF file from the chosen repo.
5. The file is downloaded to `~/.vram-supply/models/<canonical-id>/` and verified against the GGUF repo's LFS metadata.

The canonical model ID is used as the **marketplace identity** when registering with the platform. The resolved GGUF repo is used for **file verification**. These are often different repos.

`--quant` is **required** when `--model` is a canonical ID. If omitted, the CLI errors immediately without making any network requests.

Multipart GGUF files (sharded across multiple files) are not supported in v1.

## Prerequisites

- [llama-server](https://github.com/ggerganov/llama.cpp) must be installed and available in your PATH (or set `VRAM_SUPPLY_LLAMA_SERVER_PATH`)
- A GGUF model file (or use a canonical model ID with `--quant` to auto-download)

## Data storage

Agent identity is stored in `~/.vram-supply/`:

| File | Purpose |
|------|---------|
| `vramsupply.json` | Persistent agent UID |
| `verification-cache.json` | SHA-256 model verification cache |

## Model verification

When serving a model, the agent can verify its integrity by comparing the file's SHA-256 hash against metadata from HuggingFace LFS. Use `--hf-repo <repo_id>` to enable verification (e.g., `--hf-repo TheBloke/Llama-2-7B-GGUF`). Use `--skip-verify` to bypass verification entirely. Verification results are cached locally to avoid re-hashing on subsequent runs.

## Building from source

```bash
git clone https://github.com/ohone/vram-supply.git
cd vram-supply/provider-agent
cargo build --release
# Binary is at target/release/vramsupply
```

## License

Apache-2.0
