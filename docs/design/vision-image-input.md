# Vision / image input

Status: **partial** — `familyclaw-agent`'s `LlmMessage` now carries optional
images (`LlmImageRef`) and the `OpenAiChat` wire format serializes them into
the standard OpenAI-compatible multimodal `content` array
(`[{"type":"text",...},{"type":"image_url",...}]`). This is the **verified
minimal slice**: one wire format, one message-shape change, fully tested.
Everything downstream of "a caller already has `LlmMessage`s and wants to
attach images before calling the LLM" is design-only below — channel-side
image ingestion (Telegram/Discord photo → `LlmImageRef`), the Gemini/
Anthropic/Bedrock wire shapes, model-capability gating, and image redaction
are **not implemented**.

## Why this exists

Before this slice, `LlmMessage.content` was a plain `String` — there was no
way to attach an image to a request at all, so a vision-capable model
config (e.g. `gpt-4o`, `gpt-4o-mini`, most modern OpenAI-compatible vision
endpoints) could never actually receive an image through this crate, even
though the wire format (`chat/completions`) has supported multimodal
`content` for a long time. Agents that can *see* a screenshot, a photo sent
in a channel, or a generated image are a real family use case (agent_alpha's
vision/computer-use work, `skills/vision-action`) but the LLM client itself
was text-only.

## The seam: `LlmMessage.images` + `LlmImageRef`

```rust
pub struct LlmImageRef {
    pub url: String, // "https://..." or "data:{mime};base64,{data}"
}

impl LlmImageRef {
    pub fn from_url(url: impl Into<String>) -> Self;
    pub fn from_base64(mime_type: &str, base64_data: impl AsRef<str>) -> Self;
}

pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub images: Option<Vec<LlmImageRef>>, // NEW
}

impl LlmMessage {
    pub fn with_images(self, images: Vec<LlmImageRef>) -> Self; // no-op if empty
    pub fn has_images(&self) -> bool;
}
```

Both `url` forms are legal per the OpenAI vision spec
(<https://platform.openai.com/docs/guides/vision>) — a hosted URL or a
base64 **data URI**. `LlmImageRef::from_base64` exists for callers that only
have raw bytes (a screenshot, an upload) and no hosted URL; this crate does
**not** depend on a base64 codec, so the caller base64-encodes first (most
callers already have one in their dependency tree — `base64` crate, or the
image library they captured the screenshot with).

### Wire shape rule (byte-identical invariant preserved)

`LlmMessage` had `#[derive(Serialize, Deserialize)]`; this slice replaces
that with a **hand-written** impl (same bridge pattern already used for
`ToolCall`'s OpenAI wire shape in this file):

- `images` empty/`None` → `content` serializes as a **plain string**, byte-
  identical to every request built before this slice existed. All existing
  tests (`test_tool_less_request_exact_string_serialization` et al.) assert
  this at the exact-string level and still pass unmodified.
- `images` non-empty → `content` serializes as an **array** of parts: one
  `{"type":"text","text":...}` part (omitted entirely if `content` is
  empty — an image-only turn with no caption must not send a stray empty
  text part) followed by one `{"type":"image_url","image_url":{"url":...}}`
  part per image, in order.

The same bridge also backs `Deserialize`, so an image-bearing `LlmMessage`
round-trips correctly through the resumable-turn journal
(`crate::resumable`) — a crash mid-turn with an image attached reloads with
`images` intact, not silently dropped.

### What was deliberately NOT touched

- **`ChatCompletionsRequest`/`ChatCompletionsStreamRequest`** — unchanged.
  Both already serialize `messages: &[LlmMessage]` using `LlmMessage`'s own
  `Serialize`, so the multimodal shape "falls out" of the message-level
  change with zero request-struct edits.
- **`GeminiGenerateContentRequest::from_messages`** — still reads only
  `m.content`; `images` is silently ignored on that path (see "Gemini"
  below — this is a real gap, not an oversight).
- **Redaction (`agent/helpers.rs::redact_messages`-equivalent)** — text
  redaction (`familyclaw_actions::redact_free_text`) runs on `m.content`
  only; `images` pass through unredacted (functional-update `..m.clone()`).
  A base64 data URI could theoretically embed something sensitive
  (screenshot of a password field) — this slice does not add image
  redaction. Flagged as a gap below.

## What shipped (verified)

`crates/familyclaw-agent/src/llm.rs`:

- `LlmImageRef` (`from_url`, `from_base64`).
- `LlmMessage.images: Option<Vec<LlmImageRef>>`, `with_images`, `has_images`.
- Hand-written `Serialize`/`Deserialize` for `LlmMessage` implementing the
  string-vs-array rule above.
- 8 new unit tests: plain-string byte-identity, single image, multiple
  images + base64, image-only (no stray text part), full round-trip through
  JSON, `with_images(vec![])` no-op, and a `ChatCompletionsRequest`-level
  test proving the multimodal shape survives being embedded in a full
  request body (and that no stray `"images"` sibling key leaks onto the
  wire).

Verified with:

```
cargo build --workspace --all-features
cargo clippy --workspace --all-features -- -D warnings
cargo test -p familyclaw-agent --all-features --lib llm::
```

All green; 314/314 `familyclaw-agent` lib tests pass (no regressions).

## What is NOT implemented (design-only)

### 1. Gemini / Anthropic / Bedrock multimodal wire shapes

`LlmWireFormat::GeminiGenerate` only implements the tool-less text path
today (`docs/design/multi-provider-wire-formats.md`). Gemini's
`generateContent` supports images via `inlineData`
(`{"inlineData":{"mimeType":...,"data":<base64, no data: prefix>}}`) inside
the same `parts` array as text — structurally similar to what this slice
already does for OpenAI, but the wire field names and the "no `data:`
prefix on Gemini's base64" detail differ enough to need its own mapping in
`GeminiGenerateContentRequest::from_messages`. Anthropic's `/v1/messages`
uses a third shape (`{"type":"image","source":{"type":"base64",...}}`).
Neither is implemented; both are already flagged not-implemented for text
in the existing design doc, so this is additive to a known gap, not a new
one.

### 2. Model-capability gating

`LlmConfig` has no "this model/endpoint supports vision" flag. Today,
calling `.with_images(...)` on a message sent to a non-vision model config
will send a wire request the provider may reject (400) or silently ignore
the image part on — the failure mode is provider-dependent and currently
surfaces as a generic `LlmError::Http`/`LlmError::Parse`, not a clear
"this model can't see images" error. A real implementation needs either
(a) a `supports_images: bool` (or a small capability enum) on `LlmConfig`,
checked before attaching images and returned as a typed
`LlmError::UnsupportedCapability`-style variant, or (b) relying on the
provider's own rejection and mapping known error bodies to a clearer
message. Not done here — out of scope for the wire-shape slice.

### 3. Channel-side image ingestion

Nothing today converts an inbound channel attachment (a Telegram/Discord/
Slack photo message, a WhatsApp image) into an `LlmImageRef`. The
`familyclaw-channels` inbound envelope would need an attachment/media field
(if it doesn't already have one — not audited as part of this slice) and
the `agent`/`live_executor` turn-building code
(`live_executor.rs::build_messages`, `agent/mod.rs`) would need to read it
and call `LlmMessage::user(text).with_images(...)`. This is the piece that
actually makes "someone sends agent_alpha a photo" work end-to-end; today only
the LLM-client layer understands images, nothing upstream produces them.

### 4. Size / count limits and cost guardrails

No validation exists on image count per message, per-image byte size, or
total request payload size. A provider's own limits (e.g. 20MB/image, 10
images/request for many OpenAI-compatible APIs) will surface as a 400/413
from the provider today rather than being caught client-side with a clear
error. `ToolDefinition::validate` is the existing pattern for this kind of
boundary check (`crates/familyclaw-agent/src/llm.rs`) — a
`LlmImageRef::validate`/`LlmMessage::validate_images` following the same
shape (checked before the HTTP call, deterministic config error) is the
natural next step.

### 5. Image redaction

As noted above, `images` bypass the existing text/secret redaction path
entirely. If a screenshot embeds a visible API key or password, nothing
strips it before it reaches the model or a log. Out of scope for this
slice (redacting *inside* an image would require OCR or a vision-model
pre-pass — a materially bigger feature); flagged so it isn't mistaken for
"images are as safe as text" by a future reader.

## Suggested next slice

In priority order for "the shortest path to a real end-to-end vision use
case":

1. `LlmConfig.supports_images: bool` + a clear `LlmError` variant when a
   caller attaches images to a config that doesn't advertise support.
2. Channel-side ingestion for **one** channel (Discord or Telegram, whichever
   already has the simplest attachment/media field) → `LlmImageRef`.
3. Gemini `inlineData` mapping (the second wire format already has a text
   path to extend, lowest incremental cost of the three unimplemented wire
   formats).
