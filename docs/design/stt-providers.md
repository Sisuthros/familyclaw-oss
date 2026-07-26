# Speech-to-text (STT) providers

Status: **partial** — `familyclaw-stt` (new crate) ships a working
abstraction (`SttProvider`) plus two providers: `MockStt` (in-memory,
default, no network) and `OpenAiWhisper` (feature `openai`, verified
minimal slice, real HTTP adapter). A local `whisper.cpp` engine, Azure
Speech, and Google Speech-to-Text are design-only below. Wiring transcribed
text into an inbound-message pipeline (so a voice message on a channel
actually reaches an agent as text) is also design-only.

## Why this exists

Agents in FamilyClaw had no way to understand spoken audio: there was no
STT abstraction anywhere in the workspace. `familyclaw-tts` (shipped
2026-07 per `docs/design/tts-providers.md`) covers text→speech; nothing
covered the inverse direction, speech→text, so any inbound voice message
(Telegram/Discord/WhatsApp voice notes, a mic-input skill, etc.) had no way
to become text an agent's LLM chain can act on. This is the first slice:
one crate, one interface, one verified real backend — deliberately
mirroring `familyclaw-tts`'s shape so the two crates read as a matched
pair.

## The seam: `SttProvider`

```rust
pub trait SttProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn transcribe(&self, request: SttRequest) -> SttFuture<'_>;
}
```

- `SttRequest` — validated (non-empty audio) input: raw audio bytes +
  `AudioFormat` + optional language hint (ISO-639-1, e.g. `"fi"`) + optional
  bias prompt.
- `SttTranscript` — output: recognized `text`, optional detected/requested
  `language`, `provider` id.
- `SttFuture<'a>` — `Pin<Box<dyn Future<Output = SttResult<SttTranscript>> + Send + 'a>>`,
  the same "boxed future instead of `async-trait`" pattern already used by
  `familyclaw_channels::Channel::send` and `familyclaw_tts::TtsProvider::synthesize`
  — keeps the trait dyn-compatible (`Box<dyn SttProvider>`) without adding
  a proc-macro dependency.
- `SttError` — `InvalidInput` / `Request` (transport failure, no HTTP
  status) / `Backend` (provider responded with a non-2xx status or an
  empty/malformed payload). Converts into `FamilyClawError::Llm` (an STT
  call is, like an LLM call, an external generative-AI API request) — same
  classification `familyclaw-tts` uses.

This mirrors `familyclaw-tts`'s `TtsProvider` trait (and, one layer up,
`familyclaw-channels`' `Channel` trait + feature-gated real adapters, and
`familyclaw-embeddings`' "zero-dep default + real backend behind a feature
flag" split) closely enough that anyone who has read one of those three
crates can read this one without re-learning the pattern.

## What shipped

### `MockStt` (default feature, no network)

An in-memory provider — `transcribe` returns a fixed, configurable
transcript (default: `"mock transcript"`, override via
`MockStt::with_transcript`) regardless of the input audio. Exists so agent
code and tests can exercise the full `SttProvider` interface (routing,
language propagation, error handling) offline, with no API key and no
network access. Same role as `familyclaw_tts::MockTts` for the TTS layer.

Unlike `MockTts` (which meaningfully echoes UTF-8 text bytes back as
"audio"), `MockStt` cannot meaningfully "echo" arbitrary audio bytes back
as text — there is no deterministic audio→text function without a real
model. A fixed configurable transcript was chosen over e.g. hashing the
audio into a fake string, since tests that need deterministic output
per-input can just build several `MockStt` instances or check `with_id`
routing instead of trying to decode a hash-derived "transcript".

### `OpenAiWhisper` (feature `openai`, verified minimal slice)

An HTTP adapter for `POST {api_base}/audio/transcriptions`: a multipart
upload (`file`, `model`, optional `language`/`prompt`,
`response_format=verbose_json`) → JSON `{"text": ..., "language": ...}`.

- **Why "OpenAI-compatible" and not just "OpenAI"**: this exact
  request/response shape is not OpenAI-exclusive — Groq's Whisper
  endpoint, DeepInfra, and other OpenAI-API-compatible gateways implement
  the same endpoint. `api_base` is runtime configuration (same pattern as
  `familyclaw_tts::OpenAiTts::api_base`), so this one adapter, pointed at a
  different `api_base` + `model`, works against any of them.
- **Why `response_format=verbose_json` always**: the plain `json` format
  only returns `{"text": ...}`; `verbose_json` additionally reports the
  detected `language`, which the adapter surfaces on
  `SttTranscript::language`. The response parser treats `language` as
  optional (`#[serde(default)]`), so a gateway that ignores
  `response_format` and always returns the minimal `{"text": ...}` shape
  still parses correctly — the adapter then falls back to the request's
  `language` hint (if one was given) rather than leaving the field empty.
- **Auth**: a single bearer API key, read from `OPENAI_API_KEY` by default
  or supplied to the constructor — no OAuth dance, no device pairing, no
  multi-step flow. Matches the auth pattern already used for the LLM
  client (`familyclaw_agent::llm_chain`) and `familyclaw_tts::OpenAiTts`.
- **Tested**: unit tests (input validation, builder methods) plus an
  integration test (`tests/openai_http.rs`) that runs a real `reqwest`
  transport against a `std::net::TcpListener`-based mock HTTP server (same
  no-new-dependency approach as `familyclaw-tts/tests/openai_http.rs` and
  `familyclaw-channels/tests/telegram_http_errors.rs`) — covers the 200
  success path with and without a `language` field in the response,
  401/500 backend errors, and connection-refused (transport-level
  `SttError::Request`).
- **Not covered by the mock test**: a real OpenAI account was not hit (no
  live API key available in this environment), and the mock server does
  not parse/validate the multipart body it receives (it drains the
  request and returns a fixed response, same simplification the TTS
  integration test makes for its JSON body) — the mock proves the
  transport plumbing and error handling are correct; it does not prove
  OpenAI's production endpoint accepts the exact multipart shape sent
  (field names `file`/`model`/`language`/`prompt`/`response_format`,
  `Content-Type` per `AudioFormat`). Low risk since the shape matches
  OpenAI's public API docs, but flagging it as unverified against the real
  service — same caveat `docs/design/tts-providers.md` records for
  `OpenAiTts`.

## Not shipped (design only)

### Local engine (`whisper.cpp`)

A fully offline, no-network, no-API-key option:
[whisper.cpp](https://github.com/ggml-org/whisper.cpp) is a small local
inference engine (GGML models) with a CLI and a C API (bindable via
`whisper-rs` or a subprocess shell-out). A `WhisperCppStt` adapter would
shell out to (or FFI-bind) a local `whisper-cli` binary/model file and
return the produced text — closer in spirit to `familyclaw-sandbox`'s
process-isolation patterns than to an HTTP adapter, and the natural
"always works, never costs money, never leaks audio to a third party"
counterpart to `docs/design/tts-providers.md`'s proposed `PiperTts`. Not
attempted here: it needs either a bundled/downloaded model file (storage
and licensing question) or a hard runtime dependency on a binary not
present in this environment, both of which need a dedicated design pass
before landing.

### Azure Cognitive Services Speech-to-Text

A real Microsoft cloud API
(`POST {region}.stt.speech.microsoft.com/speech/recognition/...`, raw
audio body or chunked streaming, subscription-key auth via
`Ocp-Apim-Subscription-Key`). Fits the same HTTP-adapter pattern as
`OpenAiWhisper` (subscription-key auth, single POST, JSON response) for
the simple (non-streaming) recognition mode, and would be the next real
feature-gated adapter to add — same relationship `docs/design/tts-providers.md`
describes for Azure's TTS counterpart.

### Google Cloud Speech-to-Text

`POST https://speech.googleapis.com/v1/speech:recognize` with a JSON body
(`config: {encoding, languageCode}`, `audio: {content: <base64>}`) and
either an API key query param or OAuth2/service-account auth. The request
shape (JSON + base64 audio, not multipart) is different enough from
`OpenAiWhisper` to need its own module, but is still a single-POST HTTP
adapter — no exotic protocol, just a different auth/encoding convention.
Deferred rather than rushed in: base64-encoding large audio payloads has a
~33% size overhead worth flagging in a dedicated design pass (streaming
gRPC would avoid it but is a materially bigger dependency).

## Wiring into channels/agents (not shipped)

`familyclaw-stt` is a standalone crate today — it is not yet called from
`familyclaw-channels` (e.g. transcribing an inbound Telegram/Discord voice
note before it reaches the agent) or `familyclaw-actions` (e.g. a
`transcribe` tool an agent can invoke on an audio attachment it already
has). The natural next step, mirroring `docs/design/tts-providers.md`'s
proposed `speak` action for the TTS side, is:

1. A `Box<dyn SttProvider>` selected at agent/channel-adapter construction
   time (same pattern as the LLM chain and the proposed `speak` action).
2. In `familyclaw-channels`: when an inbound message carries an audio
   attachment (a channel-specific "voice note" variant), call
   `SttProvider::transcribe` before handing the message to the agent, so
   the agent's LLM chain only ever sees text — no per-channel STT
   special-casing inside the agent.

Left out of this slice for the same reason `familyclaw-tts`'s `speak`
action was left out: it requires touching `familyclaw-channels`' inbound
message schema and `familyclaw-actions`' manifest/skill schema, which
deserves its own reviewable change and design pass rather than being
guessed at here.
