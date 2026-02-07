# vram.supply Provider Agent

Connect your GPU to the [vram.supply](https://vram.supply) marketplace and earn by serving model inference.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ohone/vram-supply-agent/main/install.sh | sh
```

Pin a specific version:

```bash
VRAM_SUPPLY_AGENT_VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/ohone/vram-supply-agent/main/install.sh | sh
```

The installer downloads the binary for your platform, verifies its SHA256 checksum, and installs it to `~/.local/bin/vramsply`.

### Verify with Sigstore (optional)

Every release binary is signed with [Sigstore](https://sigstore.dev) keyless signing. To verify manually:

```bash
cosign verify-blob \
  --bundle vramsply-x86_64-unknown-linux-gnu.bundle \
  vramsply-x86_64-unknown-linux-gnu
```

## Quick start

```bash
# 1. Authenticate with the platform
vramsply auth login

# 2. Start serving a model
vramsply serve --model ./my-model.gguf
```

The agent will:
1. Start a local `llama-server` process with your model
2. Register with the vram.supply platform
3. Send periodic heartbeats and presence updates
4. Accept inference requests routed by the platform

Press `Ctrl+C` to gracefully shut down (deregisters from the platform).

## Commands

| Command | Description |
|---------|-------------|
| `vramsply auth login` | Authenticate via browser (PKCE flow) |
| `vramsply auth login --headless` | Authenticate via device code (for headless servers) |
| `vramsply auth status` | Show current authentication status |
| `vramsply auth logout` | Clear stored credentials |
| `vramsply serve --model <path>` | Start serving a model |
| `vramsply serve --model <path> --model_name <name>` | Serve with a custom model name |
| `vramsply serve --headless` | Serve with device code auth |
| `vramsply models list` | List locally available GGUF models |
| `vramsply models pull <hf_repo_id>` | Download a model from HuggingFace |
| `vramsply status` | Show agent status |

## Configuration

All configuration is via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `VRAM_SUPPLY_PLATFORM_URL` | `https://api.vram.supply` | Platform API endpoint |
| `VRAM_SUPPLY_PUBLIC_URL` | `http://localhost:$PORT` | Public URL for your inference endpoint |
| `VRAM_SUPPLY_PORT` | `8080` | Port for llama-server |
| `VRAM_SUPPLY_MODEL_DIR` | `~/.vram-supply/models` | Directory to search for model files |
| `VRAM_SUPPLY_LLAMA_SERVER_PATH` | `llama-server` | Path to the llama-server binary |
| `VRAM_SUPPLY_GPU_LAYERS` | `99` | Number of layers to offload to GPU |
| `VRAM_SUPPLY_MAX_CONCURRENT` | `1` | Max concurrent inference requests |
| `VRAM_SUPPLY_CONTEXT_LENGTH` | `8192` | Context length offered |
| `VRAM_SUPPLY_INPUT_PRICE` | `100` | Input price per million tokens (cents) |
| `VRAM_SUPPLY_OUTPUT_PRICE` | `200` | Output price per million tokens (cents) |

## Prerequisites

- [llama-server](https://github.com/ggerganov/llama.cpp) must be installed and available in your PATH (or set `VRAM_SUPPLY_LLAMA_SERVER_PATH`)
- A GGUF model file

## Data storage

Credentials and agent identity are stored in `~/.vram-supply/`:

| File | Purpose |
|------|---------|
| `credentials.json` | OAuth tokens (access + refresh) |
| `vramsply.json` | Persistent agent UID |

## Building from source

```bash
git clone https://github.com/ohone/vram-supply-agent.git
cd vram-supply-agent
cargo build --release
# Binary is at target/release/vramsply
```

## License

Apache-2.0
