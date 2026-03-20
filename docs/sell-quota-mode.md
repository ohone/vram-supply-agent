# Sell-Quota Mode Guide

The vram.supply agent's sell-quota mode allows you to monetize unused capacity from your existing coding assistant subscriptions by securely reselling inference requests through the vram.supply platform.

## Quick Start

### 1. Check Prerequisites

```bash
# Ensure vram.supply agent is configured
echo $VRAM_SUPPLY_API_KEY       # Your vram.supply API key
echo $VRAM_SUPPLY_PLATFORM_URL  # Usually https://api.vram.supply

# Claude Code sellers also need Claude + sandbox runtime installed
# OpenAI Codex sellers must run: vramsupply connect openai
```

### 2. Check Configuration

```bash
# Claude Code
vramsupply sell-quota --status

# OpenAI Codex
vramsupply sell-quota --provider openai-codex --status
```

This command validates your configuration and shows:
- Which local quota backend is selected
- Whether the local provider connection exists
- Security limits in place
- Configuration issues (if any)

### 3. Start Sell-Quota Mode

```bash
# Claude Code (default pricing from env vars or 100/200 cents per M tokens)
vramsupply sell-quota

# Claude Code with custom pricing
vramsupply sell-quota --input-price 300 --output-price 1500

# OpenAI Codex (inference-only)
vramsupply connect openai
vramsupply sell-quota --provider openai-codex --input-price 50 --output-price 150
```

The agent will:
- Connect to the vram.supply platform via secure WebSocket
- Wait for inference requests from the platform
- Forward requests to the selected local quota backend
- Stream responses back securely

## Environment Variables

### Required Variables

```bash
# vram.supply platform credentials
export VRAM_SUPPLY_API_KEY="your-vram-supply-api-key"
export VRAM_SUPPLY_PLATFORM_URL="https://api.vram.supply"

# At least one upstream API key is required
export ANTHROPIC_API_KEY="your-anthropic-api-key"    # For Claude models
export OPENAI_API_KEY="your-openai-api-key"          # For GPT models
```

### Optional Variables

```bash
# Custom API endpoints (for enterprise accounts)
export ANTHROPIC_BASE_URL="https://api.anthropic.com"  # Default
export OPENAI_BASE_URL="https://api.openai.com"        # Default
```

## Supported Models

The agent will only forward requests for these explicitly allowed models:

### Anthropic (Claude)
- `claude-3-haiku-20240307`
- `claude-3-sonnet-20240229`
- `claude-3-opus-20240229`
- `claude-3-5-haiku-20241022`
- `claude-3-5-sonnet-20241022`

### OpenAI (GPT)
- `gpt-4o-mini`
- `gpt-4o`
- `gpt-4-turbo`
- `gpt-3.5-turbo`

## Security Features

### What's Protected

✅ **Request Validation**: All incoming requests are strictly validated
✅ **Content Filtering**: Malicious patterns are blocked (tool usage, file access, etc.)
✅ **Rate Limiting**: Built-in protection against request flooding
✅ **Response Sanitization**: Upstream responses are cleaned of sensitive data
✅ **Resource Limits**: Memory, CPU, and network usage are bounded
✅ **TLS Security**: All connections use modern TLS with certificate validation

### What You Control

🔧 **API Keys**: Your upstream API keys never leave your machine
🔧 **Usage Limits**: Built-in rate limiting protects your quotas
🔧 **Model Access**: Only allowed models are accessible
🔧 **Request Content**: Malicious requests are filtered out
🔧 **Shutdown**: You can stop the agent at any time (Ctrl+C)

### Sandbox deny-read paths

Sell-quota mode runs Claude Code inside the Anthropic sandbox runtime (`srt`) with a deny-read list for sensitive local paths.

By default the sandbox blocks access to:
- `~/.ssh`
- `~/.aws`
- `~/.gnupg`
- `~/.netrc`
- `~/.npmrc`
- `~/.gitconfig`
- `~/.config`
- `~/.kube`
- `~/.docker`
- `~/.vram-supply`
- `~/.claude`
- `~/.env`
- the agent process current working directory

The path strings use the sandbox runtime's `//` absolute-path prefix convention.

You can add extra deny paths with:

```bash
export VRAM_SUPPLY_QUOTA_DENY_READ="/path/to/secrets,/another/path"
```

Each path will be added to the runtime deny list before a Claude session starts.

## Monitoring & Logs

The agent provides structured logging for monitoring:

```bash
# Run with debug logging
RUST_LOG=debug vramsupply sell-quota

# Key log events to monitor:
# - Connection status
# - Request validation failures  
# - Rate limit hits
# - Upstream API errors
# - Security filtering events
```

### Important Metrics

- **Connection uptime**: Agent should maintain stable WebSocket connection
- **Request success rate**: Most requests should succeed
- **Validation failures**: High rates may indicate attack attempts
- **Rate limit hits**: May indicate need to adjust usage patterns
- **Error rates**: Monitor for upstream API issues

## Troubleshooting

### Connection Issues

**Problem**: `WebSocket connection failed`
```bash
# Check network connectivity
curl -I https://api.vram.supply

# Verify API key
vramsupply auth

# Check firewall settings (ensure outbound HTTPS/WSS allowed)
```

**Problem**: `Invalid upstream credentials`
```bash
# Test Anthropic API key
curl -H "Authorization: Bearer $ANTHROPIC_API_KEY" \
  https://api.anthropic.com/v1/models

# Test OpenAI API key  
curl -H "Authorization: Bearer $OPENAI_API_KEY" \
  https://api.openai.com/v1/models
```

### Rate Limiting

**Problem**: `Rate limit exceeded`
```bash
# This is normal behavior - the agent protects your quotas
# Requests are automatically rejected when limits are hit
# No action needed - the system is working correctly
```

**Problem**: Upstream API rate limits
```bash
# The agent will automatically pause when upstream APIs return rate limits
# Monitor logs for "upstream rate limit" messages
# Consider upgrading your upstream API plans if this happens frequently
```

### Performance Issues

**Problem**: High memory usage
```bash
# The agent has built-in memory limits
# Memory usage should stay under 50MB total
# If higher, this may indicate a bug - please report it
```

**Problem**: Slow responses
```bash
# Check upstream API latency
# Responses are limited to 2 minutes timeout
# Network issues between agent and upstream APIs can cause slowness
```

## Best Practices

### Security

1. **Keep API keys secure**: Never share or commit your API keys
2. **Monitor logs**: Watch for unusual patterns or errors
3. **Regular updates**: Keep the agent updated to latest version
4. **Network security**: Run on trusted networks only

### Performance

1. **Stable internet**: Ensure reliable internet connection
2. **Resource allocation**: Allow sufficient CPU/memory for the agent
3. **Monitor quotas**: Track your upstream API usage
4. **Plan capacity**: Understand your upstream API limits

### Financial

1. **Understand costs**: Each forwarded request uses your upstream quota
2. **Monitor usage**: Track your upstream API consumption
3. **Set alerts**: Configure billing alerts with upstream providers
4. **Calculate margins**: Ensure sell-quota pricing covers your costs

## FAQ

**Q: Do my API keys leave my machine?**
A: No, your upstream API keys are stored locally and used only for direct API calls to Anthropic/OpenAI.

**Q: Can someone attack my machine through this?**
A: The agent is designed with security as the primary concern. All inputs are validated, and responses are sanitized. However, run it on a dedicated/isolated system for maximum security.

**Q: What happens if I lose internet connection?**
A: The agent will automatically reconnect to the platform. In-flight requests may be lost and will need to be retried by the platform.

**Q: How much can I earn?**
A: Earnings depend on demand for your available models and your pricing. Monitor your usage and costs to ensure profitability.

**Q: Can I run multiple agents?**
A: Each agent instance should have a unique configuration. Running multiple instances with the same API key may cause conflicts.

**Q: What if upstream APIs change?**
A: The agent validates against a specific set of allowed models. Updates may be needed when new models are released.

## Support

For issues or questions:

1. Check the logs for specific error messages
2. Verify your configuration with `vramsupply sell-quota --status`
3. Test your upstream API keys independently
4. Review this guide for common issues
5. Contact vram.supply support with detailed logs and error messages

## Advanced Configuration

### Custom Rate Limits

While not exposed as command-line options, you can modify the constants in the source code and recompile for custom limits:

```rust
// In src/websocket_relay/protocol.rs
pub const MAX_CONCURRENT_REQUESTS: u32 = 5;    // Concurrent requests
pub const MAX_REQUESTS_PER_MINUTE: u32 = 60;   // Request rate limit
```

### TLS Configuration

For enterprise environments with custom certificates:

```rust
// In src/websocket_relay/connection.rs
// Modify TlsConfig to add certificate pinning
pub struct TlsConfig {
    pub pinned_cert_fingerprints: Vec<String>, // Add SHA256 fingerprints
}
```

### Logging Configuration

```bash
# Detailed component logging
export RUST_LOG="vramsupply=debug,websocket_relay=trace"

# JSON structured logging
export RUST_LOG_FORMAT="json"
```

This mode enables you to monetize your unused API capacity while maintaining security and control over your credentials and usage.