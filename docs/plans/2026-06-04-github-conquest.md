# FamilyClaw GitHub Conquest Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Make FamilyClaw buildable, runnable, demoable, and publishable in one afternoon — with features OpenClaw and Hermes Agent cannot match.

**Architecture:** FamilyClaw already has 12 crates, 439 tests, and 6 unique capabilities. What's missing is: (1) an LLM brain behind the agent, (2) one real channel, (3) a knockout demo, (4) publishable docs. We add these on top of the existing architecture without changing any crate's public API.

**Tech Stack:** Rust, serenity (Discord), reqwest (LLM HTTP), tokio, cargo doc

**Time target:** Today. Each task is 5-15 minutes.

---

## Phase 1: LLM Brain (familyclaw-agent gets a mind)

### Task 1: Add LLM client module to familyclaw-agent

**Objective:** Create a generic LLM HTTP client that calls OpenAI-compatible chat completions endpoints.

**Files:**
- Create: `crates/familyclaw-agent/src/llm.rs`
- Modify: `crates/familyclaw-agent/Cargo.toml` (add reqwest + base64 deps)
- Modify: `crates/familyclaw-agent/src/lib.rs` (pub mod llm)

**Design:**
```rust
// LLMConfig — runtime configuration (KERROS B loads this, never hardcoded)
pub struct LlmConfig {
    pub api_base: String,     // e.g. "https://api.openai.com/v1"
    pub api_key: String,      // loaded from env/file at runtime
    pub model: String,        // e.g. "gpt-4o"
    pub max_tokens: u32,
}

// LlmClient — stateless HTTP caller
pub struct LlmClient {
    config: LlmConfig,
    client: reqwest::Client,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self;
    pub async fn complete(&self, messages: Vec<LlmMessage>) -> Result<String>;
}

// LlmMessage — OpenAI-compatible format
pub struct LlmMessage {
    pub role: LlmRole,  // System, User, Assistant, Tool
    pub content: String,
}
```

**Verification:** `cargo test -p familyclaw-agent` — llm module compiles, struct tests pass (no live API calls in tests).

---

### Task 2: Wire LLM into Agent::handle_turn

**Objective:** When an agent receives a text BusMessage, it calls the LLM, gets a response, and publishes it back.

**Files:**
- Modify: `crates/familyclaw-agent/src/agent.rs` (add LlmClient field, use in handle_turn)
- Modify: `crates/familyclaw-agent/src/agent.rs` (Agent::new takes Option<LlmClient>)

**Logic:**
```
handle_turn(sender, BusMessage::Text):
  1. Build prompt from soul.essence + memory context + incoming message
  2. Call llm.complete(messages)
  3. Wrap response as BusMessage::text(response)
  4. Publish to bus
  5. Record TurnOutcome in durable log
  6. Store conversation in memory
```

If no LlmClient is configured, fall back to echo (current behavior).

**Verification:** Unit test: Agent with mock LLM responds to a text message. Agent without LLM echoes.

---

### Task 3: Tool-call loop in handle_turn

**Objective:** Parse LLM tool_call responses and execute them before completing the turn.

**Files:**
- Modify: `crates/familyclaw-agent/src/agent.rs`

**Design:**
```rust
// ToolRegistry — simpleHashMap<String, Box<dyn Tool>>
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn execute(&self, args: serde_json::Value) -> Result<serde_json::Value>;
}
```

Loop: `LLM response → has tool_calls? → execute → feed result back → LLM again → final response`

If no tool_calls, return immediately. Max 10 iterations.

**Verification:** Test: mock LLM returns tool_call → mock Tool executes → LLM returns final text.

---

## Phase 2: Discord Channel (FamilyClaw speaks to the world)

### Task 4: Add serenity dependency behind feature flag

**Objective:** Add serenity as an optional dependency for the `discord` feature in familyclaw-channels.

**Files:**
- Modify: `crates/familyclaw-channels/Cargo.toml`

```toml
[dependencies]
serenity = { version = "0.12", optional = true }
futures = { version = "0.3", optional = true }

[features]
discord = ["dep:serenity", "dep:futures"]
```

**Verification:** `cargo check -p familyclaw-channels` (default features, no serenity). `cargo check -p familyclaw-channels --features discord` (serenity compiles).

---

### Task 5: Implement DiscordChannel

**Objective:** Create a `DiscordChannel` that implements the `Channel` trait using serenity.

**Files:**
- Create: `crates/familyclaw-channels/src/discord.rs`
- Modify: `crates/familyclaw-channels/src/lib.rs` (conditional `mod discord`)

**Design:**
```rust
#[cfg(feature = "discord")]
pub struct DiscordChannel {
    channel_id: String,
    discord_token: String,       // runtime config (KERROS B)
    target_channel_id: u64,      // runtime config
    inbound_tx: tokio::sync::mpsc::UnboundedSender<InboundEnvelope>,
    inbound_rx: tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>, // moved to MessageStream
    sent: Arc<Mutex<Vec<OutboundMessage>>>,
}

#[cfg(feature = "discord")]
impl Channel for DiscordChannel { ... }
```

Key: `DiscordChannel::start(bot_token)` spawns a serenity gateway shard. Messages from Discord are converted to `InboundEnvelope` and pushed into the inbound channel. `send()` posts to Discord via HTTP.

**Verification:** `cargo test -p familyclaw-channels --features discord` — DiscordChannel compiles. Mock test: inject message → receive as InboundEnvelope.

---

### Task 6: DiscordChannel end-to-end integration in demo binary

**Objective:** Update `familyclaw` demo binary to optionally use Discord instead of MockChannel.

**Files:**
- Modify: `crates/familyclaw-agent/src/bin/familyclaw.rs`

**Logic:**
```
if DISCORD_TOKEN env is set:
  DiscordChannel::new(token, channel_id)
else:
  MockChannel::new("demo")
```

**Verification:** `cargo run -p familyclaw-agent --features familyclaw-channels/discord` — binary starts, connects to Discord if token provided, falls back to mock.

---

## Phase 3: Demo Binary (30 seconds of wow)

### Task 7: Rewrite demo binary as `familyclaw demo`

**Objective:** The demo binary becomes a 30-second showcase: two agents talk, emotions leak, memory decays, dream consolidates.

**Files:**
- Modify: `crates/familyclaw-agent/src/bin/familyclaw.rs`

**Demo script (all automated, no user input needed):**
```
1. "Spawning agent_a and agent_b on the Resonance Bus..."
2. agent_a: "Hei agent_b! Tervetuloa perheeseen." → bus delivers → agent_b receives
3. agent_b: emotion contagion kicks in — logs show emotional shift
4. agent_a stores memory: excitement about new family member
5. Time jump: "7 päivää myöhemmin..." — memory ages, retention drops
6. Dream cycle runs: duplicate merge, date absolutization (e.g. "eilen" → "2026-06-03")
7. agent_a retrieves memory — shows decay curve vs identity-anchored core fact
8. "FamilyClaw: agents that remember, feel, dream, and think to each other."
```

Each step prints clear output with `info!` logging.

**Verification:** `cargo run -p familyclaw-agent 2>&1 | head -40` — shows complete demo output.

---

## Phase 4: Docs & Polish

### Task 8: QUICKSTART.md

**Objective:** 5-minute quickstart guide.

**Files:**
- Create: `docs/QUICKSTART.md`

**Content:**
```markdown
# Quick Start

## Prerequisites
- Rust 1.85+ (`rustup update`)
- Git

## Build & Run
```bash
git clone https://github.com/Sisuthros/familyclaw.git
cd familyclaw
cargo run -p familyclaw-agent
```

## What you'll see
[demo output walkthrough]

## Next steps
- Connect to Discord: `DISCORD_TOKEN=your_token cargo run -p familyclaw-agent --features familyclaw-channels/discord`
- Read ARCHITECTURE.md for the design
- Read CONTRIBUTING.md to contribute
```

**Verification:** Follow the guide on a fresh clone — it works.

---

### Task 9: API documentation pass

**Objective:** Every public item has `///` docs, `cargo doc` produces clean output.

**Files:**
- Review all crates: `cargo doc --all --no-deps 2>&1 | grep "warning: missing"`

**Verification:** `cargo doc --all --no-deps` — 0 warnings.

---

### Task 10: crates.io publish prep

**Objective:** Prepare metadata for `cargo publish`.

**Files:**
- Modify: `Cargo.toml` (add `[workspace.package]` fields: categories, keywords, readme)
- Add: `keywords = ["ai", "multi-agent", "memory", "actor", " rust"]`
- Add: `categories = ["science::robotics", "algorithms"]`
- Verify README.md exists at workspace root

**Verification:** `cargo publish --dry-run -p familyclaw-core` — metadata validates.

---

## Phase 5: GitHub Launch

### Task 11: Commit everything and push

**Objective:** All work committed to `main`, pushed to GitHub.

**Steps:**
1. `git checkout main`
2. `git add -A`
3. `git commit -m "feat: LLM brain, Discord channel, demo binary, docs, CI, CHANGELOG"`
4. Verify: `cargo test --all && cargo clippy -- -D warnings && cargo fmt --all -- --check`
5. `git push origin main`

**Verification:** GitHub Actions CI runs green on `main`.

---

### Task 12: Delete stale branches, set GitHub metadata

**Objective:** Clean repo, set topics, add website URL.

**Steps:**
1. Delete `agent_gamma-amplifier-v1` branch (contains KERROS B work)
2. `gh repo edit Sisuthros/familyclaw --add-topic rust --add-topic ai --add-topic multi-agent --add-topic actor --add-topic memory --add-topic llama --add-topic emotional-ai`
3. Set homepage URL to crates.io or GitHub Pages once available
4. Add branch protection: `main` requires CI green

**Verification:** `gh repo view Sisuthros/familyclaw --json repositoryTopics` shows topics. Branch `agent_gamma-amplifier-v1` gone.

---

## What This Delivers (by end of today)

| Before | After |
|--------|-------|
| 0 channels | 1 Discord channel + MockChannel for dev |
| No LLM brain | OpenAI-compatible LLM with tool-call loop |
| Manual demo only | `cargo run` = 30-second automated showcase |
| 0 quickstart | QUICKSTART.md: 5 minutes to running |
| No crates.io prep | Metadata ready, `cargo publish --dry-run` passes |
| No CI on GitHub | 4-job CI + Layer B audit running on every push |
| Stale branches | Clean main only |
| 0 GitHub topics | 8 relevant topics, branch protection |

## Scoreboard After (honest projection)

| Metric | FamilyClaw (after) | OpenClaw | Hermes |
|--------|-------------------|----------|--------|
| Durable execution | ✅ AINOA | ❌ | ❌ |
| Ebbinghaus + anchors | ✅ AINOA | ❌ | ❌ |
| Dream consolidation | ✅ AINOA | ❌ | ❌ |
| Affective nervous system | ✅ AINOA | ❌ | ❌ |
| Latent telepathy | ✅ AINOA | ❌ | ❌ |
| WASM sandbox | ✅ AINOA | ❌ | ❌ |
| unsafe forbidden | ✅ AINOA | ❌ | ❌ |
| Layer A/B isolation | ✅ AINOA | ❌ | ❌ |
| Discord channel | ✅ 1 | ✅ 28+ | ✅ 28+ |
| LLM agent loop | ✅ 1 (basic) | ✅ (mature) | ✅ (mature) |
| Plugin system | ❌ (next sprint) | ✅ | ✅ |
| Stars | 0 → 1+ | 376k | 180k |
| crates.io | ✅ (publishable) | ❌ (npm) | ❌ (pip) |

**FamilyClaw will never win on quantity. It wins on what others can't even attempt.**