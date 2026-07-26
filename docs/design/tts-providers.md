# Text-to-speech (TTS) providers

Status: **partial** — `familyclaw-tts` (new crate) ships a working
abstraction (`TtsProvider`) plus two providers: `MockTts` (in-memory,
default, no network) and `OpenAiTts` (feature `openai`, verified minimal
slice, real HTTP adapter). ElevenLabs, Azure/Edge TTS, and a local engine
(Piper) are design-only below.

## Why this exists

Agents in FamilyClaw had no way to produce speech audio: there was no TTS
abstraction anywhere in the workspace (`familyclaw-channels` moves text
messages, `familyclaw-actions` executes tools, but nothing turns text into
sound). This is the first slice: one crate, one interface, one verified
real backend.

## The seam: `TtsProvider`

```rust
pub trait TtsProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn synthesize(&self, request: TtsRequest) -> TtsFuture<'_>;
}
```

- `TtsRequest` — validated (non-empty text) input: text + optional voice /
  `AudioFormat` / speed (`0.25..=4.0`).
- `TtsAudio` — output: raw encoded `bytes`, `AudioFormat`, `provider` id.
- `TtsFuture<'a>` — `Pin<Box<dyn Future<Output = TtsResult<TtsAudio>> + Send + 'a>>`,
  the same "boxed future instead of `async-trait`" pattern already used by
  `familyclaw_channels::Channel::send` — keeps the trait dyn-compatible
  (`Box<dyn TtsProvider>`) without adding a proc-macro dependency.
- `TtsError` — `InvalidInput` / `Request` (transport failure, no HTTP
  status) / `Backend` (provider responded with a non-2xx status or an
  empty/malformed payload). Converts into `FamilyClawError::Llm` (a TTS
  call is, like an LLM call, an external generative-AI API request).

This mirrors `familyclaw-channels`' `Channel` trait + feature-gated real
adapters (`discord`/`telegram`/`slack`, with `whatsapp`/`signal` reserved
as empty flags) and `familyclaw-embeddings`' "zero-dep default + real
backend behind a feature flag" split (`DeterministicEmbedder` /
`OllamaEmbedder`).

## What shipped

### `MockTts` (default feature, no network)

An in-memory provider — `synthesize` returns the UTF-8 bytes of the
request text as the "audio" payload. Exists so agent code and tests can
exercise the full `TtsProvider` interface (routing, format selection,
error handling) offline, with no API key and no network access. Same role
as `familyclaw_channels::MockChannel` for the channel layer.

### `OpenAiTts` (feature `openai`, verified minimal slice)

An HTTP adapter for `POST {api_base}/audio/speech`
(`{model, input, voice, response_format, speed}` → raw audio bytes).

- **Why "OpenAI-compatible" and not just "OpenAI"**: this exact
  request/response shape is not OpenAI-exclusive — several
  OpenAI-API-compatible gateways (Groq, DeepInfra, and others) implement
  the same endpoint. `api_base` is runtime configuration (same pattern as
  `TelegramChannel::with_api_base`), so this one adapter, pointed at a
  different `api_base` + `model`, works against any of them.
- **Auth**: a single bearer API key, read from `OPENAI_API_KEY` by default
  or supplied to the constructor — no OAuth dance, no device pairing, no
  multi-step flow. Matches the auth pattern already used for the LLM
  client (`familyclaw_agent::llm_chain`, `"OPENAI_API_KEY"`).
- **Tested**: unit tests (input validation, builder methods) plus an
  integration test (`tests/openai_http.rs`) that runs a real `reqwest`
  transport against a `std::net::TcpListener`-based mock HTTP server (same
  no-new-dependency approach as
  `familyclaw-channels/tests/telegram_http_errors.rs`) — covers the 200
  success path (audio bytes returned), 401/500 backend errors, and
  connection-refused (transport-level `TtsError::Request`).
- **Not covered by the mock test**: a real OpenAI account was not hit (no
  live API key available in this environment) — the mock proves the wire
  protocol and error handling are correct; it does not prove OpenAI's
  production endpoint accepts the exact request shape sent. Low risk since
  the shape matches OpenAI's public API docs, but flagging it as unverified
  against the real service.

## Not shipped (design only)

### `ElevenLabsTts`

ElevenLabs' `POST /v1/text-to-speech/{voice_id}` uses a different request
shape (`text`, `voice_settings: {stability, similarity_boost}`) and auth
header (`xi-api-key`, not `Authorization: Bearer`). A new
`elevenlabs.rs` module + `elevenlabs` feature flag, implementing the same
`TtsProvider` trait, is a self-contained addition — no changes needed to
`provider.rs`/`error.rs`. Voice selection would map `TtsRequest::voice()`
(an ElevenLabs voice id) directly; `speed` has no direct ElevenLabs
equivalent (would need to fold into `voice_settings` or be rejected with
`TtsError::InvalidInput` for that provider).

### Azure / Edge TTS

Two different integration shapes exist under this name:

- **Azure Cognitive Services Speech** — a real Microsoft cloud API
  (`POST {region}.tts.speech.microsoft.com/cognitiveservices/v1`, SSML
  body, subscription-key auth). This fits the same HTTP-adapter pattern as
  `OpenAiTts` (bearer/subscription-key auth, single POST, binary response)
  and would be the next real feature-gated adapter to add.
- **`edge-tts` (Microsoft Edge's free "Read Aloud" voice service)** —
  unofficial, undocumented, and reverse-engineered: it speaks a WebSocket
  protocol (`wss://.../consumer/speech/synthesize/readaloud/edge/v1`) with
  a custom binary/text-frame framing, not a simple HTTP POST. It genuinely
  needs no API key/auth ("no exotic auth" in the sense of no signup), but
  the *protocol* itself is exotic relative to this crate's HTTP-adapter
  pattern (WebSocket + custom frame parsing) and would need `tokio-tungstenite`
  or similar — deliberately deferred rather than rushed in as a fragile,
  unofficial integration.

### Local engine (Piper)

A fully offline, no-network, no-API-key option:
[Piper](https://github.com/rhasspy/piper) is a small local neural TTS
engine (ONNX models) with a CLI/`piper-rs` binding. A `PiperTts` adapter
would shell out to (or FFI-bind) a local `piper` binary/model file and
return the produced WAV bytes — closer in spirit to `familyclaw-sandbox`'s
process-isolation patterns than to an HTTP adapter. Useful as the "always
works, never costs money, never leaks text to a third party" fallback tier
once the abstraction has more than one HTTP-based provider proving the
trait is provider-agnostic.

## Wiring into agents (not shipped)

`familyclaw-tts` is a standalone crate today — it is not yet called from
`familyclaw-actions` (agent tool/skill invocation) or `familyclaw-channels`
(e.g. a voice-message reply). The natural next step is a `speak` action in
`familyclaw-actions` that takes a `Box<dyn TtsProvider>` (selected at agent
construction time, same as the LLM chain) and turns an agent's text output
into a `TtsAudio` payload a channel adapter can attach to an outbound
message. Left out of this slice to keep the change reviewable and to avoid
guessing at `familyclaw-actions`' manifest/skill schema without a
dedicated design pass.
