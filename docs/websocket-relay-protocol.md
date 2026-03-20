# WebSocket Relay Protocol Specification

## Overview

The vram.supply WebSocket relay protocol enables secure 'sell-quota' mode operation, where agents resell unused capacity from their coding assistant subscriptions (Anthropic/OpenAI) through the vram.supply platform.

**Security is the primary design concern.** This protocol is designed to protect agent operators from attacks through the WebSocket channel while enabling legitimate inference request forwarding.

## Architecture

```
Platform → WebSocket → Agent → Upstream API (Anthropic/OpenAI)
    ↑         ←         ←              ←
 Response    Relay   Validation    API Call
```

### Flow Overview

1. Agent opens WSS connection to platform
2. Platform sends inference requests through WebSocket
3. Agent validates and forwards to upstream APIs using local OAuth tokens
4. Agent streams sanitized responses back through WebSocket

## 1. Message Schema (Allowlist Only)

All messages use a strict allowlist approach with typed JSON envelopes:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    // Platform → Agent
    InferenceRequest(InferenceRequest),
    CancelRequest(CancelRequest),
    
    // Agent → Platform  
    InferenceResponse(InferenceResponse),
    InferenceChunk(InferenceChunk),
    InferenceError(InferenceError),
    AgentStatus(AgentStatus),
    
    // Bidirectional
    Ping(Ping),
    Pong(Pong),
}
```

### Unknown Message Handling

- **Agent behavior**: Immediately close connection on unknown message types
- **No graceful degradation**: Security-first approach rejects any unexpected input
- **Logging**: Log unknown message attempts for security monitoring

### Platform → Agent Messages

#### InferenceRequest

```rust
pub struct InferenceRequest {
    pub request_id: String,              // Required, max 128 chars
    pub model: String,                   // Must be in ALLOWED_MODELS
    pub messages: Vec<ChatMessage>,      // Max 100 messages
    pub max_tokens: Option<u32>,         // Max 8192, enforced
    pub temperature: Option<f64>,        // 0.0-2.0 range
    pub stream: bool,                    // Default: true
    pub timestamp: SystemTime,           // For timeout tracking
}

pub struct ChatMessage {
    pub role: MessageRole,               // system, user, assistant only
    pub content: String,                 // Max total 512KB per request
}
```

**Validation Rules:**
- `request_id`: 1-128 characters, alphanumeric + hyphens only
- `model`: Must be in ALLOWED_MODELS allowlist exactly
- `messages`: 1-100 messages, total content ≤ 512KB
- `max_tokens`: ≤ 8192 if specified
- `temperature`: 0.0 ≤ temp ≤ 2.0 if specified
- `content`: No suspicious patterns (see Content Filtering)

#### CancelRequest

```rust
pub struct CancelRequest {
    pub request_id: String,
}
```

### Agent → Platform Messages

#### InferenceResponse (Non-streaming)

```rust
pub struct InferenceResponse {
    pub request_id: String,
    pub content: String,                 // Sanitized content
    pub usage: UsageStats,
    pub model: String,                   // Validated model name
    pub timestamp: SystemTime,
}

pub struct UsageStats {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
```

#### InferenceChunk (Streaming)

```rust
pub struct InferenceChunk {
    pub request_id: String,
    pub delta: String,                   // Sanitized delta content
    pub finished: bool,
    pub usage: Option<UsageStats>,       // Only on final chunk
}
```

#### InferenceError

```rust
pub struct InferenceError {
    pub request_id: String,
    pub error_type: ErrorType,
    pub error_message: String,           // Sanitized error message
    pub timestamp: SystemTime,
}

pub enum ErrorType {
    InvalidModel,                        // Model not in allowlist
    RequestTooLarge,                     // Size limits exceeded
    RateLimited,                         // Rate limits hit
    UpstreamError,                       // Sanitized upstream error
    Timeout,                             // Request timeout
    ValidationError,                     // Request validation failed
    QuotaExhausted,                      // API quota exhausted
    InternalError,                       // Agent internal error
}
```

## 2. Request Validation

Before forwarding any request to upstream APIs, agents MUST validate:

### Model Allowlist

```rust
const ALLOWED_MODELS: &[&str] = &[
    "claude-3-haiku-20240307",
    "claude-3-sonnet-20240229", 
    "claude-3-opus-20240229",
    "claude-3-5-haiku-20241022",
    "claude-3-5-sonnet-20241022",
    "gpt-4o-mini",
    "gpt-4o",
    "gpt-4-turbo",
    "gpt-3.5-turbo",
];
```

**Rule**: Exact string match required. No partial matches or normalization.

### Content Filtering

Reject requests containing these patterns (case-insensitive):

- `tool_choice` - Prevents tool/function usage
- `function_call` - Prevents function calling
- `tools` - Prevents tool definitions
- `file://` - Prevents file system access
- `data://` - Prevents data URI injection
- `<script` - Prevents script injection
- `javascript:` - Prevents JS execution
- `vbscript:` - Prevents VBScript execution

### Size Limits

```rust
const MAX_MESSAGE_SIZE: usize = 1_048_576;        // 1MB WebSocket message
const MAX_REQUEST_BODY_SIZE: usize = 524_288;     // 512KB request content
const MAX_RESPONSE_SIZE: usize = 2_097_152;       // 2MB response
```

### Parameter Limits

```rust
// Token limits
const MAX_ALLOWED_TOKENS: u32 = 8192;

// Temperature validation
fn validate_temperature(temp: f64) -> bool {
    (0.0..=2.0).contains(&temp)
}

// Message limits
const MAX_MESSAGES_PER_REQUEST: usize = 100;
```

## 3. Rate Limiting

Agents implement multi-layered rate limiting:

### Request Rate Limiting

```rust
const MAX_REQUESTS_PER_MINUTE: u32 = 60;
const MAX_CONCURRENT_REQUESTS: u32 = 5;
```

**Implementation**: Sliding window with request timestamps

### Token Rate Limiting

```rust
const MAX_TOKENS_PER_MINUTE: u32 = 100_000;
```

**Implementation**: Per-minute token usage tracking with reset

### Backpressure Mechanism

When rate limits are hit:

1. Return `RateLimited` error immediately
2. Do NOT queue requests
3. Client must implement retry with exponential backoff
4. WebSocket remains open (no connection termination)

### Circuit Breaker

If upstream API returns rate limit errors:

1. Track consecutive rate limit responses
2. After 5 consecutive rate limits, pause for 60 seconds
3. Return `RateLimited` errors during pause
4. Resume normal operation after pause

## 4. Response Sanitization

All upstream responses are sanitized before forwarding:

### Content Sanitization

Remove patterns using regex (case-insensitive):

```rust
const SANITIZATION_PATTERNS: &[&str] = &[
    r"<function_call>.*?</function_call>",    // Remove function calls
    r"<tool_call>.*?</tool_call>",            // Remove tool calls
    r"file://[^\s]+",                         // Remove file URIs
    r"data://[^\s]+",                         // Remove data URIs
    r"<script.*?</script>",                   // Remove scripts
    r"javascript:[^\s]+",                     // Remove JS URIs
    r"vbscript:[^\s]+",                       // Remove VBS URIs
];
```

**Replacement**: `[REDACTED]` for all patterns

### Header Sanitization

Remove potentially sensitive headers from upstream responses:

- `Authorization`
- `x-api-key` 
- `x-auth-token`
- `cookie`
- `set-cookie`

### Error Message Sanitization

For upstream API errors:

1. Limit to first 3 lines
2. Truncate to 200 characters max
3. Remove API key patterns: `sk-[a-zA-Z0-9]+`, `Bearer [a-zA-Z0-9]+`
4. Replace with `[REDACTED]`

### Usage Statistics Validation

Ensure usage stats are reasonable:

```rust
fn validate_usage_stats(stats: &mut UsageStats) {
    const MAX_REASONABLE_TOKENS: u32 = 50_000;
    
    if stats.prompt_tokens > MAX_REASONABLE_TOKENS {
        stats.prompt_tokens = MAX_REASONABLE_TOKENS;
    }
    if stats.completion_tokens > MAX_REASONABLE_TOKENS {
        stats.completion_tokens = MAX_REASONABLE_TOKENS;
    }
    stats.total_tokens = stats.prompt_tokens + stats.completion_tokens;
}
```

## 5. Connection Security

### TLS Configuration

```rust
pub struct TlsConfig {
    pub verify_certs: bool,                    // Always true in production
    pub expected_hostname: Option<String>,     // For additional validation
    pub pinned_cert_fingerprints: Vec<String>, // SHA256 fingerprints
    pub min_tls_version: TlsVersion,          // Minimum TLS 1.2
}
```

### Certificate Validation

1. **Standard validation**: Valid chain, not expired, hostname match
2. **Optional pinning**: SHA256 fingerprint validation
3. **Revocation checking**: OCSP where available

### WebSocket Security Headers

Required during handshake:

```
Authorization: Bearer <agent_api_key>
User-Agent: vramsupply-agent/<version>
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Version: 13
```

### Connection Authentication

1. Agent API key in `Authorization` header
2. Platform validates key and agent identity
3. Connection rejected immediately if invalid
4. No retry attempts on auth failure

### Reconnection Limits

```rust
const MAX_RECONNECTS_PER_HOUR: u32 = 10;
```

**Implementation**: Exponential backoff with jitter
- Initial delay: 1 second
- Maximum delay: 60 seconds  
- Reset after 5 minutes successful connection

## 6. Resource Limits

### Memory Budget

```rust
const MAX_MEMORY_PER_REQUEST: usize = 10_485_760; // 10MB
const MAX_CONCURRENT_MEMORY: usize = 52_428_800;  // 50MB total
```

**Enforcement**: 
- Track memory usage per request
- Reject new requests if budget exceeded
- Clean up memory aggressively after response

### Timeout Enforcement

```rust
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(120);  // 2 minutes
const WEBSOCKET_PING_INTERVAL: Duration = Duration::from_secs(30);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
```

**Timeout Handling**:
- Upstream requests: Cancel and return `Timeout` error
- WebSocket pings: Close connection if no pong within 60s
- Overall connection: Monitor health continuously

### File Descriptor Limits

- Maximum 100 concurrent connections (theoretical)
- In practice: 1 WebSocket + upstream HTTP connections
- Monitor and log FD usage

### CPU Limiting

- JSON parsing: Use streaming parsers where possible
- Regex operations: Compile patterns once, reuse
- Validation: Fail fast on first violation
- No expensive operations in request path

## Security Considerations

### Threat Model

**Protected against**:
- Code injection through content
- File system access attempts  
- Tool/function abuse
- Information disclosure via responses
- DoS through resource exhaustion
- Connection hijacking (TLS)
- Replay attacks (timestamps)

**Not protected against**:
- Legitimate but expensive requests (within limits)
- Social engineering of prompt content
- Upstream API vulnerabilities
- Local system compromise

### Security Boundaries

1. **Network boundary**: TLS encryption, certificate validation
2. **Message boundary**: Strict JSON schema validation
3. **Content boundary**: Pattern-based filtering and sanitization
4. **Resource boundary**: Hard limits on size, time, concurrency
5. **API boundary**: Allowlist-based model access

### Audit Logging

Log security-relevant events:

```rust
// Log these events with structured logging
- Connection attempts (success/failure)
- Authentication failures
- Unknown message types
- Rate limit violations  
- Content filtering triggers
- Upstream API errors
- Resource limit hits
- Timeout events
```

### Monitoring Recommendations

- Connection success rates
- Request validation failure rates
- Content filtering trigger rates
- Rate limit hit rates
- Response time percentiles
- Memory usage patterns
- Upstream API error rates

## Implementation Notes

### Error Handling Strategy

1. **Fail securely**: Close connection on unexpected errors
2. **Log extensively**: All errors logged with context
3. **No error information leak**: Generic error messages to platform
4. **Graceful degradation**: Where possible, reject rather than crash

### Performance Considerations

- Use `tokio` async runtime for concurrency
- Stream processing for large responses
- Connection pooling for upstream APIs
- JSON streaming for large messages
- Memory-mapped files avoided (security)

### Testing Strategy

- Unit tests for all validation functions
- Integration tests with mock upstream APIs
- Fuzz testing for JSON parsing
- Load testing for rate limiting
- Security testing with malicious inputs

This protocol specification prioritizes security while enabling the legitimate use case of reselling unused API capacity. The implementation follows defense-in-depth principles with multiple layers of validation, sanitization, and resource protection.