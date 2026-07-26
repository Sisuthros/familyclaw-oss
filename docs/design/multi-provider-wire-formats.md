# Multi-provider wire formats (ANY-AI provider support)

Status: **partial** — `OpenAiChat` (pre-existing) and `GeminiGenerate`
(new, verified minimal slice) are implemented. `AnthropicMessages` and
`Bedrock` are design-only below; selecting either returns a clear
`LlmError::Http` at call time instead of silently sending the wrong wire
shape to an incompatible endpoint.

## Why this exists

Before this change, `familyclaw-agent`'s [`LlmClient`] (`crates/familyclaw-agent/src/llm.rs`)
only spoke the OpenAI-compatible `/chat/completions` wire format — hardcoded
into `LlmClient::build_endpoint` and the request/response structs. Every
provider in [`LlmEndpointResolver`]/[`EnvEndpointResolver`]
(`crates/familyclaw-agent/src/llm_chain.rs`) had to expose an
OpenAI-compatible proxy to be usable, which rules out talking to a
provider's *native* API directly (Gemini `generateContent`, Anthropic
`/v1/messages`, Bedrock `InvokeModel`/`Converse`).

## The seam: `LlmWireFormat`

```rust
pub enum LlmWireFormat {
    OpenAiChat,        // implemented (pre-existing)
    GeminiGenerate,     // implemented (verified minimal slice, this change)
    AnthropicMessages,  // NOT implemented — this document
    Bedrock,            // NOT implemented — this document
}
```

`LlmConfig::wire_format: LlmWireFormat` (`#[serde(default)]`, defaults to
`OpenAiChat` — every config built before this field existed keeps behaving
identically). `LlmClient::complete_once` / `complete_with_tools_choice` /
`complete_stream` dispatch on it. `EnvEndpointResolver::with_provider_format`
/ `with_provider_keys_format` register a provider prefix with an explicit
wire format; `with_provider`/`with_provider_keys` keep defaulting to
`OpenAiChat` (backward compatible).

This means a single [`LlmFailover`] chain (`crates/familyclaw-agent/src/llm_chain.rs`)
can mix entries that speak different wire formats — e.g. an OpenAI-compatible
primary with a native Gemini fallback — because the wire format travels with
each chain entry's [`LlmConfig`] template, not as a global client setting.

## What shipped: `GeminiGenerate` (verified minimal slice)

**Implemented and tested** (`crates/familyclaw-agent/src/llm.rs`):

- `LlmClient::complete()` (plain text completion, including the
  auto-continuation-on-`length` loop) — full text-completion path.
- `LlmClient::complete_with_tools()` / `complete_with_tools_choice()` — only
  the **tool-less** path (`tools` empty) delegates to the Gemini call; a
  non-empty `tools` list returns `LlmError::Http` (see "Not shipped" below).
- Endpoint: `POST {api_base}/models/{model}:generateContent`, with the API
  key attached as a `?key=` query parameter (via `reqwest`'s `.query(...)`,
  so it is URL-encoded correctly rather than hand-formatted into the URL
  string).
- Request shape: `{"contents": [...], "systemInstruction": {...}, "generationConfig": {"maxOutputTokens": N}}`.
  `LlmRole::System` messages are extracted and concatenated into
  `systemInstruction` (Gemini has no `"system"` role inside `contents`);
  `LlmRole::User`/`LlmRole::Tool` map to Gemini's `"user"` role;
  `LlmRole::Assistant` maps to `"model"`.
- Response shape: `candidates[0].content.parts[].text` (multi-part text is
  concatenated) + `candidates[0].finishReason` (`"STOP"` → `FinishReason::Stop`,
  `"MAX_TOKENS"` → `FinishReason::Length` — the only reason
  auto-continuation triggers — anything else → `FinishReason::Other`).
- Error handling reuses the existing status-code-based classification
  (`LlmError::from_status`) — Gemini's HTTP status codes for rate-limit
  (429), auth (401/403), overload (503), and not-found (404) line up with
  the existing taxonomy, so failover/cooldown behavior (backoff ladders, key
  rotation) applies unchanged.

**Tests** (`crates/familyclaw-agent/src/llm.rs`, `mod tests`): request
role-mapping (system/user/assistant), `systemInstruction` presence/absence,
camelCase field serialization, response parsing (single- and multi-part
text, finish-reason mapping), endpoint URL construction (with/without
trailing slash), an end-to-end test through `LlmClient::complete()` against
a local mock HTTP server (same hand-rolled TCP-listener harness the OpenAI
tests already use — no new test dependency), and resolver-level tests
(`crates/familyclaw-agent/src/llm_chain.rs`) proving `ModelConfig` →
`build_llm_chain` → `LlmFailover` carries the wire format through end to
end.

### Not shipped for `GeminiGenerate` (tracked here, not silently missing)

- **Tool/function calling.** Gemini's native shape
  (`tools: [{"functionDeclarations": [...]}]` in the request,
  `candidates[0].content.parts[].functionCall` in the response, and a
  `functionResponse` part to send the result back) is structurally
  different enough from `ToolCall`'s OpenAI-shaped wire representation
  (`crates/familyclaw-agent/src/llm.rs`, `ToolCallWire`) that it needs its
  own request/response types, not a reuse of the OpenAI ones. Calling
  `complete_with_tools`/`complete_with_tools_choice` with a non-empty
  `tools` list on a `GeminiGenerate` config returns `LlmError::Http`
  pointing at this document, rather than silently sending an
  OpenAI-shaped (and therefore rejected/ignored) `tools` field.
- **Streaming.** Gemini's streaming endpoint is
  `POST {api_base}/models/{model}:streamGenerateContent?alt=sse` (note the
  different path AND the `alt=sse` query parameter — it is not the same SSE
  framing as OpenAI's `stream: true`). `complete_stream()` returns
  `LlmError::Http` for any non-`OpenAiChat` wire format today.

**Follow-up sizing:** function calling ~0.5–1 day (new wire structs +
round-trip through the existing tool-loop in `familyclaw_agent::agent`,
which currently assumes the OpenAI `ToolCall` shape end-to-end — the
agent-level tool loop, not just this client, would need a translation
layer). Streaming ~0.5 day (mostly a different SSE line-parser).

## Not shipped: `AnthropicMessages`

**Endpoint:** `POST {api_base}/v1/messages` (default `api_base`:
`https://api.anthropic.com`).

**Auth:** `x-api-key: {api_key}` header (NOT `Authorization: Bearer`) +
`anthropic-version: 2023-06-01` header (both required; Anthropic returns 400
without the version header).

**Request shape (sketch):**

```json
{
  "model": "claude-...",
  "max_tokens": 4096,
  "system": "concatenated system messages, top-level field (not in messages[])",
  "messages": [
    {"role": "user", "content": [{"type": "text", "text": "..."}]},
    {"role": "assistant", "content": [{"type": "text", "text": "..."}]}
  ],
  "tools": [{"name": "...", "description": "...", "input_schema": {...}}],
  "tool_choice": {"type": "auto"}
}
```

Key differences from OpenAI that a real implementation must handle:

- `system` is a **top-level string field**, not a `role: "system"` message
  (same shape decision as `GeminiGenerate`'s `systemInstruction` — the
  message-splitting helper this slice wrote for Gemini,
  `GeminiGenerateContentRequest::from_messages`, is a reasonable template to
  copy/adapt).
- `content` is an **array of typed blocks** (`{"type": "text", "text": ...}`,
  `{"type": "tool_use", ...}`, `{"type": "tool_result", ...}`), not a plain
  string — `LlmMessage::content` (a plain `String`) would need to become a
  single `text` block per message on this wire format.
- Tool definitions use `input_schema` directly at the top level of each tool
  entry (Anthropic's shape is actually closer to `ToolDefinition`'s in-memory
  shape than OpenAI's `{"type":"function","function":{...}}` envelope is —
  less translation needed here than for OpenAI).
- Tool **use** comes back as a `content` block
  (`{"type": "tool_use", "id", "name", "input"}`) rather than a separate
  `tool_calls` array — `finish_reason` becomes `stop_reason`
  (`"end_turn"` / `"max_tokens"` / `"tool_use"`).
- Error shape: `{"type": "error", "error": {"type": "...", "message": "..."}}`
  with the error `type` string (e.g. `"rate_limit_error"`,
  `"authentication_error"`, `"overloaded_error"`) carrying the
  classification signal that OpenAI puts in the HTTP status code alone —
  `LlmError::from_status` would need a second, body-aware classification
  path for this wire format (status codes alone are close but Anthropic
  uses 529 for "overloaded" the same way OpenAI-compatible NIM does, so
  reuse is mostly possible).

**Estimated size:** ~1 day for a `GeminiGenerate`-equivalent slice (text
completion only, tested); +0.5–1 day for tool calling (translating
`ToolCall`/`ToolDefinition` to/from the content-block shape).

## Not shipped: `Bedrock`

AWS Bedrock is qualitatively different from the other three formats: it is
not a plain HTTPS+API-key call, it requires **SigV4 request signing**
(AWS's canonical-request HMAC scheme), which needs the caller's AWS access
key ID, secret access key, (optional) session token, and region — none of
which fit `LlmConfig`'s current `api_key: String` field.

**Two API surfaces to choose between:**

1. **`InvokeModel`** (`POST /model/{modelId}/invoke`) — per-provider request
   body shape (Anthropic models on Bedrock use an Anthropic-messages-like
   body with a Bedrock-specific `anthropic_version` field; Amazon Titan/Nova
   models use their own shape entirely). This means `InvokeModel` is not
   ONE wire format but a family of them keyed by `modelId` prefix.
2. **`Converse`** (`POST /model/{modelId}/converse`) — a **unified** request/
   response shape Bedrock added specifically to abstract over the
   per-provider differences (closer to OpenAI's `messages` shape: `role` +
   `content` blocks, a top-level `system` array, and a normalized `toolConfig`/
   `toolUse` shape for function calling). **`Converse` is the right target**
   for a `LlmWireFormat::Bedrock` implementation — it avoids re-deriving
   per-model request shapes.

**What implementing this needs, concretely:**

- A minimal SigV4 signer (canonical request → string to sign → HMAC-SHA256
  signing key derived from the AWS secret key, date, region, service
  `"bedrock"` → `Authorization` header). No existing dependency in this
  workspace does this (checked: no `aws-sigv4`/`rusoto`/`aws-sdk-*` crate is
  currently a dependency of `familyclaw-agent` or its workspace-level
  `Cargo.toml`) — this would be the first AWS-signing code in the crate,
  either hand-rolled (~150–250 LOC for a `bedrock:converse` scoped signer,
  since the full generic SigV4 spec is broader than what one endpoint
  needs) or via the `aws-sigv4` crate (smaller diff, one new dependency).
- Credentials become a structured type, not a single `api_key: String` —
  `LlmConfig` would need an enum or an additional optional field set
  (`aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`,
  `aws_region`) gated behind `wire_format == Bedrock`, since the existing
  single-string key model doesn't fit.
- Region is part of the endpoint host
  (`bedrock-runtime.{region}.amazonaws.com`), so `api_base` construction
  differs from every other wire format (no user-supplied "base URL" — it's
  derived from the region).

**Estimated size:** ~2–3 days (signer + credential plumbing + `Converse`
request/response types + tests using a recorded/fixture-based approach,
since SigV4 signatures are timestamp-dependent and can't be tested against a
plain mock server the way the other three formats can without also
injecting a fake clock into the signer).

## Recommendation for the next slice

In priority order (highest value / lowest cost first): **1)** Gemini tool
calling (this slice's biggest remaining gap — text-only Gemini is not
useful for the agent's tool loop in practice), **2)** Anthropic Messages
text-only (mirrors this slice almost exactly), **3)** Gemini streaming,
**4)** Anthropic tool calling, **5)** Bedrock (signer is the real cost here,
budget it as its own slice rather than bundling with anything else).
